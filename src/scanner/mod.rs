pub(crate) mod ascii;
pub(crate) mod utf16le_ascii;
pub(crate) mod utf16le;
pub(crate) mod utf8;
pub(crate) mod iso2022jp;
pub(crate) mod dbcs;
pub(crate) mod cp932;
pub(crate) mod gbk;
pub(crate) mod gb18030;
pub(crate) mod euckr;
pub(crate) mod big5;
pub(crate) mod win1251;

use crate::READ_BUFFER_SIZE;
use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::InputEncoding;
use crate::record::MatchRecord;
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Scans one chunk of the input file for `encoding`, writing matches (in
/// this crate's intermediate record format, sorted by offset) to a fresh
/// temp file derived from `temp_path`, and returns (record count, that
/// file, rewound to the start).
///
/// To add a new encoding: add a variant to `InputEncoding` (and its
/// `name`, `ALL`, `TryFrom` and `is_self_synchronizing` arms -- see that
/// type's doc comment for the full checklist), add a
/// `scanner/<name>.rs` module with a `scan` function of this same shape,
/// and add one arm below. Nothing outside those two files needs to
/// change -- UNLESS the
/// new encoding is non-self-synchronizing (see
/// `InputEncoding::is_self_synchronizing`), in which case its `scan` will
/// produce `RecordData::Raw` boundary fragments, and one arm must also be
/// added to `segment_raw` below so `outputter` can resolve them.
pub fn scan(
    encoding: InputEncoding,
    file: &File,
    file_len: u64,
    chunk: &Chunk,
    cfg: &Config,
    temp_path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<(u64, File)> {
    // Single dispatch point: every encoding-specific scanner shares the same
    // (count, sorted temp file) contract, so callers never need to know
    // which encoding produced a given result.
    match encoding {
        InputEncoding::Ascii => ascii::scan(file, chunk, cfg, temp_path, cancelled),
        InputEncoding::Utf16leAscii => utf16le_ascii::scan(file, file_len, chunk, cfg, temp_path, cancelled),
        InputEncoding::Utf16le => utf16le::scan(file, file_len, chunk, cfg, temp_path, cancelled),
        InputEncoding::Utf8 => utf8::scan(file, file_len, chunk, cfg, temp_path, cancelled),
        InputEncoding::Iso2022Jp => iso2022jp::scan(file, file_len, chunk, cfg, temp_path, cancelled),
        InputEncoding::Cp932 => cp932::scan(file, file_len, chunk, cfg, temp_path, cancelled),
        InputEncoding::Gbk => gbk::scan(file, file_len, chunk, cfg, temp_path, cancelled),
        InputEncoding::Gb18030 => gb18030::scan(file, file_len, chunk, cfg, temp_path, cancelled),
        InputEncoding::EucKr => euckr::scan(file, file_len, chunk, cfg, temp_path, cancelled),
        InputEncoding::Big5 => big5::scan(file, file_len, chunk, cfg, temp_path, cancelled),
        InputEncoding::Windows1251 => win1251::scan(file, chunk, cfg, temp_path, cancelled),
    }
}

/// A single decoded, fully character-segmented fragment produced by
/// resolving a `RecordData::Raw` payload -- see `record::RecordData`'s doc
/// comment and `segment_raw` below. `start` is a byte offset *relative to
/// the buffer `segment_raw` was given*, not an absolute file offset;
/// callers add their own base offset.
pub(crate) struct ResolvedFragment {
    pub(crate) start: u64,
    pub(crate) cb: u64,
    pub(crate) cch: u64,
    pub(crate) data: String,
}

/// Decodes and character-segments a buffer of raw, not-yet-interpreted
/// bytes for a non-self-synchronizing `encoding` -- the counterpart to
/// `scan` for finishing the job `RecordData::Raw` deferred. Used only by
/// `outputter`, once it has determined what (if anything) a boundary
/// fragment joins with.
///
/// Returns every complete, printable run found in `bytes` as a
/// `ResolvedFragment`, plus any leftover bytes at the very end that still
/// end mid-character (a dangling lead byte with no trailing byte in
/// `bytes` to confirm or reject it) -- the caller is responsible for
/// carrying that leftover forward as a new pending fragment, since it may
/// take more than one further chunk to resolve (see
/// `outputter::resolve_for_output`'s doc comment).
///
/// Panics if called for a self-synchronizing encoding, since those never
/// produce `RecordData::Raw` in the first place -- there is nothing for
/// this function to be asked to resolve.
pub(crate) fn segment_raw(encoding: InputEncoding, bytes: &[u8]) -> (Vec<ResolvedFragment>, Vec<u8>) {
    match encoding {
        InputEncoding::Cp932 => cp932::segment_raw(bytes),
        InputEncoding::Gbk => gbk::segment_raw(bytes),
        InputEncoding::Gb18030 => gb18030::segment_raw(bytes),
        InputEncoding::EucKr => euckr::segment_raw(bytes),
        InputEncoding::Big5 => big5::segment_raw(bytes),
        InputEncoding::Iso2022Jp => iso2022jp::segment_raw(bytes),
        _ => unreachable!(
            "segment_raw called for {encoding:?}, which is self-synchronizing and should \
             never produce a RecordData::Raw record for this function to resolve"
        ),
    }
}

// The two helpers below let every scanner read its
// regions directly by absolute file offset via positioned reads (pread /
// seek_read), rather than holding a `&mut File` and seeking + reading
// sequentially. This matters because chunks may be processed concurrently
// against the same open `File`: a shared `&File` combined with positioned
// reads means no thread ever has to seek the shared file cursor, so scans
// of different chunks can safely run in parallel on separate threads.

/// Platform-specific positioned read: reads into `buf` starting at absolute
/// file `offset`, without touching (or depending on) the file's current
/// cursor position. Returns however many bytes were actually read in this
/// one underlying syscall (may be less than `buf.len()`).
#[cfg(unix)]
fn read_at_once(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at_once(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

/// Like `read_at_once`, but guarantees `buf` is fully filled (looping over
/// short reads, which positioned reads can return even without hitting
/// EOF), or returns an `UnexpectedEof` error if the file ends before `buf`
/// is full. Scanners use this as their basic "read this many bytes from
/// this offset" primitive.
pub(crate) fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    let mut done = 0usize;
    while done < buf.len() {
        let n = read_at_once(file, &mut buf[done..], offset + done as u64)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected EOF while reading chunk",
            ));
        }
        done += n;
    }
    Ok(())
}

