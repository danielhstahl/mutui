//! Pure scroll/scrollbar arithmetic.
//!
//! Invariant enforced everywhere below: `0 <= offset <= max(0, content - visible)`.

use std::cmp::Ordering;

/// Rows moved by one mouse wheel notch (documented constant, `--wheel-rows`).
pub const WHEEL_ROWS: i32 = 3;

/// Geometry of the scrollbar thumb, in rows relative to the list pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thumb {
    /// First row (relative to the top of the list pane) occupied by the thumb.
    pub start: usize,
    /// Height of the thumb in rows; always `>= 1`.
    pub len: usize,
    /// Height of the scrollable track in rows.
    pub track: usize,
}

impl Thumb {
    /// `true` when `row` (pane-relative) is inside the thumb.
    #[must_use]
    pub fn contains(&self, row: usize) -> bool {
        (self.start..self.start + self.len).contains(&row)
    }
}

/// Scroll offset plus the sizes it is clamped against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scroll {
    offset: usize,
    visible: usize,
    content: usize,
}

impl Default for Scroll {
    fn default() -> Self {
        Self::new()
    }
}

impl Scroll {
    /// An empty, unscrolled view.
    #[must_use]
    pub fn new() -> Self {
        Self {
            offset: 0,
            visible: 0,
            content: 0,
        }
    }

    /// Current offset (row index drawn at the top of the pane).
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Rows that fit in the pane.
    #[must_use]
    pub fn visible(&self) -> usize {
        self.visible
    }

    /// Total rows in the library.
    #[must_use]
    pub fn content(&self) -> usize {
        self.content
    }

    /// Largest legal offset.
    #[must_use]
    pub fn max_offset(&self) -> usize {
        self.content.saturating_sub(self.visible)
    }

    /// Update the number of rows in the library (keeps the offset clamped).
    pub fn set_content(&mut self, content: usize) {
        self.content = content;
        self.clamp();
    }

    /// Update the pane height (keeps the offset clamped).
    pub fn set_visible(&mut self, visible: usize) {
        self.visible = visible;
        self.clamp();
    }

    /// Force the offset back into range.
    pub fn clamp(&mut self) {
        self.offset = self.offset.min(self.max_offset());
    }

    /// Move by `rows` (negative scrolls up). Never leaves the clamp range.
    pub fn scroll_by(&mut self, rows: i32) {
        let target = self.offset as i64 + i64::from(rows);
        self.offset = target.clamp(0, self.max_offset() as i64) as usize;
    }

    /// Show `row` at the top if possible.
    pub fn scroll_to(&mut self, row: usize) {
        self.offset = row.min(self.max_offset());
    }

    /// One page down.
    pub fn page_down(&mut self) {
        self.scroll_by(self.visible.max(1) as i32);
    }

    /// One page up.
    pub fn page_up(&mut self) {
        self.scroll_by(-(self.visible.max(1) as i32));
    }

    /// First row.
    pub fn home(&mut self) {
        self.offset = 0;
    }

    /// Last page.
    pub fn end(&mut self) {
        self.offset = self.max_offset();
    }

