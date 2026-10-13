//! The [`Track`] record produced by the scanner and consumed by the UI/player.

use std::path::PathBuf;
use std::time::Duration;

use super::sort::{normalize, SortKey, TRACK_MISSING};

/// One row of the library: an audio file plus its (possibly missing) tags.
///
/// Every tag field is optional: a file with no tags, or an unreadable/corrupt
/// file, still becomes a row (with [`Track::corrupt`] set) so the user can see
/// and play whatever is playable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Track {
    /// Absolute path as produced by the scanner.
    pub path: PathBuf,
    /// Container/codec label, e.g. `mp3`, `flac`, `wav` (lowercase extension).
    pub format: String,
    /// Display title; falls back to the file stem.
    pub title: String,
    /// `ARTIST` tag.
    pub artist: Option<String>,
    /// `ALBUM_ARTIST` tag (used for compilations).
    pub album_artist: Option<String>,
    /// `ALBUM` tag.
    pub album: Option<String>,
    /// Track number.
    pub track_no: Option<u32>,
    /// Track total (`3/12` → `12`).
    pub track_total: Option<u32>,
    /// Disc number.
    pub disc_no: Option<u32>,
    /// Disc total.
    pub disc_total: Option<u32>,
    /// Release year, extracted from the date tag.
    pub year: Option<i32>,
    /// Raw release date when the container carries one (e.g. `2002-05-14`).
    pub date: Option<String>,
    /// Decoded duration when the scanner could compute it.
    pub duration: Option<Duration>,
    /// Audio bitrate in bits per second.
    pub bitrate: Option<u32>,
    /// Tags could not be read, or the file looks truncated.
    pub corrupt: bool,
    /// Pre-computed sort key, so the comparator never allocates.
    pub key: SortKey,
}

impl Track {
    /// A track that only has a path: tags are filled in by the scanner, the
    /// title falls back to the file stem.
    #[must_use]
    pub fn from_path(path: PathBuf) -> Self {
        let format = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("<unknown>")
            .to_string();
        let mut track = Self {
            path,
            format,
            title,
            ..Self::default()
        };
        track.refresh_key();
        track
    }

    /// Recompute the pre-computed sort key from the tag fields.
    ///
    /// This resets the album-wide `album_year` to the row's own year, because a
    /// single row cannot know the rest of its album (ADR 0002 §3.2). Anything
    /// that refreshes keys after sorting must call
    /// [`crate::model::sort_tracks`] afterwards to restore album order.
    pub fn refresh_key(&mut self) {
        let artist_src = blank_to_none(self.album_artist.as_deref())
            .or_else(|| blank_to_none(self.artist.as_deref()))
            .unwrap_or("");
        let album_src = blank_to_none(self.album.as_deref()).unwrap_or("");
        let name = self
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.key = SortKey {
            artist: normalize(artist_src),
            // Album-wide value: [`crate::model::sort_tracks`] replaces this
            // per-row copy with the earliest year found anywhere on the album.
            album_year: i64::from(self.year.unwrap_or_default()),
            album: normalize(album_src),
            disc: self.disc_no.unwrap_or(1),
            track: self.track_no.unwrap_or(TRACK_MISSING),
            year: i64::from(self.year.unwrap_or_default()),
            name: normalize(&name),
            path: self
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .to_lowercase(),
        };
    }

    /// `true` when both tracks belong to the same artist run (normalized
    /// `album_artist` else `artist` equality, see sort spec §5).
    #[must_use]
    pub fn same_artist(&self, other: &Track) -> bool {
        self.key.artist == other.key.artist
    }

    /// Artist text for display (never empty).
    #[must_use]
    pub fn display_artist(&self) -> &str {
        blank_to_none(self.album_artist.as_deref())
            .or_else(|| blank_to_none(self.artist.as_deref()))
            .unwrap_or("(no artist)")
    }

    /// Album text for display (never empty).
    #[must_use]
    pub fn display_album(&self) -> &str {
        blank_to_none(self.album.as_deref()).unwrap_or("(no album)")
    }

    /// `d1 t3` style label, with `—` for missing numbers.
    #[must_use]
    pub fn track_label(&self) -> String {
        let disc = self
            .disc_no
            .map_or_else(|| String::from("-"), |d| d.to_string());
        let track = self
            .track_no
            .map_or_else(|| String::from("-"), |t| t.to_string());
        format!("d{disc} t{track}")
    }

    /// `m:ss` duration label, or `-` when unknown.
    #[must_use]
    pub fn duration_label(&self) -> String {
        self.duration.map_or_else(
            || String::from("-"),
            |d| {
                let total = d.as_secs();
                format!("{}:{:02}", total / 60, total % 60)
            },
        )
    }
}

fn blank_to_none(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_falls_back_to_file_stem() {
        let t = Track::from_path(PathBuf::from("/music/a b/My Song.mp3"));
        assert_eq!(t.title, "My Song");
        assert_eq!(t.format, "mp3");
        assert_eq!(t.display_artist(), "(no artist)");
        assert_eq!(t.duration_label(), "-");
        assert_eq!(t.track_label(), "d- t-");
    }

    #[test]
    fn album_artist_wins_over_artist_for_sorting() {
        let mut t = Track::from_path(PathBuf::from("/m/a/1.mp3"));
        t.artist = Some("Someone".into());
        t.album_artist = Some("Various Artists".into());
        t.refresh_key();
        assert_eq!(t.key.artist, "various artists");
        t.album_artist = Some("   ".into());
        t.refresh_key();
        assert_eq!(t.key.artist, "someone", "blank album_artist falls back");
    }

    #[test]
    fn missing_tag_sentinels() {
        let t = Track::from_path(PathBuf::from("/m/a/1.mp3"));
        assert_eq!(t.key.disc, 1);
        assert_eq!(t.key.track, TRACK_MISSING);
        assert_eq!(t.key.year, 0, "missing year is the 0 sentinel");
        assert_eq!(
            t.key.album_year, 0,
            "until sorted, the album year is the row's"
        );
        assert_eq!(t.key.artist, "");
    }
}
