use anyhow::Result;
use codex_login::AuthDotJson;
use codex_protocol::auth::AuthMode;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn command(destination: &Path) -> Result<assert_cmd::Command> {
    let mut command = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("better-codex")?);
    command.env("BETTER_CODEX_HOME", destination);
    Ok(command)
}

fn api_key_auth() -> AuthDotJson {
    AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("source-api-key".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        accounts: Vec::new(),
    }
}

#[tokio::test]
async fn dry_run_reports_plan_without_creating_destination() -> Result<()> {
    let root = TempDir::new()?;
    let source = root.path().join(".codex");
    let destination = root.path().join(".better-codex");
    fs::create_dir(&source)?;
    fs::write(source.join("history.jsonl"), "source-history")?;

    command(&destination)?
        .args(["import-codex-state", "--from"])
        .arg(&source)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "Would import 1 filesystem entries",
        ))
        .stdout(predicates::str::contains(
            "The upstream source would not be modified.",
        ));

    assert!(!destination.exists());
    assert_eq!(
        fs::read_to_string(source.join("history.jsonl"))?,
        "source-history"
    );
    Ok(())
}

#[tokio::test]
async fn imports_persistent_state_credentials_and_consistent_sqlite_snapshot() -> Result<()> {
    let root = TempDir::new()?;
    let source = root.path().join(".codex");
    let destination = root.path().join(".better-codex");
    fs::create_dir_all(source.join("sessions/2026"))?;
    fs::write(source.join("sessions/2026/rollout.jsonl"), "source-rollout")?;
    fs::write(source.join("daemon.lock"), "runtime")?;
    fs::write(
        source.join("auth.json"),
        serde_json::to_vec_pretty(&api_key_auth())?,
    )?;

    let database = source.join("state_5.sqlite");
    let sqlite =
        codex_state::SqliteConfig::from_sqlite_home(AbsolutePathBuf::from_absolute_path(&source)?);
    let connection = sqlite.open_read_write_pool(&database).await?;
    sqlx::query("CREATE TABLE records (value TEXT NOT NULL)")
        .execute(&connection)
        .await?;
    sqlx::query("INSERT INTO records (value) VALUES ('source')")
        .execute(&connection)
        .await?;
    connection.close().await;
    let source_database = fs::read(&database)?;

    command(&destination)?
        .args(["import-codex-state", "--from"])
        .arg(&source)
        .assert()
        .success()
        .stdout(predicates::str::contains("was left unchanged"));

    assert_eq!(
        fs::read_to_string(destination.join("sessions/2026/rollout.jsonl"))?,
        "source-rollout"
    );
    assert!(!destination.join("daemon.lock").exists());
    let imported_auth: AuthDotJson =
        serde_json::from_slice(&fs::read(destination.join("auth.json"))?)?;
    assert_eq!(imported_auth, api_key_auth());
    assert_eq!(fs::read(&database)?, source_database);

    let imported_database_path = destination.join("state_5.sqlite");
    let sqlite = codex_state::SqliteConfig::from_sqlite_home(AbsolutePathBuf::from_absolute_path(
        &destination,
    )?);
    let imported_database = sqlite.open_read_only_pool(&imported_database_path).await?;
    let values: Vec<String> = sqlx::query_scalar("SELECT value FROM records")
        .fetch_all(&imported_database)
        .await?;
    assert_eq!(values, vec!["source"]);
    Ok(())
}

#[tokio::test]
async fn credential_failure_rolls_back_filesystem_activation() -> Result<()> {
    let root = TempDir::new()?;
    let source = root.path().join(".codex");
    let destination = root.path().join(".better-codex");
    fs::create_dir(&source)?;
    fs::write(source.join("auth.json"), "not-json")?;
    fs::write(source.join("history.jsonl"), "source-history")?;

    command(&destination)?
        .args(["import-codex-state", "--from"])
        .arg(&source)
        .assert()
        .failure()
        .stderr(predicates::str::contains("migrate imported credentials"));

    assert!(!destination.exists());
    assert_eq!(
        fs::read_to_string(source.join("history.jsonl"))?,
        "source-history"
    );
    Ok(())
}
