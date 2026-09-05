# Release runbook

Commands below run from the candidate worktree root. Replace tag/path examples with the explicitly requested release. Run network/cache-dependent operations with the execution permissions they actually require.

Use Python 3.11 or newer for the complete release workflow: the existing V8 resolver requires `tomllib`. The guard itself remains compatible with the scripts project's Python 3.10 minimum.

## Prepare and replay

1. Inspect `git status`, remotes, current HEAD and upstream tags. Fetch origin main and the exact requested upstream tag. Do not overwrite existing tags. Determine the previous base from the maintained stack; `git describe --tags --match 'rust-v*' --abbrev=0` is a starting point, not proof.
2. Record the current HEAD under `backup/pre-<target>-<timestamp>`. Create a clean worktree and candidate branch from it. Enable repository-local rerere with automatic staging disabled.
3. Prepare a new state file outside the checkout:

```sh
python3 scripts/better_codex_release/release.py --state /tmp/better-codex-release/state.json prepare --previous-tag rust-v0.153.4 --upstream-tag rust-v0.153.5
```

Pass `--revision N` to `prepare` when the requested release uses an explicit
`.N` suffix even though no earlier downstream tag exists for that upstream
version. The revision must exceed any existing downstream revision.

Preparation validates the old stack and remote target tag, selects a downstream release tag, and captures the remote main SHA for publication. The state belongs to this worktree; resume here. Do not overwrite or hand-edit validation receipts.

4. Rebase using `git rebase --onto <target-tag> <previous-tag>`. Resolve each conflict according to patch ownership. Preserve the unique `Better-Codex-Patch` trailer when amending commits.
5. Read `release_tag` in the state file. Set the literal suffix in `codex-rs/product-info/src/lib.rs`'s `VERSION` to match it: `-better-codex` for a new upstream release or `-better-codex.N` for a downstream revision. Fold this edit into the distribution patch. Selecting the tag before validation ensures source installs and built binaries report the same version as the release; do not use a build-only override.
6. Fix compile/test issues in their owning patch using fixup commits. Autosquash with `git rebase -i --autosquash <target-tag>`. Inspect `git range-diff <old-base>..<backup> <target-tag>..HEAD` and the final upstream diff. Independent fixes already implemented upstream may be dropped after verification.
7. Refresh affected schemas and Cargo/Bazel lockfiles using AGENTS.md commands. Use `just fix -p <affected-crates>` and `just fmt` to finish edits before final check receipts. Review snapshots individually; accept only intended changes. Run the repository code-review skill before finalizing commits.

## Validate the final committed tree

The helper applies Linux CPU affinity, Cargo jobs, and Rust test threads to each command. Default is eight CPUs; `check --cpus 4 <gate>` selects fewer. CPU-intensive commands outside the helper must use equivalent limits. Avoid overlapping Rust commands competing for the same target directory.

Use one new state file per release attempt; failed retries invalidate previous receipts. Commands other than `full-tests` run at the repository root, where `just` supplies its normal working directory.

```sh
python3 scripts/better_codex_release/release.py --state /tmp/better-codex-release/state.json check targeted -- just test -p codex-cli -p codex-tui -p codex-cloud-tasks -p codex-utils-home-dir --test-threads 8
python3 scripts/better_codex_release/release.py --state /tmp/better-codex-release/state.json check lint -- just clippy -p codex-cli -p codex-tui -p codex-cloud-tasks -p codex-utils-home-dir
python3 scripts/better_codex_release/release.py --state /tmp/better-codex-release/state.json check format -- just fmt-check
python3 scripts/better_codex_release/release.py --state /tmp/better-codex-release/state.json check packaging -- python3 -m unittest discover -s scripts/codex_package -p 'test_*.py'
python3 scripts/better_codex_release/release.py --state /tmp/better-codex-release/state.json check full-tests
```

Adapt targeted crates and lint scope to the changed features. Also run `python3 -m unittest discover -s scripts/better_codex_release -p 'test_release.py'` when changing maintenance scripts.

`full-tests` is fixed to `just test --workspace`, with no filters. It resolves the repository's verified V8 artifact pair and uses the Linux test runner. The current workspace needs `codex-v8-poc/sandbox` for its V8 probe; this is targeted feature unification, not `--all-features`. Reassess this when upstream changes V8 layout. Other V8-dependent commands can use the same repository resolver or an already verified environment pair, as documented by the package builder.

The runner removes Cargo-injected CA overrides for native-trust tests and isolates skills tests from personal home contents and shared temporary Git markers. It removes its own Cargo-runner variable before tests resolve child binaries. It does not modify sandbox markers or product code. Record actual suite counts, skips, retries, and environment limitations in the final review. A failed full run must eventually be replaced by a successful full run, not just selected reruns.

## Build, review, and seal

```sh
python3 scripts/better_codex_release/release.py --state /tmp/better-codex-release/state.json check build -- just assemble-codex-package --target x86_64-unknown-linux-gnu --cargo-profile release --package-dir dist/better-codex-release-staging
python3 scripts/better_codex_release/release.py --state /tmp/better-codex-release/state.json check smoke -- python3 scripts/better_codex_release/smoke.py dist/better-codex-release-staging
```

Review the final stack, validation logs, and package. Write a final review outside tracked source, including explanations and exact `Dropped-Patch: <id>` / `Added-Patch: <id>` lines for inventory changes. Preserve the previous installed package in a unique sibling backup, and move the validated staging directory to `dist/codex-package-x86_64-unknown-linux-gnu`.

```sh
python3 scripts/better_codex_release/release.py --state /tmp/better-codex-release/state.json seal --package dist/codex-package-x86_64-unknown-linux-gnu --review /tmp/better-codex-release/review.md
```

Sealing requires a clean committed tree and successful receipts for that exact tree. It verifies artifact identity, records SHA-256 digests, and verifies the previously selected tag is still unused. A history-only autosquash can retain receipts if the tree is identical; any changed tree invalidates them. Keep generated logs and state outside tracked source. Preserve them when moving/removing a worktree.

## Publish only when requested

Check that local main still matches the backed-up starting HEAD and its worktree is clean before moving it to the validated candidate. If another commit appeared, stop and reconcile it rather than discarding it. From the original clean main worktree, `git reset --keep <candidate>` updates the branch while refusing conflicting local edits. Keep the candidate worktree for the publication command because its state is bound there.

```sh
python3 scripts/better_codex_release/release.py --state /tmp/better-codex-release/state.json publish
```

Publication rechecks the sealed candidate, review, logs, package, and expected remote main. It creates an annotated downstream tag with upstream/downstream SHAs and validation/artifact digests, then atomically pushes main and that tag with explicit leases. It never moves upstream tags or overwrites an existing release tag. A rejected push leaves the local annotated tag for inspection; do not delete it and retry blindly.

Verify origin main and the remote peeled tag resolve to the tested commit. The tag triggers the existing source-only release workflow. Report the workflow status without claiming it succeeded before it completes. Keep release tags and the backup ref; do not force-update historical releases.
