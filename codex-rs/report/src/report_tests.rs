use std::io::Write;
use std::path::Path;

use chrono::DateTime;
use chrono::Utc;
use codex_state::AccountLimitObservation;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_state::UsageResetEvent;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

use super::*;

fn usage(input: i64, cached: i64, output: i64) -> Value {
    json!({
        "input_tokens": input,
        "cached_input_tokens": cached,
        "cache_write_input_tokens": 0,
        "output_tokens": output,
        "reasoning_output_tokens": 0,
        "total_tokens": input + output,
    })
}

fn line(timestamp: &str, kind: &str, payload: Value) -> String {
    serde_json::to_string(&json!({
        "timestamp": timestamp,
        "type": kind,
        "payload": payload,
    }))
    .expect("serialize fixture")
}

fn write_rollout(home: &Path, relative: &str, lines: &[String]) {
    let path = home.join(relative);
    std::fs::create_dir_all(path.parent().expect("rollout parent")).expect("create parent");
    std::fs::write(path, lines.join("\n")).expect("write rollout");
}

fn normalized_typescript(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn exact_record(input: i64) -> Value {
    json!({
        "thread_id": "thread-a",
        "turn_id": "turn-a",
        "session_id": "thread-a",
        "root_turn_id": "turn-a",
        "response_id": "response-a",
        "usage": usage(input, 3, 2),
        "turn_token_usage": usage(input, 3, 2),
        "thread_token_usage": usage(input, 3, 2),
        "account_id": "account-a",
        "requested_model": "gpt-requested",
        "requested_service_tier": "flex",
        "reported_model": "gpt-reported",
        "reported_service_tier": "priority",
    })
}

fn exact_record_for_turn(input: i64, turn_id: &str) -> Value {
    let mut record = exact_record(input);
    record["turn_id"] = Value::String(turn_id.to_string());
    record
}

#[test]
fn exact_records_deduplicate_copied_history_and_report_conflicts() {
    let home = tempfile::tempdir().expect("tempdir");
    let original = vec![
        line(
            "2026-08-01T12:00:00Z",
            "session_meta",
            json!({"id": "thread-a"}),
        ),
        line(
            "2026-08-01T12:00:01Z",
            "token_usage_record",
            exact_record(10),
        ),
    ];
    write_rollout(
        home.path(),
        "sessions/2026/08/01/rollout-original.jsonl",
        &original,
    );
    write_rollout(
        home.path(),
        "archived_sessions/nested/rollout-copy.jsonl",
        &original,
    );
    write_rollout(
        home.path(),
        "sessions/2026/08/01/rollout-conflict.jsonl",
        &[
            line(
                "2026-08-01T11:59:00Z",
                "session_meta",
                json!({"id": "thread-a"}),
            ),
            line(
                "2026-08-01T11:59:01Z",
                "token_usage_record",
                exact_record(11),
            ),
        ],
    );

    let report = extract_report_data(home.path());

    assert_eq!(
        report.usage,
        vec![UsageRecord {
            timestamp: "2026-08-01T11:59:01.000Z".to_string(),
            thread_id: "thread-a".to_string(),
            response_id: Some("response-a".to_string()),
            account_id: Some("account-a".to_string()),
            requested_model: Some("gpt-requested".to_string()),
            requested_service_tier: Some("flex".to_string()),
            reported_model: Some("gpt-reported".to_string()),
            reported_service_tier: Some("priority".to_string()),
            input_tokens: 11,
            cached_input_tokens: 3,
            cache_write_input_tokens: 0,
            output_tokens: 2,
            reasoning_output_tokens: 0,
            source: UsageSource::Exact,
        }]
    );
    assert_eq!(report.diagnostics.files_discovered, 3);
    assert_eq!(report.diagnostics.duplicate_usage_records, 0);
    assert_eq!(report.diagnostics.conflicting_usage_records, 2);
    assert_eq!(report.diagnostics.exact_usage_records, 1);
}

#[test]
fn reused_response_id_in_distinct_turns_is_not_deduplicated() {
    let home = tempfile::tempdir().expect("tempdir");
    write_rollout(
        home.path(),
        "sessions/2026/08/01/reused-response.jsonl",
        &[
            line(
                "2026-08-01T12:00:00Z",
                "session_meta",
                json!({"id": "thread-a"}),
            ),
            line(
                "2026-08-01T12:00:01Z",
                "token_usage_record",
                exact_record_for_turn(10, "turn-a"),
            ),
            line(
                "2026-08-01T12:00:02Z",
                "token_usage_record",
                exact_record_for_turn(10, "turn-b"),
            ),
        ],
    );

    let report = extract_report_data(home.path());

    assert_eq!(report.usage.len(), 2);
    assert_eq!(report.diagnostics.duplicate_usage_records, 0);
    assert_eq!(report.diagnostics.exact_usage_records, 2);
}

#[test]
fn invalid_token_subsets_are_skipped_without_poisoning_valid_usage() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut invalid = exact_record_for_turn(10, "turn-invalid");
    invalid["response_id"] = json!("invalid-response");
    invalid["usage"]["cached_input_tokens"] = json!(11);
    let valid = exact_record_for_turn(10, "turn-valid");
    write_rollout(
        home.path(),
        "sessions/2026/08/01/token-subsets.jsonl",
        &[
            line(
                "2026-08-01T12:00:00Z",
                "session_meta",
                json!({"id": "thread-a"}),
            ),
            line("2026-08-01T12:00:01Z", "token_usage_record", invalid),
            line("2026-08-01T12:00:02Z", "token_usage_record", valid),
        ],
    );

    let report = extract_report_data(home.path());

    assert_eq!(report.usage.len(), 1);
    assert_eq!(report.diagnostics.malformed_records, 1);
}

