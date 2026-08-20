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
