"""Exercise release safeguards against real temporary Git repositories."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest
from unittest.mock import patch

import release


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="better-codex-release-")
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.repo = base / "repo"
        self.repo.mkdir()
        self.root_patch = patch.object(release, "ROOT", self.repo)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Release test")
        self.git("config", "user.email", "release@example.invalid")
        (self.repo / "codex-rs").mkdir()
        (self.repo / "codex-rs/Cargo.toml").write_text(
            '[workspace.package]\nversion = "1.2.3"\n'
        )
        self.git("add", ".")
        self.git("commit", "-m", "upstream release")
        self.git("tag", "rust-v1.2.3")
        self.baseline = self.git("rev-parse", "HEAD")
        for name in ["origin", "upstream"]:
            remote = base / name
            subprocess.run(
                ["git", "init", "--bare", str(remote)], check=True, capture_output=True
            )
            self.git("remote", "add", name, str(remote))
        self.git("push", "upstream", "refs/tags/rust-v1.2.3")
        identity = self.repo / "codex-rs/product-info/src/lib.rs"
        identity.parent.mkdir(parents=True)
        identity.write_text(
            'pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-better-codex");\n'
        )
        self.git("add", ".")
        self.git(
            "commit",
            "--allow-empty",
            "-m",
            "dist: ship Better Codex\n\nBetter-Codex-Patch: distribution",
        )
        self.git("push", "origin", "main")
        self.state_path = base / "state.json"
        self.review = base / "review.md"
        self.review.write_text(
            "Reviewed the complete candidate; no unresolved findings.\n"
        )
        self.package = base / "package"
        for name in [
            "bin/codex",
            "bin/codex-code-mode-host",
            "codex-resources/bwrap",
            "codex-resources/zsh/bin/zsh",
            "codex-path/rg",
        ]:
            file = self.package / name
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text("#!/bin/sh\nexit 0\n")
            file.chmod(0o755)
        (self.package / "codex-package.json").write_text(
            json.dumps(
                {
                    "version": "1.2.3",
                    "variant": "codex",
                    "target": "x86_64-unknown-linux-gnu",
                }
            )
        )

    def git(self, *args):
        return subprocess.check_output(
            ["git", *args], cwd=self.repo, text=True, stderr=subprocess.DEVNULL
        ).strip()

    def prepare(self):
        release.prepare(self.state_path, "rust-v1.2.3", "rust-v1.2.3")
        return json.loads(self.state_path.read_text())

    def receipts(self, state):
        # Synthetic receipts avoid running the real Rust suite in guard tests.
        log = Path(self.temp.name) / "validation.log"
        log.write_text("Summary: 1 test passed\n")
        state["checks"] = {
            name: {
                "tree": self.git("rev-parse", "HEAD^{tree}"),
                "exit_code": 0,
                "log": str(log),
                "sha256": release.digest(log),
            }
            for name in release.GATES
        }
        for name in ("build", "smoke"):
            state["checks"][name]["files"] = release.package_files(
                self.package, "1.2.3"
            )
        return state

    def test_publish_exact_candidate_and_immutable_tag(self):
        state = self.receipts(self.prepare())
        release.seal(self.state_path, state, self.package, self.review)
        release.publish(state)
        refs = release.remote_refs(
            "origin", "refs/heads/main", "refs/tags/v1.2.3-better-codex^{}"
        )
        self.assertEqual(
            refs["refs/heads/main"], refs["refs/tags/v1.2.3-better-codex^{}"]
        )
        with self.assertRaisesRegex(RuntimeError, "already exists remotely"):
            release.publish(state)

    def test_refuse_remote_advance(self):
        state = self.receipts(self.prepare())
        release.seal(self.state_path, state, self.package, self.review)
        self.git("commit", "--allow-empty", "-m", "concurrent update")
        self.git("push", "origin", "main")
        self.git("reset", "--hard", state["old_head"])
        with self.assertRaisesRegex(RuntimeError, "Remote main advanced"):
            release.publish(state)

    def test_refuse_upstream_tag_mismatch(self):
        self.git("tag", "-f", "rust-v1.2.3", "HEAD")
        with self.assertRaises(RuntimeError):
            self.prepare()

    def test_refuse_upstream_main_commits_in_stack(self):
        self.git("commit", "--allow-empty", "-m", "unreleased upstream commit")
        with self.assertRaisesRegex(RuntimeError, "Missing unique patch identifier"):
            self.prepare()

    def test_refuse_duplicate_patch_identifiers(self):
        self.git(
            "commit",
            "--allow-empty",
            "-m",
            "another patch\n\nBetter-Codex-Patch: distribution",
        )
        with self.assertRaisesRegex(RuntimeError, "Duplicate patch"):
            self.prepare()

    def test_refuse_missing_or_failed_validation(self):
        state = self.prepare()
        with self.assertRaisesRegex(RuntimeError, "Missing current successful check"):
            release.seal(self.state_path, state, self.package, self.review)
        self.receipts(state)["checks"]["full-tests"]["exit_code"] = 1
        with self.assertRaisesRegex(RuntimeError, "full-tests"):
            release.seal(self.state_path, state, self.package, self.review)

    def test_failed_retry_invalidates_success(self):
        state = self.receipts(self.prepare())
        affinity = os.sched_getaffinity(0)
        self.addCleanup(os.sched_setaffinity, 0, affinity)
        with self.assertRaisesRegex(RuntimeError, "targeted failed"):
            release.check(
                self.state_path,
                state,
                "targeted",
                [sys.executable, "-c", "raise SystemExit(2)"],
                1,
            )
        self.assertEqual(
            json.loads(self.state_path.read_text())["checks"]["targeted"]["exit_code"],
            2,
        )

    def test_refuse_filtered_full_suite(self):
        state = self.prepare()
        affinity = os.sched_getaffinity(0)
        self.addCleanup(os.sched_setaffinity, 0, affinity)
        with self.assertRaisesRegex(RuntimeError, "filters are not accepted"):
            release.check(
                self.state_path,
                state,
                "full-tests",
                ["just", "test", "-p", "codex-tui"],
                1,
            )

    def test_refuse_changed_package(self):
        state = self.receipts(self.prepare())
        release.seal(self.state_path, state, self.package, self.review)
        (self.package / "bin/codex").write_text("changed")
        with self.assertRaisesRegex(RuntimeError, "Package changed"):
            release.publish(state)

    def test_refuse_changed_validation_log(self):
        state = self.receipts(self.prepare())
        Path(state["checks"]["full-tests"]["log"]).write_text("changed")
        with self.assertRaisesRegex(RuntimeError, "Validation log changed"):
            release.seal(self.state_path, state, self.package, self.review)

    def test_existing_version_uses_downstream_revision(self):
        self.git("tag", "v1.2.3-better-codex")
        self.git("push", "origin", "refs/tags/v1.2.3-better-codex")
        state = self.prepare()
        identity = self.repo / "codex-rs/product-info/src/lib.rs"
        identity.write_text(
            identity.read_text().replace('"-better-codex"', '"-better-codex.1"')
        )
        self.git("add", ".")
        self.git("commit", "--amend", "--no-edit")
        release.seal(self.state_path, self.receipts(state), self.package, self.review)
        self.assertEqual(state["sealed"]["tag"], "v1.2.3-better-codex.1")

    def test_revision_selection_does_not_fill_older_gaps(self):
        self.git("tag", "v1.2.3-better-codex.2")
        self.git("push", "origin", "refs/tags/v1.2.3-better-codex.2")
        state = self.prepare()
        self.assertEqual(state["release_tag"], "v1.2.3-better-codex.3")

    def test_refuse_tag_that_does_not_match_binary_version(self):
        self.git("tag", "v1.2.3-better-codex")
        state = self.receipts(self.prepare())
        with self.assertRaisesRegex(RuntimeError, "VERSION suffix"):
            release.seal(self.state_path, state, self.package, self.review)

    def test_refuse_package_changed_before_sealing(self):
        state = self.receipts(self.prepare())
        (self.package / "bin/codex").write_text("replacement")
        with self.assertRaisesRegex(RuntimeError, "Package differs"):
            release.seal(self.state_path, state, self.package, self.review)

    def test_setup_failure_invalidates_previous_full_suite(self):
        state = self.receipts(self.prepare())
        state["sealed"] = {"head": state["old_head"]}
        affinity = os.sched_getaffinity(0)
        self.addCleanup(os.sched_setaffinity, 0, affinity)
        with patch(
            "codex_package.v8.resolve_codex_v8_cargo_env",
            side_effect=RuntimeError("download failed"),
        ):
            with self.assertRaisesRegex(RuntimeError, "download failed"):
                release.check(self.state_path, state, "full-tests", [], 1)
        saved = json.loads(self.state_path.read_text())
        self.assertNotIn("sealed", saved)
        self.assertNotIn("full-tests", saved["checks"])

    def test_cpu_option_after_gate_name(self):
        self.prepare()
        with patch.object(
            sys,
            "argv",
            [
                "release.py",
                "--state",
                str(self.state_path),
                "check",
                "targeted",
                "--cpus",
                "1",
                "--",
                "true",
            ],
        ):
            with patch.object(release, "check") as run:
                release.main()
        self.assertEqual(run.call_args.args[-2:], (["true"], 1))

    def test_atomic_lease_rejects_race_after_preflight(self):
        state = self.receipts(self.prepare())
        release.seal(self.state_path, state, self.package, self.review)
        original = subprocess.run

        def concurrent_push(command, **kwargs):
            if command[:2] == ["git", "push"]:
                original(
                    [
                        "git",
                        "--git-dir",
                        str(Path(self.temp.name) / "origin"),
                        "update-ref",
                        "refs/heads/main",
                        self.baseline,
                    ],
                    check=True,
                )
            return original(command, **kwargs)

        with patch.object(subprocess, "run", side_effect=concurrent_push):
            with self.assertRaises(subprocess.CalledProcessError):
                release.publish(state)
        refs = release.remote_refs(
            "origin", "refs/heads/main", "refs/tags/v1.2.3-better-codex"
        )
        self.assertEqual(refs, {"refs/heads/main": self.baseline})

    def test_source_release_workflow_checks_exact_downstream_revision(self):
        workflow = (
            Path(release.__file__).resolve().parents[2]
            / ".github/workflows/better-codex-source-release.yml"
        )
        script = textwrap.dedent(
            workflow.read_text().split("run: |", 1)[1].split("      - name:", 1)[0]
        )
        cli = self.repo / "codex-rs/cli/Cargo.toml"
        cli.parent.mkdir()
        cli.write_text('name = "better-codex"\n')
        env = dict(os.environ, GITHUB_REF_NAME="v1.2.3-better-codex")
        subprocess.run(["bash", "-c", script], cwd=self.repo, env=env, check=True)
        env["GITHUB_REF_NAME"] += ".1"
        result = subprocess.run(["bash", "-c", script], cwd=self.repo, env=env)
        self.assertNotEqual(result.returncode, 0)
        identity = self.repo / "codex-rs/product-info/src/lib.rs"
        identity.write_text(
            identity.read_text().replace('"-better-codex"', '"-better-codex.1"')
        )
        subprocess.run(["bash", "-c", script], cwd=self.repo, env=env, check=True)

    def test_refuse_dirty_candidate(self):
        (self.repo / "uncommitted").write_text("work")
        with self.assertRaisesRegex(RuntimeError, "Commit changes"):
            self.prepare()


if __name__ == "__main__":
    unittest.main()