    /// Scroll so that the fraction `f` of the document is above the viewport.
    /// Non-finite input is treated as `0.0` (never NaN, never panic).
    pub fn jump_to_fraction(&mut self, f: f32) {
        // NaN -> 0.0; +/-inf clamp to the ends.
        let f = if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) };
        self.offset = ((self.max_offset() as f64 * f64::from(f)).round()) as usize;
        self.clamp();
    }

    /// Minimum scroll that brings `row` into the window (no-op when visible).
    pub fn ensure_visible(&mut self, row: usize) {
        if self.visible == 0 {
            return;
        }
        match row.cmp(&self.offset) {
            Ordering::Less => self.offset = row,
            Ordering::Greater => {
                if row >= self.offset + self.visible {
                    self.offset = row + 1 - self.visible;
                }
            }
            Ordering::Equal => {}
        }
        self.clamp();
    }

    /// Thumb geometry for the current state (see [`Thumb`]).
    #[must_use]
    pub fn thumb(&self) -> Thumb {
        let track = self.visible;
        if track == 0 {
            return Thumb {
                start: 0,
                len: 0,
                track: 0,
            };
        }
        if self.content <= track {
            return Thumb {
                start: 0,
                len: track,
                track,
            };
        }
        // Thumb length is proportional to the visible share, but never zero.
        let len = ((track as u64 * track as u64) / self.content.max(1) as u64)
            .max(1)
            .min(track as u64) as usize;
        let max = self.max_offset().max(1);
        let start =
            ((self.offset as u64 * (track.saturating_sub(len)) as u64) / max as u64) as usize;
        Thumb {
            start: start.min(track.saturating_sub(1)),
            len,
            track,
        }
    }

    /// Scroll so that the fraction `row/track` of the document is above the
    /// viewport (used when the user clicks or drags inside the scrollbar).
    pub fn scroll_to_row_in_track(&mut self, row: usize) {
        let track = self.visible;
        if track == 0 {
            return;
        }
        let fraction = (row.min(track - 1) as f32) / (track as f32);
        self.jump_to_fraction(fraction);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scroll(content: usize, visible: usize) -> Scroll {
        let mut s = Scroll::new();
        s.set_content(content);
        s.set_visible(visible);
        s
    }

    #[test]
    fn wheel_moves_three_rows_and_clamps() {
        let mut s = scroll(100, 10);
        s.scroll_by(WHEEL_ROWS);
        assert_eq!(s.offset(), 3);
        s.scroll_by(-WHEEL_ROWS);
        assert_eq!(s.offset(), 0);
        s.scroll_by(-1000);
        assert_eq!(s.offset(), 0);
        s.scroll_by(10_000);
        assert_eq!(s.offset(), 90);
    }

    #[test]
    fn thumb_shrinks_as_content_grows_and_never_vanishes() {
        let small = scroll(10, 10).thumb();
        assert_eq!(small.len, 10);
        let mid = scroll(200, 20).thumb();
        let big = scroll(50_000, 20).thumb();
        assert!(mid.len >= 1 && big.len >= 1);
        assert!(
            big.len < mid.len,
            "thumb must shrink as content grows: {mid:?} {big:?}"
        );
        assert_eq!(mid.track, 20);
    }

    #[test]
    fn thumb_travels_top_middle_bottom() {
        let mut s = scroll(1000, 20);
        assert_eq!(s.thumb().start, 0);
        s.jump_to_fraction(0.5);
        let mid = s.thumb();
        assert!(mid.start > 0 && mid.start + mid.len < mid.track);
        s.jump_to_fraction(1.0);
        assert_eq!(s.offset(), 980);
        assert_eq!(s.thumb().start + s.thumb().len, s.thumb().track);
    }

    #[test]
    fn jump_to_fraction_is_nan_safe() {
        let mut s = scroll(100, 10);
        s.jump_to_fraction(f32::NAN);
        assert_eq!(s.offset(), 0);
        s.jump_to_fraction(f32::INFINITY);
        assert_eq!(s.offset(), 90);
        s.jump_to_fraction(-1.0);
        assert_eq!(s.offset(), 0);
    }

    #[test]
    fn ensure_visible_moves_the_minimum_amount() {
        let mut s = scroll(100, 10);
        s.ensure_visible(5);
        assert_eq!(s.offset(), 0);
        s.ensure_visible(42);
        assert_eq!(s.offset(), 33);
        s.ensure_visible(0);
        assert_eq!(s.offset(), 0);
    }

    #[test]
    fn arbitrary_call_sequences_respect_the_clamp_invariant() {
        // Property-style: any interleaving keeps 0 <= offset <= content-visible.
        let mut s = scroll(500, 25);
        let ops: [i32; 9] = [-500, -3, -1, 0, 1, 3, 500, 24, 25];
        for a in ops {
            for b in ops {
                for c in ops {
                    for op in [a, b, c] {
                        match op.rem_euclid(6) {
                            0 => s.scroll_by(op),
                            1 => s.scroll_to(op.unsigned_abs() as usize),
                            2 => s.page_down(),
                            3 => s.page_up(),
                            4 => s.jump_to_fraction((op as f32) / 500.0),
                            _ => s.ensure_visible(op.unsigned_abs() as usize),
                        }
                        assert!(s.offset() <= s.max_offset(), "{:?}", s);
                    }
                    s.home();
                }
            }
        }
        // Shrinking the content clamps instead of leaving a stale offset.
        s.end();
        s.set_content(5);
        assert_eq!(s.offset(), 0);
    }

    #[test]
    fn tiny_and_empty_views_are_fine() {
        let mut s = scroll(0, 0);
        s.scroll_by(5);
        assert_eq!(s.offset(), 0);
        assert_eq!(
            s.thumb(),
            Thumb {
                start: 0,
                len: 0,
                track: 0
            }
        );
        let mut s = scroll(3, 40);
        s.end();
        assert_eq!(s.offset(), 0);
        assert_eq!(s.thumb().len, 40);
    }
}
