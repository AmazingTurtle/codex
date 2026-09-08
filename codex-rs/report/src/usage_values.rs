use codex_protocol::protocol::TokenUsage;

use crate::UsageRecord;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct UsageValues {
    input_tokens: i64,
    cached_input_tokens: i64,
    cache_write_input_tokens: i64,
    output_tokens: i64,
    reasoning_output_tokens: i64,
}

pub(super) fn valid_usage(usage: &TokenUsage) -> bool {
    usage.input_tokens >= 0
        && usage.cached_input_tokens >= 0
        && usage.cache_write_input_tokens >= 0
        && usage.output_tokens >= 0
        && usage.reasoning_output_tokens >= 0
        && usage.total_tokens >= 0
        && usage.cached_input_tokens <= usage.input_tokens
        && usage.reasoning_output_tokens <= usage.output_tokens
}

pub(super) fn has_usage(usage: &TokenUsage) -> bool {
    usage.input_tokens != 0
        || usage.cached_input_tokens != 0
        || usage.cache_write_input_tokens != 0
        || usage.output_tokens != 0
        || usage.reasoning_output_tokens != 0
}

pub(super) fn visible_usage_eq(left: &TokenUsage, right: &TokenUsage) -> bool {
    UsageValues::from(left) == UsageValues::from(right) && left.total_tokens == right.total_tokens
}

pub(super) fn usage_delta(current: &TokenUsage, previous: &TokenUsage) -> Option<TokenUsage> {
    Some(TokenUsage {
        input_tokens: current.input_tokens.checked_sub(previous.input_tokens)?,
        cached_input_tokens: current
            .cached_input_tokens
            .checked_sub(previous.cached_input_tokens)?,
        cache_write_input_tokens: current
            .cache_write_input_tokens
            .checked_sub(previous.cache_write_input_tokens)?,
        output_tokens: current.output_tokens.checked_sub(previous.output_tokens)?,
        reasoning_output_tokens: current
            .reasoning_output_tokens
            .checked_sub(previous.reasoning_output_tokens)?,
        total_tokens: current.total_tokens.checked_sub(previous.total_tokens)?,
        ..TokenUsage::default()
    })
    .filter(valid_usage)
}

impl From<&TokenUsage> for UsageValues {
    fn from(usage: &TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_write_input_tokens: usage.cache_write_input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_output_tokens: usage.reasoning_output_tokens,
        }
    }
}

impl From<&UsageRecord> for UsageValues {
    fn from(usage: &UsageRecord) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_write_input_tokens: usage.cache_write_input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_output_tokens: usage.reasoning_output_tokens,
        }
    }
}
