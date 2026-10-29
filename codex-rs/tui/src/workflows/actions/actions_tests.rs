//! Native action/menu/editor journeys preserve captured identities and read-only history.
use super::*;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ListSelectionView;
use crate::keymap::RuntimeKeymap;
use crate::render::renderable::Renderable;
use crossterm::event::KeyCode;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;

fn snapshot() -> ActionSnapshot {
    ActionSnapshot {
        footer_hidden:false,
        scope:ActionScope {context_id:uuid::Uuid::from_u128(1),owner:codex_protocol::ThreadId::from_u128(7),caller:codex_protocol::ThreadId::from_u128(42),workflow_id:"run-1".into(),execution_id:"execution-1".into(),expected_control_revision:9,target:ActionTarget::Step {logical_id:"review".into(),expected_attempt_id:Some("attempt-2".into()),expected_turn_id:Some("turn-3".into())}},
        title:"Review source".into(),availability:[Action::Guide,Action::EditQueued,Action::Skip,Action::Interrupt,Action::Revise,Action::ApplyDrain,Action::ApplyInterrupt,Action::Resume,Action::ShowResult,Action::Revisions].into_iter().map(|action|ActionAvailability {action,unavailable_reason:if action==Action::EditQueued {Some("The selected step has already started".into())} else {None}}).collect(),
        queued_prompt:None,source:"workflow.meta({name:'audit',description:'Review source',phases:[]});\nreturn workflow.args;".into(),input:json!({"n":3}),staged_revision_id:Some("revision-2".into()),active_revision_id:Some("revision-1".into()),budget_summary:"2 / 8 starts · 14K / 100K tokens".into(),
    }
}

fn channel() -> (IntentSender, Arc<Mutex<Vec<WorkflowUiIntent>>>) {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let captured = messages.clone();
    (
        Arc::new(move |intent| captured.lock().unwrap().push(intent)),
        messages,
    )
}

