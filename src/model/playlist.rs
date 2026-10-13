//! The playable sequence: sorted tracks + cursor + artist-run boundaries.
//!
//! No I/O, no terminal, no audio — everything here is exhaustively unit-testable
//! (see `tests/playlist_run.rs` for the normative behaviour).

use crate::model::track::Track;

/// Why auto-advance stopped (used to word the footer message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnded {
    /// The last row sharing the clicked song's artist was reached.
    EndOfArtist {
        /// Normalized artist name of the run that finished.
        artist: String,
    },
    /// There was nothing to play.
    EmptyList,
}

/// Sorted library plus an explicit cursor and the artist-run it belongs to.
#[derive(Debug, Clone, Default)]
pub struct Playlist {
    tracks: Vec<Track>,
    cursor: Option<usize>,
    /// Maximal contiguous block of rows sharing the cursor's artist.
    run_start: usize,
    /// Exclusive end of that block.
    run_end: usize,
}

impl Playlist {
    /// Wrap an already-sorted vector (sorting is the caller's job, see
    /// [`crate::model::sort_tracks`]).
    #[must_use]
    pub fn new(tracks: Vec<Track>) -> Self {
        Self {
            tracks,
            cursor: None,
            run_start: 0,
            run_end: 0,
        }
    }

    /// Number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// `true` when the library has no rows at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Row at `index`, if any.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    /// All rows, in sorted order.
    #[must_use]
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Append a row while the scanner is still streaming (unsorted on purpose —
    /// sorting happens once, when the scan finishes).
    pub fn push(&mut self, track: Track) {
        self.tracks.push(track);
    }

    /// Sort the streamed rows and re-anchor the cursor (called once per scan).
    pub fn finish_scan(&mut self, anchor: Option<&Track>) {
        let tracks = std::mem::take(&mut self.tracks);
        let mut sorted = tracks;
        crate::model::sort::sort_tracks(&mut sorted);
        self.replace(sorted, anchor);
    }

    /// Replace the library (rescan), re-anchoring the cursor.
    ///
    /// The anchor is resolved by path first (exact row survived), then by artist
    /// key (the row was deleted/renamed but its artist still exists). If nothing
    /// resolves, the cursor is dropped and the run is cleared.
    pub fn replace(&mut self, tracks: Vec<Track>, anchor: Option<&Track>) {
        self.tracks = tracks;
        let by_path = anchor.and_then(|a| self.index_of_path(&a.path));
        let by_artist = by_path.or_else(|| {
            anchor.and_then(|a| {
                if a.key.artist.is_empty() {
                    None
                } else {
                    self.tracks
                        .iter()
                        .position(|t| t.key.artist == a.key.artist)
                }
            })
        });
        let fallback = if self.tracks.is_empty() {
            None
        } else {
            by_artist.or(self.cursor.map(|c| c.min(self.tracks.len() - 1)))
        };
        match fallback {
            Some(index) => {
                self.select_start(index);
            }
            _ => {
                self.cursor = None;
                self.run_start = 0;
                self.run_end = 0;
            }
        }
    }

    /// Index of the row with this exact path.
    #[must_use]
    pub fn index_of_path(&self, path: &std::path::Path) -> Option<usize> {
        self.tracks.iter().position(|t| t.path == path)
    }

    /// Maximal contiguous block of rows sharing `index`'s artist.
    ///
    /// Sort spec §5 pseudocode:
    /// `run(i) = (first j <= i with artist(j) == artist(i), last j >= i with artist(j) == artist(i))`
    #[must_use]
    pub fn run_of(&self, index: usize) -> Option<(usize, usize)> {
        let artist = self.get(index)?.key.artist.as_str();
        let mut start = index;
        while start > 0 && self.tracks[start - 1].key.artist == artist {
            start -= 1;
        }
        let mut end = index + 1;
        while end < self.tracks.len() && self.tracks[end].key.artist == artist {
            end += 1;
        }
        Some((start, end))
    }

    /// Set the cursor to `index` and compute its artist run.
    /// Returns `false` when the index is out of range (state unchanged).
    pub fn select_start(&mut self, index: usize) -> bool {
        let Some((start, end)) = self.run_of(index) else {
            return false;
        };
        self.cursor = Some(index);
        self.run_start = start;
        self.run_end = end;
        true
    }

    /// Current cursor index.
    #[must_use]
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Row under the cursor.
    #[must_use]
    pub fn current(&self) -> Option<&Track> {
        self.cursor.and_then(|c| self.get(c))
    }

    /// `(run_start, run_end)` with `run_end` exclusive.
    #[must_use]
    pub fn run(&self) -> Option<(usize, usize)> {
        if self.cursor.is_some() && self.run_end > self.run_start {
            Some((self.run_start, self.run_end))
        } else {
            None
        }
    }

    /// Number of rows in the current run.
    #[must_use]
    pub fn run_len(&self) -> usize {
        self.run().map_or(0, |(a, b)| b - a)
    }

    /// 1-based `(position, length)` of the cursor inside its run.
    #[must_use]
    pub fn run_position(&self) -> Option<(usize, usize)> {
        let c = self.cursor?;
        let (start, end) = self.run()?;
        if (start..end).contains(&c) {
            Some((c - start + 1, end - start))
        } else {
            None
        }
    }

    /// Normalized artist of row `index`.
    #[must_use]
    pub fn artist_of(&self, index: usize) -> Option<&str> {
        self.get(index).map(|t| t.key.artist.as_str())
    }

    /// Display artist of row `index`.
    #[must_use]
    pub fn display_artist_of(&self, index: usize) -> Option<&str> {
        self.get(index).map(Track::display_artist)
    }

    /// `true` when the cursor sits on the last row of its artist run.
    #[must_use]
    pub fn at_run_end(&self) -> bool {
        match (self.cursor, self.run()) {
            (Some(c), Some((_, end))) => c + 1 >= end,
            _ => false,
        }
    }

    /// Move to the next row inside the current run.
    /// `None` at the end of the run (caller reports [`RunEnded`]).
    pub fn advance(&mut self) -> Option<usize> {
        let c = self.cursor?;
        let next = c + 1;
        if next < self.run_end {
            self.cursor = Some(next);
            Some(next)
        } else {
            None
        }
    }

    /// Move to the previous row inside the current run.
    pub fn prev(&mut self) -> Option<usize> {
        let c = self.cursor?;
        if c > self.run_start {
            self.cursor = Some(c - 1);
            Some(c - 1)
        } else {
            None
        }
    }

    /// Move the cursor by `delta` rows across the whole library (keyboard
    /// navigation); clamped, and the run is recomputed for the new row.
    pub fn move_cursor(&mut self, delta: i64) -> Option<usize> {
        if self.tracks.is_empty() {
            return None;
        }
        let base = self.cursor.unwrap_or(0) as i64;
        let target = (base + delta).clamp(0, self.tracks.len() as i64 - 1) as usize;
        if self.select_start(target) {
            Some(target)
        } else {
            None
        }
    }

    /// Clamp or drop a cursor that no longer exists (after a rescan).
    pub fn clamp_cursor(&mut self) {
        if self.tracks.is_empty() {
            self.cursor = None;
            self.run_start = 0;
            self.run_end = 0;
            return;
        }
        if let Some(c) = self.cursor {
            let target = c.min(self.tracks.len() - 1);
            self.select_start(target);
        }
    }
}
