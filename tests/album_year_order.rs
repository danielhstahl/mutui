//! `mutu-172` — an artist's albums must come out in release order, and an album
//! must never be split apart because its files disagree about the year.
//!
//! Normative text: `docs/adr/0002-sort-and-artist-run.md` §3.2.

use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use music_tui::model::{Playlist, Track, sort_tracks};
use music_tui::scan::{self, ScanConfig, ScanMessage};
use music_tui::testing::{FixtureFile, FixtureSet, FixtureTags, Format};

/// Scan a folder the way the app does: stream, then sort once at the end.
fn scan_sorted(root: &Path) -> Vec<Track> {
    let (tx, rx) = mpsc::channel();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    scan::scan(root, &ScanConfig::default(), &tx, &cancel);
    // The scanner keeps its own clone of `tx`; the receiver only closes once the
    // caller's handle goes away too.
    drop(tx);
    let mut tracks = Vec::new();
    for message in rx.iter() {
        if let ScanMessage::Track(track) = message {
            tracks.push(track);
        }
    }
    sort_tracks(&mut tracks);
    tracks
}

fn albums(tracks: &[Track]) -> Vec<String> {
    tracks
        .iter()
        .map(|t| t.display_album().to_string())
        .collect()
}

fn labels(tracks: &[Track]) -> Vec<String> {
    tracks
        .iter()
        .map(|t| {
            format!(
                "{} / {} {}",
                t.display_artist(),
                t.display_album(),
                t.track_label()
            )
        })
        .collect()
}

/// Rows sharing `key` must occupy one unbroken range of indices.
fn assert_contiguous<K, F>(tracks: &[Track], key: F, what: &str)
where
    K: Eq + Hash + Clone + Debug,
    F: Fn(&Track) -> K,
{
    let mut spans: HashMap<K, (usize, usize, usize)> = HashMap::new();
    for (index, track) in tracks.iter().enumerate() {
        let entry = spans.entry(key(track)).or_insert((index, index, 0usize));
        entry.1 = index;
        entry.2 += 1;
    }
    for (name, (first, last, count)) in spans {
        assert_eq!(
            last - first + 1,
            count,
            "{what} {name:?} is split across the library: {:?}",
            labels(tracks)
        );
    }
}

/// Album years must rise (weakly) as you walk down any single artist.
fn assert_album_years_rise_inside_artists(tracks: &[Track]) {
    let mut previous: Option<(&str, i64)> = None;
    for track in tracks {
        let artist = track.key.artist.as_str();
        if previous.is_some_and(|(seen, _)| seen != artist) {
            previous = None;
        }
        if let Some((_, year)) = previous {
            assert!(
                track.key.album_year >= year,
                "album year went backwards inside artist {artist:?}: {:?}",
                labels(tracks)
            );
        }
        previous = Some((artist, track.key.album_year));
    }
}

#[test]
fn scanned_albums_come_out_in_release_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Titles chosen so that alphabetical order is the reverse of release order:
    // the old key (artist, album title, ...) produced Alpha, Middle, Zebra here.
    FixtureSet::new(dir.path())
        .song(
            "delta/alpha/one.flac",
            "Alpha One",
            "Delta",
            "Alpha Years",
            1,
            1,
            2015,
        )
        .song(
            "delta/alpha/two.flac",
            "Alpha Two",
            "Delta",
            "Alpha Years",
            1,
            2,
            2015,
        )
        .song(
            "delta/middle/one.mp3",
            "Middle One",
            "Delta",
            "Middle Years",
            1,
            1,
            2008,
        )
        .song(
            "delta/zebra/one.wav",
            "Zebra One",
            "Delta",
            "Zebra Years",
            1,
            1,
            2001,
        )
        .song(
            "delta/zebra/two.wav",
            "Zebra Two",
            "Delta",
            "Zebra Years",
            1,
            2,
            2001,
        )
        .write_all()
        .expect("write fixtures");

    let tracks = scan_sorted(dir.path());
    assert_eq!(
        albums(&tracks),
        [
            "Zebra Years",
            "Zebra Years",
            "Middle Years",
            "Alpha Years",
            "Alpha Years"
        ],
        "{:?}",
        labels(&tracks)
    );
    assert!(tracks.iter().all(|t| t.key.album_year
        == match t.display_album() {
            "Zebra Years" => 2001,
            "Middle Years" => 2008,
            _ => 2015,
        }));
}

#[test]
fn an_undated_file_stays_inside_its_album() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A 2-CD set whose second disc lost its DATE tag, plus an earlier single.
    FixtureSet::new(dir.path())
        .song("echo/box/d1t1.flac", "D1T1", "Echo", "Box Set", 1, 1, 1990)
        .song("echo/box/d2t1.flac", "D2T1", "Echo", "Box Set", 2, 1, 1992)
        .raw(FixtureFile {
            path: PathBuf::from("echo/box/d2t2.flac"),
            tags: FixtureTags {
                artist: Some("Echo".into()),
                album: Some("Box Set".into()),
                title: Some("D2T2".into()),
                disc: Some(2),
                track: Some(2),
                ..FixtureTags::default()
            },
            corrupt: false,
            format: Format::Flac,
        })
        .song(
            "echo/warm/one.wav",
            "Warm Up",
            "Echo",
            "Warm Up",
            1,
            1,
            1988,
        )
        .write_all()
        .expect("write fixtures");

    let tracks = scan_sorted(dir.path());
    // The undated row must not escape to the front of the artist (its own year is
    // missing, so it would compare as 0), and the box set must stay in disc order.
    assert_eq!(
        labels(&tracks),
        [
            "Echo / Warm Up d1 t1",
            "Echo / Box Set d1 t1",
            "Echo / Box Set d2 t1",
            "Echo / Box Set d2 t2",
        ]
    );
    assert_contiguous(&tracks, |t| t.key.album.clone(), "album");
}

