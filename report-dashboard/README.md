# Report dashboard development

This package builds the offline React/ECharts dashboard embedded by `codex-report`.
Rust owns the versioned report contract; `src/data/report-data.ts` validates the
embedded JSON once before rendering. No prompts, tools, credentials, or network
requests belong in the dashboard.

From the repository root:

```sh
pnpm install --frozen-lockfile
pnpm --filter @better-codex/report-dashboard build
pnpm --filter @better-codex/report-dashboard test
pnpm --filter @better-codex/report-dashboard test:browser
pnpm --filter @better-codex/report-dashboard check:assets
pnpm --filter @better-codex/report-dashboard write:notices
```

Browser tests open generated HTML through `file://` with networking disabled.
Install Chromium with `pnpm exec playwright install chromium` if unavailable;
`REPORT_TEST_BROWSER` can point to an existing Chromium executable. Screenshots
for 390, 768, and 1440 px in both themes are under `test-results/` after testing.

Commit generated `codex-rs/report/assets/dashboard.js`, `dashboard.css`, and
`THIRD_PARTY_NOTICES.md` with source changes. `check:assets` rebuilds in a temporary
directory and rejects drift. The output is an IIFE with inlined dependencies and
production React; report generation requires no JavaScript toolchain.
