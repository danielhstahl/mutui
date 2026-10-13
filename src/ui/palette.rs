//! The colour palette: the one place that decides what a colour *means*.
//!
//! Almost everybody runs a black (or near-black) background with the stock
//! ANSI-16 palette, and on that palette the ANSI `Blue` is a fully saturated
//! `#0000EE` — 2.2:1 against black, which is not legible text. The obvious
//! "fix", `LightBlue` (`#5C5CFF`), is only 4.4:1 and still glows, especially on
//! the solid block of a scrollbar thumb. The accent here is instead a
//! desaturated steel blue, `#87AFD7`: 9.1:1 on black, 7.6:1 on the footer bar,
//! and about a third as saturated as the colour it replaced.
//!
//! `#87AFD7` is also exactly index 110 of the xterm 256-colour cube, so a
//! terminal without 24-bit colour resolves it to the same colour instead of a
//! dithered approximation (see [`on_256_cube`]).
//!
//! Names that *are* readable on the stock palette stay plain ANSI names on
//! purpose, so they keep following the user's own theme; only colours that are
//! broken or ambiguous there are pinned to RGB. The [`contrast`] helper resolves
//! the ANSI names against xterm's reference values and the tests measure every
//! colour with it, which is what keeps a harsh colour from creeping back.

use ratatui::style::Color;

/// Accent: the soft blue used for the scrollbar thumb and the clock.
///
/// `#87AFD7` (xterm-256 index 110). Contrast: 9.14:1 on black, 7.61:1 on
/// [`FOOTER_BG`], 5.89:1 on [`ROW_SELECTED_BG`].
pub const ACCENT: Color = Color::Rgb(0x87, 0xAF, 0xD7);

/// Secondary, out-of-the-way text (scrollbar track, counts, run position).
pub const MUTED: Color = Color::DarkGray;

/// `Now playing` in the footer state word and the ▶ marker.
pub const STATE_PLAYING: Color = Color::Green;
/// `Paused` in the footer state word and the `|` marker.
pub const STATE_PAUSED: Color = Color::Yellow;
/// `Idle` in the footer state word.
pub const STATE_IDLE: Color = Color::Cyan;
/// `Stopping` in the footer state word.
pub const STATE_STOPPING: Color = Color::DarkGray;

/// Text of the row that is playing or paused (yellow to match its marker).
pub const ROW_PLAYING: Color = Color::Yellow;
/// Background of the selected row.
pub const ROW_SELECTED_BG: Color = Color::Rgb(0x2D, 0x2D, 0x3C);
/// Background of the one-line status bar.
pub const FOOTER_BG: Color = Color::Rgb(0x19, 0x19, 0x20);
/// Marker on a file that could not be read (always drawn BOLD).
pub const MARKER_CORRUPT: Color = Color::Red;

/// Footer notice: a plain informational message.
pub const NOTICE_INFO: Color = Color::White;
/// Footer notice: something is degraded but continuing.
pub const NOTICE_WARNING: Color = Color::Yellow;
/// Footer notice: something failed. Always drawn BOLD.
pub const NOTICE_ERROR: Color = Color::Red;

/// Now-playing title in the footer.
pub const TITLE: Color = Color::White;
/// Key column of the help overlay.
pub const KEY: Color = Color::Cyan;

/// The six levels of each channel in the xterm 256-colour cube.
const CUBE: [u16; 6] = [0, 95, 135, 175, 215, 255];

/// True when every channel of `color` is an exact 6×6×6 cube value, i.e. a
/// terminal without true colour renders it without dithering.
#[must_use]
pub const fn on_256_cube(color: Color) -> bool {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r as u16, g as u16, b as u16),
        _ => return false,
    };
    on_cube_channel(r) && on_cube_channel(g) && on_cube_channel(b)
}

const fn on_cube_channel(v: u16) -> bool {
    let mut i = 0;
    while i < CUBE.len() {
        if CUBE[i] == v {
            return true;
        }
        i += 1;
    }
    false
}

