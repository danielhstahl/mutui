//! Domain model: [`Track`], the sort key, and the playable [`Playlist`].

mod fixtures;
mod playlist;
mod sort;
mod track;

pub use fixtures::{FixtureRow, TABLE, fixture_table, fixture_tracks};
pub use playlist::{Playlist, RunEnded};
pub use sort::{
    SORT_ALGORITHM, SortKey, TRACK_MISSING, YEAR_MISSING, apply_album_years, dump_sorted_order,
    natural_cmp, normalize, sort_tracks,
};
pub use track::Track;
