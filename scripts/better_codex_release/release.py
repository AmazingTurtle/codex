#!/usr/bin/env python3
"""Record release validation and publish an exact, reviewed downstream patch stack."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import shlex

ROOT = Path(__file__).resolve().parents[2]
os.environ["CODEX_REPO_ROOT"] = str(ROOT)
sys.path.insert(0, str(ROOT / "scripts"))
from codex_package.version import read_workspace_version
from codex_package.version import read_product_version

GATES = {"targeted", "full-tests", "lint", "format", "packaging", "build", "smoke"}


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    with Path(path).open("rb") as stream:
        checksum = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(chunk)
        return checksum.hexdigest()


def save(path, state):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, indent=2) + "\n")
    temporary.replace(path)


def remote_refs(remote, *patterns):
    return dict(
        line.split()[::-1] for line in git("ls-remote", remote, *patterns).splitlines()
    )


def resolve_tag(tag):
    require(
        re.fullmatch(r"rust-v\d+\.\d+\.\d+", tag),
        "Select an explicit stable upstream Rust tag",
    )
    return git("rev-parse", f"refs/tags/{tag}^{{commit}}")


def stack(base):
    commits = git("rev-list", "--reverse", f"{base}..HEAD").splitlines()
    require(commits, "The downstream stack is empty")
    previous, identifiers = base, []
    for commit in commits:
        parents = git("show", "-s", "--format=%P", commit).split()
        require(
            parents == [previous],
            "Candidate must be a linear stack directly on the selected tag",
        )
        trailers = git(
            "show",
            "-s",
            "--format=%(trailers:key=Better-Codex-Patch,valueonly)",
            commit,
        ).splitlines()
        require(
            len(trailers) == 1 and trailers[0],
            f"Missing unique patch identifier: {commit}",
        )
        require(
            trailers[0] not in identifiers, f"Duplicate patch identifier: {trailers[0]}"
        )
        identifiers.append(trailers[0])
        require(
            not git("show", "-s", "--format=%s", commit).startswith(
                ("fixup!", "squash!")
            ),
            "Autosquash before validation",
        )
        previous = commit
    return identifiers


def clean_tree():
    require(
        not git("status", "--porcelain"),
        "Commit changes and remove unrelated generated files first",
    )
    return git("rev-parse", "HEAD^{tree}")


def next_release_tag(version, revision=None):
    prefix = f"v{version}-better-codex"
    existing = set(remote_refs("origin", f"refs/tags/{prefix}*").keys()) | {
        f"refs/tags/{tag}" for tag in git("tag", "--list", f"{prefix}*").splitlines()
    }
    revisions = []
    for ref in existing:
        match = re.fullmatch(rf"refs/tags/{re.escape(prefix)}(?:\.(\d+))?", ref)
        if match:
            revisions.append(int(match.group(1) or "0"))
    if revision is not None:
        require(revision >= 0, "Release revision must be nonnegative")
        require(
            revision > max(revisions, default=-1),
            "Release revision must exceed all existing downstream tags",
        )
        return f"{prefix}.{revision}" if revision else prefix
    return f"{prefix}.{max(revisions) + 1}" if revisions else prefix


def check_version(state):
    version = read_workspace_version(ROOT)
    product_version = read_product_version(ROOT)
    require(
        state["upstream_tag"] == f"rust-v{version}",
        "Workspace version differs from target tag",
    )
    require(
        state["release_tag"] == f"v{product_version}",
        "Set the product-info VERSION suffix to match the prepared release tag before validation",
    )
    return product_version


def prepare(path, previous_tag, target_tag, revision=None):
    require(not path.exists(), "State already exists; resume it or choose a new path")
    clean_tree()
    previous, target = resolve_tag(previous_tag), resolve_tag(target_tag)
    identifiers = stack(previous)
    refs = remote_refs(
        "upstream", f"refs/tags/{target_tag}", f"refs/tags/{target_tag}^{{}}"
    )
    require(
        refs.get(f"refs/tags/{target_tag}^{{}}", refs.get(f"refs/tags/{target_tag}"))
        == target,
        "Local target tag differs from upstream",
    )
    branch = remote_refs("origin", "refs/heads/main").get("refs/heads/main")
    require(branch, "origin/main must exist")
    path.parent.mkdir(parents=True, exist_ok=True)
    save(
        path,
        {
            "repository": str(ROOT),
            "release_tag": next_release_tag(
                target_tag.removeprefix("rust-v"), revision
            ),
            "upstream_tag": target_tag,
            "upstream_sha": target,
            "previous_tag": previous_tag,
            "previous_sha": previous,
            "old_head": git("rev-parse", "HEAD"),
            "expected_remote_main": branch,
            "patches": identifiers,
            "checks": {},
        },
    )


def check(path, state, name, command, cpus):
    before = clean_tree()
    version = check_version(state)
    require(1 <= cpus <= 8, "CPU count must be between one and eight")
    require(
        hasattr(os, "sched_getaffinity"),
        "This validation helper currently requires Linux",
    )
    if name == "full-tests":
        require(not command, "Full-suite command is fixed; filters are not accepted")
    # Invalidate an older success before a retry, including an interrupted retry.
    state["checks"].pop(name, None)
    state.pop("sealed", None)
    save(path, state)
    allowed = sorted(os.sched_getaffinity(0))[:cpus]
    os.sched_setaffinity(0, allowed)
    env = dict(
        os.environ,
        CARGO_BUILD_JOBS=str(len(allowed)),
        RUST_TEST_THREADS=str(len(allowed)),
        CODEX_REPO_ROOT=str(ROOT),
    )
    cwd = ROOT
    if name == "full-tests":
        require(not command, "Full-suite command is fixed; filters are not accepted")
        os.environ["CODEX_REPO_ROOT"] = str(ROOT)
        sys.path.insert(0, str(ROOT / "scripts"))
        from codex_package.targets import TARGET_SPECS
        from codex_package.v8 import resolve_codex_v8_cargo_env

        env.update(resolve_codex_v8_cargo_env(TARGET_SPECS["x86_64-unknown-linux-gnu"]))
        env["CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER"] = shlex.join(
            [sys.executable, str(Path(__file__).with_name("test_runner.py"))]
        )
        command = ["just", "test", "--workspace", "--test-threads", str(len(allowed))]
        if "codex-v8-poc" in (ROOT / "codex-rs/Cargo.toml").read_text():
            command += ["--features", "codex-v8-poc/sandbox"]
        cwd = ROOT / "codex-rs"
    require(command, "Supply a validation command after --")
    package = None
    if name == "build":
        require(
            command[:2] == ["just", "assemble-codex-package"],
            "Use the canonical package builder",
        )
        require(
            command[2:6]
            == ["--target", "x86_64-unknown-linux-gnu", "--cargo-profile", "release"]
            and len(command) == 8
            and command[6] == "--package-dir",
            "Select the GNU Linux release profile and a staging package directory",
        )
        package = (ROOT / command[7]).resolve()
    elif name == "smoke":
        require(
            len(command) == 3
            and Path(command[1]).resolve()
            == Path(__file__).with_name("smoke.py").resolve(),
            "Use the package smoke script with one package directory",
        )
        package = (ROOT / command[2]).resolve()
        require(
            package_files(package, version)
            == state["checks"].get("build", {}).get("files"),
            "Smoke checks must use the recorded build artifacts",
        )
    log = path.with_name(f"{path.stem}-{name}.log")
    with log.open("w") as output:
        result = subprocess.run(
            command, cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT
        )
    state["checks"][name] = {
        "tree": before,
        "command": command,
        "exit_code": result.returncode,
        "log": str(log),
        "sha256": digest(log),
        "cpus": allowed,
        "summary": [
            line.strip()
            for line in log.read_text(errors="replace").splitlines()
            if "Summary" in line
        ][-1:],
    }
    if package is not None and result.returncode == 0:
        files = package_files(package, version)
        if name == "smoke":
            require(
                files == state["checks"]["build"]["files"],
                "Package changed during smoke checks",
            )
        state["checks"][name]["files"] = files
    save(path, state)
    require(result.returncode == 0, f"{name} failed; inspect {log}")
    require(
        clean_tree() == before,
        f"{name} changed the tree; commit changes and repeat affected validation",
    )
    print(f"{name}: passed; {log}")


def package_files(package, version):
    metadata = json.loads((package / "codex-package.json").read_text())
    require(
        metadata["version"] == version
        and metadata["target"] == "x86_64-unknown-linux-gnu"
        and metadata["variant"] == "codex",
        "Package identity does not match the release",
    )
    for name in [
        "bin/codex",
        "bin/codex-code-mode-host",
        "codex-resources/bwrap",
        "codex-resources/zsh/bin/zsh",
        "codex-path/rg",
    ]:
        require(
            os.access(package / name, os.X_OK), f"Missing package executable: {name}"
        )
    return {
        str(p.relative_to(package)): digest(p)
        for p in sorted(package.rglob("*"))
        if p.is_file()
    }


def seal(path, state, package, review):
    tree = clean_tree()
    identifiers = stack(state["upstream_sha"])
    review_text = review.read_text()
    for identifier in set(state["patches"]) - set(identifiers):
        require(
            f"Dropped-Patch: {identifier}" in review_text.splitlines(),
            f"Document why patch was dropped: {identifier}",
        )
    for identifier in set(identifiers) - set(state["patches"]):
        require(
            f"Added-Patch: {identifier}" in review_text.splitlines(),
            f"Document the added patch: {identifier}",
        )
    for name in GATES:
        receipt = state["checks"].get(name, {})
        require(
            receipt.get("exit_code") == 0 and receipt.get("tree") == tree,
            f"Missing current successful check: {name}",
        )
        require(
            digest(receipt["log"]) == receipt["sha256"],
            f"Validation log changed: {name}",
        )
    require(
        review.is_file() and review.read_text().strip(),
        "A written final review is required",
    )
    version = check_version(state)
    files = package_files(package, version)
    require(
        all(state["checks"][name].get("files") == files for name in ("build", "smoke")),
        "Package differs from validated build/smoke artifacts",
    )
    tag = state["release_tag"]
    require(
        not remote_refs("origin", f"refs/tags/{tag}") and not git("tag", "--list", tag),
        "Prepared release tag was taken; start a new release attempt",
    )
    state["sealed"] = {
        "head": git("rev-parse", "HEAD"),
        "tree": tree,
        "tag": tag,
        "review": str(review),
        "review_sha256": digest(review),
        "package": str(package),
        "files": files,
        "version": version,
    }
    save(path, state)
    print(
        f"Ready: {tag} at {state['sealed']['head']}; publication is a separate command"
    )


def publish(state):
    sealed = state["sealed"]
    require(
        clean_tree() == sealed["tree"] and git("rev-parse", "HEAD") == sealed["head"],
        "Candidate changed after sealing",
    )
    require(
        digest(sealed["review"]) == sealed["review_sha256"],
        "Review changed after sealing",
    )
    require(
        package_files(Path(sealed["package"]), sealed["version"]) == sealed["files"],
        "Package changed after sealing",
    )
    for receipt in state["checks"].values():
        require(
            digest(receipt["log"]) == receipt["sha256"],
            "Validation log changed after sealing",
        )
    require(
        remote_refs("origin", "refs/heads/main").get("refs/heads/main")
        == state["expected_remote_main"],
        "Remote main advanced; stop and reconcile",
    )
    tag = sealed["tag"]
    require(
        not remote_refs("origin", f"refs/tags/{tag}"),
        "Release tag already exists remotely",
    )
    require(
        not git("tag", "--list", tag),
        "Release tag already exists locally; inspect the previous attempt",
    )
    message = json.dumps(
        {
            "upstream_tag": state["upstream_tag"],
            "upstream_sha": state["upstream_sha"],
            "downstream_sha": sealed["head"],
            "review_sha256": sealed["review_sha256"],
            "validation": {
                name: {
                    key: value
                    for key, value in receipt.items()
                    if key in {"exit_code", "sha256", "cpus", "summary"}
                }
                for name, receipt in state["checks"].items()
            },
            "artifacts": sealed["files"],
        },
        indent=2,
    )
    subprocess.run(
        ["git", "tag", "-a", tag, sealed["head"], "-F", "-"],
        cwd=ROOT,
        input=message,
        text=True,
        check=True,
    )
    subprocess.run(
        [
            "git",
            "push",
            "--atomic",
            f"--force-with-lease=refs/heads/main:{state['expected_remote_main']}",
            f"--force-with-lease=refs/tags/{tag}:",
            "origin",
            f"{sealed['head']}:refs/heads/main",
            f"refs/tags/{tag}:refs/tags/{tag}",
        ],
        cwd=ROOT,
        check=True,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, required=True)
    commands = parser.add_subparsers(dest="action", required=True)
    prep = commands.add_parser("prepare")
    prep.add_argument("--previous-tag", required=True)
    prep.add_argument("--upstream-tag", required=True)
    prep.add_argument("--revision", type=int)
    run = commands.add_parser("check")
    run.add_argument("name", choices=sorted(GATES))
    run.add_argument("--cpus", type=int, default=8)
    finish = commands.add_parser("seal")
    finish.add_argument("--package", type=Path, required=True)
    finish.add_argument("--review", type=Path, required=True)
    commands.add_parser("publish")
    arguments = sys.argv[1:]
    separator = arguments.index("--") if "--" in arguments else len(arguments)
    command = arguments[separator + 1 :]
    args = parser.parse_args(arguments[:separator])
    require(
        args.action == "check" or not command, "Commands after -- apply only to check"
    )
    path = args.state.resolve()
    if args.action == "prepare":
        prepare(path, args.previous_tag, args.upstream_tag, args.revision)
        return
    state = json.loads(path.read_text())
    require(state["repository"] == str(ROOT), "State belongs to another checkout")
    if args.action == "check":
        check(path, state, args.name, command, args.cpus)
    elif args.action == "seal":
        seal(path, state, args.package.resolve(), args.review.resolve())
    else:
        publish(state)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
