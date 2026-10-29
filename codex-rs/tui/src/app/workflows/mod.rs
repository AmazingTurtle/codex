//! App integration for conversation workflows, persisted policy and root execution control.
mod control;
mod execution;
mod overview;
mod policy;
mod save;
#[path = "saved/catalog.rs"]
mod saved_catalog;
#[path = "saved/route.rs"]
mod saved_route;
#[path = "saved/source.rs"]
mod saved_source;
mod scope;
mod transcript;
mod updates;
use super::*;
use crate::app_server_session::workflows as transport;
use crate::workflows::SavedWorkflowRow;
use crate::workflows::WorkflowAction;
use crate::workflows::WorkflowCenter;
use crate::workflows::WorkflowEvent;

impl App {
    pub(super) fn discover_workflows_after_attach(&mut self, app_server: &AppServerSession) {
        self.sync_workflow_presentation();
        {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.loading = false;
            state.request_id = None;
            state.execution_notice_state = None;
            state.action_context_id = None;
            state.action_snapshot = None;
            state.action_pending = false;
            state.action_refresh_pending = false;
            state.overview_request = None;
            state.overview_dirty = false;
        }
        self.refresh_workflows(app_server);
        self.refresh_saved_workflow_catalog(app_server);
    }

    fn workflow_attachment_thread_id(&self) -> Option<ThreadId> {
        if self.active_side_parent_thread_id().is_some() {
            self.current_displayed_thread_id()
        } else {
            self.primary_thread_id
                .or(self.current_displayed_thread_id())
        }
    }

    pub(super) fn sync_workflow_presentation(&mut self) {
        self.chat_widget.set_workflow_state(self.workflows.clone());
        let owner = self.workflow_attachment_thread_id();
        let displayed = self.current_displayed_thread_id();
        let mut state = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.attached_thread != owner {
            if let Some(cancel) = state.catalog_cancel.take() {
                cancel.cancel();
            }
            let scope = state.scope;
            *state = crate::workflows::WorkflowState {
                owner,
                attached_thread: owner,
                scope,
                ..Default::default()
            };
        }
        if state.inspected_agent != displayed {
            state.inspected_agent = None;
        }
        if state.displayed_thread != displayed {
            state.action_context_id = None;
            state.action_snapshot = None;
            state.action_pending = false;
            state.action_refresh_pending = false;
            state.overview_request = None;
            state.overview_dirty = false;
        }
        state.displayed_thread = displayed;
        if (!state.rows.is_empty() || self.chat_widget.active_view_is(crate::workflows::CENTER_ID))
            && !state.loading
            && state
                .refresh_at
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
        {
            state.refresh_at = Some(Instant::now());
            self.app_event_tx
                .send(AppEvent::Workflow(WorkflowEvent::Refresh));
        }
        let execution = state.execution;
        let can_pause = state.can_pause;
        drop(state);
        self.chat_widget.set_execution_control(execution, can_pause);
    }

    pub(super) fn refresh_workflows(&mut self, app_server: &AppServerSession) {
        let owner = self.workflow_attachment_thread_id();
        let Some(owner) = owner else {
            return;
        };
        let request_id = Uuid::new_v4();
        {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.loading {
                return;
            }
            state.owner.get_or_insert(owner);
            state.attached_thread = Some(owner);
            state.loading = true;
            state.request_id = Some(request_id);
            state.refresh_at = Some(Instant::now());
        }
        let handle = app_server.request_handle();
        let library_watch = app_server.workflow_library_watch();
        let tx = self.app_event_tx.clone();
        let requested_scope = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .scope;
        let (cursor, saved_cursor) = {
            let state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (state.page.cursor.clone(), state.saved_page.cursor.clone())
        };
        let scope = match requested_scope {
            crate::workflows::WorkflowListScope::Current => {
                codex_app_server_protocol::WorkflowListScope::Current
            }
            crate::workflows::WorkflowListScope::Project => {
                codex_app_server_protocol::WorkflowListScope::Project
            }
            crate::workflows::WorkflowListScope::All => {
                codex_app_server_protocol::WorkflowListScope::All
            }
        };
        tokio::spawn(async move {
            let result = async {
                let control = transport::execution_control(&handle, owner).await?;
                let root = ThreadId::from_string(&control.root_thread_id)
                    .map_err(|error| error.to_string())?;
                tx.send(AppEvent::Workflow(WorkflowEvent::ExecutionSnapshot {
                    request_id,
                    snapshot:
                        codex_app_server_protocol::ThreadExecutionControlChangedNotification {
                            thread_id: owner.to_string(),
                            execution_control: control,
                        },
                }));
                transport::list(&handle, root, scope, cursor, saved_cursor, &library_watch)
                    .await
                    .map(|snapshot| {
                        let mut runs: HashMap<_, _> = snapshot
                            .runs
                            .into_iter()
                            .map(|run| (run.id.clone(), run))
                            .collect();
                        crate::workflows::WorkflowPage {
                            root,
                            rows: snapshot
                                .summaries
                                .into_iter()
                                .map(|summary| {
                                    runs.remove(&summary.id).map_or_else(
                                        || crate::workflows::summary(summary),
                                        crate::workflows::project,
                                    )
                                })
                                .collect(),
                            saved: snapshot
                                .saved
                                .into_iter()
                                .map(|row| SavedWorkflowRow {
                                    id: row.id,
                                    name: row.name,
                                    description: row
                                        .metadata
                                        .as_ref()
                                        .map(|meta| meta.description.clone()),
                                    scope: Some(row.scope),
                                    metadata: row.metadata,
                                    read_only: row.read_only,
                                    read_only_reason: row.read_only_reason,
                                    command_notice: None,
                                })
                                .collect(),
                            next_cursor: snapshot.next_cursor,
                            saved_next_cursor: snapshot.saved_next_cursor,
                        }
                    })
            }
            .await;
            tx.send(AppEvent::Workflow(WorkflowEvent::Loaded {
                owner,
                request_id,
                scope: requested_scope,
                result,
            }));
        });
    }

