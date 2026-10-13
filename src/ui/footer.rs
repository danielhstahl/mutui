//! The one-line status bar.
//!
//! [`parts`] turns application state into prioritised, styled segments and
//! [`compose`] packs them into the available width. Both are pure: the footer
//! never panics at 20 columns and never corrupts the layout.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::UnicodeWidthStr as _;

use crate::app::state::{AppState, NoticeLevel, Phase, PlaybackState};

use super::palette;
use super::truncate::clip;

/// One candidate footer segment.
#[derive(Debug, Clone)]
pub struct Part {
    /// Text to show.
    pub text: String,
    /// Minimum cells worth reserving; below that the part is dropped.
    pub min: usize,
    /// Style.
    pub style: Style,
}

impl Part {
    fn new(text: impl Into<String>, min: usize, style: Style) -> Self {
        Self {
            text: text.into(),
            min,
            style,
        }
    }
}

const STATE_COLORS: (Color, Color, Color) = (
    palette::STATE_PLAYING,
    palette::STATE_PAUSED,
    palette::STATE_IDLE,
);

fn state_word(state: &AppState) -> String {
    if state.phase == Phase::Scanning {
        return "Scanning".to_string();
    }
    match state.playback {
        PlaybackState::Playing { .. } => "Playing".to_string(),
        PlaybackState::Paused { .. } => "Paused".to_string(),
        PlaybackState::Stopping => "Stopping".to_string(),
        PlaybackState::Idle if state.end_of_artist.is_some() => "End of artist".to_string(),
        PlaybackState::Idle => "Idle".to_string(),
    }
}

