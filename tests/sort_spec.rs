//! Table-driven tests for the sort spec (docs/adr/0002-sort-and-artist-run.md).

use music_tui::model::{
    fixture_tracks, natural_cmp, sort_tracks, FixtureRow, TABLE, TRACK_MISSING,
};

fn expected_order() -> Vec<&'static str> {
    TABLE.iter().map(|row| row.file).collect()
}

fn actual_order(tracks: &[music_tui::model::Track]) -> Vec<String> {
    tracks
        .iter()
        .map(|t| {
            t.path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn fixture_order_matches_the_spec_table() {
    // fixture_tracks() hands them back in reverse order on purpose.
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    assert_eq!(actual_order(&tracks), expected_order());
}

#[test]
fn resorting_is_a_no_op() {
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    let once = actual_order(&tracks);
    sort_tracks(&mut tracks);
    assert_eq!(actual_order(&tracks), once);
    sort_tracks(&mut tracks);
    assert_eq!(actual_order(&tracks), once);
}

#[test]
fn every_field_of_the_table_is_applied() {
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    let by_file = |name: &str| {
        tracks
            .iter()
            .find(|t| t.path.file_name().is_some_and(|f| f == name))
            .unwrap_or_else(|| panic!("missing fixture {name}"))
    };
    // disc/track sentinels
    let same = by_file("zeta-same-a.mp3");
    assert_eq!(same.key.disc, 1, "missing disc means 1");
    assert_eq!(same.key.track, TRACK_MISSING, "missing track goes last");
    assert_eq!(same.key.year, 2009);
    // year breaks the tie between two otherwise identical keys
    assert!(by_file("zeta-same-a.mp3").key < by_file("zeta-same-b.mp3").key);
    // album year beats the album title: *Works* (2007) before *Same* (2009)
    assert!(by_file("zeta-works2-d1t2.mp3").key < by_file("zeta-same-a.mp3").key);
    // a row with no year still carries its album's year (spec §3.2)
    assert_eq!(by_file("bw-double-d10t1.flac").key.year, 0);
    assert_eq!(by_file("bw-double-d10t1.flac").key.album_year, 1994);
    assert_eq!(by_file("zeta-same-a.mp3").key.album_year, 2009);
    // natural album numbers
    assert!(by_file("ab-album1-d1t1.mp3").key < by_file("ab-album2-d1t1.mp3").key);
    assert!(by_file("ab-album2-d1t1.mp3").key < by_file("ab-album10-d1t1.mp3").key);
    // disc beats track
    assert!(by_file("bw-double-d2t1.flac").key < by_file("bw-double-d10t1.flac").key);
    // album_artist wins for compilations
    assert_eq!(by_file("vox-tracks2.mp3").key.artist, "various artists");
    assert_eq!(by_file("vox-tracks10.mp3").key.artist, "various artists");
    // case/whitespace variants collapse
    assert_eq!(
        by_file("beatles-blue-d1t1.mp3").key.artist,
        by_file("beatles-red-d1t1.flac").key.artist
    );
}

#[test]
fn untagged_files_group_together_and_are_deterministic() {
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    let untagged: Vec<String> = tracks
        .iter()
        .filter(|t| t.key.artist.is_empty())
        .map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(untagged, vec!["untagged-alpha.flac", "untagged-beta.mp3"]);
}

#[test]
fn comparator_is_a_strict_total_order_on_generated_keys() {
    // Property-style: antisymmetry and transitivity over generated tag combos.
    let artists = ["", "a", "A ", "alpha", "Alpha Beta", "beta wave", "B"];
    let albums = ["", "Album 1", "Album 10", "Album 2", "same"];
    let discs = [None, Some(1u32), Some(2), Some(10)];
    let track_numbers = [None, Some(1u32), Some(3), Some(12)];
    let years = [None, Some(1999i32), Some(2003)];
    let mut keys: Vec<music_tui::model::SortKey> = Vec::new();
    let mut index = 0usize;
    for artist in artists {
        for album in albums {
            for disc in discs {
                for track_no in track_numbers {
                    for year in years {
                        let mut track = music_tui::model::Track::from_path(
                            std::path::PathBuf::from(format!("/gen/{index:05}.mp3")),
                        );
                        track.artist = Some(artist.to_string());
                        track.album = Some(album.to_string());
                        track.disc_no = disc;
                        track.track_no = track_no;
                        track.year = year;
                        track.refresh_key();
                        keys.push(track.key);
                        index += 1;
                    }
                }
            }
        }
    }
    assert!(keys.len() > 1_000, "generated {}", keys.len());
    for a in &keys {
        for b in &keys {
            assert_eq!(a.cmp(b), b.cmp(a).reverse(), "antisymmetry");
        }
    }
    // Transitivity on a square subset (full cube would be 10^9 comparisons).
    let sample: Vec<&music_tui::model::SortKey> = keys.iter().step_by(keys.len() / 40).collect();
    for a in &sample {
        for b in &sample {
            for c in &sample {
                if a.cmp(b) == std::cmp::Ordering::Less && b.cmp(c) == std::cmp::Ordering::Less {
                    assert_eq!(a.cmp(c), std::cmp::Ordering::Less, "transitivity");
                }
            }
        }
    }
}

#[test]
fn natural_comparison_examples_from_the_spec() {
    assert_eq!(natural_cmp("album 1", "album 2"), std::cmp::Ordering::Less);
    assert_eq!(natural_cmp("album 2", "album 10"), std::cmp::Ordering::Less);
    assert_eq!(
        natural_cmp("tracks of 2", "tracks of 10"),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        natural_cmp("1962-1966", "1967-1970"),
        std::cmp::Ordering::Less
    );
    assert_eq!(natural_cmp("", "aaron"), std::cmp::Ordering::Less);
}

#[test]
fn fixture_rows_are_self_consistent() {
    let table: Vec<FixtureRow> = music_tui::model::fixture_table();
    assert!(table.len() >= 12, "spec needs at least 12 worked rows");
    let mut last_run = 0usize;
    for row in &table {
        assert!(row.run >= last_run, "runs must be listed in order");
        last_run = row.run;
    }
    // The spec asks for an artist with two albums and a track missing tags.
    let alpha = table
        .iter()
        .filter(|r| r.artist == Some("Alpha Beta"))
        .count();
    assert!(alpha >= 4, "two-album artist present");
    assert!(table.iter().any(|r| r.artist.is_none_or(|a| a.is_empty())));
}