    pub(super) async fn handle_workflow_event(
        &mut self,
        app_server: &mut AppServerSession,
        event: WorkflowEvent,
    ) {
        match event {
            WorkflowEvent::OverviewLoaded { target, result } => {
                self.finish_workflow_overview(app_server, target, result)
            }
            WorkflowEvent::ActionsOpen {
                owner,
                caller,
                run_id,
                execution_id,
                target,
            } => {
                self.open_workflow_actions(app_server, owner, caller, run_id, execution_id, target)
            }
            WorkflowEvent::ControlIntent(intent) => {
                self.workflow_control_intent(app_server, *intent)
            }
            WorkflowEvent::ControlResult { scope, outcome } => {
                self.finish_workflow_control(app_server, scope, *outcome)
            }
            WorkflowEvent::RefreshSavedCatalog => self.refresh_saved_workflow_catalog(app_server),
            WorkflowEvent::SavedCatalogLoaded {
                owner,
                request_id,
                result,
            } => self.finish_saved_workflow_catalog(owner, request_id, result),
            WorkflowEvent::ReadSavedSource { id, refresh } => {
                self.read_saved_workflow_source(app_server, id, refresh)
            }
            WorkflowEvent::SavedSourceLoaded {
                id,
                request_id,
                result,
            } => self.finish_saved_workflow_source(id, request_id, result),
            WorkflowEvent::Open => {
                self.sync_workflow_presentation();
                self.workflows
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .inspected_agent = None;
                self.chat_widget.show_bottom_pane_view(Box::new(
                    WorkflowCenter::new_with_keymap(
                        self.workflows.clone(),
                        self.app_event_tx.clone(),
                        self.keymap.list.clone(),
                    )
                    .with_runtime_keymap(self.keymap.clone()),
                ));
                self.refresh_workflows(app_server);
                self.refresh_visible_workflow_transcript(app_server);
            }
            WorkflowEvent::ChooseMode => self.open_workflow_policy(),
            WorkflowEvent::SetMode(mode) => self.persist_workflow_policy(app_server, mode).await,
            WorkflowEvent::Refresh => self.refresh_workflows(app_server),
            WorkflowEvent::ChooseScope => self.choose_workflow_scope(),
            WorkflowEvent::SetScope(scope) => {
                {
                    let mut state = self
                        .workflows
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.scope = scope;
                    state.page = Default::default();
                    state.refresh_pending = state.loading;
                }
                self.refresh_workflows(app_server);
            }
            WorkflowEvent::NextPage { saved } | WorkflowEvent::PreviousPage { saved } => {
                let forward = matches!(event, WorkflowEvent::NextPage { .. });
                {
                    let mut state = self
                        .workflows
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.loading {
                        return;
                    }
                    let page = if saved {
                        &mut state.saved_page
                    } else {
                        &mut state.page
                    };
                    if forward {
                        if let Some(next) = page.next.take() {
                            page.previous.push(page.cursor.take());
                            page.cursor = Some(next);
                        } else {
                            return;
                        }
                    } else if let Some(previous) = page.previous.pop() {
                        page.cursor = previous;
                    } else {
                        return;
                    }
                    state.child = None;
                    state.selected = 0;
                }
                self.refresh_workflows(app_server);
            }
            WorkflowEvent::OpenOwner(owner) => {
                self.app_event_tx
                    .send(AppEvent::SelectAgentsOverviewThread { thread_id: owner });
                self.app_event_tx
                    .send(AppEvent::Workflow(WorkflowEvent::Open));
            }
            WorkflowEvent::Loaded {
                owner,
                request_id,
                scope,
                result,
            } => {
                let mut source_to_read = None;
                let mut state = self
                    .workflows
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.attached_thread == Some(owner) && state.request_id == Some(request_id) {
                    state.loading = false;
                    match result {
                        Ok(page) if state.scope == scope => {
                            state.owner = Some(page.root);
                            state.merge(page.rows);
                            state.saved = page.saved;
                            state.reconcile_saved_selection();
                            state.page.next = page.next_cursor;
                            state.saved_page.next = page.saved_next_cursor;
                            state.load_error = None;
                            if state.center.saved
                                && state.center.saved_detail_tab
                                    == crate::workflows::DetailTab::Source
                            {
                                source_to_read = state.center.saved_id.clone();
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            state.load_error = Some(format!("Could not load workflows: {error}"))
                        }
                    }
                    if std::mem::take(&mut state.refresh_pending) {
                        self.app_event_tx
                            .send(AppEvent::Workflow(WorkflowEvent::Refresh));
                    }
                }
                drop(state);
                if let Some(id) = source_to_read {
                    self.app_event_tx
                        .send(AppEvent::Workflow(WorkflowEvent::ReadSavedSource {
                            id,
                            refresh: false,
                        }));
                }
            }
            WorkflowEvent::OperationFinished {
                owner,
                request_id,
                result,
            } => {
                let succeeded = result.is_ok();
                let error = result.as_ref().err().cloned();
                {
                    let mut state = self
                        .workflows
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.owner != Some(owner) || state.operation_id != Some(request_id) {
                        return;
                    }
                    state.operation_pending = false;
                    if let Err(error) = &result {
                        state.error = Some(error.clone());
                    }
                    state.operation_result = Some(result);
                }
                if let Some(error) = error {
                    self.chat_widget
                        .add_error_message(format!("Could not run saved workflow: {error}"));
                }
                if succeeded {
                    self.refresh_workflows(app_server);
                }
            }
            WorkflowEvent::Action { owner, id, action } => {
                if matches!(action, WorkflowAction::Save) {
                    self.open_save_workflow(owner, id);
                } else {
                    match transport::action(&app_server.request_handle(), owner, id, action).await {
                        Err(error) => {
                            self.workflows
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .error = Some(error)
                        }
                        Ok(()) => {
                            self.workflows
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .error = None;
                            self.app_event_tx
                                .send(AppEvent::Workflow(WorkflowEvent::Refresh));
                        }
                    }
                }
            }
            WorkflowEvent::StartSaved {
                owner,
                caller,
                id,
                input,
                request_id,
            } => {
                self.start_saved_workflow(app_server, owner, caller, id, input, request_id);
            }
            WorkflowEvent::ChooseSaveScope { owner, id, name } => {
                self.choose_workflow_save_scope(owner, id, name)
            }
            WorkflowEvent::SaveNamed {
                owner,
                id,
                name,
                scope,
            } => {
                let retry_id = id.clone();
                let retry_name = name.clone();
                if let Err(error) =
                    transport::save(&app_server.request_handle(), owner, id, name, scope).await
                {
                    self.edit_workflow_name(
                        owner,
                        retry_id,
                        retry_name,
                        Some(format!("Could not save workflow: {error}")),
                    );
                } else {
                    self.refresh_workflows(app_server);
                    self.refresh_saved_workflow_catalog(app_server);
                }
            }
            WorkflowEvent::SelectAgent(thread_id) => {
                self.app_event_tx
                    .send(AppEvent::SelectAgentThread(thread_id));
                self.app_event_tx
                    .send(AppEvent::Workflow(WorkflowEvent::AgentOpened(thread_id)));
            }
            WorkflowEvent::AgentOpened(thread_id) => {
                if self.current_displayed_thread_id() == Some(thread_id) {
                    let mut state = self
                        .workflows
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.inspected_agent = (state.owner != Some(thread_id)).then_some(thread_id);
                }
            }
            WorkflowEvent::Pause => {
                if let Some(thread_id) = self.current_displayed_thread_id() {
                    self.request_execution_control(app_server, thread_id, /*pause*/ true)
                        .await;
                }
            }
            WorkflowEvent::Unpause => {
                if let Some(thread_id) = self.current_displayed_thread_id() {
                    self.request_execution_control(app_server, thread_id, /*pause*/ false)
                        .await;
                }
            }
            WorkflowEvent::ExecutionSnapshot {
                request_id,
                snapshot,
            } => {
                if self
                    .workflows
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .request_id
                    == Some(request_id)
                {
                    self.apply_execution_snapshot(snapshot);
                }
            }
            WorkflowEvent::ConfirmExecution { root, pause } => {
                self.set_confirmed_execution(app_server, root, pause).await;
            }
            WorkflowEvent::ReadTranscript { target, older } => {
                self.read_workflow_transcript(app_server, target, older)
            }
            WorkflowEvent::TranscriptLoaded {
                target,
                request_id,
                result,
            } => self.finish_workflow_transcript(target, request_id, result),
        }
    }
}
