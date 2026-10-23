//! Reassembles chunk fragments per encoding, then emits complete records in
//! global offset order. A completed record waits while an earlier string is
//! still open in another encoding.

use crate::READ_BUFFER_SIZE;
use crate::encoding::InputEncoding;
use crate::record::{MatchRecord, RecordData, RecordDataRef, append_data, read_record_borrowed};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, BufReader, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

// The output line ending is platform-native, so the same input yields
// byte-different output on Windows and Unix. Tests must compare against
// `LINE_ENDING` (or split on lines) rather than hard-coding "\r\n".
#[cfg(windows)]
const LINE_ENDING: &str = "\r\n";
#[cfg(not(windows))]
const LINE_ENDING: &str = "\n";

/// Per-encoding tally of records actually written to the final output.
///
/// # Why this is counted here rather than in the scanners
///
/// The scanners' own record counts (`DetailedStats::records_by_encoding`)
/// measure scanner work, and legitimately depend on `--chunk-size`: a
/// string crossing N chunk boundaries is emitted as N fragments, and a
/// fragment touching a boundary is emitted even when it is shorter than
/// `--min-length`, since the next chunk might extend it. Both effects
/// inflate the count as chunks shrink, without changing the output at all
/// -- on a 2 KiB test file, UTF-8 reported 744 records at `--chunk-size 2`
/// versus 45 at one chunk, while the output was 44 lines either way.
///
/// The number a user actually wants ("how many strings were found") is
/// therefore only knowable at the point of writing, after fragments have
/// been rejoined and `--min-length` has been applied to the joined
/// result. Counting inside the two write functions means every path --
/// the hot borrowed-record loop, boundary processing, and the final
/// `flush_pending` -- is covered by construction, with no call site able
/// to forget to tally.
///
/// A plain `Mutex<HashMap>` is used rather than atomics per encoding
/// because the entire output stage is single-threaded (one `Receiver`
/// draining chunks in order), so this is uncontended; the cost is a few
/// nanoseconds per output line, against writing that line to disk.
static EMITTED: Mutex<Option<HashMap<InputEncoding, u64>>> = Mutex::new(None);

/// Starts tallying output records. Called once, before the output stage,
/// only when the counts will actually be reported -- when they won't,
/// `EMITTED` stays `None` and the counting in the write functions is a
/// single already-`None` check.
pub fn begin_emitted_counts() {
    *EMITTED.lock().unwrap() = Some(HashMap::new());
}

/// Returns the tally collected since `begin_emitted_counts`, if any.
pub fn take_emitted_counts() -> Option<HashMap<InputEncoding, u64>> {
    EMITTED.lock().unwrap().take()
}

#[inline]
fn note_emitted(encoding: InputEncoding) {
    if let Some(map) = EMITTED.lock().unwrap().as_mut() {
        *map.entry(encoding).or_default() += 1;
    }
}

/// State retained between chunks. In addition to unfinished strings, this
/// retains complete records that must wait for an earlier string to close.
#[derive(Default)]
pub struct OutputState {
    pending: HashMap<InputEncoding, MatchRecord>,
    ready: BTreeMap<(u64, u16, u64), MatchRecord>,
    sequence: u64,
}

impl OutputState {
    fn complete(&mut self, rec: MatchRecord, min_cch: u64) {
        let (records, leftover) = resolve_for_output(rec, min_cch);
        for record in records {
            let key = (record.offset, record.encoding as u16, self.sequence);
            self.sequence += 1;
            self.ready.insert(key, record);
        }
        if let Some(record) = leftover {
            self.pending.insert(record.encoding, record);
        }
    }

    fn emit_before(
        &mut self,
        bound: (u64, u16),
        out: &mut impl Write,
        str_only: bool,
    ) -> io::Result<()> {
        let mut scratch = Vec::new();
        while self
            .ready
            .first_key_value()
            .is_some_and(|(key, _)| (key.0, key.1) < bound)
        {
            let (_, record) = self.ready.pop_first().unwrap();
            write_output_record(out, &record, str_only, &mut scratch)?;
        }
        Ok(())
    }

    fn lower_bound(&self) -> (u64, u16) {
        self.pending
            .values()
            .map(|r| (r.offset, r.encoding as u16))
            .min()
            .unwrap_or((u64::MAX, u16::MAX))
    }
}

