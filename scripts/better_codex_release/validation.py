#!/usr/bin/env python3
"""Run retained-feature regressions once, plus full crate tests for recovery edits."""

import argparse
import json
import os
from pathlib import Path
import subprocess
from environment import cargo_env, test_helpers


FEATURES = {
    "distribution": {
        "codex-cli": "test(/import_codex_state|update|cli_help|doctor|exec_server_help|strict_config_is_not_supported|sandbox_fetches_and_enforces/)",
        "codex-utils-home-dir": "all()",
        "codex-rmcp-client": "test(/credential|oauth/)",
    },
    "multi-account": {
        "codex-app-server-protocol": "test(schema_fixtures_tests::)",
        "codex-login": "test(/account|rotation|round_robin/)",
        "codex-core": "test(/account|round_robin|usage_limit|session_follows/)",
        "codex-app-server": "test(/account|rate_limit/)",
        "codex-tui": "test(/account|usage|status/)",
    },
    "activity-display": {"codex-tui": "test(/exec_cell|history_cell|activity/)"},
    "opaque-request-retry": {"codex-api": "test(/bad_request|opaque/)"},
    "test-file-mcp-import": {"codex-core": "test(openai_file_mcp)"},
    "test-terminal-capture": {"codex-tui": "test(/terminal/)"},
    "test-pty-path-lookup": {"codex-utils-pty": "all()"},
    "bash-snapshot": {"codex-exec-server": "test(/snapshot/)"},
    "release-maintenance": {},
    "telemetry-schema-compat": {
        "codex-state": "test(/migration/)",
        "codex-app-server-protocol": "test(schema_fixtures_tests::)",
    },
    "debloat-policy": {
        "codex-config": "test(/debloat/)",
        "codex-core": "test(/debloat|skills|mcp/)",
        "codex-cli": "test(/debloat|plugin/)",
    },
    "review-capacity-reset": {"codex-tui": "test(/review/)"},
    "background-exec-completion": {"codex-core": "test(unified_exec_completion::)"},
    "test-core-fixtures": {"codex-core": "test(/hooks|sandbox/)"},
    "reasoning-display": {
        "codex-tui": "test(/reasoning|status_indicator/)",
        "codex-exec": "test(/reasoning/)",
    },
    "runtime-wake-state": {
        "codex-app-server-protocol": "test(schema_fixtures_tests::)",
        "codex-core": "test(wake_state::)",
        "codex-tui": "test(/wake|waiting/)",
    },
    "test-voice-capture": {},
    "long-context": {
        "codex-core": "test(long_context::)",
        "codex-tui": "test(long_context)",
    },
    "subagent-profiles": {
        "codex-core": "test(/subagent_profiles|agent_role|message_tests|spawn_agent_description|spawn_settings|multi_agent_resume|models_cache_ttl/)",
        "codex-app-server": "test(cached_executor_mcp_cannot_read_host_token_after_capability_downgrade)",
    },
    "provider-response-compat": {"codex-core": "test(client_compat::)"},
}


def selection(patches: list[str], registry: dict = FEATURES) -> dict[str, str]:
    selected: dict[str, set[str]] = {}
    for owner in patches:
        if owner not in registry:
            raise RuntimeError(f"No regression coverage mapping for patch '{owner}'")
        for crate, expression in registry[owner].items():
            selected.setdefault(crate, set()).add(expression)
    return {crate: " | ".join(sorted(filters)) for crate, filters in selected.items()}


def coverage_count(listing: dict, label: str) -> int:
    count = sum(
        1
        for suite in listing["rust-suites"].values()
        for case in suite.get("testcases", {}).values()
        if not case["ignored"] and case["filter-match"]["status"] == "matches"
    )
    if not count:
        raise RuntimeError(f"No runnable regressions selected for {label}")
    return count


