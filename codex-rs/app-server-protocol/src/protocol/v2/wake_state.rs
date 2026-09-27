use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

/// Live runtime waits; this does not affect whether user input is accepted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadWakeState {
    /// Number of distinct operations with a pending wake, including active waits.
    pub pending_count: u32,
    /// True when idle or suspended solely on those operations.
    pub waiting: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadWakeStateChangedNotification {
    pub thread_id: String,
    pub wake_state: ThreadWakeState,
}
