use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Args;
use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AuthKeyringBackendKind;
use codex_config::types::OAuthCredentialsStoreMode;
use codex_login::load_auth_dot_json;
use codex_login::logout;
use codex_login::save_auth;
use codex_secrets::LocalSecretsNamespace;
use codex_secrets::SecretListEntry;
use codex_secrets::SecretsBackendKind;
use codex_secrets::SecretsManager;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_home_dir::find_codex_home_for_creation;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub(crate) struct ImportCodexStateCommand {
    /// Upstream Codex home to import. Defaults to ~/.codex.
    #[arg(long, value_name = "DIR")]
    from: Option<PathBuf>,

    /// Show what would be imported without writing files or credentials.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Copy)]
struct CredentialSettings {
    auth_store: AuthCredentialsStoreMode,
    keyring_backend: AuthKeyringBackendKind,
}

pub(crate) async fn run(command: ImportCodexStateCommand) -> Result<()> {
    let source = match command.from {
        Some(path) => absolute_existing_directory(&path)?,
        None => {
            let home = dirs::home_dir().context("could not find the user home directory")?;
            absolute_existing_directory(&home.join(".codex"))?
        }
    };
    let destination = find_codex_home_for_creation()?.into_path_buf();
    if source == destination {
        bail!("source and Better Codex home resolve to the same directory");
    }
    ensure_destination_available(&destination)?;

    let entries = collect_import_entries(&source)?;
    let sqlite_files = entries.iter().filter(|path| is_sqlite_file(path)).count();
    if command.dry_run {
        println!("Source: {}", source.display());
        println!("Destination: {}", destination.display());
        println!(
            "Would import {} filesystem entries, including {sqlite_files} consistent SQLite snapshots.",
            entries.len()
        );
        println!("The upstream source would not be modified.");
        return Ok(());
    }

    let destination_was_empty = destination.exists();
    let stage = import_stage_path(&destination)?;
    fs::create_dir(&stage)
        .with_context(|| format!("create import staging directory {}", stage.display()))?;
    let staged = stage_import(&source, &stage, &destination, &entries).await;
    if let Err(err) = staged {
        let _ = fs::remove_dir_all(&stage);
        return Err(err);
    }

    if destination_was_empty {
        fs::remove_dir(&destination)
            .with_context(|| format!("remove empty Better Codex home {}", destination.display()))?;
    }
    if let Err(err) = fs::rename(&stage, &destination) {
        let _ = fs::remove_dir_all(&stage);
        if destination_was_empty {
            let _ = fs::create_dir(&destination);
        }
        return Err(err).context("activate imported Better Codex home");
    }

    let settings = credential_settings(&destination)?;
    if let Err(err) = migrate_credentials(&source, &destination, settings).await {
        rollback_credentials(&destination, settings);
        if let Err(rename_err) = fs::rename(&destination, &stage) {
            return Err(err).context(format!(
                "credential migration failed and filesystem rollback also failed: {rename_err}"
            ));
        }
        let _ = fs::remove_dir_all(&stage);
        if destination_was_empty {
            let _ = fs::create_dir(&destination);
        }
        return Err(err).context("migrate imported credentials");
    }

    println!(
        "Imported upstream Codex state into {}. The source at {} was left unchanged.",
        destination.display(),
        source.display()
    );
    Ok(())
}

fn absolute_existing_directory(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolve source directory {}", path.display()))?;
    if !canonical.is_dir() {
        bail!("source is not a directory: {}", canonical.display());
    }
    Ok(canonical)
}

fn ensure_destination_available(destination: &Path) -> Result<()> {
    if !destination.exists() {
        return Ok(());
    }
    if !destination.is_dir() {
        bail!(
            "Better Codex home is not a directory: {}",
            destination.display()
        );
    }
    if fs::read_dir(destination)?.next().transpose()?.is_some() {
        bail!(
            "Better Codex home is not empty: {}. Move it aside before importing.",
            destination.display()
        );
    }
    Ok(())
}

fn import_stage_path(destination: &Path) -> Result<PathBuf> {
    let parent = destination
        .parent()
        .context("Better Codex home has no parent directory")?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .context("Better Codex home name is not valid UTF-8")?;
    Ok(parent.join(format!(".{name}.import-{}", std::process::id())))
}

fn collect_import_entries(source: &Path) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, directory: &Path, entries: &mut Vec<PathBuf>) -> Result<()> {
        let mut children = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_unstable_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            let relative = path.strip_prefix(root)?;
            if should_exclude(relative) {
                continue;
            }
            entries.push(path.clone());
            if child.file_type()?.is_dir() {
                visit(root, &path, entries)?;
            }
        }
        Ok(())
    }

    let mut entries = Vec::new();
    visit(source, source, &mut entries)?;
    Ok(entries)
}

