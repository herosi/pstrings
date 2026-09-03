use super::support::*;
use crate::chunk::Chunk;
use crate::filter::CharacterFilter;
use crate::record::RecordData;
use crate::scanner::cp932;
use crate::READ_BUFFER_SIZE;
use std::fs::{self, File};
use std::sync::atomic::AtomicBool;

// Tests for `scanner::cp932`. Three things make this scanner distinct
// from every other scanner in this crate:
//
//   - CP932 is not self-synchronizing (see
//     `InputEncoding::is_self_synchronizing`): a byte's role (lead,
//     trail, or standalone) can be genuinely ambiguous at a chunk
//     boundary, since the trailing-byte range overlaps heavily with both
//     the ASCII range and the lead-byte range. So chunk-boundary-
//     touching runs are collected as *raw, undecoded* bytes
//     (`RecordData::Raw`) rather than decoded immediately, and are only
//     resolved once `outputter` knows whether (and how) they join with
//     their neighbor -- see scanner/cp932.rs's module doc comment.
//   - Because that raw region's true extent can't be known at scan time,
//     it is *not* narrowed down by any clever heuristic -- collection
//     just continues for as long as bytes stay loosely CP932-shaped,
//     stopping only at a genuinely invalid byte or chunk_end. Most tests
//     below go through the merge/output path (`merge_test_encoding_chunks`
//     / `scan_all_chunks_cp932`), not raw scanner output, because that's
//     the only place `RecordData::Raw` fragments actually get resolved
//     into checkable text.
//   - A chain of raw fragments can span *more than one* chunk boundary in
//     a row (e.g. alternating single/double-byte characters), so several
//     tests below deliberately use very small chunk sizes to exercise
//     that chaining, not just a single two-chunk join.
//
// Test inputs use `support::cp932(&str) -> Vec<u8>` to build raw CP932
// bytes from ordinary Rust string literals.

/// Extracts the decoded text from a `MatchRecord`, panicking if it's
/// still an unresolved `RecordData::Raw` (which should never reach a test
/// assertion -- every test here goes through a path that fully resolves
/// boundary fragments before checking their content).
fn text_of(rec: &crate::record::MatchRecord) -> &str {
    match &rec.data {
        RecordData::Text(s) => s,
        RecordData::Raw(_) => panic!("test observed an unresolved Raw record: {rec:?}"),
    }
}

#[test]
fn cp932_extracts_ascii_and_kana_text() {
    // Baseline: plain ASCII plus half-width katakana in one chunk,
    // trailing NUL so the run closes before chunk_end (avoiding the
    // deferred-Raw path entirely, so raw scanner output is directly
    // checkable here without going through the outputter).
    let (input_path, _input_guard) = temp_path("cp932-ascii-kana-input");
    let (out, _out_guard) = temp_path("cp932-ascii-kana-output");
    let mut bytes = cp932("HelloWorld");
    bytes.extend(cp932("ｶﾀｶﾅ"));
    bytes.push(0); // NUL: not loosely CP932-shaped, closes the run
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        cp932::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(text_of(&records[0]), "HelloWorldｶﾀｶﾅ");
}

#[test]
fn cp932_extracts_kanji_text() {
    // Kanji/kana text, single chunk, trailing NUL to close the run before
    // chunk_end for the same reason as above.
    let (input_path, _input_guard) = temp_path("cp932-kanji-input");
    let (out, _out_guard) = temp_path("cp932-kanji-output");
    let mut bytes = cp932("吾輩は猫である");
    bytes.push(0);
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        cp932::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(text_of(&records[0]), "吾輩は猫である");
}

#[test]
fn cp932_cch_and_cb_are_distinct() {
    // Every kanji/hiragana character here is 2 bytes, so cb (byte count)
    // must come out as exactly double cch (character count).
    let (input_path, _input_guard) = temp_path("cp932-cch-cb-input");
    let (out, _out_guard) = temp_path("cp932-cch-cb-output");
    let mut bytes = cp932("日本語");
    bytes.push(0);
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        cp932::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].cch, 3);
    assert_eq!(records[0].cb, 6);
}

