"""Prepare the shared GNU Linux Cargo environment, including restored V8 caches."""

import gzip
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys


def cargo_env(root: Path) -> dict[str, str]:
    os.environ["CODEX_REPO_ROOT"] = str(root)
    sys.path.insert(0, str(root / "scripts"))
    from codex_package.targets import TARGET_SPECS
    from codex_package.v8 import resolve_codex_v8_cargo_env

    allowed = sorted(os.sched_getaffinity(0))[:6]
    os.sched_setaffinity(0, allowed)
    env = {
        **os.environ,
        **resolve_codex_v8_cargo_env(TARGET_SPECS["x86_64-unknown-linux-gnu"]),
        "CARGO_BUILD_JOBS": str(len(allowed)),
        "RUST_TEST_THREADS": str(len(allowed)),
        "RAYON_NUM_THREADS": str(len(allowed)),
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER": shlex.join(
            [sys.executable, str(Path(__file__).with_name("test_runner.py"))]
        ),
    }
    # rust-cache prunes gn_out but can retain the build-script fingerprint.
    # Tests use the native target directory; packaging uses an explicit triple.
    if archive := env.get("RUSTY_V8_ARCHIVE"):
        target = Path(env.get("CARGO_TARGET_DIR", root / "codex-rs/target"))
        for prefix in (target, target / "x86_64-unknown-linux-gnu"):
            for profile in ("debug", "release"):
                library = prefix / profile / "gn_out/obj/librusty_v8.a"
                if not library.exists():
                    library.parent.mkdir(parents=True, exist_ok=True)
                    with gzip.open(archive, "rb") as source, library.open("wb") as dest:
                        shutil.copyfileobj(source, dest)
    return env


def test_helpers(root: Path, env: dict[str, str]) -> None:
    command = [
        "cargo",
        "build",
        "-p",
        "codex-linux-sandbox",
        "-p",
        "codex-bwrap",
        "-p",
        "codex-code-mode-host",
        "--bins",
    ]
    if "codex-v8-poc" in (root / "codex-rs/Cargo.toml").read_text():
        command.extend(["-p", "codex-v8-poc", "--features", "codex-v8-poc/sandbox"])
    subprocess.run(command, cwd=root / "codex-rs", env=env, check=True)
