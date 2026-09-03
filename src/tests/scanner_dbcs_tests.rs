//! Tests for the double-byte encodings that share `scanner::dbcs`:
//! GBK, EUC-KR, Big5 and GB18030.
//!
//! # What these are really testing
//!
//! These encodings are structurally the same shape as CP932, so they all
//! share one scanning engine (`scanner::dbcs`) and differ only in their
//! byte-range predicates. That makes two distinct things worth testing,
//! and they need to be kept apart:
//!
//!   * **The engine.** Already covered in depth by
//!     `scanner_cp932_tests.rs`, which exercises the deferred-boundary
//!     path, multi-chunk raw chaining, and end-of-file handling. Those
//!     tests pass unchanged against the shared engine, which is what
//!     establishes that the refactor preserved behaviour -- so there is no
//!     value in transcribing them twice more here.
//!
//!   * **The per-encoding data**, i.e. the lead/trail/single predicates.
//!     That is what this file concentrates on, because it is the only part
//!     that is genuinely new, and because a wrong byte range is exactly
//!     the kind of mistake that produces plausible-looking but subtly
//!     wrong output.
//!
//! The predicate tests below are written as cross-checks against
//! `encoding_rs` rather than as hand-copied range literals. Asserting that
//! `is_lead(0x81)` is true would merely restate the source; asserting that
//! the predicate agrees with the decoder over the entire byte space
//! actually catches a transcription error.
//!
//! GB18030 is the one exception to "data only": it is the sole encoding
//! with a four-byte form, so it is also the only one that exercises a
//! genuinely new *engine* path. Its section at the bottom of this file is
//! correspondingly heavier, and includes the cross-check against GBK that
//! demonstrates the four-byte support did not disturb the two-byte one.

#[cfg(test)]
mod tests {
    use crate::scanner::big5::Big5;
    use crate::scanner::cp932::Cp932;
    use crate::scanner::dbcs::Dbcs;
    use crate::scanner::euckr::EucKr;
    use crate::scanner::gb18030::Gb18030;
    use crate::scanner::gbk::Gbk;
    use crate::tests::support::*;
    use std::fs;

    // ---------------------------------------------------------------
    // Predicate cross-checks against encoding_rs
    // ---------------------------------------------------------------

    /// Every byte that actually begins a pair the decoder accepts must be
    /// recognised as a lead byte -- otherwise the scanner would silently
    /// miss real text. The converse is deliberately *not* required: the
    /// structural predicates are allowed to be slightly permissive,
    /// because `is_defined_seq` re-checks every candidate against the
    /// decoder before it is accepted. Over-admitting a byte costs nothing
    /// but a rejected candidate; under-admitting one loses data.
    ///
    /// `over_admitted` pins down exactly which bytes are permitted despite
    /// beginning no valid pair, so that the slack stays deliberate: if a
    /// future edit widens a range by accident, this still fails.
    fn assert_lead_range_matches_decoder<E: Dbcs>(tag: &str, over_admitted: &[u8]) {
        let mut missing = Vec::new();
        let mut extra = Vec::new();

        for b0 in 0u16..=0xFF {
            let b0 = b0 as u8;

            // A byte that decodes on its own is a standalone character,
            // not a lead byte; the lead predicate says nothing about it.
            let one = [b0];
            if !E::decoder().decode_without_bom_handling(&one).1 {
                continue;
            }

            let begins_a_pair = (0u16..=0xFF).any(|b1| {
                let pair = [b0, b1 as u8];
                !E::decoder().decode_without_bom_handling(&pair).1
            });

            match (E::is_lead(b0), begins_a_pair) {
                (false, true) => missing.push(b0),
                (true, false) => extra.push(b0),
                _ => {}
            }
        }

        assert!(
            missing.is_empty(),
            "{tag}: these bytes begin valid pairs but is_lead rejects them: {missing:02x?}"
        );
        assert_eq!(
            extra, over_admitted,
            "{tag}: the set of bytes admitted as leads despite beginning no valid pair changed"
        );
    }

    #[test]
    fn gbk_lead_range_matches_encoding_rs() {
        // GBK's lead range is exact: every byte in 0x81..=0xFE begins at
        // least one assigned pair, and 0xFF is excluded.
        assert_lead_range_matches_decoder::<Gbk>("GBK", &[]);
    }

    #[test]
    fn euckr_lead_range_matches_encoding_rs() {
        // 0xC9 falls inside the contiguous 0x81..=0xFD lead range but is
        // the user-defined row, which encoding_rs leaves entirely
        // unassigned -- it begins no valid pair at all. It is left in the
        // range because writing the range as a hole ("0x81..=0xC8 |
        // 0xCA..=0xFD") buys nothing: `is_defined_seq` rejects every
        // 0xC9 pair anyway, so the only effect would be a slightly
        // earlier rejection of bytes that are rejected either way.
        assert_lead_range_matches_decoder::<EucKr>("EUC-KR", &[0xC9]);
    }