#[test]
fn cp932_invalid_bytes_break_runs() {
    // A stray 0xFF (not in any valid CP932 byte range at all) and an
    // undefined-but-in-range pair (0xFC 0x4C -- both bytes individually
    // plausible, but encoding_rs doesn't assign this pair a character)
    // must each break the surrounding run without appearing in the
    // output.
    let (input_path, _input_guard) = temp_path("cp932-invalid-input");
    let (out, _out_guard) = temp_path("cp932-invalid-output");
    let mut bytes = cp932("AB");
    bytes.push(0xFF);
    bytes.extend(cp932("CD"));
    bytes.extend_from_slice(&[0xFC, 0x80]); // structurally in-range, undefined pair
    bytes.extend(cp932("EF"));
    bytes.push(0);
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        cp932::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    let texts: Vec<&str> = records.iter().map(text_of).collect();
    assert_eq!(texts, vec!["AB", "CD", "EF"], "{records:?}");
}

#[test]
fn cp932_truncated_lead_byte_at_true_eof_is_dropped() {
    // The file ends with a dangling lead byte and nothing after it -- the
    // last chunk of the file, so there's no possible continuation. The
    // dangling byte must be silently dropped, not treated as an error,
    // and must not corrupt the preceding text.
    let (input_path, _input_guard) = temp_path("cp932-truncated-input");
    let (out, _out_guard) = temp_path("cp932-truncated-output");
    let mut bytes = cp932("AB");
    bytes.push(0x82); // a valid lead byte with no trailing byte at all
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_r, file0) = cp932::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let text = merge_test_encoding_chunks(vec![file0], 1);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "AB", "{text}");
}

#[test]
fn cp932_character_crosses_chunk_boundary() {
    // '日' (93 FA) is split so its lead byte lands in chunk0 and its trail
    // byte lands in chunk1, with plain ASCII on both sides. Must come
    // back as one joined "AB日CD" record, not two.
    let (input_path, _input_guard) = temp_path("cp932-boundary-input");
    let (out0, _out0_guard) = temp_path("cp932-boundary-0");
    let (out1, _out1_guard) = temp_path("cp932-boundary-1");
    let mut bytes = cp932("AB"); // bytes[0..2]
    bytes.extend(cp932("日")); // bytes[2..4]
    bytes.extend(cp932("CD")); // bytes[4..6]
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();
    let file_len = bytes.len() as u64;

    let cfg = test_config(1);
    let cancel = AtomicBool::new(false);
    // chunk0 = bytes[0..3]: "AB" + the lead byte of 日.
    let (_r0, file0) =
        cp932::scan(&file, file_len, &Chunk { offset: 0, len: 3 }, &cfg, &out0, &cancel).unwrap();
    // chunk1 = bytes[3..6]: the trail byte of 日, then "CD".
    let (_r1, file1) =
        cp932::scan(&file, file_len, &Chunk { offset: 3, len: file_len - 3 }, &cfg, &out1, &cancel).unwrap();

    let text = merge_test_encoding_chunks(vec![file0, file1], cfg.min_cch());
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "AB日CD", "{text}");
    assert_eq!(lines[0][..20].parse::<u64>().unwrap(), 0, "{text}");
}

#[test]
fn cp932_ambiguous_boundary_resolves_via_backward_join() {
    // '＝' is 81 81 -- its trail byte (0x81) is *also* a valid lead byte
    // on its own, making this the genuinely ambiguous case (see
    // scanner/cp932.rs's module doc comment): could this position be the
    // continuation of the previous chunk's dangling lead byte, or the
    // start of a fresh pair? Split so chunk0 ends with '＝'s lead byte and
    // chunk1 starts with the rest ('＝'s trail byte, then 'あ', 'る').
    // Since chunk0 truly does have a dangling lead byte here, the correct
    // resolution is the backward join: "＝あるる".
    let (input_path, _input_guard) = temp_path("cp932-ambiguous-join-input");
    let (out0, _out0_guard) = temp_path("cp932-ambiguous-join-0");
    let (out1, _out1_guard) = temp_path("cp932-ambiguous-join-1");
    let bytes = cp932("＝あるる");
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();
    let file_len = bytes.len() as u64;

    let cfg = test_config(1);
    let cancel = AtomicBool::new(false);
    // chunk0 = bytes[0..1]: just '＝'s lead byte (0x81), dangling.
    let (_r0, file0) = cp932::scan(&file, file_len, &Chunk { offset: 0, len: 1 }, &cfg, &out0, &cancel).unwrap();
    // chunk1 = the rest of the file.
    let (_r1, file1) =
        cp932::scan(&file, file_len, &Chunk { offset: 1, len: file_len - 1 }, &cfg, &out1, &cancel).unwrap();

    let text = merge_test_encoding_chunks(vec![file0, file1], cfg.min_cch());
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "＝あるる", "{text}");
}

