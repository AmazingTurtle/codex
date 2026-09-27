//! Reasoning activity retains the latest usable line through empty items and status-row replacement.

use super::*;
use pretty_assertions::assert_eq;

fn delta(chat: &mut ChatWidget, id: &str, text: &str) {
    chat.handle_server_notification(
        ServerNotification::ReasoningSummaryTextDelta(ReasoningSummaryTextDeltaNotification {
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            item_id: id.to_string(),
            delta: text.to_string(),
            summary_index: 0,
        }),
        /*replay_kind*/ None,
    );
}

fn complete(chat: &mut ChatWidget, id: &str) {
    chat.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: 0,
            item: AppServerThreadItem::Reasoning {
                id: id.to_string(),
                summary: Vec::new(),
                content: Vec::new(),
            },
        }),
        /*replay_kind*/ None,
    );
}

fn render_active(chat: &ChatWidget) -> String {
    let active = chat
        .transcript
        .active_cell
        .as_ref()
        .expect("active cell present");
    lines_to_single_string(&active.display_lines(/*width*/ 80))
}

fn render_status_row(chat: &ChatWidget) -> String {
    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status row remains visible while raw reasoning streams");
    let mut timer = crate::status_indicator_widget::StatusTimer::default();
    timer.reset(std::time::Duration::ZERO);
    timer.pause_at(std::time::Instant::now());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
        /*width*/ 80, /*height*/ 2,
    ))
    .expect("terminal");
    terminal
        .draw(|frame| {
            status
                .with_timer(&timer)
                .render(frame.area(), frame.buffer_mut());
        })
        .expect("render status row");
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(80)
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn raw_reasoning_preview_streams_one_labeled_cell_end_to_end() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.show_raw_agent_reasoning = true;
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.app.open_transcript = vec![crate::key_hint::ctrl(crossterm::event::KeyCode::Char('k'))];
    chat.bottom_pane.set_keymap_bindings(&keymap);
    chat.on_task_started();
    chat.flush_active_cell();
    drain_insert_history(&mut rx);
    handle_agent_reasoning_started(&mut chat, "reasoning");

    delta(
        &mut chat,
        "reasoning",
        "Line one of the plan.\nLine two of the plan.\n",
    );

    let live = render_active(&chat);
    assert!(
        live.starts_with("• Reasoning: Line one of the plan.\n"),
        "{live:?}"
    );
    assert!(live.contains("Line two of the plan."), "{live:?}");
    assert!(!live.contains("Line three"), "{live:?}");
    assert_eq!(
        chat.bottom_pane
            .status_widget()
            .map(|status| status.header().to_string()),
        Some("Working".to_string()),
        "the status row keeps the elapsed timer visible below the live preview"
    );
    assert_chatwidget_snapshot!(
        "raw_reasoning_preview_keeps_elapsed_timer_on_separate_row",
        format!("{live}{}", render_status_row(&chat))
    );

    delta(
        &mut chat,
        "reasoning",
        "Line three of the plan.\nLine four of the plan.",
    );

    let updated = render_active(&chat);
    assert!(
        updated.starts_with("• Reasoning: Line one of the plan.\n"),
        "{updated:?}"
    );
    assert!(
        updated.contains("2 more lines in transcript (ctrl+k)"),
        "{updated:?}"
    );

    complete(&mut chat, "reasoning");

    assert!(chat.transcript.active_cell.is_none());
    let committed = drain_insert_history(&mut rx);
    assert_eq!(committed.len(), 1);
    assert_eq!(lines_to_single_string(&committed[0]), updated);
}

