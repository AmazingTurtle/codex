#!/usr/bin/env python3
"""Run retained-feature regressions once, plus full crate tests for recovery edits."""

import argparse
import json
import os
from pathlib import Path
import subprocess
from environment import cargo_env, test_helpers


FEATURES = {
    "distribution": {
        "codex-cli": "test(/import_codex_state|update|cli_help|doctor|exec_server_help|strict_config_is_not_supported|sandbox_fetches_and_enforces/)",
        "codex-utils-home-dir": "all()",
        "codex-rmcp-client": "test(/credential|oauth/)",
    },
    "multi-account": {
        "codex-app-server-protocol": "test(schema_fixtures_tests::)",
        "codex-login": "test(/account|rotation|round_robin/)",
        "codex-core": "test(/account|round_robin|usage_limit|session_follows/)",
        "codex-app-server": "test(/account|rate_limit/)",
        "codex-tui": "test(/account|usage|status/)",
    },
    "activity-display": {"codex-tui": "test(/exec_cell|history_cell|activity/)"},
    "opaque-request-retry": {"codex-api": "test(/bad_request|opaque/)"},
    "test-file-mcp-import": {"codex-core": "test(openai_file_mcp)"},
    "test-terminal-capture": {"codex-tui": "test(/terminal/)"},
    "test-pty-path-lookup": {"codex-utils-pty": "all()"},
    "bash-snapshot": {"codex-exec-server": "test(/snapshot/)"},
    "release-maintenance": {},
    "telemetry-schema-compat": {
        "codex-state": "test(/migration/)",
        "codex-app-server-protocol": "test(schema_fixtures_tests::)",
    },
    "debloat-policy": {
        "codex-config": "test(/debloat/)",
        "codex-core": "test(/debloat|skills|mcp/)",
        "codex-cli": "test(/debloat|plugin/)",
    },
    "review-capacity-reset": {"codex-tui": "test(/review/)"},
    "background-exec-completion": {"codex-core": "test(unified_exec_completion::)"},
    "test-core-fixtures": {"codex-core": "test(/hooks|sandbox/)"},
    "reasoning-display": {
        "codex-tui": "test(/reasoning|status_indicator/)",
        "codex-exec": "test(/reasoning/)",
    },
    "runtime-wake-state": {
        "codex-app-server-protocol": "test(schema_fixtures_tests::)",
        "codex-core": "test(wake_state::)",
        "codex-tui": "test(/wake|waiting/)",
    },
    "test-voice-capture": {},
    "long-context": {
        "codex-core": "test(long_context::)",
        "codex-tui": "test(long_context)",
    },
    "subagent-profiles": {
        "codex-core": "test(/subagent_profiles|agent_role|message_tests|spawn_agent_description|spawn_settings|multi_agent_resume|models_cache_ttl/)",
        "codex-app-server": "test(cached_executor_mcp_cannot_read_host_token_after_capability_downgrade)",
    },
    "provider-response-compat": {"codex-core": "test(client_compat::)"},
    "approval-preferences": {
        "codex-tui": "test(projectless)",
        "codex-core": "test(auto_review::require_escalated_bypasses_extension_approval_and_runs_guardian)",
    },
    "security-setup-dismissal": {"codex-tui": "test(security_setup)"},
    "conversation-pause": {
        "codex-core": "test(/conversation_pause::|conversation_activity::|execution_control::tests::|stopped_spawn_leaves_no_child|pause_before_guardian_finalization|active_turn_keeps_first_root_when_mail_coalesces|async_hook_finishing_while_idle_waits_for_the_next_turn|idle_response_items_include_pending_mailbox_in_first_request|queued_inter_agent_mail_does_not_restart_after_final_answer|idle_eviction_preserves_worker_context_for_followup|evicted_role_subagent_uses_root_service_tier_after_reload/)",
        "codex-thread-store": "test(persisted_input)",
        "codex-app-server-protocol": "test(schema_fixtures_tests::)",
        "codex-app-server": "test(/v2::execution_control::|assigned_forks_inherit_projects_for_persistent_and_ephemeral_children|project_import_is_atomic_and_notifies_after_commit_in_order/)",
        "codex-tui": "test(/center_pause_requests_parent_control|execution_and_modal_pauses|paused_activity_retains_clock|execution_commands_follow_authoritative_availability|execution_snapshots_do_not_repeat_transition_notices|idle_execution_commands_preserve_draft|paused_notice_keeps_human_submission|workflow_pause_confirms_parent_scope/)",
    },
    "dynamic-workflows": {
        "codex-dynamic-workflows": "all()",
        "codex-code-mode": "all()",
        "codex-code-mode-host": "all()",
        "codex-code-mode-protocol": "all()",
        "codex-code-mode-runtime": "binary(workflow_scopes)",
        "codex-config": "test(workflow_limits_accept_boundaries_and_reject_invalid_values)",
        "codex-skills-extension": "test(upgrading_workflow_skill_cache_preserves_user_skills_and_retires_the_old_entry)",
        "codex-state": "test(workflow_origin_upgrade_preserves_history_and_checkpoint_and_accepts_older_writers)",
        "codex-thread-store": (
            "test(/persisted_input|relation_listing_reports_only_an_absent_optional_index_as_unsupported|"
            "workflow_timeline|legacy_timeline|"
            "revert_keeps_thread_id_and_hides_suffix_across_repeated_reverts|"
            "replacement_ancestor_owner_is_bound_to_its_canonical_filename|"
            "copied_workflow_origins_survive_cold_incremental_projection_and_reject_later_foreign_facts/)"
        ),
        "codex-features": "test(dynamic_workflows)",
        "codex-protocol": "test(/workflow_overrides|workflow::tests::/)",
        "codex-rollout": "test(workflow_lifecycle_is_durable_without_becoming_a_response_item)",
        "codex-core": (
            "test(/dynamic_workflows|workflow_policy_refreshes|workflows::|workflow_v2::|"
            "workflow_lifecycle::|workflow_completion::|workflow_observation::|workflow_notice_ids::|"
            "workflow_conversation::|workflow_revision::|workflow_revision_policy::|"
            "workflow_control_tools::|workflow_targeted_interrupt::|workflow_task_transport::|"
            "session::token_usage_observer::tests::|execution_control::clock::tests::|"
            "load_config_resolves_workflow_resource_limits|"
            "workflow_limits_refresh_for_future_runs_without_changing_launch_mode|"
            "launch_arguments_preserve_explicit_null_and_legacy_input|"
            "catalog_expansion_falls_back_to_the_complete_bundled_definition|"
            "a_bounded_catalog_override_retains_its_schema_and_required_arguments/) | "
            "test(/^(?:"
            "suite::compact::paginated_compaction_cold_resume_from_bounded_suffix|"
            "suite::scenarios::agent_message_board::board_post_and_reads_reach_model_context_without_self_notices::disk|"
            "suite::scenarios::agent_message_board::board_post_and_reads_reach_model_context_without_self_notices::in_memory|"
            "suite::scenarios::astra_asks_an_async_question_and_receives_the_answer_while_working|"
            "suite::scenarios::astra_async_compaction_keeps_work_added_after_its_snapshot::newer_user|"
            "suite::scenarios::astra_continues_after_a_stream_is_interrupted|"
            "suite::scenarios::astra_continues_after_input_yields_a_code_mode_cell|"
            "suite::scenarios::astra_reads_code_mode_call_timing|"
            "suite::scenarios::astra_settings_release_check_with_direct_and_code_mode_tools|"
            "suite::scenarios::astra_switches_environments_for_the_rest_of_the_active_turn|"
            "suite::scenarios::child_completion_wakes_parent_after_final_answer|"
            "suite::scenarios::code_mode_catalog_messages::catalog|"
            "suite::scenarios::code_mode_catalog_messages::ranked_search|"
            "suite::scenarios::code_mode_settled_helpers::code_mode_settled_helpers_report_successes_and_failures|"
            "suite::scenarios::compaction::remote_slash_compact_preserves_context_across_windows|"
            "suite::scenarios::guardian_code_mode_messaging_request_history|"
            "suite::scenarios::indirect_namespace_prefixes::catalog_namespace_prefixes_follow_the_selected_model::code_mode_only|"
            "suite::scenarios::multi_agent_catalog_parameters::native_tools|"
            "suite::scenarios::partial_answers::partial_answers_survive_forked_child_requests::filtered|"
            "suite::scenarios::partial_answers::partial_answers_survive_forked_child_requests::preserved_legacy|"
            "suite::scenarios::partial_answers::partial_answers_survive_forked_child_requests::preserved_paginated|"
            "suite::scenarios::prediction_fork_preserves_context_above_compaction_threshold|"
            "suite::scenarios::provider_capabilities::custom_provider_capabilities_constrain_web_access_and_enable_compaction|"
            "suite::scenarios::retained_tool_outputs::retained_tool_output_survives_compaction_and_resume|"
            "suite::scenarios::skill_catalog_dedup::cloud_preference_preserves_executor_aliases_and_description_budget|"
            "suite::scenarios::skill_read_recovers_omitted_installation_and_reads_selected_environment|"
            "suite::scenarios::subagent_waits_for_its_inherited_environment_configuration"
            ")$/)"
        ),
        "codex-app-server-protocol": "test(/schema_fixtures_tests::|export::workflow_tests::|workflow_presentation::tests::/)",
        "codex-app-server-client": "test(tests::replies::)",
        "codex-app-server-transport": "test(transport::remote_control::segment_tests::)",
        "codex-app-server": (
            "test(/v2::workflows::|v2::workflow_transcript::|v2::workflow_revision::|"
            "v2::thread_fork::inheriting_fork_preserves_parent_config::|"
            "workflow_presentation::tests::|thread_state::live::tests::|"
            "workflow_processor::transcript::live_snapshot::tests::|"
            "workflow_final_presentation_requires_experimental_negotiation|"
            "stable_history_notifications_preserve_opt_out_and_envelope_metadata|"
            "request_processors::thread_lifecycle::workflow_resync_tests::/)"
        ),
        "codex-tui": (
            "test(/workflows::tests::|workflows::projection::tests::|workflows::strip::tests::|"
            "workflow_integration_tests|workflow_transcript_tests|workflow_strip::tests|"
            "workflow_child_input_tests|workflow_saved_integration_tests|saved_commands::tests|"
            "saved_workflow_tests|workflows::actions::tests::|workflow_history::|"
            "app::thread_timeline_cache::tests::|structured_value::|"
            "history_cell::workflow::|history_cell::workflow_message::tests::|"
            "chatwidget::workflow_final::tests::|workflows::detail_viewport::chat_tests::|"
            "workflow_overview_tests::|app::history_pagination::workflow::tests::|"
            "prompt_edit_reverts_earlier_and_first_visible_prompts_in_place|"
            "suite::worktree_stack::picker_side_worktree_fork_and_cd_run_on_the_production_stack|"
            "app::tests::workflow_actions::footer_visibility_keeps_a_run_inspectable_and_restorable_without_restarting|"
            "app::thread_events::tests::refresh_retains_live_questions_without_replaying_their_text|"
            "snapshot_keeps_typed_output_before_voice_handoff_in_the_same_turn/)"
        ),
    },
}


