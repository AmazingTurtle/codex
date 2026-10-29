use super::*;
use codex_app_server_protocol::AuthRecoveryNotification;
use codex_app_server_protocol::ConfigWarningNotification;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerNotificationEnvelope;
use codex_app_server_protocol::ThreadRealtimeStartedNotification;
use codex_protocol::protocol::RealtimeConversationVersion;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::time::Duration;
use tokio::time::timeout;

fn absolute_path(path: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(path).expect("absolute path")
}

fn thread_realtime_started_notification() -> ServerNotification {
    ServerNotification::ThreadRealtimeStarted(ThreadRealtimeStartedNotification {
        thread_id: "thread-1".to_string(),
        realtime_session_id: None,
        version: RealtimeConversationVersion::V1,
    })
}

fn app_server_notification(notification: ServerNotification) -> OutgoingMessage {
    OutgoingMessage::AppServerNotification(ServerNotificationEnvelope {
        notification,
        emitted_at_ms: Some(1_234),
    })
}

#[tokio::test]
async fn workflow_final_presentation_requires_experimental_negotiation() {
    use codex_app_server_protocol::ThreadItem;
    use codex_app_server_protocol::ThreadItemsListResponse;
    let response:ThreadItemsListResponse=serde_json::from_value(json!({
        "data":[{"turnId":"owned","item":{"type":"agentMessage","id":"final","text":"{\"done\":true}","workflow":{
            "runId":"run","executionId":"execution","attemptId":"attempt","childThreadId":"child","turnId":"owned","itemId":"final","format":"json"
        }}},{"turnId":"owned","item":{"type":"dynamicToolCall","id":"tool","namespace":null,"tool":"retained","arguments":{"workflow":"USER_VALUE","nested":{"type":"agentMessage","workflow":"ALSO_USER_VALUE"}},"status":"completed","contentItems":null,"success":true,"durationMs":1}}],"nextCursor":null,"backwardsCursor":null,
    })).unwrap();
    for proven in [false, true] {
        let mut response = response.clone();
        if !proven && let ThreadItem::AgentMessage { workflow, .. } = &mut response.data[0].item {
            *workflow = None;
        }
        for experimental in [false, true] {
            let connection_id = ConnectionId(7);
            let (tx, mut rx) = mpsc::channel(1);
            let mut connections = HashMap::from([(
                connection_id,
                OutboundConnectionState::new(
                    tx,
                    Arc::new(AtomicBool::new(true)),
                    Arc::new(AtomicBool::new(experimental)),
                    Arc::new(RwLock::new(HashSet::new())),
                    /*disconnect_sender*/ None,
                ),
            )]);
            route_outgoing_envelope(
                &mut connections,
                OutgoingEnvelope::ToConnection {
                    connection_id,
                    message: OutgoingMessage::Response(crate::outgoing_message::OutgoingResponse {
                        id: RequestId::Integer(1),
                        result: Box::new(response.clone().into()),
                    }),
                    write_complete_tx: None,
                },
            )
            .await;
            let delivered = serde_json::to_value(rx.recv().await.unwrap().message).unwrap();
            let mut expected = serde_json::to_value(&response).unwrap();
            if !experimental {
                expected["data"][0]["item"]
                    .as_object_mut()
                    .unwrap()
                    .remove("workflow");
            }
            assert_eq!(
                delivered,
                json!({"id":1,"result":expected}),
                "raw serialization changes only the experimental native item field"
            );
            if !experimental {
                assert!(
                    delivered["result"]["data"][0]["item"]
                        .get("workflow")
                        .is_none()
                );
            }
        }
    }

    // Exercise clearing beyond the first match within a turn and across turns.
    // The raw serializer also omits fields, so inspect the original typed view:
    // in-process consumers must not retain a later experimental proof either.
    let first = serde_json::to_value(&response.data[0].item).unwrap();
    let mut second = first.clone();
    second["id"] = json!("second-final");
    second["workflow"]["itemId"] = json!("second-final");
    let mut later = first.clone();
    later["id"] = json!("later-final");
    later["workflow"]["itemId"] = json!("later-final");
    later["workflow"]["turnId"] = json!("later-owned");
    let turns: codex_app_server_protocol::ThreadTurnsListResponse = serde_json::from_value(json!({
        "data":[
            {"id":"owned","status":"completed","items":[first,response.data[1].item,second]},
            {"id":"later-owned","status":"completed","items":[later]}
        ],"nextCursor":null,"backwardsCursor":null,
    }))
    .unwrap();
    let item_paths = ["/data/0/items/0", "/data/0/items/2", "/data/1/items/0"];
    for experimental in [false, true] {
        let connection = ConnectionId(7);
        let (tx, mut rx) = mpsc::channel(1);
        let mut connections = HashMap::from([(
            connection,
            OutboundConnectionState::new(
                tx,
                Arc::new(AtomicBool::new(true)),
                Arc::new(AtomicBool::new(experimental)),
                Arc::new(RwLock::new(HashSet::new())),
                /*disconnect_sender*/ None,
            ),
        )]);
        route_outgoing_envelope(
            &mut connections,
            OutgoingEnvelope::ToConnection {
                connection_id: connection,
                message: OutgoingMessage::Response(crate::outgoing_message::OutgoingResponse {
                    id: RequestId::Integer(2),
                    result: Box::new(turns.clone().into()),
                }),
                write_complete_tx: None,
            },
        )
        .await;
        let delivered = rx.recv().await.unwrap().message;
        let mut expected = serde_json::to_value(&turns).unwrap();
        if !experimental {
            for path in item_paths {
                expected.pointer_mut(path).unwrap()["workflow"] = serde_json::Value::Null;
            }
        }
        let typed = serde_json::to_value(delivered.clone().into_original()).unwrap();
        assert_eq!(
            typed["result"], expected,
            "clearing must visit every typed item and turn; short-circuiting any() leaves later proofs intact"
        );
        if !experimental {
            for path in item_paths {
                expected
                    .pointer_mut(path)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("workflow");
            }
        }
        assert_eq!(
            serde_json::to_value(delivered).unwrap(),
            json!({"id":2,"result":expected})
        );
    }
}