/// Keep boundary fragments even when they are shorter than min_cch. The
/// ordered merger needs them to reconstruct strings crossing chunks.
///
/// Every scanner funnels its matches through this single function before
/// writing, so the "too-short-unless-it's-a-boundary-fragment" filtering
/// rule lives in exactly one place instead of being duplicated (and
/// potentially drifting) across every scanner implementation.
///
/// - A record that meets the length threshold (`cch >= min_cch`) is always
///   kept: it's independently useful/reportable on its own.
/// - A record shorter than the threshold is normally dropped as noise, but
///   is still kept if it touches a chunk boundary (`starts_at_chunk` or
///   `ends_at_chunk`), because it may just be one piece of a longer string
///   that got split across two chunks -- discarding it here would silently
///   corrupt reassembly done later by the merger/reporting stage, even
///   though the fragment looks "too short" in isolation.
///
/// Returns whether the record was actually written. Callers **must** use
/// this to drive their record counter rather than incrementing
/// unconditionally: those counts are what the `--stats` output reports,
/// and a caller that counts every *attempted* record instead reports
/// wildly inflated numbers on realistic input (mostly-binary data produces
/// enormous numbers of sub-threshold runs that are dropped here). That bug
/// previously made `scanner::utf16le_ascii` report 20000 records where 1
/// was written, which looked like a detection discrepancy against
/// `scanner::utf16le` -- which counts its surviving records directly and
/// so was correct all along.
#[must_use = "the return value is the record count; ignoring it inflates --stats output"]
pub(crate) fn emit_record(
    writer: &mut BufWriter<File>,
    rec: MatchRecord,
    min_cch: u64,
) -> io::Result<bool> {
    if rec.cch >= min_cch || rec.starts_at_chunk || rec.ends_at_chunk {
        crate::record::write_record(writer, &rec)?;
        return Ok(true);
    }
    Ok(false)
}
