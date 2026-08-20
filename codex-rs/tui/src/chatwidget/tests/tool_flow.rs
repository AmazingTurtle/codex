use super::*;
use codex_app_server_protocol::WebSearchAction;
use codex_app_server_protocol::WebSearchItem;
use pretty_assertions::assert_eq;

fn search_action(query: &str) -> WebSearchAction {
    WebSearchAction::Search {
        query: Some(query.to_string()),
        queries: None,
    }
}

#[tokio::test]
async fn consecutive_web_searches_collapse_until_visible_activity_interrupts_them() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.on_web_search_begin("search-1".to_string());
    chat.on_web_search_end(
        "search-1".to_string(),
        "rust ratatui wrapping".to_string(),
        search_action("rust ratatui wrapping"),
    );
    let _ = drain_insert_history(&mut rx);

    chat.on_web_search_begin("search-2".to_string());
    assert_chatwidget_snapshot!("active_grouped_web_search", active_blob(&chat));
    chat.on_web_search_end(
        "search-2".to_string(),
        "https://docs.rs/ratatui/latest/ratatui/".to_string(),
        WebSearchAction::OpenPage {
            url: Some("https://docs.rs/ratatui/latest/ratatui/".to_string()),
        },
    );

    chat.add_to_history(PlainHistoryCell::new(vec!["Different activity".into()]));
    let cells = drain_insert_history(&mut rx);

    assert_eq!(cells.len(), 2);
    assert_chatwidget_snapshot!(
        "completed_grouped_web_search",
        lines_to_single_string(&cells[0])
    );
    assert_eq!(lines_to_single_string(&cells[1]), "\nDifferent activity\n");
}

#[tokio::test]
async fn replayed_web_searches_reconstruct_one_group() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.replay_thread_item(
        AppServerThreadItem::WebSearch(WebSearchItem {
            id: "search-1".to_string(),
            query: "rust ratatui wrapping".to_string(),
            action: Some(search_action("rust ratatui wrapping")),
            results: None,
        }),
        "turn-1".to_string(),
        ReplayKind::ThreadSnapshot,
    );
    let _ = drain_insert_history(&mut rx);
    chat.replay_thread_item(
        AppServerThreadItem::WebSearch(WebSearchItem {
            id: "search-2".to_string(),
            query: String::new(),
            action: Some(WebSearchAction::OpenPage { url: None }),
            results: None,
        }),
        "turn-1".to_string(),
        ReplayKind::ThreadSnapshot,
    );
    chat.flush_active_cell();

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1);
    assert_chatwidget_snapshot!(
        "replayed_grouped_web_search",
        lines_to_single_string(&cells[0])
    );
}

#[tokio::test]
async fn consecutive_image_views_collapse_until_visible_activity_interrupts_them() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let first_path = chat.config.cwd.join("first.png");
    let second_path = chat.config.cwd.join("second.png");

    handle_view_image_tool_call(&mut chat, "image-1", first_path);
    handle_view_image_tool_call(&mut chat, "image-2", second_path);
    assert_chatwidget_snapshot!("active_grouped_viewed_images", active_blob(&chat));

    chat.add_to_history(PlainHistoryCell::new(vec!["Different activity".into()]));
    let cells = drain_insert_history(&mut rx);

    assert_eq!(cells.len(), 2);
    assert_chatwidget_snapshot!(
        "completed_grouped_viewed_images",
        lines_to_single_string(&cells[0])
    );
    assert_eq!(lines_to_single_string(&cells[1]), "\nDifferent activity\n");

    let third_path = chat.config.cwd.join("third.png");
    handle_view_image_tool_call(&mut chat, "image-3", third_path);
    chat.flush_active_cell();

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1);
    assert_eq!(
        lines_to_single_string(&cells[0]),
        "• Viewed Image\n  └ third.png\n"
    );
}

#[tokio::test]
async fn replayed_image_views_reconstruct_one_group() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let first_path = chat.config.cwd.join("first.png").into();
    let second_path = chat.config.cwd.join("second.png").into();

    chat.replay_thread_item(
        AppServerThreadItem::ImageView {
            id: "image-1".to_string(),
            path: first_path,
        },
        "turn-1".to_string(),
        ReplayKind::ThreadSnapshot,
    );
    chat.replay_thread_item(
        AppServerThreadItem::ImageView {
            id: "image-2".to_string(),
            path: second_path,
        },
        "turn-1".to_string(),
        ReplayKind::ThreadSnapshot,
    );
    chat.flush_active_cell();

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1);
    assert_chatwidget_snapshot!(
        "replayed_grouped_viewed_images",
        lines_to_single_string(&cells[0])
    );
}

#[tokio::test]
async fn grouped_image_views_reserve_width_for_tree_prefix() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let first_path = chat.config.cwd.join("first image with a long name.png");
    let second_path = chat.config.cwd.join("second image.png");

    handle_view_image_tool_call(&mut chat, "image-1", first_path);
    handle_view_image_tool_call(&mut chat, "image-2", second_path);

    let rendered = chat
        .transcript
        .active_cell
        .as_ref()
        .expect("active image group")
        .display_lines(/*width*/ 24)
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec![
            "• Viewed Images".to_string(),
            "  └ first image with a".to_string(),
            "    long name.png".to_string(),
            "    second image.png".to_string(),
        ]
    );
}
