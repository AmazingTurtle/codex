//! Web-search activity history cells.

use super::*;

fn web_search_header(completed: bool) -> &'static str {
    if completed {
        "Searched the web"
    } else {
        "Searching the web"
    }
}

fn web_search_action_detail(action: &WebSearchAction) -> String {
    match action {
        WebSearchAction::Search { query, queries } => {
            query.clone().filter(|q| !q.is_empty()).unwrap_or_else(|| {
                let items = queries.as_ref();
                let first = items
                    .and_then(|queries| queries.first())
                    .cloned()
                    .unwrap_or_default();
                if items.is_some_and(|queries| queries.len() > 1) && !first.is_empty() {
                    format!("{first} ...")
                } else {
                    first
                }
            })
        }
        WebSearchAction::OpenPage { url } => url.clone().unwrap_or_default(),
        WebSearchAction::FindInPage { url, pattern } => match (pattern, url) {
            (Some(pattern), Some(url)) => format!("'{pattern}' in {url}"),
            (Some(pattern), None) => format!("'{pattern}'"),
            (None, Some(url)) => url.clone(),
            (None, None) => String::new(),
        },
        WebSearchAction::Other => String::new(),
    }
}

fn web_search_detail(action: Option<&WebSearchAction>, query: &str) -> String {
    if query.is_empty() {
        action.map(web_search_action_detail).unwrap_or_default()
    } else {
        query.to_string()
    }
}

fn completed_web_search_detail(action: Option<&WebSearchAction>, query: &str) -> String {
    let detail = web_search_detail(action, query);
    if !detail.is_empty() {
        return detail;
    }

    match action {
        Some(WebSearchAction::OpenPage { .. }) => "a web page".to_string(),
        Some(WebSearchAction::FindInPage { .. }) => "a web page".to_string(),
        Some(WebSearchAction::Search { .. }) | Some(WebSearchAction::Other) | None => {
            "web content".to_string()
        }
    }
}

fn web_search_action_title(action: Option<&WebSearchAction>) -> &'static str {
    match action {
        Some(WebSearchAction::Search { .. }) => "Search",
        Some(WebSearchAction::OpenPage { .. }) => "Open",
        Some(WebSearchAction::FindInPage { .. }) => "Find",
        Some(WebSearchAction::Other) | None => "Browse",
    }
}

#[derive(Debug)]
struct WebSearchCall {
    call_id: String,
    query: String,
    action: Option<WebSearchAction>,
    start_time: Option<Instant>,
    completed: bool,
}

impl WebSearchCall {
    fn new(call_id: String, query: String, action: Option<WebSearchAction>) -> Self {
        Self {
            call_id,
            query,
            action,
            start_time: Some(Instant::now()),
            completed: false,
        }
    }

    fn detail(&self) -> String {
        if self.completed {
            completed_web_search_detail(self.action.as_ref(), &self.query)
        } else {
            web_search_detail(self.action.as_ref(), &self.query)
        }
    }

    fn complete(&mut self, action: WebSearchAction, query: String) {
        self.action = Some(action);
        self.query = query;
        self.start_time = None;
        self.completed = true;
    }

    fn new_completed(call_id: String, query: String, action: WebSearchAction) -> Self {
        Self {
            call_id,
            query,
            action: Some(action),
            start_time: None,
            completed: true,
        }
    }
}

#[derive(Debug)]
pub(crate) struct WebSearchCell {
    calls: Vec<WebSearchCall>,
    animations_enabled: bool,
}

impl WebSearchCell {
    pub(crate) fn new(
        call_id: String,
        query: String,
        action: Option<WebSearchAction>,
        animations_enabled: bool,
    ) -> Self {
        Self {
            calls: vec![WebSearchCall::new(call_id, query, action)],
            animations_enabled,
        }
    }

