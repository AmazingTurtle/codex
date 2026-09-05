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

import package


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
        manifest = package.release_artifacts(self.payload, self.output, self.identity)
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