#[tokio::test]
async fn stable_history_notifications_preserve_opt_out_and_envelope_metadata() {
    let notice:ServerNotification=serde_json::from_value(json!({"method":"item/completed","params":{
        "threadId":"child","turnId":"owned","completedAtMs":1200,
        "item":{"type":"agentMessage","id":"final","text":"Native text unchanged","workflow":null}
    }})).unwrap();
    for opted_out in [false, true] {
        let connection = ConnectionId(9);
        let (tx, mut rx) = mpsc::channel(1);
        let methods = if opted_out {
            HashSet::from(["item/completed".to_owned()])
        } else {
            HashSet::new()
        };
        let mut connections = HashMap::from([(
            connection,
            OutboundConnectionState::new(
                tx,
                Arc::new(AtomicBool::new(true)),
                Arc::new(AtomicBool::new(false)),
                Arc::new(RwLock::new(methods)),
                /*disconnect_sender*/ None,
            ),
        )]);
        route_outgoing_envelope(
            &mut connections,
            OutgoingEnvelope::ToConnection {
                connection_id: connection,
                message: app_server_notification(notice.clone()),
                write_complete_tx: None,
            },
        )
        .await;
        if opted_out {
            assert!(
                rx.try_recv().is_err(),
                "projection must not hide the original notification method from opt-out filtering"
            );
        } else {
            let wire = serde_json::to_value(rx.recv().await.unwrap().message).unwrap();
            let mut expected =
                serde_json::to_value(app_server_notification(notice.clone())).unwrap();
            expected["params"]["item"]
                .as_object_mut()
                .unwrap()
                .remove("workflow");
            assert_eq!(wire, expected);
            assert_eq!(wire["emittedAtMs"], 1234);
            assert!(wire["params"]["item"].get("workflow").is_none());
        }
    }
}

