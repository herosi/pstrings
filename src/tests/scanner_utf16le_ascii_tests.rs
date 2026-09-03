use super::support::*;
use crate::chunk::Chunk;
use crate::scanner::utf16le_ascii;
use crate::READ_BUFFER_SIZE;
use std::fs::{self, File};
use std::sync::atomic::AtomicBool;

// This module tests the `scanner::utf16le` scan
// functions directly (single-chunk behavior: run detection, min_cch /
// boundary-fragment filtering via `emit_record`, record field correctness)
// as well as multi-chunk behavior via two different helpers:
//   - manually calling `scan` on adjacent hand-picked `Chunk`s and merging
//     the results, for tests that need precise control over chunk edges;
//   - `scan_all_chunks` (see `support`), which presumably drives the real
//     chunking logic end-to-end over a fixed chunk size, for tests that
//     care about behavior *however* the input happens to get split.
// A recurring theme is verifying that splitting the same logical input
// differently (chunk sizes, chunk boundaries landing mid-run or mid-code-
// unit) never changes the final joined output -- only whether a fragment
// temporarily lives in one chunk's result or two.

#[test]
fn min_cch_boundary_is_inclusive_for_utf16le_ascii() {
    // Four back-to-back NUL-separated runs of increasing length (1..4
    // chars). Re-scans the *same* input once per min_cch threshold
    // (1 through 4) to confirm the cutoff is inclusive (`cch >= min_cch`,
    // not `>`): at min_cch=N, the length-N run should still appear, and
    // only runs shorter than N should be dropped.
    let (input_path, _input_guard) = temp_path("utf16-min-boundaries-input");
    let mut bytes = Vec::new();
    for s in ["A", "AB", "ABC", "ABCD"] {
        bytes.extend_from_slice(&utf16le(s));
        bytes.extend_from_slice(&[0, 0]);
    }
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    for min_cch in 1..=4 {
        let (out, _out_guard) = temp_path(&format!("utf16-min-boundaries-output-{min_cch}"));
        let cfg = test_config(min_cch);
        let chunk = Chunk {
            offset: 0,
            len: bytes.len() as u64,
        };
        let (_records, result_file) =
            utf16le_ascii::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

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
fn utf16le_ascii_string_crosses_three_chunk_boundaries() {
    // UTF-16LE three-chunk test: a 13-character
    // string, prefixed by one stray 0x00 byte so the code units start at
    // an odd byte offset (offset 1), then chunked into 6-byte pieces --
    // exercising both odd-parity alignment and a multi-chunk join at once.
    let (input_path, _input_guard) = temp_path("utf16-three-chunks-input");
    let mut bytes = vec![0x00];
    bytes.extend_from_slice(&utf16le("ABCDEFGHIJKLM"));
    bytes.push(0);
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks(&input_path, 6, true, 4, "utf16-three-chunks");
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "ABCDEFGHIJKLM", "{text}");
    assert!(lines[0].starts_with("00000000000000000001\tUTF16LE\t"), "{text}");
}

#[test]
fn utf16le_ascii_detects_odd_byte_offset() {
    // Confirms the UTF-16LE scanner correctly recognizes a string whose
    // code units start at an *odd* byte offset (here: one leading stray
    // byte at offset 0, so "HELLO WORLD" begins at offset 1) within a
    // single chunk -- i.e. the odd-parity scan pass (see `scan_parity` in
    // scanner/utf16le.rs) is actually exercised, not just the even one.
    let (input_path, _input_guard) = temp_path("utf16-odd-input");
    let (out, _out_guard) = temp_path("utf16-odd-output");
    let mut bytes = vec![0x00];
    bytes.extend_from_slice(&utf16le("HELLO WORLD"));
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(5);
    let (_records, mut result_file) = utf16le_ascii::scan(
        &file,
        bytes.len() as u64,
        &Chunk {
            offset: 0,
            len: bytes.len() as u64,
        },
        &cfg,
        &out,
        &AtomicBool::new(false),
    )
    .unwrap();

    let records = read_records(&mut result_file);
    assert!(records.iter().any(|r| r.data.text_of() == "HELLO WORLD" && r.offset == 1));
}

#[test]
fn utf16le_ascii_boundary_fragment_below_min_cch_is_preserved_and_joined() {
    // UTF-16LE analogue of `ascii_boundary_fragment_below_min_cch_is_preserved_and_joined`:
    // a 4-character run ("ABCD") is chunked (chunk size 8) so it straddles
    // a boundary, and each side must be kept despite possibly being
    // shorter than min_cch=4 in isolation, then correctly joined back
    // together at its true starting offset (7).
    let (input_path, _input_guard) = temp_path("utf16-boundary-short-fragment-input");
    let mut bytes = vec![0u8; 7];
    bytes.extend_from_slice(&utf16le("ABCD"));
    bytes.extend_from_slice(&[0, 0]);
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks(&input_path, 8, true, 4, "utf16-boundary-short-fragment");
    let lines: Vec<_> = text.lines().collect();
    assert!(
        lines
            .iter()
            .any(|line| { line.starts_with("00000000000000000007\tUTF16LE\tABCD") }),
        "{text}"
    );
}

#[test]
fn utf16le_ascii_string_crosses_chunk_boundary_odd_offset() {
    // Combines two things at once, using manually placed chunks for exact
    // control: (1) the string starts at an odd byte offset (one leading
    // stray byte, same setup as `utf16le_ascii_detects_odd_byte_offset`), and
    // (2) the chunk boundary (at byte 8) falls in the middle of the code
    // unit for 'O' -- i.e. the split point itself is not code-unit-aligned
    // with the *parity* the string happens to use, which is exactly the
    // case `scan_parity`'s "may spill one byte into the next chunk" logic
    // exists to handle.
    let (input_path, _input_guard) = temp_path("utf16-boundary-input");
    let (out0, _out0_guard) = temp_path("utf16-boundary-0");
    let (out1, _out1_guard) = temp_path("utf16-boundary-1");
    let mut bytes = vec![0x00];
    bytes.extend_from_slice(&utf16le("HELLO WORLD"));
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(5);
    let cancel = AtomicBool::new(false);
    let (_r0, file0) =
        utf16le_ascii::scan(&file, bytes.len() as u64, &Chunk { offset: 0, len: 8 }, &cfg, &out0, &cancel).unwrap();
    let (_r1, file1) = utf16le_ascii::scan(
        &file,
        bytes.len() as u64,
        &Chunk {
            offset: 8,
            len: bytes.len() as u64 - 8,
        },
        &cfg,
        &out1,
        &cancel,
    )
    .unwrap();

    let text = merge_test_encoding_chunks(vec![file0, file1], cfg.min_cch());
    assert!(text.contains("\tUTF16LE\tHELLO WORLD"), "{text}");
}

#[test]
fn utf16le_ascii_cch_and_cb_are_distinct() {
    // `cch` (character count) and `cb` (byte count) must be tracked
    // separately for UTF-16LE, since each character is 2 bytes: "HELLO"
    // should report cch=5 but cb=10, unlike ASCII where the two always
    // coincide.
    let (input_path, _input_guard) = temp_path("utf16-length-input");
    let (out, _out_guard) = temp_path("utf16-length-output");
    let bytes = utf16le("HELLO");
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(5);
    let (_records, mut result_file) = utf16le_ascii::scan(
        &file,
        bytes.len() as u64,
        &Chunk {
            offset: 0,
            len: bytes.len() as u64,
        },
        &cfg,
        &out,
        &AtomicBool::new(false),
    )
    .unwrap();

    let rec = read_records(&mut result_file).into_iter().find(|r| r.data.text_of() == "HELLO").unwrap();
    assert_eq!(rec.cch, 5);
    assert_eq!(rec.cb, 10);
}

#[test]
fn utf16le_ascii_chunk_stream_is_sorted_across_parities() {
    // `utf16le_ascii::scan` internally scans even and odd parity separately and
    // merges them (see `scan_parity`/`merge_sorted_record_files`). This
    // test places one string at an even offset ("ABC" at 0) and another at
    // an odd offset ("DEF" at 7) within the same chunk, and checks that
    // the scanner's own final output -- read directly via `read_records`,
    // before any external merge step -- already comes out sorted by
    // offset across the two parities, not grouped by which parity pass
    // found it.
    let (input_path, _input_guard) = temp_path("utf16-parity-order-input");
    let (out, _out_guard) = temp_path("utf16-parity-order-output");

    let mut bytes = vec![0u8; 15];
    bytes[0..6].copy_from_slice(&utf16le("ABC"));
    bytes[7..13].copy_from_slice(&utf16le("DEF"));
    bytes[13] = 0;
    bytes[14] = 0;
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(3);
    let (_records, mut result_file) = utf16le_ascii::scan(
        &file,
        bytes.len() as u64,
        &Chunk {
            offset: 0,
            len: bytes.len() as u64,
        },
        &cfg,
        &out,
        &AtomicBool::new(false),
    )
    .unwrap();

    let records = read_records(&mut result_file);
    let offsets: Vec<u64> = records.iter().map(|r| r.offset).collect();
    assert_eq!(offsets, vec![0, 7]);
    assert_eq!(records[0].data.text_of(), "ABC");
    assert_eq!(records[1].data.text_of(), "DEF");
}

#[test]
fn utf16le_ascii_run_spans_multiple_read_blocks() {
    // UTF-16LE analogue of the previous test: a run of `A` characters
    // spanning more than 2 full `block_units`-sized read blocks (see
    // `scan_parity` in scanner/utf16le.rs), confirming the run survives
    // crossing internal block boundaries as a single record.
    let (input_path, _input_guard) = temp_path("utf16-multiblock-input");
    let run_chars = (READ_BUFFER_SIZE / 2) * 2 + 91;
    let mut bytes = vec![0u8, 0u8];
    bytes.extend(std::iter::repeat_with(|| utf16le("A")).take(run_chars).flatten());
    bytes.extend_from_slice(&[0, 0]);
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let (out, _out_guard) = temp_path("utf16-multiblock-output");
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) =
        utf16le_ascii::scan(&file, bytes.len() as u64, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].offset, 2);
    assert_eq!(records[0].cch, run_chars as u64);
    assert!(records[0].data.text_of().chars().all(|c| c == 'A'));
}