    #[test]
    fn big5_lead_range_matches_encoding_rs() {
        // Exact: 120 lead bytes spanning 0x87..=0xFE with no gaps.
        assert_lead_range_matches_decoder::<Big5>("Big5", &[]);
    }

    #[test]
    fn gb18030_lead_range_matches_encoding_rs() {
        // Identical to GBK's, and exact: every byte in 0x81..=0xFE begins
        // at least one assigned pair. (The helper only considers two-byte
        // pairs; the four-byte form's first byte range is a subset of
        // this one and is checked separately below.)
        assert_lead_range_matches_decoder::<Gb18030>("GB18030", &[]);
    }

    /// Every pair the predicates accept structurally must either be
    /// accepted by the decoder, or be caught by the assigned-pair check.
    /// The direction that matters is the other one: a pair the decoder
    /// accepts must never be rejected by the structural predicates, or the
    /// scanner would silently miss real text.
    fn assert_no_valid_pair_is_rejected<E: Dbcs>(tag: &str) {
        let mut missed = Vec::new();
        for b0 in 0x80u16..=0xFF {
            for b1 in 0u16..=0xFF {
                let pair = [b0 as u8, b1 as u8];
                if E::decoder().decode_without_bom_handling(&pair).1 {
                    continue; // decoder rejects it; nothing to require
                }
                // The decoder accepts these two bytes. If the lead stands
                // alone this is really two single-byte characters, so the
                // pair predicates don't apply.
                let one = [b0 as u8];
                if !E::decoder().decode_without_bom_handling(&one).1 {
                    continue;
                }
                if !(E::is_lead(b0 as u8) && E::is_trail(b1 as u8)) {
                    missed.push((b0, b1));
                }
            }
        }
        assert!(
            missed.is_empty(),
            "{tag}: {} valid pairs rejected by the structural predicates, e.g. {:02x?}",
            missed.len(),
            &missed[..missed.len().min(8)]
        );
    }

    #[test]
    fn gbk_accepts_every_pair_encoding_rs_accepts() {
        assert_no_valid_pair_is_rejected::<Gbk>("GBK");
    }

    #[test]
    fn euckr_accepts_every_pair_encoding_rs_accepts() {
        assert_no_valid_pair_is_rejected::<EucKr>("EUC-KR");
    }

    #[test]
    fn big5_accepts_every_pair_encoding_rs_accepts() {
        assert_no_valid_pair_is_rejected::<Big5>("Big5");
    }

    #[test]
    fn gb18030_accepts_every_pair_encoding_rs_accepts() {
        assert_no_valid_pair_is_rejected::<Gb18030>("GB18030");
    }

    /// The single-byte and trail-byte roles must not overlap, or a run
    /// could start in the middle of a two-byte sequence. This is why GBK's
    /// `is_single` deliberately excludes 0x80 even though GBK maps it to
    /// the euro sign.
    #[test]
    fn single_byte_and_trail_roles_do_not_overlap_ambiguously() {
        for b in 0u16..=0xFF {
            let b = b as u8;
            if Gbk::is_single(b) {
                assert!(
                    b < 0x80,
                    "GBK: 0x{b:02x} is both a standalone character and a high byte"
                );
            }
            if EucKr::is_single(b) {
                assert!(
                    b < 0x80,
                    "EUC-KR: 0x{b:02x} is both a standalone character and a high byte"
                );
            }
            if Big5::is_single(b) {
                assert!(
                    b < 0x80,
                    "Big5: 0x{b:02x} is both a standalone character and a high byte"
                );
            }
            if Gb18030::is_single(b) {
                assert!(
                    b < 0x80,
                    "GB18030: 0x{b:02x} is both a standalone character and a high byte"
                );
            }
        }
    }

    /// Control bytes must never be treated as text, in any role that could
    /// start a run.
    #[test]
    fn control_bytes_are_never_standalone_characters() {
        for b in [0x00u8, 0x01, 0x0a, 0x0d, 0x1b, 0x7f] {
            assert!(!Gbk::is_single(b), "GBK admitted control byte 0x{b:02x}");
            assert!(!EucKr::is_single(b), "EUC-KR admitted control byte 0x{b:02x}");
            assert!(!Big5::is_single(b), "Big5 admitted control byte 0x{b:02x}");
            assert!(!Gb18030::is_single(b), "GB18030 admitted control byte 0x{b:02x}");
        }
    }

    // ---------------------------------------------------------------
    // Character counting
    // ---------------------------------------------------------------

