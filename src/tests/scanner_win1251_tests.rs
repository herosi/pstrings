//! Tests for the windows-1251 scanner.
//!
//! The central concern here is *correct decoding*, not run-boundary
//! mechanics. The run logic is a copy of `scanner::ascii`'s, which is
//! already covered by `scanner_ascii_tests.rs`; what is new and what can
//! plausibly be wrong is the 256-entry table and the decision to filter on
//! decoded characters rather than raw bytes.
//!
//! `does_not_emit_latin1_mojibake` is the test that matters most. The
//! tempting way to add windows-1251 support was to reuse
//! `scanner::ascii` and just add a `cyrillic` filter -- but that scanner
//! hardcodes `b as char` (the ISO-8859-1 table), so it would have matched
//! Cyrillic text and emitted Latin letters: "Привет" in, "Ïðèâåò" out.
//! Silently wrong output is worse than no output, and that test pins the
//! difference.

#[cfg(test)]
mod tests {
    use crate::chunk::Chunk;
    use crate::filter::CharacterFilter;
    use crate::scanner::win1251;
    use crate::tests::support::*;
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    /// Encodes to windows-1251, panicking on anything unrepresentable.
    fn cp1251(s: &str) -> Vec<u8> {
        let (encoded, _, had_errors) = encoding_rs::WINDOWS_1251.encode(s);
        assert!(!had_errors, "test string {s:?} is not representable in windows-1251");
        encoded.into_owned()
    }

    fn write_temp(name: &str, bytes: &[u8]) -> (PathBuf, tempfile::TempDir) {
        let (path, guard) = temp_path(name);
        fs::write(&path, bytes).unwrap();
        (path, guard)
    }

    /// Runs the real scanner over every chunk and merges, mirroring the
    /// production pipeline. Takes an explicit filter list because -- unlike
    /// the structurally-validating scanners -- this one's behaviour is
    /// defined by the filter, so the filter is part of what's under test.
    fn scan_all(
        input: &Path,
        chunk_size: u64,
        min_cch: u64,
        filters: Vec<CharacterFilter>,
        name: &str,
    ) -> String {
        let file = File::open(input).unwrap();
        let file_len = fs::metadata(input).unwrap().len();
        let cfg = test_config_with_filters(min_cch, chunk_size, filters);
        let cancel = AtomicBool::new(false);
        let mut outputs: Vec<(u64, u64, File)> = Vec::new();
        let mut out_guards = Vec::new();
        let chunk_count = if file_len == 0 { 0 } else { file_len.div_ceil(chunk_size) };

        for index in 0..chunk_count {
            let offset = index * chunk_size;
            let len = (file_len - offset).min(chunk_size);
            let (out, out_guard) = temp_path(&format!("{name}-out-{index}"));
            let chunk = Chunk { offset, len };
            let result_file = win1251::scan(&file, &chunk, &cfg, &out, &cancel).unwrap().1;
            outputs.push((offset, len, result_file));
            out_guards.push(out_guard);
        }

        merge_test_encoding_chunks_at(outputs, min_cch)
    }

    fn both() -> Vec<CharacterFilter> {
        vec![CharacterFilter::Ascii, CharacterFilter::Cyrillic]
    }

    fn text_field(line: &str) -> &str {
        line.split('\t').nth(2).unwrap()
    }

    // ---------------------------------------------------------------
    // The table
    // ---------------------------------------------------------------

    /// The table is a transcription of `encoding_rs`'s data, so it is
    /// verified by re-deriving it rather than by spot-checking entries --
    /// a hand-written expectation could repeat the same typo the table
    /// might contain.
    #[test]
    fn table_matches_encoding_rs() {
        for b in 0u16..=0xFF {
            let one = [b as u8];
            let want = encoding_rs::WINDOWS_1251
                .decode(&one)
                .0
                .chars()
                .next()
                .unwrap();
            assert_eq!(
                win1251::TABLE[b as usize],
                want,
                "table entry for byte 0x{b:02x} is wrong"
            );
        }
    }

    /// Documents the exact repertoire, so a future edit that silently
    /// widens or narrows the table fails here.
    #[test]
    fn table_reaches_the_expected_unicode_blocks() {
        let mut ascii = 0;
        let mut cyrillic = 0;
        let mut other = 0;
        for b in 0u16..=0xFF {
            let ch = win1251::TABLE[b as usize] as u32;
            if ch < 0x80 {
                ascii += 1;
            } else if (0x0400..=0x04FF).contains(&ch) {
                cyrillic += 1;
            } else {
                other += 1;
            }
        }
        assert_eq!((ascii, cyrillic, other), (128, 94, 34));
    }

