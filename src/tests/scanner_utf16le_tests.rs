//! Tests for the full UTF-16LE scanner (`scanner::utf16le`).
//!
//! Organized in four groups, matching the layers described in
//! `scanner::utf16le`'s own doc comments:
//! 1. BMP character filtering (`filter::allows_u16` via `CharacterFilter`).
//! 2. Astral (surrogate-pair) decoding, independent of filtering.
//! 3. The dual-parity overlap mitigation (`resolve_parity_overlap`).
//! 4. Chunk/read-block boundary correctness (the trickiest, most
//!    bug-prone part of this scanner -- see `read_scan_block`).

use super::support::*;
use crate::chunk::Chunk;
use crate::filter::CharacterFilter;
use crate::scanner::utf16le;
use crate::READ_BUFFER_SIZE;
use std::fs;
use std::sync::atomic::AtomicBool;

// ---------------------------------------------------------------------
// 1. BMP character filtering
// ---------------------------------------------------------------------

#[test]
fn bmp_non_ascii_character_is_detected_when_filter_allows_it() {
    let (input_path, _input_guard) = temp_path("utf16le-kanji-input");
    fs::write(&input_path, utf16le("日本語")).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        8,
        vec![CharacterFilter::Kanji],
        1,
        "utf16le-kanji",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "日本語", "{text}");
}

#[test]
fn bmp_character_rejected_without_a_matching_filter() {
    let (input_path, _input_guard) = temp_path("utf16le-kanji-rejected-input");
    fs::write(&input_path, utf16le("日本語")).unwrap();

    // Only ASCII is allowed; none of "日本語" is ASCII, so nothing should
    // survive even though the bytes are perfectly valid UTF-16LE.
    let text = scan_all_chunks_full_utf16le(
        &input_path,
        8,
        vec![CharacterFilter::Ascii],
        1,
        "utf16le-kanji-rejected",
    );
    assert!(text.is_empty(), "{text}");
}

#[test]
fn multiple_filters_combine_with_or() {
    let (input_path, _input_guard) = temp_path("utf16le-combined-input");
    // Mixes hiragana, kanji, katakana, CJK punctuation, and ASCII in one
    // run -- every character must be covered by *some* selected filter for
    // the whole run to survive as one match.
    fs::write(&input_path, utf16le("こんにちは世界、これはテストです。Hello")).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        8,
        vec![
            CharacterFilter::Hiragana,
            CharacterFilter::Kanji,
            CharacterFilter::Katakana,
            CharacterFilter::CjkPunct,
            CharacterFilter::Ascii,
        ],
        1,
        "utf16le-combined",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(
        lines[0].split('\t').nth(2).unwrap(),
        "こんにちは世界、これはテストです。Hello",
        "{text}"
    );
}

#[test]
fn multiple_filters_missing_one_script_splits_the_run() {
    let (input_path, _input_guard) = temp_path("utf16le-missing-script-input");
    // Same text as above, but Katakana is left out this time: "テスト"
    // (katakana) should NOT bridge the hiragana/kanji text on either side
    // of it, splitting what would otherwise be one run into fragments.
    fs::write(&input_path, utf16le("ひらがなテストひらがな")).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        8,
        vec![CharacterFilter::Hiragana],
        1,
        "utf16le-missing-script",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "ひらがな", "{text}");
    assert_eq!(lines[1].split('\t').nth(2).unwrap(), "ひらがな", "{text}");
}

#[test]
fn latin1_supplement_characters_are_detected() {
    let (input_path, _input_guard) = temp_path("utf16le-latin1-input");
    fs::write(&input_path, utf16le("café")).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        8,
        vec![CharacterFilter::Ascii, CharacterFilter::Latin1],
        1,
        "utf16le-latin1",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "café", "{text}");
}

// ---------------------------------------------------------------------
// 2. Astral (surrogate-pair) decoding
// ---------------------------------------------------------------------

#[test]
fn astral_character_via_surrogate_pair_is_decoded_when_filter_allows_it() {
    // U+20000, a real CJK Unified Ideographs Extension B character --
    // requires a surrogate pair (high D840, low DC00) to represent in
    // UTF-16LE.
    let (input_path, _input_guard) = temp_path("utf16le-astral-input");
    fs::write(&input_path, utf16le("\u{20000}")).unwrap();

    let text =
        scan_all_chunks_full_utf16le(&input_path, 8, vec![CharacterFilter::KanjiExtB], 1, "utf16le-astral");
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "\u{20000}", "{text}");
}

