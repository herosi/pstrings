use super::support::*;
use crate::encoding::InputEncoding;
use crate::outputter::{output_merged_chunk, write_output_record};
use crate::record::{write_record, MatchRecord, RecordData};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Seek};
use std::path::Path;
use std::sync::atomic::AtomicBool;

// This module tests two layers of `outputter`:
//
//   - `write_output_record`: formatting a single `MatchRecord` as one line
//     of text output (offset, encoding name, data).
//   - `output_merged_chunk` (exercised indirectly through the
//     `merge_test_*` helpers in `support`): stitching boundary-fragment
//     records that were split across chunk edges back into single, complete
//     strings, using each record's `starts_at_chunk`/`ends_at_chunk` flags
//     and a `pending` map (keyed per encoding) of unresolved fragments
//     still waiting for their continuation in the next chunk.
//
// Most tests below build small, hand-crafted "scanner output" files (using
// `write_record`, the same intermediate format the real scanners produce)
// and then run them through the merge/output path to check the resulting
// text -- rather than testing `output_merged_chunk` directly -- since the
// boundary-joining behavior only actually matters in the context of a
// multi-chunk merge.


#[test]
fn output_encoding_field_has_no_padding() {
    // Baseline formatting check: offset is zero-padded to a fixed width
    // (so lines sort/align byte-for-byte), but encoding name and data are
    // written as-is with no padding of their own.
    let mut out = Vec::new();
    let rec = MatchRecord {
        offset: 12,
        cb: 5,
        cch: 5,
        encoding: InputEncoding::Ascii,
        starts_at_chunk: false,
        ends_at_chunk: false,
        data: RecordData::Text("hello".into()),
    };
    let mut scratch: Vec<u8> = Vec::with_capacity(256);
    write_output_record(&mut out, &rec, false, &mut scratch).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap().trim_end_matches(['\r', '\n']),
        "00000000000000000012\tASCII\thello"
    );
}

#[test]
fn write_output_record_offset_padding_boundaries() {
    // Confirms the offset field is always exactly 20 characters wide (the
    // decimal digit count of u64::MAX), covering the extremes: 0, a
    // single-digit value, the first double-digit value, and u64::MAX
    // itself, which should need no padding at all.
    let cases: &[(u64, &str)] = &[
        (0, "00000000000000000000"),
        (9, "00000000000000000009"),
        (10, "00000000000000000010"),
        (u64::MAX, "18446744073709551615"),
    ];

    for &(offset, expected_offset_str) in cases {
        let mut out = Vec::new();
        let rec = MatchRecord {
            offset,
            cb: 1,
            cch: 1,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: false,
            ends_at_chunk: false,
            data: RecordData::Text("x".into()),
        };
        let mut scratch: Vec<u8> = Vec::with_capacity(256);
        write_output_record(&mut out, &rec, false, &mut scratch).unwrap();
        let text = String::from_utf8(out).unwrap();
        let offset_field = text.split('\t').next().unwrap();
        assert_eq!(offset_field, expected_offset_str, "offset={offset}");
    }
}

#[test]
fn same_encoding_boundary_fragments_are_joined() {
    // Two ASCII fragments from two consecutive chunks ("HELL" ending its
    // chunk, "O WORLD" starting the next) should be stitched into a single
    // "HELLO WORLD" record at the first fragment's offset, since
    // `ends_at_chunk`/`starts_at_chunk` mark them as a continuing pair.
    let (p0, _p0_guard) = temp_path("join-0");
    let (p1, _p1_guard) = temp_path("join-1");
    let mut a = rw_temp_file(&p0);
    let mut b = rw_temp_file(&p1);
    write_record(
        &mut a,
        &MatchRecord {
            offset: 10,
            cb: 4,
            cch: 4,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: true,
            ends_at_chunk: true,
            data: RecordData::Text("HELL".into()),
        },
    )
    .unwrap();
    write_record(
        &mut b,
        &MatchRecord {
            offset: 14,
            cb: 6,
            cch: 6,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: true,
            ends_at_chunk: false,
            data: RecordData::Text("O WORLD".into()),
        },
    )
    .unwrap();
    a.seek(std::io::SeekFrom::Start(0)).unwrap();
    b.seek(std::io::SeekFrom::Start(0)).unwrap();

    let text = merge_test_encoding_chunks(vec![a, b], 5);
    assert_eq!(text.trim_end_matches(['\r', '\n']), "00000000000000000010\tASCII\tHELLO WORLD");
}