    // ---------------------------------------------------------------
    // The bug this module exists to avoid
    // ---------------------------------------------------------------

    /// The whole reason windows-1251 is not just "`scanner::ascii` plus a
    /// filter". Reusing that scanner would map byte 0xC0 to U+00C0 (À)
    /// instead of U+0410 (А), producing convincing-looking mojibake.
    #[test]
    fn does_not_emit_latin1_mojibake() {
        let word = "Привет";
        let mut bytes = cp1251(word);
        bytes.push(0);
        let (path, _guard) = write_temp("w1251-mojibake", &bytes);

        let text = scan_all(&path, bytes.len() as u64, 1, both(), "w1251-mojibake");
        let got = text_field(text.lines().next().expect("no match at all"));

        assert_eq!(got, word);

        // And explicitly: not the Latin-1 misreading a byte-as-char
        // scanner would have produced.
        let mojibake: String = cp1251(word).iter().map(|&b| b as char).collect();
        assert_ne!(got, mojibake);
        assert_eq!(mojibake, "\u{cf}\u{f0}\u{e8}\u{e2}\u{e5}\u{f2}");
    }

    /// The 0x80-0x9F range is real text in this codepage, unlike in
    /// ISO-8859-1 where it is C1 controls -- which is why `filter::latin1`
    /// stops at 0xA0 and why this scanner cannot reuse it.
    #[test]
    fn the_c1_range_is_usable_text() {
        // Ђ Ѓ љ њ -- all encoded in 0x80..=0x9F.
        let word = "\u{402}\u{403}\u{459}\u{45a}";
        let bytes = cp1251(word);
        assert!(
            bytes.iter().all(|&b| (0x80..=0x9F).contains(&b)),
            "test premise broken: {bytes:02x?}"
        );

        let mut input = bytes.clone();
        input.push(0);
        let (path, _guard) = write_temp("w1251-c1", &input);

        let text = scan_all(&path, input.len() as u64, 1, both(), "w1251-c1");
        assert_eq!(text_field(text.lines().next().unwrap()), word);
    }

    /// Byte 0x98 is the single unassigned position; it maps to a C1
    /// control and must break a run rather than appear in output.
    #[test]
    fn the_unassigned_byte_breaks_a_run() {
        let mut bytes = cp1251("АБВ");
        bytes.push(0x98);
        bytes.extend(cp1251("ГДЕ"));
        bytes.push(0);

        let (path, _guard) = write_temp("w1251-unassigned", &bytes);
        let text = scan_all(&path, bytes.len() as u64, 1, both(), "w1251-unassigned");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(text_field(lines[0]), "АБВ");
        assert_eq!(text_field(lines[1]), "ГДЕ");
    }

    // ---------------------------------------------------------------
    // Filtering
    // ---------------------------------------------------------------

    /// With the default filter (`ascii` only) this scanner must behave
    /// like `scanner::ascii`: Cyrillic bytes are not text and break runs.
    #[test]
    fn the_default_filter_finds_only_ascii() {
        let mut bytes = b"abc".to_vec();
        bytes.extend(cp1251("Привет"));
        bytes.extend(b"def");
        bytes.push(0);

        let (path, _guard) = write_temp("w1251-asciionly", &bytes);
        let text = scan_all(
            &path,
            bytes.len() as u64,
            1,
            vec![CharacterFilter::Ascii],
            "w1251-asciionly",
        );
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(text_field(lines[0]), "abc");
        assert_eq!(text_field(lines[1]), "def");
    }

    /// `ascii,cyrillic` is the combination that makes the encoding useful:
    /// real Russian text is full of ASCII punctuation and digits, and
    /// dropping `ascii` would fragment every sentence.
    #[test]
    fn ascii_and_cyrillic_together_keep_a_sentence_whole() {
        let sentence = "Версия 2.0, привет!";
        let mut bytes = cp1251(sentence);
        bytes.push(0);

        let (path, _guard) = write_temp("w1251-sentence", &bytes);
        let text = scan_all(&path, bytes.len() as u64, 1, both(), "w1251-sentence");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(text_field(lines[0]), sentence);
    }

    /// Selecting `cyrillic` alone must exclude ASCII -- the filter is a
    /// character-class selector, and the scanner must honour it exactly.
    #[test]
    fn cyrillic_alone_excludes_ascii() {
        let mut bytes = b"hello".to_vec();
        bytes.extend(cp1251("Привет"));
        bytes.push(0);

        let (path, _guard) = write_temp("w1251-cyronly", &bytes);
        let text = scan_all(
            &path,
            bytes.len() as u64,
            1,
            vec![CharacterFilter::Cyrillic],
            "w1251-cyronly",
        );
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(text_field(lines[0]), "Привет");
    }