#[tokio::test]
async fn to_connection_notification_respects_opt_out_filters() {
    let connection_id = ConnectionId(7);
    let (writer_tx, mut writer_rx) = mpsc::channel(1);
    let initialized = Arc::new(AtomicBool::new(true));
    let opted_out_notification_methods =
        Arc::new(RwLock::new(HashSet::from(["configWarning".to_string()])));

    let mut connections = HashMap::new();
    connections.insert(
        connection_id,
        OutboundConnectionState::new(
            writer_tx,
            initialized,
            Arc::new(AtomicBool::new(true)),
            opted_out_notification_methods,
            /*disconnect_sender*/ None,
        ),
    );

    route_outgoing_envelope(
        &mut connections,
        OutgoingEnvelope::ToConnection {
            connection_id,
            message: app_server_notification(ServerNotification::ConfigWarning(
                ConfigWarningNotification {
                    summary: "task_started".to_string(),
                    details: None,
                    path: None,
                    range: None,
                },
            )),
            write_complete_tx: None,
        },
    )
    .await;

    assert!(
        writer_rx.try_recv().is_err(),
        "opted-out notification should be dropped"
    );
}

#[tokio::test]
async fn to_connection_notifications_are_dropped_for_opted_out_clients() {
    let connection_id = ConnectionId(10);
    let (writer_tx, mut writer_rx) = mpsc::channel(1);

    let mut connections = HashMap::new();
    connections.insert(
        connection_id,
        OutboundConnectionState::new(
            writer_tx,
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(RwLock::new(HashSet::from(["configWarning".to_string()]))),
            /*disconnect_sender*/ None,
        ),
    );

    route_outgoing_envelope(
        &mut connections,
        OutgoingEnvelope::ToConnection {
            connection_id,
            message: app_server_notification(ServerNotification::ConfigWarning(
                ConfigWarningNotification {
                    summary: "task_started".to_string(),
                    details: None,
                    path: None,
                    range: None,
                },
            )),
            write_complete_tx: None,
        },
    )
    .await;

    assert!(
        writer_rx.try_recv().is_err(),
        "opted-out notifications should not reach clients"
    );
}

#[tokio::test]
async fn to_connection_notifications_are_preserved_for_non_opted_out_clients() {
    let connection_id = ConnectionId(11);
    let (writer_tx, mut writer_rx) = mpsc::channel(1);

    let mut connections = HashMap::new();
    connections.insert(
        connection_id,
        OutboundConnectionState::new(
            writer_tx,
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(RwLock::new(HashSet::new())),
            /*disconnect_sender*/ None,
        ),
    );

    route_outgoing_envelope(
        &mut connections,
        OutgoingEnvelope::ToConnection {
            connection_id,
            message: app_server_notification(ServerNotification::ConfigWarning(
                ConfigWarningNotification {
                    summary: "task_started".to_string(),
                    details: None,
                    path: None,
                    range: None,
                },
            )),
            write_complete_tx: None,
        },
    )
    .await;

    let message = writer_rx
        .recv()
        .await
        .expect("notification should reach non-opted-out clients");
    assert!(matches!(
        message.message,
        OutgoingMessage::AppServerNotification(ServerNotificationEnvelope {
            notification: ServerNotification::ConfigWarning(ConfigWarningNotification { summary, .. }),
            ..
        }) if summary == "task_started"
    ));
}

#[tokio::test]
async fn experimental_notifications_are_dropped_without_capability() {
    let connection_id = ConnectionId(12);
    let (writer_tx, mut writer_rx) = mpsc::channel(1);

    let mut connections = HashMap::new();
    connections.insert(
        connection_id,
        OutboundConnectionState::new(
            writer_tx,
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(RwLock::new(HashSet::new())),
            /*disconnect_sender*/ None,
        ),
    );

    route_outgoing_envelope(
        &mut connections,
        OutgoingEnvelope::ToConnection {
            connection_id,
            message: app_server_notification(thread_realtime_started_notification()),
            write_complete_tx: None,
        },
    )
    .await;

    assert!(
        writer_rx.try_recv().is_err(),
        "experimental notifications should not reach clients without capability"
    );

    let recovery = AuthRecoveryNotification {
        thread_id: "thread-1".to_string(),
        turn_id: "turn-1".to_string(),
        provider: "Amazon Bedrock".to_string(),
        message: "Refreshing AWS authentication.".to_string(),
    };

    for (method, notification) in [
        (
            "modelProvider/authRecoveryStarted",
            ServerNotification::AuthRecoveryStarted(recovery.clone()),
        ),
        (
            "modelProvider/authRecoveryCompleted",
            ServerNotification::AuthRecoveryCompleted(recovery),
        ),
    ] {
        route_outgoing_envelope(
            &mut connections,
            OutgoingEnvelope::ToConnection {
                connection_id,
                message: app_server_notification(notification),
                write_complete_tx: None,
            },
        )
        .await;

        let message = writer_rx
            .recv()
            .await
            .expect("stable auth recovery notification should reach clients without capability");
        let OutgoingMessage::AppServerNotification(envelope) = message.message else {
            panic!("stable auth recovery notification should be delivered");
        };
        assert_eq!(envelope.notification.to_string(), method);
    }
}