#[test]
fn different_encoding_fragments_are_never_joined() {
    // Mirror of the previous test, but the two fragments are different
    // encodings (ASCII vs UTF16LE). Even though they're adjacent in offset
    // and one ends its chunk while the other starts the next, joining
    // across encodings would be meaningless (the bytes came from
    // fundamentally different scans), so they must stay as two separate
    // records, each carrying its own offset and encoding label.
    let (p0, _p0_guard) = temp_path("nojoin-0");
    let (p1, _p1_guard) = temp_path("nojoin-1");
    let mut a = rw_temp_file(&p0);
    let mut b = rw_temp_file(&p1);
    write_record(
        &mut a,
        &MatchRecord {
            offset: 10,
            cb: 4,
            cch: 4,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: true,
            ends_at_chunk: true,
            data: RecordData::Text("HELL".into()),
        },
    )
    .unwrap();
    write_record(
        &mut b,
        &MatchRecord {
            offset: 14,
            cb: 8,
            cch: 4,
            encoding: InputEncoding::Utf16leAscii,
            starts_at_chunk: true,
            ends_at_chunk: false,
            data: RecordData::Text("O WOR".into()),
        },
    )
    .unwrap();
    a.seek(std::io::SeekFrom::Start(0)).unwrap();
    b.seek(std::io::SeekFrom::Start(0)).unwrap();

    let text = merge_test_encoding_chunks(vec![a, b], 1);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\tASCII\tHELL"));
    assert!(lines[1].contains("\tUTF16LE\tO WOR"));
    let offsets: Vec<u64> = lines.iter().map(|line| line[..20].parse().unwrap()).collect();
    assert_eq!(offsets, vec![10, 14]);
}

#[test]
fn multiple_encoding_boundary_fragments_preserve_chunk_order() {
    // A fuller scenario spanning two chunks and both encodings at once:
    // an ASCII fragment and a UTF16LE fragment each have their own
    // boundary-continuation pending across chunk0 -> chunk1, and chunk1
    // additionally contains two independent (non-boundary) ASCII records.
    // Checks that per-encoding pending state doesn't interfere across
    // encodings, and that final output order still follows offset overall,
    // not encoding or file order.
    let (a0_path, _a0_guard) = temp_path("multi-boundary-ascii-0");
    let (a1_path, _a1_guard) = temp_path("multi-boundary-ascii-1");
    let (u0_path, _u0_guard) = temp_path("multi-boundary-utf16-0");
    let (u1_path, _u1_guard) = temp_path("multi-boundary-utf16-1");

    {
        // chunk0, ASCII stream: "HELL" does NOT touch either chunk edge
        // (starts_at_chunk/ends_at_chunk both false) -- a normal, complete,
        // non-boundary record that should pass through untouched.
        let mut f = BufWriter::new(File::create(&a0_path).unwrap());
        write_record(
            &mut f,
            &MatchRecord {
                offset: 10,
                cb: 4,
                cch: 4,
                encoding: InputEncoding::Ascii,
                starts_at_chunk: false,
                ends_at_chunk: false,
                data: RecordData::Text("HELL".into()),
            },
        )
        .unwrap();
    }
    {
        // chunk0, UTF16LE stream: "TEST" ends its chunk (but didn't start
        // it) -- becomes pending, expecting a continuation in chunk1.
        let mut f = BufWriter::new(File::create(&u0_path).unwrap());
        write_record(
            &mut f,
            &MatchRecord {
                offset: 16,
                cb: 4,
                cch: 4,
                encoding: InputEncoding::Utf16leAscii,
                starts_at_chunk: false,
                ends_at_chunk: true,
                data: RecordData::Text("TEST".into()),
            },
        )
        .unwrap();
    }
    {
        // chunk1, ASCII stream: two independent, non-boundary records
        // ("AAA" and "END"), neither of which touches the pending "HELL"
        // above (that one already resolved as complete within chunk0).
        let mut f = BufWriter::new(File::create(&a1_path).unwrap());
        for (offset, data, end) in [(20, "AAA", false), (40, "END", false)] {
            write_record(
                &mut f,
                &MatchRecord {
                    offset,
                    cb: data.len() as u64,
                    cch: data.len() as u64,
                    encoding: InputEncoding::Ascii,
                    starts_at_chunk: offset == 20,
                    ends_at_chunk: end,
                    data: RecordData::Text( data.into()),
                },
            )
            .unwrap();
        }
    }
    {
        // chunk1, UTF16LE stream: " VALUE" starts chunk1 -> resolves the
        // pending "TEST" from chunk0 into "TEST VALUE".
        let mut f = BufWriter::new(File::create(&u1_path).unwrap());
        write_record(
            &mut f,
            &MatchRecord {
                offset: 20,
                cb: 10,
                cch: 5,
                encoding: InputEncoding::Utf16leAscii,
                starts_at_chunk: true,
                ends_at_chunk: false,
                data: RecordData::Text(" VALUE".into()),
            },
        )
        .unwrap();
    }

    let text = merge_test_full(
        &[
            (InputEncoding::Ascii, vec![a0_path, a1_path]),
            (InputEncoding::Utf16leAscii, vec![u0_path, u1_path]),
        ],
        1,
    );
    // Final order is purely by offset (10, 16, 20, 40): the joined
    // "TEST VALUE" record reports at its *first* fragment's offset (16),
    // and the two independent ASCII records ("AAA" at 20, "END" at 40)
    // interleave correctly with it despite coming from different files.
    let lines: Vec<_> = text.lines().collect();
    let offsets: Vec<u64> = lines.iter().map(|line| line[..20].parse().unwrap()).collect();
    assert_eq!(offsets, vec![10, 16, 20, 40], "{text}");
    assert!(lines[0].contains("\tASCII\tHELL"), "{text}");
    assert!(lines[1].contains("\tUTF16LE\tTEST VALUE"), "{text}");
    assert!(lines[2].contains("\tASCII\tAAA"), "{text}");
    assert!(lines[3].contains("\tASCII\tEND"), "{text}");
}

