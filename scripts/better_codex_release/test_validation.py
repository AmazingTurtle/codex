from contextlib import redirect_stdout
import io
import json
import subprocess
import unittest
from pathlib import Path
import tempfile
from unittest.mock import patch

import pipeline
import validation


class CoverageTests(unittest.TestCase):
    def test_recovery_paths_broaden_the_owning_crate_only(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            crate = root / "codex-rs/core"
            crate.mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "codex-core"\n')
            state = {
                "repository": str(root),
                "patches": ["runtime-wake-state"],
                "conflicts": ["codex-rs/core/src/new.rs"],
            }
            expected = validation.selection(state["patches"])
            expected["codex-core"] = "all()"
            self.assertEqual(validation.selected_tests(state), expected)

    def test_ignored_or_filtered_tests_do_not_satisfy_a_regression_contract(self):
        listing = {
            "rust-suites": {
                "crate": {
                    "testcases": {
                        "ignored": {
                            "ignored": True,
                            "filter-match": {"status": "matches"},
                        },
                        "filtered": {
                            "ignored": False,
                            "filter-match": {"status": "mismatch"},
                        },
                    }
                }
            }
        }
        with self.assertRaisesRegex(RuntimeError, "No runnable regressions.*wake/core"):
            validation.coverage_count(listing, "wake/core")
        listing["rust-suites"]["crate"]["testcases"]["selected"] = {
            "ignored": False,
            "filter-match": {"status": "matches"},
        }
        self.assertEqual(validation.coverage_count(listing, "wake/core"), 1)

    def test_combines_shared_crate_filters_without_duplicate_runs(self):
        registry = {
            "accounts": {"core": "test(accounts)", "login": "all()"},
            "wake": {"core": "test(wake)"},
        }
        self.assertEqual(
            validation.selection(["accounts", "wake"], registry),
            {"core": "test(accounts) | test(wake)", "login": "all()"},
        )

    def test_unknown_feature_cannot_silently_skip_coverage(self):
        with self.assertRaisesRegex(RuntimeError, "No regression coverage"):
            validation.selection(["new-feature"], {})

    def test_compile_only_sample_build_is_required_and_failure_stops_regressions(self):
        for owner, has_v8, build_fails in [
            ("dynamic-workflows", False, False),
            ("dynamic-workflows", True, False),
            ("dynamic-workflows", True, True),
            ("opaque-request-retry", True, False),
        ]:
            with (
                self.subTest(owner=owner, has_v8=has_v8, build_fails=build_fails),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary)
                (root / "codex-rs").mkdir()
                (root / "codex-rs/Cargo.toml").write_text(
                    "codex-v8-poc" if has_v8 else "[workspace]"
                )
                state = root / "state.json"
                state.write_text(
                    json.dumps(
                        {
                            "repository": str(root),
                            "patches": [owner],
                            "candidate_sha": "candidate",
                        }
                    )
                )
                listing = json.dumps(
                    {
                        "rust-suites": {
                            "crate": {
                                "testcases": {
                                    "regression": {
                                        "ignored": False,
                                        "filter-match": {"status": "matches"},
                                    }
                                }
                            }
                        }
                    }
                )

                def output(command, **_kwargs):
                    if command[0] == "git":
                        return "candidate\n"
                    self.assertNotIn(
                        "package(codex-thread-manager-sample)", command[-1]
                    )
                    return listing

                def run(command, **_kwargs):
                    if command[:2] == ["cargo", "build"] and build_fails:
                        raise subprocess.CalledProcessError(1, command)

                with (
                    patch(
                        "sys.argv",
                        ["validation.py", "--state", str(state), "--cpus", "2"],
                    ),
                    patch.object(
                        validation.os, "sched_getaffinity", return_value={0, 1, 2}
                    ),
                    patch.object(validation.os, "sched_setaffinity"),
                    patch.object(
                        validation, "cargo_env", return_value={"V8_ENV": "retained"}
                    ),
                    patch.object(validation, "test_helpers"),
                    patch.object(
                        validation.subprocess, "check_output", side_effect=output
                    ),
                    patch.object(
                        validation.subprocess, "run", side_effect=run
                    ) as commands,
                    redirect_stdout(io.StringIO()),
                ):
                    if build_fails:
                        with self.assertRaises(subprocess.CalledProcessError):
                            validation.main()
                    else:
                        validation.main()
                builds = [
                    call
                    for call in commands.call_args_list
                    if call.args[0][:2] == ["cargo", "build"]
                ]
                self.assertEqual(len(builds), int(owner == "dynamic-workflows"))
                if builds:
                    expected = [
                        "cargo",
                        "build",
                        "-p",
                        "codex-thread-manager-sample",
                        "--bin",
                        "codex-thread-manager-sample",
                    ]
                    if has_v8:
                        expected.extend(
                            ["-p", "codex-v8-poc", "--features", "codex-v8-poc/sandbox"]
                        )
                    self.assertEqual(builds[0].args[0], expected)
                    self.assertEqual(
                        builds[0].kwargs,
                        {
                            "cwd": root / "codex-rs",
                            "env": {
                                "V8_ENV": "retained",
                                "CODEX_REPO_ROOT": str(root),
                                "CARGO_BUILD_JOBS": "2",
                                "RUST_TEST_THREADS": "2",
                                "RAYON_NUM_THREADS": "2",
                            },
                            "check": True,
                        },
                    )
                test_runs = [
                    call
                    for call in commands.call_args_list
                    if call.args[0][:2] == ["just", "test"]
                ]
                self.assertEqual(len(test_runs), int(not build_fails))
                if build_fails:
                    self.assertEqual(len(commands.call_args_list), 1)

    def test_compile_only_sample_lint_remains_in_the_feature_gate(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "codex-rs").mkdir()
            (root / "codex-rs/Cargo.toml").write_text("codex-v8-poc")
            state = root / "state.json"
            state.write_text(
                json.dumps(
                    {
                        "repository": str(root),
                        "patches": ["dynamic-workflows"],
                        "status": "ready",
                        "candidate_sha": "candidate",
                        "checks": {},
                    }
                )
            )

            def check(_path, candidate, name, _command, **_kwargs):
                candidate["checks"][name] = {"status": "passed"}

            with (
                patch.object(pipeline.release, "ROOT", root),
                patch.object(pipeline.release, "git", return_value="candidate"),
                patch.object(pipeline, "cargo_env", return_value={}),
                patch.object(pipeline.release, "check", side_effect=check),
                patch.object(pipeline.release, "save"),
                patch.object(pipeline.release, "required_gates", return_value=[]),
            ):
                pipeline.run(
                    state,
                    root / "package",
                    root / "artifacts",
                    publish=False,
                    checks_only=True,
                )
            script = (root / "lint.sh").read_text().splitlines()
            self.assertEqual(script[1], "set -eu")
            self.assertTrue(script[-2].startswith("just clippy --no-deps "))
            self.assertEqual(
                script[-1],
                "exec cargo clippy --no-deps -p codex-thread-manager-sample --bin codex-thread-manager-sample -p codex-v8-poc --features codex-v8-poc/sandbox -- -D warnings",
            )
