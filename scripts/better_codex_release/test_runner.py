#!/usr/bin/env python3
"""Isolate known host-dependent Linux test inputs after Cargo launches tests."""

import os
from pathlib import Path
import resource
import subprocess
import sys
import tempfile


def main():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    env = os.environ.copy()
    # Binary lookup helpers must resolve the binary, not this Cargo runner.
    env.pop("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER", None)
    program = Path(sys.argv[1]).name
    if program.startswith("codex_http_client-"):
        # Cargo injects a CA path even when the invoking shell has none. These
        # tests exercise native trust-store behavior and set their own CA cases.
        env.pop("SSL_CERT_FILE", None)
        env.pop("CODEX_CA_CERTIFICATE", None)
    if program.startswith("codex_skills_extension-"):
        # Use separate roots: fixtures can create a .git marker at TMPDIR's
        # parent, and host-installed skills must not enter discovery assertions.
        with tempfile.TemporaryDirectory(
            prefix="better-codex-test-", dir="/var/tmp"
        ) as home:
            original = Path(env["HOME"])
            env.setdefault("CARGO_HOME", str(original / ".cargo"))
            env.setdefault("RUSTUP_HOME", str(original / ".rustup"))
            env["HOME"] = home
            temp = Path(home) / "tmp"
            temp.mkdir()
            env["TMPDIR"] = str(temp)
            return subprocess.run(sys.argv[1:], env=env).returncode
    os.execvpe(sys.argv[1], sys.argv[1:], env)


if __name__ == "__main__":
    sys.exit(main())
