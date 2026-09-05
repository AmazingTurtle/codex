#!/usr/bin/env python3
"""Build a canonical runtime package and separate matching Linux debug symbols."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

from release import ROOT
from release import digest
from release import git
from release import read_product_version
from release import read_workspace_version

from codex_package.archive import write_archive


TARGET = "x86_64-unknown-linux-gnu"
BINARIES = ("bin/codex", "bin/codex-code-mode-host", "codex-resources/bwrap")


def build_id(binary: Path) -> str:
    notes = subprocess.run(
        ["readelf", "-n", str(binary)], check=True, capture_output=True, text=True
    ).stdout
    match = re.search(r"Build ID: ([0-9a-f]+)", notes)
    if match is None:
        raise RuntimeError(f"No ELF build ID in {binary}")
    return match.group(1)


def release_artifacts(package: Path, output: Path, identity: dict) -> dict:
    package, output = package.resolve(), output.resolve()
    if output.is_relative_to(package):
        raise RuntimeError("Release artifacts must be outside the runtime package")
    metadata = json.loads((package / "codex-package.json").read_text())
    if metadata.get("target") != TARGET or metadata.get("variant") != "codex":
        raise RuntimeError("Release package target/variant must be GNU Linux codex")
    names = (
        f"better-codex-{TARGET}.tar.gz",
        f"better-codex-symbols-{TARGET}.tar.gz",
        "release.json",
        "SHA256SUMS",
    )
    output.mkdir(parents=True, exist_ok=True)
    for name in names:
        if (output / name).exists():
            raise RuntimeError(f"Release artifact already exists: {output / name}")
    ids = {name: build_id(package / name) for name in BINARIES}
    # The upstream helper strips in place. Give it private copies so Cargo outputs,
    # installed packages, and other package variants retain their original symbols.
    with tempfile.TemporaryDirectory(prefix=".symbols-", dir=output) as temporary:
        scratch = Path(temporary)
        binaries = scratch / "bin"
        binaries.mkdir()
        for name in BINARIES:
            shutil.copy2(package / name, binaries / Path(name).name)
        subprocess.run(
            [
                "bash",
                str(
                    ROOT
                    / ".github/scripts/archive-release-symbols-and-strip-binaries.sh"
                ),
                "--target",
                TARGET,
                "--artifact-name",
                TARGET,
                "--release-dir",
                str(binaries),
                "--archive-dir",
                str(scratch),
                "--binaries",
                " ".join(Path(name).name for name in BINARIES),
            ],
            env={**os.environ, "RUNNER_TEMP": str(scratch)},
            check=True,
        )
        for name in BINARIES:
            stripped = binaries / Path(name).name
            debug = (
                scratch
                / f"codex-symbols-{TARGET}"
                / f"codex-symbols-{TARGET}"
                / f"{Path(name).name}.debug"
            )
            if build_id(stripped) != ids[name] or build_id(debug) != ids[name]:
                raise RuntimeError(f"Debug symbols do not match {name}")
        for name in BINARIES:
            shutil.copy2(binaries / Path(name).name, package / name)
        (scratch / f"codex-symbols-{TARGET}.tar.gz").rename(output / names[1])
    write_archive(package, output / names[0], force=False)
    manifest = {
        "schemaVersion": 1,
        **identity,
        "version": metadata["version"],
        "target": TARGET,
        "runtime": names[0],
        "symbols": names[1],
        "buildIds": ids,
        "sha256": {name: digest(output / name) for name in names[:2]},
    }
    (output / "release.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (output / "SHA256SUMS").write_text(
        "".join(f"{digest(output / name)}  {name}\n" for name in names[:3])
    )
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--cpus", type=int, default=6)
    args = parser.parse_args()
    if not 1 <= args.cpus <= 6:
        parser.error("CPU count must be between one and six")
    allowed = sorted(os.sched_getaffinity(0))[: args.cpus]
    os.sched_setaffinity(0, allowed)
    env = {
        **os.environ,
        "CARGO_BUILD_JOBS": str(len(allowed)),
        "RUST_TEST_THREADS": str(len(allowed)),
        "RAYON_NUM_THREADS": str(len(allowed)),
    }
    subprocess.run(
        [
            "just",
            "assemble-codex-package",
            "--target",
            TARGET,
            "--cargo-profile",
            "release",
            "--package-version",
            read_product_version(ROOT),
            "--package-dir",
            str(args.package_dir.resolve()),
        ],
        cwd=ROOT,
        env=env,
        check=True,
    )
    upstream_tag = f"rust-v{read_workspace_version(ROOT)}"
    release_artifacts(
        args.package_dir,
        args.output_dir,
        {
            "upstreamTag": upstream_tag,
            "upstreamSha": git("rev-parse", f"{upstream_tag}^{{commit}}"),
            "sourceSha": git("rev-parse", "HEAD"),
        },
    )
    subprocess.run(
        [
            sys.executable,
            str(Path(__file__).with_name("smoke.py")),
            str(args.package_dir),
        ],
        env=env,
        check=True,
    )


if __name__ == "__main__":
    main()
