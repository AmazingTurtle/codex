use super::session::Session;
use crate::ConversationExecutionState;
use codex_protocol::error::Result;
use futures::future::BoxFuture;
use std::collections::HashSet;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct ResumedPendingInput;
struct RestoredQueuedInput(std::sync::atomic::AtomicBool);

impl Session {
    pub(crate) async fn pause_execution(&self) -> Result<ConversationExecutionState> {
        self.services
            .local_agent_runtime
            .execution_control
            .pause_if_active()
            .await
    }

    pub(crate) async fn can_pause_execution(&self) -> Result<bool> {
        self.services
            .local_agent_runtime
            .execution_control
            .can_pause()
            .await
    }

    pub(crate) async fn unpause_execution(self: &Arc<Self>) -> Result<ConversationExecutionState> {
        let snapshot = self
            .services
            .local_agent_runtime
            .execution_control
            .unpause()
            .await?;
        for session in self
            .services
            .local_agent_runtime
            .execution_control
            .members()
        {
            session.maybe_start_turn_for_pending_work().await;
        }
        Ok(snapshot)
    }

    pub(crate) async fn include_resumed_input(
        &self,
        context: &super::turn_context::TurnContext,
        input: &mut Vec<super::TurnInput>,
    ) {
        let restored = self
            .services
            .thread_extension_data
            .get::<RestoredQueuedInput>()
            .is_some_and(|marker| marker.0.swap(false, std::sync::atomic::Ordering::AcqRel));
        if !restored
            && context
                .extension_data
                .get::<ResumedPendingInput>()
                .is_none()
        {
            return;
        }
        input.extend(
            self.input_queue
                .get_pending_input(&self.active_turn)
                .await
                .0,
        );
        input.sort_by_key(|input| match input {
            super::TurnInput::UserInput { metadata, .. } => {
                (1, metadata.acceptance_order.unwrap_or(u64::MAX))
            }
            _ => (0, 0),
        });
    }

    pub(crate) async fn execution_state(&self) -> Result<ConversationExecutionState> {
        self.services
            .local_agent_runtime
            .execution_control
            .snapshot()
            .await
    }

    pub(crate) async fn wait_until_running(&self, cancel: &CancellationToken) -> Result<()> {
        self.services
            .local_agent_runtime
            .execution_control
            .wait_until_running(cancel)
            .await
    }

    /// Pending inputs reserved orders before they entered the restored history.
    /// Advance the shared allocator before mail, tool results, or new guidance
    /// can reuse those orders during recovery.
    pub(crate) async fn restore_pending_input_order(&self) {
        if let Some(order) = self
            .services
            .local_agent_runtime
            .execution_control
            .max_pending_user_order(self.thread_id)
            .await
        {
            self.state
                .lock()
                .await
                .history
                .advance_input_order_past(order);
        }
    }

    pub(crate) async fn retain_accepted_input(&self, input: &mut [super::TurnInput]) -> Result<()> {
        let signal = crate::execution_control::QueuedSignal::new(self.thread_id, input.to_vec());
        input.clone_from_slice(&signal.input);
        self.services
            .local_agent_runtime
            .execution_control
            .retain_input(
                signal.clone(),
                crate::execution_control::SignalDelivery::Queued,
            )
            .await?;
        self.services
            .local_agent_runtime
            .execution_control
            .retain_input(signal, crate::execution_control::SignalDelivery::Owned)
            .await
            .map(drop)
    }

    pub(crate) async fn retain_turn_input(
        &self,
        input: &mut [super::TurnInput],
        context: &super::turn_context::TurnContext,
    ) -> Result<()> {
        let mut options = context.attribution().start_options();
        options.final_output_json_schema = context.final_output_json_schema.clone();
        options.service_tier = context.initial_settings.service_tier.clone();
        options.cyber_access_program = context.cyber_access_program;
        let mut signal =
            crate::execution_control::QueuedSignal::new(self.thread_id, input.to_vec())
                .on_start(options);
        if self
            .services
            .thread_extension_data
            .get::<crate::guardian::PendingReviewContext>()
            .is_some()
        {
            signal = signal.awaiting_finalization();
        }
        input.clone_from_slice(&signal.input);
        self.services
            .local_agent_runtime
            .execution_control
            .retain_input(signal, crate::execution_control::SignalDelivery::Owned)
            .await
            .map(drop)
    }

