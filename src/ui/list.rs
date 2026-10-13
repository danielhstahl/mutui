//! The song list: one row per track, only the visible window is rendered.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};
use ratatui::Frame;

use crate::app::state::{AppState, PlaybackState};
use crate::model::Track;

use super::scroll::Scroll;
use super::truncate::{clip, clip_pad, pad_left_clip};

/// Column widths for a row, derived from the pane width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowLayout {
    /// `d1 t2` column (0 = hidden).
    pub number: usize,
    /// Artist column (0 = hidden).
    pub artist: usize,
    /// Album column (0 = hidden).
    pub album: usize,
    /// Title column.
    pub title: usize,
    /// Duration column (0 = hidden).
    pub duration: usize,
}

/// Choose column widths for a pane of `width` cells.
///
/// A single space separates every pair of columns, so the widths below exclude
/// the separators (`row_line` inserts them) — that is what keeps columns lined
/// up whatever the tag lengths are.
#[must_use]
pub fn row_layout(width: usize) -> RowLayout {
    // 1 cell for the state marker, 1 gap.
    let body = width.saturating_sub(2);
    if body < 12 {
        return RowLayout {
            number: 0,
            artist: 0,
            album: 0,
            title: body.max(1),
            duration: 0,
        };
    }
    if body < 30 {
        return RowLayout {
            number: 6,
            artist: 0,
            album: 0,
            title: body.saturating_sub(12 + 2).max(1),
            duration: 6,
        };
    }
    if body < 46 {
        return RowLayout {
            number: 6,
            artist: 14,
            album: 0,
            title: body.saturating_sub(26 + 3).max(1),
            duration: 6,
        };
    }
    RowLayout {
        number: 6,
        artist: 18,
        album: 18,
        title: body.saturating_sub(48 + 4).max(1),
        duration: 6,
    }
}

/// Highlight state of one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RowFlags {
    /// The cursor sits on this row.
    pub selected: bool,
    /// This row is the one being played.
    pub playing: bool,
    /// This row is paused.
    pub paused: bool,
    /// Prefer ASCII glyphs.
    pub ascii: bool,
}

/// The state marker glyph for a row (never zero-width).
#[must_use]
pub fn marker(flags: RowFlags, corrupt: bool) -> char {
    if corrupt {
        '!'
    } else if flags.playing {
        if flags.ascii {
            '>'
        } else {
            '▶'
        }
    } else if flags.paused {
        '|'
    } else if flags.selected {
        if flags.ascii {
            '*'
        } else {
            '•'
        }
    } else {
        ' '
    }
}

/// Build the text of one row (used by the renderer and by tests/benches).
#[must_use]
pub fn row_line(track: &Track, flags: RowFlags, width: usize) -> Line<'static> {
    let layout = row_layout(width);
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(6);
    spans.push(Span::styled(
        marker(flags, track.corrupt).to_string(),
        marker_style(flags, track.corrupt),
    ));
    spans.push(Span::raw(" "));
    if layout.number > 0 {
        spans.push(Span::styled(
            clip_pad(&track.track_label(), layout.number),
            Style::default().add_modifier(Modifier::DIM),
        ));
        spans.push(Span::raw(" "));
    }
    if layout.artist > 0 {
        spans.push(Span::raw(clip_pad(track.display_artist(), layout.artist)));
        spans.push(Span::raw(" "));
    }
    if layout.album > 0 {
        spans.push(Span::styled(
            clip_pad(track.display_album(), layout.album),
            Style::default().add_modifier(Modifier::DIM),
        ));
        spans.push(Span::raw(" "));
    }
    let mut title = String::new();
    if track.corrupt {
        title.push_str("[unreadable] ");
    }
    title.push_str(&track.title);
    spans.push(Span::raw(clip(&title, layout.title.max(1))));
    if layout.duration > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            pad_left_clip(&track.duration_label(), layout.duration),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    let mut line = Line::from(spans);
    if flags.selected {
        line = line.style(
            Style::default()
                .bg(Color::Rgb(45, 45, 60))
                .add_modifier(Modifier::BOLD),
        );
    } else if flags.playing || flags.paused {
        line = line.style(Style::default().fg(Color::Yellow));
    }
    line
}

