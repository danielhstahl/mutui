//! Natural, case-insensitive, Unicode-aware comparison and the total sort key.
//!
//! This is the normative implementation of `docs/adr/0002-sort-and-artist-run.md`.
//! Comparison allocates nothing: tokens are borrowed sub-slices of the
//! pre-computed, already-normalized key strings.

use std::cmp::Ordering;

/// Sentinel used when a file has no track number (untagged files land last
/// inside their album, see the sort spec §3).
pub const TRACK_MISSING: u32 = u32::MAX;

/// Normalize a tag value for comparison and for artist-run equality:
/// trim, collapse internal whitespace runs to a single space, lowercase
/// (Unicode-aware). `"the   BEATLES "` and `"The Beatles"` normalize to the
/// same string.
#[must_use]
pub fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
    }
    out
}

/// Tokens produced by [`NaturalTokens`].
#[derive(Debug, PartialEq, Eq)]
enum Token<'a> {
    /// A run of ASCII digits, compared by numeric value.
    Num(&'a str),
    /// Any other run of characters, compared case-insensitively.
    Txt(&'a str),
}

/// Splits a string into alternating digit / non-digit runs without allocating.
#[derive(Debug)]
struct NaturalTokens<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> NaturalTokens<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }
}

impl<'a> Iterator for NaturalTokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.src.len() {
            return None;
        }
        let rest = &self.src[self.pos..];
        let bytes = rest.as_bytes();
        let is_digit = bytes[0].is_ascii_digit();
        let end = rest
            .char_indices()
            .find(|(_, c)| c.is_ascii_digit() != is_digit)
            .map_or(rest.len(), |(i, _)| i);
        let token = &self.src[self.pos..self.pos + end];
        self.pos += end;
        Some(if is_digit {
            Token::Num(token)
        } else {
            Token::Txt(token)
        })
    }
}

/// Compare two digit runs numerically, so that `2 < 10` regardless of how many
/// digits each has, and `1 < 01` (fewer leading zeros first) for a total order.
fn numeric_cmp(a: &str, b: &str) -> Ordering {
    let ta = a.trim_start_matches('0');
    let tb = b.trim_start_matches('0');
    ta.len()
        .cmp(&tb.len())
        .then_with(|| ta.cmp(tb))
        // numeric tie (1 vs 01): the shorter, zero-free spelling wins
        .then_with(|| a.len().cmp(&b.len()))
}

/// Compare two non-numeric runs case-insensitively and Unicode-aware.
fn text_cmp(a: &str, b: &str) -> Ordering {
    let ia = a.chars().flat_map(char::to_lowercase);
    let ib = b.chars().flat_map(char::to_lowercase);
    ia.cmp(ib)
}

/// Natural (numeric-aware, case-insensitive) comparison of two strings.
///
/// ```
/// use music_tui::model::natural_cmp;
/// assert_eq!(natural_cmp("track 2", "track 10"), std::cmp::Ordering::Less);
/// assert_eq!(natural_cmp("artist a", "artist B"), std::cmp::Ordering::Less);
/// ```
#[must_use]
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ta = NaturalTokens::new(a);
    let mut tb = NaturalTokens::new(b);
    loop {
        match (ta.next(), tb.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(Token::Num(x)), Some(Token::Num(y))) => match numeric_cmp(x, y) {
                Ordering::Equal => continue,
                other => return other,
            },
            (Some(Token::Txt(x)), Some(Token::Txt(y))) => match text_cmp(x, y) {
                Ordering::Equal => continue,
                other => return other,
            },
            // Digit runs sort before text runs. This never triggers in the sort
            // key because the fields are compared independently, but it keeps
            // the function a total order on arbitrary input.
            (Some(Token::Num(_)), Some(Token::Txt(_))) => return Ordering::Less,
            (Some(Token::Txt(_)), Some(Token::Num(_))) => return Ordering::Greater,
        }
    }
}

