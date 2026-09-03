use std::io::{self, Read, Write};
use crate::encoding::InputEncoding;

/// Magic value used to identify the intermediate record format ("PST1").
/// It lets readers reject data that is not a valid record stream.
pub(crate) const RECORD_MAGIC: u32 = 0x31545350; // "PST1"

/// A record's payload: either already-decoded text, or raw,
/// not-yet-decoded bytes.
///
/// Self-synchronizing encodings (see
/// `InputEncoding::is_self_synchronizing`) never produce `Raw`: every
/// byte position unambiguously tells you where a character starts, so a
/// chunk-boundary fragment can be decoded immediately at scan time and
/// joined afterward as plain text, exactly as before this variant existed.
///
/// Non-self-synchronizing encodings (CP932/Shift_JIS, GBK, GB18030,
/// EUC-KR, Big5 and ISO-2022-JP) can't safely make that decision at scan
/// time.
/// A byte sitting at a chunk boundary might be the trailing byte of a
/// character the *previous* chunk started, the leading byte of a fresh
/// character, or a standalone character -- and which one it truly is can
/// depend on whether it ends up joined with the previous chunk's pending
/// fragment, which isn't known until `outputter` gets there. For these
/// encodings, the scanner defers both the character-boundary decision
/// *and* the decode itself, storing the untouched raw bytes here;
/// `outputter` (via `scanner::segment_raw`) performs the real
/// segmentation and decode once it knows how (or whether) the fragment
/// resolves. See `scanner::dbcs`'s module doc comment and `dbcs::scan`
/// for the full reasoning.
#[derive(Debug)]
pub(crate) enum RecordData {
    /// Already-decoded text, ready to write out directly.
    Text(String),
    /// Raw bytes in the record's original encoding, not yet decoded or
    /// even split into individual characters. Only ever produced for
    /// chunk-boundary-touching fragments of a non-self-synchronizing
    /// encoding.
    Raw(Vec<u8>),
}

impl RecordData {
    /// Extracts the decoded text, for a record known to carry `Text`.
    ///
    /// Only valid for records from a self-synchronizing encoding (see
    /// `InputEncoding::is_self_synchronizing`), which never produce
    /// `RecordData::Raw`. Callers must therefore already know the
    /// record's encoding is self-synchronizing -- which in practice they
    /// do, since every caller is either a scanner reading back its own
    /// output or a test that wrote the record itself.
    ///
    /// # Panics
    ///
    /// Panics if the payload is still `Raw`. That would mean a boundary
    /// fragment reached a caller that never resolved it through
    /// `scanner::segment_raw`, which is a bug rather than a runtime
    /// condition worth returning an error for.
    pub(crate) fn text_of(&self) -> &str {
        match &self {
            RecordData::Text(s) => s,
            RecordData::Raw(_) => unreachable!(
                "text_of() called on a Raw record -- only self-synchronizing encodings should call this, \
                 and they never produce Raw records (see InputEncoding::is_self_synchronizing)"
            ),
        }
    }
}

/// A candidate string found by a scanner, with enough metadata for the
/// merger/outputter to reconstruct strings that cross chunk boundaries.
///
/// Fields are `pub(crate)`: every module inside this crate (scanner,
/// merger, outputter) constructs and reads these directly. Only the
/// getters below are part of this crate's public API.
#[derive(Debug)]
pub struct MatchRecord {
    pub(crate) offset: u64,
    pub(crate) cb: u64,
    /// Character count. For a `RecordData::Raw` record, segmentation
    /// hasn't happened yet, so this is not yet meaningful and is always
    /// `0` as a placeholder -- callers must not rely on `cch` for a `Raw`
    /// record until it has been resolved via `scanner::segment_raw`
    /// (after which it exists only as freshly produced `Text` records).
    pub(crate) cch: u64,
    pub(crate) encoding: InputEncoding,
    pub(crate) starts_at_chunk: bool,
    pub(crate) ends_at_chunk: bool,
    pub(crate) data: RecordData,
}

impl MatchRecord {
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn cch(&self) -> u64 {
        self.cch
    }

    pub fn encoding(&self) -> InputEncoding {
        self.encoding
    }
}

/// Same shape as `MatchRecord`, but `data` borrows from a caller-owned
/// scratch buffer instead of allocating a `String`/`Vec<u8>`. Used on the
/// hot, non-boundary path in the outputter (see outputter.rs) to avoid a
/// per-record heap allocation.
pub(crate) struct RawRecord<'a> {
    pub(crate) offset: u64,
    pub(crate) cb: u64,
    pub(crate) cch: u64,
    pub(crate) encoding: InputEncoding,
    pub(crate) starts_at_chunk: bool,
    pub(crate) ends_at_chunk: bool,
    pub(crate) data: RecordDataRef<'a>,
}

