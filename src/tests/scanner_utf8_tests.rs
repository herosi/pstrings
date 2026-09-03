use super::support::*;
use crate::chunk::Chunk;
use crate::filter::CharacterFilter;
use crate::scanner::utf8;
use crate::READ_BUFFER_SIZE;
use std::fs::{self, File};
use std::sync::atomic::AtomicBool;

// Tests for `scanner::utf8`. Three things make this scanner distinct from
// `scanner::ascii`/`scanner::utf16le` and worth testing specifically:
//   - characters are variable-length (1 to 4 bytes), so `cch` (character
//     count) and `cb` (byte count) genuinely diverge for non-ASCII text,
//     and a single character's bytes can straddle a chunk boundary at any
//     of several split points, not just one;
//   - ASCII-range characters are gated by a *fixed* printable-ASCII rule
//     (`filter::is_ascii_char`, so e.g. CR/LF are excluded, matching
//     scanner::ascii's default), and genuinely multi-byte characters are
//     accepted unless they're a Unicode control character or one of the
//     two Unicode line-breaking separators (U+2028, U+2029) that fall
//     outside `char::is_control`'s C0/C1 range -- see
//     `multibyte_char_allowed`'s doc comment in scanner/utf8.rs. Crucially
//     *neither* rule consults the user's `--filter` selection: this
//     scanner is exempt, so its output is identical no matter what
//     `--filter` says (see `filter_does_not_apply_*` below);
//   - because leading bytes at a chunk boundary might be orphaned
//     continuation bytes left over from the *previous* chunk's
//     boundary-completion peek, this scanner can't know a run's
//     `starts_at_chunk` status from a precomputed position the way
//     scanner::ascii/scanner::utf16le can -- it has to track it at
//     runtime (see `still_at_chunk_start` in scanner/utf8.rs).

#[test]
fn utf8_extracts_plain_ascii_text() {
    // Baseline: a UTF-8 scan over pure ASCII bytes should behave just like
    // scanner::ascii for that same text.
    let (input_path, _input_guard) = temp_path("utf8-ascii-input");
    let (out, _out_guard) = temp_path("utf8-ascii-output");
    fs::write(&input_path, b"Hello, World!").unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(3);
    let chunk = Chunk { offset: 0, len: 13 };
    let (_records, mut result_file) =
        utf8::scan(&file, 13, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].data.text_of(), "Hello, World!");
    assert_eq!(records[0].cb, 13);
    assert_eq!(records[0].cch, 13);
}

#[test]
fn utf8_cr_lf_still_break_runs() {
    // ASCII-range characters remain gated by the configured
    // CharacterFilter (currently: tab + printable 0x20..=0x7e), same as
    // scanner::ascii -- CR and LF are neither, so they must still split
    // runs apart. Without this, a match's `data` could itself contain a
    // newline and corrupt the crate's one-match-per-line text output.
    let (input_path, _input_guard) = temp_path("utf8-crlf-input");
    let (out, _out_guard) = temp_path("utf8-crlf-output");
    let bytes = b"AB\r\nCD".to_vec();
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        utf8::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    let data: Vec<&str> = records.iter().map(|r| r.data.text_of()).collect();
    assert_eq!(data, vec!["AB", "CD"], "{records:?}");
}

#[test]
fn utf8_multibyte_characters_are_included_in_runs() {
    // Unlike the ASCII-range rule, multi-byte characters bypass the
    // configured filter entirely and are simply included: "AB日CD" should
    // come out as ONE continuous run, not split around '日' the way it
    // would be if the Ascii filter's rule applied to it too.
    let (input_path, _input_guard) = temp_path("utf8-multibyte-input");
    let (out, _out_guard) = temp_path("utf8-multibyte-output");
    let mut bytes = b"AB".to_vec();
    bytes.extend_from_slice("日".as_bytes());
    bytes.extend_from_slice(b"CD");
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        utf8::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].data.text_of(), "AB日CD");
    assert_eq!(records[0].cch, 5); // A, B, 日, C, D
    assert_eq!(records[0].cb, 7); // 1+1+3+1+1
}