/// Consumes one offset-sorted chunk. Boundary continuation and empty chunks
/// are evaluated independently for each encoding, including encodings whose
/// decoder already consumed this chunk through a boundary peek.
pub fn output_merged_chunk(
    file: File,
    chunk_offset: u64,
    chunk_len: u64,
    state: &mut OutputState,
    min_cch: u64,
    out: &mut impl Write,
    str_only: bool,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "cancelled by Ctrl+C",
        ));
    }
    let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, file);
    let mut previous = std::mem::take(&mut state.pending);
    let mut count = 0usize;
    let mut read_scratch = Vec::new();
    let mut write_scratch = Vec::new();
    while let Some(raw) = read_record_borrowed(&mut reader, &mut read_scratch)? {
        count += 1;
        if count % 4096 == 0 && cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "cancelled by Ctrl+C",
            ));
        }
        let input_bound = (raw.offset, raw.encoding as u16);
        let earlier = previous
            .values()
            .map(|r| (r.offset, r.encoding as u16))
            .min()
            .unwrap_or((u64::MAX, u16::MAX))
            .min(state.lower_bound());
        if !raw.ends_at_chunk && !previous.contains_key(&raw.encoding) && input_bound < earlier {
            if let RecordDataRef::Text(text) = raw.data {
                state.emit_before(input_bound, out, str_only)?;
                if state
                    .ready
                    .first_key_value()
                    .is_none_or(|(key, _)| (key.0, key.1) >= input_bound)
                {
                    if raw.cch >= min_cch {
                        write_output_record_str(
                            out,
                            raw.offset,
                            raw.encoding,
                            text,
                            str_only,
                            &mut write_scratch,
                        )?;
                    }
                    continue;
                }
            }
        }
        let mut rec = owned_from_raw_record(&raw);
        if let Some(prev) = previous.remove(&rec.encoding) {
            if rec.starts_at_chunk && prev.offset + prev.cb == rec.offset {
                let mut merged = prev;
                append_data(&mut merged, rec);
                rec = merged;
            } else {
                state.complete(prev, min_cch);
            }
        }
        if rec.ends_at_chunk {
            state.pending.insert(rec.encoding, rec);
        } else {
            state.complete(rec, min_cch);
        }
        let previous_bound = previous
            .values()
            .map(|r| (r.offset, r.encoding as u16))
            .min()
            .unwrap_or((u64::MAX, u16::MAX));
        state.emit_before(
            input_bound.min(previous_bound).min(state.lower_bound()),
            out,
            str_only,
        )?;
    }
    let chunk_end = chunk_offset.saturating_add(chunk_len);
    for (encoding, prev) in previous {
        if chunk_len > 0 && prev.offset.saturating_add(prev.cb) >= chunk_end {
            state.pending.insert(encoding, prev);
        } else {
            state.complete(prev, min_cch);
        }
    }
    state.emit_before(state.lower_bound().min((chunk_end, 0)), out, str_only)
}

/// Finishes the final pending strings and drains all complete records in order.
pub fn flush_pending(
    state: &mut OutputState,
    min_cch: u64,
    out: &mut impl Write,
    str_only: bool,
) -> io::Result<()> {
    for (_, rec) in std::mem::take(&mut state.pending) {
        let (records, _) = resolve_for_output(rec, min_cch);
        for record in records {
            let key = (record.offset, record.encoding as u16, state.sequence);
            state.sequence += 1;
            state.ready.insert(key, record);
        }
    }
    state.emit_before((u64::MAX, u16::MAX), out, str_only)
}

fn owned_from_raw_record(rec: &crate::record::RawRecord<'_>) -> MatchRecord {
    MatchRecord {
        offset: rec.offset,
        cb: rec.cb,
        cch: rec.cch,
        encoding: rec.encoding,
        starts_at_chunk: rec.starts_at_chunk,
        ends_at_chunk: rec.ends_at_chunk,
        data: match rec.data {
            RecordDataRef::Text(s) => RecordData::Text(s.to_owned()),
            RecordDataRef::Raw(b) => RecordData::Raw(b.to_owned()),
        },
    }
}

