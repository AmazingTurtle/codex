#!/usr/bin/env python3
"""Install verified Better Codex packages beside upstream Codex (Python 3.12+)."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
import uuid


REPOSITORY = "AmazingTurtle/codex"
TARGET = "x86_64-unknown-linux-gnu"
EXECUTABLES = (
    "bin/codex",
    "bin/codex-code-mode-host",
    "codex-resources/bwrap",
    "codex-path/rg",
    "codex-resources/zsh/bin/zsh",
)
TAG_PATTERN = re.compile(r"v\d+\.\d+\.\d+-better-codex(?:\.\d+)?")


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def atomic_link(target: Path, link: Path) -> None:
    temporary = link.with_name(f".{link.name}.{uuid.uuid4().hex}")
    try:
        temporary.symlink_to(target)
        temporary.replace(link)
    finally:
        temporary.unlink(missing_ok=True)


def package_files(package: Path) -> dict[str, str]:
    files = {}
    for file in sorted(package.rglob("*")):
        if file == package / ".better-codex-install.json":
            continue
        if not file.resolve().is_relative_to(package.resolve()):
            raise RuntimeError(
                "Package verification found a path outside the installation"
            )
        if file.is_file():
            files[str(file.relative_to(package))] = digest(file)
    return files


def smoke(package: Path, version: str) -> None:
    for name in EXECUTABLES:
        file = package / name
        if not file.resolve().is_relative_to(package.resolve()) or not os.access(
            file, os.X_OK
        ):
            raise RuntimeError(f"Missing package executable: {name}")
    with tempfile.TemporaryDirectory(prefix="better-codex-install-smoke-") as home:
        result = subprocess.run(
            [str(package / "bin/codex"), "--version"],
            env={**os.environ, "BETTER_CODEX_HOME": home},
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if not result.stdout.strip().endswith(version):
            raise RuntimeError(f"Installed binary version does not match {version}")


def install_archive(
    archive: Path, expected: str, tag: str, root: Path, bin_dir: Path
) -> Path:
    if TAG_PATTERN.fullmatch(tag) is None or len(tag) > 128:
        raise RuntimeError("Invalid Better Codex release tag")
    if digest(archive) != expected:
        raise RuntimeError("Runtime archive checksum mismatch")
    root, bin_dir = root.resolve(), bin_dir.resolve()
    current, launcher = root / "current", bin_dir / "better-codex"
    launcher_target = current / "bin/codex"
    if launcher.exists() or launcher.is_symlink():
        if not launcher.is_symlink() or launcher.readlink() != launcher_target:
            raise RuntimeError(f"Refusing to replace unmanaged command: {launcher}")
    if current.exists() and not current.is_symlink():
        raise RuntimeError(f"Refusing to replace unmanaged installation: {current}")
    root.mkdir(parents=True, exist_ok=True)
    bin_dir.mkdir(parents=True, exist_ok=True)
    installed = root / tag
    marker = installed / ".better-codex-install.json"
    version = tag.removeprefix("v")
    if installed.exists() or installed.is_symlink():
        if installed.is_symlink() or not marker.is_file():
            raise RuntimeError(f"Refusing to reuse unmanaged package: {installed}")
        recorded = json.loads(marker.read_text())
        files = package_files(installed)
        if recorded != {"tag": tag, "sha256": expected, "files": files}:
            raise RuntimeError(f"Cached installation failed verification: {installed}")
        smoke(installed, version)
    else:
        with tempfile.TemporaryDirectory(prefix=".staging-", dir=root) as temporary:
            staging = Path(temporary) / "package"
            staging.mkdir()
            with tarfile.open(archive) as payload:
                payload.extractall(staging, filter="data")
            metadata = json.loads((staging / "codex-package.json").read_text())
            if (
                metadata.get("version"),
                metadata.get("target"),
                metadata.get("variant"),
            ) != (version, TARGET, "codex"):
                raise RuntimeError(
                    "Package version/target/variant does not match the release"
                )
            smoke(staging, version)
            files = package_files(staging)
            (staging / ".better-codex-install.json").write_text(
                json.dumps({"tag": tag, "sha256": expected, "files": files}) + "\n"
            )
            staging.rename(installed)
    previous = current.readlink() if current.is_symlink() else None
    had_launcher = launcher.is_symlink()
    try:
        atomic_link(installed, current)
        atomic_link(launcher_target, launcher)
        smoke(current.resolve(), version)
    except Exception:
        if previous is None:
            current.unlink(missing_ok=True)
        else:
            atomic_link(previous, current)
        if not had_launcher:
            launcher.unlink(missing_ok=True)
        raise
    return installed


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="Exact downstream release tag")
    parser.add_argument(
        "--root", type=Path, default=Path.home() / ".local/share/better-codex"
    )
    parser.add_argument("--bin-dir", type=Path, default=Path.home() / ".local/bin")
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() not in ("x86_64", "amd64"):
        parser.error("Prebuilt packages currently support Linux x86_64 GNU")
    if TAG_PATTERN.fullmatch(args.version) is None:
        parser.error("Select an exact Better Codex release tag")
    base = f"https://github.com/{REPOSITORY}/releases/download/{args.version}"
    asset = f"better-codex-{TARGET}.tar.gz"
    with tempfile.TemporaryDirectory(prefix="better-codex-download-") as temporary:
        archive = Path(temporary) / asset
        for name in ("SHA256SUMS", asset):
            request = urllib.request.Request(
                f"{base}/{name}", headers={"User-Agent": "better-codex-installer"}
            )
            with urllib.request.urlopen(request, timeout=30) as response:
                with (Path(temporary) / name).open("wb") as output:
                    shutil.copyfileobj(response, output)
        entries = [
            line.split()
            for line in (Path(temporary) / "SHA256SUMS").read_text().splitlines()
        ]
        checksums = {parts[1]: parts[0] for parts in entries if len(parts) == 2}
        expected = checksums.get(asset)
        if expected is None or re.fullmatch(r"[0-9a-f]{64}", expected) is None:
            raise RuntimeError("Release checksums do not contain the runtime archive")
        installed = install_archive(
            archive, expected, args.version, args.root, args.bin_dir
        )
        print(f"Installed {args.version} at {installed}")
        print(f"Run {args.bin_dir / 'better-codex'}")


if __name__ == "__main__":
    main()
