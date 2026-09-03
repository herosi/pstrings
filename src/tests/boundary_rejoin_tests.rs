use super::support::*;
use std::fs;

// Regression tests for chunk-boundary rejoining across *every* chunk size,
// not just the one or two sizes each scanner's own test file happens to
// use.
//
// # The bug these exist to catch
//
// `outputter::output_merged_chunk` collects the records that might
// continue a `pending` fragment from the previous chunk. It used to decide
// membership with `rec.offset == chunk_offset` -- "a boundary record is one
// starting exactly at the chunk's first byte."
//
// That is only true for fixed-width encodings. Variable-width scanners
// deliberately read *past* their chunk's end to finish a character that
// straddles the boundary: `scanner::utf8` peeks up to
// `MAX_UTF8_CHAR_LEN - 1` bytes, and `scanner::utf16le` does the same for
// surrogate pairs. Those bytes are consumed by the earlier chunk, so the
// next chunk's first record legitimately starts 1-3 bytes *after*
// `chunk_offset`. The equality test excluded exactly those records from
// boundary processing, so instead of being joined they were emitted as
// separate fragments -- and the characters spanning the boundary were
// dropped entirely.
//
// Concretely, at `--chunk-size 8` the 60-byte string below came out as
// three disjoint pieces with characters missing between them:
//
//     「base4 」で検
//     索をかけて
//     もヒットせ
//
// The fix is to trust the scanner's own `starts_at_chunk` flag, which is
// computed with knowledge of how far that scanner actually read.
//
// Sweeping *all* chunk sizes matters because the failure depends on
// whether a multi-byte character happens to straddle a boundary, which
// varies with alignment: at `--chunk-size 6` the same input round-tripped
// perfectly while 5, 7 and 8 all failed differently.

/// Japanese text mixing 1-, 2- and 3-byte UTF-8 characters, so that some
/// character straddles a boundary at nearly every chunk size.
const MIXED: &str = "「base4 」で検索をかけても１件もヒットせず";

fn sweep_utf8(text: &str, tag: &str) {
    let bytes = text.as_bytes();
    let (input_path, _guard) = temp_path(&format!("boundary-{tag}-input"));
    fs::write(&input_path, bytes).unwrap();

    // From 1 byte (every character straddles) up to past the whole file
    // (nothing straddles), so both extremes and everything between are
    // covered.
    for chunk_size in 1..=(bytes.len() as u64 + 2) {
        let out = scan_all_chunks_utf8(&input_path, chunk_size, 1, &format!("b-{tag}-{chunk_size}"));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "chunk_size={chunk_size} split the string into {} records instead of rejoining it:\n{out}",
            lines.len()
        );
        assert!(
            lines[0].ends_with(text),
            "chunk_size={chunk_size} lost characters; got:\n{out}"
        );
        assert!(
            lines[0].starts_with("00000000000000000000\t"),
            "chunk_size={chunk_size} reported the wrong offset:\n{out}"
        );
    }
}

#[test]
fn utf8_rejoins_across_every_chunk_size() {
    sweep_utf8(MIXED, "mixed");
}

#[test]
fn utf8_rejoins_pure_multibyte_across_every_chunk_size() {
    // No ASCII at all, so a character straddles a boundary at every chunk
    // size that isn't a multiple of 3.
    sweep_utf8("検索結果一件表示中断続行", "kanji");
}

#[test]
fn utf8_rejoins_four_byte_characters_across_every_chunk_size() {
    // 4-byte characters exercise the longest possible peek past the
    // boundary, and are the case most likely to be mis-attributed.
    sweep_utf8("\u{20000}\u{20001}\u{20002}\u{20003}\u{20004}", "astral");
}

#[test]
fn utf8_rejoins_mixed_width_runs_across_every_chunk_size() {
    // Alternating widths mean the byte offset of each character start is
    // irregular, so `starts_at_chunk` cannot coincidentally agree with an
    // offset comparison.
    sweep_utf8("aあb漢c\u{20000}dえe", "alternating");
}

#[test]
fn utf16le_rejoins_across_every_chunk_size() {
    // The same property for UTF-16LE, which peeks past the boundary to
    // complete a surrogate pair. Chunk sizes must be even (the CLI
    // enforces this), so step by 2.
    let text = MIXED;
    let bytes = utf16le(text);
    let (input_path, _guard) = temp_path("boundary-u16-input");
    fs::write(&input_path, &bytes).unwrap();

    for chunk_size in (2..=(bytes.len() as u64 + 2)).step_by(2) {
        let out = scan_all_chunks_full_utf16le(
            &input_path,
            chunk_size,
            vec![
                crate::filter::CharacterFilter::Ascii,
                crate::filter::CharacterFilter::KanjiJis1,
                crate::filter::CharacterFilter::Hiragana,
                crate::filter::CharacterFilter::Katakana,
                crate::filter::CharacterFilter::CjkPunct,
            ],
            1,
            &format!("b-u16-{chunk_size}"),
        );
        assert!(
            out.lines().any(|l| l.ends_with(text)),
            "chunk_size={chunk_size} failed to rejoin the string:\n{out}"
        );
    }
}

