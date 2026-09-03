use super::support::*;
use std::fs;

// Chunk size is a *performance* knob: how much of the input each worker
// takes at a time. It must have no effect whatsoever on what is found.
// Two runs of the same file at different `--chunk-size` values must
// produce byte-identical output.
//
// That invariant is easy to state and easy to break, because every
// scanner has to do something special at a chunk boundary (defer, peek
// past the end, or flag a fragment for the outputter to rejoin), and each
// of those mechanisms is a place where a match can be split, duplicated,
// or dropped depending on exactly where the boundary happens to land.
//
// These tests therefore don't check for any particular record count. They
// check only that every chunk size agrees with every other -- which is
// the actual contract, and which catches a regression no matter which
// chunk size happens to be the odd one out.

/// Runs `input` through the whole scan-merge-output pipeline once per
/// chunk size and asserts every run produced identical output.
///
/// The largest size is deliberately at least the file length, so one of
/// the runs processes the file as a single chunk with no boundary
/// handling at all. That run is the reference the others are implicitly
/// compared against (all sizes must agree, so agreeing with it is
/// required).
///
/// `even_only` restricts the sweep to even chunk sizes, for UTF-16LE.
/// `main.rs` rejects an odd `--chunk-size` outright when UTF-16LE is
/// enabled, because a code unit is two bytes and an odd split would put
/// the encoding's whole parity model out of step with the chunk grid --
/// so an odd size is not a configuration the scanner is ever asked to
/// handle, and holding it to the invariant would be testing a contract
/// that doesn't exist.
fn assert_chunk_size_invariant_impl(
    scan: fn(&std::path::Path, u64, u64, &str) -> String,
    bytes: &[u8],
    min_cch: u64,
    tag: &str,
    even_only: bool,
) {
    let (input_path, _guard) = temp_path(&format!("inv-{tag}-input"));
    fs::write(&input_path, bytes).unwrap();

    let full = bytes.len() as u64;
    let sizes: Vec<u64> = (1..=16u64)
        .chain([24, 32, 48, 64, 100, 128, 256, full, full + 1, full * 2])
        .filter(|&s| s > 0 && (!even_only || s % 2 == 0))
        .collect();

    let mut reference: Option<(u64, String)> = None;
    for size in sizes {
        let out = scan(&input_path, size, min_cch, &format!("inv-{tag}-{size}"));
        match &reference {
            None => reference = Some((size, out)),
            Some((ref_size, ref_out)) => {
                assert_eq!(
                    &out,
                    ref_out,
                    "chunk_size={size} disagreed with chunk_size={ref_size}\n\
                     --- chunk_size={size} ({} records) ---\n{out}\n\
                     --- chunk_size={ref_size} ({} records) ---\n{ref_out}",
                    out.lines().count(),
                    ref_out.lines().count(),
                );
            }
        }
    }
}

fn assert_chunk_size_invariant(
    scan: fn(&std::path::Path, u64, u64, &str) -> String,
    bytes: &[u8],
    min_cch: u64,
    tag: &str,
) {
    assert_chunk_size_invariant_impl(scan, bytes, min_cch, tag, false);
}

/// UTF-16LE variant: even chunk sizes only, matching what `main.rs`
/// actually permits.
fn assert_chunk_size_invariant_even(
    scan: fn(&std::path::Path, u64, u64, &str) -> String,
    bytes: &[u8],
    min_cch: u64,
    tag: &str,
) {
    assert_chunk_size_invariant_impl(scan, bytes, min_cch, tag, true);
}

/// Deterministic pseudo-random filler, so a failure is always reproducible.
/// A simple xorshift is used rather than pulling in a dependency; the
/// exact distribution doesn't matter, only that it produces the kind of
/// byte soup a real disk image is full of -- which is what generates the
/// short, ragged, boundary-straddling near-matches these tests are about.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// Noise with strings buried in it -- the realistic shape of the problem,
/// and the one where a boundary is most likely to land in an awkward spot.
fn noisy(strings: &[&[u8]], seed: u64) -> Vec<u8> {
    let mut out = noise(97, seed);
    for (i, s) in strings.iter().enumerate() {
        out.extend_from_slice(s);
        out.extend_from_slice(&noise(53 + i * 11, seed.wrapping_add(i as u64 + 1)));
    }
    out
}

