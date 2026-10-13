# ADR 0002 — Sort key and artist-run boundary semantics

Status: accepted (2026-10-13), amended 2026-10-13 by `mutu-172` (§3.2: an
artist's albums are ordered by release year, not by album title)
Decides: `mutu-2h8.2`. Implemented by `src/model/sort.rs`, `src/model/playlist.rs`,
specified here normatively.

`tests/adr_spec.rs` parses the worked-example table in §6 and fails if the
implementation and this document disagree. `src/model/fixtures.rs` is the same
table as Rust data.

## 1. Vocabulary

* **row** — one line in the list pane, one `Track`.
* **tag** — metadata read from the file by the scanner (all tags optional).
* **sort artist** — `album_artist` if present and non-blank, else `artist`, else `""`.
* **album** — the pair (sort artist, normalized `album`); two files with the
  same artist and the same album title belong to the same album, whoever
  performs them.
* **album year** — the earliest year written anywhere on an album, see §3.2.
* **normalized** — the transformation in §4.
* **artist run** — see §5.

## 2. Normalization

Every tag used for ordering or grouping is normalized before use:

1. strip leading and trailing whitespace,
2. collapse every internal run of whitespace (including newlines and tabs) into
   a single `U+0020`,
3. `char::to_lowercase` each character (Unicode simple/conditional folding as
   implemented by the standard library).

`"the   BEATLES "` and `"The Beatles"` normalize to the identical string.
No locale collation, no unicode NFKD and no leading-article ("the", "a", "el")
stripping: comparison is by code point after case folding, which is cheap,
stable and predictable. Consequence: non-Latin artists sort after ASCII artists.

## 3. Sort key and sentinel values

Rows are ordered by the first differing component:

| priority | field            | source                                     | missing-value sentinel                                    |
|---------:|------------------|--------------------------------------------|-----------------------------------------------------------|
| 1        | sort artist      | `album_artist` else `artist`, normalized    | `""` (empty string sorts before every non-empty string)    |
| 2        | album year       | earliest year of the row's album (§3.2)     | `0` — an undated album sorts **first** inside its artist    |
| 3        | album            | `album`, normalized                         | `""`                                                       |
| 4        | disc number      | `disc`                                      | `1`                                                        |
| 5        | track number     | `track`                                     | `u32::MAX` — untagged files land **last** inside the album |
| 6        | release year     | year component of `date`, else `YEAR` tag   | `0` — breaks ties between rows of the same album           |
| 7        | file name        | file name of the path, normalized           | — (always present)                                         |
| 8        | full path        | absolute path, normalized, `/` separators   | — (always present)                                         |

String components (1, 3, 7, 8) compare with **natural comparison** (§4.1);
numeric components compare as integers.

Components 7 and 8 make the order a *total order*: no two rows can tie unless
they are the same file, so the sorted list is byte-for-byte identical between
runs and between machines.

The order reads as "artist, then that artist's records in the order they came
out, then the songs on each record": albums are dated before they are spelled,
and the album title only separates releases from the same year.

### 3.1 Empty and blank tags

A tag that is absent, empty, or only whitespace is treated as missing, so an
untagged file sorts to the front of the list with `sort artist == ""`. All
untagged files form a group ordered by album year (`0`), then file name/path —
they are also an artist run (run 1 in §6), because the run rule is purely "same
sort artist".

### 3.2 Album year

An artist's albums are ordered by **release year**, because that is the order
people browse a catalogue in (debut first); alphabetical album titles scatter a
catalogue across the list — `Apollo 18` (1992) before `Flood` (1990) reads as a
bug, and it was reported as one (`mutu-172`).

The year component of the key is a property of the **album**, not of the row:

```
album_year(artist, album) = min { year(t) : t in album, year(t) != 0 }    # 0 if none
```

Two consequences, both deliberate:

* **A whole album moves as one block.** Tag files are messy: a 3-CD set may
  carry `1990` on disc 1, `1992` on disc 2 and nothing on the bonus tracks. If
  each row's own year were compared at album priority, that album would shatter
  into pieces — the undated rows (`0`) would jump to the front of the artist,
  ahead of everything else they own. Deriving one year per album and comparing
  that keeps every album contiguous, and keeps contiguity independent of which
  files happen to be tagged.
* **The earliest year wins.** A reissue date must not decide where a record sits
  in the catalogue; the earliest dated row is the closest thing to an original
  release date that the folder actually contains. The accepted cost is the
  mirror image — one stray *early* year (a "recorded 1978" bonus track on a 1990
  album) dates its album earlier — which is still a calmer failure than the
  per-row rule, which would cut that album into pieces.

Because `album_year` is constant per album, sorting by it cannot interleave two
albums, and because it sits behind the sort artist it cannot break an artist run
apart either (§5). It is computed once per scan over the whole library
(`sort::apply_album_years`), which is the only allocating step in sorting; the
comparator itself still allocates nothing. Sorting the same library twice gives
the same answer, since the value depends only on the rows present.

An undated album keeps the `0` sentinel and sorts first inside its artist, in
album-title order — the same "missing sorts first" rule as every other missing
component (§3.1, §4.2). A library with no years at all therefore degrades to
exactly the pre-`mutu-172` order: artist, then album title, then disc, then
track.

Example — one artist, four files:

| # | album       | disc | track | year | album year |
|--:|-------------|-----:|------:|-----:|-----------:|
| 1 | Greatest    | 1    | 1     | –    | 0          |
| 2 | Flood       | 1    | 1     | 1990 | 1990       |
| 3 | Flood       | 1    | 2     | –    | 1990       |
| 4 | Apollo 18   | 1    | 1     | 1992 | 1992       |

Row 3 has no year of its own yet stays with the rest of *Flood*; *Greatest* has
no dated row at all, so it leads the artist on the `0` sentinel; and *Apollo 18*
follows *Flood* even though it sorts before *Flood* as a title.

## 4. Comparison rules

### 4.1 Natural (numeric-aware) comparison

A string is split into alternating runs of ASCII digits and non-digits.

* `Txt` runs compare character by character after `to_lowercase`.
* `Num` runs compare numerically: leading zeros are ignored, a longer
  zero-stripped digit string is larger, and equal-length strings compare
  lexicographically. So `2 < 10` and `Track 2 < Track 10`.
* A numeric tie (`1` vs `01`) is broken in favour of the spelling with fewer
  leading zeros, so ordering is total.
* A digit run sorts before a text run when the two strings diverge on run kind.
* When one string runs out of runs, the shorter one is smaller.

### 4.2 Empty versus missing

`""` (missing) compares as an empty run sequence, i.e. smaller than any non-empty
string. There is no separate "unknown" sentinel string.

## 5. Artist-run boundary

```
artist(i)     = normalized sort artist of row i
run(i).start  = greatest j <= i with artist(j) == artist(i)   # contiguous
run(i).end    = smallest j >  i with artist(j) != artist(i)   # exclusive
                (or len(rows) if there is no such j)
```

Because the list is sorted by artist first, the rows sharing an artist are
always contiguous, so "maximal contiguous block" and "maximal block" coincide.
The album year (§3.2) sits behind the sort artist and is constant for every row
of an album, so it can neither split an artist run in half nor interleave two
albums.

Playback rules that follow from this:

* Clicking row `i` sets the cursor to `i` and makes `run(i)` the play queue.
* After a track finishes, play the next row inside `run(i)`; when the cursor is
  on `run(i).end - 1`, **stop** and report "End of artist".
* A click in the middle of a run plays that row and only the rows after it in
  the same run — it never wraps to the beginning of the run and never continues
  into the next artist.
* A run of one row plays that row and stops.
* `n` / `p` (next/previous) move only inside the current run and say so at the
  boundary; they never cross into another artist.

`album_artist` is what groups compilations: on `various artists` samplers every
song has a different `artist` but the same `album_artist`, so the sampler is one
run (`Various Artists`), which is what a user clicking a sampler track expects.

Case and whitespace variants of the same artist (`"the   BEATLES"` vs
`"The Beatles"`) normalize equal and therefore belong to the **same** run even
when the tags disagree on spelling (§6 rows 15–17).

## 6. Worked example table

22 fixture rows. Rows are listed in **final sorted order**. `–` means "tag
missing"; `(empty)` means "tag present but blank" (both normalize to `""`).
`run` numbers the artist runs.

| #   | file                  | artist        | album artist   | album        | disc | track | year | run |
|----:|-----------------------|---------------|----------------|--------------|-----:|------:|-----:|----:|
| 1   | untagged-alpha.flac   | (empty)       | (empty)        | (empty)      | –    | –     | –    | 1   |
| 2   | untagged-beta.mp3     | (empty)       | (empty)        | (empty)      | –    | –     | –    | 1   |
| 3   | aaron-singles.mp3     | aaron         | –              | Singles      | 1    | 1     | 2001 | 2   |
| 4   | ab-album1-d1t1.mp3    | Alpha Beta    | –              | Album 1      | 1    | 1     | 1998 | 3   |
| 5   | ab-album1-d1t2.mp3    | Alpha Beta    | –              | Album 1      | 1    | 2     | 1998 | 3   |
| 6   | ab-album1-d2t1.mp3    | Alpha Beta    | –              | Album 1      | 2    | 1     | 1998 | 3   |
| 7   | ab-album2-d1t1.mp3    | Alpha Beta    | –              | Album 2      | 1    | 1     | 1999 | 3   |
| 8   | ab-album10-d1t1.mp3   | Alpha Beta    | –              | Album 10     | 1    | 1     | 2003 | 3   |
| 9   | bw-double-d1t1.flac   | Beta Wave     | –              | Double       | 1    | 1     | 1994 | 4   |
| 10  | bw-double-d1t2.flac   | Beta Wave     | –              | Double       | 1    | 2     | 1994 | 4   |
| 11  | bw-double-d2t1.flac   | Beta Wave     | –              | Double       | 2    | 1     | 1994 | 4   |
| 12  | bw-double-d10t1.flac  | Beta Wave     | –              | Double       | 10   | 1     | –    | 4   |
| 13  | bw-eleven-d1t1.mp3    | Beta Wave     | –              | Eleven       | 1    | 1     | 1996 | 4   |
| 14  | beatles-red-d1t1.flac | The Beatles   | –              | 1962-1966    | 1    | 1     | 1973 | 5   |
| 15  | beatles-red-d1t2.flac | The Beatles   | –              | 1962-1966    | 1    | 2     | 1973 | 5   |
| 16  | beatles-blue-d1t1.mp3 | the   BEATLES | –              | 1967-1970    | 1    | 1     | 1973 | 5   |
| 17  | vox-tracks2.mp3       | Sister Hazel  | Various Artists | Tracks Of 2 | 1    | 3     | 2002 | 6   |
| 18  | vox-tracks10.mp3      | Guster        | Various Artists | Tracks Of 10 | 1   | 1     | 2002 | 6   |
| 19  | zeta-works2-d1t2.mp3  | Zeta Nine     | –              | Works 2      | 1    | 2     | 2007 | 7   |
| 20  | zeta-works10-d1t10.mp3| Zeta Nine     | –              | Works 10     | 1    | 10    | 2007 | 7   |
| 21  | zeta-same-a.mp3       | Zeta Nine     | –              | Same         | –    | –     | 2009 | 7   |
| 22  | zeta-same-b.mp3       | Zeta Nine     | –              | Same         | –    | –     | 2011 | 7   |

What each boundary proves:

* **1–2** untagged/blank tags group first; both albums are undated (album year
  `0`), so the file-name tiebreaker orders `alpha` before `beta`. They are run 1
  (the "unknown artist" run).
* **4–8** one artist, three albums released `1998 < 1999 < 2003`, which happens
  to be their title order too; `Album 1 < Album 2 < Album 10` proves the natural
  numeric comparison (it decides the order of rows 7 and 8 the moment two
  albums share a year), and row 6 shows disc 2 of *Album 1* staying inside that
  album (album year before disc, disc before track).
* **9–13** `Double` disc `1, 1, 2, 10` proves disc-before-track and natural
  disc numbers; row 12 carries **no year at all** and still stays inside
  *Double*, because an album's year comes from the album, not from the row
  (§3.2); `Double` (1994) before `Eleven` (1996) proves album ordering.
* **14–16** `"the   BEATLES"` normalizes equal to `"The Beatles"`, so it stays in
  run 5 instead of starting a new one. Both *Beatles* albums are dated 1973, so
  their order falls back to the natural comparison of `1962-1966` and `1967-1970`.
* **17–18** a compilation: different `artist` per song, shared `album_artist`,
  one run; both samplers are dated 2002, so `Tracks Of 2 < Tracks Of 10` proves
  natural comparison between same-year albums.
* **19–22** the album-year rule beating the title rule: *Works 2* and *Works 10*
  (2007) come before *Same* (2009, 2011) although `Same` sorts before `Works` as
  a title.
* **19–20** `Works 2 < Works 10` — same album year, so natural comparison of the
  titles decides.
* **21–22** identical keys down to track number (missing → `u32::MAX`) are split
  by the row's own year (`2009` before `2011`).

Expected run boundaries: `1–2`, `3–3`, `4–8`, `9–13`, `14–16`, `17–18`, `19–22`.

## 7. Where this is enforced

| rule                          | test                                                  |
|-------------------------------|--------------------------------------------------------|
| table order and run column    | `tests/adr_spec.rs` (parses §6)                        |
| natural comparison            | `model::sort::tests::natural_numeric_ordering`          |
| antisymmetry/transitivity     | `model::sort::tests::total_order_properties`            |
| sentinel values               | `model::track::tests::missing_tag_sentinels`            |
| album year before album title | §6 rows 19–22, `model::sort::tests::albums_order_by_year_inside_an_artist` |
| album year is album-wide      | §6 row 12, `model::sort::tests::an_album_stays_whole_when_its_rows_disagree_on_the_year` |
| undated album sorts first     | `model::sort::tests::undated_albums_come_first_inside_the_artist_then_sort_by_title` |
| album year never leaks between artists | `model::sort::tests::album_year_is_scoped_to_its_own_artist` |
| albums and runs stay contiguous | `tests/album_year_order.rs`                           |
| album order from real tags    | `tests/album_year_order.rs` (scanned files, not fixtures) |
| re-sort is a no-op            | `tests/sort_spec.rs::resorting_is_a_no_op`              |
| run boundaries                | `tests/playlist_run.rs`                                 |
| click mid-run, stop at end    | `tests/reducer_playback.rs`                             |
| same, end to end (real loop)  | `tests/headless_loop.rs`                                |