    pub(crate) fn add_call(&mut self, call_id: String, query: String) {
        if self.calls.iter().any(|call| call.call_id == call_id) {
            return;
        }
        self.calls.push(WebSearchCall::new(call_id, query, None));
    }

    pub(crate) fn complete_call(
        &mut self,
        call_id: String,
        action: WebSearchAction,
        query: String,
    ) {
        if let Some(call) = self.calls.iter_mut().find(|call| call.call_id == call_id) {
            call.complete(action, query);
        } else {
            self.calls
                .push(WebSearchCall::new_completed(call_id, query, action));
        }
    }

    fn is_active(&self) -> bool {
        self.calls.iter().any(|call| !call.completed)
    }

    fn active_start_time(&self) -> Option<Instant> {
        self.calls
            .iter()
            .find(|call| !call.completed)
            .and_then(|call| call.start_time)
    }

    fn header(&self) -> &'static str {
        web_search_header(!self.is_active())
    }

    fn activity_marker(&self) -> Span<'static> {
        if self.is_active() {
            activity_indicator(
                self.active_start_time(),
                MotionMode::from_animations_enabled(self.animations_enabled),
                ReducedMotionIndicator::StaticBullet,
            )
            .unwrap_or_else(|| "•".dim())
        } else {
            "•".dim()
        }
    }

    fn grouped_display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(vec![
            self.activity_marker(),
            " ".into(),
            self.header().bold(),
        ])];
        let mut action_lines = Vec::new();
        for call in &self.calls {
            let detail = call.detail();
            if detail.is_empty() {
                continue;
            }
            let title = web_search_action_title(call.action.as_ref());
            let initial_indent = Line::from(vec![title.cyan(), " ".into()]);
            let subsequent_indent = " ".repeat(initial_indent.width()).into();
            let detail_line = Line::from(detail);
            let wrapped = adaptive_wrap_line(
                &detail_line,
                RtOptions::new(width.saturating_sub(4) as usize)
                    .initial_indent(initial_indent)
                    .subsequent_indent(subsequent_indent),
            );
            push_owned_lines(&wrapped, &mut action_lines);
        }
        lines.extend(prefix_lines(action_lines, "  └ ".dim(), "    ".into()));
        lines
    }

    fn grouped_raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(self.header())];
        lines.extend(self.calls.iter().filter_map(|call| {
            let detail = call.detail();
            (!detail.is_empty()).then(|| {
                Line::from(format!(
                    "{} {detail}",
                    web_search_action_title(call.action.as_ref())
                ))
            })
        }));
        lines
    }
}

impl HistoryCell for WebSearchCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let [call] = self.calls.as_slice() else {
            return self.grouped_display_lines(width);
        };
        let header = self.header();
        let detail = call.detail();
        let text: Text<'static> = if detail.is_empty() {
            Line::from(vec![header.bold()]).into()
        } else {
            let separator = if call.completed { " for " } else { " " };
            Line::from(vec![header.bold(), separator.into(), detail.into()]).into()
        };
        PrefixedWrappedHistoryCell::new(text, vec![self.activity_marker(), " ".into()], "  ")
            .display_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let [call] = self.calls.as_slice() else {
            return self.grouped_raw_lines();
        };
        let header = self.header();
        let detail = call.detail();
        if detail.is_empty() {
            vec![Line::from(header)]
        } else {
            let separator = if call.completed { " for " } else { " " };
            vec![Line::from(format!("{header}{separator}{detail}"))]
        }
    }
}

pub(crate) fn new_active_web_search_call(
    call_id: String,
    query: String,
    animations_enabled: bool,
) -> WebSearchCell {
    WebSearchCell::new(call_id, query, /*action*/ None, animations_enabled)
}

pub(crate) fn new_web_search_call(
    call_id: String,
    query: String,
    action: WebSearchAction,
) -> WebSearchCell {
    WebSearchCell {
        calls: vec![WebSearchCall::new_completed(call_id, query, action)],
        animations_enabled: false,
    }
}
