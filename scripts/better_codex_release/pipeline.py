#!/usr/bin/env python3
"""Validate, package, and optionally promote one exact CI candidate."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

import release
import validation
from environment import cargo_env


def run(
    state_path: Path,
    package: Path,
    artifacts: Path,
    *,
    publish: bool,
    checks_only: bool = False,
) -> None:
    state = json.loads(state_path.read_text())
    release.ROOT = Path(state["repository"])
    if (
        state.get("status") != "ready"
        or release.git("rev-parse", "HEAD") != state["candidate_sha"]
    ):
        raise RuntimeError("Validate only the exact completed candidate")
    os.environ.update(cargo_env(release.ROOT))
    selected = validation.selected_tests(state)
    checks = [
        ("format", [sys.executable, "scripts/format.py", "--check"]),
        (
            "packaging",
            [
                sys.executable,
                "-m",
                "unittest",
                "discover",
                "-s",
                "scripts/better_codex_release",
                "-p",
                "test_*.py",
            ],
        ),
        ("feature-tests", []),
    ]
    lint = ["just", "clippy", "--no-deps"]
    lint_crates = set(selected)
    if "subagent-profiles" in state["patches"]:
        # This crate disables standalone tests; core exercises its public loader.
        lint_crates.add("codex-agent-roles")
    for crate in sorted(lint_crates):
        lint.extend(["-p", crate])
    lint.extend(["--", "-D", "warnings"])
    # The root justfile's Rust recipes expect codex-rs as the working directory.
    lint_script = state_path.with_name("lint.sh")
    import shlex

    lint_script.write_text(
        f"#!/bin/sh\nset -eu\ncd {shlex.quote(str(release.ROOT / 'codex-rs'))}\nexec {shlex.join(lint)}\n"
    )
    checks.insert(2, ("lint", ["sh", str(lint_script)]))
    schema_script = state_path.with_name("schema.sh")
    schema_script.write_text(
        f"#!/bin/sh\nset -eu\ncd {shlex.quote(str(release.ROOT / 'codex-rs'))}\nexec just write-config-schema\n"
    )
    checks.insert(3, ("schema", ["sh", str(schema_script)]))
    for name, command in checks:
        release.check(state_path, state, name, command, cpus=6)
    # Feature validation includes all required regressions and full tests of
    # recovery crates. Reuse that receipt instead of executing them twice.
    state["checks"]["targeted"] = dict(state["checks"]["feature-tests"])
    release.save(state_path, state)
    if "full-tests" in release.required_gates(state):
        release.check(state_path, state, "full-tests", [], cpus=6)
    if checks_only:
        return
    release.check(
        state_path,
        state,
        "build",
        [
            sys.executable,
            str(release.ROOT / "scripts/better_codex_release/package.py"),
            "--package-dir",
            str(package),
            "--output-dir",
            str(artifacts),
            "--cpus",
            "6",
        ],
        cpus=6,
    )
    release.check(
        state_path,
        state,
        "smoke",
        [
            sys.executable,
            str(release.ROOT / "scripts/better_codex_release/smoke.py"),
            str(package),
        ],
        cpus=6,
    )
    review = state_path.with_name("review.md")
    range_diff = release.git(
        "range-diff",
        f"{state['previous_sha']}..{state['old_head']}",
        f"{state['upstream_sha']}..HEAD",
    )
    review.write_text(
        "# Automated candidate review\n\n"
        "Exact upstream SHA, linear feature inventory, regression gates, and final artifact hashes verified.\n\n"
        "```text\n" + range_diff + "\n```\n"
    )
    release.seal(state_path, state, package, review)
    if publish:
        release.publish(state)
        tag = state["sealed"]["tag"]
        notes = state_path.with_name("release-notes.md")
        notes.write_text(
            f"Based on upstream `{state['upstream_tag']}` ({state['upstream_sha']}).\n\n"
            f"Source: `{state['sealed']['head']}`. GNU Linux runtime and matching debug symbols are attached.\n"
            "See `release.json` and `SHA256SUMS` for artifact provenance.\n"
        )
        subprocess.run(
            [
                "gh",
                "release",
                "create",
                tag,
                "--verify-tag",
                "--draft",
                "--title",
                f"Better Codex {state['sealed']['version']}",
                "--notes-file",
                str(notes),
                *state["sealed"]["artifacts"].keys(),
            ],
            cwd=release.ROOT,
            check=True,
        )
        subprocess.run(
            ["gh", "release", "edit", tag, "--draft=false"],
            cwd=release.ROOT,
            check=True,
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--package-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--publish", action="store_true")
    parser.add_argument("--checks-only", action="store_true")
    args = parser.parse_args()
    if args.publish and args.checks_only:
        parser.error("--checks-only cannot publish")
    run(
        args.state.resolve(),
        args.package_dir.resolve(),
        args.output_dir.resolve(),
        publish=args.publish,
        checks_only=args.checks_only,
    )


if __name__ == "__main__":
    main()
