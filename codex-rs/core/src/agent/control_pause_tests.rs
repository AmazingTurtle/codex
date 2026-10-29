use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn stopped_spawn_leaves_no_child_and_does_not_require_unpausing() {
    let harness = AgentControlHarness::new().await;
    let root = ThreadId::new();
    let runtime = &harness.control.runtime;
    runtime
        .execution_control
        .initialize(harness.config.codex_home.as_path(), root)
        .await
        .expect("initialize conversation control");
    runtime.execution_control.pause().await.expect("pause tree");
    let cancellation = CancellationToken::new();
    let mut spawn = harness.control.spawn(SpawnRequest {
        caller: root,
        config: harness.config.clone(),
        input: AgentInput::UserInput(text_input("must never start")),
        source: SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: root,
            depth: 1,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
        }),
        options: SpawnAgentOptions {
            admission_cancellation: Some(cancellation.clone()),
            ..Default::default()
        },
    });
    assert!(
        spawn
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "a paused spawn must wait before allocating its child"
    );
    cancellation.cancel();
    let result = timeout(Duration::from_secs(5), spawn)
        .await
        .expect("stop must settle without waiting for unpause");
    assert_matches!(result, Err(error) if matches!(error.details(), CodexErrorDetails::TurnAborted));
    let state = runtime.upgrade().expect("manager remains alive");
    assert_eq!(state.threads.read().await.len(), 0);
    assert_eq!(
        runtime
            .execution_control
            .snapshot()
            .await
            .expect("state")
            .state,
        codex_protocol::protocol::ExecutionControlState::Paused
    );
}
