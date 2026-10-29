//! Reconciles evicted persistent contributors without loading or restarting agents.

use super::host::Children;
use crate::session::session::Session;
use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowObserver;
use codex_thread_store::ListThreadsParams;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::SortDirection;
use codex_thread_store::ThreadRelationFilter;
use codex_thread_store::ThreadSortKey;
use codex_thread_store::ThreadStoreError;
use std::collections::HashMap;
use std::sync::PoisonError;

pub(super) async fn reconcile(
    owner: &Session,
    children: &Children,
    run_id: &str,
    observer: &WorkflowObserver,
    scopes: &super::usage_ownership::Scopes,
) -> Result<(), String> {
    let known = children
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let mut live = super::usage_ownership::members(owner, children, run_id, scopes)
        .await?
        .into_iter()
        .map(|(session, attribution, selected)| (session.thread_id, (attribution, selected)))
        .collect::<HashMap<_, _>>();
    let loaded = owner
        .services
        .local_agent_runtime
        .execution_control
        .members()
        .into_iter()
        .map(|session| session.thread_id)
        .collect::<std::collections::HashSet<_>>();
    let mut parents = known
        .iter()
        .filter(|(_, attribution)| attribution.run_id == run_id)
        .map(|(child, _)| (*child, None))
        .collect::<HashMap<_, _>>();
    'descendants: for (child, _) in known
        .iter()
        .filter(|(_, attribution)| attribution.run_id == run_id)
    {
        for archived in [false, true] {
            let mut cursor = None;
            loop {
                let page = match owner
                    .services
                    .thread_store
                    .list_threads(ListThreadsParams {
                        page_size: 100,
                        cursor,
                        sort_key: ThreadSortKey::CreatedAt,
                        sort_direction: SortDirection::Asc,
                        allowed_sources: Vec::new(),
                        model_providers: Some(Vec::new()),
                        cwd_filters: None,
                        section: None,
                        project_id: None,
                        archived,
                        search_term: None,
                        relation_filter: Some(ThreadRelationFilter::DescendantsOf(*child)),
                        use_state_db_only: false,
                    })
                    .await
                {
                    Ok(page) => page,
                    Err(ThreadStoreError::Unsupported {
                        operation: "thread_relations" | "list_threads",
                    }) => {
                        observer(WorkflowHostEvent::UsageIncomplete {
                            reason:
                                "the thread store cannot enumerate historical workflow descendants"
                                    .into(),
                        })?;
                        break 'descendants;
                    }
                    Err(error) => return Err(error.to_string()),
                };
                for thread in page.items {
                    if !parents.contains_key(&thread.thread_id)
                        && parents.len() >= codex_dynamic_workflows::MAX_OBSERVED_THREADS
                    {
                        observer(WorkflowHostEvent::UsageIncomplete {
                            reason: "workflow usage contributor limit reached during recovery"
                                .into(),
                        })?;
                        return Ok(());
                    }
                    parents.insert(thread.thread_id, thread.parent_thread_id);
                }
                match page.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
        }
    }
    for thread in parents.keys().copied() {
        let mut ancestor = thread;
        let mut depth = 0;
        let attribution = loop {
            if let Some(attribution) = known.get(&ancestor) {
                break attribution;
            }
            depth += 1;
            if depth > 64 {
                return Err("workflow usage ancestry exceeds supported depth".into());
            }
            let parent = match parents.get(&ancestor) {
                Some(parent) => *parent,
                None => {
                    owner
                        .services
                        .thread_store
                        .read_thread(ReadThreadParams {
                            thread_id: ancestor,
                            include_archived: true,
                            include_history: false,
                        })
                        .await
                        .map_err(|error| error.to_string())?
                        .parent_thread_id
                }
            };
            ancestor = parent.ok_or("workflow usage contributor has no owned ancestor")?;
        };
        if attribution.run_id != run_id {
            continue;
        }
        if let Some(selected) = live
            .remove(&thread)
            .map(|(_, selected)| selected)
            .or_else(|| super::usage_ownership::cached(scopes, thread))
        {
            super::usage::report_selection(observer, &attribution.attempt_id, thread, selected)?;
            continue;
        }
        if loaded.contains(&thread) {
            continue;
        }
        let history = owner
            .services
            .thread_store
            .load_latest_model_context(LoadThreadHistoryParams {
                thread_id: thread,
                include_archived: true,
            })
            .await
            .map_err(|error| error.to_string())?;
        super::usage::reconcile_items(observer, &attribution.attempt_id, thread, &history.items)?;
    }
    // An optional persisted relation index must not hide descendants whose
    // live ownership and own-response counters are already authoritative.
    for (thread, (attribution, selected)) in live {
        super::usage::report_selection(observer, &attribution.attempt_id, thread, selected)?;
    }
    Ok(())
}
