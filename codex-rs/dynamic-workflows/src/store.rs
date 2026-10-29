//! V2 manifests reference independent attempt records; V1 snapshots remain read-only.
mod display;
mod entity_metrics;
mod entity_observations;
mod executions;
mod files;
mod metrics;
mod steering_files;
use crate::WorkflowRun;
use crate::WorkflowStatus;
use crate::WorkflowSummary;
use crate::WorkflowTokenUsage;
use crate::types::timestamp;
use codex_utils_absolute_path::AbsolutePathBuf;
pub use executions::execution_transcript_limitation;
pub use executions::list_execution_steps;
pub use executions::read_execution_attempt;
pub use executions::read_execution_snapshot;
use files::Loaded;
use metrics::UsageObservation;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);
struct CachedRun {
    run: WorkflowRun,
    durable: Loaded,
    usage: BTreeMap<String, UsageObservation>,
    last_checkpoint: Instant,
    last_event: Instant,
}
#[derive(Default)]
struct State {
    active: HashMap<String, CachedRun>,
}
pub(crate) struct Store {
    root: AbsolutePathBuf,
    state: Mutex<State>,
    pub(crate) events: tokio::sync::broadcast::Sender<WorkflowRun>,
    pub(crate) progress: tokio::sync::broadcast::Sender<WorkflowSummary>,
    pub(crate) lifecycle_events: tokio::sync::broadcast::Sender<crate::WorkflowLifecycleEvent>,
}

impl Store {
    pub(crate) fn read_execution_snapshot(
        &self,
        id: &str,
        execution: &str,
    ) -> Result<crate::WorkflowExecutionSnapshot, String> {
        read_execution_snapshot(&self.root, id, execution)
    }
    pub(crate) fn new(root: AbsolutePathBuf) -> Result<Self, String> {
        let (events, _) = tokio::sync::broadcast::channel(/*capacity*/ 8);
        let (progress, _) = tokio::sync::broadcast::channel(/*capacity*/ 32);
        let (lifecycle_events, _) = tokio::sync::broadcast::channel(/*capacity*/ 32);
        let store = Self {
            root,
            state: Mutex::new(State::default()),
            events,
            progress,
            lifecycle_events,
        };
        for id in ids(&store.root)? {
            let (header, _) = files::header(&store.root, &id)?;
            files::cleanup_on_open(&store.root, &id)?;
            if header.format_version == crate::WORKFLOW_FORMAT_VERSION
                && (matches!(
                    header.status,
                    WorkflowStatus::Pending | WorkflowStatus::Running
                ) || (header.lifecycle_version == 1
                    && header.status == WorkflowStatus::Interrupted
                    && !header.lifecycle.terminal_ready))
            {
                store.interrupt(&id)?;
            }
            if matches!(
                header.steering.state.status,
                crate::WorkflowRevisionStatus::Fencing | crate::WorkflowRevisionStatus::Draining
            ) {
                store.update(&id, |run| {
                    interrupted_revision(run);
                    Ok(())
                })?;
            }
        }
        Ok(store)
    }