    /// `dbcs::count_chars` counts Unicode scalars by decoding, rather than
    /// counting encoded sequences. For these encodings the two are the
    /// same, and this pins that they stay the same -- so the change to a
    /// decode-based count (made for BIG5, which does have
    /// one-sequence-to-two-scalar mappings) provably did not alter the
    /// behaviour of the encodings that predate it.
    ///
    /// Exhaustive rather than sampled: a one-off disagreement on a single
    /// rare pair is exactly what a sample would miss, and `cch` feeds
    /// `--min-length`, so being wrong on one pair means silently dropping
    /// or admitting matches containing it.
    fn assert_scalar_count_equals_sequence_count<E: Dbcs>(tag: &str) {
        let mut checked = 0u64;
        let mut mismatches = Vec::new();

        let check = |seq: &[u8], mismatches: &mut Vec<String>| {
            let scalars = E::decoder()
                .decode_without_bom_handling(seq)
                .0
                .chars()
                .count() as u64;
            if scalars != 1 {
                mismatches.push(format!("{seq:02x?} -> {scalars} scalars"));
            }
        };

        for b in 0u16..=0xFF {
            let b = b as u8;
            if E::is_single(b) {
                checked += 1;
                check(&[b], &mut mismatches);
            }
        }

        for b0 in 0u16..=0xFF {
            for b1 in 0u16..=0xFF {
                let (b0, b1) = (b0 as u8, b1 as u8);
                if !E::is_lead(b0) || !E::is_trail(b1) {
                    continue;
                }
                let pair = [b0, b1];
                if E::decoder().decode_without_bom_handling(&pair).1 {
                    continue; // not an assigned pair; never accumulated
                }
                checked += 1;
                check(&pair, &mut mismatches);
            }
        }

        assert!(checked > 1000, "{tag}: suspiciously few sequences checked ({checked})");
        assert!(
            mismatches.is_empty(),
            "{tag}: {} sequences decode to something other than exactly one \
             scalar, so counting sequences and counting scalars disagree: {:?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(8)]
        );
    }

    #[test]
    fn count_chars_agrees_with_sequence_counting_for_simple_encodings() {
        assert_scalar_count_equals_sequence_count::<Cp932>("CP932");
        assert_scalar_count_equals_sequence_count::<Gbk>("GBK");
        assert_scalar_count_equals_sequence_count::<EucKr>("EUC-KR");
        // GB18030's two-byte repertoire is GBK's, so it inherits the same
        // property; its four-byte sequences are checked separately, where
        // measurement showed none of them is multi-scalar either.
        assert_scalar_count_equals_sequence_count::<Gb18030>("GB18030");
    }

    /// Big5 is the exception that motivated counting scalars instead of
    /// sequences. Exactly four sequences decode to two scalars each; this
    /// pins that set, so if a future `encoding_rs` changes it, the
    /// assumption is re-examined rather than silently broken.
    #[test]
    fn big5_has_exactly_four_two_scalar_sequences() {
        let mut found = Vec::new();
        for b0 in 0u16..=0xFF {
            for b1 in 0u16..=0xFF {
                let (b0, b1) = (b0 as u8, b1 as u8);
                if !Big5::is_lead(b0) || !Big5::is_trail(b1) {
                    continue;
                }
                let pair = [b0, b1];
                let (decoded, had_errors) = Big5::decoder().decode_without_bom_handling(&pair);
                if had_errors {
                    continue;
                }
                let n = decoded.chars().count();
                if n != 1 {
                    let cps: Vec<String> =
                        decoded.chars().map(|c| format!("U+{:04X}", c as u32)).collect();
                    found.push((format!("{b0:02x}{b1:02x}"), cps.join(" ")));
                }
            }
        }

        let expected = [
            ("8862", "U+00CA U+0304"),
            ("8864", "U+00CA U+030C"),
            ("88a3", "U+00EA U+0304"),
            ("88a5", "U+00EA U+030C"),
        ];
        let got: Vec<(&str, &str)> = found
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        assert_eq!(got, expected);
    }

    /// The end-to-end consequence: those sequences must be reported as two
    /// characters, so `--min-length` treats them the same way it treats
    /// any other two Unicode scalars.
    #[test]
    fn big5_two_scalar_sequences_count_as_two_characters() {
        // Two such sequences: 4 bytes, 2 sequences, 4 scalars.
        let mut bytes = vec![0x88, 0x62, 0x88, 0x64];
        bytes.push(0);
        let (path, _guard) = write_temp("big5-twoscalar", &bytes);
        let full = bytes.len() as u64;

        let at_four = scan_all_chunks_big5(&path, full, 4, "big5-ts-4");
        assert_eq!(
            at_four.lines().next().map(text_field),
            Some("\u{CA}\u{304}\u{CA}\u{30C}"),
            "m=4 should still admit a 4-scalar match: {at_four}"
        );

        // If they were counted as sequences (2), m=3 would drop this.
        let at_three = scan_all_chunks_big5(&path, full, 3, "big5-ts-3");
        assert!(at_three.lines().next().is_some(), "m=3 dropped a 4-scalar match");

        // And five scalars is genuinely more than there are.
        let at_five = scan_all_chunks_big5(&path, full, 5, "big5-ts-5");
        assert!(at_five.lines().next().is_none(), "m=5 admitted a 4-scalar match: {at_five}");
    }

