#!/usr/bin/env python3
"""Run focused gates for an exact pull-request commit without publishing artifacts."""

import argparse
from pathlib import Path

import pipeline
import rebase
import release


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, required=True)
    args = parser.parse_args()
    root = release.ROOT
    head = release.git("rev-parse", "HEAD")
    tags = release.git(
        "for-each-ref",
        "--merged",
        head,
        "--format=%(refname:short)",
        "refs/tags/rust-v*",
    ).splitlines()
    base = max(
        (tuple(map(int, match.groups())), tag)
        for tag in tags
        if (match := rebase.STABLE.fullmatch(tag))
    )[1]
    sha = release.git("rev-parse", f"{base}^{{commit}}")
    state = {
        "repository": str(root),
        "status": "ready",
        "candidate_sha": head,
        "upstream_tag": base,
        "upstream_sha": sha,
        "release_tag": f"v{release.read_product_version(root)}",
        "patches": rebase.inventory(root, sha, head),
        "checks": {},
        "validation_policy": "focused",
    }
    args.state.parent.mkdir(parents=True, exist_ok=True)
    release.save(args.state, state)
    pipeline.run(
        args.state.resolve(),
        args.state.parent / "package",
        args.state.parent / "artifacts",
        publish=False,
        checks_only=True,
    )


if __name__ == "__main__":
    main()
