use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use codex_protocol::protocol::TokenUsage;

use crate::Diagnostics;
use crate::LimitRecord;
use crate::ReportData;
use crate::UsageRecord;
use crate::UsageSource;
use crate::limits::HistoricalLimitKey;
use crate::limits::historical_limits;
use crate::model::REPORT_SCHEMA_VERSION;
use crate::rollout_reader::BoundedLineReader;
use crate::rollout_reader::discover_rollouts;
use crate::rollout_wire::MinimalEventMsg;
use crate::rollout_wire::MinimalRolloutItem;
use crate::rollout_wire::MinimalRolloutLine;
use crate::usage_values::UsageValues;
use crate::usage_values::has_usage;
use crate::usage_values::usage_delta;
use crate::usage_values::valid_usage;
use crate::usage_values::visible_usage_eq;

#[derive(Clone, Default)]
struct Attribution {
    requested_model: Option<String>,
    requested_service_tier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ExactKey {
    thread_id: String,
    turn_id: String,
    response_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LegacyKey {
    thread_id: String,
    timestamp: String,
    usage: UsageValues,
}

struct ExactUsage {
    record: UsageRecord,
    thread_total: UsageValues,
}

struct LegacyUsage {
    record: UsageRecord,
    thread_total: UsageValues,
}

struct Extraction {
    diagnostics: Diagnostics,
    exact: HashMap<ExactKey, ExactUsage>,
    legacy: Vec<LegacyUsage>,
    legacy_seen: HashSet<LegacyKey>,
    thread_attribution: HashMap<String, Attribution>,
    turn_attribution: HashMap<(String, String), Attribution>,
    limits: Vec<LimitRecord>,
    limits_seen: HashSet<HistoricalLimitKey>,
}

impl Extraction {
    fn new() -> Self {
        Self {
            diagnostics: Diagnostics::default(),
            exact: HashMap::new(),
            legacy: Vec::new(),
            legacy_seen: HashSet::new(),
            thread_attribution: HashMap::new(),
            turn_attribution: HashMap::new(),
            limits: Vec::new(),
            limits_seen: HashSet::new(),
        }
    }

    fn scan_file(&mut self, path: &Path) {
        self.diagnostics.files_scanned += 1;
        let file = match codex_rollout::open_rollout_seekable_reader(path) {
            Ok(file) => file,
            Err(_) => {
                self.diagnostics.file_read_errors += 1;
                return;
            }
        };
        let mut reader = BoundedLineReader::new(file);
        let mut current_thread_id = None;
        let mut previous_total = None;
        let mut legacy_start_ordinal = None;
        loop {
            let line = match reader.next_line() {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(_) => {
                    self.diagnostics.file_read_errors += 1;
                    break;
                }
            };
            let Some(line) = line else {
                self.diagnostics.oversized_records += 1;
                continue;
            };
            let Ok(line) = serde_json::from_slice::<MinimalRolloutLine>(line) else {
                self.diagnostics.malformed_records += 1;
                continue;
            };
            self.diagnostics.records_parsed += 1;
            self.process_line(
                line,
                &mut current_thread_id,
                &mut previous_total,
                &mut legacy_start_ordinal,
            );
        }
    }

    fn process_line(
        &mut self,
        line: MinimalRolloutLine,
        current_thread_id: &mut Option<String>,
        previous_total: &mut Option<TokenUsage>,
        legacy_start_ordinal: &mut Option<u64>,
    ) {
        let ordinal = line.ordinal;
        let Some(timestamp) = normalize_timestamp(line.timestamp.as_str()) else {
            self.diagnostics.malformed_records += 1;
            return;
        };
        match line.item {
            MinimalRolloutItem::SessionMeta { payload } => {
                let thread_id = payload.id;
                if current_thread_id.as_ref() != Some(&thread_id) {
                    *previous_total = None;
                }
                *current_thread_id = Some(thread_id);
                *legacy_start_ordinal = payload
                    .forked_from_ordinal_exclusive
                    .into_iter()
                    .chain(payload.subagent_history_start_ordinal)
                    .max();
            }
            MinimalRolloutItem::TurnContext { payload: context } => {
                let Some(thread_id) = current_thread_id.clone() else {
                    return;
                };
                let attribution = Attribution {
                    requested_model: Some(context.model),
                    requested_service_tier: self
                        .thread_attribution
                        .get(&thread_id)
                        .and_then(|value| value.requested_service_tier.clone()),
                };
                if let Some(turn_id) = context.turn_id {
                    self.turn_attribution
                        .insert((thread_id.clone(), turn_id), attribution.clone());
                }
                self.thread_attribution.insert(thread_id, attribution);
            }
            MinimalRolloutItem::EventMsg {
                payload: MinimalEventMsg::ThreadSettingsApplied(event),
            } => {
                let thread_id = event.thread_id.or_else(|| current_thread_id.clone());
                if let Some(thread_id) = thread_id {
                    self.thread_attribution.insert(
                        thread_id,
                        Attribution {
                            requested_model: Some(event.thread_settings.model),
                            requested_service_tier: event.thread_settings.service_tier,
                        },
                    );
                }
            }
            MinimalRolloutItem::TokenUsageRecord { payload: record } => {
                if !valid_usage(&record.usage) {
                    self.diagnostics.malformed_records += 1;
                    return;
                }
                let thread_id = record.thread_id.to_string();
                let attribution = self
                    .turn_attribution
                    .get(&(thread_id.clone(), record.turn_id.clone()))
                    .or_else(|| self.thread_attribution.get(&thread_id))
                    .cloned()
                    .unwrap_or_default();
                let attribution = Attribution {
                    requested_model: record.requested_model.or(attribution.requested_model),
                    requested_service_tier: record
                        .requested_service_tier
                        .or(attribution.requested_service_tier),
                };
                let response_id = record.response_id;
                let turn_id = record.turn_id;
                let thread_total = UsageValues::from(&record.thread_token_usage);
                let mut usage = usage_record(
                    timestamp,
                    thread_id.clone(),
                    Some(response_id.clone()),
                    attribution,
                    record.usage,
                    UsageSource::Exact,
                );
                usage.account_id = record.account_id;
                usage.reported_model = record.reported_model;
                usage.reported_service_tier = record.reported_service_tier;
                self.insert_exact(
                    ExactKey {
                        thread_id,
                        turn_id,
                        response_id,
                    },
                    ExactUsage {
                        record: usage,
                        thread_total,
                    },
                );
            }
            MinimalRolloutItem::EventMsg {
                payload: MinimalEventMsg::TokenCount(event),
            } => {
                if let Some(rate_limits) = event.rate_limits {
                    let (limits, malformed) = historical_limits(
                        timestamp.as_str(),
                        current_thread_id.as_deref(),
                        rate_limits,
                    );
                    self.diagnostics.malformed_records += malformed;
                    for limit in limits {
                        if self.limits_seen.insert(limit.key) {
                            self.limits.push(limit.record);
                        }
                    }
                }
                let Some(info) = event.info else {
                    return;
                };
                if !valid_usage(&info.total_token_usage) || !valid_usage(&info.last_token_usage) {
                    self.diagnostics.malformed_records += 1;
                    return;
                }
                let delta = match previous_total.as_ref() {
                    Some(previous) => usage_delta(&info.total_token_usage, previous),
                    None if visible_usage_eq(&info.total_token_usage, &info.last_token_usage) => {
                        Some(info.last_token_usage.clone())
                    }
                    None => None,
                };
                let thread_total = UsageValues::from(&info.total_token_usage);
                *previous_total = Some(info.total_token_usage);
                if ordinal
                    .zip(*legacy_start_ordinal)
                    .is_some_and(|(ordinal, start)| ordinal < start)
                {
                    return;
                }
                let Some(delta) = delta.filter(has_usage) else {
                    return;
                };
                let thread_id = current_thread_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let attribution = self
                    .thread_attribution
                    .get(&thread_id)
                    .cloned()
                    .unwrap_or_default();
                let usage = usage_record(
                    timestamp.clone(),
                    thread_id,
                    None,
                    attribution,
                    delta,
                    UsageSource::LegacyApproximate,
                );
                let key = LegacyKey {
                    thread_id: usage.thread_id.clone(),
                    timestamp,
                    usage: UsageValues::from(&usage),
                };
                if self.legacy_seen.insert(key) {
                    self.legacy.push(LegacyUsage {
                        record: usage,
                        thread_total,
                    });
                } else {
                    self.diagnostics.duplicate_usage_records += 1;
                }
            }
            MinimalRolloutItem::EventMsg {
                payload: MinimalEventMsg::Other,
            }
            | MinimalRolloutItem::Other => {}
        }
    }

    fn insert_exact(&mut self, key: ExactKey, usage: ExactUsage) {
        let Some(existing) = self.exact.get_mut(&key) else {
            self.exact.insert(key, usage);
            return;
        };
        if UsageValues::from(&existing.record) == UsageValues::from(&usage.record) {
            self.diagnostics.duplicate_usage_records += 1;
            merge_attribution(&mut existing.record, &usage.record);
            if usage.record.timestamp < existing.record.timestamp {
                existing.record.timestamp = usage.record.timestamp;
            }
            return;
        }
        self.diagnostics.conflicting_usage_records += 1;
        if usage.record.timestamp < existing.record.timestamp {
            *existing = usage;
        }
    }

    fn finish(mut self) -> ReportData {
        let exact_overlap = exact_overlap_index(self.exact.values());
        self.legacy.retain(|usage| {
            !exact_overlap.contains(&(
                usage.record.thread_id.clone(),
                UsageValues::from(&usage.record),
                usage.thread_total.clone(),
            ))
        });
        let mut usage = self
            .exact
            .into_values()
            .map(|usage| usage.record)
            .collect::<Vec<_>>();
        usage.extend(self.legacy.into_iter().map(|usage| usage.record));
        usage.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.thread_id.cmp(&right.thread_id))
                .then_with(|| left.response_id.cmp(&right.response_id))
        });
        self.diagnostics.exact_usage_records = usage
            .iter()
            .filter(|record| record.source == UsageSource::Exact)
            .count() as u64;
        self.diagnostics.legacy_usage_records = usage
            .iter()
            .filter(|record| record.source == UsageSource::LegacyApproximate)
            .count() as u64;
        self.limits.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.limit_id.cmp(&right.limit_id))
                .then_with(|| left.window_seconds.cmp(&right.window_seconds))
        });
        ReportData {
            schema_version: REPORT_SCHEMA_VERSION,
            generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            usage,
            limits: self.limits,
            resets: Vec::new(),
            diagnostics: self.diagnostics,
        }
    }
}