#[test]
fn utf16le_ascii_exact_chunk_size_boundaries_and_odd_eof() {
    // UTF-16LE analogue of the previous sweep, with an extra wrinkle: at
    // odd byte sizes (7, 9, 15, 17) the file ends mid-code-unit (an
    // incomplete trailing byte), so `expected_cch` is computed as
    // `bytes.len() / 2` (integer division truncates the dangling half
    // code unit). At size 7 that yields `expected_cch == 0`, meaning the
    // file is too short to contain even one full code unit -- the test
    // branches to assert no output at all in that case, rather than
    // treating it as just another N-character case like the rest.
    for size in [7usize, 8, 9, 15, 16, 17] {
        let (input_path, _input_guard) = temp_path(&format!("utf16-size-{size}"));
        let full = utf16le("ABCDEFGHIJKLMNOPQRSTUVWX");
        let bytes = full[..size.min(full.len())].to_vec();
        fs::write(&input_path, &bytes).unwrap();

        let text = scan_all_chunks(&input_path, 8, true, 1, &format!("utf16-size-{size}"));
        let lines: Vec<_> = text.lines().collect();
        let expected_cch = (bytes.len() / 2) as u64;
        if expected_cch == 0 {
            assert!(lines.is_empty(), "size={size}: {text}");
        } else {
            assert_eq!(lines.len(), 1, "size={size}: {text}");
            let record = lines[0].split('\t').nth(2).unwrap();
            assert_eq!(record.chars().count() as u64, expected_cch, "size={size}: {text}");
            assert_eq!(
                record.as_bytes(),
                b"ABCDEFGHIJKLMNOPQRSTUVWX"[..expected_cch as usize].as_ref(),
                "size={size}: {text}"
            );
        }
    }
}

