"""Exercise native and explicit-target cache restoration with a tiny V8 archive."""

import gzip
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from environment import cargo_env


class EnvironmentTests(unittest.TestCase):
    def test_restores_both_layouts_without_overwriting_existing_libraries(self):
        with tempfile.TemporaryDirectory(prefix="better-codex-env-") as temporary:
            root = Path(temporary)
            archive = root / "v8.a.gz"
            with gzip.open(archive, "wb") as stream:
                stream.write(b"V8 test archive")
            target = root / "cache with spaces"
            existing = target / "debug/gn_out/obj/librusty_v8.a"
            existing.parent.mkdir(parents=True)
            existing.write_bytes(b"existing library")
            affinity = os.sched_getaffinity(0)
            self.addCleanup(os.sched_setaffinity, 0, affinity)
            with patch.dict(
                os.environ,
                {
                    "CARGO_TARGET_DIR": str(target),
                    "RUSTY_V8_ARCHIVE": str(archive),
                    "RUSTY_V8_SRC_BINDING_PATH": str(root / "binding.rs"),
                },
            ):
                env = cargo_env(root)
            self.assertEqual(existing.read_bytes(), b"existing library")
            for relative in [
                "release",
                "x86_64-unknown-linux-gnu/debug",
                "x86_64-unknown-linux-gnu/release",
            ]:
                self.assertEqual(
                    (target / relative / "gn_out/obj/librusty_v8.a").read_bytes(),
                    b"V8 test archive",
                )
            self.assertLessEqual(int(env["CARGO_BUILD_JOBS"]), 6)


if __name__ == "__main__":
    unittest.main()