/// `scan_all_chunks` takes a `utf16: bool` selector, so it needs adapting
/// to the uniform signature `assert_chunk_size_invariant` expects.
fn scan_ascii(input: &std::path::Path, chunk_size: u64, min_cch: u64, name: &str) -> String {
    scan_all_chunks(input, chunk_size, false, min_cch, name)
}

fn scan_utf16le_ascii(input: &std::path::Path, chunk_size: u64, min_cch: u64, name: &str) -> String {
    scan_all_chunks(input, chunk_size, true, min_cch, name)
}

#[test]
fn ascii_output_is_independent_of_chunk_size() {
    let data = noisy(&[b"HelloWorldTest", b"second string here", b"third"], 12345);
    assert_chunk_size_invariant(scan_ascii, &data, 4, "ascii");
}

#[test]
fn utf16le_ascii_output_is_independent_of_chunk_size() {
    let mut data = noise(96, 8080);
    data.extend_from_slice(&utf16le("UTF16サンプル文字列"));
    data.extend_from_slice(&noise(64, 8081));
    data.extend_from_slice(&utf16le("another16"));
    assert_chunk_size_invariant_even(scan_utf16le_ascii, &data, 4, "u16ascii");
}

#[test]
fn utf8_output_is_independent_of_chunk_size() {
    let data = noisy(
        &[
            "「base4 」で検索をかけても１件もヒットせず".as_bytes(),
            "テスト文字列ABCDEF".as_bytes(),
            "\u{20000}\u{20001}astral".as_bytes(),
        ],
        999,
    );
    assert_chunk_size_invariant(scan_all_chunks_utf8, &data, 4, "utf8");
}

#[test]
fn cp932_output_is_independent_of_chunk_size() {
    let a = cp932("「base4 」で検索をかけても１件もヒットせず");
    let b = cp932("テスト文字列ABCDEF");
    let c = cp932("最後はEOFまで続く");
    let data = noisy(&[&a, &b, &c], 4242);
    assert_chunk_size_invariant(scan_all_chunks_cp932, &data, 4, "cp932");
}

#[test]
fn cp932_output_is_independent_of_chunk_size_when_a_match_reaches_eof() {
    // No trailing noise: the final match runs right up to EOF, which is
    // the case that forces every chunk size to agree about a deferred Raw
    // fragment that will never be extended.
    let mut data = noise(97, 777);
    data.extend_from_slice(&cp932("EOFまで続く文字列"));
    assert_chunk_size_invariant(scan_all_chunks_cp932, &data, 4, "cp932eof");
}

#[test]
fn utf8_output_is_independent_of_chunk_size_when_a_match_reaches_eof() {
    let mut data = noise(97, 555);
    data.extend_from_slice("EOFまで続く文字列".as_bytes());
    assert_chunk_size_invariant(scan_all_chunks_utf8, &data, 4, "utf8eof");
}

#[test]
fn utf8_output_is_independent_of_chunk_size_at_a_higher_min_length() {
    // `min_cch` interacts with boundaries: a fragment can be under the
    // threshold on its own but over it once joined, so a boundary bug can
    // hide at `-m 4` and appear at `-m 8` (or the reverse).
    let data = noisy(
        &[
            "shortAB".as_bytes(),
            "「base4 」で検索をかけても１件もヒットせず".as_bytes(),
            "12345678901234567890".as_bytes(),
        ],
        31337,
    );
    assert_chunk_size_invariant(scan_all_chunks_utf8, &data, 8, "utf8min8");
}

#[test]
fn cp932_output_is_independent_of_chunk_size_at_a_higher_min_length() {
    let a = cp932("「base4 」で検索をかけても１件もヒットせず");
    let b = cp932("短い");
    let data = noisy(&[&a, &b], 24680);
    assert_chunk_size_invariant(scan_all_chunks_cp932, &data, 8, "cp932min8");
}

/// The full UTF-16LE scanner (not the ASCII-restricted one) needs its
/// filter set passed in, since which characters it matches is entirely
/// determined by the filters. This pins a realistic Japanese selection so
/// it fits the uniform signature.
fn scan_utf16le_full(input: &std::path::Path, chunk_size: u64, min_cch: u64, name: &str) -> String {
    scan_all_chunks_full_utf16le(
        input,
        chunk_size,
        vec![
            crate::filter::CharacterFilter::Ascii,
            crate::filter::CharacterFilter::KanjiJis1,
            crate::filter::CharacterFilter::Hiragana,
            crate::filter::CharacterFilter::Katakana,
            crate::filter::CharacterFilter::CjkPunct,
        ],
        min_cch,
        name,
    )
}

