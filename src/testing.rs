//! Test-support: generate a synthetic music library with real audio bytes and
//! real tags, without needing an encoder on the machine.
//!
//! WAV files are synthesized as PCM, FLAC as a header-only stream (STREAMINFO +
//! VORBIS_COMMENT), MP3 as a run of valid MPEG-1 Layer III frame headers. That
//! is enough for `lofty` to report duration, bitrate and tags, which is all the
//! scanner reads. `src/bin/make-fixtures.rs` writes the same tree to disk.
//!
//! Used by the integration tests; also handy for manual testing.

use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::prelude::{Accessor as _, AudioFile as _, ItemKey, TaggedFileExt as _};
use lofty::tag::Tag;
use lofty::tag::items::Timestamp;

/// Tag values to write into a generated file.
#[derive(Debug, Clone, Default)]
pub struct FixtureTags {
    /// `ARTIST`.
    pub artist: Option<String>,
    /// `ALBUM_ARTIST`.
    pub album_artist: Option<String>,
    /// `ALBUM`.
    pub album: Option<String>,
    /// `TITLE`.
    pub title: Option<String>,
    /// `TRACK`.
    pub track: Option<u32>,
    /// `DISC`.
    pub disc: Option<u32>,
    /// `DATE` (year).
    pub year: Option<u32>,
}

/// One requested fixture file.
#[derive(Debug, Clone)]
pub struct FixtureFile {
    /// Path relative to the library root.
    pub path: PathBuf,
    /// Tags to write (empty = untagged).
    pub tags: FixtureTags,
    /// Truncate the file after writing (simulates a corrupt download).
    pub corrupt: bool,
    /// Container to synthesize.
    pub format: Format,
}

/// Containers the generator can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// PCM WAV.
    Wav,
    /// Header-only FLAC.
    Flac,
    /// Frame-header MP3.
    Mp3,
}

/// PCM WAV with `seconds` of a sine-ish tone at 8 kHz.
#[must_use]
pub fn wav_bytes(seconds: f32) -> Vec<u8> {
    let rate = 8000u32;
    let samples = (rate as f32 * seconds) as usize;
    let data_len = samples * 2;
    let mut out: Vec<u8> = Vec::with_capacity(44 + data_len);
    let file_len: u32 = (36 + data_len) as u32;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&file_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2u32).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for i in 0..samples {
        let value = ((i as f32 / 8.0).sin() * 12000.0) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Frame-header MP3 (valid headers, silence payload) of `seconds` length.
#[must_use]
pub fn mp3_bytes(seconds: f32) -> Vec<u8> {
    // MPEG-1 Layer III, 128 kbit/s, 44.1 kHz, no padding.
    let header: [u8; 4] = [0xFF, 0xFB, 0x90, 0x44];
    let frame_len = 417usize;
    let frames = ((seconds * 44100.0) / 1152.0).ceil() as usize;
    let mut out: Vec<u8> = Vec::with_capacity(frames * frame_len);
    for _ in 0..frames.max(1) {
        out.extend_from_slice(&header);
        while !out.len().is_multiple_of(frame_len) {
            out.push(0);
        }
    }
    out
}

/// Header-only FLAC: `fLaC` + STREAMINFO + VORBIS_COMMENT.
#[must_use]
pub fn flac_bytes(seconds: f32) -> Vec<u8> {
    let rate = 44100u32;
    let total_samples = ((f64::from(rate) * f64::from(seconds)) as u64).max(1);
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"fLaC");

    // STREAMINFO, big-endian bit packing (spec section 6.2.2).
    let mut info = BitWriter::new(34);
    info.put(4096, 16); // minimum block size
    info.put(4096, 16); // maximum block size
    info.put(0, 24); // minimum frame size (unknown)
    info.put(0, 24); // maximum frame size (unknown)
    info.put(u64::from(rate), 20);
    info.put(1, 3); // channels - 1
    info.put(16, 5); // bits per sample - 1 -> 16
    info.put(total_samples, 36);
    // The trailing 16 MD5 bytes stay zero (a zero MD5 means "unset").
    let info = info.finish();

    let mut comment: Vec<u8> = Vec::new();
    let vendor = b"music-tui fixture";
    comment.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    comment.extend_from_slice(vendor);
    comment.extend_from_slice(&0u32.to_le_bytes()); // zero comments

    // Metadata block header = 1 flag byte + a 24-bit big-endian length.
    out.push(0x00); // STREAMINFO, not the last block
    out.extend_from_slice(&block_len(34));
    out.extend_from_slice(&info);
    out.push(0x84); // VORBIS_COMMENT, last block
    out.extend_from_slice(&block_len(comment.len()));
    out.extend_from_slice(&comment);
    out
}