#[tokio::test]
async fn raw_reasoning_preview_takeover_flushes_partial_without_duplicate_at_completion() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.show_raw_agent_reasoning = true;
    chat.on_task_started();
    chat.flush_active_cell();
    drain_insert_history(&mut rx);
    handle_agent_reasoning_started(&mut chat, "reasoning");

    delta(&mut chat, "reasoning", "Line one of the plan.");

    begin_exec(&mut chat, "cmd-1", "sleep 2");

    let flushed = drain_insert_history(&mut rx);
    let flushed_rendered: Vec<String> = flushed
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .filter(|text| text.contains("Reasoning"))
        .collect();
    assert_eq!(flushed_rendered.len(), 1, "{flushed:?}");
    assert!(
        flushed_rendered[0].contains("• Reasoning: Line one of the plan."),
        "{:?}",
        flushed_rendered[0]
    );

    complete(&mut chat, "reasoning");

    let late = drain_insert_history(&mut rx);
    assert!(
        late.iter()
            .all(|lines| !lines_to_single_string(lines).contains("Line one of the plan.")),
        "{late:?}"
    );
}

#[tokio::test]
async fn raw_reasoning_warning_flush_preserves_only_uncommitted_suffix() {
    for suffix in ["", " Later thought."] {
        let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.config.show_raw_agent_reasoning = true;
        chat.on_task_started();
        chat.flush_active_cell();
        drain_insert_history(&mut rx);
        handle_agent_reasoning_started(&mut chat, "reasoning");
        delta(&mut chat, "reasoning", "Earlier thought.");
        chat.add_to_history(history_cell::new_usage_warning_event(
            "Usage warning during reasoning.".to_string(),
        ));
        assert!(chat.transcript.active_cell.is_none());
        delta(&mut chat, "reasoning", suffix);
        chat.add_to_history(history_cell::new_usage_warning_event(
            "Another usage warning during reasoning.".to_string(),
        ));
        complete(&mut chat, "reasoning");
        assert!(chat.transcript.active_cell.is_none());

        let sources = std::cell::RefCell::new(Vec::new());
        drain_insert_history_with(&mut rx, |cell| {
            if let Some(reasoning) = cell
                .as_any()
                .downcast_ref::<history_cell::ReasoningSummaryCell>()
            {
                sources
                    .borrow_mut()
                    .push(reasoning.markdown_source().to_string());
            }
            cell.display_lines(/*width*/ 80)
        });
        let expected = if suffix.is_empty() {
            vec!["Earlier thought."]
        } else {
            vec!["Earlier thought.", "Later thought."]
        };
        assert_eq!(sources.into_inner(), expected);
    }
}

#[tokio::test]
async fn raw_reasoning_preview_takeover_preserves_content_received_after_flush() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.show_raw_agent_reasoning = true;
    chat.on_task_started();
    chat.flush_active_cell();
    drain_insert_history(&mut rx);
    handle_agent_reasoning_started(&mut chat, "reasoning");

    delta(&mut chat, "reasoning", "Earlier thought.");
    let exec = begin_exec(&mut chat, "cmd-1", "sleep 2");
    let flushed_sources = std::cell::RefCell::new(Vec::new());
    drain_insert_history_with(&mut rx, |cell| {
        if let Some(reasoning) = cell
            .as_any()
            .downcast_ref::<history_cell::ReasoningSummaryCell>()
        {
            flushed_sources
                .borrow_mut()
                .push(reasoning.markdown_source().to_string());
        }
        cell.display_lines(/*width*/ 80)
    });

    delta(&mut chat, "reasoning", " Final thought.");
    end_exec(&mut chat, exec, "", "", /*exit_code*/ 0);
    drain_insert_history(&mut rx);

    // When the exec cell releases the active slot, later deltas must not rebuild and reflush a
    // second preview containing the already-committed prefix.
    delta(&mut chat, "reasoning", " More after the tool.");
    assert!(chat.transcript.active_cell.is_none());
    complete(&mut chat, "reasoning");

    let late_sources = std::cell::RefCell::new(Vec::new());
    drain_insert_history_with(&mut rx, |cell| {
        if let Some(reasoning) = cell
            .as_any()
            .downcast_ref::<history_cell::ReasoningSummaryCell>()
        {
            late_sources
                .borrow_mut()
                .push(reasoning.markdown_source().to_string());
        }
        cell.display_lines(/*width*/ 80)
    });
    let combined_source = flushed_sources
        .into_inner()
        .into_iter()
        .chain(late_sources.into_inner())
        .collect::<Vec<_>>();
    assert_eq!(
        combined_source,
        vec![
            "Earlier thought.".to_string(),
            "Final thought. More after the tool.".to_string(),
        ]
    );
}

