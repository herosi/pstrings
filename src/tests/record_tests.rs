use super::support::*;
use crate::encoding::InputEncoding;
use crate::record::{read_record, write_record, MatchRecord, RECORD_MAGIC, RecordData};
use std::io::{Seek, Write};

// Tests for `read_record`'s error handling on malformed/corrupt input.
// `write_record`/`read_record` are the crate's on-disk intermediate
// format (used for every temp file produced by scanners, the merger, and
// the outputter's pending state), so these guard against silent
// misinterpretation of bad data: each case constructs a specific way the
// bytes on disk could be wrong and checks that it's surfaced as a
// distinct, correctly-classified `io::Error` rather than, say, panicking,
// silently returning a garbage record, or misreporting the failure as a
// different error kind.

/// `InputEncoding`'s discriminant is written into every intermediate
/// record and read back by `TryFrom<u16>`, so the enum's numbering and
/// that conversion have to agree exactly.
///
/// They once silently stopped agreeing: with implicit discriminants,
/// inserting `Big5` before `Windows1251` renumbered the latter from 8 to
/// 9, while `TryFrom` still mapped 8 to it. Records were written as one
/// encoding and read back as another -- Cyrillic text appeared under a
/// `BIG5` label, and a `segment_raw` call for `Windows1251` (which has no
/// arm there, being self-synchronizing) hit `unreachable!`.
///
/// The discriminants are explicit now, but that alone would not catch a
/// *missing* `TryFrom` arm for a newly added encoding. This does.
#[test]
fn encoding_discriminants_round_trip() {
    for &enc in InputEncoding::ALL {
        let n = enc as u16;
        let back = InputEncoding::try_from(n).unwrap_or_else(|e| {
            panic!("encoding {enc:?} has discriminant {n}, which TryFrom<u16> rejects: {e}")
        });
        assert_eq!(
            back, enc,
            "encoding {enc:?} has discriminant {n}, but that number maps back to {back:?}"
        );
    }
}

/// A second, independent guard: two encodings sharing a discriminant would
/// round-trip individually (each mapping to whichever one `TryFrom` names)
/// yet still corrupt records. Checking the numbers are distinct catches
/// that, and checking `ALL` is complete catches a variant added to the
/// enum but forgotten here -- which would otherwise silently shrink the
/// coverage of the test above.
#[test]
fn encoding_discriminants_are_distinct_and_all_is_complete() {
    let mut seen = std::collections::BTreeMap::new();
    for &enc in InputEncoding::ALL {
        if let Some(other) = seen.insert(enc as u16, enc) {
            panic!("{enc:?} and {other:?} share discriminant {}", enc as u16);
        }
    }

    // `ALL` must list every variant. `clap::ValueEnum` derives its own
    // exhaustive list, so it can be used as the independent source of
    // truth rather than a hand-updated count.
    use clap::ValueEnum;
    assert_eq!(
        InputEncoding::ALL.len(),
        <InputEncoding as ValueEnum>::value_variants().len(),
        "InputEncoding::ALL is missing at least one variant"
    );
}

#[test]
fn read_record_rejects_bad_magic() {
    let (path, _guard) = temp_path("bad-magic");
    let mut input = rw_temp_file(&path);
    // Full-length header (40 bytes) so read_exact succeeds, but with an
    // invalid magic value so the magic check itself is exercised.
    let mut bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
    bytes.extend_from_slice(&[0u8; 36]);
    input.write_all(&bytes).unwrap();
    input.seek(std::io::SeekFrom::Start(0)).unwrap();

    // Distinguishes "this isn't a record file / the header is corrupt in a
    // way that's structurally complete but semantically wrong" from a
    // plain I/O short-read: `InvalidData`, not `UnexpectedEof`, since the
    // full header WAS present and readable -- its *content* is what's
    // wrong.
    let err = read_record(&mut input).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn read_record_rejects_truncated_header() {
    let (path, _guard) = temp_path("truncated-header");
    let mut input = rw_temp_file(&path);
    // Valid magic, but the file ends partway through the fixed header.
    let mut bytes = RECORD_MAGIC.to_le_bytes().to_vec();
    bytes.extend_from_slice(&[0u8; 10]);
    input.write_all(&bytes).unwrap();
    input.seek(std::io::SeekFrom::Start(0)).unwrap();

    // Here the magic itself is fine, but there simply aren't enough bytes
    // to read the rest of the fixed-size header -- a true short read, so
    // this must be reported as `UnexpectedEof` (distinct from the
    // `InvalidData` case above, which had a complete header to inspect).
    let err = read_record(&mut input).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn read_record_rejects_truncated_data() {
    let (path, _guard) = temp_path("truncated-data");
    let mut input = rw_temp_file(&path);
    // Write a valid header claiming 100 bytes of data, but supply none.
    write_record(
        &mut input,
        &MatchRecord {
            offset: 0,
            cb: 100,
            cch: 100,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: false,
            ends_at_chunk: false,
            data: RecordData::Text("x".repeat(100)),
        },
    )
    .unwrap();

    // Truncate away exactly the data payload (everything after the
    // header), leaving a complete, valid, self-consistent header that
    // *claims* a 100-byte payload the file no longer actually contains.
    // This is a third distinct failure mode from the two above: the
    // header itself is entirely correct, but reading the payload it
    // promises runs off the end of the file.
    let full_len = input.metadata().unwrap().len();
    let header_len = full_len - 100;
    input.set_len(header_len).unwrap();
    input.seek(std::io::SeekFrom::Start(0)).unwrap();

    // A trustworthy header describing an untrustworthy file is still an
    // unexpected end-of-file, same classification as the truncated-header
    // case -- from the caller's perspective, both are "the file ended
    // before all the bytes the format promised were available."
    let err = read_record(&mut input).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
}