/// Borrowed counterpart to `RecordData`, mirroring its two variants.
/// `Clone`/`Copy` are cheap and correct here since both variants only
/// ever hold a borrowed slice (`&str`/`&[u8]`), themselves `Copy` --
/// deriving these lets callers match on `rec.data` through a shared
/// `&RawRecord` reference without needing to move anything out of it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RecordDataRef<'a> {
    Text(&'a str),
    Raw(&'a [u8]),
}


/// Appends a fragment that has already been verified as contiguous to `dst`.
/// The length metadata, boundary state, and payload are merged together.
///
/// `dst` and `src` must carry the *same* `RecordData` variant. This always
/// holds in practice: a given `InputEncoding` consistently produces either
/// always-`Text` boundary records (self-synchronizing encodings) or
/// always-`Raw` ones (non-self-synchronizing encodings) -- see
/// `InputEncoding::is_self_synchronizing` -- so two records of the same
/// encoding that both made it into `pending`/`boundary` can never disagree
/// on variant.
pub(crate) fn append_data(dst: &mut MatchRecord, src: MatchRecord) {
    dst.cb += src.cb;
    dst.ends_at_chunk = src.ends_at_chunk;
    match (&mut dst.data, src.data) {
        (RecordData::Text(dst_s), RecordData::Text(src_s)) => {
            dst.cch += src.cch;
            dst_s.push_str(&src_s);
        }
        (RecordData::Raw(dst_b), RecordData::Raw(src_b)) => {
            // `cch` stays a placeholder here -- real character counting
            // happens once `outputter` decodes+segments the combined raw
            // bytes via `scanner::segment_raw`.
            dst_b.extend_from_slice(&src_b);
        }
        _ => unreachable!(
            "append_data: mismatched RecordData variants for encoding {:?} -- every encoding \
             consistently produces either Text or Raw boundary records, never a mix",
            dst.encoding
        ),
    }
}

/// Serializes one `MatchRecord` into the binary intermediate-record format.
///
/// The record consists of a fixed-size header followed by the payload
/// bytes (UTF-8 text for `RecordData::Text`, or the original untouched
/// bytes for `RecordData::Raw`). Integer fields are stored in
/// little-endian order.
pub(crate) fn write_record(w: &mut impl Write, rec: &MatchRecord) -> io::Result<()> {
    // A non-self-synchronizing encoding defers its boundary-touching runs
    // as `Raw`, but still writes ordinary interior runs as `Text`, so only
    // the converse direction is an invariant worth asserting: a `Raw`
    // record must never come from a self-synchronizing encoding.
    debug_assert!(
        !matches!(rec.data, RecordData::Raw(_)) || !rec.encoding.is_self_synchronizing(),
        "encoding {:?} produced a RecordData::Raw record, but is_self_synchronizing() says it \
         should always be able to decode at scan time",
        rec.encoding,
    );

    // Only meaningful for Raw payloads: `cb` there is defined as exactly
    // the raw byte count (the payload literally IS the original bytes).
    // For Text payloads, `cb` counts bytes in the *original* input
    // encoding (e.g. 2 bytes per UTF-16LE code unit), while `data` is
    // always stored as UTF-8 -- the two byte counts are unrelated
    // whenever original-encoding bytes-per-char differs from UTF-8's, so
    // no such invariant holds there.
    if let RecordData::Raw(bytes) = &rec.data {
        debug_assert_eq!(
            rec.cb as usize,
            bytes.len(),
            "record.cb ({}) doesn't match the actual raw payload length ({}) for encoding {:?}",
            rec.cb,
            bytes.len(),
            rec.encoding,
        );
    }

    // Keep the header fixed-size so the reader can parse records without
    // searching for delimiters.
    let mut header = [0u8; 40]; // magic(4) + offset(8) + cb(8) + cch(8) + encoding(2) + flags(2) + data_len(8)

    header[0..4].copy_from_slice(&RECORD_MAGIC.to_le_bytes());
    header[4..12].copy_from_slice(&rec.offset.to_le_bytes());
    header[12..20].copy_from_slice(&rec.cb.to_le_bytes());
    header[20..28].copy_from_slice(&rec.cch.to_le_bytes());
    header[28..30].copy_from_slice(&(rec.encoding as u16).to_le_bytes());

    // Pack the three boolean/discriminant bits into one flags value:
    // bit 0 = starts_at_chunk, bit 1 = ends_at_chunk, bit 2 = is_raw
    // (payload is RecordData::Raw rather than RecordData::Text).
    let mut flags = 0u16;
    if rec.starts_at_chunk {
        flags |= 1;
    }
    if rec.ends_at_chunk {
        flags |= 2;
    }
    let data: &[u8] = match &rec.data {
        RecordData::Text(s) => s.as_bytes(),
        RecordData::Raw(b) => {
            flags |= 4;
            b.as_slice()
        }
    };
    header[30..32].copy_from_slice(&flags.to_le_bytes());
    header[32..40].copy_from_slice(&(data.len() as u64).to_le_bytes());

    w.write_all(&header)?;
    w.write_all(data)?;
    Ok(())
}

/// Reads and validates the fixed-size record header without reading its payload.
/// The payload length is returned separately so callers can choose how to store it.
pub(crate) fn read_record_header(
    r: &mut impl Read,
) -> io::Result<Option<(u64, u64, u64, InputEncoding, u16, u64)>> {
    // `Read::read` may return fewer bytes than requested, so keep reading
    // until the complete fixed-size header has been received.
    let mut header = [0u8; 40];
    let mut filled = 0;
    while filled < header.len() {
        match r.read(&mut header[filled..])? {
            0 if filled == 0 => return Ok(None), // Just EOF
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated record header",
                ));
            }
            n => filled += n,
        }
    }

    // Validate the magic before interpreting the remaining bytes as a record.
    let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
    if magic != RECORD_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid intermediate record magic",
        ));
    }
    let offset = u64::from_le_bytes(header[4..12].try_into().unwrap());
    let cb = u64::from_le_bytes(header[12..20].try_into().unwrap());
    let cch = u64::from_le_bytes(header[20..28].try_into().unwrap());
    let encoding = InputEncoding::try_from(u16::from_le_bytes(header[28..30].try_into().unwrap()))?;
    let flags = u16::from_le_bytes(header[30..32].try_into().unwrap());
    let data_len = u64::from_le_bytes(header[32..40].try_into().unwrap());

    Ok(Some((offset, cb, cch, encoding, flags, data_len)))
}