def selected_tests(state: dict) -> dict[str, str]:
    root = Path(state["repository"])
    selected = selection(state["patches"])
    recovery_crates = set(state.get("recovery_crates", []))
    for path in state.get("conflicts", []):
        for directory in (root / path).parents:
            manifest = directory / "Cargo.toml"
            if manifest.is_file() and directory != root / "codex-rs":
                import tomllib

                package = (
                    tomllib.loads(manifest.read_text()).get("package", {}).get("name")
                )
                if package is not None:
                    recovery_crates.add(package)
                    break
            if directory == root:
                break
    for crate in recovery_crates:
        selected[crate] = "all()"
    return selected


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--cpus", type=int, default=6)
    parser.add_argument("--scope-only", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.cpus <= 6:
        parser.error("CPU count must be between one and six")
    allowed = sorted(os.sched_getaffinity(0))[: args.cpus]
    os.sched_setaffinity(0, allowed)
    state = json.loads(args.state.read_text())
    selected = selected_tests(state)
    if args.scope_only:
        print(json.dumps(sorted(selected)))
        return
    root = Path(state["repository"])
    current = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True
    ).strip()
    if state.get("candidate_sha", current) != current:
        raise RuntimeError(
            "Candidate SHA changed; start validation for the new candidate"
        )
    env = {
        **cargo_env(root),
        "CODEX_REPO_ROOT": str(root),
        "CARGO_BUILD_JOBS": str(len(allowed)),
        "RUST_TEST_THREADS": str(len(allowed)),
        "RAYON_NUM_THREADS": str(len(allowed)),
    }
    # One invocation builds each selected crate once. Package-qualified filters
    # prevent a broad filter for one crate from expanding the others' test scope.
    expression = " | ".join(
        f"(package({crate}) & ({filter}))" for crate, filter in sorted(selected.items())
    )
    # Build once and check each owner's filter against the same binaries before
    # running their union. An unrelated match cannot conceal obsolete coverage.
    binaries = args.state.with_name("test-binaries.json")
    metadata = args.state.with_name("cargo-metadata.json")
    build = [
        "cargo",
        "nextest",
        "list",
        "--list-type",
        "binaries-only",
        "--message-format",
        "json",
    ]
    if "codex-v8-poc" in (root / "codex-rs/Cargo.toml").read_text():
        build.extend(["--features", "codex-v8-poc/sandbox"])
    if state.get("validation_policy") == "bootstrap":
        build.append("--workspace")
    else:
        for crate in sorted(selected):
            build.extend(["-p", crate])
        if "codex-v8-poc" in (root / "codex-rs/Cargo.toml").read_text():
            build.extend(["-p", "codex-v8-poc"])
    if "codex-core" in selected:
        test_helpers(root, env)
    with binaries.open("w") as output:
        subprocess.run(build, cwd=root / "codex-rs", env=env, stdout=output, check=True)
    with metadata.open("w") as output:
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1"],
            cwd=root / "codex-rs",
            env=env,
            stdout=output,
            check=True,
        )
    reuse = [
        "--binaries-metadata",
        str(binaries.resolve()),
        "--cargo-metadata",
        str(metadata.resolve()),
    ]
    for owner in state["patches"]:
        for crate, filter in FEATURES[owner].items():
            listing = subprocess.check_output(
                [
                    "cargo",
                    "nextest",
                    "list",
                    *reuse,
                    "--message-format",
                    "json",
                    "-E",
                    f"package({crate}) & ({filter})",
                ],
                cwd=root / "codex-rs",
                env=env,
                text=True,
            )
            count = coverage_count(json.loads(listing), f"{owner}/{crate}")
            print(f"Coverage: {owner}/{crate}: {count} runnable tests", flush=True)
    command = ["just", "test", *reuse]
    command.extend(
        ["-E", expression, "--no-tests=fail", "--test-threads", str(len(allowed))]
    )
    subprocess.run(command, cwd=root / "codex-rs", env=env, check=True)


if __name__ == "__main__":
    main()
