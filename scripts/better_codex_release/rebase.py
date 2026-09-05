#!/usr/bin/env python3
"""Prepare an exact, isolated stable-tag rebase or a reproducible recovery handoff."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess


STABLE = re.compile(r"rust-v(\d+)\.(\d+)\.(\d+)")


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True
    )
    if result.returncode:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def inventory(repo: Path, base: str, head: str) -> list[str]:
    result, previous = [], base
    for commit in git(repo, "rev-list", "--reverse", f"{base}..{head}").splitlines():
        if git(repo, "show", "-s", "--format=%P", commit).split() != [previous]:
            raise RuntimeError("Downstream stack must remain linear")
        owner = git(
            repo,
            "show",
            "-s",
            "--format=%(trailers:key=Better-Codex-Patch,valueonly)",
            commit,
        ).splitlines()
        if len(owner) != 1 or not owner[0] or owner[0] in result:
            raise RuntimeError(f"Missing or duplicate patch owner in {commit}")
        if git(repo, "show", "-s", "--format=%s", commit).startswith(
            ("fixup!", "squash!")
        ):
            raise RuntimeError("Autosquash before preparing a new release")
        result.append(owner[0])
        previous = commit
    if not result:
        raise RuntimeError("Downstream feature inventory is empty")
    return result


def save(path: Path, state: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, indent=2) + "\n")
    temporary.replace(path)


def prepare(
    repo: Path,
    worktree: Path,
    state_path: Path,
    upstream_tag: str,
    *,
    source: str = "HEAD",
    expected_upstream: str | None = None,
) -> dict:
    repo, worktree, state_path = (
        repo.resolve(),
        worktree.resolve(),
        state_path.resolve(),
    )
    if STABLE.fullmatch(upstream_tag) is None:
        raise RuntimeError("Select an exact stable upstream rust-vX.Y.Z tag")
    source_sha = git(repo, "rev-parse", f"{source}^{{commit}}")
    if state_path.exists():
        state = json.loads(state_path.read_text())
        if (state["old_head"], state["upstream_tag"], state["worktree"]) != (
            source_sha,
            upstream_tag,
            str(worktree),
        ):
            raise RuntimeError(
                "Existing candidate state belongs to a different attempt"
            )
        if expected_upstream is not None and state["upstream_sha"] != expected_upstream:
            raise RuntimeError(
                "Existing candidate upstream SHA differs from the dispatch"
            )
        return state
    if git(repo, "status", "--porcelain"):
        raise RuntimeError("Prepare from a clean source checkout")
    refs = dict(
        line.split()[::-1]
        for line in git(repo, "ls-remote", "origin", "refs/heads/main").splitlines()
    )
    expected_main = refs.get("refs/heads/main")
    if expected_main != source_sha:
        raise RuntimeError(
            "Remote main advanced from the selected source; start a fresh attempt"
        )
    tags = git(
        repo,
        "for-each-ref",
        "--merged",
        source_sha,
        "--format=%(refname:short)",
        "refs/tags/rust-v*",
    ).splitlines()
    stable = [
        (tuple(map(int, STABLE.fullmatch(tag).groups())), tag)
        for tag in tags
        if STABLE.fullmatch(tag)
    ]
    if not stable:
        raise RuntimeError("The source has no stable upstream base")
    previous_tag = max(stable)[1]
    if tuple(map(int, STABLE.fullmatch(upstream_tag).groups())) < max(stable)[0]:
        raise RuntimeError("Refusing to downgrade the maintained upstream release")
    previous_sha = git(repo, "rev-parse", f"{previous_tag}^{{commit}}")
    patches = inventory(repo, previous_sha, source_sha)
    remote = dict(
        line.split()[::-1]
        for line in git(
            repo,
            "ls-remote",
            "upstream",
            f"refs/tags/{upstream_tag}",
            f"refs/tags/{upstream_tag}^{{}}",
        ).splitlines()
    )
    upstream_sha = remote.get(
        f"refs/tags/{upstream_tag}^{{}}", remote.get(f"refs/tags/{upstream_tag}")
    )
    if upstream_sha is None or (
        expected_upstream is not None and expected_upstream != upstream_sha
    ):
        raise RuntimeError("Upstream tag SHA does not match the requested release")
    local = subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "rev-parse",
            "--verify",
            f"refs/tags/{upstream_tag}^{{commit}}",
        ],
        capture_output=True,
        text=True,
    )
    if local.returncode == 0:
        if local.stdout.strip() != upstream_sha:
            raise RuntimeError(
                "Local upstream tag SHA differs; immutable tags are not overwritten"
            )
    else:
        git(
            repo,
            "fetch",
            "--no-tags",
            "upstream",
            f"refs/tags/{upstream_tag}:refs/tags/{upstream_tag}",
        )
    branch = f"candidate/{upstream_tag}/{source_sha[:12]}-{worktree.name}"
    backup = f"refs/heads/backup/pre-{upstream_tag}/{source_sha}"
    git(repo, "update-ref", backup, source_sha)
    git(repo, "worktree", "add", "-b", branch, str(worktree), source_sha)
    git(worktree, "config", "rerere.enabled", "true")
    git(worktree, "config", "rerere.autoupdate", "false")
    state = {
        "repository": str(worktree),
        "worktree": str(worktree),
        "candidate_branch": branch,
        "old_head": source_sha,
        "expected_remote_main": expected_main,
        "previous_tag": previous_tag,
        "previous_sha": previous_sha,
        "upstream_tag": upstream_tag,
        "upstream_sha": upstream_sha,
        "patches": patches,
        "backup_ref": backup,
        "checks": {},
        "validation_policy": "focused",
        "status": "preparing",
        "candidate_sha": source_sha,
    }
    save(state_path, state)
    state_path.parent.mkdir(parents=True, exist_ok=True)
    log = state_path.with_name("rebase.log")
    with log.open("w") as stream:
        result = subprocess.run(
            [
                "git",
                "-C",
                str(worktree),
                "rebase",
                "--onto",
                upstream_sha,
                previous_sha,
            ],
            env={**os.environ, "GIT_EDITOR": "true"},
            stdout=stream,
            stderr=subprocess.STDOUT,
        )
    if result.returncode:
        state["status"] = "conflict"
        state["candidate_sha"] = git(worktree, "rev-parse", "HEAD")
        state["conflicts"] = git(
            worktree, "diff", "--name-only", "--diff-filter=U"
        ).splitlines()
        failed = subprocess.run(
            [
                "git",
                "-C",
                str(worktree),
                "show",
                "-s",
                "--format=%(trailers:key=Better-Codex-Patch,valueonly)",
                "REBASE_HEAD",
            ],
            capture_output=True,
            text=True,
        )
        state["failed_patch"] = failed.stdout.strip() or "rebase-preparation"
        state["log"] = str(log)
        save(state_path, state)
        details = "\n".join(f"- {path}" for path in state["conflicts"][:10])
        summary = (
            f"# Recover {upstream_tag}\n\n"
            f"Source: `{source_sha}`\nTarget: `{upstream_sha}`\n"
            f"Failed owner: `{state['failed_patch']}`\n\n{details}\n\n"
            "This is an incomplete candidate. Preserve the full feature inventory, resolve only "
            "the failed patches, and continue the rebase. Fold fixes into their owners before validation.\n\n"
            f"Full diagnostics: {log.name}\nState: {state_path.name}\n"
            f"Recreate: `python3 scripts/better_codex_release/rebase.py --upstream-tag {upstream_tag} "
            f"--source {source_sha} --upstream-sha {upstream_sha} --worktree <fresh-path> --state <fresh-state>`\n"
        )
        state_path.with_name("RECOVERY.md").write_text(summary[:4000])
        return state
    return finish(repo, worktree, state_path, state)


def recovery_paths(
    repo: Path, old_base: str, old_head: str, new_base: str, new_head: str
) -> list[str]:
    """Broaden tests only for changed owner patches, excluding version stamping."""

    def payload(commit: str) -> str:
        lines = []
        identity = False
        for line in git(
            repo,
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-renames",
            "--unified=0",
            f"{commit}^",
            commit,
        ).splitlines():
            if line.startswith("diff --git "):
                identity = line.endswith(" b/codex-rs/product-info/src/lib.rs")
            if line.startswith(("index ", "@@")):
                continue
            if identity and "pub const VERSION:" in line:
                line = re.sub(r'"-better-codex(?:\.\d+)?"', '"-better-codex"', line)
            lines.append(line)
        return "\n".join(lines)

    old = git(repo, "rev-list", "--reverse", f"{old_base}..{old_head}").splitlines()
    new = git(repo, "rev-list", "--reverse", f"{new_base}..{new_head}").splitlines()
    if len(old) != len(new):
        raise RuntimeError("Recovery patch count differs from the maintained source")
    paths = set()
    for before, after in zip(old, new):
        if payload(before) != payload(after):
            for commit in (before, after):
                paths.update(
                    git(
                        repo, "diff-tree", "--no-commit-id", "--name-only", "-r", commit
                    ).splitlines()
                )
    return sorted(paths)


def recover(
    repo: Path,
    worktree: Path,
    state_path: Path,
    upstream_tag: str,
    source_sha: str,
    branch: str,
    candidate_sha: str,
    *,
    expected_upstream: str | None = None,
) -> dict:
    repo, worktree, state_path = (
        repo.resolve(),
        worktree.resolve(),
        state_path.resolve(),
    )
    if STABLE.fullmatch(upstream_tag) is None or any(
        re.fullmatch(r"[0-9a-f]{40}", sha) is None
        for sha in (source_sha, candidate_sha)
    ):
        raise RuntimeError(
            "Recovery requires an exact stable tag and full source/candidate SHAs"
        )
    git(repo, "check-ref-format", f"refs/heads/{branch}")
    remote = git(repo, "ls-remote", "origin", "refs/heads/main").split()
    if not remote or remote[0] != source_sha:
        raise RuntimeError("Remote main advanced from the recovery source")
    refs = dict(
        line.split()[::-1]
        for line in git(
            repo,
            "ls-remote",
            "upstream",
            f"refs/tags/{upstream_tag}",
            f"refs/tags/{upstream_tag}^{{}}",
        ).splitlines()
    )
    upstream_sha = refs.get(
        f"refs/tags/{upstream_tag}^{{}}", refs.get(f"refs/tags/{upstream_tag}")
    )
    if upstream_sha is None or (
        expected_upstream is not None and expected_upstream != upstream_sha
    ):
        raise RuntimeError(
            "Recovery upstream tag SHA differs from the requested release"
        )
    git(repo, "fetch", "--no-tags", "origin", f"refs/heads/{branch}")
    if git(repo, "rev-parse", "FETCH_HEAD^{commit}") != candidate_sha:
        raise RuntimeError("Recovery branch moved from the nominated candidate SHA")
    local_tag = subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "rev-parse",
            "--verify",
            f"refs/tags/{upstream_tag}^{{commit}}",
        ],
        capture_output=True,
        text=True,
    )
    if local_tag.returncode:
        git(
            repo,
            "fetch",
            "--no-tags",
            "upstream",
            f"refs/tags/{upstream_tag}:refs/tags/{upstream_tag}",
        )
    elif local_tag.stdout.strip() != upstream_sha:
        raise RuntimeError("Local upstream tag SHA differs from the recovery target")
    tags = git(
        repo,
        "for-each-ref",
        "--merged",
        source_sha,
        "--format=%(refname:short)",
        "refs/tags/rust-v*",
    ).splitlines()
    previous_tag = max(
        (tuple(map(int, match.groups())), tag)
        for tag in tags
        if (match := STABLE.fullmatch(tag))
    )[1]
    if tuple(map(int, STABLE.fullmatch(upstream_tag).groups())) < tuple(
        map(int, STABLE.fullmatch(previous_tag).groups())
    ):
        raise RuntimeError("Refusing to downgrade the maintained upstream release")
    previous_sha = git(repo, "rev-parse", f"{previous_tag}^{{commit}}")
    patches = inventory(repo, previous_sha, source_sha)
    if inventory(repo, upstream_sha, candidate_sha) != patches:
        raise RuntimeError(
            "Recovery must preserve the complete linear feature inventory"
        )
    git(repo, "worktree", "add", "--detach", str(worktree), candidate_sha)
    state = {
        "repository": str(worktree),
        "worktree": str(worktree),
        "candidate_branch": branch,
        "old_head": source_sha,
        "expected_remote_main": source_sha,
        "previous_tag": previous_tag,
        "previous_sha": previous_sha,
        "upstream_tag": upstream_tag,
        "upstream_sha": upstream_sha,
        "patches": patches,
        "checks": {},
        "validation_policy": "focused",
        "expected_candidate_branch": candidate_sha,
    }
    # Unchanged retained patches still run their focused regressions. Changed
    # owner patches additionally run full tests for their affected crates.
    state["conflicts"] = recovery_paths(
        repo, previous_sha, source_sha, upstream_sha, candidate_sha
    )
    return finish(repo, worktree, state_path, state, require_committed_version=True)


def finish(
    repo: Path,
    worktree: Path,
    state_path: Path,
    state: dict,
    *,
    require_committed_version=False,
) -> dict:
    state.update(status="replayed", candidate_sha=git(worktree, "rev-parse", "HEAD"))
    save(state_path, state)
    upstream_tag, upstream_sha = state["upstream_tag"], state["upstream_sha"]
    patches = state["patches"]
    version = upstream_tag.removeprefix("rust-v")
    prefix = f"v{version}-better-codex"
    existing = set(git(repo, "tag", "--list", f"{prefix}*").splitlines())
    existing.update(
        line.split()[1].removeprefix("refs/tags/")
        for line in git(
            repo, "ls-remote", "--tags", "origin", f"refs/tags/{prefix}*"
        ).splitlines()
    )
    revisions = [
        int(match.group(1) or 0)
        for tag in existing
        if (match := re.fullmatch(re.escape(prefix) + r"(?:\.(\d+))?", tag))
    ]
    revision = max(revisions) + 1 if revisions else 0
    suffix = f"-better-codex.{revision}" if revision else "-better-codex"
    identity = worktree / "codex-rs/product-info/src/lib.rs"
    text = identity.read_text()
    updated, count = re.subn(r"(?<=\")-better-codex(?:\.\d+)?(?=\")", suffix, text)
    if count != 1:
        raise RuntimeError(
            "The distribution patch must define exactly one product version suffix"
        )
    if updated != text:
        if require_committed_version:
            raise RuntimeError(
                "Stamp the selected release version in the distribution owner before recovery dispatch"
            )
        identity.write_text(updated)
        commits = git(worktree, "rev-list", f"{upstream_sha}..HEAD").splitlines()
        owner = next(
            commit
            for commit in commits
            if git(
                worktree,
                "show",
                "-s",
                "--format=%(trailers:key=Better-Codex-Patch,valueonly)",
                commit,
            )
            == "distribution"
        )
        git(worktree, "add", "codex-rs/product-info/src/lib.rs")
        git(worktree, "commit", f"--fixup={owner}")
        subprocess.run(
            ["git", "-C", str(worktree), "rebase", "-i", "--autosquash", upstream_sha],
            env={**os.environ, "GIT_SEQUENCE_EDITOR": "true", "GIT_EDITOR": "true"},
            check=True,
            capture_output=True,
        )
    if inventory(worktree, upstream_sha, "HEAD") != patches:
        raise RuntimeError("Rebase changed the downstream feature inventory")
    state.update(
        status="ready",
        candidate_sha=git(worktree, "rev-parse", "HEAD"),
        release_tag=f"v{version}{suffix}",
    )
    save(state_path, state)
    return state


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream-tag", required=True)
    parser.add_argument("--upstream-sha")
    parser.add_argument("--source", default="HEAD")
    parser.add_argument("--worktree", type=Path, required=True)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--cpus", type=int, default=6)
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--candidate-branch")
    parser.add_argument("--candidate-sha")
    args = parser.parse_args()
    if not 1 <= args.cpus <= 6:
        parser.error("CPU count must be between one and six")
    os.sched_setaffinity(0, sorted(os.sched_getaffinity(0))[: args.cpus])
    if args.candidate_branch:
        if not args.candidate_sha:
            parser.error("Recovery requires --candidate-sha")
        result = recover(
            Path.cwd(),
            args.worktree,
            args.state,
            args.upstream_tag,
            args.source,
            args.candidate_branch,
            args.candidate_sha,
            expected_upstream=args.upstream_sha,
        )
    elif args.resume:
        state = json.loads(args.state.read_text())
        if args.upstream_tag != state["upstream_tag"] or (
            args.upstream_sha is not None and args.upstream_sha != state["upstream_sha"]
        ):
            raise RuntimeError("Resume state belongs to a different upstream target")
        worktree = Path(state["worktree"])
        if git(worktree, "status", "--porcelain"):
            raise RuntimeError(
                "Commit recovery changes and complete the rebase before resuming"
            )
        remote = git(worktree, "ls-remote", "origin", "refs/heads/main").split()
        if not remote or remote[0] != state["expected_remote_main"]:
            raise RuntimeError("Remote main advanced; start a fresh candidate")
        if inventory(worktree, state["upstream_sha"], "HEAD") != state["patches"]:
            raise RuntimeError("Restore the complete feature inventory before resuming")
        result = finish(worktree, worktree, args.state.resolve(), state)
    else:
        result = prepare(
            Path.cwd(),
            args.worktree,
            args.state,
            args.upstream_tag,
            source=args.source,
            expected_upstream=args.upstream_sha,
        )
    print(
        json.dumps(
            {
                key: result[key]
                for key in ("status", "candidate_branch", "candidate_sha")
            }
        )
    )
    if result["status"] != "ready":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
