//! Guards the contract that `docs/adr/0002-sort-and-artist-run.md` describes
//! exactly what the code does: the test parses the markdown table in §6 and
//! compares it against `src/model/fixtures.rs` and the real comparator.

use music_tui::model::{fixture_tracks, sort_tracks, FixtureRow, TABLE};

/// One row parsed out of the markdown table.
struct DocRow {
    file: String,
    artist: Option<String>,
    album_artist: Option<String>,
    album: Option<String>,
    disc: Option<u32>,
    track: Option<u32>,
    year: Option<u32>,
    run: usize,
}

fn cell_to_text(cell: &str) -> Option<String> {
    let trimmed = cell.trim();
    match trimmed {
        "" | "-" | "\u{2013}" | "\u{2014}" | "(empty)" | "\u{2013} " => None,
        other => Some(other.to_string()),
    }
}

fn cell_to_number(cell: &str) -> Option<u32> {
    cell_to_text(cell).and_then(|text| text.parse().ok())
}

fn parse_table(markdown: &str) -> Vec<DocRow> {
    let mut rows = Vec::new();
    for line in markdown.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() != 9 {
            continue;
        }
        // Skip the header and the separator row.
        if cells[0].eq_ignore_ascii_case("#") || cells[0].chars().all(|c| c == '-' || c == ':') {
            continue;
        }
        let Some(run_value) = cell_to_number(cells[8]) else {
            continue;
        };
        rows.push(DocRow {
            file: cells[1].to_string(),
            artist: cell_to_text(cells[2]),
            album_artist: cell_to_text(cells[3]),
            album: cell_to_text(cells[4]),
            disc: cell_to_number(cells[5]),
            track: cell_to_number(cells[6]),
            year: cell_to_number(cells[7]),
            run: usize::try_from(run_value).unwrap_or(0),
        });
    }
    rows
}

fn adr() -> String {
    let path = format!(
        "{}/docs/adr/0002-sort-and-artist-run.md",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"))
}

#[test]
fn the_spec_table_exists_and_has_at_least_12_rows() {
    let rows = parse_table(&adr());
    assert!(
        rows.len() >= 12,
        "expected the spec table to have >= 12 rows, found {}",
        rows.len()
    );
}

#[test]
fn the_spec_table_matches_the_rust_fixtures() {
    let rows = parse_table(&adr());
    assert_eq!(
        rows.len(),
        TABLE.len(),
        "doc has {} rows, code has {}",
        rows.len(),
        TABLE.len()
    );
    for (doc, code) in rows.iter().zip(TABLE.iter()) {
        assert_eq!(doc.file, code.file, "file name mismatch");
        assert_eq!(doc.run, code.run, "run mismatch for {}", doc.file);
        let normalize = |value: Option<&str>| -> Option<String> {
            value
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        assert_eq!(
            doc.artist.as_deref(),
            normalize(code.artist).as_deref(),
            "artist mismatch for {}",
            doc.file
        );
        assert_eq!(
            doc.album_artist.as_deref(),
            normalize(code.album_artist).as_deref(),
            "album_artist mismatch for {}",
            doc.file
        );
        assert_eq!(
            doc.album.as_deref(),
            normalize(code.album).as_deref(),
            "album mismatch for {}",
            doc.file
        );
        assert_eq!(doc.disc, code.disc, "disc mismatch for {}", doc.file);
        assert_eq!(doc.track, code.track, "track mismatch for {}", doc.file);
        assert_eq!(
            doc.year.map(|year| i32::try_from(year).unwrap_or(0)),
            code.year,
            "year mismatch for {}",
            doc.file
        );
    }
}

#[test]
fn the_comparator_produces_the_documented_order_and_runs() {
    let rows = parse_table(&adr());
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);

    let actual: Vec<String> = tracks
        .iter()
        .map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let documented: Vec<String> = rows.iter().map(|r| r.file.clone()).collect();
    assert_eq!(actual, documented, "sorted order must match the spec table");

    // Run boundaries as documented in §5/§6.
    let mut playlist = music_tui::model::Playlist::new(tracks);
    let mut index = 0usize;
    let mut runs = Vec::new();
    while index < playlist.len() {
        playlist.select_start(index);
        let (start, end) = playlist.run().expect("a run for every row");
        runs.push((start + 1, end));
        index = end;
    }
    let mut expected: Vec<(usize, usize)> = Vec::new();
    let mut last_run = 0usize;
    for (i, row) in rows.iter().enumerate() {
        if row.run != last_run {
            expected.push((i + 1, i + 1));
            last_run = row.run;
        }
        if let Some(last) = expected.last_mut() {
            last.1 = i + 1;
        }
    }
    assert_eq!(runs, expected, "artist-run boundaries must match the spec");
}

#[test]
fn the_spec_documents_the_required_edge_cases() {
    let text = adr();
    for needle in [
        "album_artist",
        "contiguous",
        "End of artist",
        "u32::MAX",
        "natural",
    ] {
        assert!(text.contains(needle), "spec must mention {needle:?}");
    }
    let _: Option<FixtureRow> = None;
}