#[test]
fn boundary_join_order_follows_final_offset_not_encoding_order() {
    // Stress-tests that output ordering is driven by each *joined* record's
    // resulting offset, not by input file order or encoding. Here the
    // UTF16LE fragment pair joins into "ABCDEFGH" (offset 6), while the
    // ASCII fragment pair joins into "LATER" (offset 16) -- and although
    // ASCII's own fragments were written first/lower in the u1/a1 files,
    // the UTF16LE join (lower final offset) must still be emitted first.
    let (a0_path, _a0_guard) = temp_path("order-ascii-0");
    let (a1_path, _a1_guard) = temp_path("order-ascii-1");
    let (u0_path, _u0_guard) = temp_path("order-utf16-0");
    let (u1_path, _u1_guard) = temp_path("order-utf16-1");

    {
        // chunk0, ASCII: "LATE" ends chunk0 -> pending, offset 16.
        let mut f = BufWriter::new(File::create(&a0_path).unwrap());
        write_record(
            &mut f,
            &MatchRecord {
                offset: 16,
                cb: 4,
                cch: 4,
                encoding: InputEncoding::Ascii,
                starts_at_chunk: false,
                ends_at_chunk: true,
                data: RecordData::Text("LATE".into()),
            },
        )
        .unwrap();
    }
    {
        // chunk0, UTF16LE: "ABCDEFG" ends chunk0 -> pending, offset 6
        // (lower than the ASCII fragment's offset 16, despite being the
        // second write in this test).
        let mut f = BufWriter::new(File::create(&u0_path).unwrap());
        write_record(
            &mut f,
            &MatchRecord {
                offset: 6,
                cb: 14,
                cch: 7,
                encoding: InputEncoding::Utf16leAscii,
                starts_at_chunk: false,
                ends_at_chunk: true,
                data: RecordData::Text("ABCDEFG".into()),
            },
        )
        .unwrap();
    }
    {
        // chunk1, ASCII: "R" starts chunk1 -> resolves "LATE" + "R" =
        // "LATER" at offset 16.
        let mut f = BufWriter::new(File::create(&a1_path).unwrap());
        write_record(
            &mut f,
            &MatchRecord {
                offset: 20,
                cb: 4,
                cch: 4,
                encoding: InputEncoding::Ascii,
                starts_at_chunk: true,
                ends_at_chunk: false,
                data: RecordData::Text("R".into()),
            },
        )
        .unwrap();
    }
    {
        // chunk1, UTF16LE: "H" starts chunk1 -> resolves "ABCDEFG" + "H" =
        // "ABCDEFGH" at offset 6.
        let mut f = BufWriter::new(File::create(&u1_path).unwrap());
        write_record(
            &mut f,
            &MatchRecord {
                offset: 20,
                cb: 2,
                cch: 1,
                encoding: InputEncoding::Utf16leAscii,
                starts_at_chunk: true,
                ends_at_chunk: false,
                data: RecordData::Text("H".into()),
            },
        )
        .unwrap();
    }

    let text = merge_test_full(
        &[
            (InputEncoding::Ascii, vec![a0_path, a1_path]),
            (InputEncoding::Utf16leAscii, vec![u0_path, u1_path]),
        ],
        1,
    );
    let lines: Vec<_> = text.lines().collect();
    let offsets: Vec<u64> = lines.iter().map(|line| line[..20].parse().unwrap()).collect();

    // UTF16LE's joined record (offset 6) sorts before ASCII's (offset 16),
    // confirming ordering is by final joined offset, not by which stream
    // or file the fragments happened to come from.
    assert_eq!(offsets, vec![6, 16], "{text}");
    assert!(lines[0].contains("\tUTF16LE\tABCDEFGH"), "{text}");
    assert!(lines[1].contains("\tASCII\tLATER"), "{text}");
}

