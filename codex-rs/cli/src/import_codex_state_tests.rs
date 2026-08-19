use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[test]
fn import_excludes_runtime_and_credential_artifacts() {
    let excluded = [
        ".tmp/plugins.sync.lock",
        "tmp/arg0/helper",
        "secrets/local.age",
        "state_5.sqlite-wal",
        "daemon.sock",
        "version.json",
    ];
    assert_eq!(
        excluded.map(|path| should_exclude(Path::new(path))),
        [true; 6]
    );
}

#[test]
fn import_keeps_persistent_state() {
    let included = [
        "config.toml",
        "auth.json",
        "sessions/2026/rollout.jsonl",
        "history.jsonl",
        "skills/custom/SKILL.md",
        "state_5.sqlite",
    ];
    assert_eq!(
        included.map(|path| should_exclude(Path::new(path))),
        [false; 6]
    );
}

#[test]
fn destination_must_be_absent_or_empty() {
    let root = TempDir::new().expect("temp dir");
    let missing = root.path().join("missing");
    ensure_destination_available(&missing).expect("missing destination");

    let empty = root.path().join("empty");
    fs::create_dir(&empty).expect("empty destination");
    ensure_destination_available(&empty).expect("empty destination");

    fs::write(empty.join("config.toml"), "model = 'gpt-5'").expect("destination file");
    assert!(ensure_destination_available(&empty).is_err());

    let file = root.path().join("file");
    fs::write(&file, "not a directory").expect("destination file");
    assert!(ensure_destination_available(&file).is_err());
}

#[tokio::test]
async fn stage_import_snapshots_sqlite_and_rewrites_config_paths() {
    let root = TempDir::new().expect("temp dir");
    let source = root.path().join(".codex");
    let stage = root.path().join("stage");
    let destination = root.path().join(".better-codex");
    fs::create_dir(&source).expect("source");
    fs::create_dir(&stage).expect("stage");
    fs::write(
        source.join("config.toml"),
        format!("log_dir = {:?}\n", source.join("log")),
    )
    .expect("config");
    fs::create_dir(source.join("sessions")).expect("sessions");
    fs::write(source.join("sessions/rollout.jsonl"), "session").expect("rollout");
    fs::write(source.join("daemon.lock"), "runtime").expect("lock");

    let database = source.join("state_5.sqlite");
    let sqlite = codex_state::SqliteConfig::from_sqlite_home(
        AbsolutePathBuf::from_absolute_path(&source).expect("absolute source"),
    );
    let connection = sqlite
        .open_read_write_pool(&database)
        .await
        .expect("source database");
    sqlx::query("CREATE TABLE records (value TEXT NOT NULL)")
        .execute(&connection)
        .await
        .expect("schema");
    sqlx::query("INSERT INTO records (value) VALUES ('source')")
        .execute(&connection)
        .await
        .expect("row");
    connection.close().await;

    let entries = collect_import_entries(&source).expect("entries");
    stage_import(&source, &stage, &destination, &entries)
        .await
        .expect("stage import");

    assert!(!stage.join("daemon.lock").exists());
    assert_eq!(
        fs::read_to_string(stage.join("sessions/rollout.jsonl")).expect("staged rollout"),
        "session"
    );
    let config = fs::read_to_string(stage.join("config.toml")).expect("staged config");
    assert!(config.contains(&destination.to_string_lossy().to_string()));
    assert!(!config.contains(&source.to_string_lossy().to_string()));

    let snapshot_path = stage.join("state_5.sqlite");
    let sqlite = codex_state::SqliteConfig::from_sqlite_home(
        AbsolutePathBuf::from_absolute_path(&stage).expect("absolute stage"),
    );
    let snapshot = sqlite
        .open_read_only_pool(&snapshot_path)
        .await
        .expect("snapshot database");
    let values: Vec<String> = sqlx::query_scalar("SELECT value FROM records")
        .fetch_all(&snapshot)
        .await
        .expect("snapshot rows");
    assert_eq!(values, vec!["source"]);
}

#[cfg(unix)]
#[test]
fn absolute_symlinks_into_source_are_rewritten() {
    let root = TempDir::new().expect("temp dir");
    let source = root.path().join(".codex");
    let destination = root.path().join(".better-codex");
    fs::create_dir(&source).expect("source");
    fs::write(source.join("target"), "value").expect("target");
    let link = source.join("link");
    std::os::unix::fs::symlink(source.join("target"), &link).expect("symlink");

    assert_eq!(
        rewritten_symlink_target(&link, &source, &destination).expect("rewritten target"),
        destination.join("target")
    );
}