#[tokio::test]
async fn raw_reasoning_live_preview_is_bounded_but_completion_keeps_full_source() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.show_raw_agent_reasoning = true;
    chat.on_task_started();
    chat.flush_active_cell();
    drain_insert_history(&mut rx);
    handle_agent_reasoning_started(&mut chat, "reasoning");

    let full_source = format!(
        "{}Final reasoning line.",
        "A long reasoning line that keeps streaming. ".repeat(199)
    );
    delta(&mut chat, "reasoning", &full_source);

    let live = chat
        .transcript
        .active_cell
        .as_ref()
        .expect("bounded live preview")
        .as_any()
        .downcast_ref::<history_cell::ReasoningSummaryCell>()
        .expect("reasoning preview cell");
    assert!(live.markdown_source().len() <= 4 * 1024);
    assert!(live.live_preview_truncated());

    complete(&mut chat, "reasoning");

    let completed_source = std::cell::RefCell::new(None);
    drain_insert_history_with(&mut rx, |cell| {
        if let Some(reasoning) = cell
            .as_any()
            .downcast_ref::<history_cell::ReasoningSummaryCell>()
        {
            *completed_source.borrow_mut() = Some(reasoning.markdown_source().to_string());
        }
        cell.display_lines(/*width*/ 80)
    });
    let completed_source = completed_source.into_inner();
    assert_eq!(completed_source.as_deref(), Some(full_source.as_str()));
}

#[tokio::test]
async fn raw_reasoning_preview_is_dropped_when_the_turn_finalizes_midstream() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.show_raw_agent_reasoning = true;
    chat.on_task_started();
    chat.flush_active_cell();
    drain_insert_history(&mut rx);
    handle_agent_reasoning_started(&mut chat, "reasoning");

    delta(&mut chat, "reasoning", "Halfway there.");

    assert!(render_active(&chat).contains("Halfway there."));

    chat.finalize_turn();

    assert!(chat.transcript.active_cell.is_none());
    let drained = drain_insert_history(&mut rx);
    assert!(
        drained
            .iter()
            .all(|lines| !lines_to_single_string(lines).contains("Halfway there.")),
        "{drained:?}"
    );
}

#[tokio::test]
async fn raw_reasoning_preview_drops_stale_cell_when_a_new_item_starts() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.show_raw_agent_reasoning = true;
    chat.on_task_started();
    chat.flush_active_cell();
    drain_insert_history(&mut rx);
    handle_agent_reasoning_started(&mut chat, "first");

    delta(&mut chat, "first", "Stale item text.");
    assert!(render_active(&chat).contains("Stale item text."));

    handle_agent_reasoning_started(&mut chat, "next");

    assert!(chat.transcript.active_cell.is_none());
    let drained = drain_insert_history(&mut rx);
    assert!(
        drained
            .iter()
            .all(|lines| !lines_to_single_string(lines).contains("Stale item text.")),
        "{drained:?}"
    );
}
#[tokio::test]
async fn reasoning_status_accepts_bold_text_with_a_plain_suffix() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.on_task_started();
    handle_agent_reasoning_started(&mut chat, "reasoning");

    delta(&mut chat, "reasoning", "**Checking tests");
    let before_close = chat
        .bottom_pane
        .status_widget()
        .unwrap()
        .header()
        .to_string();
    delta(&mut chat, "reasoning", "**: running suite");
    let after_close = chat
        .bottom_pane
        .status_widget()
        .unwrap()
        .header()
        .to_string();

    insta::assert_debug_snapshot!(vec![before_close, after_close], @r###"
    [
        "Working",
        "Checking tests: running suite",
    ]
    "###);
}

