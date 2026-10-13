//! Top-level render: list pane + scrollbar + footer, plus the help overlay.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;

use crate::app::state::AppState;

use super::{footer, list};

/// Every key binding, also used by `--help`.
pub const KEY_BINDINGS: &[(&str, &str)] = &[
    ("click row", "play that song (starts a run for its artist)"),
    ("wheel", "scroll three rows"),
    ("drag scrollbar", "scroll; click the track to jump"),
    ("Enter", "play the selected row"),
    ("Space", "pause / resume"),
    ("Up/Down, j/k", "move selection"),
    ("PageUp/PageDown", "scroll by one page"),
    ("Home / End", "jump to the first / last song"),
    ("n / p", "next / previous song inside the artist run"),
    ("m", "mute / unmute"),
    ("+ / -", "volume up / down"),
    ("r", "rescan the folder"),
    ("? or F1", "toggle this help"),
    ("q, Esc, Ctrl-C", "quit"),
];

/// Render the whole screen.
pub fn draw(frame: &mut Frame, state: &AppState) {
    let areas = state.areas;
    if areas.list.height > 0 {
        list::render(frame, areas.list, state);
        draw_scrollbar(frame, areas.gutter, state);
    }
    if areas.footer.height > 0 {
        footer::render(frame, areas.footer, state);
    }
    if state.show_help {
        draw_help(frame, frame.area(), state);
    }
}

fn draw_scrollbar(frame: &mut Frame, area: Rect, state: &AppState) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // The thumb is proportional to the visible share and positioned by our own
    // offset, which is the same fraction `Scroll::scroll_to_row_in_track` uses
    // when the user clicks or drags the bar (`Scroll::thumb` covers the math).
    let mut scrollbar_state = ScrollbarState::new(state.scroll.content())
        .position(state.scroll.offset())
        .viewport_content_length(usize::from(state.areas.list.height));
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some(if state.opts.ascii { "|" } else { "\u{2502}" }))
        .thumb_symbol(if state.opts.ascii { "#" } else { "\u{2588}" })
        .thumb_style(Style::default().fg(Color::LightBlue))
        .track_style(Style::default().fg(Color::DarkGray));
    frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

fn draw_help(frame: &mut Frame, area: Rect, state: &AppState) {
    // + title, blank line, folder line, trailing blank, plus the two borders.
    let rows = KEY_BINDINGS.len() + 4;
    let height = (rows + 2).min(usize::from(area.height));
    let width = usize::from(area.width).min(76);
    if height < 3 || width < 20 {
        return;
    }
    let popup = centered(Rect::new(0, 0, width as u16, height as u16), area);
    frame.render_widget(Clear, popup);
    let mut lines: Vec<Line<'_>> = vec![Line::from(
        "Keys and mouse — wheel or drag the bar on the right to scroll",
    )];
    lines.push(Line::from(""));
    for (key, what) in KEY_BINDINGS {
        lines.push(Line::from(vec![
            Span::styled(format!("{key:<28}"), Style::default().fg(Color::Cyan)),
            Span::raw(*what),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "folder: {} · backend: {}",
            state.opts.folder.display(),
            state.opts.backend
        ),
        Style::default().add_modifier(Modifier::DIM),
    )));
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(" help "));
    frame.render_widget(paragraph, popup);
}

fn centered(size: Rect, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(size.width) / 2;
    let y = area.y + area.height.saturating_sub(size.height) / 2;
    Rect::new(
        x,
        y,
        size.width.min(area.width),
        size.height.min(area.height),
    )
}

/// Render to plain text lines — the snapshot helper used by the tests.
#[must_use]
pub fn render_lines(state: &AppState, width: u16, height: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    // `Terminal::new` on a test backend cannot fail (Infallible error type).
    let mut terminal = match ratatui::Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => match err {},
    };
    if terminal.draw(|frame| draw(frame, state)).is_err() {
        return vec!["(render failed)".to_string()];
    }
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(usize::from(width))
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        })
        .collect()
}

/// Column of `n` centered constraints helper.
#[must_use]
pub fn centered_constraints(n: u16) -> Vec<Constraint> {
    vec![
        Constraint::Fill(1),
        Constraint::Length(n),
        Constraint::Fill(1),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{Options, PlaybackState};
    use crate::model::{fixture_tracks, sort_tracks};

    fn state(rows: usize, width: u16, height: u16) -> AppState {
        let mut tracks = fixture_tracks();
        tracks.truncate(rows.min(fixture_tracks().len()));
        sort_tracks(&mut tracks);
        let mut app = AppState::new(Options {
            backend: "mpg123".into(),
            ..Default::default()
        });
        app.set_library(tracks);
        app.set_size(width, height);
        app.playlist.select_start(0);
        app.playback = PlaybackState::Playing {
            track_id: 1,
            index: 0,
        };
        app
    }

    #[test]
    fn renders_a_full_window_snapshot() {
        let app = state(22, 100, 12);
        let lines = render_lines(&app, 100, 12);
        assert_eq!(lines.len(), 12);
        let joined = lines.join("\n");
        insta::assert_snapshot!(joined);
    }

    #[test]
    fn renders_with_three_visible_rows_and_a_thousand_songs() {
        let mut app = state(3, 20, 3);
        let many: Vec<_> = app
            .playlist
            .tracks()
            .iter()
            .cloned()
            .cycle()
            .take(1000)
            .collect();
        app.set_library(many);
        app.set_size(20, 3);
        app.scroll.scroll_to(500);
        app.scroll.ensure_visible(500);
        let lines = render_lines(&app, 20, 3);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2].chars().count(), 20);
        // The scrollbar column is drawn on the far right; scrolled to the
        // middle, the thumb sits on the middle row.
        let last_col: Vec<char> = lines
            .iter()
            .map(|l| l.chars().last().unwrap_or(' '))
            .collect();
        assert!(
            last_col.contains(&'\u{2588}'),
            "no thumb in the last column: {last_col:?}"
        );
        assert!(
            last_col.contains(&'\u{2502}') || last_col.contains(&' '),
            "track missing: {last_col:?}"
        );
    }

    #[test]
    fn help_overlay_renders() {
        let mut app = state(5, 90, 20);
        app.show_help = true;
        let lines = render_lines(&app, 90, 20);
        let joined = lines.join("\n");
        assert!(joined.contains("click row"), "{joined}");
    }
}
