//! Consumes one chunk's already-merged (across encodings) record stream at a
//! time, in chunk order, and writes the final tab-separated output. Also
//! resolves strings that cross a chunk boundary by holding the last
//! unterminated record per encoding (`pending`) until the next chunk's
//! stream supplies its continuation.
//!
//! Like `merger`, this module only deals in `InputEncoding` as an opaque,
//! `Hash`-able key (via `HashMap<InputEncoding, MatchRecord>`) and the
//! intermediate record format. It requires no changes when a new
//! self-synchronizing scanner is added. Non-self-synchronizing scanners
//! (every encoding for which `InputEncoding::is_self_synchronizing`
//! returns `false`) also need no *bespoke* handling here -- their boundary
//! fragments arrive as `RecordData::Raw` and are resolved generically via
//! `scanner::segment_raw`, dispatched by `InputEncoding` exactly the way
//! `scanner::scan` itself is -- but see `resolve_for_output`'s doc comment
//! for the one behavioral difference this introduces: a boundary fragment
//! can now resolve into *several* final records, or leave behind a new,
//! still-unresolved `pending` entry that keeps chaining forward across
//! more than one further chunk.

use crate::READ_BUFFER_SIZE;
use crate::encoding::InputEncoding;
use crate::record::{append_data, read_record, read_record_borrowed, MatchRecord, RecordData, RecordDataRef};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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

