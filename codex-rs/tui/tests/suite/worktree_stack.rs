//! Exercises stack-sensitive session transitions through the real TUI event loop.
//!
//! Uses the CLI binary when available (including Bazel CI) to cover its dispatch frames.

use super::focus_palette::PtyCodex;
use super::focus_palette::write_test_config;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use codex_worktree::ManagedWorktree;
use codex_worktree::WorktreeManager;
use codex_worktree::WorktreeSettings;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;
use wiremock::Match;
use wiremock::matchers::body_string_contains;
use wiremock::matchers::header;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn picker_side_worktree_fork_and_cd_run_on_the_production_stack() -> Result<()> {
    let start = if codex_utils_cargo_bin::cargo_bin("codex").is_ok() {
        PtyCodex::start_cli
    } else {
        PtyCodex::start
    };
    let repository = tempfile::tempdir_in("/tmp")?;
    let root = repository.path().canonicalize()?;
    let status = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&root)
        .status()?;
    ensure!(status.success(), "initialize test repository");
    fs::write(root.join("README.md"), "stack transition test\n")?;
    ensure!(
        Command::new("git")
            .args(["-C"])
            .arg(&root)
            .args(["add", "README.md"])
            .status()?
            .success(),
        "stage test repository"
    );
    ensure!(
        Command::new("git")
            .args(["-C"])
            .arg(&root)
            .args([
                "-c",
                "user.name=Codex Test",
                "-c",
                "user.email=test@example.invalid"
            ])
            .args(["commit", "--no-gpg-sign", "-qm", "initial"])
            .status()?
            .success(),
        "commit test repository"
    );

    let codex_home = tempfile::tempdir_in("/tmp")?;
    write_test_config(codex_home.path(), &root)?;
    let server = responses::start_mock_server().await;
    let config_path = codex_home.path().join("config.toml");
    let config = fs::read_to_string(&config_path)?
        .replace("model_provider = \"openai\"", "model_provider = \"test\"");
    fs::write(
        &config_path,
        format!(
            "{config}\n[model_providers.test]\nname = \"Mock\"\nbase_url = \"{}/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false\nsupports_websockets = false\n",
            server.uri()
        ),
    )?;
    // Title generation can include the user prompt and race the actual user turn.
    let _title = responses::mount_sse_once_match(
        &server,
        body_string_contains(r#"\"thread_source\":\"thread_title\""#),
        responses::sse(vec![
            responses::ev_assistant_message("title", r#"{"title":"Stack transition test"}"#),
            responses::ev_completed("title-response"),
        ]),
    )
    .await;
    let _reply = responses::mount_sse_once_match(
        &server,
        body_string_contains(r#"\"thread_source\":\"user\""#),
        responses::sse(vec![
            responses::ev_assistant_message("saved", "STACK_SAVED_HISTORY"),
            responses::ev_completed("saved-response"),
        ]),
    )
    .await;

    let worktrees = WorktreeManager::new(WorktreeSettings::for_cli(
        codex_home.path(),
        /*desktop*/ None,
    )?);
    // Both launchers give codex-main and Tokio workers explicit 16 MiB stacks.
    let mut terminal = start(
        &root,
        codex_home,
        &[
            "--no-alt-screen",
            "-c",
            "features.worktrees=true",
            "-c",
            "features.shell_snapshot=false",
            "Reply with the marker requested by this test.",
        ],
    )?;
    terminal.wait_for_startup()?;
    terminal.wait_for_screen("STACK_SAVED_HISTORY")?;
    terminal.wait_for_screen("Worked for")?;
    terminal.wait_for_screen("Ask Codex to do anything")?;

    // Starting and closing a side conversation both rebuild the displayed chat widget.
    // Repeat through the real event loop, whose dev-build frames share the production stack.
    for attempt in 1..=3 {
        submit(&mut terminal, "/side")?;
        terminal.wait_for_screen("Side from main thread")?;
        terminal.ensure_running()?;
        terminal.write_input(b"\x03")?;
        terminal.wait_for_screen("STACK_SAVED_HISTORY")?;
        terminal.wait_for_screen("Ask Codex to do anything")?;
        terminal.ensure_running()?;

        let draft = format!("SIDE_STACK_DRAFT_{attempt}");
        terminal.write_input(draft.as_bytes())?;
        terminal.wait_for_screen(&draft)?;
        terminal.write_input(b"\x15")?;
        terminal.wait_for_screen("Ask Codex to do anything")?;
    }

    submit(&mut terminal, "/resume")?;
    terminal.wait_for_screen("Resume a previous session")?;
    terminal.ensure_running()?;
    terminal.write_input(b"\x1b")?;
    terminal.wait_for_screen("Ask Codex to do anything")?;

    submit(&mut terminal, "/worktree")?;
    terminal.wait_for_screen("Continue current conversation")?;
    terminal.write_input(b"1")?;
    let forked = wait_for_owned_worktrees(&mut terminal, &worktrees, &root, /*count*/ 1)?;
    // The header changes before the queued history replay has finished attaching.
    // Wait for the transition acknowledgement before submitting another command.
    terminal.wait_for_screen(&format!(
        "Working directory changed to: {}",
        forked[0].root.display()
    ))?;
    terminal.ensure_running()?;

    // Exercise the attached fork and wait for its own live completion. A cached
    // idle status from the parent cannot establish the fork's task lifecycle.
    let fork_owner = worktrees
        .owner(&forked[0].root)?
        .context("forked worktree must have a conversation owner")?;
    const FORK_PROMPT: &str = "Confirm this forked worktree conversation is usable.";
    const FORK_REPLY: &str = "STACK_FORK_READY";
    let fork_header = header("thread-id", fork_owner.as_str());
    let fork_reply = responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            fork_header.matches(request)
                && body_string_contains(r#"\"thread_source\":\"user\""#).matches(request)
                && body_string_contains(FORK_PROMPT).matches(request)
        },
        responses::sse(vec![
            responses::ev_assistant_message("fork-ready", FORK_REPLY),
            responses::ev_completed("fork-ready-response"),
        ]),
    )
    .await;
    let fork_turn_start = terminal.output_len();
    submit(&mut terminal, FORK_PROMPT)?;
    terminal.wait_for_screen(FORK_REPLY)?;
    terminal.wait_for_done_since(fork_turn_start)?;
    let request = fork_reply.single_request();
    assert_eq!(request.header("thread-id"), Some(fork_owner));
    let request_json = request.body_json().to_string();
    ensure!(
        request_json.contains(FORK_PROMPT),
        "the fork probe prompt was not submitted"
    );
    ensure!(
        request_json.contains(r#"\"thread_source\":\"user\""#),
        "the fork probe was not an actual user turn"
    );
    ensure!(
        request_json.contains("STACK_SAVED_HISTORY"),
        "the forked native turn lost the original conversation"
    );
    ensure!(
        request_json.contains(forked[0].root.to_string_lossy().as_ref()),
        "the forked native turn did not receive its worktree cwd"
    );

    submit(&mut terminal, &format!("/cd {}", root.display()))?;
    terminal.wait_for_screen(&format!("Working directory changed to: {}", root.display()))?;
    terminal.ensure_running()?;

    submit(&mut terminal, "/worktree")?;
    terminal.wait_for_screen("Start new conversation")?;
    terminal.write_input(b"2")?;
    let created = wait_for_owned_worktrees(&mut terminal, &worktrees, &root, /*count*/ 2)?;
    ensure!(
        created
            .iter()
            .any(|checkout| checkout.root == forked[0].root)
    );
    let second = created
        .iter()
        .find(|checkout| checkout.root != forked[0].root)
        .expect("second checkout");
    ensure!(
        worktrees.owner(&forked[0].root)? != worktrees.owner(&second.root)?,
        "new worktree reused the forked thread owner"
    );
    terminal.wait_for_screen(&format!(
        "Working directory changed to: {}",
        second.root.display()
    ))?;
    terminal.ensure_running()?;
    Ok(())
}

fn submit(terminal: &mut PtyCodex, input: &str) -> Result<()> {
    // Send one paste event so slashes in arguments cannot accept a completion
    // while earlier characters are still buffered by paste-burst detection.
    terminal.write_input(format!("\x1b[200~{input}\x1b[201~").as_bytes())?;
    terminal.wait_for_screen(input)?;
    // Visible history and cwd acknowledgement can precede pending turn attachment.
    terminal.wait_for_idle()?;
    terminal.write_input(b"\r")?;
    Ok(())
}

fn wait_for_owned_worktrees(
    terminal: &mut PtyCodex,
    manager: &WorktreeManager,
    source: &Path,
    count: usize,
) -> Result<Vec<ManagedWorktree>> {
    let deadline = Instant::now() + Duration::from_secs(/*secs*/ 30);
    while Instant::now() < deadline {
        terminal.read_output(Duration::from_millis(/*millis*/ 50))?;
        let checkouts = manager.list(source).unwrap_or_default();
        if checkouts.len() == count
            && checkouts
                .iter()
                .all(|checkout| manager.owner(&checkout.root).ok().flatten().is_some())
        {
            return Ok(checkouts);
        }
        terminal.ensure_running()?;
    }
    anyhow::bail!(
        "did not bind {count} managed checkouts; screen:\n{}",
        terminal.screen_contents()
    )
}