#[test]
fn an_undated_album_leads_its_artist() {
    let dir = tempfile::tempdir().expect("tempdir");
    FixtureSet::new(dir.path())
        .song(
            "foxtrot/dated/one.mp3",
            "Dated",
            "Foxtrot",
            "Dated",
            1,
            1,
            1999,
        )
        .untagged("misc/untagged.wav")
        .raw(FixtureFile {
            path: PathBuf::from("foxtrot/lost/one.mp3"),
            tags: FixtureTags {
                artist: Some("Foxtrot".into()),
                album: Some("Lost Sessions".into()),
                title: Some("Lost".into()),
                ..FixtureTags::default()
            },
            corrupt: false,
            format: Format::Mp3,
        })
        .write_all()
        .expect("write fixtures");

    let tracks = scan_sorted(dir.path());
    assert_eq!(
        albums(&tracks),
        ["(no album)", "Lost Sessions", "Dated"],
        "{:?}",
        labels(&tracks)
    );
}

/// A deterministic mess of artists, albums, discs and years — including missing
/// years and an untagged artist — that must sort into well-formed blocks.
fn generated_library() -> Vec<Track> {
    let artists = ["Artist One", "Artist Two", ""];
    let albums = ["", "Same", "Works 2", "Works 10"];
    let years: [Option<i32>; 4] = [None, Some(1990), Some(2005), Some(1998)];
    let mut tracks = Vec::new();
    let mut index = 0usize;
    for artist in artists {
        for album in albums {
            for (position, year) in years.iter().enumerate() {
                let mut track = Track::from_path(PathBuf::from(format!("/gen/{index:04}.mp3")));
                if !artist.is_empty() {
                    track.artist = Some(artist.to_string());
                }
                if !album.is_empty() {
                    track.album = Some(album.to_string());
                }
                track.disc_no = Some(1 + (index % 3) as u32);
                track.track_no = Some(1 + ((index + position) % 4) as u32);
                track.year = *year;
                track.refresh_key();
                tracks.push(track);
                index += 1;
            }
        }
    }
    tracks.reverse();
    tracks
}

#[test]
fn generated_libraries_sort_into_whole_artists_and_whole_albums() {
    let mut tracks = generated_library();
    sort_tracks(&mut tracks);

    assert_contiguous(&tracks, |t| t.key.artist.clone(), "artist");
    assert_contiguous(
        &tracks,
        |t| (t.key.artist.clone(), t.key.album.clone()),
        "album",
    );
    assert_album_years_rise_inside_artists(&tracks);

    // Inside one album, disc then track still decides.
    let mut previous: Option<(&str, &str, u32, u32)> = None;
    for track in &tracks {
        let here = (
            track.key.artist.as_str(),
            track.key.album.as_str(),
            track.key.disc,
            track.key.track,
        );
        if let Some((artist, album, disc, number)) = previous
            && artist == here.0
            && album == here.1
        {
            assert!(
                (disc, number) <= (here.2, here.3),
                "disc/track order broken: {:?}",
                labels(&tracks)
            );
        }
        previous = Some((here.0, here.1, here.2, here.3));
    }

    // Sorting is a fixed point, album years included.
    let before = labels(&tracks);
    sort_tracks(&mut tracks);
    assert_eq!(labels(&tracks), before, "re-sorting must be a no-op");
}

#[test]
fn a_run_still_spans_the_whole_artist_when_the_years_are_scrambled() {
    let mut tracks = generated_library();
    sort_tracks(&mut tracks);
    let mut playlist = Playlist::new(tracks);
    let mut index = 0usize;
    let mut checked = 0usize;
    while index < playlist.len() {
        playlist.select_start(index);
        let (start, end) = playlist.run().expect("a run for every row");
        let artist = playlist
            .get(start)
            .expect("run start exists")
            .key
            .artist
            .clone();
        let rows = end - start;
        assert!(rows >= 1);
        assert!(
            playlist.get(end).is_none_or(|t| t.key.artist != artist),
            "run {start}..{end} stops at the artist boundary"
        );
        // Clicking the first row of a run queues exactly that run: walking the
        // run from its first row reaches its last row and then stops.
        playlist.select_start(start);
        for _ in 1..rows {
            assert!(
                playlist.advance().is_some(),
                "row {start} must advance inside its own run"
            );
        }
        assert!(
            playlist.advance().is_none(),
            "the last row of run {start}..{end} must end the artist"
        );
        checked += 1;
        index = end;
    }
    assert!(checked >= 3, "expected several runs, got {checked}");
}

#[test]
fn applying_album_years_twice_changes_nothing() {
    let mut tracks = generated_library();
    sort_tracks(&mut tracks);
    let once: Vec<i64> = tracks.iter().map(|t| t.key.album_year).collect();
    music_tui::model::apply_album_years(&mut tracks);
    let twice: Vec<i64> = tracks.iter().map(|t| t.key.album_year).collect();
    assert_eq!(once, twice);
}
