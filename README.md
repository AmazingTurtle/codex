# Better Codex

Better Codex is an unofficial derivative of OpenAI's Codex
CLI. It adds multi-account ChatGPT subscription rotation and keeps its command,
configuration, credentials, sessions, and caches isolated from upstream Codex.

Better Codex is maintained independently by AmazingTurtle. It is not endorsed
by or affiliated with OpenAI. “Codex”, “OpenAI”, and “ChatGPT” remain the
property of their respective owners.

---

## Quickstart

### Installing and running Better Codex

On GNU Linux x86_64, install the pinned release with Python 3.12 or newer:

```shell
curl -fsSL https://raw.githubusercontent.com/AmazingTurtle/codex/v0.159.0-better-codex.2/scripts/better_codex_release/install.py \
  | python3 - --version v0.159.0-better-codex.2
```

Then run:

```shell
better-codex
```

The installer verifies checksums, checks the package in an isolated home, and
atomically switches `~/.local/share/better-codex/current`. Earlier versions
remain available for rollback. It creates `~/.local/bin/better-codex`; add that
directory to your `PATH`. The executable inside the package remains `codex`
so internal upstream paths and helper discovery need fewer downstream changes.
An existing unmanaged `better-codex` command is preserved; choose a different
`--bin-dir` or move the old command before installing.

For other platforms, build the canonical package from the pinned source tag:

```shell
git clone --depth 1 --branch v0.159.0-better-codex.2 https://github.com/AmazingTurtle/codex better-codex-source
cd better-codex-source
just assemble-codex-package
```

Use a `better-codex` launcher pointing to the assembled package's `bin/codex`.
Keep the package directory together: it includes the code-mode host and sandbox,
shell, and search resources. Installing only the Cargo binary omits those resources.

Better Codex uses `~/.better-codex` by default. Override it only with
`BETTER_CODEX_HOME`; upstream `CODEX_HOME` and `CODEX_SQLITE_HOME` are not used.

To preview or perform a one-time, non-destructive import of your upstream state:

```shell
better-codex import-codex-state --dry-run
better-codex import-codex-state
```

The import leaves `~/.codex` untouched. Use `--from DIR` for a non-default
upstream home. The destination must be absent or empty.

### Release and rebase policy

Downstream releases track stable upstream tags. For example, upstream
`rust-v0.153.1` maps to `v0.153.1-better-codex`; downstream-only hotfixes use
`v0.153.1-better-codex.1`, `.2`, and so on. Rebases are best effort and may lag
an upstream release while conflicts and regressions are resolved.

GitHub releases include a GNU Linux runtime, a separate matching debug-symbol
archive, `release.json`, and `SHA256SUMS`. Only staged package copies are stripped;
Cargo outputs keep their symbols. Run `better-codex update` to check the latest
release and print its pinned installation command.

Hourly CI prepares a rebase in an isolated worktree. Clean candidates run focused
regressions, formatting, lint, package checks, and smoke tests before main and an
immutable release tag are pushed atomically with leases. Conflicts or failed gates
leave a draft recovery PR and diagnostics. A weekly job runs the complete Linux
workspace. See [maintenance instructions](scripts/better_codex_release/README.md).

### Profile-backed workers

Register a trusted existing profile explicitly in `~/.better-codex/config.toml`:

```toml
[agents.spark]
description = "Local coding worker"
profile = "spark"
```

This loads `spark.config.toml` from the same home when an agent uses
`agent_type = "spark"`. The worker uses the profile's provider, authentication,
model, catalog, instructions, and context limits, while keeping the parent's
permissions, approval policy, tools, and filesystem boundaries. `profile` and
`config_file` are mutually exclusive. Set `[features.multi_agent_v2] enabled = false`
in the profile to make it a leaf worker. Provider switches require a fresh
conversation (`fork_turns = "none"`); profiles are never exposed as roles automatically.

### Using Better Codex with your ChatGPT plan

Run `better-codex` and select **Sign in with ChatGPT**. Better Codex uses the
same OpenAI service and model APIs as upstream Codex; eligibility and usage are
still governed by your ChatGPT plan and OpenAI's terms.

You can also use Better Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Installing & maintaining Better Codex**](scripts/better_codex_release/README.md)
- [**Upstream Codex documentation**](https://developers.openai.com/codex)

This derivative remains licensed under the [Apache-2.0 License](LICENSE). The
upstream license and notices are preserved.