fn state_style(state: &AppState) -> Style {
    let (playing, paused, idle) = STATE_COLORS;
    let color = match state.playback {
        PlaybackState::Playing { .. } => playing,
        PlaybackState::Paused { .. } => paused,
        PlaybackState::Stopping => palette::STATE_STOPPING,
        _ => idle,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn fmt_duration(total: u64) -> String {
    format!("{}:{:02}", total / 60, total % 60)
}

/// Build the footer segments in decreasing priority (first = most important).
#[must_use]
pub fn parts(state: &AppState) -> Vec<Part> {
    let mut parts = vec![Part::new(state_word(state), 4, state_style(state))];

    if let Some(notice) = state.notice.as_ref().filter(|n| n.is_live(state.now)) {
        let style = match notice.level {
            NoticeLevel::Info => Style::default().fg(palette::NOTICE_INFO),
            NoticeLevel::Warning => Style::default().fg(palette::NOTICE_WARNING),
            NoticeLevel::Error => Style::default()
                .fg(palette::NOTICE_ERROR)
                .add_modifier(Modifier::BOLD),
        };
        parts.push(Part::new(notice.text.clone(), 8, style));
    }

    let focused = state
        .playlist
        .current()
        .or_else(|| state.playlist.cursor().and_then(|c| state.playlist.get(c)));
    if let Some(track) = focused {
        let label = format!("{} - {}", track.display_artist(), track.title);
        let style = if state.playback.is_active() {
            Style::default()
                .fg(palette::TITLE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette::MUTED)
        };
        parts.push(Part::new(label, 8, style));
    }

    let total = state
        .playlist
        .current()
        .and_then(|t| t.duration)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if state.playback.is_active() || total > 0 {
        let elapsed = state.position.as_secs();
        parts.push(Part::new(
            format!(
                "{}{}",
                fmt_duration(elapsed),
                if total > 0 {
                    format!("/{}", fmt_duration(total))
                } else {
                    String::new()
                }
            ),
            5,
            Style::default().fg(palette::ACCENT),
        ));
    }

    if let Some((position, length)) = state.playlist.run_position() {
        parts.push(Part::new(
            format!("song {position}/{length} for this artist"),
            5,
            Style::default().fg(palette::MUTED),
        ));
    }

    match state.phase {
        Phase::Scanning => {
            let label = state.scan.label();
            if !label.is_empty() {
                parts.push(Part::new(label, 8, Style::default().fg(palette::MUTED)));
            }
        }
        Phase::Ready => {
            let corrupt = state
                .scan
                .report
                .as_ref()
                .map(|r| r.failed.len())
                .unwrap_or(0);
            let mut label = format!("{} songs", state.playlist.len());
            if corrupt > 0 {
                label.push_str(&format!(", {corrupt} unreadable"));
            }
            if state.end_of_artist.is_none() && state.notice.is_none() {
                parts.push(Part::new(
                    label,
                    6,
                    Style::default().add_modifier(Modifier::DIM),
                ));
            }
        }
    }

    parts.push(Part::new(
        format!("[?] help · {}", state.opts.backend),
        6,
        Style::default().add_modifier(Modifier::DIM),
    ));
    parts
}

/// Pack prioritised parts into `width` cells, clipping what does not fit.
#[must_use]
pub fn compose(parts: &[Part], width: usize, ascii: bool) -> Line<'static> {
    if width == 0 {
        return Line::from("");
    }
    let separator = if ascii { " | " } else { " \u{2502} " };
    // Both separators are three cells wide; the width is a constant on purpose.
    let sep_width = 3;
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(parts.len());
    let mut used = 0usize;
    for part in parts {
        let gap = if spans.is_empty() { 0 } else { sep_width };
        let Some(remaining) = width.checked_sub(used + gap) else {
            continue;
        };
        if remaining < part.min {
            continue;
        }
        let text = clip(&part.text, remaining);
        if !spans.is_empty() {
            spans.push(Span::styled(
                separator.to_string(),
                Style::default().add_modifier(Modifier::DIM),
            ));
            used += gap;
        }
        used += text.width();
        spans.push(Span::styled(text, part.style));
        if used >= width {
            break;
        }
    }
    if spans.is_empty() {
        return Line::from(Span::raw(clip(
            &parts.first().map(|p| p.text.clone()).unwrap_or_default(),
            width,
        )));
    }
    Line::from(spans)
}

/// Render the footer into `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let line = compose(&parts(state), usize::from(area.width), state.opts.ascii);
    let paragraph = Paragraph::new(line).style(Style::default().bg(palette::FOOTER_BG));
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{Notice, Options};
    use crate::model::{fixture_tracks, sort_tracks, Track};

    fn state(width_hint: u16) -> AppState {
        let mut tracks = fixture_tracks();
        sort_tracks(&mut tracks);
        let mut app = AppState::new(Options {
            backend: "mpg123".into(),
            ..Default::default()
        });
        app.set_library(tracks);
        app.scroll.set_visible(usize::from(width_hint));
        app.set_size(width_hint, 3);
        app
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn fits_every_width_from_20_up() {
        for width in [20usize, 40, 80, 120, 200] {
            let line = compose(&parts(&state(20)), width, false);
            let text = text_of(&line);
            assert!(
                unicode_width::UnicodeWidthStr::width(text.as_str()) <= width,
                "width {width} overflowed: {text:?} ({} cols)",
                unicode_width::UnicodeWidthStr::width(text.as_str())
            );
        }
    }

    #[test]
    fn the_clock_uses_the_soft_accent_blue() {
        let mut app = state(120);
        app.phase = crate::app::state::Phase::Ready;
        app.playlist.select_start(5);
        app.playback = PlaybackState::Playing {
            track_id: 1,
            index: 5,
        };
        app.position = std::time::Duration::from_secs(95);
        let clock = parts(&app)
            .into_iter()
            .find(|p| p.text.starts_with("1:35"))
            .expect("no position part");
        assert_eq!(clock.style.fg, Some(palette::ACCENT));
        // The plain ANSI blues are unreadable on black (2.2:1 and 4.4:1).
        assert_ne!(clock.style.fg, Some(Color::Blue));
        assert_ne!(clock.style.fg, Some(Color::LightBlue));
        assert!(palette::contrast(palette::ACCENT, Color::Black) >= 4.5);
    }

    #[test]
    fn shows_now_playing_and_run_position() {
        let mut app = state(120);
        app.phase = crate::app::state::Phase::Ready;
        app.playlist.select_start(5);
        app.playback = PlaybackState::Playing {
            track_id: 1,
            index: 5,
        };
        app.position = std::time::Duration::from_secs(95);
        let line = compose(&parts(&app), 120, false);
        let text = text_of(&line);
        assert!(text.contains("Playing"), "{text}");
        assert!(text.contains("song 3/5"), "{text}");
        assert!(text.contains("1:35"), "{text}");
    }

    #[test]
    fn notices_appear_and_expire() {
        let mut app = state(120);
        app.notice = Some(Notice::error("decoder said no", 1000));
        assert!(text_of(&compose(&parts(&app), 120, false)).contains("decoder said no"));
        app.notice = Some(Notice {
            ttl: Some(std::time::Duration::from_millis(1)),
            ..Notice::error("decoder said no", 0)
        });
        assert!(!app.notice.as_ref().is_some_and(|n| n.is_live(10_000)));
    }

    #[test]
    fn end_of_artist_is_visible() {
        let mut app = state(120);
        app.end_of_artist = Some("Beta Wave".into());
        app.notice = Some(Notice::sticky("End of artist: Beta Wave", 0));
        let text = text_of(&compose(&parts(&app), 120, false));
        assert!(text.contains("End of artist"), "{text}");
    }

    #[test]
    fn long_titles_degrade_to_state_plus_title() {
        let mut app = state(40);
        let mut long = Track::from_path("/m/long.mp3".into());
        long.title = "A".repeat(300);
        long.artist = Some("B".repeat(300));
        long.refresh_key();
        let mut tracks = app.playlist.tracks().to_vec();
        tracks[0] = long;
        app.set_library(tracks);
        app.playlist.select_start(0);
        app.playback = PlaybackState::Playing {
            track_id: 1,
            index: 0,
        };
        let line = compose(&parts(&app), 20, false);
        let text = text_of(&line);
        assert!(text.contains('…'), "{text}");
        assert!(unicode_width::UnicodeWidthStr::width(text.as_str()) <= 20);
    }

    #[test]
    fn ascii_mode_avoids_box_glyphs() {
        let line = compose(&parts(&state(80)), 80, true);
        let text = text_of(&line);
        assert!(!text.contains('\u{2502}'), "{text}");
        assert!(text.contains('|'));
    }
}