#[test]
fn astral_character_rejected_without_kanji_ext_b() {
    // Same input as above, but with only BMP `Kanji` selected -- the
    // surrogate pair is still structurally valid UTF-16LE (this isn't
    // about decoding failing), it's just that no selected filter admits
    // the astral scalar it decodes to.
    let (input_path, _input_guard) = temp_path("utf16le-astral-rejected-input");
    fs::write(&input_path, utf16le("\u{20000}")).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        8,
        vec![CharacterFilter::Kanji],
        1,
        "utf16le-astral-rejected",
    );
    assert!(text.is_empty(), "{text}");
}

#[test]
fn astral_character_is_never_matched_by_default_bmp_only_filters() {
    // Documents the current, intentional behavior described in `scan`'s
    // doc comment: none of the BMP-only filters admit any astral scalar,
    // so a run mixing BMP and astral characters only ever reports the BMP
    // portion.
    let (input_path, _input_guard) = temp_path("utf16le-astral-mixed-input");
    let mut s = String::from("東");
    s.push('\u{20000}');
    s.push_str("京");
    fs::write(&input_path, utf16le(&s)).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        8,
        vec![CharacterFilter::Kanji],
        1,
        "utf16le-astral-mixed",
    );
    let lines: Vec<_> = text.lines().collect();
    // The astral character in the middle is unmatched, splitting "東" and
    // "京" into two separate single-character runs (each below the
    // default min_cch=1 threshold is fine here since min_cch=1).
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "東", "{text}");
    assert_eq!(lines[1].split('\t').nth(2).unwrap(), "京", "{text}");
}

#[test]
fn cb_and_cch_account_for_astral_characters_correctly() {
    // A 4-byte-per-character astral run's cb (source byte count) must be
    // 4x its cch (character count), unlike a BMP run where cb is 2x cch.
    let (input_path, _input_guard) = temp_path("utf16le-astral-len-input");
    fs::write(&input_path, utf16le("\u{20000}\u{20001}\u{20002}")).unwrap();

    let file = std::fs::File::open(&input_path).unwrap();
    let cfg_filter = vec![CharacterFilter::KanjiExtB];
    let cfg = crate::config::Config::new(
        vec![],
        cfg_filter,
        1,
        1,
        8,
        false,
        None,
        false,
    );
    let chunk = Chunk { offset: 0, len: 12 };
    let (_records, mut result_file) =
        utf16le::scan(&file, 12, &chunk, &cfg, &input_path, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].cch, 3);
    assert_eq!(records[0].cb, 12); // 3 characters * 4 bytes each
    assert_eq!(records[0].data.text_of(), "\u{20000}\u{20001}\u{20002}");
}

#[test]
fn lone_high_surrogate_is_rejected_and_does_not_desync_parity() {
    // A high surrogate (D800) with no low surrogate following it, sitting
    // between two real BMP characters. It must be skipped without
    // corrupting recognition of the character right after it.
    let (input_path, _input_guard) = temp_path("utf16le-lone-high-input");
    let mut bytes = utf16le("木");
    bytes.extend_from_slice(&0xD800u16.to_le_bytes()); // lone high surrogate
    bytes.extend_from_slice(&utf16le("林"));
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        16,
        vec![CharacterFilter::Kanji],
        1,
        "utf16le-lone-high",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "木", "{text}");
    assert_eq!(lines[1].split('\t').nth(2).unwrap(), "林", "{text}");
}

#[test]
fn lone_low_surrogate_is_rejected() {
    let (input_path, _input_guard) = temp_path("utf16le-lone-low-input");
    let mut bytes = utf16le("木");
    bytes.extend_from_slice(&0xDC00u16.to_le_bytes()); // lone low surrogate
    bytes.extend_from_slice(&utf16le("林"));
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        16,
        vec![CharacterFilter::Kanji],
        1,
        "utf16le-lone-low",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "木", "{text}");
    assert_eq!(lines[1].split('\t').nth(2).unwrap(), "林", "{text}");
}