/// Reads a complete record and owns its payload.
///
/// Ownership is useful when the record must outlive the input buffer, such as
/// when a fragment is stored in `pending` across a chunk boundary.
pub(crate) fn read_record(r: &mut impl Read) -> io::Result<Option<MatchRecord>> {
    let Some((offset, cb, cch, encoding, flags, data_len)) = read_record_header(r)? else {
        return Ok(None);
    };

    if data_len > usize::MAX as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "record data is too large",
        ));
    }

    // Allocate exactly enough space for this record's serialized payload.
    let mut data = vec![0u8; data_len as usize];
    r.read_exact(&mut data)?;

    let is_raw = flags & 4 != 0;
    let data = if is_raw {
        RecordData::Raw(data)
    } else {
        // Text payloads are stored as UTF-8, regardless of the original
        // input encoding; Raw payloads are the encoding's own bytes and
        // are deliberately NOT validated as UTF-8 here.
        RecordData::Text(
            String::from_utf8(data)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "record is not UTF-8"))?,
        )
    };

    Ok(Some(MatchRecord {
        offset,
        cb,
        cch,
        encoding,
        starts_at_chunk: flags & 1 != 0,
        ends_at_chunk: flags & 2 != 0,
        data,
    }))
}

/// Reads a complete record while borrowing its payload from `scratch`.
///
/// This is the allocation-free hot path used by the outputter for ordinary
/// records. The returned `RawRecord` must be consumed before `scratch` is reused.
pub(crate) fn read_record_borrowed<'a>(
    r: &mut impl Read,
    scratch: &'a mut Vec<u8>,
) -> io::Result<Option<RawRecord<'a>>> {
    let Some((offset, cb, cch, encoding, flags, data_len)) = read_record_header(r)? else {
        return Ok(None);
    };

    if data_len > usize::MAX as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "record data is too large",
        ));
    }

    // Reuse the caller-owned buffer instead of allocating a new Vec for every record.
    scratch.clear();
    scratch.resize(data_len as usize, 0);
    r.read_exact(scratch)?;

    let is_raw = flags & 4 != 0;
    let data = if is_raw {
        RecordDataRef::Raw(scratch.as_slice())
    } else {
        let s = std::str::from_utf8(scratch)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "record is not UTF-8"))?;
        RecordDataRef::Text(s)
    };

    Ok(Some(RawRecord {
        offset,
        cb,
        cch,
        encoding,
        starts_at_chunk: flags & 1 != 0,
        ends_at_chunk: flags & 2 != 0,
        data,
    }))
}