    pub(crate) fn get(&self, id: &str) -> Result<WorkflowRun, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        let mut run = match state.active.get(id) {
            Some(cached) => cached.run.clone(),
            None => files::read(&self.root, id)?.run,
        };
        if run.format_version == crate::WORKFLOW_FORMAT_VERSION
            && matches!(
                run.status,
                WorkflowStatus::Pending | WorkflowStatus::Running
            )
            && let Some(cached) = state.active.get(id)
        {
            metrics::refresh(&mut run, &cached.usage, metrics::now_ms());
        }
        Ok(run)
    }

    pub(crate) fn insert(&self, run: &mut WorkflowRun) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        if files::path(&self.root, &run.id)?.exists() {
            return Err("workflow already exists".into());
        }
        if run.format_version != crate::WORKFLOW_FORMAT_VERSION {
            return Err("legacy workflows are read-only".into());
        }
        let latest = ids(&self.root)?
            .into_iter()
            .map(|id| files::header(&self.root, &id).map(|(run, _)| run.worker_sequence))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max()
            .unwrap_or_default();
        run.worker_sequence = latest
            .checked_add(1)
            .ok_or("workflow worker sequence exhausted")?;
        let usage = BTreeMap::new();
        metrics::refresh(run, &usage, metrics::now_ms());
        let manifest = files::write(&self.root, run, None, &usage)?;
        state.active.insert(
            run.id.clone(),
            CachedRun {
                run: run.clone(),
                durable: Loaded {
                    run: run.clone(),
                    manifest: Some(manifest),
                    usage: usage.clone(),
                },
                usage,
                last_checkpoint: Instant::now(),
                last_event: Instant::now(),
            },
        );
        let _ = self.events.send(run.clone());
        Ok(())
    }

    pub(crate) fn update<T>(
        &self,
        id: &str,
        edit: impl FnOnce(&mut WorkflowRun) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cached = self.cached(&mut state, id)?;
        let mut next = cached.run.clone();
        let result = edit(&mut next)?;
        next.updated_at = timestamp();
        if !matches!(
            next.status,
            WorkflowStatus::Pending | WorkflowStatus::Running
        ) {
            metrics::set_active(&mut next, false, metrics::now_ms());
            if matches!(
                cached.run.status,
                WorkflowStatus::Pending | WorkflowStatus::Running
            ) {
                next.metrics.wall_duration_ms = None;
            }
        }
        metrics::refresh(&mut next, &cached.usage, metrics::now_ms());
        let updated_at = next.updated_at;
        entity_metrics::stamp(
            &cached.run,
            &mut next,
            updated_at,
            entity_metrics::ChangeSource::Transition,
        );
        metrics::refresh(&mut next, &cached.usage, metrics::now_ms());
        crate::lifecycle::record_transitions(&cached.run, &mut next)?;
        let previous = std::mem::replace(&mut cached.run, next);
        if let Err(error) = self.commit(cached) {
            cached.run = previous;
            return Err(error);
        }
        for event in cached
            .run
            .lifecycle
            .events
            .iter()
            .skip(previous.lifecycle.events.len())
        {
            let _ = self.lifecycle_events.send(event.clone());
        }
        if !matches!(
            cached.run.status,
            WorkflowStatus::Pending | WorkflowStatus::Running
        ) {
            state.active.remove(id);
        }
        Ok(result)
    }

    pub(crate) fn interrupt(&self, id: &str) -> Result<(), String> {
        let run = self.get(id)?;
        if run.format_version != crate::WORKFLOW_FORMAT_VERSION {
            return Ok(());
        }
        if matches!(
            run.status,
            WorkflowStatus::Pending | WorkflowStatus::Running
        ) || (run.lifecycle_version == 1
            && run.status == WorkflowStatus::Interrupted
            && !run.lifecycle.terminal_ready)
        {
            self.update(id, |run| {
                if let Some(checkpoint) = run.activity.checkpoint_at_ms {
                    metrics::set_active(run, false, checkpoint);
                }
                run.status = WorkflowStatus::Interrupted;
                interrupted_revision(run);
                run.error.get_or_insert_with(|| {
                    "workflow interrupted; explicit recovery required".into()
                });
                if run.lifecycle_version == 1 {
                    run.lifecycle.terminal_ready = true;
                    run.lifecycle.interrupted_unsettled = true;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    pub(crate) fn acknowledge_notification(
        &self,
        id: &str,
        execution_id: &str,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cached = self.cached(&mut state, id)?;
        if cached.run.execution_id == execution_id && !cached.run.notification_delivered {
            let previous = cached.run.clone();
            cached.run.notification_delivered = true;
            if let Err(error) = self.commit_receipt(cached) {
                cached.run = previous;
                return Err(error);
            }
        }
        if !matches!(
            cached.run.status,
            WorkflowStatus::Pending | WorkflowStatus::Running
        ) {
            state.active.remove(id);
        }
        Ok(())
    }

    pub(crate) fn acknowledge_lifecycle_event(
        &self,
        id: &str,
        event_id: &str,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cached = self.cached(&mut state, id)?;
        if !cached
            .run
            .lifecycle
            .events
            .iter()
            .any(|event| event.id == event_id)
        {
            return Err("workflow lifecycle event not found".into());
        }
        if !cached.run.lifecycle.delivered.contains(event_id) {
            let previous = cached.run.clone();
            cached.run.lifecycle.delivered.insert(event_id.to_string());
            if let Err(error) = self.commit_receipt(cached) {
                cached.run = previous;
                return Err(error);
            }
        }
        if !matches!(
            cached.run.status,
            WorkflowStatus::Pending | WorkflowStatus::Running
        ) {
            state.active.remove(id);
        }
        Ok(())
    }

    pub(crate) fn record_usage(
        &self,
        id: &str,
        attempt: &str,
        thread: &str,
        usage: WorkflowTokenUsage,
    ) -> Result<(), String> {
        self.observe(id, attempt, thread, Some(usage), false)
    }
    pub(crate) fn complete_usage(
        &self,
        id: &str,
        attempt: &str,
        thread: &str,
    ) -> Result<(), String> {
        self.observe(id, attempt, thread, None, true)
    }
    fn observe(
        &self,
        id: &str,
        attempt: &str,
        thread: &str,
        usage: Option<WorkflowTokenUsage>,
        complete: bool,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cached = self.cached(&mut state, id)?;
        let previous = cached.run.clone();
        if !cached.usage.contains_key(thread) && cached.usage.len() >= crate::MAX_OBSERVED_THREADS {
            let mut changed = cached.run.metrics.usage_incomplete_reason.is_none();
            let reason = cached
                .run
                .metrics
                .usage_incomplete_reason
                .get_or_insert_with(|| {
                    "Workflow usage contributor limit exceeded (16384 threads).".into()
                })
                .clone();
            if let Some(attempt) = cached
                .run
                .steps
                .values_mut()
                .flat_map(|step| &mut step.attempts)
                .find(|candidate| candidate.id == attempt)
                && attempt.metrics.usage_incomplete_reason.is_none()
            {
                attempt.metrics.usage_incomplete_reason = Some(reason);
                changed = true;
            }
            if !changed {
                return Ok(());
            }
            return self.commit_progress(cached);
        }
        metrics::observe(
            &cached.run,
            &mut cached.usage,
            attempt,
            thread,
            usage,
            complete,
        )?;
        metrics::refresh(&mut cached.run, &cached.usage, metrics::now_ms());
        entity_metrics::stamp(
            &previous,
            &mut cached.run,
            timestamp(),
            entity_metrics::ChangeSource::Observation,
        );
        let terminal = !matches!(
            cached.run.status,
            WorkflowStatus::Pending | WorkflowStatus::Running
        );
        if terminal || complete || cached.last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
            self.commit_progress(cached)?;
        } else if cached.last_event.elapsed() >= Duration::from_millis(100) {
            let _ = self
                .progress
                .send(summary(&cached.run, cached.run.steps.len() as u32));
            cached.last_event = Instant::now();
        }
        if terminal {
            state.active.remove(id);
        }
        Ok(())
    }
    pub(crate) fn mark_usage_incomplete(&self, id: &str, reason: String) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cached = self.cached(&mut state, id)?;
        // Derive each attempt's current certificate before recording a run-wide gap.
        // A sibling's interruption must not revoke an independently settled worker.
        metrics::refresh(&mut cached.run, &cached.usage, metrics::now_ms());
        let reason = reason.chars().take(/*n*/ 512).collect::<String>();
        let mut changed = cached.run.metrics.usage_incomplete_reason.is_none();
        cached
            .run
            .metrics
            .usage_incomplete_reason
            .get_or_insert_with(|| reason.clone());
        for attempt in cached
            .run
            .steps
            .values_mut()
            .flat_map(|step| &mut step.attempts)
            .filter(|attempt| {
                attempt.admission == crate::AdmissionStatus::Accepted
                    && !attempt.metrics.usage_complete
                    && attempt.metrics.usage_incomplete_reason.is_none()
            })
        {
            attempt.metrics.usage_incomplete_reason = Some(reason.clone());
            changed = true;
        }
        if !changed {
            return Ok(());
        }
        self.commit_progress(cached)
    }

    pub(crate) fn set_execution_active(
        &self,
        id: &str,
        active: bool,
        at_ms: i64,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cached = self.cached(&mut state, id)?;
        if !matches!(
            cached.run.status,
            WorkflowStatus::Pending | WorkflowStatus::Running
        ) {
            state.active.remove(id);
            return Ok(());
        }
        metrics::set_active(&mut cached.run, active, at_ms);
        self.commit_progress(cached)
    }
    pub(crate) fn record_clock(
        &self,
        id: &str,
        execution_id: &str,
        elapsed_ms: u64,
        at_ms: i64,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cached = self.cached(&mut state, id)?;
        if cached.run.execution_id != execution_id {
            return Ok(());
        }
        let activity = &mut cached.run.activity;
        activity.active_duration_ms = activity
            .active_duration_ms
            .saturating_add(elapsed_ms.saturating_sub(activity.execution_duration_ms));
        activity.execution_duration_ms = activity.execution_duration_ms.max(elapsed_ms);
        activity.active_since_ms = None;
        activity.checkpoint_at_ms = Some(at_ms);
        metrics::refresh(&mut cached.run, &cached.usage, at_ms);
        let terminal = !matches!(
            cached.run.status,
            WorkflowStatus::Pending | WorkflowStatus::Running
        );
        if terminal || cached.last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
            self.commit_progress(cached)?;
        } else if cached.last_event.elapsed() >= Duration::from_millis(100) {
            let _ = self
                .progress
                .send(summary(&cached.run, cached.run.steps.len() as u32));
            cached.last_event = Instant::now();
        }
        if terminal {
            state.active.remove(id);
        }
        Ok(())
    }
    pub(crate) fn checkpoint(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cached = self.cached(&mut state, id)?;
        self.commit_progress(cached)
    }

    fn cached<'a>(&self, state: &'a mut State, id: &str) -> Result<&'a mut CachedRun, String> {
        if !state.active.contains_key(id) {
            let durable = files::read(&self.root, id)?;
            if durable.manifest.is_none()
                || durable.run.format_version != crate::WORKFLOW_FORMAT_VERSION
            {
                return Err("legacy workflows are read-only".into());
            }
            state.active.insert(
                id.to_string(),
                CachedRun {
                    run: durable.run.clone(),
                    usage: durable.usage.clone(),
                    durable,
                    last_checkpoint: Instant::now(),
                    last_event: Instant::now(),
                },
            );
        }
        state
            .active
            .get_mut(id)
            .ok_or("workflow cache entry missing".into())
    }

    fn commit(&self, cached: &mut CachedRun) -> Result<(), String> {
        self.persist(cached)?;
        let _ = self.events.send(cached.run.clone());
        Ok(())
    }
    /// Delivery bookkeeping must not advance workflow clocks or public activity timestamps.
    fn commit_receipt(&self, cached: &mut CachedRun) -> Result<(), String> {
        let manifest = files::write(
            &self.root,
            &cached.run,
            Some(&cached.durable),
            &cached.usage,
        )?;
        cached.durable = Loaded {
            run: cached.run.clone(),
            manifest: Some(manifest),
            usage: cached.usage.clone(),
        };
        // Retain the ordinary settlement wakeup for subscribers waiting for all receipts.
        let _ = self.events.send(cached.run.clone());
        Ok(())
    }
    fn commit_progress(&self, cached: &mut CachedRun) -> Result<(), String> {
        self.persist(cached)?;
        let _ = self
            .progress
            .send(summary(&cached.run, cached.run.steps.len() as u32));
        Ok(())
    }
    fn persist(&self, cached: &mut CachedRun) -> Result<(), String> {
        metrics::checkpoint_clock(&mut cached.run, metrics::now_ms());
        metrics::refresh(&mut cached.run, &cached.usage, metrics::now_ms());
        let manifest = files::write(
            &self.root,
            &cached.run,
            Some(&cached.durable),
            &cached.usage,
        )?;
        cached.durable = Loaded {
            run: cached.run.clone(),
            manifest: Some(manifest),
            usage: cached.usage.clone(),
        };
        cached.last_checkpoint = Instant::now();
        cached.last_event = Instant::now();
        Ok(())
    }

    pub(crate) fn list(
        &self,
        owner: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowRun>, String> {
        self.list_summaries(owner, offset, limit)?
            .into_iter()
            .map(|summary| self.get(&summary.id))
            .collect()
    }
    pub(crate) fn list_summaries(
        &self,
        owner: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowSummary>, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        let mut summaries = Vec::new();
        for id in ids(&self.root)? {
            let row = if let Some(cached) = state.active.get(&id) {
                summary(&cached.run, cached.run.steps.len() as u32)
            } else {
                let (header, count) = files::header(&self.root, &id)?;
                summary(&header, count)
            };
            if row.owner_thread_id == owner {
                summaries.push(row);
            }
        }
        Ok(page(summaries, offset, limit))
    }
}