fn marker_style(flags: RowFlags, corrupt: bool) -> Style {
    if corrupt {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if flags.playing {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else if flags.paused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// `(start, end)` of the rows that must be drawn for the current scroll state.
#[must_use]
pub fn visible_window(scroll: &Scroll, len: usize) -> (usize, usize) {
    let start = scroll.offset().min(len);
    let end = (start + scroll.visible().max(1)).min(len);
    (start, end)
}

/// Render the list pane. Cost is O(visible rows), never O(library).
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let width = usize::from(area.width);
    let (start, end) = visible_window(&state.scroll, state.playlist.len());
    let playing = match state.playback {
        PlaybackState::Playing { index, .. } => Some(index),
        _ => None,
    };
    let paused = match state.playback {
        PlaybackState::Paused { index, .. } => Some(index),
        _ => None,
    };
    let cursor = state.playlist.cursor();
    let ascii = state.opts.ascii;

    let items: Vec<ListItem<'_>> = state
        .playlist
        .tracks()
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|(index, track)| {
            let flags = RowFlags {
                selected: cursor == Some(index),
                playing: playing == Some(index),
                paused: paused == Some(index),
                ascii,
            };
            ListItem::new(row_line(track, flags, width))
        })
        .collect();

    let list = List::new(items).highlight_style(Style::default());
    // The window we pass in is already offset, so the widget's own offset is 0.
    let mut list_state = ListState::default().with_selected(None).with_offset(0);
    list_state.select(None);
    frame.render_stateful_widget(list, area, &mut list_state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn track(artist: &str, album: &str, title: &str) -> Track {
        let mut t = Track::from_path(PathBuf::from(format!("/m/{title}.mp3")));
        t.artist = Some(artist.to_string());
        t.album = Some(album.to_string());
        t.title = title.to_string();
        t.refresh_key();
        t
    }

    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn truncates_unicode_titles_without_splitting_cells() {
        let t = track("Ärzte", "Devil's Music", "日本語のタイトル");
        let line = row_line(&t, RowFlags::default(), 40);
        let text = plain(&line);
        assert!(
            unicode_width::UnicodeWidthStr::width(text.as_str()) <= 40,
            "row too wide: {text:?}"
        );
        assert!(text.contains('日'), "expected CJK content in {text:?}");
    }

    #[test]
    fn narrow_terminals_drop_columns_in_order() {
        assert_eq!(row_layout(0).title, 1);
        assert_eq!(row_layout(10).album, 0);
        assert_eq!(row_layout(10).duration, 0);
        assert_eq!(row_layout(28).artist, 0);
        assert!(row_layout(36).artist > 0);
        assert_eq!(row_layout(36).album, 0);
        assert!(row_layout(40).artist > 0);
        assert_eq!(row_layout(40).album, 0);
        assert!(row_layout(80).album > 0);
    }

    #[test]
    fn rows_never_exceed_the_pane_width() {
        for width in [4usize, 10, 19, 20, 30, 40, 60, 79, 120] {
            let track = Track::from_path(PathBuf::from("/m/a-very-long-file-name.mp3"));
            let line = row_line(&track, RowFlags::default(), width);
            let used: usize = line
                .spans
                .iter()
                .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            assert!(used <= width, "width {width} produced {used}: {line:?}");
        }
    }

    #[test]
    fn markers_distinguish_playing_selected_and_corrupt() {
        let mut corrupt = track("a", "b", "c");
        corrupt.corrupt = true;
        assert_eq!(marker(RowFlags::default(), true), '!');
        assert_eq!(
            marker(
                RowFlags {
                    playing: true,
                    ..Default::default()
                },
                false
            ),
            '▶'
        );
        assert_eq!(
            marker(
                RowFlags {
                    playing: true,
                    ascii: true,
                    ..Default::default()
                },
                false
            ),
            '>'
        );
        let line = row_line(&corrupt, RowFlags::default(), 80);
        assert!(plain(&line).contains("[unreadable]"));
    }

    #[test]
    fn visible_window_is_a_slice_of_the_library() {
        let mut scroll = Scroll::new();
        scroll.set_content(1000);
        scroll.set_visible(3);
        scroll.scroll_to(500);
        assert_eq!(visible_window(&scroll, 1000), (500, 503));
        scroll.scroll_to(999);
        assert_eq!(visible_window(&scroll, 1000), (997, 1000));
        assert_eq!(visible_window(&Scroll::new(), 0), (0, 0));
    }
}
