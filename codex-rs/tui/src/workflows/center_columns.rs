//! Optional worker settings consume spare list space without moving the detail divider.
use ratatui::layout::Rect;

pub(super) struct Columns {
    pub(super) title: Rect,
    pub(super) model: Rect,
    pub(super) reasoning: Rect,
    pub(super) status: Rect,
    pub(super) updated: Rect,
    pub(super) combined_settings: bool,
}

pub(super) fn columns(area: Rect, saved: bool) -> Columns {
    let (model, reasoning, status, updated, combined_settings) = if saved {
        if area.width >= 56 {
            (0, 0, 11, 9, false)
        } else {
            (0, 0, 0, 0, false)
        }
    } else if area.width >= 76 {
        (16, 9, 11, 9, false)
    } else if area.width >= 58 {
        (14, 9, 11, 0, false)
    } else if area.width >= 44 {
        (20, 0, 0, 0, true)
    } else {
        (0, 0, 0, 0, false)
    };
    let reserved = [model, reasoning, status, updated]
        .into_iter()
        .filter(|width| *width > 0)
        .map(|width| width + 2)
        .sum::<u16>();
    let title = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(reserved),
        area.height,
    );
    let mut x = title.right();
    let mut next = |width| {
        if width == 0 {
            return Rect::default();
        }
        x += 2;
        let rect = Rect::new(x, area.y, width, area.height);
        x += width;
        rect
    };
    Columns {
        title,
        model: next(model),
        reasoning: next(reasoning),
        status: next(status),
        updated: next(updated),
        combined_settings,
    }
}
