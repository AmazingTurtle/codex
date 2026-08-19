/// The current Better Codex distribution version.
#[cfg(not(test))]
pub const CODEX_CLI_VERSION: &str = codex_product_info::VERSION;

// Keep rendered snapshots stable across releases, including truncated headers.
// Production binaries still expose the real workspace version.
#[cfg(test)]
pub const CODEX_CLI_VERSION: &str = "0.999.0-better-codex";