    /// The reported character count must match the string actually
    /// emitted, end to end. `cch` drives `--min-length`, so a count that
    /// disagrees with the output would make `-m` silently off by one.
    #[test]
    fn reported_length_matches_the_emitted_text() {
        for (tag, bytes, scan) in [
            (
                "gbk",
                gbk("中文测试ABC"),
                scan_all_chunks_gbk as fn(&std::path::Path, u64, u64, &str) -> String,
            ),
            ("euckr", euckr("한국어ABC"), scan_all_chunks_euckr),
        ] {
            let mut input = bytes.clone();
            input.push(0);
            let (path, _guard) = write_temp(&format!("{tag}-cch"), &input);

            let want = if tag == "gbk" { "中文测试ABC" } else { "한국어ABC" };
            let n = want.chars().count() as u64;

            // At exactly the character count the match survives; one more
            // and it is dropped. That brackets `cch` precisely.
            let at = scan(&path, input.len() as u64, n, &format!("{tag}-cch-at"));
            assert_eq!(at.lines().next().map(text_field), Some(want), "{tag} at m={n}");

            let over = scan(&path, input.len() as u64, n + 1, &format!("{tag}-cch-over"));
            assert!(over.lines().next().is_none(), "{tag} at m={} : {over}", n + 1);
        }
    }

    // ---------------------------------------------------------------
    // End-to-end, through the real scanner and merger
    // ---------------------------------------------------------------

    fn write_temp(name: &str, bytes: &[u8]) -> (std::path::PathBuf, tempfile::TempDir) {
        let (path, guard) = temp_path(name);
        fs::write(&path, bytes).unwrap();
        (path, guard)
    }

    /// The central property, and the one that catches boundary bugs:
    /// `--chunk-size` is a performance knob and must never change what is
    /// found. Sweeping from 1 byte upward forces boundaries to land
    /// between the two halves of every double-byte character in the input.
    fn assert_chunk_size_invariant(
        scan: fn(&std::path::Path, u64, u64, &str) -> String,
        bytes: &[u8],
        tag: &str,
    ) {
        let (path, _guard) = write_temp(&format!("dbcs-inv-{tag}"), bytes);
        let full = bytes.len() as u64;
        let reference = scan(&path, full, 1, &format!("{tag}-ref"));

        for size in (1..=16u64).chain([24, 32, 64, full, full + 1, full * 2]) {
            if size == 0 {
                continue;
            }
            let got = scan(&path, size, 1, &format!("{tag}-{size}"));
            assert_eq!(
                got, reference,
                "chunk_size={size} disagreed with the single-chunk result for {tag}"
            );
        }
    }

    fn text_field(line: &str) -> &str {
        line.split('\t').nth(2).unwrap()
    }

    #[test]
    fn gbk_extracts_chinese_text() {
        let mut bytes = gbk("中文测试");
        bytes.push(0);
        let (path, _guard) = write_temp("gbk-basic", &bytes);

        let text = scan_all_chunks_gbk(&path, bytes.len() as u64, 1, "gbk-basic");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(text_field(lines[0]), "中文测试");
    }

    #[test]
    fn euckr_extracts_korean_text() {
        let mut bytes = euckr("한국어테스트");
        bytes.push(0);
        let (path, _guard) = write_temp("euckr-basic", &bytes);

        let text = scan_all_chunks_euckr(&path, bytes.len() as u64, 1, "euckr-basic");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(text_field(lines[0]), "한국어테스트");
    }

    #[test]
    fn gbk_results_are_independent_of_chunk_size() {
        assert_chunk_size_invariant(scan_all_chunks_gbk, &gbk("中文测试ABC"), "gbk");
    }

    #[test]
    fn euckr_results_are_independent_of_chunk_size() {
        assert_chunk_size_invariant(scan_all_chunks_euckr, &euckr("한국어테스트ABC"), "euckr");
    }

    #[test]
    fn big5_extracts_traditional_chinese_text() {
        let mut bytes = big5("繁體中文測試");
        bytes.push(0);
        let (path, _guard) = write_temp("big5-basic", &bytes);

        let text = scan_all_chunks_big5(&path, bytes.len() as u64, 1, "big5-basic");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(text_field(lines[0]), "繁體中文測試");
    }

    #[test]
    fn big5_results_are_independent_of_chunk_size() {
        assert_chunk_size_invariant(scan_all_chunks_big5, &big5("繁體中文測試ABC"), "big5");
    }

    /// Chunk-size invariance specifically across the two-scalar sequences,
    /// where a boundary can fall between the lead and trail of a sequence
    /// that expands to two characters.
    #[test]
    fn big5_two_scalar_sequences_are_chunk_size_invariant() {
        let mut bytes = b"AB".to_vec();
        bytes.extend([0x88, 0x62, 0x88, 0x64, 0x88, 0xa3, 0x88, 0xa5]);
        bytes.extend(b"CD");
        assert_chunk_size_invariant(scan_all_chunks_big5, &bytes, "big5-twoscalar-inv");
    }

