"""Keep shared-cache snapshot output in the candidate being tested."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


@unittest.skipUnless(sys.platform == "linux", "GNU Linux release test runner")
class SnapshotWorkspaceTests(unittest.TestCase):
    def run_candidate(self, root: Path, environment: dict[str, str]) -> dict:
        runner = root / "scripts/better_codex_release/test_runner.py"
        runner.parent.mkdir(parents=True)
        shutil.copyfile(Path(__file__).with_name("test_runner.py"), runner)
        output = subprocess.check_output(
            [
                sys.executable,
                str(runner),
                sys.executable,
                "-c",
                "import json, os; print(json.dumps({key: os.environ.get(key) for key in "
                "('INSTA_WORKSPACE_ROOT', 'CARGO_MANIFEST_DIR')}))",
            ],
            env=environment,
            text=True,
        )
        return json.loads(output)

    def test_cached_manifest_cannot_redirect_another_candidates_snapshots(self):
        with tempfile.TemporaryDirectory() as temporary:
            roots = [Path(temporary) / name for name in ("first", "second")]
            manifest = str(roots[0] / "codex-rs/tui")
            environment = {**os.environ, "CARGO_MANIFEST_DIR": manifest}
            environment.pop("INSTA_WORKSPACE_ROOT", None)
            for root in roots:
                with self.subTest(candidate=root.name):
                    self.assertEqual(
                        self.run_candidate(root, environment),
                        {
                            "INSTA_WORKSPACE_ROOT": str(root / "codex-rs"),
                            "CARGO_MANIFEST_DIR": manifest,
                        },
                    )

    def test_explicit_workspace_override_is_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "candidate"
            override = str(Path(temporary) / "snapshot-workspace")
            manifest = str(Path(temporary) / "cached-worktree/codex-rs/tui")
            self.assertEqual(
                self.run_candidate(
                    root,
                    {
                        **os.environ,
                        "CARGO_MANIFEST_DIR": manifest,
                        "INSTA_WORKSPACE_ROOT": override,
                    },
                ),
                {
                    "INSTA_WORKSPACE_ROOT": override,
                    "CARGO_MANIFEST_DIR": manifest,
                },
            )
