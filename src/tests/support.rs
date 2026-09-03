//! Helpers shared by every test file in this suite. Nothing in here is
//! `#[test]` itself.
//!
//! Three tiers of helper live here, roughly from lowest- to
//! highest-level:
//!   - Plumbing: `temp_path`, `rw_temp_file`, `utf16le_ascii`, `read_records`,
//!     `test_config` -- building blocks every test file uses directly.
//!   - Pipeline simulators: `merge_test_encoding_chunks`,
//!     `merge_test_single_chunk`, `merge_test_full` -- each re-creates a
//!     different slice of the real scan -> merge -> output pipeline
//!     (see `merger.rs`/`outputter.rs`) from hand-built inputs, so tests
//!     can check the merge/join logic without needing a real file scan.
//!   - `scan_all_chunks` -- the one helper that *does* run the real
//!     scanner across real `Chunk` boundaries, for tests that care about
//!     genuine chunking behavior end-to-end.

use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::{InputEncoding, DEFAULT_ENCODINGS};
use crate::filter::CharacterFilter;
use crate::merger::merge_chunk_encodings;
use crate::outputter::output_merged_chunk;
use crate::record::{read_record, MatchRecord};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};

/// A minimal, otherwise-default `Config` for tests, with only `min_cch`
/// exposed as a parameter since it's the field most tests need to vary.
/// `jobs: 1` and `chunk_size: 8` keep test behavior single-threaded and
/// deterministic; individual tests override `chunk_size` directly (see
/// `scan_all_chunks`) when they need to control chunking explicitly.
pub(crate) fn test_config(min_cch: u64) -> Config {
    Config::new(
        DEFAULT_ENCODINGS.to_vec(),
        [CharacterFilter::Ascii].to_vec(),
        min_cch,
        1,
        8,
        false,
        None,
        false,
    )
}

pub(crate) fn test_config2(min_cch: u64, chunk_size: u64) -> Config {
    Config::new(
        DEFAULT_ENCODINGS.to_vec(),
        [CharacterFilter::Ascii].to_vec(),
        min_cch,
        1,
        chunk_size,
        false,
        None,
        false,
    )
}

/// Like `test_config2`, but with the character filter set supplied by the
/// caller instead of hardcoded to `[Ascii]`.
///
/// `test_config`/`test_config2` bake in `[CharacterFilter::Ascii]`, which
/// is right for the ASCII-oriented scanners but makes it impossible to
/// exercise any other filter -- and that blind spot is exactly what let a
/// `Latin1` bug survive in `scanner::utf16le_ascii` (bytes 0xA0-0xFF are
/// admitted by `latin1` but were being pushed into a `String` as raw
/// single bytes, which is not valid UTF-8). Tests that care about *which*
/// characters are matched -- the whole `scanner::utf16le` suite, and the
/// Latin-1 regression tests -- go through this instead.
pub(crate) fn test_config_with_filters(
    min_cch: u64,
    chunk_size: u64,
    filters: Vec<CharacterFilter>,
) -> Config {
    Config::new(
        DEFAULT_ENCODINGS.to_vec(),
        filters,
        min_cch,
        1,
        chunk_size,
        false,
        None,
        false,
    )
}

/// Returns a path inside a fresh, uniquely-named temp directory, plus a
/// guard that removes the directory (and everything under it) on drop.
///
/// IMPORTANT: bind the guard to a *named* variable (e.g. `_foo_guard`),
/// never to `_`. `let (p, _) = temp_path(...)` drops the guard
/// immediately (end of that statement), deleting the directory before
/// `p` is ever used.
pub(crate) fn temp_path(name: &str) -> (PathBuf, tempfile::TempDir) {
    // A monotonically increasing counter (on top of the process ID and the
    // caller-supplied `name`) guarantees every call gets a distinct temp
    // directory even when many tests run in parallel and pick overlapping
    // names, or when the same test calls this helper multiple times with
    // the same name (as several tests above do, once per loop iteration).
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = tempfile::Builder::new()
        .prefix(&format!("pstrings-test-{}-{}-{}", std::process::id(), id, name))
        .tempdir()
        .unwrap();
    let path = dir.path().join("data");
    (path, dir)
}

