use super::support::*;
use crate::chunk::Chunk;
use crate::filter::CharacterFilter;
use crate::scanner::{ascii, cp932, utf16le, utf16le_ascii, utf8};
use std::fs::{self, File};
use std::sync::atomic::AtomicBool;

// Every `scan` returns `(record_count, File)`, and that count is what the
// `--stats` output reports per encoding. It must equal the number of
// records actually written to the returned file.
//
// This is easy to get wrong because `scanner::emit_record` silently drops
// records that fall below `min_cch` without touching a chunk boundary.
// Scanners that incremented their counter unconditionally around that call
// counted every *attempted* run instead of every *emitted* one -- and on
// realistic (mostly binary) input the overwhelming majority of runs are
// short-lived noise that gets dropped, so the reported figure came out
// orders of magnitude too high.
//
// That bug was found via a 36 GiB disk image where `scanner::utf16le_ascii`
// reported ~55 million UTF-16LE records while `scanner::utf16le` reported
// ~1600 on the same input with the same filter. The two scanners were in
// fact detecting the same things; only the counting differed, because
// `scanner::utf16le` derives its count from the records it actually holds.
//
// These tests use deliberately noisy input (lots of sub-`min_cch` runs) so
// that a regression to unconditional counting fails loudly.

/// Pseudo-random bytes: dense in short, sub-threshold runs at every
/// encoding, which is exactly the case that exposed the bug.
fn pseudo_random(len: usize) -> Vec<u8> {
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.push((state >> 24) as u8);
    }
    bytes
}

/// Single-byte ASCII text separated by NULs. Read as UTF-16LE this yields
/// many short false-positive runs; read as ASCII it yields many genuine
/// but short ones. Either way, most fall below `min_cch`.
fn short_strings() -> Vec<u8> {
    let mut bytes = Vec::new();
    for i in 0..2000 {
        bytes.extend_from_slice(format!("s{i:03}").as_bytes());
        bytes.extend_from_slice(&[0u8; 5]);
    }
    bytes
}

fn check(tag: &str, bytes: &[u8], min_cch: u64) {
    let (input_path, _input_guard) = temp_path(&format!("count-{tag}-input"));
    fs::write(&input_path, bytes).unwrap();
    let len = bytes.len() as u64;
    let cfg = test_config_with_filters(min_cch, 1 << 20, vec![CharacterFilter::Ascii]);
    let chunk = Chunk { offset: 0, len };
    let cancel = AtomicBool::new(false);

    // `ascii::scan` doesn't take `file_len` (it never needs to look past
    // the chunk), so it gets its own arm.
    macro_rules! check_one {
        ($name:literal, $scan:path) => {{
            let (out, _guard) = temp_path(&format!("count-{tag}-{}", $name));
            let file = File::open(&input_path).unwrap();
            let (reported, mut result) =
                $scan(&file, len, &chunk, &cfg, &out, &cancel).unwrap();
            let actual = read_records(&mut result).len() as u64;
            assert_eq!(
                reported, actual,
                "{} reported {reported} records for `{tag}` but wrote {actual}; \
                 the count must come from emit_record's return value, not from \
                 counting every attempted run",
                $name
            );
        }};
    }

    {
        let (out, _guard) = temp_path(&format!("count-{tag}-ascii"));
        let file = File::open(&input_path).unwrap();
        let (reported, mut result) = ascii::scan(&file, &chunk, &cfg, &out, &cancel).unwrap();
        let actual = read_records(&mut result).len() as u64;
        assert_eq!(
            reported, actual,
            "ascii reported {reported} records for `{tag}` but wrote {actual}; \
             the count must come from emit_record's return value, not from \
             counting every attempted run"
        );
    }

    check_one!("utf16le_ascii", utf16le_ascii::scan);
    check_one!("utf16le", utf16le::scan);
    check_one!("utf8", utf8::scan);
    check_one!("cp932", cp932::scan);
}

#[test]
fn reported_record_count_matches_emitted_records_on_random_data() {
    check("random", &pseudo_random(1 << 18), 4);
}

#[test]
fn reported_record_count_matches_emitted_records_on_short_strings() {
    // min_cch deliberately just above the run length so nearly every run
    // is dropped by emit_record.
    check("short", &short_strings(), 8);
}

#[test]
fn reported_record_count_matches_emitted_records_when_nothing_qualifies() {
    // The extreme case: a threshold so high that *no* record survives.
    // Every scanner must report exactly zero.
    check("none", &short_strings(), 1000);
}

#[test]
fn reported_record_count_matches_emitted_records_when_everything_qualifies() {
    // The opposite extreme, to make sure the fix didn't start
    // under-counting: min_cch of 1 keeps essentially everything.
    check("all", &short_strings(), 1);
}

#[test]
fn utf16le_scanners_agree_on_detection_for_ascii_text() {
    // The observation that surfaced the bug: `scanner::utf16le` and
    // `scanner::utf16le_ascii` are expected to find the same UTF-16LE
    // ASCII strings when only the `ascii` filter is selected, since the
    // wider scanner's extra capability (non-ASCII BMP + astral) is
    // inert under that filter.
    let mut bytes = Vec::new();
    for i in 0..300 {
        bytes.extend_from_slice(&utf16le(&format!("Message{i:04}")));
        bytes.extend_from_slice(&[0xFF, 0xFE, 0xFF, 0xFE]);
    }
    let (input_path, _input_guard) = temp_path("count-agree-input");
    fs::write(&input_path, &bytes).unwrap();
    let len = bytes.len() as u64;
    let cfg = test_config_with_filters(4, 1 << 20, vec![CharacterFilter::Ascii]);
    let chunk = Chunk { offset: 0, len };
    let cancel = AtomicBool::new(false);

    let (out_a, _ga) = temp_path("count-agree-a");
    let file_a = File::open(&input_path).unwrap();
    let (n_a, mut f_a) = utf16le_ascii::scan(&file_a, len, &chunk, &cfg, &out_a, &cancel).unwrap();
    let texts_a: Vec<String> = read_records(&mut f_a)
        .iter()
        .map(|r| r.data.text_of().to_string())
        .collect();

    let (out_f, _gf) = temp_path("count-agree-f");
    let file_f = File::open(&input_path).unwrap();
    let (n_f, mut f_f) = utf16le::scan(&file_f, len, &chunk, &cfg, &out_f, &cancel).unwrap();
    let texts_f: Vec<String> = read_records(&mut f_f)
        .iter()
        .map(|r| r.data.text_of().to_string())
        .collect();

    assert_eq!(n_a, 300, "utf16le_ascii should report exactly the 300 planted strings");
    assert_eq!(n_f, 300, "utf16le should report exactly the 300 planted strings");
    assert_eq!(texts_a, texts_f, "the two UTF-16LE scanners disagreed on content");
}
