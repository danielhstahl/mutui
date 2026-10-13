//! The worked-example table from `docs/adr/0002-sort-and-artist-run.md`.
//!
//! This is the single source of truth shared by the sort tests, the playlist
//! tests, the UI tests and the spec document: `tests/adr_spec.rs` re-parses the
//! markdown table and fails if code and document ever disagree.

use std::path::PathBuf;

use crate::model::track::Track;

/// One row of the spec table. Rows are listed in **expected sorted order**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixtureRow {
    /// File name (the path used in tests is `/spec/<file>`).
    pub file: &'static str,
    /// `ARTIST` tag, `None` means the tag is absent.
    pub artist: Option<&'static str>,
    /// `ALBUM_ARTIST` tag.
    pub album_artist: Option<&'static str>,
    /// `ALBUM` tag.
    pub album: Option<&'static str>,
    /// Disc number.
    pub disc: Option<u32>,
    /// Track number.
    pub track: Option<u32>,
    /// Release year.
    pub year: Option<i32>,
    /// Expected artist-run id (runs are numbered from 1 in sorted order).
    pub run: usize,
}

/// Rows of the spec table, in expected sorted order.
#[rustfmt::skip]
pub const TABLE: &[FixtureRow] = &[
    // --- run 1: untagged files group together, ordered by path ---------------
    FixtureRow { file: "untagged-alpha.flac",   artist: Some(""),         album_artist: Some(""),         album: Some(""),       disc: None, track: None, year: None, run: 1 },
    FixtureRow { file: "untagged-beta.mp3",     artist: Some(""),         album_artist: Some(""),         album: Some(""),       disc: None, track: None, year: None, run: 1 },
    // --- run 2: single-song artist -------------------------------------------
    FixtureRow { file: "aaron-singles.mp3",     artist: Some("aaron"),    album_artist: None,             album: Some("Singles"), disc: Some(1), track: Some(1), year: Some(2001), run: 2 },
    // --- run 3: two albums, natural album numbers, second disc ---------------
    FixtureRow { file: "ab-album1-d1t1.mp3",    artist: Some("Alpha Beta"), album_artist: None,           album: Some("Album 1"),  disc: Some(1), track: Some(1), year: Some(1998), run: 3 },
    FixtureRow { file: "ab-album1-d1t2.mp3",    artist: Some("Alpha Beta"), album_artist: None,           album: Some("Album 1"),  disc: Some(1), track: Some(2), year: Some(1998), run: 3 },
    FixtureRow { file: "ab-album1-d2t1.mp3",    artist: Some("Alpha Beta"), album_artist: None,           album: Some("Album 1"),  disc: Some(2), track: Some(1), year: Some(1998), run: 3 },
    FixtureRow { file: "ab-album2-d1t1.mp3",    artist: Some("Alpha Beta"), album_artist: None,           album: Some("Album 2"),  disc: Some(1), track: Some(1), year: Some(1999), run: 3 },
    FixtureRow { file: "ab-album10-d1t1.mp3",   artist: Some("Alpha Beta"), album_artist: None,           album: Some("Album 10"), disc: Some(1), track: Some(1), year: Some(2003), run: 3 },
    // --- run 4: multi-disc album (disc 1, 2, then 10) ------------------------
    FixtureRow { file: "bw-double-d1t1.flac",   artist: Some("Beta Wave"), album_artist: None,            album: Some("Double"),   disc: Some(1), track: Some(1), year: Some(1994), run: 4 },
    FixtureRow { file: "bw-double-d1t2.flac",   artist: Some("Beta Wave"), album_artist: None,            album: Some("Double"),   disc: Some(1), track: Some(2), year: Some(1994), run: 4 },
    FixtureRow { file: "bw-double-d2t1.flac",   artist: Some("Beta Wave"), album_artist: None,            album: Some("Double"),   disc: Some(2), track: Some(1), year: Some(1994), run: 4 },
    FixtureRow { file: "bw-double-d10t1.flac",  artist: Some("Beta Wave"), album_artist: None,            album: Some("Double"),   disc: Some(10), track: Some(1), year: None, run: 4 },
    FixtureRow { file: "bw-eleven-d1t1.mp3",    artist: Some("Beta Wave"), album_artist: None,            album: Some("Eleven"),   disc: Some(1), track: Some(1), year: Some(1996), run: 4 },
    // --- run 5: same artist through different casing / whitespace ------------
    FixtureRow { file: "beatles-red-d1t1.flac", artist: Some("The Beatles"), album_artist: None,          album: Some("1962-1966"), disc: Some(1), track: Some(1), year: Some(1973), run: 5 },
    FixtureRow { file: "beatles-red-d1t2.flac", artist: Some("The Beatles"), album_artist: None,          album: Some("1962-1966"), disc: Some(1), track: Some(2), year: Some(1973), run: 5 },
    FixtureRow { file: "beatles-blue-d1t1.mp3", artist: Some("the   BEATLES"), album_artist: None,        album: Some("1967-1970"), disc: Some(1), track: Some(1), year: Some(1973), run: 5 },
    // --- run 6: compilation, grouped by album_artist --------------------------
    FixtureRow { file: "vox-tracks2.mp3",       artist: Some("Sister Hazel"), album_artist: Some("Various Artists"), album: Some("Tracks Of 2"),  disc: Some(1), track: Some(3), year: Some(2002), run: 6 },
    FixtureRow { file: "vox-tracks10.mp3",      artist: Some("Guster"),       album_artist: Some("Various Artists"), album: Some("Tracks Of 10"), disc: Some(1), track: Some(1), year: Some(2002), run: 6 },
    // --- run 7: album year beats the album title; year splits identical keys ---
    FixtureRow { file: "zeta-works2-d1t2.mp3",  artist: Some("Zeta Nine"),  album_artist: None,           album: Some("Works 2"),  disc: Some(1), track: Some(2), year: Some(2007), run: 7 },
    FixtureRow { file: "zeta-works10-d1t10.mp3",artist: Some("Zeta Nine"),  album_artist: None,           album: Some("Works 10"), disc: Some(1), track: Some(10), year: Some(2007), run: 7 },
    FixtureRow { file: "zeta-same-a.mp3",       artist: Some("Zeta Nine"),  album_artist: None,           album: Some("Same"),     disc: None, track: None, year: Some(2009), run: 7 },
    FixtureRow { file: "zeta-same-b.mp3",       artist: Some("Zeta Nine"),  album_artist: None,           album: Some("Same"),     disc: None, track: None, year: Some(2011), run: 7 },
];

/// The table as `Vec` (kept for symmetry with [`fixture_tracks`]).
#[must_use]
pub fn fixture_table() -> Vec<FixtureRow> {
    TABLE.to_vec()
}

/// The fixture rows as [`Track`]s, deliberately inserted in reverse order so a
/// test that forgets to sort fails loudly.
#[must_use]
pub fn fixture_tracks() -> Vec<Track> {
    let mut tracks: Vec<Track> = TABLE
        .iter()
        .map(|row| {
            let mut t = Track::from_path(PathBuf::from("/spec").join(row.file));
            t.artist = row.artist.map(str::to_string);
            t.album_artist = row.album_artist.map(str::to_string);
            t.album = row.album.map(str::to_string);
            t.disc_no = row.disc;
            t.track_no = row.track;
            t.year = row.year;
            t.refresh_key();
            t
        })
        .collect();
    tracks.reverse();
    tracks
}