#[test]
fn cp932_ambiguous_leading_byte_pairs_fresh_when_first_chunk() {
    // Same raw bytes as the previous test (81 81 82 a0 82 e9 82 e9), but
    // this time as the FIRST chunk of a file (no predecessor to possibly
    // join with). `is_first_chunk` must short-circuit the leading
    // collection entirely, so the whole thing is scanned completely
    // normally, with no deferred/boundary machinery triggered at all --
    // the raw scanner output itself is directly checkable here.
    let (input_path, _input_guard) = temp_path("cp932-first-chunk-input");
    let (out, _out_guard) = temp_path("cp932-first-chunk-output");
    let mut bytes = cp932("＝あるる");
    bytes.push(0);
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        cp932::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(text_of(&records[0]), "＝あるる");
}

#[test]
fn cp932_leading_region_merges_seamlessly_with_interior_content() {
    // Regression test for a bug found while designing this scanner: the
    // deferred leading region must not stop the moment ambiguity is
    // structurally resolved -- it must keep collecting raw bytes for as
    // long as the run continues uninterrupted, right through into what
    // would otherwise be scanned as ordinary "interior" content. Here,
    // '＝あるる。' (all one continuous run, no invalid byte anywhere) is
    // split with chunk0 holding only '＝'s lead byte (forcing chunk1's
    // entire remaining content into the deferred/boundary path). If the
    // leading region were cut short as soon as the join-vs-fresh
    // ambiguity structurally resolves (the bug), this would wrongly come
    // back as two records ("＝あ" and "るる。") instead of one.
    let (input_path, _input_guard) = temp_path("cp932-seamless-input");
    let (out0, _out0_guard) = temp_path("cp932-seamless-0");
    let (out1, _out1_guard) = temp_path("cp932-seamless-1");
    let bytes = cp932("＝あるる。");
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();
    let file_len = bytes.len() as u64;

    let cfg = test_config(1);
    let cancel = AtomicBool::new(false);
    let (_r0, file0) = cp932::scan(&file, file_len, &Chunk { offset: 0, len: 1 }, &cfg, &out0, &cancel).unwrap();
    let (_r1, file1) =
        cp932::scan(&file, file_len, &Chunk { offset: 1, len: file_len - 1 }, &cfg, &out1, &cancel).unwrap();

    let text = merge_test_encoding_chunks(vec![file0, file1], cfg.min_cch());
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "expected ONE unbroken record: {text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "＝あるる。", "{text}");
}

#[test]
fn cp932_pending_chain_spans_more_than_two_chunks() {
    // Forces a chain of deferred fragments across three chunk boundaries
    // in a row (chunk size 1, so nearly every byte lands in its own
    // chunk), exercising the outputter's "leftover" re-chaining -- not
    // just a single two-chunk join.
    let (input_path, _input_guard) = temp_path("cp932-chain-input");
    let bytes = cp932("吾輩は猫である");
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks_cp932(&input_path, 1, 1, "cp932-chain");
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "吾輩は猫である", "{text}");
    assert!(lines[0].starts_with("00000000000000000000\tCP932\t"), "{text}");
}

#[test]
fn cp932_string_crosses_three_chunk_boundaries() {
    // ASCII text spanning three small chunks (chunk size 4), using the
    // real end-to-end chunking path.
    let (input_path, _input_guard) = temp_path("cp932-three-chunks-input");
    fs::write(&input_path, b"ABCDEFGHIJKLM\0").unwrap();

    let text = scan_all_chunks_cp932(&input_path, 4, 4, "cp932-three-chunks");
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "ABCDEFGHIJKLM", "{text}");
}

#[test]
fn cp932_boundary_fragment_below_min_cch_is_preserved_and_joined() {
    // CP932 analogue of `ascii_boundary_fragment_below_min_cch_is_preserved_and_joined`.
    // The input is deliberately 16 bytes (NUL + seven 2-byte characters +
    // NUL) so that at chunk size 8 the run genuinely straddles the
    // boundary at offset 8 -- and, because '猫' (94 4C) sits exactly on
    // it, the split even lands *inside* a character. Neither chunk-local
    // piece is independently reportable (a `RecordData::Raw` fragment's
    // `cch` is only a placeholder, so it can never clear `min_cch=4` on
    // its own); both survive purely by `emit_record`'s boundary
    // exception, and must then be joined back into the full 7-character
    // string.
    let (input_path, _input_guard) = temp_path("cp932-short-fragment-input");
    let mut bytes = vec![0u8];
    bytes.extend(cp932("吾輩は猫である"));
    bytes.push(0);
    assert_eq!(bytes.len(), 16, "fixture must span exactly two 8-byte chunks");

    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks_cp932(&input_path, 8, 4, "cp932-short-fragment");
    let lines: Vec<_> = text.lines().collect();
    assert!(
        lines
            .iter()
            .any(|line| line.split('\t').nth(2) == Some("吾輩は猫である")),
        "{text}"
    );
}