/// Extracts normalized telemetry from every active and archived rollout under a Codex home.
pub fn extract_report_data(codex_home: &Path) -> ReportData {
    let mut extraction = Extraction::new();
    let (paths, discovery_errors) = discover_rollouts(codex_home);
    extraction.diagnostics.files_discovered = paths.len() as u64;
    extraction.diagnostics.discovery_errors = discovery_errors;
    for path in paths {
        extraction.scan_file(path.as_path());
    }
    extraction.finish()
}

fn usage_record(
    timestamp: String,
    thread_id: String,
    response_id: Option<String>,
    attribution: Attribution,
    usage: TokenUsage,
    source: UsageSource,
) -> UsageRecord {
    UsageRecord {
        timestamp,
        thread_id,
        response_id,
        account_id: None,
        requested_model: attribution.requested_model,
        requested_service_tier: attribution.requested_service_tier,
        reported_model: None,
        reported_service_tier: None,
        input_tokens: usage.input_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_write_input_tokens: usage.cache_write_input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        source,
    }
}

fn merge_attribution(target: &mut UsageRecord, candidate: &UsageRecord) {
    if target.account_id.is_none() {
        target.account_id.clone_from(&candidate.account_id);
    }
    if target.requested_model.is_none() {
        target
            .requested_model
            .clone_from(&candidate.requested_model);
    }
    if target.requested_service_tier.is_none() {
        target
            .requested_service_tier
            .clone_from(&candidate.requested_service_tier);
    }
    if target.reported_model.is_none() {
        target.reported_model.clone_from(&candidate.reported_model);
    }
    if target.reported_service_tier.is_none() {
        target
            .reported_service_tier
            .clone_from(&candidate.reported_service_tier);
    }
}

fn exact_overlap_index<'a>(
    exact: impl Iterator<Item = &'a ExactUsage>,
) -> HashSet<(String, UsageValues, UsageValues)> {
    let mut index = HashSet::new();
    for usage in exact {
        index.insert((
            usage.record.thread_id.clone(),
            UsageValues::from(&usage.record),
            usage.thread_total.clone(),
        ));
    }
    index
}

fn normalize_timestamp(timestamp: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|timestamp| {
            timestamp
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        })
}
