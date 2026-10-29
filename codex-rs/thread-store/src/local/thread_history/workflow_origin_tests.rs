//! Copied source ownership survives projection checkpoints without becoming local work.
use super::super::*;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::ThreadSettingsAppliedEvent;
use codex_protocol::protocol::ThreadSettingsSnapshot;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::io::Write;

fn append(path: &std::path::Path, ordinal: u64, item: RolloutItem) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&RolloutLine {
            timestamp: "2026-07-16T00:00:00.000Z".into(),
            ordinal: Some(ordinal),
            item
        })
        .unwrap()
    )
    .unwrap();
}

fn fact(owner: ThreadId, id: &str) -> codex_protocol::workflow::WorkflowLifecycleEvent {
    serde_json::from_value(json!({
        "id":id,"ownerThreadId":owner,"runId":"source-run","executionId":"source-execution","workflowName":"Original report",
        "completionMode":"display","sequence":0,"createdAt":1,"kind":{"type":"started","name":"Original report","description":"Inherited history retains its original owner"}
    })).unwrap()
}

#[tokio::test]
async fn copied_workflow_origins_survive_cold_incremental_projection_and_reject_later_foreign_facts()
 {
    let (home, store, child) = store_with_mode(ThreadHistoryMode::Paginated).await;
    let parent = ThreadId::new();
    let path = rollout_path(home.path(), child);
    let original = fs::read(&path).unwrap();
    assert!(
        matches!(
            crate::local::thread_history_materialization::materialize_to_sqlite(
                &store, child, parent, &path
            )
            .await,
            Err(ThreadStoreError::InvalidRequest { .. })
        ),
        "a physical projection key does not authorize an unrelated logical owner"
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    fs::write(&path, "").unwrap();
    for (ordinal, id, forked_from_id) in [(0, child, Some(parent)), (1, parent, None)] {
        append(
            &path,
            ordinal,
            RolloutItem::SessionMeta(Box::new(SessionMetaLine {
                meta: SessionMeta {
                    id,
                    session_id: id.into(),
                    forked_from_id,
                    history_mode: ThreadHistoryMode::Paginated,
                    ..Default::default()
                },
                git: None,
            })),
        );
    }
    crate::local::thread_history_materialization::materialize_to_sqlite(
        &store, child, child, &path,
    )
    .await
    .expect("project only the proven copied headers");
    let inherited = fact(parent, "inherited-card");
    append(
        &path,
        2,
        RolloutItem::EventMsg(EventMsg::WorkflowLifecycle(inherited.clone())),
    );
    let cold = LocalThreadStore::new(store.config.clone(), store.state_db.clone());
    crate::local::thread_history_materialization::materialize_to_sqlite(&cold, child, child, &path)
        .await
        .expect("restore source proof from the preceding checkpoint");
    let inherited_page = cold
        .list_timeline(ListTimelineParams {
            thread_id: child,
            cursor: None,
            page_size: 100,
        })
        .await
        .unwrap();
    assert_eq!(
        inherited_page.items,
        vec![ThreadTimelineEntry::Workflow {
            position: 2,
            event: Box::new(inherited.clone().into())
        }]
    );

    let prefix = fs::read(&path).unwrap();
    append(
        &path,
        3,
        RolloutItem::EventMsg(EventMsg::WorkflowLifecycle(fact(
            ThreadId::new(),
            "unrelated-card",
        ))),
    );
    assert!(
        matches!(
            crate::local::thread_history_materialization::materialize_to_sqlite(
                &cold, child, child, &path
            )
            .await,
            Err(ThreadStoreError::InvalidRequest { .. })
        ),
        "being inside a copied prefix does not authorize an unrelated owner"
    );
    // Restore the isolated invalid-input fixture after proving transaction rollback.
    fs::write(&path, prefix).unwrap();
    let cwd = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path_checked(home.path())
        .unwrap();
    append(
        &path,
        3,
        RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(
            ThreadSettingsAppliedEvent {
                thread_id: Some(child),
                thread_settings: ThreadSettingsSnapshot {
                    long_context: None,
                    model: "model".into(),
                    model_provider_id: "test-provider".into(),
                    service_tier: None,
                    approval_policy: AskForApproval::Never,
                    approvals_reviewer: ApprovalsReviewer::User,
                    permission_profile: PermissionProfile::Disabled,
                    active_permission_profile: None,
                    cwd,
                    runtime_workspace_roots: None,
                    reasoning_effort: None,
                    reasoning_summary: None,
                    personality: None,
                    collaboration_mode: CollaborationMode {
                        mode: ModeKind::Default,
                        settings: Settings {
                            model: "model".into(),
                            reasoning_effort: None,
                            developer_instructions: None,
                        },
                    },
                    disabled_plugin_ids: Vec::new(),
                },
            },
        )),
    );
    let own = fact(child, "local-card");
    append(
        &path,
        4,
        RolloutItem::EventMsg(EventMsg::WorkflowLifecycle(own.clone())),
    );
    let bytes = fs::read(&path).unwrap();
    crate::local::thread_history_materialization::materialize_to_sqlite(&cold, child, child, &path)
        .await
        .unwrap();
    let expected = vec![
        ThreadTimelineEntry::Workflow {
            position: 2,
            event: Box::new(inherited.clone().into()),
        },
        ThreadTimelineEntry::Workflow {
            position: 4,
            event: Box::new(own.into()),
        },
    ];
    assert_eq!(
        cold.list_timeline(ListTimelineParams {
            thread_id: child,
            cursor: None,
            page_size: 100
        })
        .await
        .unwrap()
        .items,
        expected
    );

    sqlx::query("UPDATE thread_history_projection_state SET workflow_origin_state = NULL WHERE thread_id = ?")
        .bind(child.to_string()).execute(history_db(&cold).await).await.unwrap();
    crate::local::thread_history_materialization::materialize_to_sqlite(&cold, child, child, &path)
        .await
        .expect("upgrade only the old copied-fork projection once");
    assert_eq!(
        cold.list_timeline(ListTimelineParams {
            thread_id: child,
            cursor: None,
            page_size: 100
        })
        .await
        .unwrap()
        .items,
        expected
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        bytes,
        "checkpoint upgrade must not rewrite authoritative history"
    );
    let checkpoint = crate::local::thread_history::projection_state(&cold, child)
        .await
        .unwrap()
        .unwrap();
    assert!(checkpoint.workflow_origin_state.is_some());

    append(
        &path,
        5,
        RolloutItem::EventMsg(EventMsg::WorkflowLifecycle(inherited)),
    );
    assert!(
        matches!(
            crate::local::thread_history_materialization::materialize_to_sqlite(
                &cold, child, child, &path
            )
            .await,
            Err(ThreadStoreError::InvalidRequest { .. })
        ),
        "even a genuine source fact cannot be appended as local work after the child's own boundary"
    );
}
