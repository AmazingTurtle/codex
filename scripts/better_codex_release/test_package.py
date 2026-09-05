"""Exercise symbol separation with real ELF files, without building Rust."""

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import package

from codex_package import cli
from codex_package.cargo import SourceBuildOutputs


@unittest.skipUnless(
    sys.platform.startswith("linux")
    and all(shutil.which(tool) for tool in ("cc", "objcopy", "strip", "readelf")),
    "ELF packaging requires Linux and binutils",
)
class SymbolsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="better-codex-symbols-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.cache = self.root / "cached-build"
        source = self.root / "main.c"
        source.write_text("int main(void) { return 0; }\n")
        subprocess.run(
            ["cc", "-g", "-Wl,--build-id", str(source), "-o", str(self.cache)],
            check=True,
            capture_output=True,
        )
        self.payload = self.root / "package"
        for name in ("bin/codex", "bin/codex-code-mode-host", "codex-resources/bwrap"):
            dest = self.payload / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(self.cache, dest)
        self.metadata = {
            "version": "1.2.3-better-codex",
            "target": package.TARGET,
            "variant": "codex",
        }
        (self.payload / "codex-package.json").write_text(json.dumps(self.metadata))
        self.output = self.root / "artifacts"
        self.identity = {
            "upstreamTag": "rust-v1.2.3",
            "upstreamSha": "a" * 40,
            "sourceSha": "b" * 40,
        }

    def test_strips_copies_and_archives_matching_symbols(self):
        cached_bytes = self.cache.read_bytes()
        original_id = package.build_id(self.cache)
        shutil.rmtree(self.payload)
        run = subprocess.run
        stages = []

        def run_package_command(command, **kwargs):
            if command[:2] == ["just", "assemble-codex-package"]:
                stages.append("assemble")
                # Keep the real source-built release strip policy: prebuilt input
                # flags would bypass the premature stripping this test detects.
                args = cli.create_parser().parse_args(command[2:])
                cli.assemble_package(args)
                return subprocess.CompletedProcess(command, 0)
            if command[:2] == [
                sys.executable,
                str(Path(package.__file__).with_name("smoke.py")),
            ]:
                # The C fixture cannot run product smoke checks. Extraction must
                # already have produced the artifacts before that separate step.
                self.assertTrue((self.output / "release.json").is_file())
                stages.append("smoke")
                return subprocess.CompletedProcess(command, 0)
            return run(command, **kwargs)

        outputs = SourceBuildOutputs(
            entrypoint_bin=self.cache,
            code_mode_host_bin=self.cache,
            bwrap_bin=self.cache,
            codex_command_runner_bin=None,
            codex_windows_sandbox_setup_bin=None,
        )
        with (
            patch.object(
                sys,
                "argv",
                [
                    "package.py",
                    "--package-dir",
                    str(self.payload),
                    "--output-dir",
                    str(self.output),
                ],
            ),
            patch.object(package.os, "sched_setaffinity"),
            patch.object(
                package, "read_product_version", return_value=self.metadata["version"]
            ),
            patch.object(package, "read_workspace_version", return_value="1.2.3"),
            patch.object(package, "git", side_effect=["a" * 40, "b" * 40]),
            patch.object(cli, "build_source_binaries", return_value=outputs),
            patch.object(cli, "resolve_rg_bin", return_value=self.cache),
            patch.object(package.subprocess, "run", side_effect=run_package_command),
        ):
            package.main()
        self.assertEqual(stages, ["assemble", "smoke"])
        manifest = json.loads((self.output / "release.json").read_text())
        self.assertEqual(self.cache.read_bytes(), cached_bytes)
        self.assertEqual(
            manifest["buildIds"],
            {name: original_id for name in package.BINARIES},
        )
        for name in package.BINARIES:
            binary = self.payload / name
            self.assertLess(binary.stat().st_size, len(cached_bytes))
            self.assertEqual(package.build_id(binary), original_id)
            sections = subprocess.check_output(
                ["readelf", "-S", str(binary)], text=True
            )
            self.assertNotIn(".debug_info", sections)
            self.assertIn(".gnu_debuglink", sections)
        symbols = self.output / manifest["symbols"]
        extracted = self.root / "symbols"
        with tarfile.open(symbols) as archive:
            archive.extractall(extracted, filter="data")
        debug_files = sorted(extracted.rglob("*.debug"))
        self.assertEqual(len(debug_files), len(package.BINARIES))
        for debug in debug_files:
            self.assertEqual(package.build_id(debug), original_id)
            dwarf = subprocess.check_output(
                ["readelf", "--debug-dump=info", str(debug)], text=True
            )
            self.assertIn("DW_TAG_compile_unit", dwarf)
            self.assertIn("main.c", dwarf)
        checksums = dict(
            (name, digest)
            for digest, name in (
                line.split("  ", 1)
                for line in (self.output / "SHA256SUMS").read_text().splitlines()
            )
        )
        self.assertEqual(
            checksums,
            {
                file.name: hashlib.sha256(file.read_bytes()).hexdigest()
                for file in self.output.iterdir()
                if file.name != "SHA256SUMS"
            },
        )

    def test_rejects_output_inside_payload_without_modifying_binaries(self):
        with self.assertRaisesRegex(RuntimeError, "outside"):
            package.release_artifacts(
                self.payload, self.payload / "artifacts", self.identity
            )
        self.assertEqual(
            (self.payload / "bin/codex").read_bytes(), self.cache.read_bytes()
        )

    def test_rejects_foreign_target_before_stripping(self):
        self.metadata["target"] = "aarch64-apple-darwin"
        (self.payload / "codex-package.json").write_text(json.dumps(self.metadata))
        with self.assertRaisesRegex(RuntimeError, "target"):
            package.release_artifacts(self.payload, self.output, self.identity)
        self.assertEqual(
            (self.payload / "bin/codex").read_bytes(), self.cache.read_bytes()
        )

    def test_refuses_existing_artifacts_before_touching_package(self):
        package.release_artifacts(self.payload, self.output, self.identity)
        before = {name: (self.payload / name).read_bytes() for name in package.BINARIES}
        with self.assertRaisesRegex(RuntimeError, "exists"):
            package.release_artifacts(self.payload, self.output, self.identity)
        self.assertEqual(
            {name: (self.payload / name).read_bytes() for name in package.BINARIES},
            before,
        )


if __name__ == "__main__":
    unittest.main()
