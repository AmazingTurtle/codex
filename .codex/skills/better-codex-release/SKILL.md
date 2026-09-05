---
name: better-codex-release
description: Rebase, validate, build, and optionally publish Better Codex on an explicit upstream Rust release while maintaining one commit per downstream feature. Use for Better Codex release maintenance and patch-stack updates.
---

# Better Codex release maintenance

Maintain fork `main` as an exact `openai/codex` stable `rust-vX.Y.Z` tag plus the curated downstream patches. Unreleased development belongs on topic branches. Never use upstream `main` as a release base.

Read [the runbook](references/runbook.md) for commands and [patch ownership](references/patches.md) before rewriting the stack. The repository scripts record validation and enforce publication checks; this skill handles conflict resolution, behavior review, and patch ownership.

## Inputs and defaults

Require an explicit upstream Rust tag. Default to GNU Linux, release profile, package directory `dist/codex-package-x86_64-unknown-linux-gnu`, and at most eight CPU cores total. Use existing repository commands and package tooling. Do not introduce a second build system.

A request to rebase/build/commit does not authorize pushing. When the request includes publication, complete the validation and publish without asking again. Publication updates fork main and creates the downstream tag; the existing workflow creates a source-only GitHub release. Binary uploads are separate work.

## Invariants

- Preserve the old HEAD in a backup ref. Keep existing release tags immutable.
- Work in a clean isolated worktree. Inspect dirty user work and leave it untouched.
- Each downstream commit has one unique `Better-Codex-Patch: <id>` trailer. Keep feature IDs stable across rebases; never identify patch ownership solely by a SHA.
- Split mixed fixes into their owning feature. Use temporary `git commit --fixup=<owner>` commits, then autosquash before final validation. There must be no version-integration, `fixup!`, or `squash!` commits in the published stack.
- Keep independent upstream bug fixes separate. Drop them only after verifying equivalent upstream behavior and relevant coverage. Explain inventory changes in the final review with `Dropped-Patch: <id>` or `Added-Patch: <id>` lines.
- Enable local `rerere.enabled=true` and `rerere.autoupdate=false`. Review reused resolutions before staging. Compare old/new patches using `git range-diff` and inspect the final diff against the selected upstream tag.
- Product identity lives in `codex-product-info`. Preserve intentional upstream compatibility names. Keep snapshots branded; stabilize the version before layout, not by replacing rendered text indiscriminately.
- Resolve substantive conflicts manually. Rebuild generated lockfiles/schemas through repository commands; do not resolve them by blindly choosing one side.
- Review code at least once before creating final feature commits. Apply the repository code-review skill and address every finding.
- Require targeted tests and one successful complete local Linux workspace run before publication. Report skipped and flaky tests; a collection of passing reruns does not replace that run. Do not weaken assertions or add skip guards to pass the gate.
- Respect sandbox execution restrictions. The Linux runner isolates known host CA/skill inputs but does not disable sandbox guards. When sandbox restrictions prevent meaningful validation, report the limitation and obtain the necessary execution permission through the normal tool mechanism.
- Apply CPU affinity to the whole process tree, plus Cargo jobs and test threads. The helper selects up to eight CPUs from the allowed set.
- Never retry a rejected lease with a new expected SHA automatically. Stop, inspect the concurrent change, reconcile it, and repeat affected review/validation.

## Completion

Report upstream tag/SHA, downstream commit/tag, patch inventory changes, full-suite summary including skips/retries, lint/format/package checks, installed binary path and version, backup ref, and publication status. Keep detailed logs outside tracked source; the annotated tag records validation and artifact digests without machine-local paths.