#[test]
fn utf8_cch_and_cb_diverge_for_multibyte_text() {
    // "café" is 4 characters but 5 bytes ('é' = U+00E9 = 2 UTF-8 bytes).
    // Now that multi-byte characters are accepted, this whole string
    // should come out as a single run with cch != cb.
    let (input_path, _input_guard) = temp_path("utf8-cch-cb-input");
    let (out, _out_guard) = temp_path("utf8-cch-cb-output");
    let bytes = "café".as_bytes().to_vec();
    assert_eq!(bytes.len(), 5, "sanity: 'café' should be 5 UTF-8 bytes");
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        utf8::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].data.text_of(), "café");
    assert_eq!(records[0].cch, 4);
    assert_eq!(records[0].cb, 5);
    assert_eq!(records[0].offset, 0);
}

#[test]
fn utf8_unicode_space_characters_are_allowed() {
    // Unicode space separators (category Zs -- here, U+00A0 NO-BREAK SPACE
    // and U+3000 IDEOGRAPHIC SPACE) are neither control characters nor the
    // two line-breaking separators excluded below, so they must be
    // included in runs just like an ordinary ASCII space.
    let (input_path, _input_guard) = temp_path("utf8-space-input");
    let (out, _out_guard) = temp_path("utf8-space-output");
    let mut bytes = b"A".to_vec();
    bytes.extend_from_slice("\u{a0}".as_bytes()); // NBSP
    bytes.extend_from_slice(b"B");
    bytes.extend_from_slice("\u{3000}".as_bytes()); // ideographic space
    bytes.extend_from_slice(b"C");
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        utf8::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].data.text_of(), "A\u{a0}B\u{3000}C");
}

#[test]
fn utf8_unicode_line_and_paragraph_separators_break_runs() {
    // U+2028 LINE SEPARATOR and U+2029 PARAGRAPH SEPARATOR are well-formed,
    // non-control UTF-8 (category Zl/Zp, not Cc), so `char::is_control`
    // alone wouldn't catch them -- but letting them into `data` would
    // split a record's text across multiple output lines, corrupting the
    // crate's one-match-per-line format. Both must still break runs.
    let (input_path, _input_guard) = temp_path("utf8-linesep-input");
    let (out, _out_guard) = temp_path("utf8-linesep-output");
    let mut bytes = b"AB".to_vec();
    bytes.extend_from_slice("\u{2028}".as_bytes()); // LINE SEPARATOR
    bytes.extend_from_slice(b"CD");
    bytes.extend_from_slice("\u{2029}".as_bytes()); // PARAGRAPH SEPARATOR
    bytes.extend_from_slice(b"EF");
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        utf8::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    let data: Vec<&str> = records.iter().map(|r| r.data.text_of()).collect();
    assert_eq!(data, vec!["AB", "CD", "EF"], "{records:?}");
}

#[test]
fn utf8_invalid_bytes_are_dropped_and_break_runs() {
    // A lone continuation byte (0x80) and an overlong-encoded '/' (C0 AF)
    // are both invalid UTF-8, independent of any filtering rule. Neither
    // should appear in the output, and each should close out whatever run
    // precedes it, same as any other disallowed byte.
    let (input_path, _input_guard) = temp_path("utf8-invalid-input");
    let (out, _out_guard) = temp_path("utf8-invalid-output");
    let mut bytes = b"AB".to_vec();
    bytes.push(0x80); // lone continuation byte
    bytes.extend_from_slice(b"CD");
    bytes.extend_from_slice(&[0xC0, 0xAF]); // overlong encoding of '/'
    bytes.extend_from_slice(b"EF");
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        utf8::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    let data: Vec<&str> = records.iter().map(|r| r.data.text_of()).collect();
    assert_eq!(data, vec!["AB", "CD", "EF"], "{records:?}");
}