#[test]
fn cp932_min_cch_boundary_is_inclusive() {
    // Same inclusive-cutoff check used for the other scanners: at
    // min_cch=N, a length-N run must still survive, and only strictly
    // shorter runs are dropped.
    let (input_path, _input_guard) = temp_path("cp932-min-cch-input");
    let mut bytes = Vec::new();
    for s in ["亜", "亜亜", "亜亜亜", "亜亜亜亜"] {
        bytes.extend(cp932(s));
        bytes.push(0);
    }
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    for min_cch in 1..=4u64 {
        let (out, _out_guard) = temp_path(&format!("cp932-min-cch-output-{min_cch}"));
        let cfg = test_config(min_cch);
        let chunk = Chunk {
            offset: 0,
            len: bytes.len() as u64,
        };
        let (_records, result_file) =
            cp932::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

        let text = merge_test_encoding_chunks(vec![result_file], min_cch);
        let expected: Vec<&str> = match min_cch {
            1 => vec!["亜", "亜亜", "亜亜亜", "亜亜亜亜"],
            2 => vec!["亜亜", "亜亜亜", "亜亜亜亜"],
            3 => vec!["亜亜亜", "亜亜亜亜"],
            4 => vec!["亜亜亜亜"],
            _ => unreachable!(),
        };
        let actual: Vec<&str> = text.lines().map(|line| line.split('\t').nth(2).unwrap()).collect();
        assert_eq!(actual, expected, "min_cch={min_cch}: {text}");
    }
}

#[test]
fn cp932_run_spans_multiple_read_blocks() {
    // A run longer than 2 full READ_BUFFER_SIZE read blocks, confirming
    // it survives crossing an internal I/O block boundary as a single
    // record rather than being split or duplicated at the block edges.
    let (input_path, _input_guard) = temp_path("cp932-multiblock-input");
    let run_len = READ_BUFFER_SIZE * 2 + 137;
    let mut bytes = vec![0u8];
    bytes.extend(std::iter::repeat(b'A').take(run_len));
    bytes.push(0);
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let (out, _out_guard) = temp_path("cp932-multiblock-output");
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        cp932::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].offset, 1);
    assert_eq!(records[0].cch, run_len as u64);
    assert_eq!(records[0].cb, run_len as u64);
    assert!(text_of(&records[0]).bytes().all(|b| b == b'A'));
}

// --- `--filter` independence -------------------------------------------
//
// `--filter` exists to suppress false positives in scanners that cannot
// validate their own input -- overwhelmingly `scanner::utf16le`, where
// every even-aligned byte pair is a syntactically valid code unit. CP932
// has no such problem: lead/trail byte ranges are checked structurally and
// every two-byte pair is confirmed against `encoding_rs`, so a CP932 match
// is already trustworthy.
//
// Filtering CP932 would therefore be not merely unnecessary but actively
// harmful, in two distinct ways:
//
//   1. A user scanning a Japanese binary would reasonably write
//      `--filter kanji,hiragana,katakana`, dropping `ascii` precisely to
//      quiet the UTF-16LE scanner -- and would then be surprised to find
//      CP932 had silently stopped reporting plain ASCII strings too.
//   2. Half-width katakana is part of CP932's natural *single-byte* set
//      (0xA1..=0xDF) but no filter variant can express it as a byte:
//      `Latin1`'s byte range 0xA0..=0xFF would wrongly admit it (those
//      bytes are U+FF61..=U+FF9F here, not U+00A1..=U+00DF), while
//      `Katakana`'s `allows_u8` returns false for every byte and would
//      wrongly reject it. Either way the answer would be wrong.
//
// These tests pin that independence down so that introducing
// `cfg.filter()` into this scanner fails loudly rather than silently
// narrowing its output.