/// When, within one chunk's boundary-collection phase, an incoming `pending`
/// fragment is resolved (joined and written) *and* a brand-new fragment 
/// immediately becomes pending again (because the record that broke the 
/// collection loop itself reaches that chunk's end), the newly-set `pending` 
/// entry was being terminated and written out immediately instead of waiting
/// for the next chunk's continuation -- silently splitting a string that 
/// should have stayed joined.
///
/// Layout (single encoding, ASCII, is enough to trigger it):
///   chunk0 (offset 0-7):   "WXYZ" reaches chunk0's end -> pending.
///   chunk1 (offset 8-15):  "AB" at the chunk's start joins pending into
///                          "WXYZAB" (a complete, non-continuing string,
///                          written here); "MNOP" starts mid-chunk1 and
///                          itself reaches chunk1's end -> pending again.
///   chunk2 (offset 16-23): "QRST" at the chunk's start continues "MNOP".
///
/// Expected: "WXYZAB" and "MNOPQRST" as two joined records. Before the fix,
/// this produced "WXYZAB", "MNOP", and "QRST" as three records instead.
#[test]
fn pending_set_while_resolving_boundary_is_not_terminated_early() {
    let (a0_path, _a0_guard) = temp_path("pending-reset-0");
    let (a1_path, _a1_guard) = temp_path("pending-reset-1");
    let (a2_path, _a2_guard) = temp_path("pending-reset-2");

    let write = |path: &Path, recs: &[MatchRecord]| {
        let mut f = BufWriter::new(File::create(path).unwrap());
        for rec in recs {
            write_record(&mut f, rec).unwrap();
        }
    };

    write(
        &a0_path,
        &[MatchRecord {
            offset: 4,
            cb: 4,
            cch: 4,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: false,
            ends_at_chunk: true,
            data: RecordData::Text("WXYZ".into()),
        }],
    );
    write(
        &a1_path,
        &[
            // "AB" starts chunk1 -> resolves the incoming pending "WXYZ"
            // into "WXYZAB" (does NOT end chunk1, so this join is final,
            // not itself carried forward).
            MatchRecord {
                offset: 8,
                cb: 2,
                cch: 2,
                encoding: InputEncoding::Ascii,
                starts_at_chunk: true,
                ends_at_chunk: false,
                data: RecordData::Text("AB".into()),
            },
            // "MNOP" starts mid-chunk1 (not at its very start, so it's
            // unrelated to the "WXYZAB" join above) and reaches chunk1's
            // end -> this is the fragment that must become the *new*
            // pending entry, and must NOT be flushed early just because
            // resolving the previous pending entry happened in the same
            // chunk pass.
            MatchRecord {
                offset: 12,
                cb: 4,
                cch: 4,
                encoding: InputEncoding::Ascii,
                starts_at_chunk: false,
                ends_at_chunk: true,
                data: RecordData::Text("MNOP".into()),
            },
        ],
    );
    write(
        &a2_path,
        &[MatchRecord {
            offset: 16,
            cb: 4,
            cch: 4,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: true,
            ends_at_chunk: false,
            data: RecordData::Text("QRST".into()),
        }],
    );

    let text = merge_test_full(&[(InputEncoding::Ascii, vec![a0_path, a1_path, a2_path])], 1);
    let lines: Vec<_> = text.lines().collect();

    // The bug being guarded against would have produced 3 lines
    // ("WXYZAB", "MNOP", "QRST") instead of 2 ("WXYZAB", "MNOPQRST").
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "WXYZAB", "{text}");
    assert_eq!(lines[1].split('\t').nth(2).unwrap(), "MNOPQRST", "{text}");
}

