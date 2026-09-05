"""Verify package verification, atomic selection, and command ownership."""

import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

import install


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="better-codex-install-test-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / "releases"
        self.bin = self.base / "bin"
        self.bin.mkdir()
        self.official = self.bin / "codex"
        self.official.write_text("user-owned upstream command")

    def archive(self, version, *, reported_version=None, traversal=False):
        payload = self.base / version
        payload.mkdir()
        (payload / "codex-package.json").write_text(
            json.dumps(
                {
                    "version": version,
                    "target": install.TARGET,
                    "variant": "codex",
                    "entrypoint": "bin/codex",
                }
            )
        )
        for name in install.EXECUTABLES:
            binary = payload / name
            binary.parent.mkdir(parents=True, exist_ok=True)
            binary.write_text(
                f"#!/bin/sh\nprintf '%s\\n' 'better-codex {reported_version or version}'\n"
            )
            binary.chmod(0o755)
        archive = self.base / f"{version}.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(payload, arcname=".")
            if traversal:
                member = tarfile.TarInfo("../escape")
                member.size = 1
                tar.addfile(member, io.BytesIO(b"x"))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        return archive, digest

    def test_updates_selection_and_retains_previous_package(self):
        first, digest = self.archive("1.2.3-better-codex")
        old = install.install_archive(
            first, digest, "v1.2.3-better-codex", self.root, self.bin
        )
        second, digest = self.archive("1.2.4-better-codex")
        new = install.install_archive(
            second, digest, "v1.2.4-better-codex", self.root, self.bin
        )
        self.assertEqual((self.root / "current").resolve(), new)
        self.assertEqual((self.bin / "better-codex").resolve(), new / "bin/codex")
        self.assertTrue((old / "bin/codex").is_file())
        self.assertEqual(self.official.read_text(), "user-owned upstream command")

    def test_bad_checksum_does_not_change_selection(self):
        archive, digest = self.archive("1.2.3-better-codex")
        installed = install.install_archive(
            archive, digest, "v1.2.3-better-codex", self.root, self.bin
        )
        with self.assertRaisesRegex(RuntimeError, "checksum"):
            install.install_archive(
                archive, "0" * 64, "v1.2.3-better-codex", self.root, self.bin
            )
        self.assertEqual((self.root / "current").resolve(), installed)

    def test_failed_smoke_preserves_previous_selection(self):
        archive, digest = self.archive("1.2.3-better-codex")
        installed = install.install_archive(
            archive, digest, "v1.2.3-better-codex", self.root, self.bin
        )
        broken, digest = self.archive("1.2.4-better-codex", reported_version="wrong")
        with self.assertRaisesRegex(RuntimeError, "version"):
            install.install_archive(
                broken, digest, "v1.2.4-better-codex", self.root, self.bin
            )
        self.assertEqual((self.root / "current").resolve(), installed)

    def test_refuses_an_unmanaged_launcher(self):
        launcher = self.bin / "better-codex"
        launcher.write_text("user-owned command")
        archive, digest = self.archive("1.2.3-better-codex")
        with self.assertRaisesRegex(RuntimeError, "unmanaged"):
            install.install_archive(
                archive, digest, "v1.2.3-better-codex", self.root, self.bin
            )
        self.assertEqual(launcher.read_text(), "user-owned command")
        self.assertFalse((self.root / "current").exists())

    def test_rejects_archive_paths_outside_staging(self):
        archive, digest = self.archive("1.2.3-better-codex", traversal=True)
        with self.assertRaises(tarfile.FilterError):
            install.install_archive(
                archive, digest, "v1.2.3-better-codex", self.root, self.bin
            )
        self.assertFalse((self.root / "escape").exists())
        self.assertFalse((self.root / "current").exists())

    def test_reuses_only_verified_managed_installations(self):
        archive, digest = self.archive("1.2.3-better-codex")
        first = install.install_archive(
            archive, digest, "v1.2.3-better-codex", self.root, self.bin
        )
        self.assertEqual(
            install.install_archive(
                archive, digest, "v1.2.3-better-codex", self.root, self.bin
            ),
            first,
        )
        extra = first / "codex-resources/unexpected-library.so"
        extra.write_bytes(b"changed package closure")
        with self.assertRaisesRegex(RuntimeError, "verification"):
            install.install_archive(
                archive, digest, "v1.2.3-better-codex", self.root, self.bin
            )
        extra.unlink()
        (first / ".better-codex-install.json").unlink()
        with self.assertRaisesRegex(RuntimeError, "unmanaged"):
            install.install_archive(
                archive, digest, "v1.2.3-better-codex", self.root, self.bin
            )


if __name__ == "__main__":
    unittest.main()
