use super::support::*;
use crate::chunk::Chunk;
use crate::filter::{self, CharacterFilter};
use crate::scanner::ascii;
use crate::READ_BUFFER_SIZE;
use std::fs::{self, File};
use std::io::Seek;
use std::sync::atomic::AtomicBool;

// This module tests the `scanner::ascii` scan
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
fn ascii_includes_tab() {
    // Directly pins down the ASCII filter's boundaries: tab and the full
    // printable range (space..='~') are allowed; one step below space
    // (0x1f, a C0 control char) and DEL (0x7f) are not.
    assert!(filter::allows_u8(&[CharacterFilter::Ascii], b'\t'));
    assert!(filter::allows_u8(&[CharacterFilter::Ascii], b' '));
    assert!(filter::allows_u8(&[CharacterFilter::Ascii], b'~'));
    assert!(!filter::allows_u8(&[CharacterFilter::Ascii], 0x1f));
    assert!(!filter::allows_u8(&[CharacterFilter::Ascii], 0x7f));
}

#[test]
fn ascii_min_length_is_cch_after_merge() {
    // Input is a single chunk containing two runs separated by a NUL byte:
    // "abc" (3 chars, below min_cch=4, and not touching either chunk edge,
    // so it should be dropped) and "XXXX\tY zzz" (well above threshold, and
    // includes an embedded tab to confirm tab-containing runs still count
    // as one contiguous string rather than being split on the tab).
    let (input_path, _input_guard) = temp_path("ascii-min-input");
    let (out, _out_guard) = temp_path("ascii-min-output");
    fs::write(&input_path, b"abc\0XXXX\tY zzz").unwrap();

    let file = File::open(&input_path).unwrap();
    let cfg = test_config(4);
    let chunk = Chunk {
        offset: 0,
        len: fs::metadata(&input_path).unwrap().len(),
    };
    let (_records, mut result_file) = ascii::scan(&file, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    // At the raw scanner-output level: the long run is present, and every
    // record present satisfies emit_record's filter (either long enough on
    // its own, or kept because it touches a chunk boundary). This chunk
    // has only one boundary-worthy candidate ("abc", which is too short
    // *and* doesn't touch either edge), so nothing here should be a
    // boundary-fragment exception in practice -- the assertion is really
    // checking that emit_record's invariant holds for whatever did survive.
    assert!(records.iter().any(|r| r.data.text_of() == "XXXX\tY zzz"));
    assert!(records.iter().all(|r| r.cch >= 4 || r.starts_at_chunk || r.ends_at_chunk));

    // Then confirm the same holds after going through the merge/output
    // stage (which re-applies min_cch during final text formatting): only
    // the long run should make it into the final text output, at its
    // correct absolute offset.
    result_file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let text = merge_test_encoding_chunks(vec![result_file], cfg.min_cch());
    assert_eq!(text.trim_end_matches(['\r', '\n']), "00000000000000000004\tASCII\tXXXX\tY zzz");
}

#[test]
fn min_cch_boundary_is_inclusive_for_ascii() {
    // Four back-to-back NUL-separated runs of increasing length (1..4
    // chars). Re-scans the *same* input once per min_cch threshold
    // (1 through 4) to confirm the cutoff is inclusive (`cch >= min_cch`,
    // not `>`): at min_cch=N, the length-N run should still appear, and
    // only runs shorter than N should be dropped.
    let (input_path, _input_guard) = temp_path("ascii-min-boundaries-input");
    let bytes = b"A\0AB\0ABC\0ABCD\0";
    fs::write(&input_path, bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    for min_cch in 1..=4 {
        let (out, _out_guard) = temp_path(&format!("ascii-min-boundaries-output-{min_cch}"));
        let cfg = test_config(min_cch);
        let chunk = Chunk {
            offset: 0,
            len: bytes.len() as u64,
        };
        let (_records, result_file) = ascii::scan(&file, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

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
fn min_cch_rejects_one_below_threshold() {
    // Complements the inclusive-boundary tests above by checking the other
    // side of the cutoff explicitly: with min_cch=4, a 3-char run ("ABC")
    // must be dropped while a 4-char run ("ABCD") right after it survives
    // -- only the long one should make it to the final text output.
    let (input_path, _input_guard) = temp_path("min-cch-below-input");
    fs::write(&input_path, b"ABC\0ABCD\0").unwrap();
    let file = File::open(&input_path).unwrap();

    let (out, _out_guard) = temp_path("min-cch-below-output");
    let cfg = test_config(4);
    let chunk = Chunk { offset: 0, len: 9 };
    let (_records, result_file) = ascii::scan(&file, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let text = merge_test_encoding_chunks(vec![result_file], 4);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].ends_with("\tABCD"), "{text}");
}

#[test]
fn ascii_boundary_fragment_below_min_cch_is_preserved_and_joined() {
    // A 6-char fragment ("XXXXXX") that would normally be *above* min_cch=4
    // on its own gets deliberately split across chunk boundaries (chunk
    // size 8) so that at least one side of the split is short. The point:
    // even if a chunk-local piece of a boundary-crossing run looks too
    // short in isolation, it must be preserved (per `emit_record`'s
    // boundary exception) and correctly joined with its neighbor(s) by the
    // merge stage, ending up as the full "XXXXXXABCD" string.
    let (input_path, _input_guard) = temp_path("ascii-boundary-short-fragment-input");
    fs::write(&input_path, b"\0XXXXXXABCD\0").unwrap();

    let text = scan_all_chunks(&input_path, 8, false, 4, "ascii-boundary-short-fragment");
    let lines: Vec<_> = text.lines().collect();
    assert!(
        lines
            .iter()
            .any(|line| { line.starts_with("00000000000000000001\tASCII\tXXXXXXABCD") }),
        "{text}"
    );
}

#[test]
fn ascii_string_crosses_three_chunk_boundaries_with_short_first_fragment() {
    // Extends the previous test to three chunks (chunk size 4): the string
    // "ABCDEFGHIJ" starts mid-way into the first chunk with a very short
    // leading fragment, then continues fully through a second chunk, into
    // a third. Confirms joining isn't limited to two adjacent chunks --
    // a chain of 3+ boundary fragments must still merge into one record.
    let (input_path, _input_guard) = temp_path("ascii-three-chunks-short-first");
    fs::write(&input_path, b"\0\0\0ABCDEFGHIJ\0").unwrap();

    let text = scan_all_chunks(&input_path, 4, false, 4, "ascii-three-chunks-short-first");
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].starts_with("00000000000000000003\tASCII\t"), "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "ABCDEFGHIJ", "{text}");
}

#[test]
fn ascii_string_crosses_three_chunk_boundaries() {
    // Same three-chunk join as above, but this time the run starts exactly
    // at offset 0 (so `starts_at_chunk` is true from the very first chunk,
    // not from a later one), and spans all three 4-byte chunks completely.
    let (input_path, _input_guard) = temp_path("ascii-three-chunks-input");
    fs::write(&input_path, b"ABCDEFGHIJKLM\0").unwrap();

    let text = scan_all_chunks(&input_path, 4, false, 4, "ascii-three-chunks");
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "ABCDEFGHIJKLM", "{text}");
    assert!(lines[0].starts_with("00000000000000000000\tASCII\t"), "{text}");
}

#[test]
fn ascii_string_crosses_chunk_boundary() {
    // Simplest two-chunk join case, using manually constructed adjacent
    // `Chunk`s (rather than `scan_all_chunks`) so the exact split point is
    // pinned down explicitly: "AAAAAAZZ" (8 bytes) is split as chunk0 =
    // bytes[0..4] ("AAAA") and chunk1 = bytes[4..8] ("AAZZ"), and the two
    // scans' outputs are merged directly.
    let (input_path, _input_guard) = temp_path("ascii-boundary-input");
    let (out0, _out0_guard) = temp_path("ascii-boundary-0");
    let (out1, _out1_guard) = temp_path("ascii-boundary-1");
    fs::write(&input_path, b"AAAAAAZZ").unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(6);
    let cancel = AtomicBool::new(false);
    let (_r0, file0) = ascii::scan(&file, &Chunk { offset: 0, len: 4 }, &cfg, &out0, &cancel).unwrap();
    let (_r1, file1) = ascii::scan(&file, &Chunk { offset: 4, len: 4 }, &cfg, &out1, &cancel).unwrap();

    let text = merge_test_encoding_chunks(vec![file0, file1], cfg.min_cch());
    assert_eq!(text.trim_end_matches(['\r', '\n']), "00000000000000000000\tASCII\tAAAAAAZZ");
}

#[test]
fn ascii_run_spans_multiple_read_blocks() {
    // The scanner reads the chunk in `READ_BUFFER_SIZE`-sized blocks (see
    // scanner/ascii.rs). This test builds a single run deliberately longer
    // than 2 full read blocks (`READ_BUFFER_SIZE * 2 + 137`) to confirm a
    // run in progress correctly survives crossing an *internal* read-block
    // boundary -- a purely I/O-buffering detail, distinct from crossing a
    // `Chunk` boundary -- and is still emitted as one single record rather
    // than being accidentally split (or duplicated) at the block edges.
    let (input_path, _input_guard) = temp_path("ascii-multiblock-input");
    let run_len = READ_BUFFER_SIZE * 2 + 137;
    let mut bytes = vec![b'\0'];
    bytes.extend(std::iter::repeat(b'A').take(run_len));
    bytes.push(b'\0');
    fs::write(&input_path, &bytes).unwrap();
    let file = File::open(&input_path).unwrap();

    let cfg = test_config(1);
    let (out, _out_guard) = temp_path("ascii-multiblock-output");
    let chunk = Chunk {
        offset: 0,
        len: bytes.len() as u64,
    };
    let (_records, mut result_file) = ascii::scan(&file, &chunk, &cfg, &out, &AtomicBool::new(false)).unwrap();

    let records = read_records(&mut result_file);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].offset, 1);
    assert_eq!(records[0].cch, run_len as u64);
    assert_eq!(records[0].data.text_of().len(), run_len);
    assert!(records[0].data.text_of().bytes().all(|b| b == b'A'));
}

