//! Natural, case-insensitive, Unicode-aware comparison and the total sort key.
//!
//! This is the normative implementation of `docs/adr/0002-sort-and-artist-run.md`.
//! Comparison allocates nothing: tokens are borrowed sub-slices of the
//! pre-computed, already-normalized key strings. The only allocating step is
//! [`sort_tracks`] working out each album's release year ([`apply_album_years`]),
//! which happens once per scan rather than once per comparison.

use std::cmp::Ordering;
use std::collections::HashMap;

/// Sentinel used when a file has no track number (untagged files land last
/// inside their album, see the sort spec §3).
pub const TRACK_MISSING: u32 = u32::MAX;

/// Version of the *meaning* of a [`SortKey`], i.e. of the rules in
/// `docs/adr/0002-sort-and-artist-run.md`.
///
/// **Bump this whenever the sort or artist-run semantics change.** The library
/// cache (`crate::library_cache`) stamps it into every file it writes, so a
/// cache built by an older algorithm is thrown away instead of quietly keeping
/// the user on the old order — a cached library must never be able to hide a
/// sorting fix (this is what would have hidden `mutu-172`).
pub const SORT_ALGORITHM: u32 = 2;

/// Sentinel used when neither a track nor any of its album-mates carry a year
/// (an undated release lands first inside its artist, see the sort spec §3.2).
pub const YEAR_MISSING: i64 = 0;

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
/// Comparison priority: artist, album year, album, disc, track, year, file
/// name, full path — so an artist's albums play in release order (§3.2) and
/// the tracks inside one album stay together.
///
/// Missing tags use the sentinels fixed by the spec: empty string (artist,
/// album), `1` (disc), [`TRACK_MISSING`] (track), [`YEAR_MISSING`] (year,
/// album year).
///
/// [`SortKey::album_year`] is a property of the *album*, not of the row: it is
/// filled in for the whole library by [`sort_tracks`], which is the only way to
/// know every year written on an album.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SortKey {
    /// Normalized `album_artist` else `artist` (`""` when untagged).
    pub artist: String,
    /// Earliest year found anywhere on this row's album, [`YEAR_MISSING`] when
    /// the album is undated. Written by [`sort_tracks`].
    pub album_year: i64,
    /// Normalized album title (`""` when untagged).
    pub album: String,
    /// Disc number, `1` when missing.
    pub disc: u32,
    /// Track number, [`TRACK_MISSING`] when missing.
    pub track: u32,
    /// Release year of this row, `0` when missing (breaks ties inside an album).
    pub year: i64,
    /// File name — first deterministic tiebreaker.
    pub name: String,
    /// Full path — final deterministic tiebreaker.
    pub path: String,
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        natural_cmp(&self.artist, &other.artist)
            // Albums are ordered by when they came out, not alphabetically;
            // the album title only breaks ties between same-year releases.
            .then_with(|| self.album_year.cmp(&other.album_year))
            .then_with(|| natural_cmp(&self.album, &other.album))
            .then_with(|| self.disc.cmp(&other.disc))
            .then_with(|| self.track.cmp(&other.track))
            .then_with(|| self.year.cmp(&other.year))
            .then_with(|| natural_cmp(&self.name, &other.name))
            .then_with(|| natural_cmp(&self.path, &other.path))
    }
}

/// Give every row the release year of its album (sort spec §3.2).
///
/// An *album* is the pair (sort artist, album title); its year is the earliest
/// non-zero year written on any of its rows, `0` when the album carries no year
/// at all. Deriving it per row would split an album whose discs disagree on the
/// year — or whose files are partly untagged — into as many pieces as it has
/// distinct years, so the value has to be derived from the whole library at
/// once. This is the only part of sorting that allocates; the comparator itself
/// never does.
pub fn apply_album_years(tracks: &mut [crate::model::Track]) {
    let years = album_years(tracks);
    for (track, year) in tracks.iter_mut().zip(years) {
        track.key.album_year = year;
    }
}