/// Opens a fresh read+write file at `path`. Only used for tests that
/// hand-build a MatchRecord stream and pass the *same* File directly
/// into merge_test_encoding_chunks / read_record / output_merged_chunk
/// (as opposed to passing a path that gets reopened via File::open,
/// which only needs read access).
pub(crate) fn rw_temp_file(path: &Path) -> File {
    // A monotonically increasing counter (on top of the process ID and the
    // caller-supplied `name`) guarantees every call gets a distinct temp
    // directory even when many tests run in parallel and pick overlapping
    // names, or when the same test calls this helper multiple times with
    // the same name (as several tests above do, once per loop iteration).
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .unwrap()
}

/// Encodes a `&str` as raw UTF-16LE bytes, for building synthetic scanner
/// input in tests that need UTF-16LE data on disk.
pub(crate) fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

/// Encodes a `&str` as raw CP932 (Shift_JIS) bytes, for building synthetic
/// scanner input in tests that need CP932 data on disk. Panics if `s`
/// contains a character CP932 can't represent -- test inputs should stick
/// to characters known to round-trip (ASCII, half-width katakana, and
/// common JIS X 0208 kanji/kana/punctuation).
pub(crate) fn cp932(s: &str) -> Vec<u8> {
    let (encoded, _, had_errors) = encoding_rs::SHIFT_JIS.encode(s);
    assert!(!had_errors, "test string {s:?} is not representable in CP932");
    encoded.into_owned()
}

/// Encodes a `&str` as raw GBK bytes. Panics if `s` contains a character
/// GBK can't represent.
pub(crate) fn gbk(s: &str) -> Vec<u8> {
    let (encoded, _, had_errors) = encoding_rs::GBK.encode(s);
    assert!(!had_errors, "test string {s:?} is not representable in GBK");
    encoded.into_owned()
}

/// Encodes a `&str` as raw EUC-KR bytes. Panics if `s` contains a
/// character EUC-KR can't represent.
pub(crate) fn euckr(s: &str) -> Vec<u8> {
    let (encoded, _, had_errors) = encoding_rs::EUC_KR.encode(s);
    assert!(!had_errors, "test string {s:?} is not representable in EUC-KR");
    encoded.into_owned()
}

/// Encodes a `&str` as raw Big5 bytes. Panics if `s` contains a character
/// Big5 can't represent.
pub(crate) fn big5(s: &str) -> Vec<u8> {
    let (encoded, _, had_errors) = encoding_rs::BIG5.encode(s);
    assert!(!had_errors, "test string {s:?} is not representable in Big5");
    encoded.into_owned()
}

/// Encodes a `&str` as raw GB18030 bytes.
///
/// Unlike its siblings this has no `had_errors` assertion, because
/// GB18030 covers the whole of Unicode -- there is no string it cannot
/// represent, so a failure would be unreachable.
pub(crate) fn gb18030(s: &str) -> Vec<u8> {
    encoding_rs::GB18030.encode(s).0.into_owned()
}

/// Drains every record from `f` (from its current position through EOF)
/// into a `Vec`, for tests that want to inspect a scanner/merger's raw
/// intermediate-format output directly rather than going through the
/// merge/output text-formatting stage.
pub(crate) fn read_records(f: &mut File) -> Vec<MatchRecord> {
    let mut out = Vec::new();
    while let Some(r) = read_record(f).unwrap() {
        out.push(r);
    }
    out
}

