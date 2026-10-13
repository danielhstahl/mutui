//! Artist-run / cursor tests (`mutu-2h8.7`), reusing the spec fixture table.

use music_tui::model::{Playlist, RunEnded, Track, fixture_tracks};

fn playlist() -> Playlist {
    let mut tracks = fixture_tracks();
    music_tui::model::sort_tracks(&mut tracks);
    Playlist::new(tracks)
}

fn file(playlist: &Playlist, index: usize) -> String {
    playlist
        .get(index)
        .and_then(|t| t.path.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Runs from the spec table, 0-based and inclusive.
const RUNS: [(usize, usize); 7] = [
    (0, 1),
    (2, 2),
    (3, 7),
    (8, 12),
    (13, 15),
    (16, 17),
    (18, 21),
];

#[test]
fn run_boundaries_match_the_spec() {
    let mut playlist = playlist();
    let mut runs = Vec::new();
    let mut index = 0usize;
    while index < playlist.len() {
        assert!(playlist.select_start(index));
        let (start, end) = playlist.run().expect("every row has a run");
        runs.push((start, end - 1));
        index = end;
    }
    assert_eq!(runs, RUNS.to_vec());
}

#[test]
fn click_first_row_of_a_run_plays_the_whole_run() {
    let mut playlist = playlist();
    playlist.select_start(3); // ab-album1-d1t1
    let mut order = vec![playlist.cursor().expect("cursor")];
    while let Some(next) = playlist.advance() {
        order.push(next);
    }
    assert_eq!(order, vec![3, 4, 5, 6, 7]);
    assert!(playlist.at_run_end());
    assert_eq!(
        playlist.advance(),
        None,
        "must not cross into the next artist"
    );
}

#[test]
fn click_middle_of_a_run_never_wraps() {
    let mut playlist = playlist();
    playlist.select_start(5); // third row of run 3
    assert_eq!(playlist.run(), Some((3, 8)));
    assert_eq!(playlist.run_position(), Some((3, 5)));
    let mut order = vec![5];
    while let Some(next) = playlist.advance() {
        order.push(next);
    }
    assert_eq!(order, vec![5, 6, 7]);
    assert_eq!(file(&playlist, 7), "ab-album10-d1t1.mp3");
    assert_eq!(
        file(&playlist, 8),
        "bw-double-d1t1.flac",
        "next row is a different artist"
    );
}

#[test]
fn click_last_row_of_a_run_stops_immediately() {
    let mut playlist = playlist();
    playlist.select_start(7);
    assert!(playlist.at_run_end());
    assert_eq!(playlist.advance(), None);
    assert_eq!(playlist.prev(), Some(6), "previous is still inside the run");
}

#[test]
fn single_song_artist_plays_and_stops() {
    let mut playlist = playlist();
    playlist.select_start(2); // aaron, alone in run 2
    assert_eq!(playlist.run_len(), 1);
    assert_eq!(playlist.run_position(), Some((1, 1)));
    assert_eq!(playlist.advance(), None);
    assert_eq!(playlist.prev(), None);
}

#[test]
fn same_artist_in_different_casing_is_one_run() {
    let mut playlist = playlist();
    playlist.select_start(14); // beatles-red-d1t2
    assert_eq!(playlist.run(), Some((13, 16)));
    assert_eq!(
        playlist.artist_of(15),
        playlist.artist_of(13),
        "'the   BEATLES' normalizes equal to 'The Beatles'"
    );
}

#[test]
fn empty_playlist_reports_empty_list() {
    let mut playlist = Playlist::new(Vec::new());
    assert!(playlist.is_empty());
    assert_eq!(playlist.advance(), None);
    assert_eq!(playlist.prev(), None);
    assert!(!playlist.select_start(0), "no row to select");
    assert!(!playlist.at_run_end());
    assert_eq!(playlist.current(), None);
    assert_eq!(playlist.move_cursor(1), None);
    let ended = if playlist.is_empty() {
        RunEnded::EmptyList
    } else {
        RunEnded::EndOfArtist {
            artist: String::new(),
        }
    };
    assert_eq!(ended, RunEnded::EmptyList);
}

#[test]
fn index_past_end_after_rescan_is_clamped() {
    let mut playlist = playlist();
    playlist.select_start(20);
    let anchor = playlist.current().cloned();
    let mut small = fixture_tracks();
    small.truncate(4);
    music_tui::model::sort_tracks(&mut small);
    playlist.replace(small, anchor.as_ref());
    let cursor = playlist.cursor().expect("cursor survives");
    assert!(cursor < playlist.len());
    assert!(playlist.run().is_some());
    // Deleting everything drops the cursor cleanly.
    playlist.replace(Vec::new(), None);
    assert!(playlist.is_empty());
    assert_eq!(playlist.cursor(), None);
}

#[test]
fn rescan_reanchors_by_path_then_by_artist() {
    let mut playlist = playlist();
    playlist.select_start(9); // bw-double-d1t2
    let anchor = playlist.current().cloned();

    // Exact row survived: cursor lands on the same path.
    let mut same = fixture_tracks();
    music_tui::model::sort_tracks(&mut same);
    playlist.replace(same, anchor.as_ref());
    assert_eq!(
        playlist.current().map(|t| t.path.clone()),
        anchor.map(|t| t.path)
    );

    // Row deleted: re-resolve to the first row of the same artist.
    let mut without = fixture_tracks();
    without.retain(|t| !t.path.to_string_lossy().ends_with("bw-double-d1t2.flac"));
    music_tui::model::sort_tracks(&mut without);
    let anchor2 = Some(Track::from_path(std::path::PathBuf::from(
        "/spec/bw-double-d1t2.flac",
    )));
    playlist.replace(without, anchor2.as_ref());
    assert_eq!(
        playlist.current().map(|t| t.key.artist.clone()),
        Some("beta wave".to_string())
    );
    assert_eq!(playlist.run_len(), 4, "run recomputed after the rescan");
}

#[test]
fn move_cursor_clamps_and_follows_the_new_row() {
    let mut playlist = playlist();
    assert_eq!(playlist.move_cursor(-100), Some(0));
    assert_eq!(playlist.move_cursor(100), Some(playlist.len() - 1));
    assert!(playlist.at_run_end(), "last row is the end of run 7");
    assert_eq!(playlist.move_cursor(-1), Some(playlist.len() - 2));
}