/// Earliest non-zero year of each row's album, one value per row (index-matched
/// to `tracks`). Split out so the borrows of the lookup map end before the rows
/// are written back.
fn album_years(tracks: &[crate::model::Track]) -> Vec<i64> {
    let mut earliest: HashMap<(&str, &str), i64> = HashMap::new();
    for track in tracks {
        let year = track.key.year;
        if year == YEAR_MISSING {
            continue;
        }
        earliest
            .entry((track.key.artist.as_str(), track.key.album.as_str()))
            .and_modify(|seen| *seen = (*seen).min(year))
            .or_insert(year);
    }
    tracks
        .iter()
        .map(|track| {
            earliest
                .get(&(track.key.artist.as_str(), track.key.album.as_str()))
                .copied()
                .unwrap_or(YEAR_MISSING)
        })
        .collect()
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Sort a library into the canonical order. Uses a stable sort and a key that
/// ends in the full path, so the result never changes between runs.
///
/// This is the function that establishes the album-year key ([`apply_album_years`]
/// runs first), so a key built by [`crate::model::Track::refresh_key`] alone is
/// only *almost* comparable: always sort before relying on album order.
pub fn sort_tracks(tracks: &mut [crate::model::Track]) {
    apply_album_years(tracks);
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
                if t.key.year == YEAR_MISSING {
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

    /// One row for the album-year tests below.
    fn song(
        artist: &str,
        album: &str,
        disc: u32,
        track_no: u32,
        year: Option<i32>,
        file: &str,
    ) -> crate::model::Track {
        let mut track =
            crate::model::Track::from_path(std::path::PathBuf::from("/music").join(file));
        track.artist = Some(artist.to_string());
        track.album = Some(album.to_string());
        track.disc_no = Some(disc);
        track.track_no = Some(track_no);
        track.year = year;
        track.refresh_key();
        track
    }

    fn albums(tracks: &[crate::model::Track]) -> Vec<String> {
        tracks
            .iter()
            .map(|t| t.display_album().to_string())
            .collect()
    }

    fn album_years(tracks: &[crate::model::Track]) -> Vec<i64> {
        tracks.iter().map(|t| t.key.album_year).collect()
    }

    #[test]
    fn albums_order_by_year_inside_an_artist() {
        // Titles chosen so that alphabetical order is the reverse of release
        // order: sorting by title alone (the old key order) gets this wrong.
        let mut tracks = vec![
            song("Delta", "Zebra Years", 1, 1, Some(2001), "a.mp3"),
            song("Delta", "Middle Years", 1, 1, Some(2008), "b.mp3"),
            song("Delta", "Alpha Years", 1, 1, Some(2015), "c.mp3"),
        ];
        tracks.reverse();
        sort_tracks(&mut tracks);
        assert_eq!(
            albums(&tracks),
            ["Zebra Years", "Middle Years", "Alpha Years"]
        );
        assert_eq!(album_years(&tracks), [2001, 2008, 2015]);
    }

    #[test]
    fn an_album_stays_whole_when_its_rows_disagree_on_the_year() {
        let mut tracks = vec![
            song("Echo", "Warm Up", 1, 1, Some(1988), "d.mp3"),
            song("Echo", "Box Set", 1, 1, Some(1990), "a.mp3"),
            song("Echo", "Box Set", 2, 1, Some(1992), "b.mp3"),
            song("Echo", "Box Set", 2, 2, None, "c.mp3"),
        ];
        sort_tracks(&mut tracks);
        // The undated disc-2 track must not escape to the front of the artist,
        // and the box set must not split around "Warm Up".
        assert_eq!(
            albums(&tracks),
            ["Warm Up", "Box Set", "Box Set", "Box Set"]
        );
        assert_eq!(album_years(&tracks), [1988, 1990, 1990, 1990]);
        let numbers: Vec<(u32, u32)> = tracks
            .iter()
            .skip(1)
            .map(|t| (t.key.disc, t.key.track))
            .collect();
        assert_eq!(
            numbers,
            [(1, 1), (2, 1), (2, 2)],
            "disc then track inside an album"
        );
    }

    #[test]
    fn undated_albums_come_first_inside_the_artist_then_sort_by_title() {
        let mut tracks = vec![
            song("Foxtrot", "Dated", 1, 1, Some(1999), "a.mp3"),
            song("Foxtrot", "Undated Two", 1, 1, None, "b.mp3"),
            song("Foxtrot", "Undated One", 1, 1, None, "c.mp3"),
        ];
        sort_tracks(&mut tracks);
        assert_eq!(
            albums(&tracks),
            ["Undated One", "Undated Two", "Dated"],
            "year 0 is the missing sentinel, so an undated release leads"
        );
    }

    #[test]
    fn same_year_albums_fall_back_to_natural_title_order() {
        let mut tracks = vec![
            song("Gina", "Works 10", 1, 1, Some(2004), "a.mp3"),
            song("Gina", "Works 2", 1, 1, Some(2004), "b.mp3"),
        ];
        sort_tracks(&mut tracks);
        assert_eq!(albums(&tracks), ["Works 2", "Works 10"]);
    }

    #[test]
    fn album_year_is_scoped_to_its_own_artist() {
        // Two artists, one album title: neither may borrow the other's year.
        let mut tracks = vec![
            song("Artist A", "Self Titled", 1, 1, Some(2010), "a.mp3"),
            song("Artist B", "Self Titled", 1, 1, Some(1990), "b.mp3"),
        ];
        sort_tracks(&mut tracks);
        assert_eq!(albums(&tracks), ["Self Titled", "Self Titled"]);
        assert_eq!(
            album_years(&tracks),
            [2010, 1990],
            "artists sort first, and each 'Self Titled' keeps its own year"
        );
    }

    #[test]
    fn sorting_twice_is_a_no_op_for_album_years_too() {
        let mut tracks = vec![
            song("Delta", "Zebra Years", 1, 1, Some(2001), "a.mp3"),
            song("Delta", "Alpha Years", 1, 1, Some(2015), "b.mp3"),
            song("Delta", "Alpha Years", 1, 2, None, "c.mp3"),
        ];
        sort_tracks(&mut tracks);
        let once = albums(&tracks);
        let years = album_years(&tracks);
        apply_album_years(&mut tracks);
        sort_tracks(&mut tracks);
        assert_eq!(albums(&tracks), once);
        assert_eq!(album_years(&tracks), years);
    }
}
