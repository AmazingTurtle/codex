use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::CODEX_ACCESS_TOKEN_ENV_VAR;
use codex_protocol::shell_environment::OPENAI_FEDERATION_RULE_ID_ENV_VAR;
use codex_protocol::shell_environment::OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("better-codex")?);
    cmd.env("BETTER_CODEX_HOME", codex_home);
    Ok(cmd)
}

fn write_file_auth_config(codex_home: &Path) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        "cli_auth_credentials_store = \"file\"\n",
    )?;
    Ok(())
}

fn read_auth_json(codex_home: &Path) -> Result<Value> {
    let auth_json = std::fs::read_to_string(codex_home.join("auth.json"))?;
    Ok(serde_json::from_str(&auth_json)?)
}

#[test]
fn login_with_api_key_reads_stdin_and_writes_auth_json() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args([
        "-c",
        "forced_login_method=\"api\"",
        "login",
        "--with-api-key",
    ])
    .write_stdin("sk-test\n")
    .assert()
    .success()
    .stderr(contains("Successfully logged in"));

    let auth = read_auth_json(codex_home.path())?;
    assert_eq!(auth["OPENAI_API_KEY"], "sk-test");
    assert!(auth.get("tokens").is_none());
    assert!(auth.get("agent_identity").is_none());

    Ok(())
}

#[test]
fn login_ignores_legacy_home_environment_variables() -> Result<()> {
    let better_home = TempDir::new()?;
    let legacy_home = TempDir::new()?;
    let legacy_sqlite_home = TempDir::new()?;
    write_file_auth_config(better_home.path())?;

    codex_command(better_home.path())?
        .env("CODEX_HOME", legacy_home.path())
        .env("CODEX_SQLITE_HOME", legacy_sqlite_home.path())
        .args([
            "-c",
            "forced_login_method=\"api\"",
            "login",
            "--with-api-key",
        ])
        .write_stdin("sk-isolated\n")
        .assert()
        .success();

    assert_eq!(
        read_auth_json(better_home.path())?["OPENAI_API_KEY"],
        "sk-isolated"
    );
    assert!(!legacy_home.path().join("auth.json").exists());
    assert!(legacy_sqlite_home.path().read_dir()?.next().is_none());
    Ok(())
}

#[test]
fn login_status_reports_auth_storage_errors() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    std::fs::write(codex_home.path().join("auth.json"), "{invalid json")?;

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .failure()
        .stderr(contains("Error checking login status:"));

    Ok(())
}

#[test]
fn login_status_validates_configured_workload_identity() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    let missing_assertion = codex_home.path().join("missing-identity-token");

    codex_command(codex_home.path())?
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env(OPENAI_FEDERATION_RULE_ID_ENV_VAR, "rule-test")
        .env(OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR, &missing_assertion)
        .args(["login", "status"])
        .assert()
        .failure()
        .stderr(contains("could not read workload identity assertion file"));

    Ok(())
}

#[test]
fn login_with_access_token_rejects_invalid_jwt() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["login", "--with-access-token"])
        .write_stdin("not-a-jwt\n")
        .assert()
        .failure()
        .stderr(contains("Error logging in with access token"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn debug_prompt_input_follows_authenticated_attribution_setting() -> Result<()> {
    let server = MockServer::start().await;
    let request_count = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/settings/user"))
        .and(header("chatgpt-account-id", "workspace-123"))
        .respond_with(move |_request: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(json!({
                "commit_attribution_enabled": request_count.fetch_add(1, Ordering::SeqCst) == 0,
            }))
        })
        .expect(2)
        .mount(&server)
        .await;
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n",
            server.uri()
        ),
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("workspace-123")
            .plan_type("enterprise"),
        AuthCredentialsStoreMode::File,
    )?;
    for enabled in [true, false] {
        let output = codex_command(codex_home.path())?
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("OPENAI_API_KEY")
            .args(["debug", "prompt-input"])
            .output()?;
        assert!(output.status.success());
        let prompt = String::from_utf8(output.stdout)?;
        assert_eq!(
            prompt.contains("Co-authored-by: Codex <noreply@openai.com>"),
            enabled
        );
        assert!(!prompt.contains("attribution is disabled for the current workspace"));
    }
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_login_preserves_existing_chatgpt_account() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-123",
            "user_code": "CODE-12345",
            "interval": "0",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_code": "authorization-code-123",
            "code_challenge": "code-challenge-123",
            "code_verifier": "code-verifier-123",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id_token": "eyJhbGciOiJub25lIn0.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoibmV3LWFjY291bnQifX0.c2ln",
            "access_token": "new-access",
            "refresh_token": "new-refresh",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    std::fs::write(
        codex_home.path().join("auth.json"),
        serde_json::to_vec(&json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "account_id": "old-account",
            },
        }))?,
    )?;

    let issuer = server.uri();
    let mut cmd = codex_command(codex_home.path())?;
    cmd.env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("OPENAI_API_KEY")
        .args(["login", "--device-auth", "--experimental_issuer", &issuer])
        .assert()
        .success()
        .stderr(contains("Successfully logged in"));

    let requests = server
        .received_requests()
        .await
        .context("failed to read mock OAuth requests")?;
    let paths: Vec<&str> = requests.iter().map(|request| request.url.path()).collect();
    assert_eq!(
        paths,
        vec![
            "/api/accounts/deviceauth/usercode",
            "/api/accounts/deviceauth/token",
            "/oauth/token",
        ]
    );

    let auth = read_auth_json(codex_home.path())?;
    assert_eq!(auth["tokens"]["refresh_token"], "new-refresh");
    assert_eq!(auth["tokens"]["account_id"], "new-account");
    assert_eq!(auth["accounts"][0]["tokens"]["account_id"], "old-account");
    Ok(())
}

#[test]
fn logout_can_remove_one_chatgpt_account() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    std::fs::write(
        codex_home.path().join("auth.json"),
        serde_json::to_vec(&json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
                "access_token": "",
                "refresh_token": "",
                "account_id": "active-account",
            },
            "accounts": [{
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
                    "access_token": "",
                    "refresh_token": "",
                    "account_id": "other-account",
                },
            }],
        }))?,
    )?;

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .success()
        .stderr(contains("active-account"))
        .stderr(contains("other-account"));

    codex_command(codex_home.path())?
        .args(["logout", "--account", "other-account"])
        .assert()
        .success()
        .stderr(contains("Successfully logged out account other-account"));

    let auth = read_auth_json(codex_home.path())?;
    assert_eq!(auth["tokens"]["account_id"], "active-account");
    assert!(auth.get("accounts").is_none());
    Ok(())
}

#[test]
fn login_status_uses_detailed_view_for_one_chatgpt_account() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    std::fs::write(
        codex_home.path().join("auth.json"),
        serde_json::to_vec(&json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
                "access_token": "access-token",
                "refresh_token": "refresh-token",
                "account_id": "one-account",
            },
        }))?,
    )?;

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .success()
        .stderr(contains(
            "Logged in using ChatGPT\nStored ChatGPT 1 account:",
        ))
        .stderr(contains("  one-account — active"));
    Ok(())
}
