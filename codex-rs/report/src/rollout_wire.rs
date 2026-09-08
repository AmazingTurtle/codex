use codex_protocol::protocol::TokenUsage;
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct MinimalRolloutLine {
    pub(super) timestamp: String,
    #[serde(default)]
    pub(super) ordinal: Option<u64>,
    #[serde(flatten)]
    pub(super) item: MinimalRolloutItem,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum MinimalRolloutItem {
    SessionMeta {
        payload: MinimalSessionMeta,
    },
    TurnContext {
        payload: MinimalTurnContext,
    },
    TokenUsageRecord {
        payload: Box<MinimalTokenUsageRecord>,
    },
    EventMsg {
        payload: MinimalEventMsg,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
pub(super) struct MinimalSessionMeta {
    pub(super) id: String,
    #[serde(default)]
    pub(super) forked_from_ordinal_exclusive: Option<u64>,
    #[serde(default)]
    pub(super) subagent_history_start_ordinal: Option<u64>,
}

#[derive(Deserialize)]
pub(super) struct MinimalTurnContext {
    #[serde(default)]
    pub(super) turn_id: Option<String>,
    pub(super) model: String,
}

#[derive(Deserialize)]
pub(super) struct MinimalTokenUsageRecord {
    pub(super) thread_id: String,
    pub(super) turn_id: String,
    pub(super) response_id: String,
    pub(super) usage: TokenUsage,
    pub(super) thread_token_usage: TokenUsage,
    #[serde(default)]
    pub(super) account_id: Option<String>,
    #[serde(default)]
    pub(super) requested_model: Option<String>,
    #[serde(default)]
    pub(super) requested_service_tier: Option<String>,
    #[serde(default)]
    pub(super) reported_model: Option<String>,
    #[serde(default)]
    pub(super) reported_service_tier: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum MinimalEventMsg {
    TokenCount(Box<MinimalTokenCountEvent>),
    ThreadSettingsApplied(MinimalThreadSettingsAppliedEvent),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
pub(super) struct MinimalTokenCountEvent {
    pub(super) info: Option<MinimalTokenUsageInfo>,
    #[serde(default)]
    pub(super) rate_limits: Option<MinimalRateLimitSnapshot>,
}

#[derive(Deserialize)]
pub(super) struct MinimalRateLimitSnapshot {
    #[serde(default)]
    pub(super) limit_id: Option<String>,
    #[serde(default)]
    pub(super) primary: Option<MinimalRateLimitWindow>,
    #[serde(default)]
    pub(super) secondary: Option<MinimalRateLimitWindow>,
}

#[derive(Deserialize)]
pub(super) struct MinimalRateLimitWindow {
    pub(super) used_percent: serde_json::Number,
    #[serde(default)]
    pub(super) window_minutes: Option<i64>,
    #[serde(default)]
    pub(super) resets_at: Option<i64>,
}

#[derive(Deserialize)]
pub(super) struct MinimalTokenUsageInfo {
    pub(super) total_token_usage: TokenUsage,
    pub(super) last_token_usage: TokenUsage,
}

#[derive(Deserialize)]
pub(super) struct MinimalThreadSettingsAppliedEvent {
    #[serde(default)]
    pub(super) thread_id: Option<String>,
    pub(super) thread_settings: MinimalThreadSettings,
}

#[derive(Deserialize)]
pub(super) struct MinimalThreadSettings {
    pub(super) model: String,
    #[serde(default)]
    pub(super) service_tier: Option<String>,
}