#[test]
fn empty_and_tiny_files_do_not_produce_records() {
    // Degenerate-size sweep (0 to 3 bytes) to make sure the scanners don't
    // panic or misbehave on inputs too small to contain anything
    // meaningful. For UTF-16LE,
    // sizes below 2 bytes can't contain even one code unit, so output must
    // be empty; sizes 2-3 aren't asserted on here (3 bytes is one code
    // unit plus one dangling byte, which the exact-boundary test above
    // covers in more detail).
    for size in 0usize..=3 {
        let (input_path, _input_guard) = temp_path(&format!("tiny-{size}"));
        let bytes = vec![b'A'; size];
        fs::write(&input_path, &bytes).unwrap();

        let utf16 = scan_all_chunks(&input_path, 8, true, 1, &format!("tiny-utf16-{size}"));
        if size < 2 {
            assert!(utf16.is_empty(), "UTF16LE size={size}: {utf16}");
        }
    }
}

#[test]
fn utf16le_ascii_code_unit_crosses_exact_chunk_boundary() {
    // Targets a very specific edge case: the chunk boundary (chunk size 8)
    // falls *exactly* between the two bytes of a single code unit (the
    // code unit for 'A', starting at byte 7, straddling bytes 7-8). This
    // is the scenario `scan_parity`'s `max_start`/`last_start` handling
    // exists for -- allowing a code unit to start one byte before the
    // chunk technically ends so its second byte can be picked up from the
    // next chunk -- confirmed here by checking the full "ABCDEFG" string
    // comes out joined and correctly anchored at offset 7.
    let (input_path, _input_guard) = temp_path("utf16-exact-boundary-input");
    let mut bytes = vec![0u8; 7];
    bytes.extend_from_slice(&utf16le("A"));
    bytes.extend_from_slice(&utf16le("BCDEFG"));
    fs::write(&input_path, &bytes).unwrap();

    let text = scan_all_chunks(&input_path, 8, true, 1, "utf16-exact-boundary");
    let lines: Vec<_> = text.lines().collect();
    assert!(!lines.is_empty(), "{text}");
    assert!(lines.iter().any(|line| line.contains("\tUTF16LE\tABCDEFG")), "{text}");
    assert_eq!(lines[0][..20].parse::<u64>().unwrap(), 7);
}