/// FLAC metadata block header length field: 24-bit big endian.
fn block_len(len: usize) -> [u8; 3] {
    let len = len & 0x00FF_FFFF;
    [(len >> 16) as u8, (len >> 8) as u8, (len & 0xFF) as u8]
}

/// Minimal big-endian bit packer for the FLAC stream info block.
struct BitWriter {
    bytes: Vec<u8>,
    used: usize,
}

impl BitWriter {
    fn new(len: usize) -> Self {
        Self {
            bytes: vec![0u8; len],
            used: 0,
        }
    }

    fn put(&mut self, value: u64, bits: usize) {
        assert!(bits <= u64::BITS as usize, "cannot pack {bits} bits");
        for bit in (0..bits).rev() {
            let bit_value = (value >> bit) & 1;
            if bit_value == 1 {
                self.bytes[self.used / 8] |= 1 << (7 - (self.used % 8));
            }
            self.used += 1;
        }
    }

    fn finish(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

/// Write one fixture file (and its tags) under `root`.
pub fn write_fixture(root: &Path, fixture: &FixtureFile) -> std::io::Result<PathBuf> {
    let path = root.join(&fixture.path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = match fixture.format {
        Format::Wav => wav_bytes(0.5),
        Format::Mp3 => mp3_bytes(0.6),
        Format::Flac => flac_bytes(0.5),
    };
    std::fs::write(&path, &bytes)?;
    if fixture.corrupt {
        // Keep a plausible file name but cut the body down to a few bytes: no
        // decoder can use it, which is exactly the case the scanner must survive.
        let truncated = &bytes[..bytes.len().min(3)];
        std::fs::write(&path, truncated)?;
        return Ok(path);
    }
    if !matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("wav") | Some("mp3") | Some("flac")
    ) {
        // Non-audio fixtures exist only to prove the scanner skips them.
        return Ok(path);
    }
    if let Err(err) = write_tags(&path, &fixture.tags) {
        // A file we cannot tag is still a valid fixture (it becomes a "corrupt"
        // row), but say so loudly in tests.
        eprintln!("music-tui fixture: could not tag {}: {err}", path.display());
    }
    Ok(path)
}

fn write_tags(path: &Path, tags: &FixtureTags) -> Result<(), String> {
    let mut tagged = lofty::probe::Probe::open(path)
        .and_then(|probe| probe.read())
        .map_err(|err| err.to_string())?;
    let tag_type = tagged.file_type().primary_tag_type();
    if tagged.primary_tag_mut().is_none() && tagged.first_tag_mut().is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .first_tag_mut()
        .ok_or_else(|| "no tag container".to_string())?;
    if let Some(artist) = &tags.artist {
        tag.set_artist(artist.clone());
    }
    if let Some(album) = &tags.album {
        tag.set_album(album.clone());
    }
    if let Some(title) = &tags.title {
        tag.set_title(title.clone());
    }
    if let Some(album_artist) = &tags.album_artist {
        tag.insert_text(ItemKey::AlbumArtist, album_artist.clone());
    }
    if let Some(track) = tags.track {
        tag.set_track(track);
    }
    if let Some(disc) = tags.disc {
        tag.set_disk(disc);
    }
    if let Some(year) = tags.year {
        tag.set_date(Timestamp {
            year: year as u16,
            ..Default::default()
        });
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|err| err.to_string())
}

/// A small library that exercises every scanner path: nested folders, several
/// containers, multi-disc and compilation tags, an untagged file, a corrupt
/// file, Unicode and spaced names, and non-audio files that must be ignored.
#[must_use]
pub fn synthetic_library() -> Vec<FixtureFile> {
    let mut files = vec![
        FixtureFile {
            path: "alpha/first/one.wav".into(),
            tags: FixtureTags {
                artist: Some("Alpha Beta".into()),
                album: Some("Album 1".into()),
                title: Some("First Song".into()),
                track: Some(1),
                disc: Some(1),
                year: Some(1998),
                album_artist: None,
            },
            corrupt: false,
            format: Format::Wav,
        },
        FixtureFile {
            path: "alpha/first/two.wav".into(),
            tags: FixtureTags {
                artist: Some("Alpha Beta".into()),
                album: Some("Album 1".into()),
                title: Some("Second Song".into()),
                track: Some(2),
                disc: Some(1),
                year: Some(1998),
                album_artist: None,
            },
            corrupt: false,
            format: Format::Wav,
        },
        FixtureFile {
            path: "alpha/tenth/ten.mp3".into(),
            tags: FixtureTags {
                artist: Some("Alpha Beta".into()),
                album: Some("Album 10".into()),
                title: Some("Tenth Song".into()),
                track: Some(1),
                year: Some(2003),
                disc: None,
                album_artist: None,
            },
            corrupt: false,
            format: Format::Mp3,
        },
        FixtureFile {
            path: "beta/double/disc1-a.flac".into(),
            tags: FixtureTags {
                artist: Some("Beta Wave".into()),
                album: Some("Double".into()),
                title: Some("D1T1".into()),
                track: Some(1),
                disc: Some(1),
                year: Some(1994),
                album_artist: None,
            },
            corrupt: false,
            format: Format::Flac,
        },
        FixtureFile {
            path: "beta/double/disc2-a.flac".into(),
            tags: FixtureTags {
                artist: Some("Beta Wave".into()),
                album: Some("Double".into()),
                title: Some("D2T1".into()),
                track: Some(1),
                disc: Some(2),
                year: Some(1994),
                album_artist: None,
            },
            corrupt: false,
            format: Format::Flac,
        },
        // Compilation: same album_artist, different artists.
        FixtureFile {
            path: "comp/track3.mp3".into(),
            tags: FixtureTags {
                artist: Some("Sister Hazel".into()),
                album_artist: Some("Various Artists".into()),
                album: Some("Tracks Of 2".into()),
                title: Some("Album Rocker".into()),
                track: Some(3),
                year: Some(2002),
                disc: None,
            },
            corrupt: false,
            format: Format::Mp3,
        },
        FixtureFile {
            path: "comp/track1.mp3".into(),
            tags: FixtureTags {
                artist: Some("Guster".into()),
                album_artist: Some("Various Artists".into()),
                album: Some("Tracks Of 10".into()),
                title: Some("Airport Song".to_string()),
                track: Some(1),
                year: Some(2002),
                disc: None,
            },
            corrupt: false,
            format: Format::Mp3,
        },
        // Untagged (no tags at all) plus a deliberately truncated file.
        FixtureFile {
            path: "misc/untagged.wav".into(),
            tags: FixtureTags::default(),
            corrupt: false,
            format: Format::Wav,
        },
        FixtureFile {
            path: "misc/broken.mp3".into(),
            tags: FixtureTags::default(),
            corrupt: true,
            format: Format::Mp3,
        },
        // Unicode + spaces in the name, must survive truncation and sorting.
        FixtureFile {
            path: "misc/日本語 — «live» take.wav".into(),
            tags: FixtureTags {
                artist: Some("The   BEATLES".into()),
                album: Some("1967-1970".into()),
                title: Some("日本語のタイトル".into()),
                track: Some(1),
                year: Some(1973),
                disc: None,
                album_artist: None,
            },
            corrupt: false,
            format: Format::Wav,
        },
        // Things that must never become rows.
        FixtureFile {
            path: "notes/readme.txt".into(),
            tags: FixtureTags::default(),
            corrupt: false,
            format: Format::Wav,
        },
        FixtureFile {
            path: "cover.jpg".into(),
            tags: FixtureTags::default(),
            corrupt: false,
            format: Format::Wav,
        },
    ];
    // A dot-directory that must be skipped.
    files.push(FixtureFile {
        path: ".hidden/hidden.wav".into(),
        tags: FixtureTags {
            artist: Some("Hidden".into()),
            album: None,
            title: None,
            track: None,
            disc: None,
            year: None,
            album_artist: None,
        },
        corrupt: false,
        format: Format::Wav,
    });
    files
}

/// Write the synthetic library into `root`.
pub fn write_synthetic_library(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for fixture in synthetic_library() {
        // The two non-audio fixtures are written as real text/image bytes.
        let path = write_fixture(root, &fixture)?;
        if let Some(ext) = path.extension()
            && (ext == "txt" || ext == "jpg")
        {
            std::fs::write(&path, b"not audio, and must be ignored")?;
        }
        paths.push(path);
    }
    Ok(paths)
}

/// Builder for ad-hoc libraries in tests.
///
/// ```no_run
/// use music_tui::testing::FixtureSet;
/// let set = FixtureSet::new(std::path::Path::new("/tmp/lib"))
///     .song("a/one.mp3", "One", "Artist", "Album", 1, 1, 2000);
/// ```
#[derive(Debug, Default)]
pub struct FixtureSet {
    root: PathBuf,
    files: Vec<FixtureFile>,
}

impl FixtureSet {
    /// An empty set rooted at `root`.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            files: Vec::new(),
        }
    }