/// Runs a sequence of already-encoding-merged chunk files (i.e. each
/// `File` here represents one chunk's worth of records, already combined
/// across encodings the way `merge_chunk_encodings` would produce) through
/// `output_merged_chunk`, then flushes anything left pending, and returns
/// the resulting text output as a `String`.
///
/// This is the workhorse most `outputter`/`scanner` tests build on: it
/// simulates *only* the output/boundary-joining stage of the pipeline,
/// taking pre-merged chunk files as a given rather than actually running
/// `merger::merge_chunk_encodings` itself (contrast with
/// `merge_test_single_chunk`/`merge_test_full` below, which do exercise
/// the real merge step).
pub(crate) fn merge_test_encoding_chunks(files: Vec<File>, min_cch: u64) -> String {
    let mut output = Vec::new();
    let mut pending: HashMap<InputEncoding, MatchRecord> = HashMap::new();
    let cancel = AtomicBool::new(false);
    for (index, mut file) in files.into_iter().enumerate() {
        // `output_merged_chunk` needs to know where the *current* chunk
        // starts (to decide whether a record's `starts_at_chunk` flag
        // genuinely marks the start of this chunk). Rather than requiring
        // every test to pass that offset in explicitly, it's inferred here
        // from the file's own first record (a record's `offset` combined
        // with `starts_at_chunk: true` pins down the chunk's start), or
        // falls back to `index * 8` -- matching `test_config`'s default
        // `chunk_size: 8` -- for a chunk file with no records at all
        // (which has no record to infer an offset from).
        //
        // # Why callers that know the real offsets must not use this
        //
        // The inference is only sound for hand-built fixtures, where the
        // test author picked the offsets and knows the first record starts
        // at the chunk boundary. It is *wrong* for real scanner output
        // from a variable-width encoding, because a chunk's first record
        // can legitimately begin a few bytes after the boundary (the
        // previous chunk peeked past its own end to finish a straddling
        // character). Inferring the offset from that record silently
        // redefines the chunk as starting there -- which is exactly the
        // condition a chunk-boundary bug in `output_merged_chunk` depends
        // on, so the helper would paper over the very defect under test.
        //
        // Every helper that drives a real scan therefore computes the
        // true offsets itself and calls
        // `merge_test_encoding_chunks_at` instead.
        let chunk_offset = read_record(&mut file)
            .ok()
            .flatten()
            .map(|r| r.offset)
            .unwrap_or(index as u64 * 8);
        file.seek(io::SeekFrom::Start(0)).unwrap();
        // `u64::MAX` disables the "this chunk was entirely consumed by the
        // previous chunk's boundary peek" special case, which needs a real
        // chunk length to evaluate. Hand-built fixtures don't model that
        // situation anyway, and the inferred `chunk_offset` above already
        // means this path isn't reproducing a real chunk layout.
        output_merged_chunk(file, chunk_offset, u64::MAX, &mut pending, min_cch, &mut output, false, &cancel).unwrap();
    }
    // Delegates to `outputter::flush_pending` rather than re-implementing
    // "check cch, write" here: that logic now has to call
    // `scanner::segment_raw` to resolve any still-pending `RecordData::Raw`
    // fragment (e.g. from `scanner::cp932`) before it can even know a
    // real `cch`, so it belongs in one place, not duplicated per caller.
    crate::outputter::flush_pending(&mut pending, min_cch, &mut output, false).unwrap();
    String::from_utf8(output).unwrap()
}

/// Like `merge_test_encoding_chunks`, but with each chunk's true starting
/// offset supplied by the caller rather than inferred from the chunk's
/// first record.
///
/// This is what every helper that drives a *real* scanner uses, since
/// those helpers compute the chunk layout themselves and therefore know
/// the exact offsets `main.rs` would pass in production. Using the real
/// values keeps the tests honest about cross-chunk boundary joining --
/// see the long comment in `merge_test_encoding_chunks` for why the
/// inferred version would hide boundary bugs instead of catching them.
pub(crate) fn merge_test_encoding_chunks_at(chunks: Vec<(u64, u64, File)>, min_cch: u64) -> String {
    let mut output = Vec::new();
    let mut pending: HashMap<InputEncoding, MatchRecord> = HashMap::new();
    let cancel = AtomicBool::new(false);
    for (chunk_offset, chunk_len, file) in chunks {
        output_merged_chunk(file, chunk_offset, chunk_len, &mut pending, min_cch, &mut output, false, &cancel).unwrap();
    }
    crate::outputter::flush_pending(&mut pending, min_cch, &mut output, false).unwrap();
    String::from_utf8(output).unwrap()
}