/// Processes one chunk's merged record stream and writes its records to the final output.
///
/// Each chunk has already been k-way merged across its encodings. Since record
/// offsets are derived from the absolute input position, every record in chunk N
/// precedes every record in chunk N+1. Therefore the output stage only needs to
/// process chunks in order and resolve strings that cross chunk boundaries.
///
/// `pending` contains records that ended at the previous chunk boundary. Such records
/// cannot be written immediately because the next chunk may contain their continuation.
/// This function therefore first tries to resolve those pending records, then processes
/// the rest of the current chunk normally.
///
/// `chunk_len` is the length in bytes of this chunk, so that
/// `[chunk_offset, chunk_offset + chunk_len)` describes exactly the range
/// the chunk covers. It is needed only to recognise the degenerate case
/// where a previous chunk's boundary peek already consumed this entire
/// chunk (possible when `chunk_size` is smaller than the longest
/// character a scanner may read across), which makes an empty record
/// stream expected rather than a signal that pending fragments are
/// orphaned.
pub fn output_merged_chunk(
    file: File,
    chunk_offset: u64,
    chunk_len: u64,
    pending: &mut HashMap<InputEncoding, MatchRecord>,
    min_cch: u64,
    out: &mut impl Write,
    str_only: bool,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    // The intermediate file is read sequentially, so buffering reduces the number of
    // relatively expensive file-system reads.
    let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, file);

    // Reusable buffers avoid allocating a new Vec for every intermediate record and
    // every output line. They are deliberately small because records are processed one
    // at a time rather than accumulated in memory.
    let mut read_scratch = Vec::with_capacity(256);
    let mut write_scratch = Vec::with_capacity(256);

    if !pending.is_empty() {
        // Collect the records at the beginning of this chunk. These records are the
        // only candidates that can continue one of the records stored in `pending`.
        let mut boundary = Vec::<MatchRecord>::new();

        // Distinguishes "the loop below already fully resolved `pending`
        // (via the offset-mismatch branch)" from "the stream ran out while
        // still collecting records at `chunk_offset`". Only the latter case
        // needs the fallback below; conflating them previously caused a
        // freshly-set `pending` entry (set by process_chunk_record on the
        // offset-mismatch branch) to be immediately terminated and written
        // out early, splitting strings that should have joined with the
        // next chunk.
        let mut exhausted = false;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled by Ctrl+C"));
            }
            let Some(rec) = read_record(&mut reader)? else {
                exhausted = true;
                break;
            };

            // The first record that is not a boundary candidate marks the
            // end of the boundary-only portion. Process the boundary
            // records first, then feed this record into the normal
            // chunk-processing path.
            //
            // A record qualifies as a boundary candidate if the *scanner*
            // flagged it `starts_at_chunk`. That flag -- not an offset
            // comparison -- is the authoritative signal, because for
            // variable-width encodings the first record of a chunk does
            // not necessarily begin exactly at `chunk_offset`:
            // `scanner::utf8` peeks past the boundary to finish a
            // straddling character, so the *next* chunk legitimately
            // starts 1-3 bytes in, with those bytes already consumed by
            // the previous chunk's final record. `scanner::utf16le` does
            // the same for surrogate pairs and for odd-parity alignment.
            // Requiring `rec.offset == chunk_offset` here silently
            // excluded exactly those records from boundary processing, so
            // they were emitted as separate fragments instead of being
            // joined -- splitting one string into several and losing the
            // characters that spanned the boundary.
            //
            // `chunk_offset` is still used as a lower bound: a record
            // starting before it cannot belong to this chunk at all.
            if !rec.starts_at_chunk || rec.offset < chunk_offset {
                let boundary = std::mem::take(&mut boundary);
                process_boundary_records(boundary, pending, min_cch, out, str_only)?;
                process_chunk_record(rec, pending, min_cch, out, str_only, &mut write_scratch)?;
                break;
            }
            boundary.push(rec);
        }
        if exhausted {
            if !boundary.is_empty() {
                process_boundary_records(boundary, pending, min_cch, out, str_only)?;
            } else if !pending.is_empty() && chunk_len > 0 && chunk_fully_consumed(pending, chunk_offset, chunk_len) {
                // Degenerate case: this whole chunk was already consumed
                // by the previous chunk's final record, which peeked past
                // its own boundary to finish a straddling character. The
                // scanner for this chunk therefore had nothing left to
                // look at and produced no records at all -- but that is
                // *not* evidence that the pending fragments are orphaned.
                // They are still live and must be carried forward
                // untouched, to be joined by whichever later chunk
                // actually contains the continuation.
                //
                // This only arises when `chunk_size` is smaller than the
                // longest character a scanner may peek across (up to 3
                // extra bytes for UTF-8, 2 for a UTF-16LE surrogate
                // pair), so in practice only at very small chunk sizes --
                // but at those sizes it previously truncated strings
                // outright.
            } else if !pending.is_empty() {
                // The stream had zero records at all; nothing can join the
                // pending fragments (if the chunk's own first byte had
                // been loosely shaped like a continuation, the scanner
                // would have
                // produced at least a leading Raw record for it -- zero
                // records means it wasn't), so they are genuinely
                // orphaned.
                flush_pending(pending, min_cch, out, str_only)?;
            }
        }
    }

    let mut check_counter: u32 = 0;

    // From this point on, records belong to the normal body of the chunk. Borrowed
    // records are used here so that ordinary records do not need to be cloned.
    while let Some(rec) = read_record_borrowed(&mut reader, &mut read_scratch)? {
        check_counter += 1;
        if check_counter % 4096 == 0 && cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled by Ctrl+C"));
        }

        if rec.ends_at_chunk {
            // A record ending at the chunk boundary must survive until the next chunk
            // is processed. Convert the borrowed record into an owned MatchRecord
            // before storing it in `pending`.
            pending.insert(rec.encoding, owned_from_raw_record(&rec));
        } else {
            match rec.data {
                // Common, fast path: Text data can be written straight from the
                // borrowed &str with no extra allocation, exactly as before.
                RecordDataRef::Text(s) => {
                    if rec.cch >= min_cch {
                        write_output_record_str(out, rec.offset, rec.encoding, s, str_only, &mut write_scratch)?;
                    }
                }
                // Rare path: a non-`ends_at_chunk` Raw record reaching this hot
                // loop. This only happens when `pending` was empty at this
                // chunk's start (so the boundary-collection phase above was
                // skipped entirely) and the scanner's leading deferred prefix
                // resolved entirely within this chunk. `rec.cch` is a
                // placeholder here (see `RecordData`'s doc comment), so it
                // must go through the same resolve-and-filter path as
                // everything else rather than being compared to `min_cch`
                // directly.
                RecordDataRef::Raw(bytes) => {
                    let owned = MatchRecord {
                        offset: rec.offset,
                        cb: rec.cb,
                        cch: rec.cch,
                        encoding: rec.encoding,
                        starts_at_chunk: rec.starts_at_chunk,
                        ends_at_chunk: false,
                        data: RecordData::Raw(bytes.to_owned()),
                    };
                    let (written, leftover) = resolve_for_output(owned, min_cch);
                    for r in &written {
                        write_output_record(out, r, str_only, &mut write_scratch)?;
                    }
                    if let Some(l) = leftover {
                        pending.insert(l.encoding, l);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Returns whether every pending fragment already extends past the end of
/// the chunk `[chunk_offset, chunk_offset + chunk_len)`.
///
/// # Why this matters
///
/// Variable-width scanners read a little past their chunk's end to finish
/// a character that straddles the boundary: `scanner::utf8` peeks up to
/// three extra bytes, `scanner::utf16le` up to two for a surrogate pair.
/// Normally that just shifts the next chunk's first record a few bytes in.
/// But when `chunk_size` is smaller than that peek distance, the peek can
/// swallow one or more *entire* subsequent chunks. Those chunks' scanners
/// then have nothing left to examine and legitimately emit zero records.
///
/// An empty record stream is otherwise strong evidence that nothing in
/// this chunk could continue a pending fragment, so `output_merged_chunk`
/// flushes `pending` on seeing one. In this degenerate case that
/// conclusion is wrong: the fragment is still live, its continuation
/// simply lies further ahead. Flushing it there truncated strings outright
/// at small chunk sizes.
///
/// The test is that each pending record already covers the whole chunk --
/// `prev.offset + prev.cb >= chunk_offset + chunk_len` -- which is exactly
/// the "the previous chunk already read past here" condition. If any
/// pending record stops short of the chunk's end, then that chunk really
/// did contain unexamined bytes for it, the emptiness is meaningful, and
/// the normal flush applies.
fn chunk_fully_consumed(
    pending: &HashMap<InputEncoding, MatchRecord>,
    chunk_offset: u64,
    chunk_len: u64,
) -> bool {
    let chunk_end = chunk_offset.saturating_add(chunk_len);
    pending
        .values()
        .all(|prev| prev.offset.saturating_add(prev.cb) >= chunk_end)
}

/// Resolves and writes out every remaining entry in `pending`, applying
/// `min_cch`. Used both when a chunk's own record stream turns out to be
/// completely empty (nothing left within that chunk that could possibly
/// continue any pending fragment -- if the chunk's first byte had been
/// loosely shaped like a continuation of the pending encoding, the
/// scanner would have produced at least a
/// leading `Raw` record for it, so zero records means it wasn't) and, via
/// the public `flush_pending` below, once the entire file has been
/// processed and no further chunk will ever arrive. In both cases,
/// whatever's left in `pending` is genuinely as complete as it will ever
/// get; any dangling tail `resolve_for_output` might still report on top
/// of that is simply truncated/orphaned data and is dropped, exactly like
/// any other truncated-at-EOF sequence.
fn drain_pending(
    pending: &mut HashMap<InputEncoding, MatchRecord>,
    min_cch: u64,
    out: &mut impl Write,
    str_only: bool,
) -> io::Result<()> {
    let terminated = std::mem::take(pending);
    let mut records: Vec<MatchRecord> = Vec::new();
    for (_, rec) in terminated {
        let (written, leftover) = resolve_for_output(rec, min_cch);
        records.extend(written);
        let _ = leftover; // nothing left to wait for -- truncated, dropped
    }
    records.sort_by_key(|rec| (rec.offset, rec.encoding as u16));
    let mut scratch: Vec<u8> = Vec::with_capacity(256);
    for rec in &records {
        write_output_record(out, rec, str_only, &mut scratch)?;
    }
    Ok(())
}

/// Flushes whatever's left in `pending` once the entire file has been
/// processed (no more chunks will ever call `output_merged_chunk` again).
/// Callers -- both the crate's top-level pipeline and tests -- must call
/// this exactly once, after the last chunk; any fragment still waiting at
/// that point is otherwise silently lost, since nothing else will ever
/// prompt it to be written.
pub fn flush_pending(
    pending: &mut HashMap<InputEncoding, MatchRecord>,
    min_cch: u64,
    out: &mut impl Write,
    str_only: bool,
) -> io::Result<()> {
    drain_pending(pending, min_cch, out, str_only)
}

/// Converts a borrowed `RawRecord` into an owned `MatchRecord`, for
/// storage in `pending`. Used on both the boundary-collection and
/// hot-loop paths.
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

/// Resolves one record for final output, applying `min_cch`.
///
/// - For `RecordData::Text` (self-synchronizing encodings, or a deferred
///   fragment that has already been through this function once): the
///   record is already fully decoded, so this is just the original
///   `min_cch` check, and the second return value is always `None`.
/// - For `RecordData::Raw` (a non-self-synchronizing encoding's
///   boundary-touching fragment): the accumulated raw bytes are decoded
///   and split into characters here, via `scanner::segment_raw`, for the
///   first time. That call can find more than one printable run inside
///   the fragment (an invalid or filtered byte partway through the
///   now-resolved bytes still breaks a run, exactly as it would have at
///   scan time for ordinary, non-boundary content) -- so this returns a
///   `Vec`, not a single record. It can *also* find that the fragment
///   still ends mid-character (its very last bytes are a lead byte with
///   no trailing byte yet available) -- in which case that dangling tail
///   is returned as the second value, a fresh, still-unresolved `pending`
///   entry for this encoding. This lets a run of alternating single/
///   double-byte characters correctly keep deferring across more than one
///   chunk boundary in a row, not just one.
///
/// IMPORTANT precondition for `Raw` records: callers must only call this
/// once they're sure no further chunk could still extend `rec` -- i.e.
/// `rec.ends_at_chunk` is already `false`, or there is genuinely no next
/// chunk left to check (end of the whole file). Calling this while
/// `ends_at_chunk` is still `true` would resolve prematurely: `segment_raw`
/// has no way to know a completely different, still-unread chunk might
/// continue the very same run with no invalid byte in between, and would
/// happily report whatever's on hand as "finished" the moment it happens
/// to end on a clean character boundary -- silently truncating a longer
/// match. See the call sites in `process_boundary_records` for the
/// `ends_at_chunk` check this relies on.
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

/// Resolves `rec` and immediately writes whatever comes out of it,
/// returning any leftover (still-unresolved) fragment so the caller can
/// decide what to do with it (chain it forward as new `pending`, or -- if
/// there's genuinely nothing left to wait for -- drop it as truncated).
fn resolve_and_write(
    rec: MatchRecord,
    min_cch: u64,
    out: &mut impl Write,
    str_only: bool,
    scratch: &mut Vec<u8>,
) -> io::Result<Option<MatchRecord>> {
    let (written, leftover) = resolve_for_output(rec, min_cch);
    for r in &written {
        write_output_record(out, r, str_only, scratch)?;
    }
    Ok(leftover)
}

/// Resolves records from the beginning of the current chunk against records carried
/// over from the previous chunk.
///
/// A continuation is considered valid only when it has the same encoding and starts
/// exactly where the previous fragment ended (`prev.offset + prev.cb == rec.offset`).
/// This prevents unrelated records from being joined merely because their encodings
/// happen to match. This offset-continuity check is the same for both `RecordData`
/// variants: `cb` (a byte count) is always meaningful, even for a `Raw` record whose
/// `cch` is still a placeholder.
fn process_boundary_records(
    boundary: Vec<MatchRecord>,
    pending: &mut HashMap<InputEncoding, MatchRecord>,
    min_cch: u64,
    out: &mut impl Write,
    str_only: bool,
) -> io::Result<()> {
    // Temporarily take ownership of the old map. This lets us freely remove entries
    // while building the new `pending` state without borrowing the same map twice.
    let mut old_pending = std::mem::take(pending);

    // Records that successfully continue across the current boundary -- or that
    // resolved but are *still* left with a dangling, unresolved tail (see
    // `resolve_for_output`'s doc comment) -- are kept here so they can become the
    // `pending` state for the next chunk.
    let mut continued = HashMap::<InputEncoding, MatchRecord>::new();

    // Boundary records that did not match an old pending record are processed as new
    // records after all old pending records have had a chance to find their continuation.
    let mut remaining = Vec::with_capacity(boundary.len());

    // Completed records are collected before writing because joining fragments can
    // change their effective starting offset. Sorting afterward restores global offset
    // order within the records emitted by this boundary-processing step.
    let mut to_write: Vec<MatchRecord> = Vec::new();

    for rec in boundary {
        if let Some(prev) = old_pending.remove(&rec.encoding) {
            // The offset check is the actual continuity test. Matching the encoding
            // alone is insufficient because multiple independent strings can use the
            // same encoding.
            if prev.offset + prev.cb == rec.offset {
                let mut merged = prev;
                append_data(&mut merged, rec);
                // Match on a reference here (not `merged.data` by value):
                // the arms below need to move `merged` as a whole
                // (`continued.insert(merged.encoding, merged)`,
                // `to_write.push(merged)`, `resolve_for_output(merged, ..)`),
                // which a prior partial move of just `.data` would block.
                // The `_` inside each pattern means no borrow of the
                // payload survives into the arm bodies, so `merged` is
                // fully available there.
                match &merged.data {
                    RecordData::Text(_) if merged.ends_at_chunk => {
                        continued.insert(merged.encoding, merged);
                    }
                    RecordData::Text(_) => {
                        if merged.cch >= min_cch {
                            to_write.push(merged);
                        }
                    }
                    RecordData::Raw(_) if merged.ends_at_chunk => {
                        // The just-joined-in record ALSO touched the end
                        // of *its* chunk, meaning a chain of raw
                        // fragments can span more than two chunks (e.g.
                        // alternating single/double-byte characters can
                        // keep deferring resolution chunk after chunk).
                        // Resolving now would be premature: whatever
                        // comes immediately after in the *next* chunk
                        // might continue this exact run with no
                        // intervening invalid byte, and calling
                        // `resolve_for_output` here can't know that --
                        // it would happily report the bytes gathered so
                        // far as "complete" the moment they happen to end
                        // on a clean character boundary, silently
                        // truncating a longer match. So: keep chaining,
                        // exactly as an unmerged `ends_at_chunk` record
                        // would, until a merge finally produces a result
                        // that *doesn't* touch its chunk's end.
                        continued.insert(merged.encoding, merged);
                    }
                    RecordData::Raw(_) => {
                        // The joined-in record did NOT touch its own
                        // chunk's end, so nothing further needs waiting
                        // for -- resolve now via `segment_raw`, which may
                        // still find its own dangling tail from an
                        // internal (not chunk-boundary-caused) reason,
                        // hence the `leftover` handling below.
                        let (written, leftover) = resolve_for_output(merged, min_cch);
                        to_write.extend(written);
                        if let Some(l) = leftover {
                            continued.insert(l.encoding, l);
                        }
                    }
                }
                continue;
            }
            // No continuity -- the old pending is orphaned; resolve it
            // independently of `rec`.
            let (written, leftover) = resolve_for_output(prev, min_cch);
            to_write.extend(written);
            if let Some(l) = leftover {
                // Defensive: an orphaned (never-joined) fragment resolving
                // into yet another leftover shouldn't happen in practice
                // (there was nothing waiting to extend it), but chaining it
                // forward is still correct and strictly safer than
                // silently dropping data.
                continued.insert(l.encoding, l);
            }
        }
        remaining.push(rec);
    }
    for (_, prev) in old_pending {
        let (written, leftover) = resolve_for_output(prev, min_cch);
        to_write.extend(written);
        if let Some(l) = leftover {
            continued.insert(l.encoding, l);
        }
    }

    // Join results reset each record's offset to wherever the run started,
    // which may no longer match the order records were read in this chunk
    // (e.g. one encoding's continuation started earlier in a previous chunk
    // than another's). Re-sort before writing so output stays offset-ordered.
    to_write.sort_by_key(|rec| (rec.offset, rec.encoding as u16));
    let mut scratch: Vec<u8> = Vec::with_capacity(256);
    for rec in &to_write {
        write_output_record(out, rec, str_only, &mut scratch)?;
    }

    // Preserve fragments that still need to wait for the next chunk.
    pending.extend(continued);
    for rec in remaining {
        process_chunk_record(rec, pending, min_cch, out, str_only, &mut scratch)?;
    }
    Ok(())
}

/// Processes a single record that does not belong to the special boundary-resolution
/// pass. Records ending at the chunk boundary are deferred; all other records are
/// resolved (decoding/splitting a `Raw` payload if needed) and written immediately,
/// subject to `min_cch`.
fn process_chunk_record(
    rec: MatchRecord,
    pending: &mut HashMap<InputEncoding, MatchRecord>,
    min_cch: u64,
    out: &mut impl Write,
    str_only: bool,
    scratch: &mut Vec<u8>,
) -> io::Result<()> {
    if rec.ends_at_chunk {
        pending.insert(rec.encoding, rec);
    } else if let Some(leftover) = resolve_and_write(rec, min_cch, out, str_only, scratch)? {
        // A non-"ends_at_chunk" record resolving into a leftover happens
        // for a leading deferred-prefix record that the scanner
        // determined resolves *within* this chunk (not touching
        // chunk_end) -- if `segment_raw` still finds a dangling tail at
        // the very end of that small buffer, chain it forward rather than
        // dropping it.
        pending.insert(leftover.encoding, leftover);
    }
    Ok(())
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