#[tokio::test]
async fn reasoning_status_tracks_items_and_restores_after_tool_activity() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.on_task_started();
    let mut headers = Vec::new();
    let capture = |chat: &ChatWidget, headers: &mut Vec<String>| {
        headers.push(
            chat.bottom_pane
                .status_widget()
                .expect("running status")
                .header()
                .to_string(),
        );
    };

    handle_agent_reasoning_started(&mut chat, "first");
    delta(&mut chat, "first", "**Researching backend");
    capture(&chat, &mut headers);
    delta(&mut chat, "first", " freshness**");
    capture(&chat, &mut headers);
    complete(&mut chat, "first");
    delta(&mut chat, "first", "**Late old header**");
    capture(&chat, &mut headers);

    // A tool recreates the row after streamed commentary has hidden it.
    chat.bottom_pane.hide_status_indicator();
    begin_unified_exec_startup(&mut chat, "tool-1", "process-1", "sleep 2");
    capture(&chat, &mut headers);

    handle_agent_reasoning_started(&mut chat, "second");
    capture(&chat, &mut headers);
    chat.handle_server_notification(
        ServerNotification::ReasoningSummaryPartAdded(
            codex_app_server_protocol::ReasoningSummaryPartAddedNotification {
                thread_id: "thread-1".to_string(),
                turn_id: "turn-1".to_string(),
                item_id: "first".to_string(),
                summary_index: 1,
            },
        ),
        /*replay_kind*/ None,
    );
    complete(&mut chat, "first");
    delta(&mut chat, "first", "**Stale heading**");
    delta(&mut chat, "second", "No summary heading is available.");
    capture(&chat, &mut headers);
    delta(&mut chat, "second", "\n**Preparing evidence report**");
    capture(&chat, &mut headers);

    insta::assert_debug_snapshot!(headers);
    let mut timer = crate::status_indicator_widget::StatusTimer::default();
    timer.pause_at(std::time::Instant::now());
    timer.reset(std::time::Duration::from_secs(/*secs*/ 42));
    for width in [80, 40] {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, /*height*/ 2))
                .expect("terminal");
        terminal
            .draw(|frame| {
                chat.bottom_pane
                    .status_widget()
                    .expect("running status")
                    .with_timer(&timer)
                    .render(frame.area(), frame.buffer_mut());
            })
            .expect("render reasoning status");
        assert_chatwidget_snapshot!(
            format!("reasoning_activity_row_{width}"),
            terminal.backend()
        );
    }
    chat.on_interrupted_turn(TurnAbortReason::Interrupted);
    delta(&mut chat, "second", "**After interruption**");
    assert_eq!(chat.status_state.reasoning_item_id, None);
    assert_eq!(chat.reasoning_header, None);
    assert!(!chat.bottom_pane.status_indicator_visible());
    chat.on_task_started();
    assert_eq!(
        chat.bottom_pane.status_widget().unwrap().header(),
        "Working"
    );
}

#[tokio::test]
async fn voice_handoff_preserves_typed_reasoning_and_ignores_private_items() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.on_task_started();
    handle_agent_reasoning_started(&mut chat, "typed");
    delta(&mut chat, "typed", "**Checking repository**");

    chat.remember_realtime_delegated_reasoning_turn("turn-1");
    handle_agent_reasoning_started(&mut chat, "private");
    delta(&mut chat, "private", "**Private voice reasoning**");
    complete(&mut chat, "private");
    assert_eq!(
        chat.status_state.reasoning_item_id.as_deref(),
        Some("typed")
    );
    assert_eq!(
        chat.bottom_pane.status_widget().unwrap().header(),
        "Checking repository"
    );

    delta(&mut chat, "typed", "\n**Verifying changes**");
    assert_eq!(
        chat.bottom_pane.status_widget().unwrap().header(),
        "Verifying changes"
    );
    complete(&mut chat, "typed");
    assert_eq!(chat.status_state.reasoning_item_id, None);
}