fn resolve_for_output(rec: MatchRecord, min_cch: u64) -> (Vec<MatchRecord>, Option<MatchRecord>) {
    // Destructure up front (a full move of `rec`, which is fine since we
    // never need `rec` as a whole again) rather than matching on
    // `rec.data` directly -- matching on just that field would partially
    // move it out of `rec`, making the other fields unusable for
    // reconstructing a record afterward.
    let MatchRecord {
        offset,
        cb,
        cch,
        encoding,
        starts_at_chunk,
        ends_at_chunk,
        data,
    } = rec;

    match data {
        RecordData::Text(s) => {
            let keep = cch >= min_cch;
            let written = if keep {
                vec![MatchRecord {
                    offset,
                    cb,
                    cch,
                    encoding,
                    starts_at_chunk,
                    ends_at_chunk,
                    data: RecordData::Text(s),
                }]
            } else {
                vec![]
            };
            (written, None)
        }
        RecordData::Raw(bytes) => {
            let (fragments, tail) = crate::scanner::segment_raw(encoding, &bytes);
            let consumed = bytes.len() - tail.len();
            let written: Vec<MatchRecord> = fragments
                .into_iter()
                .filter(|f| f.cch >= min_cch)
                .map(|f| MatchRecord {
                    offset: offset + f.start,
                    cb: f.cb,
                    cch: f.cch,
                    encoding,
                    // Both boundary flags are now moot: this record has
                    // already been fully resolved (and, if it needed to
                    // keep waiting, that need is represented by the
                    // separate `leftover` return instead).
                    starts_at_chunk: false,
                    ends_at_chunk: false,
                    data: RecordData::Text(f.data),
                })
                .collect();
            let leftover = if tail.is_empty() {
                None
            } else {
                Some(MatchRecord {
                    offset: offset + consumed as u64,
                    cb: tail.len() as u64,
                    cch: 0,
                    encoding,
                    starts_at_chunk,
                    ends_at_chunk: true,
                    data: RecordData::Raw(tail),
                })
            };
            (written, leftover)
        }
    }
}

/// Serializes one owned `MatchRecord` into the final output format.
///
/// In normal mode the format is `<20-digit offset>\\t<encoding>\\t<data><line ending>`.
/// With `str_only`, only the string data and line ending are emitted.
///
/// `rec.data` must be `RecordData::Text` here -- by the time a record
/// reaches this function it has always already been resolved (via
/// `resolve_for_output`, for anything that started out as `Raw`).
pub fn write_output_record(
    out: &mut impl Write,
    rec: &MatchRecord,
    str_only: bool,
    scratch: &mut Vec<u8>,
) -> io::Result<()> {
    let text = match &rec.data {
        RecordData::Text(s) => s.as_str(),
        RecordData::Raw(_) => {
            debug_assert!(
                false,
                "write_output_record called with an unresolved Raw record for encoding {:?}",
                rec.encoding
            );
            return Ok(());
        }
    };

    // Reuse the caller-provided buffer instead of allocating a temporary String for
    // every output record.
    scratch.clear();
    if str_only {
        scratch.extend_from_slice(text.as_bytes());
        scratch.extend_from_slice(LINE_ENDING.as_bytes());
    } else {
        // `itoa` handles integer formatting without an intermediate heap allocation.
        // The output requires a fixed-width, 20-digit decimal offset, so the missing
        // leading digits are inserted manually.
        let mut num_buf = itoa::Buffer::new();
        let num_str = num_buf.format(rec.offset);
        for _ in 0..(20usize.saturating_sub(num_str.len())) {
            scratch.push(b'0');
        }
        scratch.extend_from_slice(num_str.as_bytes());
        scratch.push(b'\t');
        scratch.extend_from_slice(rec.encoding.name().as_bytes());
        scratch.push(b'\t');
        scratch.extend_from_slice(text.as_bytes());
        scratch.extend_from_slice(LINE_ENDING.as_bytes());
    }
    note_emitted(rec.encoding);
    out.write_all(scratch)
}

/// Equivalent to `write_output_record` for a borrowed string slice. This avoids creating
/// a temporary `MatchRecord` when the record came directly from the intermediate stream.
fn write_output_record_str(
    out: &mut impl Write,
    offset: u64,
    encoding: InputEncoding,
    data: &str,
    str_only: bool,
    scratch: &mut Vec<u8>,
) -> io::Result<()> {
    scratch.clear();
    if str_only {
        scratch.extend_from_slice(data.as_bytes());
        scratch.extend_from_slice(LINE_ENDING.as_bytes());
    } else {
        let mut num_buf = itoa::Buffer::new();
        let num_str = num_buf.format(offset);
        for _ in 0..(20usize.saturating_sub(num_str.len())) {
            scratch.push(b'0');
        }
        scratch.extend_from_slice(num_str.as_bytes());
        scratch.push(b'\t');
        scratch.extend_from_slice(encoding.name().as_bytes());
        scratch.push(b'\t');
        scratch.extend_from_slice(data.as_bytes());
        scratch.extend_from_slice(LINE_ENDING.as_bytes());
    }
    note_emitted(encoding);
    out.write_all(scratch)
}
