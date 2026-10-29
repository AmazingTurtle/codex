use super::*;
use crate::history_cell::HistoryCell;
use pretty_assertions::assert_eq;
use ratatui::text::Line;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

fn cells(first: usize, last: usize) -> Vec<Arc<dyn crate::history_cell::HistoryCell>> {
    (first..last)
        .map(|index| {
            Arc::new(crate::history_cell::PlainHistoryCell::new(vec![
                Line::from(format!(
                    "Entry{index:02} · content stays with its logical history cell"
                )),
            ])) as Arc<dyn crate::history_cell::HistoryCell>
        })
        .collect()
}

fn first_chat_line(screen: &str) -> &str {
    screen
        .lines()
        .filter_map(|line| line.split_once('│').map(|(_, right)| right.trim_start()))
        .find(|line| line.starts_with("Entry"))
        .expect("a scoped transcript entry is visible")
}

#[test]
fn four_row_transcript_scroll_keeps_the_same_cell_after_prepend_append_refresh_and_resize() {
    let state = fixture();
    state.lock().unwrap().child = Some(0);
    state.lock().unwrap().center.detail_tab = DetailTab::Transcript;
    let key = selected_transcript_key(&state);
    let original = cells(10, 40);
    state.lock().unwrap().transcripts.insert(
        key.clone(),
        WorkflowTranscript {
            cells: original.clone(),
            content_revision: 1,
            ..Default::default()
        },
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    let before = render(&center, 140, 16);
    assert!(first_chat_line(&before).starts_with("Entry10"), "{before}");
    center.handle_key_event(KeyCode::Char('k').into());
    let scrolled = render(&center, 140, 16);
    // The normal history viewport inserts one separator row per one-line cell.
    // Four physical rows therefore advance by two entries, retaining overlap.
    assert!(
        first_chat_line(&scrolled).starts_with("Entry12"),
        "{scrolled}"
    );
    {
        let state = state.lock().unwrap();
        assert_eq!(
            (state.child, state.center.detail_tab),
            (Some(0), DetailTab::Transcript)
        );
    }
    {
        let mut state = state.lock().unwrap();
        let transcript = state.transcripts.get_mut(&key).unwrap();
        transcript.cells = cells(0, 10)
            .into_iter()
            .chain(original.clone())
            .chain(cells(40, 45))
            .collect();
        transcript.content_revision += 1;
    }
    let updated = render(&center, 140, 16);
    assert_eq!(first_chat_line(&updated), first_chat_line(&scrolled));
    let resized = render(&center, 100, 16);
    assert_eq!(first_chat_line(&resized), first_chat_line(&scrolled));
    insta::assert_snapshot!("workflow_chat_anchored_after_prepend_and_resize", resized);
    center.handle_key_event(KeyCode::Char('j').into());
    let up = render(&center, 100, 16);
    assert!(first_chat_line(&up).starts_with("Entry10"), "{up}");
}

#[derive(Debug)]
struct CountedMessage {
    message: crate::history_cell::AgentMarkdownCell,
    renders: Arc<AtomicUsize>,
}

impl HistoryCell for CountedMessage {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.renders.fetch_add(1, Ordering::Relaxed);
        self.message.display_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.message.raw_lines()
    }
}