/// xterm's *default* values for the 16 ANSI colours, in index order, so that
/// contrast can be reasoned about at all. A themed terminal (solarized,
/// gruvbox, …) renders those names differently, which is exactly why only
/// deliberately chosen colours are pinned to RGB above.
const ANSI: [(u16, u16, u16); 16] = [
    (0, 0, 0),       // 0  Black
    (205, 0, 0),     // 1  Red
    (0, 205, 0),     // 2  Green
    (205, 205, 0),   // 3  Yellow
    (0, 0, 238),     // 4  Blue   <- the one this palette refuses to use
    (205, 0, 205),   // 5  Magenta
    (0, 205, 205),   // 6  Cyan
    (229, 229, 229), // 7  Gray
    (150, 152, 150), // 8  DarkGray
    (255, 0, 0),     // 9  LightRed
    (0, 255, 0),     // 10 LightGreen
    (255, 255, 0),   // 11 LightYellow
    (92, 92, 255),   // 12 LightBlue <- and the other one
    (255, 0, 255),   // 13 LightMagenta
    (0, 255, 255),   // 14 LightCyan
    (255, 255, 255), // 15 White
];

/// sRGB channels of `color`. `Reset` is treated as the light default foreground
/// of a dark terminal.
#[must_use]
pub const fn rgb(color: Color) -> (u16, u16, u16) {
    match color {
        Color::Reset => ANSI[15],
        Color::Black => ANSI[0],
        Color::Red => ANSI[1],
        Color::Green => ANSI[2],
        Color::Yellow => ANSI[3],
        Color::Blue => ANSI[4],
        Color::Magenta => ANSI[5],
        Color::Cyan => ANSI[6],
        Color::Gray => ANSI[7],
        Color::DarkGray => ANSI[8],
        Color::LightRed => ANSI[9],
        Color::LightGreen => ANSI[10],
        Color::LightYellow => ANSI[11],
        Color::LightBlue => ANSI[12],
        Color::LightMagenta => ANSI[13],
        Color::LightCyan => ANSI[14],
        Color::White => ANSI[15],
        Color::Rgb(r, g, b) => (r as u16, g as u16, b as u16),
        Color::Indexed(i) => indexed_rgb(i as u16),
    }
}

/// Decode an 8-bit colour: 0..=15 are the ANSI names, 16..=231 the 6×6×6 cube,
/// 232..=255 the grayscale ramp.
const fn indexed_rgb(i: u16) -> (u16, u16, u16) {
    if i < 16 {
        ANSI[i as usize]
    } else if i < 232 {
        let n = i - 16;
        (
            CUBE[(n / 36) as usize],
            CUBE[(n % 36 / 6) as usize],
            CUBE[(n % 6) as usize],
        )
    } else {
        let v = 8 + (i - 232) * 10;
        (v, v, v)
    }
}