fn ids(root: &AbsolutePathBuf) -> Result<Vec<String>, String> {
    let directory = match fs::read_dir(root) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut ids = Vec::new();
    for entry in directory {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let id = path
                .file_stem()
                .and_then(|id| id.to_str())
                .ok_or("invalid workflow filename")?;
            uuid::Uuid::parse_str(id).map_err(|_| "invalid workflow filename")?;
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

fn interrupted_revision(run: &mut WorkflowRun) {
    if matches!(
        run.steering.state.status,
        crate::WorkflowRevisionStatus::Fencing | crate::WorkflowRevisionStatus::Draining
    ) {
        run.steering.state.status = crate::WorkflowRevisionStatus::Blocked;
        if let Some(id) = &run.steering.state.pending_revision_id
            && let Some(revision) = run.steering.revisions.get_mut(id)
        {
            revision.status = crate::WorkflowRevisionStatus::Blocked;
            revision.error=Some("revision drain was interrupted; recover the original plan to reconcile its native work".into());
        }
    }
}

/// Reads one stored run without creating a service, normalizing status, or loading agents.
pub fn read_persisted(root: &AbsolutePathBuf, id: &str) -> Result<WorkflowRun, String> {
    files::read(root, id).map(|loaded| loaded.run)
}
fn summary(run: &WorkflowRun, step_count: u32) -> WorkflowSummary {
    WorkflowSummary {
        id: run.id.clone(),
        execution_id: run.execution_id.clone(),
        owner_thread_id: run.owner_thread_id.clone(),
        project_root: run.project_root.clone(),
        format_version: run.format_version,
        metadata: run.metadata.clone(),
        limits: run.limits.clone(),
        accepted_starts: run.accepted_starts,
        phase: run.phase.clone(),
        phase_count: run.phases.len() as u32,
        phase_progress: run.phase_progress.clone(),
        metrics: run.metrics.clone(),
        status: run.status,
        created_at: run.created_at,
        updated_at: run.updated_at,
        step_count,
    }
}
fn page(mut rows: Vec<WorkflowSummary>, offset: usize, limit: usize) -> Vec<WorkflowSummary> {
    rows.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    rows.into_iter()
        .skip(offset)
        .take(limit.min(crate::MAX_PAGE_SIZE))
        .collect()
}

pub fn list_persisted_summaries(
    root: &AbsolutePathBuf,
    project: Option<&AbsolutePathBuf>,
    offset: usize,
    limit: usize,
) -> Result<Vec<WorkflowSummary>, String> {
    let directories = match fs::read_dir(root) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut rows = Vec::new();
    for directory in directories {
        let path = directory.map_err(|error| error.to_string())?.path();
        if !path.is_dir() {
            continue;
        }
        let root =
            AbsolutePathBuf::from_absolute_path_checked(path).map_err(|error| error.to_string())?;
        for id in ids(&root)? {
            let (header, count) = files::header(&root, &id)?;
            if project.is_none_or(|project| header.project_root.as_ref() == Some(project)) {
                rows.push(summary(&header, count));
            }
        }
    }
    Ok(page(rows, offset, limit))
}
/// Compatibility read for callers that explicitly need complete run details.
pub fn list_persisted(
    root: &AbsolutePathBuf,
    project: Option<&AbsolutePathBuf>,
    offset: usize,
    limit: usize,
) -> Result<Vec<WorkflowRun>, String> {
    list_persisted_summaries(root, project, offset, limit)?
        .into_iter()
        .map(|summary| {
            files::read(&root.join(&summary.owner_thread_id), &summary.id).map(|loaded| loaded.run)
        })
        .collect()
}

#[cfg(test)]
#[path = "store/entity_metrics_tests.rs"]
mod entity_metrics_tests;
#[cfg(test)]
#[path = "store/store_tests.rs"]
mod tests;
