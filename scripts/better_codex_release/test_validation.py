import unittest
from pathlib import Path
import tempfile

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