/// Simulates the *entire* pipeline for a single chunk: takes one file per
/// encoding (as if each were that encoding's raw scanner output for one
/// chunk), runs the real `merge_chunk_encodings` to combine them into one
/// offset-sorted stream (exercising the actual merger, unlike
/// `merge_test_encoding_chunks` above), then runs that single merged
/// result through `output_merged_chunk` at offset 0 (since by construction
/// there's only the one chunk).
///
/// Used by `merger_tests.rs`, where the whole point is to exercise the
/// real k-way merge across multiple per-encoding files.
pub(crate) fn merge_test_single_chunk(paths: &[PathBuf], min_cch: u64) -> String {
    let (merged_path, _merged_guard) = temp_path("merged-chunk-test");
    let cancel = AtomicBool::new(false);
    let cfg = test_config(min_cch);
    let inputs: Vec<File> = paths.iter().map(|p| File::open(p).unwrap()).collect();
    let merged_file = merge_chunk_encodings(inputs, &merged_path, &cancel, &cfg).unwrap();

    let mut output = Vec::new();
    let mut pending: HashMap<InputEncoding, MatchRecord> = HashMap::new();
    output_merged_chunk(merged_file, 0, u64::MAX, &mut pending, min_cch, &mut output, false, &cancel).unwrap();
    crate::outputter::flush_pending(&mut pending, min_cch, &mut output, false).unwrap();
    String::from_utf8(output).unwrap()
}

/// The most complete simulator: takes several encodings, each with its own
/// list of per-chunk files (`encoding_chunks[i] = (encoding, chunk_files)`),
/// re-groups them by chunk *index* rather than by encoding (so
/// `chunks[i]` ends up holding "every encoding's file for chunk i"),
/// merges each chunk's encodings together via the real
/// `merge_chunk_encodings` (like `merge_test_single_chunk`, but repeated
/// once per chunk instead of just once), and finally feeds the resulting
/// per-chunk merged files through `output_merged_chunk` in chunk order
/// (like `merge_test_encoding_chunks`, but now genuinely built on real
/// merge output for every chunk rather than pre-merged fixtures).
///
/// In short: this exercises both the merger and the outputter together,
/// across multiple chunks and multiple encodings at once -- the closest
/// thing to the real end-to-end pipeline that doesn't also involve a real
/// scanner run. Used by the `outputter_tests.rs` tests that need to prove
/// out cross-encoding, multi-chunk ordering.
pub(crate) fn merge_test_full(encoding_chunks: &[(InputEncoding, Vec<PathBuf>)], min_cch: u64) -> String {
    let chunk_count = encoding_chunks.iter().map(|(_, paths)| paths.len()).max().unwrap_or(0);
    let mut chunks = vec![Vec::<PathBuf>::new(); chunk_count];
    for (_, paths) in encoding_chunks {
        for (index, path) in paths.iter().enumerate() {
            chunks[index].push(path.clone());
        }
    }

    let cancel = AtomicBool::new(false);
    let cfg = test_config(min_cch);
    let mut merged_files: Vec<File> = Vec::with_capacity(chunk_count);
    // Keep every naming-hint guard alive until all merges are done:
    // create_temp_file needs the parent directory to still exist.
    let mut merged_guards = Vec::with_capacity(chunk_count);
    for paths in &chunks {
        let (merged_path, merged_guard) = temp_path("merged-full");
        let inputs: Vec<File> = paths.iter().map(|p| File::open(p).unwrap()).collect();
        let merged_file = merge_chunk_encodings(inputs, &merged_path, &cancel, &cfg).unwrap();
        merged_files.push(merged_file);
        merged_guards.push(merged_guard);
    }

    let mut output = Vec::new();
    let mut pending: HashMap<InputEncoding, MatchRecord> = HashMap::new();
    for (index, mut file) in merged_files.into_iter().enumerate() {
        // Same offset-inference trick as `merge_test_encoding_chunks`
        // (see the comment there): read the first record's offset if one
        // exists, otherwise fall back to the fixed `chunk_size: 8` default
        // from `test_config` for an empty chunk.
        let chunk_offset = read_record(&mut file)
            .ok()
            .flatten()
            .map(|r| r.offset)
            .unwrap_or(index as u64 * 8);
        file.seek(io::SeekFrom::Start(0)).unwrap();
        output_merged_chunk(file, chunk_offset, u64::MAX, &mut pending, min_cch, &mut output, false, &cancel).unwrap();
    }
    crate::outputter::flush_pending(&mut pending, min_cch, &mut output, false).unwrap();

    String::from_utf8(output).unwrap()
}