#[tokio::test]
async fn experimental_notifications_are_preserved_with_capability() {
    let connection_id = ConnectionId(13);
    let (writer_tx, mut writer_rx) = mpsc::channel(1);

    let mut connections = HashMap::new();
    connections.insert(
        connection_id,
        OutboundConnectionState::new(
            writer_tx,
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(RwLock::new(HashSet::new())),
            /*disconnect_sender*/ None,
        ),
    );

    route_outgoing_envelope(
        &mut connections,
        OutgoingEnvelope::ToConnection {
            connection_id,
            message: app_server_notification(thread_realtime_started_notification()),
            write_complete_tx: None,
        },
    )
    .await;

    let message = writer_rx
        .recv()
        .await
        .expect("experimental notification should reach opted-in client");
    assert!(matches!(
        message.message,
        OutgoingMessage::AppServerNotification(ServerNotificationEnvelope {
            notification: ServerNotification::ThreadRealtimeStarted(_),
            ..
        })
    ));
}

#[tokio::test]
async fn command_execution_request_approval_strips_additional_permissions_without_capability() {
    let connection_id = ConnectionId(8);
    let (writer_tx, mut writer_rx) = mpsc::channel(1);

    let mut connections = HashMap::new();
    connections.insert(
        connection_id,
        OutboundConnectionState::new(
            writer_tx,
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(RwLock::new(HashSet::new())),
            /*disconnect_sender*/ None,
        ),
    );

    route_outgoing_envelope(
        &mut connections,
        OutgoingEnvelope::ToConnection {
            connection_id,
            message: OutgoingMessage::Request(ServerRequest::CommandExecutionRequestApproval {
                request_id: RequestId::Integer(1),
                params: codex_app_server_protocol::CommandExecutionRequestApprovalParams {
                    kind: Default::default(),
                    thread_id: "thr_123".to_string(),
                    turn_id: "turn_123".to_string(),
                    item_id: "call_123".to_string(),
                    started_at_ms: 0,
                    approval_id: None,
                    environment_id: None,
                    reason: Some("Need extra read access".to_string()),
                    network_approval_context: None,
                    command: Some("cat file".to_string()),
                    cwd: Some(absolute_path("/tmp").into()),
                    command_actions: None,
                    additional_permissions: Some(
                        codex_app_server_protocol::AdditionalPermissionProfile {
                            network: None,
                            file_system: Some(
                                codex_app_server_protocol::AdditionalFileSystemPermissions {
                                    read: Some(vec![absolute_path("/tmp/allowed").into()]),
                                    write: None,
                                    glob_scan_max_depth: None,
                                    entries: None,
                                },
                            ),
                        },
                    ),
                    proposed_execpolicy_amendment: None,
                    proposed_network_policy_amendments: None,
                    available_decisions: None,
                },
            }),
            write_complete_tx: None,
        },
    )
    .await;

    let message = writer_rx
        .recv()
        .await
        .expect("request should be delivered to the connection");
    let json = serde_json::to_value(message.message).expect("request should serialize");
    assert_eq!(json["params"].get("additionalPermissions"), None);
}

