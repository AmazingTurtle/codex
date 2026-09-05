#!/usr/bin/env python3
"""Smoke-check the assembled GNU Linux package without reading personal state."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from release import ROOT
from release import read_product_version


def main():
    package = Path(sys.argv[1]).resolve()
    metadata = json.loads((package / "codex-package.json").read_text())
    if not read_product_version(ROOT).startswith(f"{metadata['version']}-better-codex"):
        raise RuntimeError("Package workspace version differs from product version")
    commands = [
        ("bin/codex", ["--version"]),
        ("bin/codex", ["--help"]),
        ("bin/codex", ["features", "list"]),
        ("bin/codex", ["completion", "bash"]),
        ("bin/codex-code-mode-host", ["--help"]),
        ("codex-path/rg", ["--version"]),
        ("codex-resources/bwrap", ["--version"]),
        ("codex-resources/zsh/bin/zsh", ["--version"]),
    ]
    with tempfile.TemporaryDirectory(prefix="better-codex-smoke-") as home:
        env = dict(os.environ, BETTER_CODEX_HOME=home)
        for binary, arguments in commands:
            result = subprocess.run(
                [str(package / binary), *arguments],
                env=env,
                text=True,
                capture_output=True,
                timeout=60,
                check=True,
            )
            if not result.stdout.strip():
                raise RuntimeError(f"No output from {binary} {arguments}")
            if binary == "bin/codex" and arguments == ["--version"]:
                expected = f"better-codex {read_product_version(ROOT)}"
                if result.stdout.strip() != expected:
                    raise RuntimeError(f"Expected {expected!r}, got {result.stdout!r}")
            print(
                f"PASS {binary} {' '.join(arguments)}: {result.stdout.splitlines()[0]}"
            )


if __name__ == "__main__":
    main()