    #[test]
    fn gbk_results_are_independent_of_chunk_size_when_buried_in_binary() {
        let mut bytes = vec![0x00, 0x01, 0x02];
        bytes.extend(gbk("简体中文"));
        bytes.extend([0x00, 0x1a]);
        bytes.extend(gbk("more中文here"));
        bytes.push(0x00);
        assert_chunk_size_invariant(scan_all_chunks_gbk, &bytes, "gbk-binary");
    }

    #[test]
    fn euckr_results_are_independent_of_chunk_size_when_buried_in_binary() {
        let mut bytes = vec![0x00, 0x01, 0x02];
        bytes.extend(euckr("안녕하세요"));
        bytes.extend([0x00, 0x1a]);
        bytes.extend(euckr("mixed한글text"));
        bytes.push(0x00);
        assert_chunk_size_invariant(scan_all_chunks_euckr, &bytes, "euckr-binary");
    }

    #[test]
    fn big5_results_are_independent_of_chunk_size_when_buried_in_binary() {
        let mut bytes = vec![0x00, 0x01, 0x02];
        bytes.extend(big5("繁體中文"));
        bytes.extend([0x00, 0x1a]);
        bytes.extend(big5("mixed中文here"));
        bytes.push(0x00);
        assert_chunk_size_invariant(scan_all_chunks_big5, &bytes, "big5-binary");
    }

    /// A match running to the very end of the file, with no terminating
    /// byte to close it. This exact shape hid a whole-file data-loss bug in
    /// the CP932 path (every EOF-reaching match was silently dropped), so
    /// it is checked explicitly for the new encodings too.
    #[test]
    fn a_match_reaching_end_of_file_is_reported() {
        for (tag, bytes, scan, want) in [
            (
                "gbk",
                gbk("中文测试"),
                scan_all_chunks_gbk as fn(&std::path::Path, u64, u64, &str) -> String,
                "中文测试",
            ),
            (
                "euckr",
                euckr("한국어테스트"),
                scan_all_chunks_euckr as fn(&std::path::Path, u64, u64, &str) -> String,
                "한국어테스트",
            ),
            (
                "big5",
                big5("繁體中文測試"),
                scan_all_chunks_big5 as fn(&std::path::Path, u64, u64, &str) -> String,
                "繁體中文測試",
            ),
        ] {
            let (path, _guard) = write_temp(&format!("{tag}-eof"), &bytes);
            for size in [1u64, 2, 3, 4, 5, 7, 64] {
                let text = scan(&path, size, 1, &format!("{tag}-eof-{size}"));
                assert_eq!(
                    text.lines().next().map(text_field),
                    Some(want),
                    "{tag} chunk_size={size}: {text}"
                );
            }
        }
    }

    /// Runs separated by a byte that cannot appear in any sequence must
    /// stay separate, rather than being joined into one over-long match.
    #[test]
    fn independent_runs_stay_separate() {
        let mut bytes = gbk("第一");
        bytes.push(0x00);
        bytes.extend(gbk("第二"));
        bytes.push(0x00);

        let (path, _guard) = write_temp("gbk-separate", &bytes);
        let text = scan_all_chunks_gbk(&path, bytes.len() as u64, 1, "gbk-sep");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(text_field(lines[0]), "第一");
        assert_eq!(text_field(lines[1]), "第二");
    }

