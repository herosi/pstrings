use super::support::*;
use crate::encoding::InputEncoding;
use crate::record::{write_record, MatchRecord, RecordData};
use std::fs::File;
use std::io::BufWriter;

// Tests for `merger::merge_chunk_encodings`'s core guarantee: given several
// already-offset-sorted per-encoding record streams, the merged output is a
// single stream sorted by offset (see `ChunkStream::next_record`'s
// `(offset, encoding.code())` sort key), regardless of which stream a
// record came from or how many records each stream contributes.

#[test]
fn chunk_streams_are_merged_by_offset() {
    // Simplest possible case: one record per stream, with the ASCII
    // stream's record (offset 100) at a *higher* offset than the UTF16LE
    // stream's record (offset 20). Confirms the merge actually reorders
    // across streams by offset rather than just concatenating them in
    // input order (which would wrongly put ASCII's offset-100 record
    // first, since it was written/listed first).
    let (a_path, _a_guard) = temp_path("merge-ascii");
    let (u_path, _u_guard) = temp_path("merge-utf16");
    {
        let mut a = BufWriter::new(File::create(&a_path).unwrap());
        write_record(
            &mut a,
            &MatchRecord {
                offset: 100,
                cb: 5,
                cch: 5,
                encoding: InputEncoding::Ascii,
                starts_at_chunk: false,
                ends_at_chunk: false,
                data: RecordData::Text("ASCII".into()),
            },
        )
        .unwrap();
    }
    {
        let mut u = BufWriter::new(File::create(&u_path).unwrap());
        write_record(
            &mut u,
            &MatchRecord {
                offset: 20,
                cb: 10,
                cch: 5,
                encoding: InputEncoding::Utf16leAscii,
                starts_at_chunk: false,
                ends_at_chunk: false,
                data: RecordData::Text("HELLO".into()),
            },
        )
        .unwrap();
    }

    let text = merge_test_single_chunk(&[a_path, u_path], 1);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    // UTF16LE (offset 20) must come before ASCII (offset 100) in the
    // output, even though the ASCII file was passed/written first.
    assert!(lines[0].contains("\tUTF16LE\tHELLO"));
    assert!(lines[1].contains("\tASCII\tASCII"));
}

#[test]
fn k_way_merge_interleaves_multiple_records_per_encoding() {
    // A tougher version of the same guarantee: each of the two streams now
    // contributes *multiple* records, at offsets that genuinely interleave
    // with each other (10, 30, 50, 90 for ASCII vs. 20, 25, 60, 70 for
    // UTF16LE) rather than one stream's records all falling before or
    // after the other's. This exercises the actual k-way-merge stepping
    // logic in `ChunkStream::next_record` -- repeatedly advancing whichever
    // stream currently holds the next-smallest offset -- rather than a
    // single min-of-two comparison.
    let (a_path, _a_guard) = temp_path("kway-ascii");
    let (u_path, _u_guard) = temp_path("kway-utf16");

    {
        let mut a = BufWriter::new(File::create(&a_path).unwrap());
        for (offset, data) in [(10, "A10"), (30, "A30"), (50, "A50"), (90, "A90")] {
            write_record(
                &mut a,
                &MatchRecord {
                    offset,
                    cb: data.len() as u64,
                    cch: data.chars().count() as u64,
                    encoding: InputEncoding::Ascii,
                    starts_at_chunk: false,
                    ends_at_chunk: false,
                    data: RecordData::Text(data.into()),
                },
            )
            .unwrap();
        }
    }
    {
        let mut u = BufWriter::new(File::create(&u_path).unwrap());
        for (offset, data) in [(20, "U20"), (25, "U25"), (60, "U60"), (70, "U70")] {
            write_record(
                &mut u,
                &MatchRecord {
                    offset,
                    cb: data.len() as u64,
                    cch: data.chars().count() as u64,
                    encoding: InputEncoding::Utf16leAscii,
                    starts_at_chunk: false,
                    ends_at_chunk: false,
                    data: RecordData::Text(data.into()),
                },
            )
            .unwrap();
        }
    }

    let text = merge_test_single_chunk(&[a_path, u_path], 1);
    let offsets: Vec<u64> = text.lines().map(|line| line[..20].parse::<u64>().unwrap()).collect();
    // The merged offsets must be in strictly ascending order across both
    // streams combined -- proof that the merge correctly steps back and
    // forth between streams rather than draining one before touching the
    // other.
    assert_eq!(offsets, vec![10, 20, 25, 30, 50, 60, 70, 90]);

    // Cross-checks the same interleaving from the encoding-label side: the
    // pattern of which stream "wins" at each step (A, U, U, A, A, U, U, A)
    // matches exactly what sorting the two offset lists together would
    // produce, confirming the offsets above are actually tied to the
    // records they claim to be (not just coincidentally in order).
    let encodings: Vec<&str> = text.lines().map(|line| line.split('\t').nth(1).unwrap()).collect();
    assert_eq!(
        encodings,
        vec!["ASCII", "UTF16LE", "UTF16LE", "ASCII", "ASCII", "UTF16LE", "UTF16LE", "ASCII"]
    );
}