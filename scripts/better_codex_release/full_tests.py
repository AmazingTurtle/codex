#!/usr/bin/env python3
"""Run the unfiltered workspace baseline using the repository's V8/test environment."""

from pathlib import Path
import subprocess
from environment import cargo_env, test_helpers

ROOT = Path(__file__).resolve().parents[2]


def main():
    env = cargo_env(ROOT)
    test_helpers(ROOT, env)
    command = [
        "just",
        "test",
        "--workspace",
        "--test-threads",
        env["RUST_TEST_THREADS"],
    ]
    if "codex-v8-poc" in (ROOT / "codex-rs/Cargo.toml").read_text():
        command.extend(["--features", "codex-v8-poc/sandbox"])
    subprocess.run(command, cwd=ROOT / "codex-rs", env=env, check=True)


if __name__ == "__main__":
    main()