#[test]
fn utf8_truncated_sequence_at_true_eof_is_dropped() {
    // The file ends mid-character (only the first byte of a 3-byte
    // sequence is present, with nothing after it -- this is the last
    // chunk AND the last byte of the file, so there's nothing to peek).
    // The dangling lead byte must be silently dropped, not treated as an
    // error, and must not corrupt the preceding run.
    let (input_path, _input_guard) = temp_path("utf8-truncated-eof-input");
    let (out, _out_guard) = temp_path("utf8-truncated-eof-output");
    let mut bytes = b"AB".to_vec();
    bytes.push(0xE6); // first byte of '日' (E6 97 A5), with no continuation bytes at all
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        utf8::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].data.text_of(), "AB");
}

#[test]
fn utf8_multibyte_character_crosses_chunk_boundary_and_stays_one_run() {
    // '日' (E6 97 A5, 3 bytes) is split so its first byte lands in chunk0
    // and the remaining two land in chunk1, with plain ASCII on both
    // sides. Since multi-byte characters are now included in runs, the
    // whole thing -- "AB日CD" -- must come back as ONE joined record, not
    // two, despite the character straddling the boundary and more content
    // immediately following it in chunk1's own territory.
    //
    // This specifically exercises `still_at_chunk_start`: chunk1's first
    // *usable* content ("CD") sits a couple of bytes after chunk1.offset,
    // behind the two orphaned continuation bytes left over from chunk0's
    // boundary peek -- so `starts_at_chunk` can only be computed correctly
    // by tracking "nothing but invalid bytes seen yet", not by comparing
    // the run's offset directly against `chunk.offset`.
    let (input_path, _input_guard) = temp_path("utf8-boundary-input");
    let (out0, _out0_guard) = temp_path("utf8-boundary-0");
    let (out1, _out1_guard) = temp_path("utf8-boundary-1");
    let mut bytes = b"AB".to_vec(); // bytes[0..2]
    bytes.extend_from_slice("日".as_bytes()); // bytes[2..5]
    bytes.extend_from_slice(b"CD"); // bytes[5..7]
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();
    let file_len = bytes.len() as u64;

    let cfg = test_config(1);
    let cancel = AtomicBool::new(false);
    // chunk0 = bytes[0..3]: "AB" + the first byte of 日.
    let (_r0, file0) =
        utf8::scan(&file, file_len, &Chunk { offset: 0, len: 3 }, &cfg, &out0, &cancel).unwrap();
    // chunk1 = bytes[3..7]: the remaining two bytes of 日, then "CD".
    let (_r1, file1) =
        utf8::scan(&file, file_len, &Chunk { offset: 3, len: file_len - 3 }, &cfg, &out1, &cancel).unwrap();

    let text = merge_test_encoding_chunks(vec![file0, file1], cfg.min_cch());
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "AB日CD", "{text}");
    assert_eq!(lines[0][..20].parse::<u64>().unwrap(), 0, "{text}");
}

#[test]
fn utf8_four_byte_character_crosses_chunk_boundary_at_every_split_point() {
    // Same idea as the 3-byte case above, but with a 4-byte emoji
    // character, and repeated for every possible split point (1, 2, or 3
    // of its bytes left in chunk0) to make sure the boundary-completion
    // peek and the `still_at_chunk_start` join logic both hold up
    // regardless of exactly how many bytes were orphaned.
    let emoji = "😀".as_bytes().to_vec();
    assert_eq!(emoji.len(), 4);

    for split in 1..=3usize {
        let (input_path, _input_guard) = temp_path(&format!("utf8-emoji-split-{split}"));
        let (out0, _out0_guard) = temp_path(&format!("utf8-emoji-split-{split}-0"));
        let (out1, _out1_guard) = temp_path(&format!("utf8-emoji-split-{split}-1"));
        let mut bytes = b"AB".to_vec();
        bytes.extend_from_slice(&emoji);
        bytes.extend_from_slice(b"CD");
        fs::write(&input_path, &bytes).unwrap();
        let file = File::open(&input_path).unwrap();
        let file_len = bytes.len() as u64;

        let cfg = test_config(1);
        let cancel = AtomicBool::new(false);
        let chunk0_len = 2 + split as u64; // "AB" + first `split` bytes of the emoji
        let (_r0, file0) =
            utf8::scan(&file, file_len, &Chunk { offset: 0, len: chunk0_len }, &cfg, &out0, &cancel).unwrap();
        let (_r1, file1) = utf8::scan(
            &file,
            file_len,
            &Chunk {
                offset: chunk0_len,
                len: file_len - chunk0_len,
            },
            &cfg,
            &out1,
            &cancel,
        )
        .unwrap();

        let text = merge_test_encoding_chunks(vec![file0, file1], cfg.min_cch());
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 1, "split={split}: {text}");
        assert_eq!(lines[0].split('\t').nth(2).unwrap(), "AB😀CD", "split={split}: {text}");
        assert_eq!(lines[0][..20].parse::<u64>().unwrap(), 0, "split={split}: {text}");
    }
}