#[test]
fn legacy_reconstruction_skips_inherited_baseline_and_keeps_unrelated_threads() {
    let home = tempfile::tempdir().expect("tempdir");
    for thread in ["thread-a", "thread-b"] {
        write_rollout(
            home.path(),
            format!("sessions/2026/08/01/{thread}.jsonl").as_str(),
            &[
                line(
                    "2026-08-01T12:00:00Z",
                    "session_meta",
                    json!({"id": thread}),
                ),
                line(
                    "2026-08-01T12:00:01Z",
                    "event_msg",
                    json!({
                        "type": "token_count",
                        "info": {
                            "total_token_usage": usage(100, 20, 10),
                            "last_token_usage": usage(10, 2, 1),
                            "model_context_window": 1000,
                        },
                        "rate_limits": null,
                    }),
                ),
                line(
                    "2026-08-01T12:00:02Z",
                    "event_msg",
                    json!({
                        "type": "token_count",
                        "info": {
                            "total_token_usage": usage(107, 22, 13),
                            "last_token_usage": usage(7, 2, 3),
                            "model_context_window": 1000,
                        },
                        "rate_limits": null,
                    }),
                ),
            ],
        );
    }

    let report = extract_report_data(home.path());

    assert_eq!(report.usage.len(), 2);
    assert!(report.usage.iter().all(|record| {
        record.input_tokens == 7
            && record.output_tokens == 3
            && record.source == UsageSource::LegacyApproximate
    }));
    assert_eq!(report.diagnostics.legacy_usage_records, 2);
}

#[test]
fn forked_legacy_history_before_ordinal_boundary_is_not_counted() {
    let home = tempfile::tempdir().expect("tempdir");
    write_rollout(
        home.path(),
        "sessions/2026/08/01/fork.jsonl",
        &[
            serde_json::to_string(&json!({
                "timestamp": "2026-08-01T12:00:00Z",
                "ordinal": 0,
                "type": "session_meta",
                "payload": {
                    "id": "fork-thread",
                    "forked_from_id": "parent-thread",
                    "forked_from_ordinal_exclusive": 3,
                },
            }))
            .expect("metadata"),
            serde_json::to_string(&json!({
                "timestamp": "2026-08-01T12:00:01Z",
                "ordinal": 1,
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": usage(10, 2, 1),
                        "last_token_usage": usage(10, 2, 1),
                    },
                    "rate_limits": null,
                },
            }))
            .expect("inherited usage"),
            serde_json::to_string(&json!({
                "timestamp": "2026-08-01T12:00:02Z",
                "ordinal": 3,
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": usage(15, 3, 3),
                        "last_token_usage": usage(5, 1, 2),
                    },
                    "rate_limits": null,
                },
            }))
            .expect("owned usage"),
        ],
    );

    let report = extract_report_data(home.path());

    assert_eq!(report.usage.len(), 1);
    assert_eq!(report.usage[0].input_tokens, 5);
    assert_eq!(report.usage[0].thread_id, "fork-thread");
}

