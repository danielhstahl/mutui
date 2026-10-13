//! Domain model: [`Track`], the sort key, and the playable [`Playlist`].

mod fixtures;
mod playlist;
mod sort;
mod track;

pub use fixtures::{fixture_table, fixture_tracks, FixtureRow, TABLE};
pub use playlist::{Playlist, RunEnded};
pub use sort::{dump_sorted_order, natural_cmp, normalize, sort_tracks, SortKey, TRACK_MISSING};
pub use track::Track;