#[test]
fn utf8_string_crosses_three_chunk_boundaries() {
    // ASCII text spanning three small chunks (chunk size 4), using the
    // real end-to-end chunking path (`scan_all_chunks_utf8`, which computes
    // real `Chunk`s from a chunk size rather than manually constructing
    // them) -- confirms joining isn't limited to two adjacent chunks for
    // this scanner either.
    let (input_path, _input_guard) = temp_path("utf8-three-chunks-input");
    fs::write(&input_path, b"ABCDEFGHIJKLM\0").unwrap();

    let text = scan_all_chunks_utf8(&input_path, 4, 4, "utf8-three-chunks");
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "ABCDEFGHIJKLM", "{text}");
    assert!(lines[0].starts_with("00000000000000000000\tUTF8\t"), "{text}");
}

#[test]
fn utf8_boundary_fragment_below_min_cch_is_preserved_and_joined() {
    // A 6-char fragment ("XXXXXX") split across a chunk boundary (chunk
    // size 8) so at least one side is short in isolation; per
    // `emit_record`'s boundary exception it must survive regardless of
    // `min_cch` and be correctly joined back together.
    let (input_path, _input_guard) = temp_path("utf8-boundary-short-fragment-input");
    fs::write(&input_path, b"\0XXXXXXABCD\0").unwrap();

    let text = scan_all_chunks_utf8(&input_path, 8, 4, "utf8-boundary-short-fragment");
    let lines: Vec<_> = text.lines().collect();
    assert!(
        lines.iter().any(|line| line.starts_with("00000000000000000001\tUTF8\tXXXXXXABCD")),
        "{text}"
    );
}

#[test]
fn utf8_run_spans_multiple_read_blocks() {
    // A run longer than 2 full `READ_BUFFER_SIZE` read blocks, confirming
    // it survives crossing an internal I/O block boundary (a distinct
    // concern from crossing a `Chunk` boundary) as a single record rather
    // than being split or duplicated at the block edges.
    let (input_path, _input_guard) = temp_path("utf8-multiblock-input");
    let run_len = READ_BUFFER_SIZE * 2 + 137;
    let mut bytes = vec![b'\0'];
    bytes.extend(std::iter::repeat(b'A').take(run_len));
    bytes.push(b'\0');
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let (out, _out_guard) = temp_path("utf8-multiblock-output");
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        utf8::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].offset, 1);
    assert_eq!(records[0].cch, run_len as u64);
    assert_eq!(records[0].cb, run_len as u64);
    assert!(records[0].data.text_of().bytes().all(|b| b == b'A'));
}