    pub(crate) async fn acknowledge_paused_signals(&self) -> Result<()> {
        let control = &self.services.local_agent_runtime.execution_control;
        let pending = control.pending_input_ids(self.thread_id).await;
        if pending.is_empty() {
            return Ok(());
        }
        let mut ids = HashSet::new();
        if let Some(thread) = self.live_thread() {
            thread
                .flush()
                .await
                .map_err(|error| codex_protocol::error::CodexErr::Fatal(error.to_string()))?;
            for batch in pending.chunks(codex_thread_store::MAX_PERSISTED_INPUT_IDS) {
                let found = self
                    .services
                    .thread_store
                    .find_persisted_input_ids(codex_thread_store::FindPersistedInputIdsParams {
                        thread_id: self.thread_id,
                        ids: batch.to_vec(),
                    })
                    .await
                    .map_err(|error| codex_protocol::error::CodexErr::Fatal(error.to_string()))?;
                ids.extend(found.into_iter().map(|id| match id {
                    codex_thread_store::PersistedInputId::ResponseItem(id) => id.to_string(),
                    codex_thread_store::PersistedInputId::UserInputOrder(order) => {
                        format!("user:{order}")
                    }
                }));
            }
        } else {
            for item in self.clone_history().await.annotated_items() {
                if let Some(id) = item.item.id() {
                    ids.insert(id.to_string());
                }
                if matches!(&item.item, codex_protocol::models::ResponseItem::Message { role, .. } if role == "user")
                    && let Some(order) = item
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.user_input_order)
                {
                    ids.insert(format!("user:{order}"));
                }
            }
        }
        control.acknowledge(self.thread_id, &ids).await
    }

    pub(crate) async fn discard_rejected_input(&self, input: &super::TurnInput) -> Result<()> {
        let id = input_delivery_id(input);
        if let Some(id) = id {
            self.services
                .local_agent_runtime
                .execution_control
                .acknowledge(self.thread_id, &HashSet::from([id]))
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn acknowledge_recorded_input(
        &self,
        input: &[super::TurnInput],
    ) -> Result<()> {
        if input.is_empty() {
            return Ok(());
        }
        let control = &self.services.local_agent_runtime.execution_control;
        if !control.has_signals(self.thread_id).await {
            return Ok(());
        }
        let ids = input
            .iter()
            .filter_map(input_delivery_id)
            .collect::<HashSet<_>>();
        if ids.is_empty() || control.acknowledge_transient(self.thread_id, &ids).await? {
            return Ok(());
        }
        self.flush_rollout()
            .await
            .map_err(|error| codex_protocol::error::CodexErr::Fatal(error.to_string()))?;
        control.acknowledge(self.thread_id, &ids).await
    }

    pub(crate) async fn discard_interrupted_execution_input(&self, generation: u64) {
        if !self.state.lock().await.shutting_down
            && let Err(error) = self
                .services
                .local_agent_runtime
                .execution_control
                .discard_interrupted_input(self.thread_id, generation)
                .await
        {
            tracing::error!(%error, "failed to discard interrupted durable input");
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "claim and transfer queued work under the same admission and active-turn locks"
    )]
    pub(crate) fn drain_paused_signals(self: &Arc<Self>) -> BoxFuture<'static, Result<()>> {
        let session = Arc::clone(self);
        Box::pin(async move {
            if !session
                .services
                .local_agent_runtime
                .execution_control
                .has_signals(session.thread_id)
                .await
            {
                return Ok(());
            }
            session.restore_pending_input_order().await;
            session.acknowledge_paused_signals().await?;
            let _admission = session.turn_start_lock.lock().await;
            let input = session
                .services
                .local_agent_runtime
                .execution_control
                .take_signals(session.thread_id)
                .await?;
            if input.is_empty() {
                return Ok(());
            }
            if let Some(task) = session
                .active_turn
                .lock()
                .await
                .as_ref()
                .and_then(|turn| turn.task.as_ref())
            {
                task.turn_context.extension_data.insert(ResumedPendingInput);
            } else {
                session
                    .services
                    .thread_extension_data
                    .get_or_init(|| RestoredQueuedInput(std::sync::atomic::AtomicBool::new(true)))
                    .0
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            let mut regular_input = Vec::new();
            let mut regular_start = None;
            for signal in input {
                for input in signal.input {
                    match input {
                        super::TurnInput::InterAgentCommunication(mail) => {
                            session
                                .input_queue
                                .enqueue_mailbox_communication(mail, signal.start_options.clone())
                                .await;
                        }
                        input => {
                            regular_start.get_or_insert_with(|| signal.start_options.clone());
                            regular_input.push(input);
                        }
                    }
                }
            }
            if regular_input.is_empty() {
                return Ok(());
            }
            let mut active = session.active_turn.lock().await;
            if let Some(turn) = active.as_mut() {
                session
                    .input_queue
                    .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                        turn.turn_state.as_ref(),
                        regular_input,
                    )
                    .await;
            } else {
                *active = Some(crate::state::ActiveTurn::default());
                drop(active);
                let options = regular_start.unwrap_or_default();
                let (context, _) = session
                    .new_turn_with_sub_id(
                        super::new_submission_id(),
                        super::session::SessionSettingsUpdate {
                            service_tier_for_turn: options.service_tier,
                            ..Default::default()
                        },
                        super::turn_context::NewTurnContextOptions {
                            final_output_json_schema: options.final_output_json_schema,
                            cyber_access_program: options.cyber_access_program,
                        },
                    )
                    .await?;
                if let Some(trigger) = options.turn_trigger {
                    context.turn_metadata_state.set_turn_trigger(trigger);
                }
                if let Some(parent) = options.parent_turn_id {
                    context.turn_metadata_state.set_parent_turn_id(parent);
                }
                if let Some(root) = options.root_turn_id {
                    context.turn_metadata_state.set_root_turn_id(root);
                }
                if let Some(path) = options.initiating_agent_path {
                    context.turn_metadata_state.set_initiating_agent_path(path);
                }
                session
                    .start_task(context, regular_input, crate::tasks::RegularTask::new())
                    .await;
            }
            Ok(())
        })
    }
}

fn input_delivery_id(input: &super::TurnInput) -> Option<String> {
    match input {
        super::TurnInput::UserInput { metadata, .. } => metadata
            .acceptance_order
            .map(|order| format!("user:{order}")),
        super::TurnInput::FunctionCallOutput(item) | super::TurnInput::ResponseItem(item) => {
            item.item.id().map(ToString::to_string)
        }
        super::TurnInput::InterAgentCommunication(mail) => {
            mail.id.as_ref().map(ToString::to_string)
        }
    }
}
