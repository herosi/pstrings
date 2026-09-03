//! Tests for `CharacterFilter::Latin1` (`filter::latin1`) and for the
//! scanner behavior it exposes.
//!
//! `Latin1` was added after the other filters and is the first one to
//! admit *non-ASCII single bytes* (0xA0-0xFF). That is more disruptive
//! than it sounds: the byte-oriented scanners (`scanner::ascii`,
//! `scanner::utf16le_ascii`) were written when "a matched byte" and "a
//! valid standalone UTF-8 byte" were the same thing, and both accumulated
//! matched bytes raw before validating the whole run with
//! `String::from_utf8`. With `Latin1` selected that validation fails, so
//! both scanners panicked on any input containing 0xA0-0xFF. Both now
//! accumulate decoded `char`s instead (byte N -> U+00N, the ISO-8859-1
//! mapping), and the scanner tests below are the regression tests for
//! that.
//!
//! Organized in three groups:
//! 1. The filter predicates themselves, at their range boundaries.
//! 2. `FilterSet`'s compiled tables agreeing with those predicates.
//! 3. The scanners, end-to-end, with `Latin1` selected.

use super::support::*;
use crate::chunk::Chunk;
use crate::filter::{self, CharacterFilter, FilterSet};
use std::fs;
use std::sync::atomic::AtomicBool;

// ---------------------------------------------------------------------
// 1. Filter predicates
// ---------------------------------------------------------------------

/// The exact boundaries of the Latin-1 supplement range, tested from both
/// sides. 0xA0 (NBSP) and 0xFF (ÿ) are the first and last accepted values;
/// 0x9F is the last rejected one below the range, and there is nothing
/// above 0xFF for `u8` to reject.
#[test]
fn latin1_u8_range_boundaries() {
    let f = [CharacterFilter::Latin1];

    assert!(!filter::allows_u8(&f, 0x9F), "0x9F is a C1 control, not Latin-1 supplement");
    assert!(filter::allows_u8(&f, 0xA0), "0xA0 (NBSP) starts the range");
    assert!(filter::allows_u8(&f, 0xE9), "0xE9 (e-acute) is mid-range");
    assert!(filter::allows_u8(&f, 0xFF), "0xFF (y-diaeresis) ends the range");
}

/// The C1 control block (0x80-0x9F) sits between ASCII and the Latin-1
/// supplement and is deliberately *not* matched: those are control codes,
/// and admitting them would make `Latin1` fire on essentially any dense
/// binary data.
#[test]
fn latin1_rejects_c1_controls() {
    let f = [CharacterFilter::Latin1];
    for b in 0x80u8..=0x9F {
        assert!(!filter::allows_u8(&f, b), "0x{b:02X} is a C1 control and must be rejected");
    }
}

/// `Latin1` alone must not admit ASCII -- that is `Ascii`'s job. The two
/// are separate filters precisely so "printable ASCII" and "Latin-1
/// accented letters" can be selected independently.
#[test]
fn latin1_alone_does_not_admit_ascii() {
    let f = [CharacterFilter::Latin1];

    assert!(!filter::allows_u8(&f, b'A'));
    assert!(!filter::allows_u8(&f, b' '));
    assert!(!filter::allows_u8(&f, b'\t'));
    assert!(!filter::allows_u16(&f, u16::from(b'A')));
    assert!(!filter::allows_char(&f, 'A'));
}

/// The `u16` and `char` predicates must agree with the `u8` one over the
/// whole byte range: the same character is the same character regardless
/// of which width the scanner happens to be looking at it through.
#[test]
fn latin1_predicates_agree_across_widths() {
    let f = [CharacterFilter::Latin1];
    for b in 0u8..=u8::MAX {
        let by_u8 = filter::allows_u8(&f, b);
        let by_u16 = filter::allows_u16(&f, u16::from(b));
        let by_char = filter::allows_char(&f, b as char);
        assert_eq!(by_u8, by_u16, "0x{b:02X}: allows_u8 vs allows_u16 disagree");
        assert_eq!(by_u8, by_char, "0x{b:02X}: allows_u8 vs allows_char disagree");
    }
}

/// Nothing above U+00FF belongs to `Latin1`. In particular U+0100 (the
/// very next code point) must be rejected, as must anything astral --
/// `Latin1` must not make `FilterSet::has_astral` true.
#[test]
fn latin1_matches_nothing_above_u00ff() {
    let f = [CharacterFilter::Latin1];

    assert!(!filter::allows_u16(&f, 0x0100));
    assert!(!filter::allows_u16(&f, 0x4E00)); // kanji
    assert!(!filter::allows_char(&f, '\u{20000}')); // astral
}

