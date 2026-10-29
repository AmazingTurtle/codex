//! A lagged observer rereads the existing root service without loading any thread.
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::request_processors::workflow_processor::conversion;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::WorkflowProgressNotification;
use codex_app_server_protocol::WorkflowUpdatedNotification;
use codex_core::workflows::DynamicWorkflowService;

pub(super) enum Stream {
    Snapshots,
    Progress,
}

pub(super) async fn replay(
    service: &DynamicWorkflowService,
    outgoing: &ThreadScopedOutgoingMessageSender,
    stream: Stream,
) -> Result<(), String> {
    let owner = service.owner_thread_id();
    let mut offset = 0;
    loop {
        let page = service.list_summaries(offset, /*limit*/ 50)?;
        let count = page.len();
        for summary in page {
            // Preserve root isolation at the delivery boundary.
            if summary.owner_thread_id != owner {
                continue;
            }
            let notification = match stream {
                Stream::Snapshots => {
                    ServerNotification::WorkflowUpdated(WorkflowUpdatedNotification {
                        thread_id: owner.to_string(),
                        workflow: conversion::workflow(service.get(&summary.id)?),
                    })
                }
                Stream::Progress => {
                    ServerNotification::WorkflowProgress(WorkflowProgressNotification {
                        thread_id: owner.to_string(),
                        workflow: conversion::summary(summary),
                    })
                }
            };
            outgoing.send_server_notification(notification).await;
        }
        if count < 50 {
            return Ok(());
        }
        offset += count;
    }
}