#[test]
fn mismatched_surrogate_pair_is_rejected() {
    // A high surrogate immediately followed by *another* high surrogate
    // (not a valid low surrogate) must not be accepted as a pair.
    let (input_path, _input_guard) = temp_path("utf16le-mismatched-surrogate-input");
    let mut bytes = utf16le("木");
    bytes.extend_from_slice(&0xD800u16.to_le_bytes());
    bytes.extend_from_slice(&0xD801u16.to_le_bytes());
    bytes.extend_from_slice(&utf16le("林"));
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        16,
        vec![CharacterFilter::Kanji],
        1,
        "utf16le-mismatched-surrogate",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "木", "{text}");
    assert_eq!(lines[1].split('\t').nth(2).unwrap(), "林", "{text}");
}

// ---------------------------------------------------------------------
// 3. Dual-parity overlap resolution
// ---------------------------------------------------------------------

#[test]
fn ascii_misread_as_kanji_at_wrong_parity_is_discarded() {
    // The exact failure mode documented on `scan`: real ASCII text,
    // misread at the wrong parity, decodes into a same-length run of
    // (mostly) valid Kanji-range code units. With both `Ascii` and
    // `Kanji` selected, only the genuine ASCII run should survive.
    let (input_path, _input_guard) = temp_path("utf16le-parity-noise-input");
    fs::write(&input_path, utf16le("HELLO WORLD")).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        32,
        vec![CharacterFilter::Ascii, CharacterFilter::Kanji],
        1,
        "utf16le-parity-noise",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "HELLO WORLD", "{text}");
}

#[test]
fn genuinely_independent_non_overlapping_matches_at_both_parities_both_survive() {
    // Two short, non-overlapping matches -- one only visible at even
    // parity, one only visible at odd parity, positioned so their byte
    // ranges don't overlap at all. Overlap resolution must not remove
    // either one just because they came from different parities.
    let (input_path, _input_guard) = temp_path("utf16le-independent-parities-input");
    let mut bytes = utf16le("AB"); // even parity, offset 0-3
    bytes.push(0); // one extra byte to shift what follows to odd parity
    bytes.extend_from_slice(&utf16le("CD")); // odd parity, offset 5-8
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        32,
        vec![CharacterFilter::Ascii],
        1,
        "utf16le-independent-parities",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    let data: Vec<&str> = lines.iter().map(|l| l.split('\t').nth(2).unwrap()).collect();
    assert!(data.contains(&"AB"), "{text}");
    assert!(data.contains(&"CD"), "{text}");
}

#[test]
fn astral_run_beats_the_wrong_parity_misreading_of_the_same_bytes() {
    // Regression test for `prefer_a_over_b` comparing `cch` instead of
    // `cb`. A run of astral characters spends four bytes per character,
    // while the wrong-parity misreading of those same bytes decodes them
    // as twice as many BMP code units -- so on a character count the
    // spurious run scores double and wins, discarding the genuine one.
    //
    // Only reproducible with a filter wide enough for the misreading to
    // form a run at all, which is why it went unnoticed until `Printable`
    // was added: the misread units here land outside every script filter.
    let (input_path, _input_guard) = temp_path("utf16le-astral-parity-input");
    fs::write(&input_path, utf16le("𠀋𠀌𠀍𠀎𠀏")).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        32,
        vec![CharacterFilter::Printable],
        3,
        "utf16le-astral-parity",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "𠀋𠀌𠀍𠀎𠀏", "{text}");
}

// ---------------------------------------------------------------------
// 4. Chunk / read-block boundary correctness
// ---------------------------------------------------------------------

