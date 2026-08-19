# Better Codex

Better Codex is an unofficial, source-distributed derivative of OpenAI's Codex
CLI. It adds multi-account ChatGPT subscription rotation and keeps its command,
configuration, credentials, sessions, and caches isolated from upstream Codex.

Better Codex is maintained independently by AmazingTurtle. It is not endorsed
by or affiliated with OpenAI. “Codex”, “OpenAI”, and “ChatGPT” remain the
property of their respective owners.

---

## Quickstart

### Installing and running Better Codex

Install the tagged source release with Rust and Cargo:

```shell
cargo install --git https://github.com/AmazingTurtle/codex \
  --tag v0.148.0-better-codex \
  --locked --force --bin better-codex codex-cli
```

Then run:

```shell
better-codex
```

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
`rust-v0.148.0` maps to `v0.148.0-better-codex`; downstream-only hotfixes use
`v0.148.0-better-codex.1`, `.2`, and so on. Rebases are best effort and may lag
an upstream release while conflicts and regressions are resolved.

GitHub releases contain source metadata only. Better Codex does not publish or
reuse OpenAI's signed binaries, npm package, Homebrew cask, installers, or
release infrastructure. Run `better-codex update` to check the fork's latest
release and print its pinned Cargo install command.

### Using Better Codex with your ChatGPT plan

Run `better-codex` and select **Sign in with ChatGPT**. Better Codex uses the
same OpenAI service and model APIs as upstream Codex; eligibility and usage are
still governed by your ChatGPT plan and OpenAI's terms.

You can also use Better Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Installing & building**](./docs/install.md)
- [**Upstream Codex documentation**](https://developers.openai.com/codex)

This derivative remains licensed under the [Apache-2.0 License](LICENSE). The
upstream license and notices are preserved.
