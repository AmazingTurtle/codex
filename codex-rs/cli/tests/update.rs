use anyhow::Result;
use std::path::Path;
use tempfile::TempDir;

fn better_codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("better-codex")?);
    cmd.env("BETTER_CODEX_HOME", codex_home);
    Ok(cmd)
}

#[tokio::test]
async fn update_help_does_not_start_a_network_request() -> Result<()> {
    let codex_home = TempDir::new()?;

    better_codex_command(codex_home.path())?
        .args(["update", "--help"])
        .assert()
        .success();

    Ok(())
}