/// Combining `Latin1` with `Ascii` is the practically useful case (Western
/// European text is a mix of both), and filters combine with OR, so the
/// union must admit exactly the union of the two ranges.
#[test]
fn ascii_and_latin1_combine_to_cover_both_ranges() {
    let f = [CharacterFilter::Ascii, CharacterFilter::Latin1];

    assert!(filter::allows_u8(&f, b'A'));
    assert!(filter::allows_u8(&f, 0xE9));
    // The gap between the two ranges is still a gap.
    assert!(!filter::allows_u8(&f, 0x7F)); // DEL
    assert!(!filter::allows_u8(&f, 0x80));
    assert!(!filter::allows_u8(&f, 0x9F));
}

// ---------------------------------------------------------------------
// 2. FilterSet's compiled tables
// ---------------------------------------------------------------------

/// `FilterSet` exists to answer the same questions as the free functions,
/// only faster, by precomputing bitsets. If the two ever disagree the
/// bitset build is wrong, so this checks them against each other
/// exhaustively over every byte and every BMP code point -- which is
/// cheap, and is the only way to catch an off-by-one in the bit indexing
/// that happens to fall outside whatever values the other tests sample.
#[test]
fn filterset_tables_match_the_predicates_for_latin1() {
    for filters in [
        vec![CharacterFilter::Latin1],
        vec![CharacterFilter::Ascii, CharacterFilter::Latin1],
    ] {
        let set = FilterSet::new(filters.clone());

        for b in 0u8..=u8::MAX {
            assert_eq!(
                set.allows_u8(b),
                filter::allows_u8(&filters, b),
                "byte 0x{b:02X} with {filters:?}"
            );
        }
        for u in 0u16..=u16::MAX {
            assert_eq!(
                set.allows_u16(u),
                filter::allows_u16(&filters, u),
                "u+{u:04X} with {filters:?}"
            );
        }
    }
}

/// `Latin1` is BMP-only, so `FilterSet`'s astral fast-rejection path must
/// reject every astral character outright.
#[test]
fn filterset_rejects_astral_for_latin1() {
    let set = FilterSet::new(vec![CharacterFilter::Ascii, CharacterFilter::Latin1]);

    assert!(!set.allows_char('\u{10000}'));
    assert!(!set.allows_char('\u{20000}'));
    assert!(!set.allows_char('\u{10FFFF}'));
}

// ---------------------------------------------------------------------
// 3. Scanners, end-to-end
// ---------------------------------------------------------------------

/// REGRESSION: `scanner::ascii` accumulated matched bytes into a
/// `Vec<u8>` and then called `String::from_utf8(..).expect(..)`. Latin-1
/// bytes are not valid standalone UTF-8, so this panicked outright for
/// any input containing them. The run must now come through intact, with
/// each byte decoded as its ISO-8859-1 character.
#[test]
fn ascii_scanner_handles_latin1_bytes_without_panicking() {
    let (input_path, _input_guard) = temp_path("latin1-ascii-scanner-input");
    // "caf<0xE9> na<0xEF>ve" in ISO-8859-1: ASCII letters interleaved with
    // two high bytes, so the run genuinely mixes both filters' ranges
    // rather than being purely one or the other.
    let bytes = b"caf\xE9 na\xEFve";
    fs::write(&input_path, bytes).unwrap();

    let file = std::fs::File::open(&input_path).unwrap();
    let cfg = test_config_with_filters(1, 64, vec![CharacterFilter::Ascii, CharacterFilter::Latin1]);
    let (out, _out_guard) = temp_path("latin1-ascii-scanner-output");
    let chunk = Chunk { offset: 0, len: bytes.len() as u64 };
    let (_count, mut result_file) =
        crate::scanner::ascii::scan(&file, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].data.text_of(), "café naïve");
    // cb counts *source* bytes (one per character here, since the input is
    // single-byte-encoded), while the emitted text is UTF-8 and therefore
    // longer in bytes. Asserting cb explicitly pins down that the
    // byte-to-char change didn't quietly redefine it.
    assert_eq!(records[0].cb, bytes.len() as u64);
    assert_eq!(records[0].cch, 10);
}