#[test]
fn astral_character_straddling_a_chunk_boundary_is_reassembled() {
    // Positions a surrogate pair so its high half is the very last code
    // unit of chunk 0 (offsets 6-7) and its low half is the first code
    // unit of chunk 1 (offsets 8-9). This is precisely the case
    // `read_scan_block`'s trailing-high-surrogate carry-over exists for:
    // chunk 0's nominal read stops at offset 8, so without that extra
    // 2-byte read the pair could never be decoded by either chunk (chunk
    // 0 would see a dangling high surrogate, chunk 1 a lone low one).
    //
    // Note the padding is *even*-length so the pair sits at even parity:
    // an odd-length padding would put it at odd parity, where chunk 0's
    // even-parity scan would additionally read `00 <high-lo-byte>` as a
    // separate BMP code unit and muddy what this test is checking.
    let (input_path, _input_guard) = temp_path("utf16le-astral-chunk-boundary-input");
    let mut bytes = vec![0u8; 6]; // padding, offsets 0-5 (non-matching)
    bytes.extend_from_slice(&0xD840u16.to_le_bytes()); // offsets 6-7: high surrogate, chunk 0's last unit
    bytes.extend_from_slice(&0xDC00u16.to_le_bytes()); // offsets 8-9: low surrogate, chunk 1's first unit
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        8,
        vec![CharacterFilter::Kanji, CharacterFilter::KanjiExtB],
        1,
        "utf16le-astral-chunk-boundary",
    );
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    // Reported at the offset of the *high* surrogate, and counted as one
    // 4-byte character rather than two 2-byte ones.
    assert!(lines[0].starts_with("00000000000000000006\t"), "{text}");
    assert!(
        lines.iter().any(|l| l.contains("\tUTF16LE\t\u{20000}")),
        "{text}"
    );
}

#[test]
fn bmp_character_straddling_a_chunk_boundary_is_reassembled() {
    // A BMP run at *odd* parity that begins in chunk 0 and continues into
    // chunk 1, with one of its characters (府) split across the boundary:
    // its low byte is chunk 0's last byte (offset 7) and its high byte is
    // chunk 1's first (offset 8).
    //
    // The run is deliberately several characters long rather than one or
    // two. Scanning the even parity over the same bytes also produces
    // matches -- `00 71` reads as U+7100, `90 9C` as U+9C90, both inside
    // `Kanji`'s range -- and `resolve_parity_overlap` picks between the
    // two parities by run length. Those spurious even-parity runs are
    // necessarily short (each is an isolated accident, not a coherent
    // stream), so a genuine run of length 4 wins cleanly, whereas a
    // one-character genuine run would tie and lose to the even parity on
    // the tie-break. That's the documented dual-parity limitation, not
    // what this test is about.
    let (input_path, _input_guard) = temp_path("utf16le-bmp-chunk-boundary-input");
    let mut bytes = vec![0u8; 1]; // single byte of padding, so the run starts at odd offset 1
    bytes.extend_from_slice(&utf16le("東京都府県")); // offsets 1-10; 府 straddles the chunk-8 boundary
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        8,
        vec![CharacterFilter::Kanji],
        1,
        "utf16le-bmp-chunk-boundary",
    );
    let lines: Vec<_> = text.lines().collect();
    assert!(
        lines.iter().any(|l| l.contains("\tUTF16LE\t東京都府県")),
        "{text}"
    );
}

#[test]
fn astral_run_spans_multiple_read_blocks() {
    // Forces scan_parity's block-reading loop (batched to roughly
    // READ_BUFFER_SIZE) to execute multiple iterations, with the run
    // continuing right across at least one block boundary. This is the
    // scenario `read_scan_block`'s high-surrogate carry-over exists for:
    // without it, whichever astral character happens to land on a block
    // boundary would be incorrectly split.
    let (input_path, _input_guard) = temp_path("utf16le-astral-multiblock-input");
    let run_chars = (READ_BUFFER_SIZE / 4) * 2 + 137; // several blocks' worth of 4-byte astral chars
    let mut bytes = vec![0u8; 2]; // leading non-match code unit
    for _ in 0..run_chars {
        bytes.extend_from_slice(&utf16le("\u{20000}"));
    }
    bytes.extend_from_slice(&[0, 0]); // trailing non-match code unit
    fs::write(&input_path, &bytes).unwrap();

    let file = std::fs::File::open(&input_path).unwrap();
    let file_len = fs::metadata(&input_path).unwrap().len();
    let cfg = crate::config::Config::new(
        vec![],
        vec![CharacterFilter::KanjiExtB],
        1,
        1,
        file_len,
        false,
        None,
        false,
    );
    let (out, _out_guard) = temp_path("utf16le-astral-multiblock-output");
    let chunk = Chunk { offset: 0, len: file_len };
    let (_records, mut result_file) =
        utf16le::scan(&file, file_len, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "expected one run, found {}", records.len());
    assert_eq!(records[0].offset, 2);
    assert_eq!(records[0].cch, run_chars as u64);
    assert_eq!(records[0].cb, run_chars as u64 * 4);
    assert!(records[0].data.text_of().chars().all(|c| c == '\u{20000}'));
}