#[test]
fn cp932_filter_does_not_apply_ascii_survives_without_the_ascii_filter() {
    // The motivating case: `ascii` is NOT selected, yet plain ASCII text
    // must still be matched.
    let (input_path, _input_guard) = temp_path("cp932-nofilter-ascii-input");
    let (out, _out_guard) = temp_path("cp932-nofilter-ascii-output");
    let mut bytes = cp932("HelloWorld");
    bytes.push(0); // close the run before chunk_end, avoiding the Raw path
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let len = bytes.len() as u64;
    let cfg = test_config_with_filters(1, 8, vec![CharacterFilter::Kanji]);
    let chunk = Chunk { offset: 0, len };
    let (_records, mut result_file) =
        cp932::scan(&file, len, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(
        records.len(),
        1,
        "ASCII must survive --filter kanji in the CP932 scanner: {records:?}"
    );
    assert_eq!(text_of(&records[0]), "HelloWorld");
}

#[test]
fn cp932_filter_does_not_apply_half_width_kana_survives() {
    // Point 2 above: half-width katakana must be matched even under a
    // filter selection that no `allows_u8` implementation would accept
    // those bytes for.
    let (input_path, _input_guard) = temp_path("cp932-nofilter-kana-input");
    let (out, _out_guard) = temp_path("cp932-nofilter-kana-output");
    let mut bytes = cp932("ｶﾀｶﾅﾃｽﾄ");
    bytes.push(0);
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let len = bytes.len() as u64;
    let cfg = test_config_with_filters(1, 8, vec![CharacterFilter::Kanji]);
    let chunk = Chunk { offset: 0, len };
    let (_records, mut result_file) =
        cp932::scan(&file, len, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(text_of(&records[0]), "ｶﾀｶﾅﾃｽﾄ");
}

#[test]
fn cp932_filter_choice_never_changes_the_output() {
    // Strongest form of the invariant: sweep several very different filter
    // selections over one mixed input and assert every one produces
    // identical output. Any future re-introduction of `cfg.filter()` into
    // this scanner would have to change at least one of these.
    let (input_path, _input_guard) = temp_path("cp932-filter-sweep-input");
    let mut bytes = cp932("ASCII 漢字ひらがな");
    bytes.extend(cp932("ｶﾀｶﾅ"));
    bytes.push(0);
    fs::write(&input_path, &bytes).unwrap();

    let selections = [
        vec![CharacterFilter::Ascii],
        vec![CharacterFilter::Latin1],
        vec![CharacterFilter::Kanji],
        vec![CharacterFilter::Hiragana, CharacterFilter::Katakana],
        vec![CharacterFilter::Hangul, CharacterFilter::CjkPunct],
        vec![CharacterFilter::Ascii, CharacterFilter::Latin1, CharacterFilter::Kanji],
    ];

    let len = bytes.len() as u64;
    let mut baseline: Option<String> = None;
    for (i, filters) in selections.into_iter().enumerate() {
        let (out, _out_guard) = temp_path(&format!("cp932-filter-sweep-out-{i}"));
        let file = File::open(&input_path).unwrap();
        let cfg = test_config_with_filters(1, 8, filters.clone());
        let chunk = Chunk { offset: 0, len };
        let (_records, mut result_file) =
            cp932::scan(&file, len, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

        let records = read_records(&mut result_file);
        let rendered = records
            .iter()
            .map(|r| format!("{}:{}:{}:{}", r.offset, r.cb, r.cch, text_of(r)))
            .collect::<Vec<_>>()
            .join("|");

        match &baseline {
            None => baseline = Some(rendered),
            Some(expected) => assert_eq!(
                &rendered, expected,
                "--filter {filters:?} changed scanner::cp932's output; it must be filter-independent"
            ),
        }
    }

    // Sanity check that the sweep was actually comparing something real.
    assert!(
        baseline.as_deref().is_some_and(|b| b.contains("ASCII 漢字ひらがなｶﾀｶﾅ")),
        "{baseline:?}"
    );
}

#[test]
fn cp932_control_characters_are_excluded_regardless_of_filter() {
    // The flip side of exemption: being filter-independent must not mean
    // "accept everything". The fixed rule still excludes C0 controls,
    // because a newline inside a record would corrupt the crate's
    // one-record-per-line output format.
    let (input_path, _input_guard) = temp_path("cp932-nofilter-controls-input");
    let (out, _out_guard) = temp_path("cp932-nofilter-controls-output");
    let mut bytes = cp932("AB");
    bytes.push(b'\n');
    bytes.extend(cp932("CD"));
    bytes.push(0);
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let len = bytes.len() as u64;
    // A filter that admits nothing in the ASCII range at all, to prove the
    // exclusion comes from the fixed rule and not from the filter.
    let cfg = test_config_with_filters(1, 8, vec![CharacterFilter::Kanji]);
    let chunk = Chunk { offset: 0, len };
    let (_records, mut result_file) =
        cp932::scan(&file, len, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    let texts: Vec<&str> = records.iter().map(text_of).collect();
    assert_eq!(texts, vec!["AB", "CD"], "{records:?}");
}