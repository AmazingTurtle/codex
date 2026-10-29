//! Connection-specific serialization at known native-history item locations.
use super::OutgoingMessage;
use codex_app_server_protocol::ClientResponsePayload as Response;
use codex_app_server_protocol::ServerNotification as Notification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadTimelineEntry;
use codex_app_server_protocol::Turn;
use serde::Serialize;
use serde::Serializer;
use serde::ser::Error;
use serde_json::Value;

fn clear_item(item: &mut ThreadItem) -> bool {
    if let ThreadItem::AgentMessage { workflow, .. } = item {
        *workflow = None;
        true
    } else {
        false
    }
}

fn clear_turn(turn: &mut Turn) -> bool {
    let mut found = false;
    // Every item must be cleared, including those after the first match.
    for item in &mut turn.items {
        found |= clear_item(item);
    }
    found
}

fn clear_turns(turns: &mut [Turn]) -> bool {
    let mut found = false;
    for turn in turns {
        found |= clear_turn(turn);
    }
    found
}

pub(super) fn clear(message: &mut OutgoingMessage) -> bool {
    match message {
        OutgoingMessage::Response(response) => match response.result.as_mut() {
            Response::ThreadRead(response) => clear_turns(&mut response.thread.turns),
            Response::ThreadResume(response) => {
                let mut found = clear_turns(&mut response.thread.turns);
                if let Some(page) = &mut response.initial_turns_page {
                    found |= clear_turns(&mut page.data);
                }
                found
            }
            Response::ThreadStart(response) => clear_turns(&mut response.thread.turns),
            Response::ThreadFork(response) => clear_turns(&mut response.thread.turns),
            Response::ThreadRevert(response) => clear_turns(&mut response.thread.turns),
            Response::ThreadUnarchive(response) => clear_turns(&mut response.thread.turns),
            Response::ThreadMetadataUpdate(response) => clear_turns(&mut response.thread.turns),
            Response::ThreadList(response) => {
                let mut found = false;
                for thread in &mut response.data {
                    found |= clear_turns(&mut thread.turns);
                }
                found
            }
            Response::ThreadSearch(response) => {
                let mut found = false;
                for entry in &mut response.data {
                    found |= clear_turns(&mut entry.thread.turns);
                }
                found
            }
            Response::ThreadTurnsList(response) => clear_turns(&mut response.data),
            Response::ThreadItemsList(response) => {
                let mut found = false;
                for entry in &mut response.data {
                    found |= clear_item(&mut entry.item);
                }
                found
            }
            Response::ThreadItemsRead(response) => {
                let mut found = false;
                for entry in &mut response.data {
                    found |= clear_item(&mut entry.item);
                }
                found
            }
            Response::ThreadTimelineList(response) => {
                response.data.iter_mut().fold(false, |found, entry| {
                    if let ThreadTimelineEntry::Item { item, .. } = entry {
                        clear_item(item) || found
                    } else {
                        found
                    }
                })
            }
            Response::TurnStart(response) => clear_turn(&mut response.turn),
            Response::ThreadQueueStart(response) => clear_turn(&mut response.turn),
            _ => false,
        },
        OutgoingMessage::AppServerNotification(envelope) => match &mut envelope.notification {
            Notification::ItemStarted(notification) => clear_item(&mut notification.item),
            Notification::ItemCompleted(notification) => clear_item(&mut notification.item),
            Notification::TurnStarted(notification) => clear_turn(&mut notification.turn),
            Notification::TurnCompleted(notification) => clear_turn(&mut notification.turn),
            Notification::ThreadStarted(notification) => {
                clear_turns(&mut notification.thread.turns)
            }
            _ => false,
        },
        OutgoingMessage::Request(_)
        | OutgoingMessage::Error(_)
        | OutgoingMessage::StableHistory(_) => false,
    }
}

pub(super) fn serialize<S: Serializer>(
    message: &OutgoingMessage,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut value = serde_json::to_value(message).map_err(S::Error::custom)?;
    match message {
        OutgoingMessage::Response(response) => {
            let result = &mut value["result"];
            match response.result.as_ref() {
                Response::ThreadRead(_)
                | Response::ThreadStart(_)
                | Response::ThreadFork(_)
                | Response::ThreadRevert(_)
                | Response::ThreadUnarchive(_)
                | Response::ThreadMetadataUpdate(_) => json_turns(&mut result["thread"]["turns"]),
                Response::ThreadResume(_) => {
                    json_turns(&mut result["thread"]["turns"]);
                    if result["initialTurnsPage"].is_object() {
                        json_turns(&mut result["initialTurnsPage"]["data"]);
                    }
                }
                Response::ThreadTurnsList(_) => json_turns(&mut result["data"]),
                Response::ThreadItemsList(_)
                | Response::ThreadItemsRead(_)
                | Response::ThreadTimelineList(_) => {
                    if let Some(entries) = result["data"].as_array_mut() {
                        for entry in entries {
                            if let Some(item) = entry.get_mut("item") {
                                json_item(item);
                            }
                        }
                    }
                }
                Response::ThreadList(_) => {
                    if let Some(threads) = result["data"].as_array_mut() {
                        for thread in threads {
                            json_turns(&mut thread["turns"]);
                        }
                    }
                }
                Response::ThreadSearch(_) => {
                    if let Some(entries) = result["data"].as_array_mut() {
                        for entry in entries {
                            json_turns(&mut entry["thread"]["turns"]);
                        }
                    }
                }
                Response::TurnStart(_) | Response::ThreadQueueStart(_) => {
                    json_items(&mut result["turn"]["items"])
                }
                _ => {}
            }
        }
        OutgoingMessage::AppServerNotification(envelope) => {
            let params = &mut value["params"];
            match &envelope.notification {
                Notification::ItemStarted(_) | Notification::ItemCompleted(_) => {
                    json_item(&mut params["item"])
                }
                Notification::TurnStarted(_) | Notification::TurnCompleted(_) => {
                    json_items(&mut params["turn"]["items"])
                }
                Notification::ThreadStarted(_) => json_turns(&mut params["thread"]["turns"]),
                _ => {}
            }
        }
        OutgoingMessage::Request(_)
        | OutgoingMessage::Error(_)
        | OutgoingMessage::StableHistory(_) => {}
    }
    value.serialize(serializer)
}

fn json_turns(turns: &mut Value) {
    if let Some(turns) = turns.as_array_mut() {
        for turn in turns {
            json_items(&mut turn["items"]);
        }
    }
}

fn json_items(items: &mut Value) {
    if let Some(items) = items.as_array_mut() {
        for item in items {
            json_item(item);
        }
    }
}

fn json_item(item: &mut Value) {
    // Never descend into user JSON, tool arguments, result bodies, or text.
    if item["type"] == "agentMessage"
        && let Some(object) = item.as_object_mut()
    {
        object.remove("workflow");
    }
}