/// Runs a *real* scan (either `scanner::ascii` or `scanner::utf16le_ascii`,
/// chosen by `utf16`) over `input`, split into genuine, correctly-sized
/// `Chunk`s of `chunk_size` bytes each (including a possibly-shorter final
/// chunk when the file length isn't an exact multiple of `chunk_size`),
/// then feeds every chunk's scan output through `merge_test_encoding_chunks`
/// to produce final text output.
///
/// Unlike every other helper in this file, this one doesn't take
/// pre-built fixture files -- it drives the actual scanner across actual
/// chunk boundaries computed from `chunk_size`, so it's the right choice
/// whenever a test's whole point is to check what happens when a
/// particular byte pattern gets split at a particular chunk size (as
/// opposed to testing the merge/join logic in isolation with hand-picked
/// records).
pub(crate) fn scan_all_chunks(input: &Path, chunk_size: u64, utf16: bool, min_cch: u64, name: &str) -> String {
    let file = File::open(input).unwrap();
    let file_len = fs::metadata(input).unwrap().len();
    let cfg = test_config2(min_cch, chunk_size);
    let cancel = AtomicBool::new(false);
    let mut outputs: Vec<(u64, u64, File)> = Vec::new();
    let mut out_guards = Vec::new();
    // Number of chunks needed to cover the whole file (ceiling division),
    // with the special case of an empty file needing zero chunks rather
    // than one degenerate zero-length chunk.
    let chunk_count = if file_len == 0 { 0 } else { (file_len + chunk_size - 1) / chunk_size };

    for index in 0..chunk_count {
        let offset = index * chunk_size;
        // The final chunk may be shorter than `chunk_size` if `file_len`
        // isn't an exact multiple of it.
        let len = (file_len - offset).min(chunk_size);
        let (out, out_guard) = temp_path(&format!("{name}-out-{index}"));
        let chunk = Chunk { offset, len };
        let result_file = if utf16 {
            crate::scanner::utf16le_ascii::scan(&file, file_len, &chunk, &cfg, &out, &cancel).unwrap().1
        } else {
            crate::scanner::ascii::scan(&file, &chunk, &cfg, &out, &cancel).unwrap().1
        };
        outputs.push((offset, len, result_file));
        out_guards.push(out_guard);
    }

    merge_test_encoding_chunks_at(outputs, min_cch)
}

/// Like `scan_all_chunks` with `utf16: false`, but with the filter set
/// supplied by the caller instead of hardcoded to `[Ascii]`.
///
/// Needed because `scanner::ascii`'s behavior across chunk boundaries is
/// only interesting for non-ASCII filters once `Latin1` exists: a
/// Latin-1 byte is one source byte but *two* UTF-8 bytes on output, so a
/// run split mid-boundary exercises a size relationship the ASCII-only
/// tests never produce.
pub(crate) fn scan_all_chunks_ascii_with_filters(
    input: &Path,
    chunk_size: u64,
    filters: Vec<CharacterFilter>,
    min_cch: u64,
    name: &str,
) -> String {
    let file = File::open(input).unwrap();
    let file_len = fs::metadata(input).unwrap().len();
    let cfg = test_config_with_filters(min_cch, chunk_size, filters);
    let cancel = AtomicBool::new(false);
    let mut outputs: Vec<(u64, u64, File)> = Vec::new();
    let mut out_guards = Vec::new();
    let chunk_count = if file_len == 0 { 0 } else { (file_len + chunk_size - 1) / chunk_size };

    for index in 0..chunk_count {
        let offset = index * chunk_size;
        let len = (file_len - offset).min(chunk_size);
        let (out, out_guard) = temp_path(&format!("{name}-out-{index}"));
        let chunk = Chunk { offset, len };
        let result_file = crate::scanner::ascii::scan(&file, &chunk, &cfg, &out, &cancel).unwrap().1;
        outputs.push((offset, len, result_file));
        out_guards.push(out_guard);
    }

    merge_test_encoding_chunks_at(outputs, min_cch)
}