fn render(view: &dyn Renderable, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn action_picker_shows_unavailable_reasons_and_preserves_captured_control_identity() {
    let snapshot = snapshot();
    let (send, messages) = channel();
    let params = menu(snapshot.clone(), send);
    assert!(params.items[1].is_disabled);
    assert_eq!(
        params.items[1].disabled_reason.as_deref(),
        Some("The selected step has already started")
    );
    assert!(params.items[3].require_explicit_confirmation);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let tx = AppEventSender::new(tx);
    (params.items[0].actions[0])(&tx);
    assert_eq!(
        messages.lock().unwrap().as_slice(),
        [WorkflowUiIntent::OpenForm {
            snapshot: Box::new(snapshot),
            action: Action::Guide
        }]
    );
    let picker = ListSelectionView::new(params, tx, RuntimeKeymap::defaults().list);
    insta::assert_snapshot!("workflow_actions_picker", render(&picker, 100, 24));
    insta::assert_snapshot!("workflow_actions_picker_narrow", render(&picker, 42, 16));
}

#[test]
fn footer_visibility_action_restores_a_run_without_a_backend_mutation() {
    let mut original = snapshot();
    original.footer_hidden = true;
    let (send, messages) = channel();
    let mut params = menu(original.clone(), send);
    let restore = params.items.len() - 1;
    assert_eq!(params.items[restore].name, "Show in footer");
    assert!(!params.items[restore].is_disabled);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let tx = AppEventSender::new(tx);
    (params.items[restore].actions[0])(&tx);
    assert_eq!(
        messages.lock().unwrap().as_slice(),
        [WorkflowUiIntent::SetFooterVisibility {
            scope: original.scope,
            visible: true
        }]
    );
    params.initial_selected_idx = Some(restore);
    let view = ListSelectionView::new(params, tx, RuntimeKeymap::defaults().list);
    let screen = render(&view, 100, 24);
    assert!(
        screen.contains("Show in footer") && screen.contains("stays available"),
        "{screen}"
    );
    insta::assert_snapshot!("workflow_footer_restore_action", screen);
}

#[test]
fn guide_form_treats_workflow_keys_as_text_and_does_not_retarget_after_opening() {
    let original = snapshot();
    let (send, messages) = channel();
    let mut view = form(
        original.clone(),
        Action::Guide,
        send,
        &RuntimeKeymap::defaults(),
    )
    .unwrap();
    view.handle_paste("Use a narrower review. Preserve 日本語. 1/2/3 Space actions".into());
    view.handle_key_event(KeyCode::Enter.into());
    assert_eq!(
        messages.lock().unwrap().as_slice(),
        [WorkflowUiIntent::Control {
            scope: original.scope,
            operation: ControlOperation::Guide {
                message: "Use a narrower review. Preserve 日本語. 1/2/3 Space actions".into()
            }
        }]
    );
}

#[test]
fn skip_defaults_to_null_and_invalid_json_stays_editable_without_dispatch() {
    let (send, messages) = channel();
    let original = snapshot();
    let mut view = form(
        original.clone(),
        Action::Skip,
        send,
        &RuntimeKeymap::defaults(),
    )
    .unwrap();
    view.handle_key_event(KeyCode::Enter.into());
    assert_eq!(
        messages.lock().unwrap().as_slice(),
        [WorkflowUiIntent::Control {
            scope: original.scope,
            operation: ControlOperation::Skip {
                replacement: serde_json::Value::Null
            }
        }]
    );
    let (send, messages) = channel();
    let mut invalid = form(snapshot(), Action::Skip, send, &RuntimeKeymap::defaults()).unwrap();
    invalid.handle_paste(" trailing".into());
    invalid.handle_key_event(KeyCode::Enter.into());
    assert!(messages.lock().unwrap().is_empty());
    assert!(!invalid.is_complete());
    let screen = render(&invalid, 80, 12);
    assert!(screen.contains("Enter JSON"), "{screen}");
    insta::assert_snapshot!("workflow_skip_invalid_json", screen);
}

#[test]
fn revision_source_is_prefilled_multiline_then_staged_with_explicit_arguments() {
    let original = snapshot();
    let (send, messages) = channel();
    let mut source = form(
        original.clone(),
        Action::Revise,
        send,
        &RuntimeKeymap::defaults(),
    )
    .unwrap();
    let rendered = render(&source, 100, 16);
    assert!(
        rendered.contains("workflow.meta") && rendered.contains("existing budget remains"),
        "{rendered}"
    );
    insta::assert_snapshot!("workflow_revision_source_form", rendered);
    source.handle_key_event(KeyCode::Enter.into());
    assert_eq!(
        messages.lock().unwrap().as_slice(),
        [WorkflowUiIntent::RevisionArguments {
            scope: original.scope.clone(),
            source: original.source.clone(),
            initial_input: original.input.clone()
        }]
    );
    let (send, messages) = channel();
    let mut args = revision_arguments(
        original.scope.clone(),
        original.source.clone(),
        serde_json::Value::Null,
        send,
        &RuntimeKeymap::defaults(),
    );
    args.handle_key_event(KeyCode::Enter.into());
    assert_eq!(
        messages.lock().unwrap().as_slice(),
        [WorkflowUiIntent::StageRevision {
            scope: original.scope,
            source: original.source,
            input: serde_json::Value::Null
        }]
    );
}

#[test]
fn revision_preview_separates_default_drain_from_interrupt_and_keeps_the_same_budget() {
    let original = snapshot();
    let (send, messages) = channel();
    let revision = RevisionPreview {
        id: "revision-2".into(),
        parent_id: Some("revision-1".into()),
        number: 2,
        execution_ids: vec!["execution-1".into(), "execution-2".into()],
        status: "Draft".into(),
        source: original.source.clone(),
        input: original.input.clone(),
        reusable_statements: 2,
        statement_count: 5,
        invalidation_reason: "Statement 3 changed; its suffix will run again".into(),
        error: None,
    };
    let params = preview(
        original.scope.clone(),
        revision,
        "2 / 8 starts · 14K / 100K tokens".into(),
        Ok(()),
        send,
    );
    assert!(
        !params.items[0].require_explicit_confirmation
            && params.items[1].require_explicit_confirmation
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let tx = AppEventSender::new(tx);
    (params.items[0].actions[0])(&tx);
    assert_eq!(
        messages.lock().unwrap().as_slice(),
        [WorkflowUiIntent::ApplyRevision {
            scope: original.scope,
            revision_id: "revision-2".into(),
            mode: RevisionApplyMode::DrainAccepted
        }]
    );
    (params.items[3].actions[0])(&tx);
    assert!(
        matches!(messages.lock().unwrap().last(),Some(WorkflowUiIntent::ViewExecution {revision_id,execution_id,..}) if revision_id=="revision-2" && execution_id=="execution-2")
    );
    let picker = ListSelectionView::new(params, tx, RuntimeKeymap::defaults().list);
    insta::assert_snapshot!("workflow_revision_apply_preview", render(&picker, 100, 22));
}

fn running_context() -> codex_app_server_protocol::WorkflowRun {
    serde_json::from_value(json!({
        "id":"run-1","executionId":"execution-1","ownerThreadId":codex_protocol::ThreadId::from_u128(7).to_string(),"readOnly":false,"formatVersion":2,
        "metadata":{"name":"audit","description":"Inspect source","phases":[]},"source":"return workflow.args;","input":{},"status":"running","phase":null,"phases":[],"createdAt":1,"updatedAt":2,
        "steps":{"opaque":{"id":"opaque","logicalId":"review","label":"Review source","options":{"message":"Inspect source"},"attempts":[{"id":"attempt-2","admission":"accepted","status":"running","childThreadId":codex_protocol::ThreadId::from_u128(42).to_string(),"ownedTurnIds":["owned-older","turn-3"]}]}}
    })).unwrap()
}

#[test]
fn active_guidance_uses_native_targets_and_never_guesses_the_last_owned_turn_or_retargets_history()
{
    use codex_app_server_protocol::WorkflowControlActiveTarget;
    use codex_app_server_protocol::WorkflowControlState;
    use codex_app_server_protocol::WorkflowRevisionStatus;
    let scope = snapshot().scope;
    let run = running_context();
    let state = WorkflowControlState {
        control_revision: 10,
        active_revision_id: "revision-1".into(),
        pending_revision_id: None,
        status: WorkflowRevisionStatus::Active,
    };
    let unavailable = context(scope.clone(), run.clone(), state.clone(), &[]).unwrap();
    assert!(
        unavailable
            .availability
            .iter()
            .find(|entry| entry.action == Action::Guide)
            .unwrap()
            .unavailable_reason
            .is_some()
    );
    let active = [WorkflowControlActiveTarget {
        logical_id: "review".into(),
        attempt_id: "attempt-2".into(),
        turn_id: "turn-3".into(),
    }];
    let hydrated = context(scope.clone(), run.clone(), state.clone(), &active).unwrap();
    assert_eq!(hydrated.scope.expected_control_revision, 10);
    assert!(
        hydrated
            .availability
            .iter()
            .find(|entry| entry.action == Action::Guide)
            .unwrap()
            .unavailable_reason
            .is_none()
    );
    let mut historical = scope;
    historical.target = ActionTarget::Step {
        logical_id: "review".into(),
        expected_attempt_id: Some("attempt-1".into()),
        expected_turn_id: None,
    };
    let old = context(historical, run, state, &active).unwrap();
    assert!(
        old.availability
            .iter()
            .find(|entry| entry.action == Action::Guide)
            .unwrap()
            .unavailable_reason
            .is_some()
    );
}

#[test]
fn queued_edits_keep_the_effective_prompt_and_refuse_an_older_attempt_target() {
    use codex_app_server_protocol::WorkflowAdmissionStatus;
    use codex_app_server_protocol::WorkflowControlState;
    use codex_app_server_protocol::WorkflowRevisionStatus;
    let mut run = running_context();
    let step = run.steps.get_mut("opaque").unwrap();
    step.effective_options = Some(json!({"message":"Use the edited queued prompt"}));
    let attempt = step.attempts.last_mut().unwrap();
    attempt.admission = WorkflowAdmissionStatus::Reserved;
    attempt.child_thread_id = None;
    let state = WorkflowControlState {
        control_revision: 10,
        active_revision_id: "revision-1".into(),
        pending_revision_id: None,
        status: WorkflowRevisionStatus::Active,
    };
    let available = context(snapshot().scope, run.clone(), state.clone(), &[]).unwrap();
    assert_eq!(
        available.queued_prompt.as_deref(),
        Some("Use the edited queued prompt")
    );
    assert!(
        available
            .availability
            .iter()
            .find(|entry| entry.action == Action::EditQueued)
            .unwrap()
            .unavailable_reason
            .is_none()
    );
    let mut old = snapshot().scope;
    old.target = ActionTarget::Step {
        logical_id: "review".into(),
        expected_attempt_id: Some("attempt-1".into()),
        expected_turn_id: None,
    };
    let unavailable = context(old, run, state, &[]).unwrap();
    for action in [Action::EditQueued, Action::Skip] {
        assert!(
            unavailable
                .availability
                .iter()
                .find(|entry| entry.action == action)
                .unwrap()
                .unavailable_reason
                .is_some()
        );
    }
}

#[test]
fn failed_skip_requires_settlement_and_resume_uses_only_the_ready_pending_revision() {
    use codex_app_server_protocol::WorkflowControlState;
    use codex_app_server_protocol::WorkflowRevisionStatus;
    use codex_app_server_protocol::WorkflowStatus;
    let mut run = running_context();
    run.status = WorkflowStatus::Failed;
    let attempt = run
        .steps
        .get_mut("opaque")
        .unwrap()
        .attempts
        .last_mut()
        .unwrap();
    attempt.status = WorkflowStatus::Failed;
    let state = WorkflowControlState {
        control_revision: 10,
        active_revision_id: "revision-1".into(),
        pending_revision_id: None,
        status: WorkflowRevisionStatus::Active,
    };
    let uncertain = context(snapshot().scope, run.clone(), state.clone(), &[]).unwrap();
    assert!(
        uncertain
            .availability
            .iter()
            .find(|entry| entry.action == Action::Skip)
            .unwrap()
            .unavailable_reason
            .is_some()
    );
    run.steps
        .get_mut("opaque")
        .unwrap()
        .attempts
        .last_mut()
        .unwrap()
        .final_turn_id = Some("turn-3".into());
    let settled = context(snapshot().scope, run.clone(), state.clone(), &[]).unwrap();
    assert!(
        settled
            .availability
            .iter()
            .find(|entry| entry.action == Action::Skip)
            .unwrap()
            .unavailable_reason
            .is_none()
    );
    let mut ready_state = state;
    ready_state.status = WorkflowRevisionStatus::Ready;
    ready_state.pending_revision_id = Some("revision-2".into());
    let ready = context(snapshot().scope, run.clone(), ready_state.clone(), &[]).unwrap();
    assert!(
        ready
            .availability
            .iter()
            .find(|entry| entry.action == Action::Resume)
            .unwrap()
            .unavailable_reason
            .is_none()
    );
    let (send, messages) = channel();
    let params = menu(ready, send);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let tx = AppEventSender::new(tx);
    (params.items[7].actions[0])(&tx);
    assert!(
        matches!(messages.lock().unwrap().as_slice(),[WorkflowUiIntent::Resume {revision_id,..}] if revision_id=="revision-2")
    );
    for status in [
        WorkflowRevisionStatus::Ready,
        WorkflowRevisionStatus::Blocked,
    ] {
        let mut state = ready_state.clone();
        state.status = status;
        let unavailable = context(snapshot().scope, run.clone(), state, &[]).unwrap();
        for action in [
            Action::Guide,
            Action::EditQueued,
            Action::Skip,
            Action::Interrupt,
            Action::ApplyDrain,
            Action::ApplyInterrupt,
        ] {
            assert!(
                unavailable
                    .availability
                    .iter()
                    .find(|entry| entry.action == action)
                    .unwrap()
                    .unavailable_reason
                    .is_some(),
                "{status:?}: {action:?}"
            );
        }
        for action in [Action::ShowResult, Action::Revisions] {
            assert!(
                unavailable
                    .availability
                    .iter()
                    .find(|entry| entry.action == action)
                    .unwrap()
                    .unavailable_reason
                    .is_none()
            );
        }
        assert_eq!(
            unavailable
                .availability
                .iter()
                .find(|entry| entry.action == Action::Resume)
                .unwrap()
                .unavailable_reason
                .is_none(),
            status == WorkflowRevisionStatus::Ready
        );
    }
}

#[test]
fn rejected_revision_form_keeps_the_original_scope_source_and_arguments() {
    let mut original = snapshot();
    original.input = json!({"keep":"original arguments"});
    let (send, messages) = channel();
    let mut view = form_with_error(
        original.clone(),
        Action::Revise,
        original.source.clone(),
        "Control revision changed; reopen Actions before submitting".into(),
        send,
        &RuntimeKeymap::defaults(),
    )
    .unwrap();
    let screen = render(&view, 100, 16);
    assert!(
        screen.contains("Control revision changed") && screen.contains("workflow.meta"),
        "{screen}"
    );
    view.handle_key_event(KeyCode::Enter.into());
    assert_eq!(
        messages.lock().unwrap().as_slice(),
        [WorkflowUiIntent::RevisionArguments {
            scope: original.scope,
            source: original.source,
            initial_input: original.input
        }]
    );
    insta::assert_snapshot!("workflow_revision_rejected_draft", screen);
}

#[test]
fn historical_inspection_keeps_explicit_execution_source_result_and_chat_separate_from_live_state()
{
    use codex_app_server_protocol::WorkflowCompletionMode;
    use codex_app_server_protocol::WorkflowExecutionSnapshot;
    use codex_app_server_protocol::WorkflowResult;
    use codex_app_server_protocol::WorkflowStatus;
    let original = snapshot();
    let shared = std::rc::Rc::new(Mutex::new(crate::workflows::WorkflowState {
        owner: Some(original.scope.owner),
        attached_thread: Some(original.scope.owner),
        ..Default::default()
    }));
    let execution = WorkflowExecutionSnapshot {
        run_id: "run-1".into(),
        execution_id: "historical-execution".into(),
        revision_id: Some("revision-1".into()),
        source: "return 'historical source';".into(),
        input: json!({"before":true}),
        metadata: None,
        completion_mode: WorkflowCompletionMode::Display,
        status: WorkflowStatus::Succeeded,
        partial: false,
        result: WorkflowResult::Value {
            value: json!({"result":"Historical result"}),
        },
        error: None,
        metrics: Default::default(),
        created_at: 1,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut view = InspectionView::execution(
        original.scope,
        execution,
        shared.clone(),
        AppEventSender::new(tx),
    );
    let overview = render(&view, 100, 20);
    assert!(overview.contains("Historical result"), "{overview}");
    view.handle_key_event(KeyCode::Char('3').into());
    let source = render(&view, 100, 20);
    assert!(source.contains("historical source"), "{source}");
    view.handle_key_event(KeyCode::Char('2').into());
    assert!(
        matches!(rx.try_recv().unwrap(),crate::app_event::AppEvent::Workflow(crate::workflows::WorkflowEvent::ReadTranscript {target,..}) if target.execution_id=="historical-execution")
    );
    view.handle_key_event(KeyCode::Char(' ').into());
    assert!(
        rx.try_recv().is_err(),
        "immutable inspection has no mutation controls"
    );
    assert!(
        shared.lock().unwrap().rows.is_empty(),
        "historical inspection never replaces the live center rows"
    );
    view.handle_key_event(KeyCode::Esc.into());
    assert!(view.is_complete());
    insta::assert_snapshot!("workflow_historical_execution_result", overview);
    insta::assert_snapshot!("workflow_historical_execution_source", source);
}