#[test]
fn output_merged_chunk_stops_when_cancelled() {
    // Unlike the scanner/merger cancellation paths (which stop early and
    // return their partial results as `Ok`), `output_merged_chunk` is
    // expected to surface cancellation as an `Interrupted` error to its
    // caller -- this is the final output stage, so "stopping early" here
    // means the run should abort with an error rather than silently
    // truncating the user-visible output.
    let (path, _guard) = temp_path("cancel-output-merged");
    let mut input = rw_temp_file(&path);
    for i in 0..10_000u64 {
        write_record(
            &mut input,
            &MatchRecord {
                offset: i * 2,
                cb: 1,
                cch: 1,
                encoding: InputEncoding::Ascii,
                starts_at_chunk: false,
                ends_at_chunk: false,
                data: RecordData::Text("x".into()),
            },
        )
        .unwrap();
    }
    input.seek(std::io::SeekFrom::Start(0)).unwrap();

    let mut output = Vec::new();
    let mut pending = HashMap::new();
    // Cancelled from the very start (before any record is processed), so
    // this also confirms the cancellation check happens before doing any
    // work, not just partway through a large input.
    let cancelled = AtomicBool::new(true);
    let result = output_merged_chunk(input, 0, u64::MAX, &mut pending, 1, &mut output, false, &cancelled);
    assert!(result.is_err(), "should return an error when already cancelled");
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
}

#[test]
fn boundary_record_waits_for_next_chunk_only_for_continuation() {
    // Checks that a pending fragment is only resolved by a record that
    // *actually* starts at the next chunk's boundary (`starts_at_chunk`),
    // and is otherwise carried forward untouched. Here ASCII's "ABCD" +
    // "EFGH" (both boundary-marked) correctly join into "ABCDEFGH", while
    // a same-offset-region UTF16LE fragment ("TEST", not chunk-boundary at
    // either end within the files given) stays a separate, unjoined
    // record rather than being pulled into the ASCII join.
    let (a0_path, _a0_guard) = temp_path("boundary-wait-ascii-0");
    let (a1_path, _a1_guard) = temp_path("boundary-wait-ascii-1");
    let (u0_path, _u0_guard) = temp_path("boundary-wait-utf16-0");
    let (u1_path, _u1_guard) = temp_path("boundary-wait-utf16-1");
    // u0 is deliberately empty: this encoding has no fragment pending out
    // of chunk0, only chunk1's "TEST" record, so there is nothing for it
    // to join with.
    File::create(&u0_path).unwrap();

    let write = |path: &Path, recs: &[MatchRecord]| {
        let mut f = BufWriter::new(File::create(path).unwrap());
        for rec in recs {
            write_record(&mut f, rec).unwrap();
        }
    };

    write(
        &a0_path,
        &[MatchRecord {
            offset: 4,
            cb: 4,
            cch: 4,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: false,
            ends_at_chunk: true,
            data: RecordData::Text("ABCD".into()),
        }],
    );
    write(
        &a1_path,
        &[MatchRecord {
            offset: 8,
            cb: 4,
            cch: 4,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: true,
            ends_at_chunk: false,
            data: RecordData::Text("EFGH".into()),
        }],
    );
    write(
        &u1_path,
        &[MatchRecord {
            offset: 12,
            cb: 8,
            cch: 4,
            encoding: InputEncoding::Utf16leAscii,
            // Neither boundary flag set: a normal, self-contained record
            // that must pass through unchanged, never touched by ASCII's
            // pending/join logic.
            starts_at_chunk: false,
            ends_at_chunk: false,
            data: RecordData::Text("TEST".into()),
        }],
    );

    let text = merge_test_full(
        &[
            (InputEncoding::Ascii, vec![a0_path, a1_path]),
            (InputEncoding::Utf16leAscii, vec![u0_path, u1_path]),
        ],
        1,
    );

    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0].split('\t').nth(2).unwrap(), "ABCDEFGH", "{text}");
    assert_eq!(lines[1].split('\t').nth(2).unwrap(), "TEST", "{text}");
}

