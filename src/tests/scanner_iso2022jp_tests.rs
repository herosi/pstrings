//! Tests for `scanner::iso2022jp`.
//!
//! # What went wrong before, and what these tests are guarding
//!
//! The previous implementation could not find a single string. It fed
//! `encoding_rs` one byte at a time and only built a candidate when a call
//! produced output -- but `encoding_rs`'s ISO-2022-JP decoder buffers
//! internally and returns nothing for a one-byte call, so the "produced
//! output" branch was never taken and no candidate was ever created.
//!
//! Two things follow for the tests here. First, most of the old unit tests
//! passed anyway, because they exercised `encoding_rs` directly (asserting
//! that `ISO_2022_JP.decode` decodes ISO-2022-JP) or poked at a state enum
//! in isolation -- neither of which involves the scanner. Tests that can
//! pass while the module under test finds nothing at all are not testing
//! the module, so they are gone.
//!
//! Second, the two tests that *did* fail were the only two that ran actual
//! input through `scan`. Those are kept, and are joined here by a much
//! wider set that drives real input through the real scanner and merger,
//! including a full chunk-size sweep -- the property that catches boundary
//! bugs.

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::scanner::iso2022jp::*;
    use crate::tests::support::{scan_all_chunks_iso2022jp, temp_path};

    /// `ESC $ B` + the JIS X 0208 bytes for 日本.
    const KANJI_NIHON: &[u8] = b"\x1b$B\x46\x7c\x4b\x5c";

    // ---------------------------------------------------------------
    // Escape-sequence recognition
    // ---------------------------------------------------------------

    #[test]
    fn recognizes_every_supported_designation() {
        for (bytes, want) in [
            (&b"\x1b(B"[..], Mode::Ascii),
            (&b"\x1b(J"[..], Mode::Roman),
            (&b"\x1b(I"[..], Mode::Katakana),
            (&b"\x1b$@"[..], Mode::Kanji),
            (&b"\x1b$B"[..], Mode::Kanji),
        ] {
            assert_eq!(
                read_escape(bytes),
                EscapeStep::Complete { mode: want, len: 3 },
                "{bytes:02x?}"
            );
        }
    }

    /// These three are real ISO-2022-JP-family sequences that `encoding_rs`
    /// nonetheless rejects. The scanner must reject them too: accepting a
    /// designation the final decode step refuses would let the scanner
    /// build a run it cannot then decode.
    #[test]
    fn rejects_designations_encoding_rs_rejects() {
        for bytes in [&b"\x1b(H"[..], &b"\x1b$A"[..], &b"\x1b$("[..]] {
            assert_eq!(read_escape(bytes), EscapeStep::Invalid, "{bytes:02x?}");
        }
    }

    #[test]
    fn a_truncated_escape_is_incomplete_not_invalid() {
        assert_eq!(read_escape(b"\x1b"), EscapeStep::Incomplete);
        assert_eq!(read_escape(b"\x1b$"), EscapeStep::Incomplete);
        assert_eq!(read_escape(b"\x1b("), EscapeStep::Incomplete);
    }

    // ---------------------------------------------------------------
    // Per-mode character decoding
    // ---------------------------------------------------------------

    #[test]
    fn roman_mode_substitutes_yen_and_overline() {
        assert_eq!(decode_char(Mode::Roman, b"\x5c"), Step::Complete { ch: '\u{00a5}', len: 1 });
        assert_eq!(decode_char(Mode::Roman, b"\x7e"), Step::Complete { ch: '\u{203e}', len: 1 });
        // Everything else in range stays plain ASCII.
        assert_eq!(decode_char(Mode::Roman, b"A"), Step::Complete { ch: 'A', len: 1 });
    }

    #[test]
    fn ascii_mode_rejects_high_bytes_and_controls() {
        assert_eq!(decode_char(Mode::Ascii, b"\x80"), Step::Invalid);
        assert_eq!(decode_char(Mode::Ascii, b"\x00"), Step::Invalid);
        assert_eq!(decode_char(Mode::Ascii, b"\x7f"), Step::Invalid);
        assert_eq!(decode_char(Mode::Ascii, b"\n"), Step::Invalid);
    }

    #[test]
    fn katakana_mode_maps_onto_halfwidth_block() {
        assert_eq!(decode_char(Mode::Katakana, b"\x21"), Step::Complete { ch: '\u{ff61}', len: 1 });
        assert_eq!(decode_char(Mode::Katakana, b"\x5f"), Step::Complete { ch: '\u{ff9f}', len: 1 });
        // Just outside the range on both sides.
        assert_eq!(decode_char(Mode::Katakana, b"\x20"), Step::Invalid);
        assert_eq!(decode_char(Mode::Katakana, b"\x60"), Step::Invalid);
    }

    #[test]
    fn kanji_mode_consumes_two_bytes() {
        assert_eq!(decode_char(Mode::Kanji, b"\x46\x7c"), Step::Complete { ch: '日', len: 2 });
    }

    #[test]
    fn a_lone_kanji_lead_byte_is_incomplete() {
        assert_eq!(decode_char(Mode::Kanji, b"\x46"), Step::Incomplete);
    }

    /// Structural range checks alone are not enough -- `0x22 0x2F` is
    /// in-range on both bytes but unassigned, and `encoding_rs` errors on
    /// it. This is the check that keeps the scanner's notion of validity
    /// and the decoder's from drifting apart.
    #[test]
    fn structurally_valid_but_unassigned_kanji_pairs_are_rejected() {
        assert!(!is_defined_jis_pair(0x22, 0x2f));
        assert_eq!(decode_char(Mode::Kanji, b"\x22\x2f"), Step::Invalid);

        assert!(is_defined_jis_pair(0x46, 0x7c));
    }

    #[test]
    fn only_seven_bit_bytes_can_belong_to_a_run() {
        assert!(is_iso2022jp_byte(0x1b));
        assert!(is_iso2022jp_byte(b'\t'));
        assert!(is_iso2022jp_byte(b' '));
        assert!(is_iso2022jp_byte(0x7e));

        assert!(!is_iso2022jp_byte(0x00));
        assert!(!is_iso2022jp_byte(b'\n'));
        assert!(!is_iso2022jp_byte(0x7f));
        assert!(!is_iso2022jp_byte(0x80));
        assert!(!is_iso2022jp_byte(0xff));
    }

    // ---------------------------------------------------------------
    // segment_raw: the shared byte-to-character step
    // ---------------------------------------------------------------

    #[test]
    fn segment_raw_decodes_a_mixed_run_as_one_fragment() {
        let mut input = b"AB".to_vec();
        input.extend_from_slice(KANJI_NIHON);
        input.extend_from_slice(b"\x1b(BCD");

        let (fragments, tail) = segment_raw(&input);

        assert!(tail.is_empty(), "{tail:02x?}");
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].data, "AB日本CD");
        assert_eq!(fragments[0].cch, 6);
        assert_eq!(fragments[0].start, 0);
        assert_eq!(fragments[0].cb, input.len() as u64);
    }

    /// A designation sequence that ends the string designates nothing and
    /// must not be counted as part of it -- otherwise `cb` claims three
    /// bytes past the last character.
    #[test]
    fn a_trailing_escape_is_excluded_from_the_run() {
        let mut input = b"AB".to_vec();
        input.extend_from_slice(KANJI_NIHON);
        input.extend_from_slice(b"\x1b(B");

        let (fragments, tail) = segment_raw(&input);

        assert!(tail.is_empty());
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].data, "AB日本");
        // 2 ASCII + ESC$B + 4 kanji bytes = 9, with the trailing ESC(B excluded.
        assert_eq!(fragments[0].cb, 9);
    }

    /// A leading designation, by contrast, *is* part of the run: it is what
    /// makes the run's byte span self-contained and independently decodable.
    #[test]
    fn a_leading_escape_is_included_in_the_run() {
        let (fragments, _) = segment_raw(KANJI_NIHON);

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].start, 0);
        assert_eq!(fragments[0].cb, KANJI_NIHON.len() as u64);
        assert_eq!(fragments[0].data, "日本");
    }

    #[test]
    fn segment_raw_splits_at_a_byte_that_cannot_belong_to_any_run() {
        let (fragments, tail) = segment_raw(b"ABCD\x00EFGH");

        assert!(tail.is_empty());
        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].data, "ABCD");
        assert_eq!(fragments[1].data, "EFGH");
        assert_eq!(fragments[1].start, 5);
    }

    #[test]
    fn segment_raw_returns_an_undecidable_tail() {
        // Ends on a lone kanji lead byte.
        let (fragments, tail) = segment_raw(b"\x1b$B\x46\x7c\x4b");
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].data, "日");
        assert_eq!(tail, b"\x4b");

        // Ends mid-escape.
        let (_, tail) = segment_raw(b"AB\x1b$");
        assert_eq!(tail, b"\x1b$");
    }

    /// Mode is a property of a run, not of the file: once a run ends, the
    /// next one starts from ASCII again. This is what removes the old
    /// implementation's need to scan backward through the file for the
    /// most recent escape.
    #[test]
    fn mode_does_not_leak_across_a_run_boundary() {
        // ESC $ B, 日, then a NUL that ends the run, then bytes which
        // would decode as kanji if the mode had persisted.
        let mut input = KANJI_NIHON[..5].to_vec();
        input.push(0x00);
        input.extend_from_slice(b"\x4b\x5c");

        let (fragments, _) = segment_raw(&input);

        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].data, "日");
        // Read as ASCII, not as 本.
        assert_eq!(fragments[1].data, "K\\");
    }

    // ---------------------------------------------------------------
    // End-to-end, through the real scanner and merger
    // ---------------------------------------------------------------

    fn write_temp(name: &str, bytes: &[u8]) -> (std::path::PathBuf, tempfile::TempDir) {
        let (path, guard) = temp_path(name);
        fs::write(&path, bytes).unwrap();
        (path, guard)
    }

    fn mixed_sample() -> Vec<u8> {
        let mut input = b"AB".to_vec();
        input.extend_from_slice(KANJI_NIHON);
        input.extend_from_slice(b"\x1b(B");
        input.extend_from_slice(b"CD");
        input
    }

    #[test]
    fn kanji_and_ascii_in_a_single_chunk() {
        let input = mixed_sample();
        let (path, _guard) = write_temp("iso2022jp-single", &input);

        let text = scan_all_chunks_iso2022jp(&path, input.len() as u64, 1, "iso-single");

        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");

        let fields: Vec<_> = lines[0].split('\t').collect();
        assert_eq!(fields[0].parse::<u64>().unwrap(), 0);
        assert_eq!(fields[1], "ISO2022JP");
        assert_eq!(fields[2], "AB日本CD");
    }

    #[test]
    fn kanji_and_ascii_across_a_chunk_boundary() {
        let input = mixed_sample();
        let (path, _guard) = write_temp("iso2022jp-boundary", &input);

        let text = scan_all_chunks_iso2022jp(&path, 1, 1, "iso-boundary");

        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(lines[0].split('\t').nth(2).unwrap(), "AB日本CD", "{text}");
    }

    /// The central property: `--chunk-size` is a performance knob and must
    /// never change what is found. Every size from 1 byte up to past the
    /// end of the file has to agree, which is what exercises boundaries
    /// falling inside escape sequences and inside kanji pairs.
    fn assert_chunk_size_invariant(bytes: &[u8], tag: &str) {
        let (path, _guard) = write_temp(&format!("iso-inv-{tag}"), bytes);
        let full = bytes.len() as u64;

        let reference = scan_all_chunks_iso2022jp(&path, full, 1, &format!("{tag}-ref"));

        let sizes: Vec<u64> = (1..=16u64).chain([24, 32, 64, full, full + 1, full * 2]).collect();
        for size in sizes {
            if size == 0 {
                continue;
            }
            let got = scan_all_chunks_iso2022jp(&path, size, 1, &format!("{tag}-{size}"));
            assert_eq!(
                got, reference,
                "chunk_size={size} disagreed with the single-chunk result for {tag}"
            );
        }
    }

    #[test]
    fn results_are_independent_of_chunk_size_for_mixed_text() {
        assert_chunk_size_invariant(&mixed_sample(), "mixed");
    }

    #[test]
    fn results_are_independent_of_chunk_size_for_pure_kanji() {
        assert_chunk_size_invariant(KANJI_NIHON, "kanji");
    }

    #[test]
    fn results_are_independent_of_chunk_size_with_every_mode() {
        let mut input = b"start".to_vec();
        input.extend_from_slice(b"\x1b(J\x5c\x7e");
        input.extend_from_slice(b"\x1b(I\x31\x32");
        input.extend_from_slice(KANJI_NIHON);
        input.extend_from_slice(b"\x1b(Bend");
        assert_chunk_size_invariant(&input, "allmodes");
    }

    #[test]
    fn results_are_independent_of_chunk_size_when_buried_in_binary() {
        let mut input = vec![0x00, 0xff, 0x80, 0x01];
        input.extend_from_slice(&mixed_sample());
        input.extend_from_slice(&[0x00, 0xfe]);
        input.extend_from_slice(b"\x1b$B\x4b\x5c\x1b(B");
        input.extend_from_slice(&[0x00, 0x90]);
        assert_chunk_size_invariant(&input, "binary");
    }

    /// A match that runs to the very end of the file, with no terminating
    /// byte to close it. This is the shape that hid a whole-file data-loss
    /// bug in the CP932 scanner, so it is checked explicitly here.
    #[test]
    fn a_match_reaching_end_of_file_is_reported() {
        let input = mixed_sample();
        let (path, _guard) = write_temp("iso2022jp-eof", &input);

        for size in [1u64, 2, 3, 4, 5, 7, 64] {
            let text = scan_all_chunks_iso2022jp(&path, size, 1, &format!("iso-eof-{size}"));
            assert_eq!(
                text.lines().next().and_then(|l| l.split('\t').nth(2)),
                Some("AB日本CD"),
                "chunk_size={size}: {text}"
            );
        }
    }

    #[test]
    fn independent_strings_stay_separate() {
        let mut input = b"FIRST".to_vec();
        input.push(0x00);
        input.extend_from_slice(b"SECOND");

        let (path, _guard) = write_temp("iso2022jp-separate", &input);
        let text = scan_all_chunks_iso2022jp(&path, input.len() as u64, 1, "iso-sep");

        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0].split('\t').nth(2).unwrap(), "FIRST");
        assert_eq!(lines[1].split('\t').nth(2).unwrap(), "SECOND");
    }

    /// The reported offset must point at where the run actually begins in
    /// the file -- including its leading designation sequence.
    #[test]
    fn the_reported_offset_includes_a_leading_escape() {
        let mut input = vec![0x00, 0x00];
        input.extend_from_slice(KANJI_NIHON);
        input.push(0x00);

        let (path, _guard) = write_temp("iso2022jp-offset", &input);
        let text = scan_all_chunks_iso2022jp(&path, input.len() as u64, 1, "iso-offset");

        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");

        let fields: Vec<_> = lines[0].split('\t').collect();
        assert_eq!(fields[0].parse::<u64>().unwrap(), 2, "{text}");
        assert_eq!(fields[2], "日本");
    }

    #[test]
    fn min_length_is_applied_to_the_decoded_character_count() {
        let mut input = b"AB".to_vec();
        input.push(0x00);
        input.extend_from_slice(b"LONGER");

        let (path, _guard) = write_temp("iso2022jp-minlen", &input);
        let text = scan_all_chunks_iso2022jp(&path, input.len() as u64, 4, "iso-minlen");

        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(lines[0].split('\t').nth(2).unwrap(), "LONGER");
    }
}
