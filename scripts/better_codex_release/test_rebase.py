"""Exercise candidate rebases and failure handoffs against real local Git remotes."""

import json
from pathlib import Path
import subprocess
import tempfile
import unittest

import rebase


class RebaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="better-codex-rebase-test-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.repo = self.base / "source"
        self.repo.mkdir()
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Rebase test")
        self.git("config", "user.email", "test@example.invalid")
        (self.repo / "codex-rs").mkdir()
        self.manifest = self.repo / "codex-rs/Cargo.toml"
        self.manifest.write_text('[workspace.package]\nversion = "1.2.3"\n')
        (self.repo / "shared.txt").write_text("base\n")
        self.commit("upstream")
        self.git("tag", "rust-v1.2.3")
        self.upstream = self.git("rev-parse", "HEAD")
        identity = self.repo / "codex-rs/product-info/src/lib.rs"
        identity.parent.mkdir(parents=True)
        identity.write_text(
            'pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-better-codex.1");\n'
        )
        self.commit("distribution", "distribution")
        (self.repo / "feature.txt").write_text("retained feature\n")
        self.commit("feature", "example-feature")
        for name in ("origin", "upstream"):
            path = self.base / name
            subprocess.run(
                ["git", "init", "--bare", str(path)], check=True, capture_output=True
            )
            self.git("remote", "add", name, str(path))
        self.git("push", "origin", "main")
        self.git("push", "upstream", "refs/tags/rust-v1.2.3")

    def git(self, *args):
        return subprocess.check_output(
            ["git", *args], cwd=self.repo, text=True, stderr=subprocess.DEVNULL
        ).strip()

    def commit(self, title, owner=None):
        self.git("add", ".")
        message = title if owner is None else f"{title}\n\nBetter-Codex-Patch: {owner}"
        self.git("commit", "-m", message)

    def target(self, *, shared=None):
        source = self.git("rev-parse", "HEAD")
        self.git("checkout", "-b", "upstream-release", self.upstream)
        self.manifest.write_text('[workspace.package]\nversion = "1.2.4"\n')
        if shared is not None:
            (self.repo / "shared.txt").write_text(shared)
        self.commit("new upstream")
        self.git("tag", "rust-v1.2.4")
        self.git("push", "upstream", "refs/tags/rust-v1.2.4")
        self.git("checkout", "main")
        return source

    def test_prepares_a_linear_candidate_without_changing_source(self):
        source = self.target()
        result = rebase.prepare(
            self.repo, self.base / "candidate", self.base / "state.json", "rust-v1.2.4"
        )
        self.assertEqual(
            (result["status"], result["patches"]),
            ("ready", ["distribution", "example-feature"]),
        )
        self.assertEqual(self.git("rev-parse", "HEAD"), source)
        candidate = Path(result["worktree"])
        self.assertEqual((candidate / "feature.txt").read_text(), "retained feature\n")
        self.assertIn(
            '"-better-codex"',
            (candidate / "codex-rs/product-info/src/lib.rs").read_text(),
        )
        parents = subprocess.check_output(
            ["git", "rev-list", "--parents", f"{result['upstream_sha']}..HEAD"],
            cwd=candidate,
            text=True,
        ).splitlines()
        self.assertTrue(all(len(line.split()) == 2 for line in parents))

    def test_conflict_records_owned_recovery_and_leaves_main_unchanged(self):
        (self.repo / "shared.txt").write_text("downstream\n")
        self.commit("shared downstream", "shared-behavior")
        self.git("push", "origin", "main")
        source = self.target(shared="upstream changed\n")
        result = rebase.prepare(
            self.repo, self.base / "candidate", self.base / "state.json", "rust-v1.2.4"
        )
        self.assertEqual(
            (result["status"], result["failed_patch"], result["conflicts"]),
            ("conflict", "shared-behavior", ["shared.txt"]),
        )
        self.assertEqual(self.git("rev-parse", "main"), source)
        self.assertTrue((self.base / "RECOVERY.md").is_file())
        self.assertLessEqual((self.base / "RECOVERY.md").stat().st_size, 4000)

    def test_refuses_target_tag_mismatch_before_creating_worktree(self):
        self.target()
        with self.assertRaisesRegex(RuntimeError, "SHA"):
            rebase.prepare(
                self.repo,
                self.base / "candidate",
                self.base / "state.json",
                "rust-v1.2.4",
                expected_upstream="0" * 40,
            )
        self.assertFalse((self.base / "candidate").exists())

    def test_refuses_a_moved_source_lease(self):
        old = self.target()
        (self.repo / "extra.txt").write_text("concurrent feature\n")
        self.commit("extra", "extra")
        self.git("push", "origin", "main")
        with self.assertRaisesRegex(RuntimeError, "advanced"):
            rebase.prepare(
                self.repo,
                self.base / "candidate",
                self.base / "state.json",
                "rust-v1.2.4",
                source=old,
            )

    def test_resuming_existing_state_does_not_replay_the_candidate(self):
        self.target()
        state = self.base / "state.json"
        first = rebase.prepare(self.repo, self.base / "candidate", state, "rust-v1.2.4")
        self.assertEqual(
            rebase.prepare(self.repo, self.base / "candidate", state, "rust-v1.2.4"),
            first,
        )
        self.assertEqual(json.loads(state.read_text()), first)

    def test_validates_exact_recovered_commit_and_retained_inventory(self):
        source = self.target()
        prepared = rebase.prepare(
            self.repo, self.base / "candidate", self.base / "state.json", "rust-v1.2.4"
        )
        candidate = Path(prepared["worktree"])
        subprocess.run(
            ["git", "push", "origin", f"HEAD:refs/heads/recovery/test"],
            cwd=candidate,
            check=True,
            capture_output=True,
        )
        result = rebase.recover(
            self.repo,
            self.base / "recovered",
            self.base / "recovered-state.json",
            "rust-v1.2.4",
            source,
            "recovery/test",
            prepared["candidate_sha"],
        )
        self.assertEqual(
            (result["status"], result["candidate_sha"], result["patches"]),
            ("ready", prepared["candidate_sha"], prepared["patches"]),
        )
        self.assertEqual(result["conflicts"], [])

    def test_recovery_verifies_dispatched_upstream_sha_before_fetching_candidate(self):
        source = self.target()
        with self.assertRaisesRegex(RuntimeError, "upstream tag SHA"):
            rebase.recover(
                self.repo,
                self.base / "recovered",
                self.base / "state.json",
                "rust-v1.2.4",
                source,
                "recovery/test",
                "0" * 40,
                expected_upstream="0" * 40,
            )
        self.assertFalse((self.base / "recovered").exists())

    def test_recovery_broadens_only_the_edited_owner_paths(self):
        source = self.target()
        prepared = rebase.prepare(
            self.repo, self.base / "candidate", self.base / "state.json", "rust-v1.2.4"
        )
        candidate = Path(prepared["worktree"])
        (candidate / "feature.txt").write_text("resolved feature\n")
        subprocess.run(
            ["git", "add", "feature.txt"],
            cwd=candidate,
            check=True,
            capture_output=True,
        )
        subprocess.run(
            ["git", "commit", "--amend", "--no-edit"],
            cwd=candidate,
            check=True,
            capture_output=True,
        )
        sha = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=candidate, text=True
        ).strip()
        self.assertEqual(
            rebase.recovery_paths(
                self.repo,
                prepared["previous_sha"],
                source,
                prepared["upstream_sha"],
                sha,
            ),
            ["feature.txt"],
        )

    def test_recovery_refuses_a_branch_that_moved(self):
        source = self.target()
        self.git("push", "origin", "HEAD:refs/heads/recovery/test")
        with self.assertRaisesRegex(RuntimeError, "candidate SHA"):
            rebase.recover(
                self.repo,
                self.base / "recovered",
                self.base / "state.json",
                "rust-v1.2.4",
                source,
                "recovery/test",
                "0" * 40,
            )


if __name__ == "__main__":
    unittest.main()