fn should_exclude(relative: &Path) -> bool {
    let Some(first) = relative.components().next() else {
        return false;
    };
    let first = first.as_os_str().to_string_lossy();
    if matches!(
        first.as_ref(),
        "tmp" | ".tmp" | "arg0" | "standalone" | "packages" | "secrets" | "version.json"
    ) {
        return true;
    }
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with(".lock")
        || name.ends_with(".sock")
        || name.ends_with(".sqlite-wal")
        || name.ends_with(".sqlite-shm")
        || name.ends_with(".age")
}

fn is_sqlite_file(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("sqlite")
}

async fn stage_import(
    source: &Path,
    stage: &Path,
    destination: &Path,
    entries: &[PathBuf],
) -> Result<()> {
    for path in entries {
        let relative = path.strip_prefix(source)?;
        let target = stage.join(relative);
        let file_type = fs::symlink_metadata(path)?.file_type();
        if file_type.is_dir() {
            fs::create_dir(&target)?;
            fs::set_permissions(&target, fs::metadata(path)?.permissions())?;
        } else if file_type.is_symlink() {
            copy_symlink(path, &target, source, destination)?;
        } else if file_type.is_file() {
            if is_sqlite_file(path) {
                snapshot_sqlite(path, &target).await?;
            } else {
                fs::copy(path, &target)?;
                fs::set_permissions(&target, fs::metadata(path)?.permissions())?;
            }
        }
    }
    rewrite_home_paths(stage, source, destination)?;
    Ok(())
}

async fn snapshot_sqlite(source: &Path, destination: &Path) -> Result<()> {
    let sqlite_home = source
        .parent()
        .context("SQLite database has no parent directory")?;
    let sqlite_home = AbsolutePathBuf::from_absolute_path(sqlite_home)?;
    let sqlite = codex_state::SqliteConfig::from_sqlite_home(sqlite_home);
    let pool = sqlite
        .open_read_only_pool(source)
        .await
        .with_context(|| format!("open SQLite database {}", source.display()))?;
    let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&pool)
        .await
        .with_context(|| format!("check SQLite database {}", source.display()))?;
    if integrity != "ok" {
        bail!(
            "SQLite integrity check failed for {}: {integrity}",
            source.display()
        );
    }
    let quoted = destination.to_string_lossy().replace(char::from(39), "''");
    let snapshot_query = format!("VACUUM INTO '{quoted}'");
    sqlx::query(sqlx::AssertSqlSafe(snapshot_query))
        .execute(&pool)
        .await
        .with_context(|| {
            format!(
                "snapshot busy SQLite database {}; stop upstream Codex and retry",
                source.display()
            )
        })?;
    pool.close().await;
    Ok(())
}

fn rewrite_home_paths(stage: &Path, source: &Path, destination: &Path) -> Result<()> {
    let source = source.to_string_lossy();
    let destination = destination.to_string_lossy();
    for entry in fs::read_dir(stage)? {
        let entry = entry?;
        let path = entry.path();
        let is_toml = path.extension().and_then(|extension| extension.to_str()) == Some("toml");
        if !is_toml || !entry.file_type()?.is_file() {
            continue;
        }
        let contents = fs::read_to_string(&path)?;
        let rewritten = contents.replace(source.as_ref(), destination.as_ref());
        if rewritten != contents {
            fs::write(path, rewritten)?;
        }
    }
    Ok(())
}