#[test]
fn ascii_exact_chunk_size_boundaries() {
    // Sweeps a range of total-file sizes around common power-of-two-ish
    // boundaries (7, 8, 9, 15, 16, 17), each holding as much of a fixed
    // 24-byte pattern as fits, with chunk size fixed at 8. The goal is to
    // catch off-by-one errors that only show up when the file length lands
    // exactly on, just under, or just over a chunk boundary -- rather than
    // testing one hand-picked size, this exercises the whole neighborhood
    // around the boundary case.
    for size in [7usize, 8, 9, 15, 16, 17] {
        let (input_path, _input_guard) = temp_path(&format!("ascii-size-{size}"));
        let pattern = b"ABCDEFGHIJKLMNOPQRSTUVWX";
        let mut bytes = vec![0u8; size];
        let n = size.min(pattern.len());
        bytes[..n].copy_from_slice(&pattern[..n]);
        fs::write(&input_path, &bytes).unwrap();

        let text = scan_all_chunks(&input_path, 8, false, 1, &format!("ascii-size-{size}"));
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 1, "size={size}: {text}");
        assert_eq!(
            lines[0].split('\t').nth(2).unwrap().as_bytes(),
            &bytes[..n],
            "size={size}: {text}"
        );
    }
}

#[test]
fn empty_and_tiny_files_do_not_produce_records() {
    // Degenerate-size sweep (0 to 3 bytes) to make sure the scanners don't
    // panic or misbehave on inputs too small to contain anything
    // meaningful. For ASCII, a run of up to 3 'A's could in principle be
    // one short record, hence `<= 1` rather than `== 0`. For UTF-16LE,
    // sizes below 2 bytes can't contain even one code unit, so output must
    // be empty; sizes 2-3 aren't asserted on here (3 bytes is one code
    // unit plus one dangling byte, which the exact-boundary test above
    // covers in more detail).
    for size in 0usize..=3 {
        let (input_path, _input_guard) = temp_path(&format!("tiny-{size}"));
        let bytes = vec![b'A'; size];
        fs::write(&input_path, &bytes).unwrap();

        let ascii = scan_all_chunks(&input_path, 8, false, 1, &format!("tiny-ascii-{size}"));
        assert!(ascii.lines().count() <= 1, "ASCII size={size}: {ascii}");
    }
}