#[test]
fn utf8_min_cch_boundary_is_inclusive() {
    // Same inclusive-cutoff check as the ASCII/UTF-16LE versions of this
    // test in scanner_tests.rs: at min_cch=N, a length-N run must still
    // survive, and only strictly shorter runs are dropped.
    let (input_path, _input_guard) = temp_path("utf8-min-boundaries-input");
    let bytes = b"A\0AB\0ABC\0ABCD\0";
    fs::write(&input_path, bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    for min_cch in 1..=4u64 {
        let (out, _out_guard) = temp_path(&format!("utf8-min-boundaries-output-{min_cch}"));
        let cfg = test_config(min_cch);
        let chunk = Chunk {
            offset: 0,
            len: bytes.len() as u64,
        };
        let (_records, result_file) =
            utf8::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

        let text = merge_test_encoding_chunks(vec![result_file], min_cch);
        let expected: Vec<&str> = match min_cch {
            1 => vec!["A", "AB", "ABC", "ABCD"],
            2 => vec!["AB", "ABC", "ABCD"],
            3 => vec!["ABC", "ABCD"],
            4 => vec!["ABCD"],
            _ => unreachable!(),
        };
        let actual: Vec<&str> = text.lines().map(|line| line.split('\t').nth(2).unwrap()).collect();
        assert_eq!(actual, expected, "min_cch={min_cch}: {text}");
    }
}

#[test]
fn utf8_content_before_invalid_bytes_prevents_false_chunk_start_join() {
    // Guards the OTHER side of `still_at_chunk_start`: if a chunk contains
    // genuine content ("X") *before* some invalid bytes and a later,
    // otherwise boundary-looking run ("CD"), that later run must NOT be
    // treated as touching the chunk's start just because a naive
    // implementation might think "everything before it was skippable."
    // Real content resets the tracking -- only *leading* invalid bytes
    // (with nothing decodable before them) are treated as orphaned
    // continuation-byte noise.
    let (input_path, _input_guard) = temp_path("utf8-false-start-input");
    let (out, _out_guard) = temp_path("utf8-false-start-output");
    let mut bytes = b"AB".to_vec();
    bytes.extend_from_slice("X".as_bytes());
    bytes.push(0x80); // invalid, but NOT at the very start of the chunk
    bytes.extend_from_slice(b"CD");
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        utf8::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[0].data.text_of(), "ABX");
    assert!(records[0].starts_at_chunk, "{records:?}");
    assert_eq!(records[1].data.text_of(), "CD");
    assert!(!records[1].starts_at_chunk, "the second run must NOT look like a chunk start: {records:?}");
}

// --- `--filter` independence -------------------------------------------
//
// `--filter` exists to suppress false positives in scanners that cannot
// validate their own input -- overwhelmingly `scanner::utf16le`, where
// every even-aligned byte pair is a syntactically valid code unit. UTF-8
// has no such problem: `decode_step` structurally rejects anything
// malformed, so a UTF-8 match is already trustworthy.
//
// That makes filtering UTF-8 not merely unnecessary but actively harmful.
// A user scanning a Japanese binary would reasonably write
// `--filter kanji,hiragana,katakana`, dropping `ascii` precisely to quiet
// the UTF-16LE scanner -- and would then be surprised to find the UTF-8
// scanner had silently stopped reporting plain ASCII strings too.
//
// These tests pin that independence down in both directions, so that
// re-introducing `cfg.filter()` into scanner::utf8's hot path fails
// loudly rather than silently narrowing its output.

#[test]
fn utf8_filter_does_not_apply_ascii_survives_without_the_ascii_filter() {
    // The motivating case: `ascii` is NOT selected, yet plain ASCII text
    // must still be matched by the UTF-8 scanner.
    let (input_path, _input_guard) = temp_path("utf8-nofilter-ascii-input");
    let (out, _out_guard) = temp_path("utf8-nofilter-ascii-output");
    fs::write(&input_path, b"Hello, World!").unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config_with_filters(3, 8, vec![CharacterFilter::Kanji]);
    let chunk = Chunk { offset: 0, len: 13 };
    let (_records, mut result_file) =
        utf8::scan(&file, 13, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(
        records.len(),
        1,
        "ASCII must survive --filter kanji in the UTF-8 scanner: {records:?}"
    );
    assert_eq!(records[0].data.text_of(), "Hello, World!");
}

#[test]
fn utf8_filter_does_not_apply_non_selected_scripts_still_match() {
    // The converse: a script the filter does *not* mention must still be
    // matched, since multi-byte characters are judged only on whether they
    // would corrupt the line-oriented output.
    let (input_path, _input_guard) = temp_path("utf8-nofilter-script-input");
    let (out, _out_guard) = temp_path("utf8-nofilter-script-output");
    // Hiragana + hangul + a Latin-1 supplement character, none of which
    // `--filter ascii` covers.
    let text = "ひらがな한글é";
    fs::write(&input_path, text.as_bytes()).unwrap();
    let file = File::open(&input_path).unwrap();

    let len = text.len() as u64;
    let cfg = test_config_with_filters(1, 8, vec![CharacterFilter::Ascii]);
    let chunk = Chunk { offset: 0, len };
    let (_records, mut result_file) =
        utf8::scan(&file, len, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].data.text_of(), text);
    assert_eq!(records[0].cch, text.chars().count() as u64);
}

