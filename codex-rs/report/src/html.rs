use anyhow::Context;
use anyhow::Result;

use crate::ReportData;

const DASHBOARD_CSS: &str = include_str!("../assets/dashboard.css");
const DASHBOARD_JS: &str = include_str!("../assets/dashboard.js");

/// Renders report data and embedded dashboard assets into one offline HTML document.
pub fn render_report_html(report: &ReportData) -> Result<String> {
    let data = serde_json::to_string(report)
        .context("serialize telemetry report")?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    let css = escape_raw_element_end(DASHBOARD_CSS, "style");
    let javascript = escape_raw_element_end(DASHBOARD_JS, "script");

    Ok(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="dark light">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'none'; font-src data:">
<title>Better Codex telemetry</title>
<style>{css}</style>
</head>
<body>
<div id="root"></div>
<script id="report-data" type="application/json">{data}</script>
<script>{javascript}</script>
</body>
</html>
"#
    ))
}

fn escape_raw_element_end(content: &str, element: &str) -> String {
    let needle = format!("</{element}");
    let lowercase = content.to_ascii_lowercase();
    let mut output = String::with_capacity(content.len());
    let mut copied = 0;
    while let Some(relative_index) = lowercase[copied..].find(needle.as_str()) {
        let index = copied + relative_index;
        output.push_str(&content[copied..index]);
        output.push_str("<\\/");
        output.push_str(&content[index + 2..index + needle.len()]);
        copied = index + needle.len();
    }
    output.push_str(&content[copied..]);
    output
}
