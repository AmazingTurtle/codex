"""Version discovery for Codex packages."""

import re
from pathlib import Path

from .targets import REPO_ROOT


WORKSPACE_VERSION_PATTERN = re.compile(r'^version\s*=\s*"([^"]+)"')


def read_workspace_version(repo_root: Path = REPO_ROOT) -> str:
    cargo_toml = repo_root / "codex-rs" / "Cargo.toml"
    in_workspace_package = False
    with open(cargo_toml, encoding="utf-8") as fh:
        for line in fh:
            stripped = line.strip()
            if stripped == "[workspace.package]":
                in_workspace_package = True
                continue

            if in_workspace_package and stripped.startswith("["):
                break

            if in_workspace_package:
                match = WORKSPACE_VERSION_PATTERN.match(stripped)
                if match is not None:
                    return match.group(1)

    raise RuntimeError(f"Could not find [workspace.package].version in {cargo_toml}")


def read_product_version(repo_root: Path = REPO_ROOT) -> str:
    identity = repo_root / "codex-rs/product-info/src/lib.rs"
    match = re.search(
        r'pub const VERSION:\s*&str\s*=\s*concat!\(\s*env!\("CARGO_PKG_VERSION"\),\s*"(-better-codex(?:\.\d+)?)"\s*,?\s*\);',
        identity.read_text(encoding="utf-8"),
    )
    if match is None:
        raise RuntimeError(
            f"Could not find the downstream version suffix in {identity}"
        )
    return read_workspace_version(repo_root) + match.group(1)