/// Without `Latin1` selected, the high bytes must *break* the run rather
/// than extend it -- confirming the scanner is genuinely consulting the
/// filter for these bytes and not just passing anything non-control
/// through.
#[test]
fn ascii_scanner_splits_on_latin1_bytes_when_filter_not_selected() {
    let (input_path, _input_guard) = temp_path("latin1-ascii-scanner-split-input");
    let bytes = b"caf\xE9 na\xEFve";
    fs::write(&input_path, bytes).unwrap();

    let file = std::fs::File::open(&input_path).unwrap();
    let cfg = test_config_with_filters(1, 64, vec![CharacterFilter::Ascii]);
    let (out, _out_guard) = temp_path("latin1-ascii-scanner-split-output");
    let chunk = Chunk { offset: 0, len: bytes.len() as u64 };
    let (_count, mut result_file) =
        crate::scanner::ascii::scan(&file, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    let texts: Vec<_> = records.iter().map(|r| r.data.text_of()).collect();
    assert_eq!(texts, vec!["caf", " na", "ve"], "{records:?}");
}

/// REGRESSION: the same latent panic as `ascii_scanner_handles_latin1_
/// bytes_without_panicking`, in `scanner::utf16le_ascii`. That scanner
/// matches on the full `u16` (so U+00E9 is matched by `latin1::
/// allows_u16`) but stored only the low byte, again as raw bytes later
/// validated as UTF-8.
#[test]
fn utf16le_ascii_scanner_handles_latin1_characters_without_panicking() {
    let (input_path, _input_guard) = temp_path("latin1-utf16le-ascii-input");
    let bytes = utf16le("café naïve");
    fs::write(&input_path, &bytes).unwrap();

    let file = std::fs::File::open(&input_path).unwrap();
    let file_len = bytes.len() as u64;
    let cfg = test_config_with_filters(1, 64, vec![CharacterFilter::Ascii, CharacterFilter::Latin1]);
    let (out, _out_guard) = temp_path("latin1-utf16le-ascii-output");
    let chunk = Chunk { offset: 0, len: file_len };
    let (_count, mut result_file) =
        crate::scanner::utf16le_ascii::scan(&file, file_len, &chunk, &cfg, &out, &AtomicBool::new(false))
            .unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].data.text_of(), "café naïve");
    assert_eq!(records[0].cb, file_len); // 10 characters * 2 bytes each
    assert_eq!(records[0].cch, 10);
}

/// The full UTF-16LE scanner reaches Latin-1 characters through its BMP
/// path (`FilterSet::allows_u16`), not the byte path, so it needs its own
/// coverage. Unlike the two scanners above it never had the UTF-8 bug --
/// it always accumulated decoded `char`s -- but this pins the behavior
/// down so a future refactor can't reintroduce it.
#[test]
fn utf16le_full_scanner_matches_latin1_characters() {
    let (input_path, _input_guard) = temp_path("latin1-utf16le-full-input");
    let mut bytes = vec![0u8; 2]; // non-matching lead-in
    bytes.extend_from_slice(&utf16le("àéîõü"));
    bytes.extend_from_slice(&[0, 0]); // non-matching tail
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks_full_utf16le(
        &input_path,
        bytes.len() as u64,
        vec![CharacterFilter::Latin1],
        1,
        "latin1-utf16le-full",
    );

    assert!(text.lines().any(|l| l.contains("\tUTF16LE\tàéîõü")), "{text}");
}

/// A Latin-1 run split across a chunk boundary must be rejoined by the
/// outputter, exactly like an ASCII one. This is worth its own test
/// because Latin-1 is the first filter where one *source* byte becomes
/// two *output* bytes (U+00E0 encodes as 2 bytes of UTF-8), so a run
/// fragment's `cb` and its stored text length now differ -- and the
/// boundary-joining path in `outputter` has to keep those straight.
#[test]
fn latin1_run_straddling_a_chunk_boundary_is_rejoined() {
    let (input_path, _input_guard) = temp_path("latin1-chunk-boundary-input");
    // 12 Latin-1 bytes, so with chunk_size 8 the run is split at offset 8,
    // squarely mid-run.
    let bytes = b"\xE0\xE1\xE2\xE3\xE4\xE5\xE6\xE7\xE8\xE9\xEA\xEB";
    fs::write(&input_path, bytes).unwrap();

    let text = scan_all_chunks_ascii_with_filters(
        &input_path,
        8,
        vec![CharacterFilter::Ascii, CharacterFilter::Latin1],
        1,
        "latin1-chunk-boundary",
    );

    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "run must be rejoined into one record: {text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "àáâãäåæçèéêë", "{text}");
    assert!(lines[0].starts_with("00000000000000000000\t"), "{text}");
}

/// The same input with only `Ascii` selected must produce nothing at all,
/// confirming the high bytes are inert without `Latin1` and that the test
/// above is really measuring the filter rather than some default
/// pass-through behavior.
#[test]
fn latin1_bytes_are_inert_without_the_latin1_filter() {
    let (input_path, _input_guard) = temp_path("latin1-inert-input");
    let bytes = b"\xE0\xE1\xE2\xE3\xE4\xE5\xE6\xE7\xE8\xE9\xEA\xEB";
    fs::write(&input_path, bytes).unwrap();

    let text = scan_all_chunks(&input_path, 8, false, 1, "latin1-inert");
    assert_eq!(text, "", "{text}");
}