/// Full-UTF-16LE counterpart to `scan_all_chunks`, driving the real
/// `scanner::utf16le::scan` (not the ASCII-restricted
/// `scanner::utf16le_ascii`) across genuine chunk boundaries.
///
/// Unlike the other `scan_all_chunks*` helpers this one takes the filter
/// set explicitly, because that's the whole point of the full scanner:
/// which characters it matches is entirely determined by the selected
/// filters, so a test that hardcoded `[Ascii]` (as `test_config2` does)
/// could never reach the BMP or astral code paths at all.
///
/// Note that `chunk_size` is *not* forced even here, even though
/// `main.rs` rejects odd chunk sizes when UTF-16LE is enabled: the
/// scanner itself handles both byte parities regardless (that's what
/// `scan_parity` is for), and tests deliberately pick small sizes like 8
/// to force splits at specific offsets.
pub(crate) fn scan_all_chunks_full_utf16le(
    input: &Path,
    chunk_size: u64,
    filters: Vec<CharacterFilter>,
    min_cch: u64,
    name: &str,
) -> String {
    let file = File::open(input).unwrap();
    let file_len = fs::metadata(input).unwrap().len();
    let cfg = test_config_with_filters(min_cch, chunk_size, filters);
    let cancel = AtomicBool::new(false);
    let mut outputs: Vec<(u64, u64, File)> = Vec::new();
    let mut out_guards = Vec::new();
    let chunk_count = if file_len == 0 { 0 } else { (file_len + chunk_size - 1) / chunk_size };

    for index in 0..chunk_count {
        let offset = index * chunk_size;
        let len = (file_len - offset).min(chunk_size);
        let (out, out_guard) = temp_path(&format!("{name}-out-{index}"));
        let chunk = Chunk { offset, len };
        let result_file = crate::scanner::utf16le::scan(&file, file_len, &chunk, &cfg, &out, &cancel)
            .unwrap()
            .1;
        outputs.push((offset, len, result_file));
        out_guards.push(out_guard);
    }

    merge_test_encoding_chunks_at(outputs, min_cch)
}

/// UTF-8 counterpart to `scan_all_chunks`, kept as a separate function
/// rather than folding a third encoding into that one's `utf16: bool`
/// parameter (which would force every existing call site to be updated
/// for a `bool`-based API that no longer reads clearly with three
/// options). Otherwise identical in structure: splits `input` into real
/// `Chunk`s of `chunk_size` bytes, runs the real `scanner::utf8::scan`
/// over each one, and merges the results through
/// `merge_test_encoding_chunks`.
pub(crate) fn scan_all_chunks_utf8(input: &Path, chunk_size: u64, min_cch: u64, name: &str) -> String {
    let file = File::open(input).unwrap();
    let file_len = fs::metadata(input).unwrap().len();
    let cfg = test_config2(min_cch, chunk_size);
    let cancel = AtomicBool::new(false);
    let mut outputs: Vec<(u64, u64, File)> = Vec::new();
    let mut out_guards = Vec::new();
    let chunk_count = if file_len == 0 { 0 } else { (file_len + chunk_size - 1) / chunk_size };

    for index in 0..chunk_count {
        let offset = index * chunk_size;
        let len = (file_len - offset).min(chunk_size);
        let (out, out_guard) = temp_path(&format!("{name}-out-{index}"));
        let chunk = Chunk { offset, len };
        let result_file = crate::scanner::utf8::scan(&file, file_len, &chunk, &cfg, &out, &cancel).unwrap().1;
        outputs.push((offset, len, result_file));
        out_guards.push(out_guard);
    }

    merge_test_encoding_chunks_at(outputs, min_cch)
}

/// CP932 counterpart to `scan_all_chunks`/`scan_all_chunks_utf8`. Boundary
/// fragments *are* joined across chunks here (see scanner/dbcs.rs's
/// module doc comment for how) via the same `merge_test_encoding_chunks`
/// path the other encodings use -- `output_merged_chunk` resolves
/// `RecordData::Raw` fragments generically through `scanner::segment_raw`.
pub(crate) fn scan_all_chunks_cp932(input: &Path, chunk_size: u64, min_cch: u64, name: &str) -> String {
    scan_all_chunks_dbcs(input, chunk_size, min_cch, name, crate::scanner::cp932::scan)
}

/// GBK counterpart to `scan_all_chunks_cp932`. Same deferred-boundary
/// path; only the encoding differs (see scanner/dbcs.rs).
pub(crate) fn scan_all_chunks_gbk(input: &Path, chunk_size: u64, min_cch: u64, name: &str) -> String {
    scan_all_chunks_dbcs(input, chunk_size, min_cch, name, crate::scanner::gbk::scan)
}