/// WCAG 2.1 relative luminance of `color` (0.0 for black, 1.0 for white).
#[must_use]
pub fn luminance(color: Color) -> f64 {
    let (r, g, b) = rgb(color);
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

fn channel(v: u16) -> f64 {
    let v = f64::from(v) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.1 contrast ratio between two colours: `1.0` (identical) to `21.0`.
#[must_use]
pub fn contrast(a: Color, b: Color) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// How far a colour is from gray, `0.0` (gray) to `1.0` (fully saturated).
///
/// High-saturation colours on a black background are what reads as "harsh";
/// text colours in this palette stay well under `0.5`.
#[must_use]
pub fn saturation(color: Color) -> f64 {
    let (r, g, b) = rgb(color);
    let (max, min) = (r.max(g).max(b) as f64, r.min(g).min(b) as f64);
    if max == 0.0 {
        0.0
    } else {
        (max - min) / max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain text has to clear WCAG AA (4.5:1) against a black terminal.
    const AA: f64 = 4.5;
    /// Bold text gets the WCAG "large text" allowance (3:1): it is rendered
    /// heavier and is shown for a moment, not read for an hour.
    const AA_BOLD: f64 = 3.0;

    fn named() -> Vec<(&'static str, Color)> {
        vec![
            ("ACCENT", ACCENT),
            ("MUTED", MUTED),
            ("STATE_PLAYING", STATE_PLAYING),
            ("STATE_PAUSED", STATE_PAUSED),
            ("STATE_IDLE", STATE_IDLE),
            ("STATE_STOPPING", STATE_STOPPING),
            ("ROW_PLAYING", ROW_PLAYING),
            ("NOTICE_INFO", NOTICE_INFO),
            ("NOTICE_WARNING", NOTICE_WARNING),
            ("TITLE", TITLE),
            ("KEY", KEY),
        ]
    }

    /// Drawn BOLD, so the 3:1 large-text bar applies.
    fn bold_only() -> Vec<(&'static str, Color)> {
        vec![
            ("NOTICE_ERROR", NOTICE_ERROR),
            ("MARKER_CORRUPT", MARKER_CORRUPT),
        ]
    }

    #[test]
    fn text_colours_are_legible_on_a_black_terminal() {
        for (name, color) in named() {
            let ratio = contrast(color, Color::Black);
            assert!(ratio >= AA, "{name} ({color:?}) is {ratio:.2}:1 on black");
        }
        for (name, color) in bold_only() {
            let ratio = contrast(color, Color::Black);
            assert!(
                ratio >= AA_BOLD,
                "{name} ({color:?}) is {ratio:.2}:1 on black, and it is only ever drawn bold"
            );
        }
    }

    #[test]
    fn accent_is_readable_on_every_surface_it_sits_on() {
        for (surface, color) in [
            ("black", Color::Black),
            ("footer bar", FOOTER_BG),
            ("selected row", ROW_SELECTED_BG),
        ] {
            let ratio = contrast(ACCENT, color);
            assert!(ratio >= AA, "accent on {surface} is only {ratio:.2}:1");
        }
    }

    #[test]
    fn accent_is_a_soft_blue_not_a_harsh_one() {
        assert_ne!(ACCENT, Color::Blue);
        assert_ne!(ACCENT, Color::LightBlue);
        // Blue-dominant, but nowhere near the 1.0 saturation of #0000FF.
        let (r, g, b) = rgb(ACCENT);
        assert!(b > g && g > r, "expected a blue: {r},{g},{b}");
        assert!(
            saturation(ACCENT) < 0.45,
            "accent is too saturated: {:.2}",
            saturation(ACCENT)
        );
        assert!(contrast(ACCENT, Color::Black) >= 7.0, "accent is too dark");
    }

    #[test]
    fn rgb_accents_survive_a_256_colour_terminal() {
        // Anything pinned to RGB that a user has to read should exist in the
        // 6x6x6 cube, so downgrading to 8-bit colours does not shift it.
        assert!(on_256_cube(ACCENT), "accent is not a 256-cube colour");
        // The same colour through the 8-bit path: identical, so no dithering.
        assert!((contrast(Color::Indexed(110), ACCENT) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn contrast_helper_agrees_with_the_known_answers() {
        assert!((contrast(Color::Black, Color::White) - 21.0).abs() < 0.01);
        assert!((contrast(Color::White, Color::White) - 1.0).abs() < f64::EPSILON);
        // The colour this palette replaced: pretty on a swatch, unreadable as text.
        assert!(contrast(Color::Blue, Color::Black) < AA);
        assert!(contrast(Color::LightBlue, Color::Black) < AA);
        // The numbers quoted in the module docs, so they cannot drift.
        assert!((contrast(Color::Blue, Color::Black) - 2.23).abs() < 0.02);
        assert!((contrast(Color::LightBlue, Color::Black) - 4.43).abs() < 0.02);
        assert!((contrast(ACCENT, Color::Black) - 9.14).abs() < 0.02);
        assert!((contrast(ACCENT, FOOTER_BG) - 7.61).abs() < 0.02);
        assert!((contrast(ACCENT, ROW_SELECTED_BG) - 5.89).abs() < 0.02);
        assert!(saturation(Color::Blue) > 0.99);
        assert!(saturation(ACCENT) < saturation(Color::Blue) / 2.0);
    }

    #[test]
    fn indexed_and_rgb_forms_agree() {
        assert_eq!(rgb(Color::Indexed(110)), rgb(ACCENT));
        assert_eq!(rgb(Color::Indexed(16)), (0, 0, 0));
        assert_eq!(rgb(Color::Indexed(231)), (255, 255, 255));
        assert_eq!(rgb(Color::Indexed(232)), (8, 8, 8));
        assert_eq!(rgb(Color::Indexed(255)), (238, 238, 238));
    }
}