    /// The Cyrillic filter must not be reachable from a raw byte: which
    /// byte means a Cyrillic letter depends on the codepage, so
    /// `allows_u8` has to stay false or `scanner::ascii` would start
    /// admitting high bytes and rendering them as Latin-1.
    #[test]
    fn the_cyrillic_filter_has_no_single_byte_form() {
        let cfg = test_config_with_filters(1, 4096, vec![CharacterFilter::Cyrillic]);
        for b in 0u16..=0xFF {
            assert!(
                !cfg.filter().allows_u8(b as u8),
                "cyrillic filter admitted raw byte 0x{b:02x}; scanner::ascii would \
                 render it via the ISO-8859-1 table and emit mojibake"
            );
        }
    }

    // ---------------------------------------------------------------
    // Chunking
    // ---------------------------------------------------------------

    /// `--chunk-size` is a performance knob and must never change results.
    #[test]
    fn results_are_independent_of_chunk_size() {
        let mut bytes = vec![0x00, 0x01, 0x02];
        bytes.extend(cp1251("Привет, мир"));
        bytes.extend([0x00, 0x1a]);
        bytes.extend(cp1251("Версия 2.0"));
        bytes.push(0x00);

        let (path, _guard) = write_temp("w1251-inv", &bytes);
        let full = bytes.len() as u64;
        let reference = scan_all(&path, full, 1, both(), "w1251-inv-ref");

        for size in (1..=16u64).chain([24, 32, 64, full, full + 1, full * 2]) {
            let got = scan_all(&path, size, 1, both(), &format!("w1251-inv-{size}"));
            assert_eq!(got, reference, "chunk_size={size} changed the result");
        }
    }

    /// A match running to the very end of the file, with nothing to close
    /// it. This shape hid a whole-file data-loss bug in the CP932 scanner,
    /// so it is checked explicitly here too.
    #[test]
    fn a_match_reaching_end_of_file_is_reported() {
        let bytes = cp1251("Привет");
        let (path, _guard) = write_temp("w1251-eof", &bytes);

        for size in [1u64, 2, 3, 5, 7, 64] {
            let text = scan_all(&path, size, 1, both(), &format!("w1251-eof-{size}"));
            assert_eq!(
                text.lines().next().map(text_field),
                Some("Привет"),
                "chunk_size={size}: {text}"
            );
        }
    }

    // ---------------------------------------------------------------
    // Record bookkeeping
    // ---------------------------------------------------------------

    /// `cb` counts source bytes and `cch` counts characters; for this
    /// encoding they are equal (one byte per character) even though the
    /// UTF-8 output is longer. Verified through the reported offset of a
    /// following match, which is only correct if `cb` was right.
    #[test]
    fn offsets_account_for_source_bytes_not_output_bytes() {
        let first = cp1251("Привет"); // 6 source bytes, 12 UTF-8 bytes
        assert_eq!(first.len(), 6);

        let mut bytes = first.clone();
        bytes.push(0);
        let second_at = bytes.len() as u64;
        bytes.extend(cp1251("мир"));
        bytes.push(0);

        let (path, _guard) = write_temp("w1251-offsets", &bytes);
        let text = scan_all(&path, bytes.len() as u64, 1, both(), "w1251-offsets");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0].split('\t').next().unwrap().parse::<u64>().unwrap(), 0);
        assert_eq!(
            lines[1].split('\t').next().unwrap().parse::<u64>().unwrap(),
            second_at
        );
    }

    /// `min_length` counts characters. Every character here is one source
    /// byte, so this also pins that the threshold isn't accidentally
    /// applied to the UTF-8 length (which would be twice as long and let
    /// short runs through).
    #[test]
    fn min_length_counts_characters() {
        let mut bytes = cp1251("Да"); // 2 characters, 4 UTF-8 bytes
        bytes.push(0);
        bytes.extend(cp1251("Привет"));
        bytes.push(0);

        let (path, _guard) = write_temp("w1251-minlen", &bytes);
        let text = scan_all(&path, bytes.len() as u64, 4, both(), "w1251-minlen");
        let lines: Vec<_> = text.lines().collect();

        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(text_field(lines[0]), "Привет");
    }

    /// The output must be labelled distinctly from the other encodings.
    #[test]
    fn output_is_labelled_cp1251() {
        let mut bytes = cp1251("Привет");
        bytes.push(0);
        let (path, _guard) = write_temp("w1251-label", &bytes);

        let text = scan_all(&path, bytes.len() as u64, 1, both(), "w1251-label");
        assert_eq!(
            text.lines().next().unwrap().split('\t').nth(1),
            Some("CP1251"),
            "{text}"
        );
    }
}