/// EUC-KR counterpart to `scan_all_chunks_cp932`.
pub(crate) fn scan_all_chunks_euckr(input: &Path, chunk_size: u64, min_cch: u64, name: &str) -> String {
    scan_all_chunks_dbcs(input, chunk_size, min_cch, name, crate::scanner::euckr::scan)
}

/// Big5 counterpart to `scan_all_chunks_cp932`.
pub(crate) fn scan_all_chunks_big5(input: &Path, chunk_size: u64, min_cch: u64, name: &str) -> String {
    scan_all_chunks_dbcs(input, chunk_size, min_cch, name, crate::scanner::big5::scan)
}

/// GB18030 counterpart to `scan_all_chunks_cp932`.
pub(crate) fn scan_all_chunks_gb18030(input: &Path, chunk_size: u64, min_cch: u64, name: &str) -> String {
    scan_all_chunks_dbcs(input, chunk_size, min_cch, name, crate::scanner::gb18030::scan)
}

/// Shared driver for the `scanner::dbcs` scanners: splits `input` into
/// real `Chunk`s, runs `scan` over each, and merges the results the same
/// way the production pipeline does. Written once rather than per encoding
/// so every encoding on that engine is provably exercised through an
/// identical path -- a difference in results is then a difference in the
/// encoding, not in the harness.
fn scan_all_chunks_dbcs(
    input: &Path,
    chunk_size: u64,
    min_cch: u64,
    name: &str,
    scan: fn(&File, u64, &Chunk, &Config, &Path, &AtomicBool) -> std::io::Result<(u64, File)>,
) -> String {
    let file = File::open(input).unwrap();
    let file_len = fs::metadata(input).unwrap().len();
    let cfg = test_config2(min_cch, chunk_size);
    let cancel = AtomicBool::new(false);
    let mut outputs: Vec<(u64, u64, File)> = Vec::new();
    let mut out_guards = Vec::new();
    let chunk_count = if file_len == 0 { 0 } else { (file_len + chunk_size - 1) / chunk_size };

    for index in 0..chunk_count {
        let offset = index * chunk_size;
        let len = (file_len - offset).min(chunk_size);
        let (out, out_guard) = temp_path(&format!("{name}-out-{index}"));
        let chunk = Chunk { offset, len };
        let result_file = scan(&file, file_len, &chunk, &cfg, &out, &cancel).unwrap().1;
        outputs.push((offset, len, result_file));
        out_guards.push(out_guard);
    }

    merge_test_encoding_chunks_at(outputs, min_cch)
}

/// Runs the real ISO-2022-JP scanner over `input`, split into genuine
/// `Chunk`s of `chunk_size` bytes each, then feeds every chunk's scan output
/// through `merge_test_encoding_chunks` to produce final text output.
///
/// This exercises the actual ISO-2022-JP scanner across real chunk
/// boundaries, including boundaries inside escape sequences and JIS X 0208
/// characters.
pub(crate) fn scan_all_chunks_iso2022jp(
    input: &Path,
    chunk_size: u64,
    min_cch: u64,
    name: &str,
) -> String {
    let file = File::open(input).unwrap();
    let file_len = fs::metadata(input).unwrap().len();

    let cfg = test_config2(min_cch, chunk_size);

    let cancel = AtomicBool::new(false);
    let mut outputs: Vec<(u64, u64, File)> = Vec::new();
    let mut out_guards = Vec::new();

    let chunk_count = if file_len == 0 {
        0
    } else {
        (file_len + chunk_size - 1) / chunk_size
    };

    for index in 0..chunk_count {
        let offset = index * chunk_size;
        let len = (file_len - offset).min(chunk_size);

        let (out, out_guard) =
            temp_path(&format!("{name}-out-{index}"));

        let chunk = Chunk { offset, len };

        let result_file =
            crate::scanner::iso2022jp::scan(
                &file,
                file_len,
                &chunk,
                &cfg,
                &out,
                &cancel,
            )
            .unwrap()
            .1;

        outputs.push((offset, len, result_file));
        out_guards.push(out_guard);
    }

    merge_test_encoding_chunks_at(outputs, min_cch)
}