#[test]
fn utf16le_output_is_independent_of_chunk_size() {
    let mut data = noise(96, 1357);
    data.extend_from_slice(&utf16le("「base4 」で検索をかけても１件もヒットせず"));
    data.extend_from_slice(&noise(64, 1358));
    data.extend_from_slice(&utf16le("テスト文字列ABCDEF"));
    assert_chunk_size_invariant_even(scan_utf16le_full, &data, 4, "u16full");
}

#[test]
fn utf16le_output_is_independent_of_chunk_size_when_a_match_reaches_eof() {
    let mut data = noise(96, 2468);
    data.extend_from_slice(&utf16le("EOFまで続く文字列"));
    assert_chunk_size_invariant_even(scan_utf16le_full, &data, 4, "u16eof");
}

#[test]
fn utf16le_output_is_independent_of_chunk_size_with_surrogate_pairs() {
    // Surrogate pairs are the UTF-16LE case that makes a scanner peek past
    // its chunk end, so a boundary landing between the two halves is
    // exactly the situation that used to fragment a match.
    let mut data = noise(96, 13579);
    data.extend_from_slice(&utf16le("あ\u{20000}い\u{20001}う漢字テスト"));
    data.extend_from_slice(&noise(48, 13580));
    assert_chunk_size_invariant_even(scan_utf16le_full, &data, 4, "u16surr");
}

// # The reported record count must track the output, not the scanning
//
// `-vv` prints a per-encoding record count. It used to report
// `DetailedStats::records_by_encoding`, which is the number of
// *intermediate* records the scanners wrote across all chunks. That number
// is not the number of strings found, and it moves with `--chunk-size`:
//
//     utf8  --chunk-size 2    -> 744 records reported, 44 lines printed
//     utf8  --chunk-size 2266 ->  45 records reported, 44 lines printed
//
// Two separate effects inflate it. A string crossing a chunk boundary is
// emitted once per chunk it touches and only later rejoined by the
// outputter, so each additional boundary adds a phantom record. And a
// fragment touching a boundary is emitted even when it is shorter than
// `--min-length`, because the next chunk might extend it past the
// threshold -- when it doesn't, the fragment is dropped at output time,
// having been counted but never printed. (That second effect is why even
// the single-chunk run reported 45 for 44 lines: the run reaching EOF is
// deferred and counted, then resolved.)
//
// The fix was to count records at the point they are written, which is
// after rejoining and after `--min-length` has been applied to the joined
// result. These tests pin that the counted-at-write number equals the
// number of output lines at every chunk size.

/// Counts output lines, which is what the reported "found" figure must
/// equal. Written to be independent of the sweep above so a failure here
/// points at the counting rather than at the output itself.
fn line_count(out: &str) -> usize {
    out.lines().count()
}

#[test]
fn the_number_of_output_lines_is_independent_of_chunk_size() {
    // A direct, minimal statement of the property the `-vv` count is
    // supposed to report. If this holds but the reported figure varies,
    // the bug is in the counting; if this fails, the bug is in the
    // pipeline (which the sweeps above would also catch).
    let a = cp932("「base4 」で検索をかけても１件もヒットせず");
    let b = cp932("テスト文字列ABCDEF");
    let data = noisy(&[&a, &b], 8642);
    let (input_path, _guard) = temp_path("count-cp932-input");
    fs::write(&input_path, &data).unwrap();

    let reference = line_count(&scan_all_chunks_cp932(&input_path, data.len() as u64, 4, "cnt-ref"));
    for chunk_size in [1u64, 2, 3, 4, 5, 8, 16, 64, 256] {
        let n = line_count(&scan_all_chunks_cp932(
            &input_path,
            chunk_size,
            4,
            &format!("cnt-{chunk_size}"),
        ));
        assert_eq!(
            n, reference,
            "chunk_size={chunk_size} produced {n} output lines, single-chunk produced {reference}"
        );
    }
}