#[test]
fn historical_rate_limit_snapshots_are_recovered_without_account_inference() {
    let home = tempfile::tempdir().expect("tempdir");
    let rate_limit_line = line(
        "2026-08-01T12:00:01Z",
        "event_msg",
        json!({
            "type": "token_count",
            "info": null,
            "rate_limits": {
                "limit_id": "codex",
                "primary": {
                    "used_percent": 42.0,
                    "window_minutes": 300,
                    "resets_at": 1_786_104_000i64,
                },
                "secondary": null,
                "credits": null,
                "plan_type": null,
            },
        }),
    );
    serde_json::from_str::<super::rollout_wire::MinimalRolloutLine>(&rate_limit_line)
        .expect("rate-limit rollout line should deserialize");
    write_rollout(
        home.path(),
        "sessions/2026/08/01/limits.jsonl",
        &[
            line(
                "2026-08-01T12:00:00Z",
                "session_meta",
                json!({"id": "thread-a"}),
            ),
            rate_limit_line,
        ],
    );

    let report = extract_report_data(home.path());

    assert_eq!(
        report.limits,
        vec![LimitRecord {
            timestamp: "2026-08-01T12:00:01.000Z".to_string(),
            account_id: "unknown".to_string(),
            limit_id: "codex".to_string(),
            window_seconds: Some(18_000),
            used_percent: Some(42.0),
            resets_at: Some("2026-08-07T12:00:00.000Z".to_string()),
            source: "rollout".to_string(),
        }]
    );
}

#[test]
fn exact_overlap_does_not_remove_another_threads_legacy_usage() {
    let home = tempfile::tempdir().expect("tempdir");
    write_rollout(
        home.path(),
        "sessions/2026/08/01/exact.jsonl",
        &[
            line(
                "2026-08-01T12:00:00Z",
                "session_meta",
                json!({"id": "thread-a"}),
            ),
            line(
                "2026-08-01T12:00:02Z",
                "token_usage_record",
                exact_record(7),
            ),
        ],
    );
    write_rollout(
        home.path(),
        "sessions/2026/08/01/legacy.jsonl",
        &[
            line(
                "2026-08-01T12:00:00Z",
                "session_meta",
                json!({"id": "thread-b"}),
            ),
            line(
                "2026-08-01T12:00:02Z",
                "event_msg",
                json!({
                    "type": "token_count",
                    "info": {
                        "total_token_usage": usage(7, 3, 2),
                        "last_token_usage": usage(7, 3, 2),
                    },
                    "rate_limits": null,
                }),
            ),
        ],
    );

    let report = extract_report_data(home.path());

    assert_eq!(report.usage.len(), 2);
    assert_eq!(report.diagnostics.exact_usage_records, 1);
    assert_eq!(report.diagnostics.legacy_usage_records, 1);
}

#[test]
fn compressed_and_malformed_rollouts_degrade_without_losing_valid_records() {
    let home = tempfile::tempdir().expect("tempdir");
    let path = home
        .path()
        .join("archived_sessions/2026/08/01/rollout-a.jsonl.zst");
    std::fs::create_dir_all(path.parent().expect("rollout parent")).expect("create parent");
    let file = std::fs::File::create(path).expect("create compressed rollout");
    let mut encoder = zstd::stream::write::Encoder::new(file, /*level*/ 1).expect("encoder");
    writeln!(encoder, "not-json").expect("write malformed record");
    writeln!(
        encoder,
        "{}",
        line(
            "2026-08-01T12:00:00Z",
            "session_meta",
            json!({"id": "thread-a"}),
        )
    )
    .expect("write metadata");
    writeln!(
        encoder,
        "{}",
        line(
            "2026-08-01T12:00:01Z",
            "token_usage_record",
            exact_record(10),
        )
    )
    .expect("write usage");
    encoder.finish().expect("finish compressed rollout");

    let report = extract_report_data(home.path());

    assert_eq!(report.usage.len(), 1);
    assert_eq!(report.diagnostics.files_scanned, 1);
    assert_eq!(report.diagnostics.malformed_records, 1);
}