def selection(patches: list[str], registry: dict = FEATURES) -> dict[str, str]:
    selected: dict[str, set[str]] = {}
    for owner in patches:
        if owner not in registry:
            raise RuntimeError(f"No regression coverage mapping for patch '{owner}'")
        for crate, expression in registry[owner].items():
            selected.setdefault(crate, set()).add(expression)
    return {crate: " | ".join(sorted(filters)) for crate, filters in selected.items()}


def coverage_count(listing: dict, label: str) -> int:
    count = sum(
        1
        for suite in listing["rust-suites"].values()
        for case in suite.get("testcases", {}).values()
        if not case["ignored"] and case["filter-match"]["status"] == "matches"
    )
    if not count:
        raise RuntimeError(f"No runnable regressions selected for {label}")
    return count


def selected_tests(state: dict) -> dict[str, str]:
    root = Path(state["repository"])
    selected = selection(state["patches"])
    recovery_crates = set(state.get("recovery_crates", []))
    for path in state.get("conflicts", []):
        for directory in (root / path).parents:
            manifest = directory / "Cargo.toml"
            if manifest.is_file() and directory != root / "codex-rs":
                import tomllib

                package = (
                    tomllib.loads(manifest.read_text()).get("package", {}).get("name")
                )
                if package is not None:
                    recovery_crates.add(package)
                    break
            if directory == root:
                break
    for crate in recovery_crates:
        selected[crate] = "all()"
    return selected


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--cpus", type=int, default=6)
    parser.add_argument("--scope-only", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.cpus <= 6:
        parser.error("CPU count must be between one and six")
    allowed = sorted(os.sched_getaffinity(0))[: args.cpus]
    os.sched_setaffinity(0, allowed)
    state = json.loads(args.state.read_text())
    selected = selected_tests(state)
    if args.scope_only:
        print(json.dumps(sorted(selected)))
        return
    root = Path(state["repository"])
    current = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True
    ).strip()
    if state.get("candidate_sha", current) != current:
        raise RuntimeError(
            "Candidate SHA changed; start validation for the new candidate"
        )
    env = {
        **cargo_env(root),
        "CODEX_REPO_ROOT": str(root),
        "CARGO_BUILD_JOBS": str(len(allowed)),
        "RUST_TEST_THREADS": str(len(allowed)),
        "RAYON_NUM_THREADS": str(len(allowed)),
    }
    has_v8 = "codex-v8-poc" in (root / "codex-rs/Cargo.toml").read_text()
    if "dynamic-workflows" in state["patches"]:
        # This sample has no runnable regressions. Link its actual binary as a
        # separate feature requirement instead of accepting an empty test filter.
        sample_build = [
            "cargo",
            "build",
            "-p",
            "codex-thread-manager-sample",
            "--bin",
            "codex-thread-manager-sample",
        ]
        if has_v8:
            sample_build.extend(
                ["-p", "codex-v8-poc", "--features", "codex-v8-poc/sandbox"]
            )
        subprocess.run(sample_build, cwd=root / "codex-rs", env=env, check=True)
        print(
            "Build coverage: dynamic-workflows/codex-thread-manager-sample: binary linked",
            flush=True,
        )
    # One invocation builds each selected crate once. Package-qualified filters
    # prevent a broad filter for one crate from expanding the others' test scope.
    expression = " | ".join(
        f"(package({crate}) & ({filter}))" for crate, filter in sorted(selected.items())
    )
    # Build once and check each owner's filter against the same binaries before
    # running their union. An unrelated match cannot conceal obsolete coverage.
    binaries = args.state.with_name("test-binaries.json")
    metadata = args.state.with_name("cargo-metadata.json")
    build = [
        "cargo",
        "nextest",
        "list",
        "--list-type",
        "binaries-only",
        "--message-format",
        "json",
    ]
    if has_v8:
        build.extend(["--features", "codex-v8-poc/sandbox"])
    if state.get("validation_policy") == "bootstrap":
        build.append("--workspace")
    else:
        for crate in sorted(selected):
            build.extend(["-p", crate])
        if has_v8:
            build.extend(["-p", "codex-v8-poc"])
    if "codex-core" in selected:
        test_helpers(root, env)
    with binaries.open("w") as output:
        subprocess.run(build, cwd=root / "codex-rs", env=env, stdout=output, check=True)
    with metadata.open("w") as output:
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1"],
            cwd=root / "codex-rs",
            env=env,
            stdout=output,
            check=True,
        )
    reuse = [
        "--binaries-metadata",
        str(binaries.resolve()),
        "--cargo-metadata",
        str(metadata.resolve()),
    ]
    for owner in state["patches"]:
        for crate, filter in FEATURES[owner].items():
            listing = subprocess.check_output(
                [
                    "cargo",
                    "nextest",
                    "list",
                    *reuse,
                    "--message-format",
                    "json",
                    "-E",
                    f"package({crate}) & ({filter})",
                ],
                cwd=root / "codex-rs",
                env=env,
                text=True,
            )
            count = coverage_count(json.loads(listing), f"{owner}/{crate}")
            print(f"Coverage: {owner}/{crate}: {count} runnable tests", flush=True)
    command = ["just", "test", *reuse]
    command.extend(
        ["-E", expression, "--no-tests=fail", "--test-threads", str(len(allowed))]
    )
    subprocess.run(command, cwd=root / "codex-rs", env=env, check=True)


if __name__ == "__main__":
    main()
