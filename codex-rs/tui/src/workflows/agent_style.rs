//! Table rows and transcript captions share one stable, theme-aware agent identity color.
use ratatui::style::Color;
use ratatui::style::Style;

pub(crate) fn agent_style(run_id: &str, step_id: &str, background: Option<Color>) -> Style {
    let hash = run_id
        .bytes()
        .chain(std::iter::once(0))
        .chain(b"step:".iter().copied())
        .chain(step_id.bytes())
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    let color =
        crate::thread_color::thread_color(codex_protocol::ThreadId::from_u128(u128::from(hash)));
    Style::default().fg(crate::style::readable_color_on(color, background))
}
