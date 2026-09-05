# Patch ownership

| ID | Responsibility |
| --- | --- |
| distribution | Product identity, home isolation/import, updater, packaging, branding tests and stable snapshot versions |
| multi-account | Account storage, selection/rotation, account APIs, account UI and status-all behavior |
| activity-display | Individual tool details inside grouped commands/searches/images and optional summaries |
| opaque-request-retry | Retry opaque bad-request responses |
| resume-identifiers | Use stable thread identifiers in resume hints |
| test-async-hook | Re-arm the gated async-hook fixture between turns |
| test-file-mcp-import | Remove the unused file-MCP fixture matcher import |
| test-terminal-capture | Make terminal capture independent of host color behavior |
| test-guardian-analytics | Await queued analytics before shutting down the guardian test server |
| bash-snapshot | Suppress Bash startup files during snapshot restoration |
| release-maintenance | This skill, release validation/publication helpers, and their tests |

Keep schemas, fixtures, and regression tests in the commit that owns their behavior. A file can contain several features: split by hunks rather than moving the entire file into a convenient commit. Maintenance scripts do not own unrelated runtime fixes.

The initial history-only migration includes `b43b99277f`. Its curated tree must equal that baseline before adding branding/test and maintenance improvements. Later releases compare patch ranges instead of demanding equal trees across different upstream versions.
