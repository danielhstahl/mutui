//! Mouse hit-testing, as a pure function so it is unit-testable.
//!
//! The application turns each [`MouseAction`] into exactly one state change in
//! its reducer, which is what keeps mouse and keyboard behaviour identical.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::scroll::{Scroll, WHEEL_ROWS};

/// What a mouse event means for the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Click on a row: select it and play it (the core requirement).
    PlayRow(usize),
    /// Drag started or moved inside the scrollbar: scroll so the thumb covers
    /// this pane-relative row.
    ScrollTo(usize),
    /// Drag released.
    EndDrag,
    /// Wheel notch (positive scrolls down).
    Wheel(i32),
    /// Nothing to do (outside the list, past the last row, right button, ...).
    Ignore,
}

/// Everything hit-testing needs to know.
#[derive(Debug, Clone, Copy)]
pub struct HitContext<'a> {
    /// List pane area.
    pub list: Rect,
    /// Scrollbar gutter area.
    pub gutter: Rect,
    /// Current scroll state (offset + geometry).
    pub scroll: &'a Scroll,
    /// Number of rows in the library.
    pub len: usize,
    /// `true` while a press started on the scrollbar is being dragged.
    pub dragging: bool,
}

/// Map a screen coordinate to a pane-relative row (columns are checked too).
fn relative_row(rect: Rect, col: u16, row: u16) -> Option<usize> {
    let in_columns = col >= rect.x && (rect.width == 0 || col < rect.right());
    let in_rows = row >= rect.y && row < rect.bottom();
    if rect.height == 0 || !in_columns || !in_rows {
        return None;
    }
    Some(usize::from(row - rect.top()))
}

/// Translate a mouse event into a [`MouseAction`] (see module docs).
#[must_use]
pub fn hit_test(event: &MouseEvent, ctx: HitContext<'_>) -> MouseAction {
    match event.kind {
        MouseEventKind::ScrollUp => MouseAction::Wheel(-WHEEL_ROWS),
        MouseEventKind::ScrollDown => MouseAction::Wheel(WHEEL_ROWS),
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => MouseAction::Ignore,
        MouseEventKind::Moved => MouseAction::Ignore,
        MouseEventKind::Up(MouseButton::Left) => {
            if ctx.dragging {
                MouseAction::EndDrag
            } else {
                MouseAction::Ignore
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(row) = relative_row(ctx.gutter, event.column, event.row) {
                // Clicking the scrollbar: jump and arm a drag.
                return MouseAction::ScrollTo(row);
            }
            let Some(row) = relative_row(ctx.list, event.column, event.row) else {
                return MouseAction::Ignore;
            };
            let index = row + ctx.scroll.offset();
            if index >= ctx.len {
                // Click below the last row: ignore, never wrap.
                return MouseAction::Ignore;
            }
            MouseAction::PlayRow(index)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if !ctx.dragging {
                // Dragging with the press origin outside the scrollbar must not
                // jump the view.
                return MouseAction::Ignore;
            }
            match relative_row(ctx.gutter, event.column, event.row) {
                Some(row) => MouseAction::ScrollTo(row),
                None => MouseAction::Ignore,
            }
        }
        _ => MouseAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn ev(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn scenario(len: usize, visible: u16, offset: usize) -> (Rect, Rect, Scroll) {
        let list = Rect::new(0, 0, 49, visible);
        let gutter = Rect::new(49, 0, 1, visible);
        let mut scroll = Scroll::new();
        scroll.set_content(len);
        scroll.set_visible(usize::from(visible));
        scroll.scroll_to(offset);
        (list, gutter, scroll)
    }

    #[test]
    fn row_clicks_account_for_the_scroll_offset() {
        let (list, gutter, scroll) = scenario(500, 10, 120);
        let ctx = HitContext {
            list,
            gutter,
            scroll: &scroll,
            len: 500,
            dragging: false,
        };
        assert_eq!(
            hit_test(&ev(MouseEventKind::Down(MouseButton::Left), 10, 0), ctx),
            MouseAction::PlayRow(120)
        );
        assert_eq!(
            hit_test(&ev(MouseEventKind::Down(MouseButton::Left), 10, 4), ctx),
            MouseAction::PlayRow(124)
        );
        assert_eq!(
            hit_test(&ev(MouseEventKind::Down(MouseButton::Left), 10, 9), ctx),
            MouseAction::PlayRow(129)
        );
    }

    #[test]
    fn clicks_outside_the_list_are_ignored() {
        let (list, gutter, scroll) = scenario(5, 10, 0);
        let ctx = HitContext {
            list,
            gutter,
            scroll: &scroll,
            len: 5,
            dragging: false,
        };
        // Below the last row.
        assert_eq!(
            hit_test(&ev(MouseEventKind::Down(MouseButton::Left), 10, 7), ctx),
            MouseAction::Ignore
        );
        // Footer row.
        assert_eq!(
            hit_test(&ev(MouseEventKind::Down(MouseButton::Left), 10, 10), ctx),
            MouseAction::Ignore
        );
        // Right button.
        assert_eq!(
            hit_test(&ev(MouseEventKind::Down(MouseButton::Right), 10, 1), ctx),
            MouseAction::Ignore
        );
    }

    #[test]
    fn scrollbar_click_jumps_and_drags() {
        let (list, gutter, scroll) = scenario(1000, 20, 0);
        let ctx = HitContext {
            list,
            gutter,
            scroll: &scroll,
            len: 1000,
            dragging: false,
        };
        assert_eq!(
            hit_test(&ev(MouseEventKind::Down(MouseButton::Left), 49, 5), ctx),
            MouseAction::ScrollTo(5)
        );
        // Dragging is only honoured when the press started on the gutter.
        let dragging = HitContext {
            dragging: true,
            ..ctx
        };
        assert_eq!(
            hit_test(
                &ev(MouseEventKind::Drag(MouseButton::Left), 49, 9),
                dragging
            ),
            MouseAction::ScrollTo(9)
        );
        assert_eq!(
            hit_test(&ev(MouseEventKind::Drag(MouseButton::Left), 49, 9), ctx),
            MouseAction::Ignore
        );
        assert_eq!(
            hit_test(&ev(MouseEventKind::Up(MouseButton::Left), 49, 9), dragging),
            MouseAction::EndDrag
        );
        // A drag that leaves the gutter vertically is ignored (no wild jumps).
        assert_eq!(
            hit_test(
                &ev(MouseEventKind::Drag(MouseButton::Left), 49, 99),
                dragging
            ),
            MouseAction::Ignore
        );
    }

    #[test]
    fn wheel_reports_three_rows() {
        let (list, gutter, scroll) = scenario(100, 10, 0);
        let ctx = HitContext {
            list,
            gutter,
            scroll: &scroll,
            len: 100,
            dragging: false,
        };
        assert_eq!(
            hit_test(&ev(MouseEventKind::ScrollUp, 5, 3), ctx),
            MouseAction::Wheel(-3)
        );
        assert_eq!(
            hit_test(&ev(MouseEventKind::ScrollDown, 5, 3), ctx),
            MouseAction::Wheel(3)
        );
    }

    // Kept so the unused-import lint stays honest if the file changes shape.
}