#[tokio::test]
async fn command_execution_request_approval_keeps_additional_permissions_with_capability() {
    let connection_id = ConnectionId(9);
    let (writer_tx, mut writer_rx) = mpsc::channel(1);

    let mut connections = HashMap::new();
    connections.insert(
        connection_id,
        OutboundConnectionState::new(
            writer_tx,
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(RwLock::new(HashSet::new())),
            /*disconnect_sender*/ None,
        ),
    );

    route_outgoing_envelope(
        &mut connections,
        OutgoingEnvelope::ToConnection {
            connection_id,
            message: OutgoingMessage::Request(ServerRequest::CommandExecutionRequestApproval {
                request_id: RequestId::Integer(1),
                params: codex_app_server_protocol::CommandExecutionRequestApprovalParams {
                    kind: Default::default(),
                    thread_id: "thr_123".to_string(),
                    turn_id: "turn_123".to_string(),
                    item_id: "call_123".to_string(),
                    started_at_ms: 0,
                    approval_id: None,
                    environment_id: None,
                    reason: Some("Need extra read access".to_string()),
                    network_approval_context: None,
                    command: Some("cat file".to_string()),
                    cwd: Some(absolute_path("/tmp").into()),
                    command_actions: None,
                    additional_permissions: Some(
                        codex_app_server_protocol::AdditionalPermissionProfile {
                            network: None,
                            file_system: Some(
                                codex_app_server_protocol::AdditionalFileSystemPermissions {
                                    read: Some(vec![absolute_path("/tmp/allowed").into()]),
                                    write: None,
                                    glob_scan_max_depth: None,
                                    entries: None,
                                },
                            ),
                        },
                    ),
                    proposed_execpolicy_amendment: None,
                    proposed_network_policy_amendments: None,
                    available_decisions: None,
                },
            }),
            write_complete_tx: None,
        },
    )
    .await;

    let message = writer_rx
        .recv()
        .await
        .expect("request should be delivered to the connection");
    let json = serde_json::to_value(message.message).expect("request should serialize");
    let allowed_path = absolute_path("/tmp/allowed").to_string_lossy().into_owned();
    assert_eq!(
        json["params"]["additionalPermissions"],
        json!({
            "network": null,
            "fileSystem": {
                "read": [allowed_path],
            "write": null,
            },
        })
    );
}

/// Broadcasts preserve their payload for eligible recipients and skip every other connection.
/// The empty eligible set must not accidentally send to an uninitialized or opted-out client.
#[tokio::test]
async fn broadcast_only_reaches_eligible_connections() {
    for eligible_count in 0..=2 {
        let notification = thread_realtime_started_notification();
        let method = notification.to_string();
        let message = app_server_notification(notification);
        let expected = serde_json::to_value(&message).expect("serialize notification");
        let mut connections = HashMap::new();
        let mut receivers = Vec::new();
        let configs = [
            (false, true, HashSet::new()),
            (true, false, HashSet::new()),
            (true, true, HashSet::from([method])),
        ]
        .into_iter()
        .chain((0..eligible_count).map(|_| (true, true, HashSet::new())));
        for (index, (initialized, experimental_api, opted_out_methods)) in configs.enumerate() {
            let (writer_tx, writer_rx) = mpsc::channel(1);
            connections.insert(
                ConnectionId(index as u64),
                OutboundConnectionState::new(
                    writer_tx,
                    Arc::new(AtomicBool::new(initialized)),
                    Arc::new(AtomicBool::new(experimental_api)),
                    Arc::new(RwLock::new(opted_out_methods)),
                    /*disconnect_sender*/ None,
                ),
            );
            receivers.push(writer_rx);
        }

        route_outgoing_envelope(&mut connections, OutgoingEnvelope::Broadcast { message }).await;

        for (index, mut receiver) in receivers.into_iter().enumerate() {
            if index < 3 {
                assert!(receiver.try_recv().is_err());
            } else {
                let queued = receiver
                    .try_recv()
                    .expect("eligible client receives notification");
                assert_eq!(serde_json::to_value(&queued.message).unwrap(), expected);
                assert!(queued.write_complete_tx.is_none());
                assert!(receiver.try_recv().is_err());
            }
        }
    }
}