#[test]
fn utf8_filter_choice_never_changes_the_output() {
    // Strongest form of the invariant: sweep several very different filter
    // selections over one mixed-script input and assert every one produces
    // byte-identical output. Any future re-introduction of `cfg.filter()`
    // into this scanner would have to change at least one of these.
    let text = "ASCII ひらがな 漢字 한글 café";
    let (input_path, _input_guard) = temp_path("utf8-filter-sweep-input");
    fs::write(&input_path, text.as_bytes()).unwrap();

    let selections = [
        vec![CharacterFilter::Ascii],
        vec![CharacterFilter::Latin1],
        vec![CharacterFilter::Kanji],
        vec![CharacterFilter::Hiragana, CharacterFilter::Katakana],
        vec![CharacterFilter::Hangul, CharacterFilter::CjkPunct],
        vec![CharacterFilter::Ascii, CharacterFilter::Latin1, CharacterFilter::Kanji],
    ];

    let len = text.len() as u64;
    let mut baseline: Option<String> = None;
    for (i, filters) in selections.into_iter().enumerate() {
        let (out, _out_guard) = temp_path(&format!("utf8-filter-sweep-out-{i}"));
        let file = File::open(&input_path).unwrap();
        let cfg = test_config_with_filters(1, 8, filters.clone());
        let chunk = Chunk { offset: 0, len };
        let (_records, mut result_file) =
            utf8::scan(&file, len, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

        let records = read_records(&mut result_file);
        let rendered = records
            .iter()
            .map(|r| format!("{}:{}:{}:{}", r.offset, r.cb, r.cch, r.data.text_of()))
            .collect::<Vec<_>>()
            .join("|");

        match &baseline {
            None => baseline = Some(rendered),
            Some(expected) => assert_eq!(
                &rendered, expected,
                "--filter {filters:?} changed scanner::utf8's output; it must be filter-independent"
            ),
        }
    }

    // Sanity check that the sweep was actually comparing something real.
    assert!(baseline.as_deref().is_some_and(|b| b.contains(text)), "{baseline:?}");
}

#[test]
fn utf8_control_characters_are_excluded_regardless_of_filter() {
    // The flip side of exemption: being filter-independent must not mean
    // "accept everything". The fixed rules still exclude C0 controls in
    // the ASCII range and Unicode line separators outside it, because both
    // would corrupt the one-record-per-line output format.
    let (input_path, _input_guard) = temp_path("utf8-nofilter-controls-input");
    let (out, _out_guard) = temp_path("utf8-nofilter-controls-output");
    let mut bytes = b"AB".to_vec();
    bytes.push(b'\n');
    bytes.extend_from_slice("CD\u{2028}EF".as_bytes());
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let len = bytes.len() as u64;
    // A filter that admits nothing in the ASCII range at all, to prove the
    // exclusions come from the fixed rules and not from the filter.
    let cfg = test_config_with_filters(1, 8, vec![CharacterFilter::Kanji]);
    let chunk = Chunk { offset: 0, len };
    let (_records, mut result_file) =
        utf8::scan(&file, len, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    let texts: Vec<&str> = records.iter().map(|r| r.data.text_of()).collect();
    assert_eq!(texts, vec!["AB", "CD", "EF"], "{records:?}");
}