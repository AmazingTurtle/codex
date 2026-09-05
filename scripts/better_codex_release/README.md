# Better Codex maintenance

Maintain one linear commit per retained feature, with a unique
`Better-Codex-Patch: <owner>` trailer. Fold fixes into their owner before running
release gates. Keep internal upstream executable names; brand the public launcher,
home, credential namespace, version, and update command.

## Automatic updates

`better-codex-upstream.yml` checks stable `rust-vX.Y.Z` tags hourly. The driver
verifies both the upstream tag SHA and the maintained source SHA, creates a backup
ref and isolated worktree, rebases the curated stack, and folds the deterministic
version change into the distribution owner. It never silently skips a patch.

The pipeline runs formatting, scoped Clippy, Python maintenance tests, and retained
feature regressions. A recovery also runs full tests for affected Rust crates.
Unknown patch owners fail until `validation.py` defines their coverage. Successful
candidates build one GNU Linux package, archive matching ELF debug symbols, strip
only staging copies, and smoke-test the complete package with an isolated home.
Receipts bind the candidate tree, logs, runtime, symbols, and final review by hash.
Publication atomically pushes main and the immutable tag with explicit leases,
then attaches assets to a draft GitHub release and publishes it.

`better-codex-checks.yml` runs focused gates on an exact PR head, without publishing.
`better-codex-full-tests.yml` runs the unfiltered workspace weekly and on demand.
All Rust work is restricted to at most six available CPUs. Bootstrap release states
require a complete workspace baseline in addition to the focused gates.

## Conflict recovery

A failed attempt records the source SHA, target tag/SHA, failed patch owner,
conflicted paths, rebase log, and `RECOVERY.md`. The draft PR contains a partial
candidate when replay stopped at a conflict; it must not be merged directly.

Recreate the attempt from the recorded source in a fresh worktree:

```shell
python3 scripts/better_codex_release/rebase.py \
  --source <source-sha> --upstream-tag rust-vX.Y.Z --upstream-sha <target-sha> \
  --worktree /tmp/better-codex-recovery --state /tmp/better-codex-state/state.json
```

Resolve the reported conflicts, continue the rebase, and fold fixes into their
owning commits. Repeat until the entire inventory is present. Use `--resume` with
the same state and worktree to finish local preparation. Stamp the selected product
version in the distribution owner, push the complete stack to the recovery branch,
then dispatch the upstream workflow with all four inputs: `upstream_tag`,
`candidate_branch`, exact `candidate_sha`, and the recorded `source_sha`.
Moved source/branch/tag leases and incomplete inventories are rejected.

If publication of Git refs succeeds but GitHub asset publication fails, retain the
workflow artifacts. The source/tag have already been promoted; upload the sealed
assets to that exact release and publish its draft. Do not recreate the tag or run
a fresh rebase for an asset upload failure.

## Local bootstrap and package installation

Commit the selected product version, prepare a bootstrap state using
`release.py prepare`, and run each required gate with `release.py check`. The `full-tests` gate
fixes its own command and does not accept filters. Use `pipeline.py` to execute the
same gates, package builder, smoke tests, and final seal. Publication is opt-in via
`--publish`. Keep the state and validation logs outside the tracked tree.

`package.py` emits `better-codex-x86_64-unknown-linux-gnu.tar.gz`, a matching
`better-codex-symbols-x86_64-unknown-linux-gnu.tar.gz`, `release.json`, and
`SHA256SUMS`. The runtime remains a complete canonical Codex package.

The standalone installer requires Python 3.12+. It checks the pinned archive hash,
target, variant, and version, safely extracts the package, then atomically switches
the managed `current` link. An unsuccessful smoke check restores the prior link.
Unmanaged commands are preserved. Older version directories remain available;
rollback means atomically pointing `current` at a previously verified directory.

The former source-only tag workflow is retired. Configure fork Actions to enable
the three Better Codex workflows; upstream signing, publishing, provisioned-host,
and infrastructure workflows are not needed for this distribution.