#[tokio::test]
async fn reasoning_status_preserves_an_explicit_wait_and_restores_the_heading() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.on_task_started();
    chat.status_state
        .pending_guardian_review_status
        .start_or_update("review-1".to_string(), "Checking a command".to_string());
    chat.set_status_header("Reviewing approval request".to_string());
    handle_agent_reasoning_started(&mut chat, "first");
    delta(&mut chat, "first", "**Choosing the missing-data fix**");
    complete(&mut chat, "first");
    assert_eq!(
        chat.bottom_pane.status_widget().unwrap().header(),
        "Reviewing approval request"
    );
    chat.status_state
        .pending_guardian_review_status
        .finish("review-1");
    chat.restore_reasoning_status_header();
    assert_eq!(
        chat.bottom_pane.status_widget().unwrap().header(),
        "Choosing the missing-data fix"
    );
}

#[tokio::test]
async fn reasoning_status_replay_retains_last_usable_heading() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.on_task_started();
    for (id, summary, expected) in [
        (
            "first",
            vec!["**First heading**".to_string()],
            "First heading",
        ),
        (
            "second",
            vec!["**Second heading**".to_string()],
            "Second heading",
        ),
        ("third", Vec::new(), "Second heading"),
    ] {
        chat.handle_thread_item(
            AppServerThreadItem::Reasoning {
                id: id.to_string(),
                summary,
                content: Vec::new(),
            },
            "turn-1".to_string(),
            ThreadItemRenderSource::Replay(ReplayKind::ThreadSnapshot),
        );
        assert_eq!(chat.bottom_pane.status_widget().unwrap().header(), expected);
    }
}

#[tokio::test]
async fn reasoning_status_accepts_plain_lines_and_ignores_empty_sections() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.on_task_started();
    handle_agent_reasoning_started(&mut chat, "first");
    delta(
        &mut chat,
        "first",
        "# Checking files\n<!-- progress -->\nPreparing response",
    );
    assert_eq!(
        chat.bottom_pane.status_widget().unwrap().header(),
        "Preparing response"
    );
    complete(&mut chat, "first");
    handle_agent_reasoning_started(&mut chat, "second");
    delta(&mut chat, "second", "<!-- no public update -->");
    assert_eq!(
        chat.bottom_pane.status_widget().unwrap().header(),
        "Preparing response"
    );
    delta(&mut chat, "second", "\n**Verifying results**");
    assert_eq!(
        chat.bottom_pane.status_widget().unwrap().header(),
        "Verifying results"
    );
}

#[tokio::test]
async fn completed_reasoning_stays_in_expanded_transcript_for_live_and_replay() {
    let mut renders = Vec::new();
    for replay in [false, true] {
        let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.on_task_started();
        for (id, summary) in [
            ("first", "**Inspecting repository structure**"),
            ("second", "Mapping the app structure"),
            (
                "third",
                "**Tracing terminal components**\n\nThe playback clock preserves elapsed time.",
            ),
        ] {
            if replay {
                chat.handle_thread_item(
                    AppServerThreadItem::Reasoning {
                        id: id.to_string(),
                        summary: vec![summary.to_string()],
                        content: Vec::new(),
                    },
                    "turn-1".to_string(),
                    ThreadItemRenderSource::Replay(ReplayKind::ThreadSnapshot),
                );
            } else {
                handle_agent_reasoning_started(&mut chat, id);
                delta(&mut chat, id, summary);
                complete(&mut chat, id);
            }
        }
        let mut compact = Vec::new();
        let mut expanded = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                compact.extend(cell.display_lines(/*width*/ 80));
                expanded.extend(cell.transcript_lines(/*width*/ 80));
                assert!(cell.raw_lines().is_empty());
            }
        }
        let rendered = format!(
            "Compact history:\n{}\nLive status:\n{}\nExpanded transcript:\n{}",
            ratatui::text::Text::from(compact),
            chat.bottom_pane.status_widget().unwrap().header(),
            ratatui::text::Text::from(expanded),
        );
        renders.push(rendered);
    }
    assert_eq!(renders[0], renders[1]);
    insta::assert_snapshot!(renders[0], @r"
        Compact history:

        Live status:
        The playback clock preserves elapsed time.
        Expanded transcript:
        • Inspecting repository structure
        • Mapping the app structure
        • The playback clock preserves elapsed time.
        ");
}