fn credential_settings(home: &Path) -> Result<CredentialSettings> {
    let path = home.join(codex_config::CONFIG_TOML_FILE);
    let config = match fs::read_to_string(path) {
        Ok(contents) => toml::from_str::<toml::Value>(&contents)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(Default::default())
        }
        Err(err) => return Err(err.into()),
    };
    let auth_store = match config
        .get("cli_auth_credentials_store")
        .and_then(toml::Value::as_str)
    {
        Some("keyring") => AuthCredentialsStoreMode::Keyring,
        Some("auto") => AuthCredentialsStoreMode::Auto,
        Some("ephemeral") => AuthCredentialsStoreMode::Ephemeral,
        _ => AuthCredentialsStoreMode::File,
    };
    let secret_auth_storage = config
        .get("features")
        .and_then(|features| features.get("secret_auth_storage"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    let keyring_backend = if secret_auth_storage {
        AuthKeyringBackendKind::Secrets
    } else {
        AuthKeyringBackendKind::Direct
    };
    Ok(CredentialSettings {
        auth_store,
        keyring_backend,
    })
}

async fn migrate_credentials(
    source: &Path,
    destination: &Path,
    settings: CredentialSettings,
) -> Result<()> {
    if let Some(auth) = load_auth_dot_json(source, settings.auth_store, settings.keyring_backend)? {
        save_auth(
            destination,
            &auth,
            settings.auth_store,
            settings.keyring_backend,
        )?;
    }
    migrate_secrets_namespace(source, destination, LocalSecretsNamespace::ManagedSecrets)?;
    if settings.keyring_backend == AuthKeyringBackendKind::Secrets {
        migrate_secrets_namespace(source, destination, LocalSecretsNamespace::McpOAuth)?;
    } else {
        migrate_direct_mcp_credentials(destination)?;
    }
    Ok(())
}

fn migrate_secrets_namespace(
    source: &Path,
    destination: &Path,
    namespace: LocalSecretsNamespace,
) -> Result<Vec<SecretListEntry>> {
    let source_manager = SecretsManager::new_with_keyring_store_and_namespace(
        source.to_path_buf(),
        SecretsBackendKind::Local,
        std::sync::Arc::new(codex_keyring_store::DefaultKeyringStore),
        namespace,
    );
    let destination_manager = SecretsManager::new_with_keyring_store_and_namespace(
        destination.to_path_buf(),
        SecretsBackendKind::Local,
        std::sync::Arc::new(codex_keyring_store::DefaultKeyringStore),
        namespace,
    );
    let entries = source_manager.list(/*scope_filter*/ None)?;
    for entry in &entries {
        if let Some(value) = source_manager.get(&entry.scope, &entry.name)? {
            destination_manager.set(&entry.scope, &entry.name, &value)?;
        }
    }
    Ok(entries)
}

fn migrate_direct_mcp_credentials(home: &Path) -> Result<()> {
    let path = home.join(codex_config::CONFIG_TOML_FILE);
    let Ok(contents) = fs::read_to_string(path) else {
        return Ok(());
    };
    let config: toml::Value = toml::from_str(&contents)?;
    let Some(servers) = config.get("mcp_servers").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    let mut imported = Vec::new();
    for (name, server) in servers {
        if let Some(url) = server.get("url").and_then(toml::Value::as_str) {
            match codex_rmcp_client::import_upstream_direct_keyring_oauth_tokens(name, url) {
                Ok(true) => imported.push((name.as_str(), url)),
                Ok(false) => {}
                Err(err) => {
                    for (imported_name, imported_url) in imported {
                        let _ = codex_rmcp_client::delete_oauth_tokens(
                            imported_name,
                            imported_url,
                            OAuthCredentialsStoreMode::Keyring,
                            AuthKeyringBackendKind::Direct,
                        );
                    }
                    return Err(err);
                }
            }
        }
    }
    Ok(())
}

fn rollback_credentials(destination: &Path, settings: CredentialSettings) {
    let _ = logout(destination, settings.auth_store, settings.keyring_backend);
    let _ = codex_secrets::delete_local_secrets_key(destination);
}

#[cfg(unix)]
fn copy_symlink(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
) -> Result<()> {
    let target = rewritten_symlink_target(source, source_root, destination_root)?;
    std::os::unix::fs::symlink(target, destination)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
) -> Result<()> {
    let target = rewritten_symlink_target(source, source_root, destination_root)?;
    if fs::metadata(source)?.is_dir() {
        std::os::windows::fs::symlink_dir(target, destination)?;
    } else {
        std::os::windows::fs::symlink_file(target, destination)?;
    }
    Ok(())
}

fn rewritten_symlink_target(
    link: &Path,
    source_root: &Path,
    destination_root: &Path,
) -> Result<PathBuf> {
    let target = fs::read_link(link)?;
    if let Ok(relative) = target.strip_prefix(source_root) {
        Ok(destination_root.join(relative))
    } else {
        Ok(target)
    }
}

#[cfg(test)]
#[path = "import_codex_state_tests.rs"]
mod tests;
