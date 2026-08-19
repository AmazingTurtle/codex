//! Downstream product identity for the Better Codex distribution.

pub const DISPLAY_NAME: &str = "Better Codex";
pub const CLI_NAME: &str = "better-codex";
pub const HOME_ENV: &str = "BETTER_CODEX_HOME";
pub const HOME_DIR_NAME: &str = ".better-codex";
pub const REPOSITORY: &str = "https://github.com/AmazingTurtle/codex";
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-better-codex");