    /// Append one fully tagged audio file (container chosen from the suffix).
    #[must_use]
    #[allow(clippy::too_many_arguments, reason = "mirrors a tag table")]
    pub fn song(
        mut self,
        rel: &str,
        title: &str,
        artist: &str,
        album: &str,
        disc: u32,
        track: u32,
        year: u32,
    ) -> Self {
        self.files.push(FixtureFile {
            path: PathBuf::from(rel),
            tags: FixtureTags {
                artist: Some(artist.to_string()),
                album_artist: None,
                album: Some(album.to_string()),
                title: Some(title.to_string()),
                track: Some(track),
                disc: Some(disc),
                year: Some(year),
            },
            corrupt: false,
            format: format_for(rel),
        });
        self
    }

    /// Append a file with no tags at all.
    #[must_use]
    pub fn untagged(mut self, rel: &str) -> Self {
        self.files.push(FixtureFile {
            path: PathBuf::from(rel),
            tags: FixtureTags::default(),
            corrupt: false,
            format: format_for(rel),
        });
        self
    }

    /// Append a file whose bytes are truncated (no decoder can read it).
    #[must_use]
    pub fn corrupt(mut self, rel: &str) -> Self {
        self.files.push(FixtureFile {
            path: PathBuf::from(rel),
            tags: FixtureTags::default(),
            corrupt: true,
            format: format_for(rel),
        });
        self
    }

    /// Append anything (escape hatch).
    #[must_use]
    pub fn raw(mut self, fixture: FixtureFile) -> Self {
        self.files.push(fixture);
        self
    }

    /// The requested files, in order.
    #[must_use]
    pub fn files(&self) -> &[FixtureFile] {
        &self.files
    }

    /// Write every file and return the paths in the order given.
    pub fn write_all(&self) -> std::io::Result<Vec<PathBuf>> {
        self.files
            .iter()
            .map(|fixture| write_fixture(&self.root, fixture))
            .collect()
    }
}

/// Pick a container from a file name suffix.
#[must_use]
pub fn format_for(name: &str) -> Format {
    match Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("mp3")
        .to_ascii_lowercase()
        .as_str()
    {
        "wav" => Format::Wav,
        "flac" => Format::Flac,
        _ => Format::Mp3,
    }
}