#[test]
fn oversized_irrelevant_record_is_bounded_and_following_usage_is_read() {
    let home = tempfile::tempdir().expect("tempdir");
    let oversized = format!(
        "{{\"timestamp\":\"2026-08-01T12:00:00Z\",\"type\":\"response_item\",\"payload\":{{\"text\":\"{}\"}}}}",
        "x".repeat(2 * 1024 * 1024)
    );
    write_rollout(
        home.path(),
        "sessions/2026/08/01/oversized.jsonl",
        &[
            oversized,
            line(
                "2026-08-01T12:00:01Z",
                "session_meta",
                json!({"id": "thread-a"}),
            ),
            line(
                "2026-08-01T12:00:02Z",
                "token_usage_record",
                exact_record(10),
            ),
        ],
    );

    let report = extract_report_data(home.path());

    assert_eq!(report.usage.len(), 1);
    assert_eq!(report.diagnostics.oversized_records, 1);
    assert_eq!(report.diagnostics.malformed_records, 0);
}

#[test]
fn html_embedding_is_offline_and_cannot_close_the_data_script() {
    let mut report = extract_report_data(tempfile::tempdir().expect("tempdir").path());
    report.resets.push(ResetRecord {
        timestamp: "2026-08-01T12:00:00.000Z".to_string(),
        account_id: "</script><script>alert(1)</script>".to_string(),
        idempotency_key: None,
        limit_id: None,
    });

    let html = render_report_html(&report).expect("render report");

    assert!(html.contains("id=\"report-data\" type=\"application/json\""));
    assert!(html.contains("\\u003c/script\\u003e"));
    assert!(!html.contains("<script src="));
    assert!(!html.contains("<link rel=\"stylesheet\""));
    assert_eq!(html.matches("</script>").count(), 2);
    assert_eq!(html.matches("</html>").count(), 1);
}

#[test]
fn checked_in_typescript_contract_matches_rust_types() {
    assert_eq!(
        normalized_typescript(include_str!("../assets/report-contract.ts")),
        normalized_typescript(&typescript_contract()),
    );
}

#[tokio::test]
async fn generated_report_is_retained_after_generation_returns() {
    let home = tempfile::tempdir().expect("tempdir");

    let report_path = generate_report(home.path()).await.expect("generate report");

    assert_eq!(
        report_path.file_name().and_then(|name| name.to_str()),
        Some("report.html")
    );
    assert!(report_path.is_file());
}

#[tokio::test]
async fn account_telemetry_is_mapped_into_the_report_contract() {
    let home = tempfile::tempdir().expect("tempdir");
    let sqlite = SqliteConfig::new_for_testing(
        AbsolutePathBuf::from_absolute_path(home.path()).expect("absolute home"),
    );
    let runtime = StateRuntime::init(sqlite, "test-provider".to_string())
        .await
        .expect("initialize state");
    let captured_at = DateTime::parse_from_rfc3339("2026-08-01T12:00:00Z")
        .expect("timestamp")
        .with_timezone(&Utc);
    runtime
        .record_account_limit_observations(&[AccountLimitObservation {
            captured_at,
            account_id: "account-a".to_string(),
            limit_id: "weekly".to_string(),
            window_seconds: Some(604_800),
            used_percent: Some(42.5),
            resets_at: None,
            source: "poll".to_string(),
        }])
        .await
        .expect("record limit");
    runtime
        .record_usage_reset_event(&UsageResetEvent {
            consumed_at: captured_at,
            account_id: "account-a".to_string(),
            idempotency_key: Some("reset-a".to_string()),
            limit_id: Some("weekly".to_string()),
        })
        .await
        .expect("record reset");

    let telemetry = read_account_telemetry(home.path())
        .await
        .expect("read telemetry");

    assert_eq!(
        telemetry,
        Some((
            vec![LimitRecord {
                timestamp: "2026-08-01T12:00:00.000Z".to_string(),
                account_id: "account-a".to_string(),
                limit_id: "weekly".to_string(),
                window_seconds: Some(604_800),
                used_percent: Some(42.5),
                resets_at: None,
                source: "poll".to_string(),
            }],
            vec![ResetRecord {
                timestamp: "2026-08-01T12:00:00.000Z".to_string(),
                account_id: "account-a".to_string(),
                idempotency_key: Some("reset-a".to_string()),
                limit_id: Some("weekly".to_string()),
            }],
        ))
    );
}