#[test]
fn same_offset_different_encodings_are_both_emitted() {
    // Two complete (non-boundary), independent records from different
    // encodings that happen to share the exact same offset must both
    // appear in the output -- offset alone is not a dedup/merge key,
    // encoding is part of a record's identity too.
    let (ascii_p, _ascii_guard) = temp_path("same-offset-ascii");
    let (utf16_p, _utf16_guard) = temp_path("same-offset-utf16");
    let mut ascii_file = rw_temp_file(&ascii_p);
    let mut utf16_file = rw_temp_file(&utf16_p);

    let ascii = MatchRecord {
        offset: 100,
        cb: 5,
        cch: 5,
        encoding: InputEncoding::Ascii,
        data: RecordData::Text("HELLO".to_string()),
        starts_at_chunk: false,
        ends_at_chunk: false,
    };
    let utf16 = MatchRecord {
        offset: 100,
        cb: 10,
        cch: 5,
        encoding: InputEncoding::Utf16leAscii,
        data: RecordData::Text("WORLD".to_string()),
        starts_at_chunk: false,
        ends_at_chunk: false,
    };
    write_record(&mut ascii_file, &ascii).unwrap();
    write_record(&mut utf16_file, &utf16).unwrap();
    ascii_file.seek(std::io::SeekFrom::Start(0)).unwrap();
    utf16_file.seek(std::io::SeekFrom::Start(0)).unwrap();

    let text = merge_test_encoding_chunks(vec![ascii_file, utf16_file], 1);
    let lines: Vec<_> = text.lines().collect();

    assert_eq!(lines.len(), 2, "same-offset records must not be deduplicated: {text}");
    assert!(lines[0].contains("\tASCII\tHELLO"), "{text}");
    assert!(lines[1].contains("\tUTF16LE\tWORLD"), "{text}");
    assert!(lines[0].starts_with("00000000000000000100\t"), "{text}");
    assert!(lines[1].starts_with("00000000000000000100\t"), "{text}");
}

#[test]
fn same_offset_pending_and_current_different_encodings_are_both_emitted() {
    // Same guarantee as the previous test, but exercised through the
    // pending/current-chunk boundary path rather than two plain records:
    // one encoding has a fragment already resolved/pending at offset 100
    // (`ends_at_chunk: true`, waiting to see if it continues), and a
    // different encoding independently has a complete record at that same
    // offset in the current chunk. The two must not be conflated just
    // because they share an offset.
    let (current_p, _current_guard) = temp_path("same-offset-pending-current");
    let mut current_file = rw_temp_file(&current_p);
    let current = MatchRecord {
        offset: 100,
        cb: 10,
        cch: 5,
        encoding: InputEncoding::Utf16leAscii,
        data: RecordData::Text("WORLD".to_string()),
        starts_at_chunk: true,
        ends_at_chunk: false,
    };
    write_record(&mut current_file, &current).unwrap();
    current_file.seek(std::io::SeekFrom::Start(0)).unwrap();

    let (pending_p, _pending_guard) = temp_path("same-offset-pending");
    let mut pending_file = rw_temp_file(&pending_p);
    let pending = MatchRecord {
        offset: 100,
        cb: 5,
        cch: 5,
        encoding: InputEncoding::Ascii,
        data: RecordData::Text("HELLO".to_string()),
        starts_at_chunk: false,
        ends_at_chunk: true,
    };
    write_record(&mut pending_file, &pending).unwrap();
    pending_file.seek(std::io::SeekFrom::Start(0)).unwrap();

    let text = merge_test_encoding_chunks(vec![pending_file, current_file], 1);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2, "same-offset records must both survive: {text}");
    assert!(lines[0].contains("\tASCII\tHELLO"), "{text}");
    assert!(lines[1].contains("\tUTF16LE\tWORLD"), "{text}");
}