#[test]
fn utf8_multiple_independent_strings_stay_separate() {
    // The complement of the fix: making boundary matching more permissive
    // must not start gluing together strings that are genuinely distinct.
    // Two runs separated by a NUL must remain two records at every chunk
    // size.
    let mut bytes = "検索結果".as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice("一件表示".as_bytes());
    let (input_path, _guard) = temp_path("boundary-sep-input");
    fs::write(&input_path, &bytes).unwrap();

    for chunk_size in 1..=(bytes.len() as u64 + 2) {
        let out = scan_all_chunks_utf8(&input_path, chunk_size, 1, &format!("b-sep-{chunk_size}"));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "chunk_size={chunk_size} should keep the two NUL-separated runs distinct:\n{out}"
        );
        assert!(lines[0].ends_with("検索結果"), "chunk_size={chunk_size}:\n{out}");
        assert!(lines[1].ends_with("一件表示"), "chunk_size={chunk_size}:\n{out}");
    }
}

// # The second bug: matches running to EOF were dropped for CP932
//
// `scanner::cp932` cannot resolve a run that touches its chunk's end,
// because a CP932 trail byte is indistinguishable from a fresh ASCII or
// lead byte (see that module's doc comment). It therefore defers such a
// run as an undecoded `RecordData::Raw` record, whose `cch` is a
// placeholder `0` until `scanner::segment_raw` decodes it.
//
// The *last* chunk's trailing run touches the end of the file, so it too
// is deferred and lands in `pending`. Resolving it is the whole job of
// `outputter::flush_pending`, which decodes first and only then applies
// `min_cch`.
//
// `main.rs` never called it. It had its own inline drain loop that
// compared each leftover record's `cch` against `min_cch` directly --
// against the placeholder `0`, which never reaches any `min_cch >= 1`. So
// every CP932 match that ran to EOF was discarded outright, and
// `pstrings -e cp932` printed *nothing at all* for a file that ended with
// a match.
//
// The bug was invisible to the test suite because every helper in
// `support.rs` already went through `flush_pending`, exercising the
// correct path while the real binary used the broken one. These tests
// therefore assert on the property directly: a match that reaches EOF
// must be reported, with no terminating byte needed to "close" it.

/// Runs the whole pipeline and asserts the input is recovered intact at
/// every chunk size, for input that deliberately has *no* terminator --
/// the match runs right up to EOF.
fn sweep_cp932_to_eof(text: &str, tag: &str) {
    let bytes = cp932(text);
    let (input_path, _guard) = temp_path(&format!("eof-{tag}-input"));
    fs::write(&input_path, &bytes).unwrap();

    for chunk_size in 1..=(bytes.len() as u64 + 2) {
        let out = scan_all_chunks_cp932(&input_path, chunk_size, 1, &format!("e-{tag}-{chunk_size}"));
        let joined: String = out
            .lines()
            .map(|l| l.rsplit('\t').next().unwrap_or(""))
            .collect();
        assert_eq!(
            joined, text,
            "chunk_size={chunk_size} did not recover the text that runs to EOF:\n{out}"
        );
    }
}

#[test]
fn cp932_reports_an_ascii_match_that_runs_to_eof() {
    // The minimal reproducer: pure ASCII, no trailing NUL. Before the fix
    // this produced no output whatsoever, while the identical file with a
    // trailing NUL printed correctly -- because the NUL closed the run
    // *before* the boundary, so it was emitted as decoded `Text` and never
    // took the deferred `Raw` path at all.
    sweep_cp932_to_eof("HelloWorldTest", "ascii");
}

#[test]
fn cp932_reports_a_japanese_match_that_runs_to_eof() {
    sweep_cp932_to_eof("「base4 」で検索をかけても１件もヒットせず", "japanese");
}

#[test]
fn cp932_reports_a_match_that_runs_to_eof_ending_on_a_double_byte_char() {
    // Ending on a two-byte character is the case where the final deferred
    // fragment is most likely to be mistaken for an incomplete lead byte
    // and dropped as a truncated tail.
    sweep_cp932_to_eof("test検索結果", "double");
}

#[test]
fn cp932_terminated_and_unterminated_matches_agree() {
    // The two spellings of the same match must produce the same text: one
    // where a NUL closes the run before EOF (the path that always worked)
    // and one where the run reaches EOF (the path that was broken). Any
    // divergence means EOF is still being treated as a special case.
    let text = "HelloWorldTest";

    let (bare_path, _bare_guard) = temp_path("eof-agree-bare");
    fs::write(&bare_path, cp932(text)).unwrap();

    let (term_path, _term_guard) = temp_path("eof-agree-term");
    let mut terminated = cp932(text);
    terminated.push(0);
    fs::write(&term_path, &terminated).unwrap();

    for chunk_size in 1..=(text.len() as u64 + 4) {
        let bare = scan_all_chunks_cp932(&bare_path, chunk_size, 1, &format!("agree-b-{chunk_size}"));
        let term = scan_all_chunks_cp932(&term_path, chunk_size, 1, &format!("agree-t-{chunk_size}"));
        assert_eq!(
            bare, term,
            "chunk_size={chunk_size}: reaching EOF gave a different result than being NUL-terminated"
        );
    }
}