/// The pre-computed sort key of a track (sort spec §2/§3).
///
/// Comparison priority: artist, album, disc, track, year, file name, full path.
/// Missing tags use the sentinels fixed by the spec: empty string (artist,
/// album), `1` (disc), [`TRACK_MISSING`] (track), `0` (year).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SortKey {
    /// Normalized `album_artist` else `artist` (`""` when untagged).
    pub artist: String,
    /// Normalized album title (`""` when untagged).
    pub album: String,
    /// Disc number, `1` when missing.
    pub disc: u32,
    /// Track number, [`TRACK_MISSING`] when missing.
    pub track: u32,
    /// Release year, `0` when missing (sorts first inside its group).
    pub year: i64,
    /// File name — first deterministic tiebreaker.
    pub name: String,
    /// Full path — final deterministic tiebreaker.
    pub path: String,
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        natural_cmp(&self.artist, &other.artist)
            .then_with(|| natural_cmp(&self.album, &other.album))
            .then_with(|| self.disc.cmp(&other.disc))
            .then_with(|| self.track.cmp(&other.track))
            .then_with(|| self.year.cmp(&other.year))
            .then_with(|| natural_cmp(&self.name, &other.name))
            .then_with(|| natural_cmp(&self.path, &other.path))
    }
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Sort a library into the canonical order. Uses a stable sort and a key that
/// ends in the full path, so the result never changes between runs.
pub fn sort_tracks(tracks: &mut [crate::model::Track]) {
    tracks.sort_by(|a, b| a.key.cmp(&b.key));
}

/// Human readable dump of the sorted order, used by `--dump-sort` and the
/// integration tests.
#[must_use]
pub fn dump_sorted_order(tracks: &[crate::model::Track]) -> String {
    let mut out = String::new();
    for (i, t) in tracks.iter().enumerate() {
        let _ = writeln::write_line(
            &mut out,
            format_args!(
                "{:>4}  {:<24} {:<20} d{:>2} t{:<4} {:<6} {}\n",
                i + 1,
                crate::ui::truncate::clip(&t.key.artist, 24),
                crate::ui::truncate::clip(&t.key.album, 20),
                t.key.disc,
                if t.key.track == TRACK_MISSING {
                    "-".to_string()
                } else {
                    t.key.track.to_string()
                },
                if t.key.year == 0 {
                    String::new()
                } else {
                    t.key.year.to_string()
                },
                t.path.display()
            ),
        );
    }
    out
}

mod writeln {
    use std::fmt;
    pub fn write_line(out: &mut String, args: fmt::Arguments<'_>) -> fmt::Result {
        fmt::Write::write_fmt(out, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_numeric_ordering() {
        assert_eq!(natural_cmp("2", "10"), Ordering::Less);
        assert_eq!(natural_cmp("track 2", "track 10"), Ordering::Less);
        assert_eq!(natural_cmp("album 1", "album 10"), Ordering::Less);
        assert_eq!(natural_cmp("album 2", "album 10"), Ordering::Less);
        // Fewer leading zeros wins the tie-break, so ordering stays total.
        assert_eq!(natural_cmp("d2", "d02"), Ordering::Less);
    }

    #[test]
    fn case_insensitive_and_unicode() {
        assert_eq!(natural_cmp("artist a", "artist B"), Ordering::Less);
        // Case folding only: no locale collation, so non-ASCII sorts after ASCII.
        assert_eq!(natural_cmp("\u{00C4}rzte", "B"), Ordering::Greater);
        assert_eq!(natural_cmp("apple", "\u{00C4}rzte"), Ordering::Less);
        assert_eq!(natural_cmp("a", "A"), Ordering::Equal);
        assert_eq!(natural_cmp("straße", "STRASSE"), Ordering::Greater);
    }

    #[test]
    fn total_order_properties() {
        let samples = [
            "", "a", "A", "a1", "a01", "a2", "a10", "A 2", "b", "1", "01", "10", "ä", "z z",
        ];
        for a in samples {
            for b in samples {
                let ab = natural_cmp(a, b);
                let ba = natural_cmp(b, a);
                assert_eq!(ab, ba.reverse(), "antisymmetry {a:?} {b:?}");
                for c in samples {
                    // transitivity
                    if ab == Ordering::Less && natural_cmp(b, c) == Ordering::Less {
                        assert_eq!(natural_cmp(a, c), Ordering::Less, "{a:?}{b:?}{c:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn normalize_collapses_whitespace_and_case() {
        assert_eq!(normalize("  the   BEATLES \n"), "the beatles");
        assert_eq!(normalize("The Beatles"), normalize("the    beatles"));
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn tokens_are_allocation_free_runs() {
        let got: Vec<Token<'_>> = NaturalTokens::new("ab12c3").collect();
        assert_eq!(
            got,
            vec![
                Token::Txt("ab"),
                Token::Num("12"),
                Token::Txt("c"),
                Token::Num("3")
            ]
        );
        assert_eq!(NaturalTokens::new("").next(), None);
    }
}