#[tokio::test]
async fn broadcast_does_not_block_on_slow_connection() {
    let fast_connection_id = ConnectionId(1);
    let slow_connection_id = ConnectionId(2);

    let (fast_writer_tx, mut fast_writer_rx) = mpsc::channel(1);
    let (slow_writer_tx, mut slow_writer_rx) = mpsc::channel(1);
    let fast_disconnect_token = CancellationToken::new();
    let slow_disconnect_token = CancellationToken::new();

    let mut connections = HashMap::new();
    connections.insert(
        fast_connection_id,
        OutboundConnectionState::new(
            fast_writer_tx,
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(RwLock::new(HashSet::new())),
            Some(fast_disconnect_token.clone()),
        ),
    );
    connections.insert(
        slow_connection_id,
        OutboundConnectionState::new(
            slow_writer_tx.clone(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(RwLock::new(HashSet::new())),
            Some(slow_disconnect_token.clone()),
        ),
    );

    let queued_message = app_server_notification(ServerNotification::ConfigWarning(
        ConfigWarningNotification {
            summary: "already-buffered".to_string(),
            details: None,
            path: None,
            range: None,
        },
    ));
    slow_writer_tx
        .try_send(QueuedOutgoingMessage::new(queued_message))
        .expect("channel should have room");

    let broadcast_message = app_server_notification(ServerNotification::ConfigWarning(
        ConfigWarningNotification {
            summary: "test".to_string(),
            details: None,
            path: None,
            range: None,
        },
    ));
    timeout(
        Duration::from_millis(100),
        route_outgoing_envelope(
            &mut connections,
            OutgoingEnvelope::Broadcast {
                message: broadcast_message,
            },
        ),
    )
    .await
    .expect("broadcast should return even when one connection is slow");
    assert!(!connections.contains_key(&slow_connection_id));
    assert!(slow_disconnect_token.is_cancelled());
    assert!(!fast_disconnect_token.is_cancelled());
    let fast_message = fast_writer_rx
        .try_recv()
        .expect("fast connection should receive the broadcast notification");
    assert!(matches!(
        fast_message.message,
        OutgoingMessage::AppServerNotification(ServerNotificationEnvelope {
            notification: ServerNotification::ConfigWarning(ConfigWarningNotification { summary, .. }),
            ..
        }) if summary == "test"
    ));

    let slow_message = slow_writer_rx
        .try_recv()
        .expect("slow connection should retain its original buffered message");
    assert!(matches!(
        slow_message.message,
        OutgoingMessage::AppServerNotification(ServerNotificationEnvelope {
            notification: ServerNotification::ConfigWarning(ConfigWarningNotification { summary, .. }),
            ..
        }) if summary == "already-buffered"
    ));
}

#[tokio::test]
async fn to_connection_stdio_waits_instead_of_disconnecting_when_writer_queue_is_full() {
    let connection_id = ConnectionId(3);
    let (writer_tx, mut writer_rx) = mpsc::channel(1);
    writer_tx
        .send(QueuedOutgoingMessage::new(app_server_notification(
            ServerNotification::ConfigWarning(ConfigWarningNotification {
                summary: "queued".to_string(),
                details: None,
                path: None,
                range: None,
            }),
        )))
        .await
        .expect("channel should accept the first queued message");

    let mut connections = HashMap::new();
    connections.insert(
        connection_id,
        OutboundConnectionState::new(
            writer_tx,
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(RwLock::new(HashSet::new())),
            /*disconnect_sender*/ None,
        ),
    );

    let route_task = tokio::spawn(async move {
        route_outgoing_envelope(
            &mut connections,
            OutgoingEnvelope::ToConnection {
                connection_id,
                message: app_server_notification(ServerNotification::ConfigWarning(
                    ConfigWarningNotification {
                        summary: "second".to_string(),
                        details: None,
                        path: None,
                        range: None,
                    },
                )),
                write_complete_tx: None,
            },
        )
        .await
    });

    let first = timeout(Duration::from_millis(100), writer_rx.recv())
        .await
        .expect("first queued message should be readable")
        .expect("first queued message should exist");
    timeout(Duration::from_millis(100), route_task)
        .await
        .expect("routing should finish after the first queued message is drained")
        .expect("routing task should succeed");

    assert!(matches!(
        first.message,
        OutgoingMessage::AppServerNotification(ServerNotificationEnvelope {
            notification: ServerNotification::ConfigWarning(ConfigWarningNotification { summary, .. }),
            ..
        }) if summary == "queued"
    ));
    let second = writer_rx
        .try_recv()
        .expect("second notification should be delivered once the queue has room");
    assert!(matches!(
        second.message,
        OutgoingMessage::AppServerNotification(ServerNotificationEnvelope {
            notification: ServerNotification::ConfigWarning(ConfigWarningNotification { summary, .. }),
            ..
        }) if summary == "second"
    ));
}
