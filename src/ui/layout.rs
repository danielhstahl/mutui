//! Window layout: one list pane, a one-column scrollbar gutter and a footer.

use ratatui::layout::Rect;

/// The three regions the UI draws into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Areas {
    /// Song list.
    pub list: Rect,
    /// Scrollbar column (zero-width on absurdly narrow terminals).
    pub gutter: Rect,
    /// Status line.
    pub footer: Rect,
}

/// Compute the layout for `area`.
///
/// Degrades instead of panicking: at height 1 the footer takes the whole screen,
/// at width < 8 the scrollbar gutter disappears.
#[must_use]
pub fn areas(area: Rect) -> Areas {
    let height = area.height;
    let width = area.width;
    let footer_height = if height >= 2 { 1 } else { height };
    let list_height = height.saturating_sub(footer_height);
    let gutter_width = u16::from(width >= 8);
    let list_width = width.saturating_sub(gutter_width);
    Areas {
        list: Rect::new(area.x, area.y, list_width, list_height),
        gutter: Rect::new(
            area.x.saturating_add(list_width),
            area.y,
            gutter_width,
            list_height,
        ),
        footer: Rect::new(
            area.x,
            area.y.saturating_add(list_height),
            width,
            footer_height,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserves_footer_and_gutter() {
        let a = areas(Rect::new(0, 0, 80, 24));
        assert_eq!(a.footer.height, 1);
        assert_eq!(a.list.height, 23);
        assert_eq!(a.gutter.width, 1);
        assert_eq!(a.list.width, 79);
        assert_eq!(a.list.y + a.list.height, a.footer.y);
    }

    #[test]
    fn degrades_at_absurd_sizes() {
        let tiny = areas(Rect::new(0, 0, 4, 1));
        assert_eq!(tiny.list.height, 0);
        assert_eq!(tiny.gutter.width, 0);
        assert_eq!(tiny.footer.height, 1);
        let three = areas(Rect::new(0, 0, 20, 3));
        assert_eq!(three.list.height, 2);
        assert_eq!(three.footer.height, 1);
        assert_eq!(three.gutter.width, 1);
    }
}