#[test]
fn long_chat_endpoint_repeats_reuse_visible_layouts_instead_of_rendering_the_whole_history() {
    let state = fixture();
    let renders = Arc::new(AtomicUsize::new(0));
    {
        let mut state = state.lock().unwrap();
        state.child = Some(0);
        state.center.detail_tab = DetailTab::Transcript;
    }
    let key = selected_transcript_key(&state);
    let message_cells = (0..240).map(|index| {
        let output = (0..80).map(|line| format!("module_{index}/src/check_{line}.rs: checked long diagnostic output and retained its complete original text\n")).collect::<String>();
        Arc::new(CountedMessage {
            message: crate::history_cell::AgentMarkdownCell::new(
                format!("Message {index:03}\n\n```text\n{output}```\n\nEND{index:03}"),
                std::path::Path::new("."),
            ),
            renders: Arc::clone(&renders),
        }) as Arc<dyn HistoryCell>
    }).collect();
    state.lock().unwrap().transcripts.insert(
        key.clone(),
        WorkflowTranscript {
            cells: message_cells,
            content_revision: 1,
            ..Default::default()
        },
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    let top = render(&center, 140, 20);
    assert!(top.contains("Message 000"), "{top}");
    assert!(
        renders.load(Ordering::Relaxed) < 12,
        "only visible message layouts should be built"
    );

    center.handle_key_event(KeyCode::PageDown.into());
    let bottom = render(&center, 140, 20);
    assert!(bottom.contains("END239"), "{bottom}");
    let warm_renders = renders.load(Ordering::Relaxed);
    for _ in 0..40 {
        center.handle_key_event(KeyCode::PageDown.into());
        assert_eq!(render(&center, 140, 20), bottom);
    }
    assert_eq!(renders.load(Ordering::Relaxed), warm_renders);

    center.handle_key_event(KeyCode::PageUp.into());
    assert_eq!(render(&center, 140, 20), top);
    let warm_renders = renders.load(Ordering::Relaxed);
    for _ in 0..40 {
        center.handle_key_event(KeyCode::PageUp.into());
        assert_eq!(render(&center, 140, 20), top);
    }
    assert_eq!(renders.load(Ordering::Relaxed), warm_renders);
    assert!(
        rx.try_recv().is_err(),
        "scrolling performs no transcript RPC"
    );

    // An appended message refreshes the endpoint without rebuilding the preceding 240 entries.
    {
        let mut state = state.lock().unwrap();
        let transcript = state.transcripts.get_mut(&key).unwrap();
        transcript.cells.extend(cells(240, 241));
        transcript.content_revision += 1;
    }
    render(&center, 140, 20);
    center.handle_key_event(KeyCode::PageDown.into());
    let appended = render(&center, 100, 20);
    assert!(appended.contains("Entry240"), "{appended}");
}

#[test]
fn cached_source_reflows_on_resize_and_replaces_same_length_content_without_resetting_the_view() {
    let state = fixture();
    {
        let mut state = state.lock().unwrap();
        state.child = None;
        state.center.detail_tab = DetailTab::Source;
        state.rows[0].source = (0..80)
            .map(|index| format!("const value_{index:02} = 'original payload';\n"))
            .collect();
    }
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    let top = render(&center, 140, 20);
    center.handle_key_event(KeyCode::PageDown.into());
    let bottom = render(&center, 140, 20);
    assert!(bottom.contains("value_79"), "{bottom}");
    for _ in 0..10 {
        center.handle_key_event(KeyCode::PageDown.into());
    }
    assert_eq!(render(&center, 140, 20), bottom);
    {
        let mut state = state.lock().unwrap();
        state.rows[0].source = state.rows[0]
            .source
            .replace("original payload", "modified payload");
    }
    let modified = render(&center, 140, 20);
    assert!(modified.contains("modified payload"), "{modified}");
    assert!(!modified.contains("original payload"), "{modified}");
    assert!(modified.contains("value_79"), "{modified}");
    center.handle_key_event(KeyCode::PageUp.into());
    let restored = render(&center, 140, 20);
    assert!(restored.contains("value_00"), "{restored}");
    assert_ne!(restored, top);
    let narrow = render(&center, 100, 20);
    assert!(narrow.contains("value_00"), "{narrow}");
    insta::assert_snapshot!("workflow_cached_source_updated_and_resized", narrow);
}

#[test]
fn phase_overview_compacts_actual_models_and_unknown_metadata_without_root_fallback() {
    let state = fixture();
    {
        let mut state = state.lock().unwrap();
        state.rows[0].children[0].resolved_model = Some("gpt-review".into());
        state.rows[0].children[0].reasoning_effort = Some("high".into());
    }
    state.lock().unwrap().rows[0].children.insert(0,WorkflowChild {
        key:"phase:review".into(),label:"Review · Active".into(),depth:1,
        overview:Some(WorkflowOverview {title:"Review".into(),metadata:vec![("Status".into(),"Active".into())],phase_steps:vec![
            WorkflowPhaseStep {label:"Inspect source".into(),model:Some("gpt-review".into()),reasoning:Some("high".into()),prompt:"Inspect source and report a concise actionable review without rewriting the implementation".into(),status:"Running".into()},
            WorkflowPhaseStep {label:"Check tests".into(),model:None,reasoning:None,prompt:"Check the changed tests".into(),status:"Discovered".into()},
        ],..Default::default()}),..Default::default()
    });
    state.lock().unwrap().child = Some(0);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    let wide = render(&center, 180, 20);
    assert!(
        wide.lines().any(|line| line.contains("Inspect source")
            && line.contains("gpt-review")
            && line.contains("high")
            && line.contains("Running")),
        "{wide}"
    );
    assert!(wide.contains("Unknown"), "{wide}");
    insta::assert_snapshot!("workflow_phase_compact_steps", wide);
    render(&center, 42, 20);
    center.handle_key_event(KeyCode::Char('1').into());
    insta::assert_snapshot!(
        "workflow_phase_compact_steps_narrow",
        render(&center, 42, 20)
    );
}