    /// The reported offset must be the run's true position in the file.
    #[test]
    fn the_reported_offset_is_correct() {
        let mut bytes = vec![0x00, 0x00, 0x00];
        let payload = gbk("中文");
        bytes.extend(&payload);
        bytes.push(0x00);

        let (path, _guard) = write_temp("gbk-offset", &bytes);
        let text = scan_all_chunks_gbk(&path, bytes.len() as u64, 1, "gbk-offset");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 1, "{text}");
        let fields: Vec<_> = lines[0].split('\t').collect();
        assert_eq!(fields[0].parse::<u64>().unwrap(), 3, "{text}");
        assert_eq!(fields[1], "GBK");
        assert_eq!(fields[2], "中文");
    }

    /// The encodings sharing this engine must remain independently
    /// identifiable in the output, not collapse onto one label.
    #[test]
    fn each_encoding_labels_its_own_output() {
        let mut bytes = euckr("한국어");
        bytes.push(0x00);
        let (path, _guard) = write_temp("euckr-label", &bytes);

        let text = scan_all_chunks_euckr(&path, bytes.len() as u64, 1, "euckr-label");
        assert_eq!(text.lines().next().unwrap().split('\t').nth(1), Some("EUCKR"), "{text}");
    }

    /// `min_length` counts decoded characters, not bytes -- a two-character
    /// Chinese string is 4 bytes but must not satisfy `-m 3`.
    #[test]
    fn min_length_counts_characters_not_bytes() {
        let mut bytes = gbk("中文");
        bytes.push(0x00);
        bytes.extend(gbk("中文测试"));
        bytes.push(0x00);

        let (path, _guard) = write_temp("gbk-minlen", &bytes);
        let text = scan_all_chunks_gbk(&path, bytes.len() as u64, 3, "gbk-minlen");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(text_field(lines[0]), "中文测试");
    }

    // ---------------------------------------------------------------
    // GB18030: the four-byte form
    // ---------------------------------------------------------------
    //
    // Everything above tests encodings whose characters are one or two
    // bytes. GB18030 adds a four-byte form, which is the only thing in
    // this family that exercises a new path through the engine, so it gets
    // its own section.
    //
    // The design rests on one measured property: no valid two-byte pair
    // has a digit as its trail byte, so the second byte alone decides
    // which form is being read and no backtracking is ever needed. The
    // first two tests below pin exactly that property, because if it ever
    // stopped holding, the scanner would not merely mis-measure something
    // -- it would silently mis-segment text.

    /// Two encodings of a character with a four-byte GB18030 form,
    /// convenient for building test inputs. U+00A5 (YEN SIGN) is not in
    /// the two-byte repertoire, so `gb18030()` emits its four-byte form.
    const FOUR_BYTE_SAMPLE: char = '\u{00A5}';

    /// The property the whole four-byte design depends on: the second byte
    /// alone distinguishes the two forms.
    ///
    /// If any valid two-byte pair had a digit as its trail byte, then
    /// `starts_four_byte` -- which looks at nothing else -- would
    /// misclassify it, and a two-byte character followed by two more bytes
    /// would be swallowed as one bogus four-byte character. Exhaustive
    /// over all 65,536 pairs, because a single counterexample is enough to
    /// break the design and sampling would very likely miss it.
    #[test]
    fn no_two_byte_sequence_has_a_digit_trail_byte() {
        let mut offenders = Vec::new();
        for b0 in 0u16..=0xFF {
            for b1 in 0x30u16..=0x39 {
                let pair = [b0 as u8, b1 as u8];
                // Only pairs that are genuinely two-byte characters
                // matter; if b0 stands alone this is two single-byte
                // characters, which is unrelated.
                let one = [b0 as u8];
                if !Gb18030::decoder().decode_without_bom_handling(&one).1 {
                    continue;
                }
                if !Gb18030::decoder().decode_without_bom_handling(&pair).1 {
                    offenders.push(format!("{b0:02x}{b1:02x}"));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "GB18030: these two-byte pairs end in a digit, which would make \
             starts_four_byte ambiguous: {offenders:?}"
        );
    }

    /// The complement: every four-byte sequence really does have the
    /// structure `starts_four_byte` assumes, and the structural predicates
    /// admit its first byte as a lead.
    ///
    /// Walking all 1,087,996 valid sequences would be slow for little
    /// extra confidence, so this walks the full first-byte range against a
    /// fixed tail -- enough to catch a wrong `is_lead` range, which is the
    /// realistic failure -- plus the two extremes of the encoding.
    #[test]
    fn every_four_byte_sequence_starts_with_a_lead_byte() {
        let mut checked = 0u32;
        for b0 in 0x81u16..=0xFE {
            for b2 in [0x81u16, 0xFE] {
                let seq = [b0 as u8, 0x30, b2 as u8, 0x30];
                if Gb18030::decoder().decode_without_bom_handling(&seq).1 {
                    continue;
                }
                checked += 1;
                assert!(
                    Gb18030::is_lead(seq[0]),
                    "GB18030: 0x{:02x} begins a valid four-byte sequence but is_lead rejects it",
                    seq[0]
                );
                assert!(
                    Gb18030::starts_four_byte(&seq),
                    "GB18030: {seq:02x?} is a four-byte sequence but starts_four_byte says no"
                );
            }
        }
        assert!(checked > 80, "suspiciously few sequences checked ({checked})");
    }

    /// `starts_four_byte` must stay `false` for every encoding that has no
    /// four-byte form, since that default is what keeps their behaviour
    /// byte-identical to before it existed.
    #[test]
    fn only_gb18030_has_a_four_byte_form() {
        for b1 in 0x30u8..=0x39 {
            let probe = [0x81u8, b1, 0x81, 0x30];
            assert!(!Cp932::starts_four_byte(&probe), "CP932 claimed a four-byte form");
            assert!(!Gbk::starts_four_byte(&probe), "GBK claimed a four-byte form");
            assert!(!EucKr::starts_four_byte(&probe), "EUC-KR claimed a four-byte form");
            assert!(!Big5::starts_four_byte(&probe), "Big5 claimed a four-byte form");
            assert!(Gb18030::starts_four_byte(&probe), "GB18030 lost its four-byte form");
        }
    }

    /// GB18030's two-byte repertoire is identical to GBK's, so on input
    /// containing no four-byte sequences the two scanners must agree
    /// exactly, down to the offsets -- only the encoding label differs.
    ///
    /// This is the strongest available statement that adding the four-byte
    /// path did not perturb the two-byte one: rather than asserting
    /// against hand-written expectations, it holds GB18030 against a
    /// scanner that was already correct.
    #[test]
    fn gb18030_matches_gbk_on_input_without_four_byte_sequences() {
        let mut bytes = vec![0x00, 0x01];
        bytes.extend(gbk("中文测试ABC"));
        bytes.extend([0x00, 0x1a]);
        bytes.extend(gbk("more中文here"));
        bytes.push(0x00);

        let (path, _guard) = write_temp("gb-vs-gbk", &bytes);
        let full = bytes.len() as u64;

        for size in [1u64, 2, 3, 5, 8, 64, full] {
            let via_gbk = scan_all_chunks_gbk(&path, size, 1, &format!("gbk-x-{size}"));
            let via_gb = scan_all_chunks_gb18030(&path, size, 1, &format!("gb-x-{size}"));
            assert_eq!(
                via_gbk.replace("\tGBK\t", "\tGB18030\t"),
                via_gb,
                "chunk_size={size}: GB18030 disagreed with GBK on two-byte-only input"
            );
        }
    }

    #[test]
    fn gb18030_extracts_chinese_text() {
        let mut bytes = gb18030("中文测试");
        bytes.push(0);
        let (path, _guard) = write_temp("gb-basic", &bytes);

        let text = scan_all_chunks_gb18030(&path, bytes.len() as u64, 1, "gb-basic");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(text_field(lines[0]), "中文测试");
    }

    /// A character that only exists in the four-byte form must round-trip
    /// through the scanner intact.
    #[test]
    fn gb18030_extracts_four_byte_characters() {
        let want: String = std::iter::repeat_n(FOUR_BYTE_SAMPLE, 4).collect();
        let mut bytes = gb18030(&want);
        assert_eq!(bytes.len(), 16, "expected four four-byte sequences: {bytes:02x?}");
        bytes.push(0);

        let (path, _guard) = write_temp("gb-four", &bytes);
        let text = scan_all_chunks_gb18030(&path, bytes.len() as u64, 1, "gb-four");
        assert_eq!(text.lines().next().map(text_field), Some(want.as_str()), "{text}");
    }

    /// GB18030 reaches the astral planes, which no other encoding in this
    /// family can do. These two sequences are its exact endpoints.
    #[test]
    fn gb18030_decodes_astral_characters() {
        for (seq, want) in [
            ([0x90u8, 0x30, 0x81, 0x30], '\u{10000}'),
            ([0xe3, 0x32, 0x9a, 0x35], '\u{10FFFF}'),
        ] {
            // Establish the reference fact independently of the scanner.
            let decoded = Gb18030::decoder()
                .decode_without_bom_handling(&seq)
                .0
                .into_owned();
            assert_eq!(decoded.chars().next(), Some(want), "{seq:02x?}");

            // Padding keeps the run above any plausible min length and
            // gives the scanner an unambiguous terminator.
            let mut bytes = b"AAAA".to_vec();
            bytes.extend(seq);
            bytes.extend(b"BBBB");
            bytes.push(0);

            let (path, _guard) = write_temp("gb-astral", &bytes);
            let text = scan_all_chunks_gb18030(&path, bytes.len() as u64, 1, "gb-astral");
            let got = text.lines().next().map(text_field).unwrap_or_default();
            assert_eq!(got, format!("AAAA{want}BBBB"), "{text}");
        }
    }

    /// An astral character is one Unicode scalar but two UTF-16 code
    /// units, and four bytes. `cch` must report the scalar count, exactly
    /// as every other scanner does, so `-m` means the same thing
    /// regardless of `-e`.
    #[test]
    fn gb18030_counts_an_astral_character_as_one() {
        // Four ASCII characters plus one astral character = 5 scalars.
        let mut bytes = b"ABCD".to_vec();
        bytes.extend([0x90u8, 0x30, 0x81, 0x30]);
        bytes.push(0);
        let (path, _guard) = write_temp("gb-astral-cch", &bytes);
        let full = bytes.len() as u64;

        let at_five = scan_all_chunks_gb18030(&path, full, 5, "gb-cch-5");
        assert!(
            at_five.lines().next().is_some(),
            "m=5 dropped a 5-scalar match, so cch is under-counting: {at_five}"
        );

        let at_six = scan_all_chunks_gb18030(&path, full, 6, "gb-cch-6");
        assert!(
            at_six.lines().next().is_none(),
            "m=6 admitted a 5-scalar match, so cch is over-counting \
             (bytes or UTF-16 units rather than scalars): {at_six}"
        );
    }

    /// The central invariance property, applied to the four-byte form:
    /// sweeping chunk sizes from 1 upward forces a boundary to land at
    /// *every* position inside every four-byte sequence, which is the
    /// case the `Step::Incomplete` carry logic had to be generalised for.
    #[test]
    fn gb18030_four_byte_sequences_are_chunk_size_invariant() {
        let mut bytes = b"AB".to_vec();
        bytes.extend(gb18030(&std::iter::repeat_n(FOUR_BYTE_SAMPLE, 3).collect::<String>()));
        bytes.extend(b"CD");
        bytes.extend([0x90u8, 0x30, 0x81, 0x30]); // astral
        bytes.extend(b"EF");
        assert_chunk_size_invariant(scan_all_chunks_gb18030, &bytes, "gb-four-inv");
    }

    /// The hardest boundary case in this family: a four-byte sequence
    /// immediately followed by a two-byte one, so a mis-read of the first
    /// would consume part of the second and desynchronise everything
    /// after it.
    #[test]
    fn gb18030_mixed_lengths_are_chunk_size_invariant() {
        let mut bytes = b"x".to_vec();
        bytes.extend(gb18030(&format!("{FOUR_BYTE_SAMPLE}中{FOUR_BYTE_SAMPLE}文A")));
        bytes.push(0x00);
        bytes.extend(gb18030(&format!("中{FOUR_BYTE_SAMPLE}")));
        bytes.push(0x00);
        assert_chunk_size_invariant(scan_all_chunks_gb18030, &bytes, "gb-mixed-inv");
    }

    #[test]
    fn gb18030_results_are_independent_of_chunk_size_when_buried_in_binary() {
        let mut bytes = vec![0x00, 0x01, 0x02];
        bytes.extend(gb18030("简体中文"));
        bytes.extend([0x00, 0x1a]);
        bytes.extend(gb18030(&format!("mixed{FOUR_BYTE_SAMPLE}中文here")));
        bytes.push(0x00);
        assert_chunk_size_invariant(scan_all_chunks_gb18030, &bytes, "gb-binary");
    }

    /// A four-byte sequence running right up to end of file, with no
    /// terminating byte -- the shape that once hid a data-loss bug in the
    /// CP932 path, now with a character that can be truncated three ways
    /// rather than one.
    #[test]
    fn gb18030_four_byte_sequence_at_end_of_file_is_reported() {
        let want = format!("ABC{FOUR_BYTE_SAMPLE}");
        let bytes = gb18030(&want);
        let (path, _guard) = write_temp("gb-four-eof", &bytes);

        for size in [1u64, 2, 3, 4, 5, 6, 7, 8, 64] {
            let text = scan_all_chunks_gb18030(&path, size, 1, &format!("gb-four-eof-{size}"));
            assert_eq!(
                text.lines().next().map(text_field),
                Some(want.as_str()),
                "chunk_size={size}: {text}"
            );
        }
    }

    /// A truncated four-byte sequence at true end of file is not a
    /// character and must be dropped, not emitted as mojibake -- while the
    /// valid text before it survives.
    #[test]
    fn gb18030_truncated_four_byte_sequence_at_eof_is_dropped() {
        for truncate_to in 1usize..=3 {
            let mut bytes = b"ABCD".to_vec();
            let four = gb18030(&FOUR_BYTE_SAMPLE.to_string());
            bytes.extend(&four[..truncate_to]);

            let (path, _guard) = write_temp(&format!("gb-trunc-{truncate_to}"), &bytes);
            let full = bytes.len() as u64;
            for size in [1u64, 2, 3, 5, full] {
                let text = scan_all_chunks_gb18030(&path, size, 1, &format!("gb-tr-{truncate_to}-{size}"));
                assert_eq!(
                    text.lines().next().map(text_field),
                    Some("ABCD"),
                    "truncated to {truncate_to} bytes, chunk_size={size}: {text}"
                );
            }
        }
    }

    #[test]
    fn gb18030_labels_its_own_output() {
        let mut bytes = gb18030(&format!("测试{FOUR_BYTE_SAMPLE}"));
        bytes.push(0x00);
        let (path, _guard) = write_temp("gb-label", &bytes);

        let text = scan_all_chunks_gb18030(&path, bytes.len() as u64, 1, "gb-label");
        assert_eq!(
            text.lines().next().unwrap().split('\t').nth(1),
            Some("GB18030"),
            "{text}"
        );
    }

    /// Offsets must count source bytes, so a four-byte character advances
    /// the position by four even though it is one character.
    #[test]
    fn gb18030_offsets_account_for_four_byte_sequences() {
        let mut bytes = vec![0x00, 0x00];
        bytes.extend(gb18030(&FOUR_BYTE_SAMPLE.to_string())); // 4 bytes at offset 2
        bytes.extend(b"ABC");
        bytes.push(0x00);
        // A second run, whose offset is only right if the first run's
        // four-byte character was measured as four bytes.
        bytes.extend(b"WXYZ");
        bytes.push(0x00);

        let (path, _guard) = write_temp("gb-offset", &bytes);
        let text = scan_all_chunks_gb18030(&path, bytes.len() as u64, 1, "gb-offset");
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");

        let first: Vec<_> = lines[0].split('\t').collect();
        assert_eq!(first[0].parse::<u64>().unwrap(), 2, "{text}");
        assert_eq!(first[2], format!("{FOUR_BYTE_SAMPLE}ABC"));

        let second: Vec<_> = lines[1].split('\t').collect();
        assert_eq!(second[0].parse::<u64>().unwrap(), 10, "{text}");
        assert_eq!(second[2], "WXYZ");
    }
}
