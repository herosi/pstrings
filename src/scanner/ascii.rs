use super::{emit_record, read_exact_at, READ_BUFFER_SIZE};
use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::InputEncoding;
use crate::record::{MatchRecord, RecordData};
use crate::tempfile_helper::create_temp_file;
use std::cmp::min;
use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Scans one chunk for runs of "ASCII-like" bytes (as decided by
/// `FilterSet::allows_u8`), writing each run that survives `emit_record`'s
/// length/boundary filtering to a fresh, offset-sorted temp file.
///
/// Note this takes no `file_len`, unlike every other scanner: it never
/// needs to clamp its chunk against EOF, because a short read simply ends
/// the pass.
///
/// This is a single, linear left-to-right pass over the chunk: bytes are
/// read in `READ_BUFFER_SIZE`-ish blocks, and a "run" (a maximal sequence of
/// consecutive matching bytes) is accumulated in `run_data` until a
/// non-matching byte or the end of the chunk closes it out. Because the
/// scan is strictly sequential, output records are naturally emitted in
/// offset order -- no separate sort step is needed downstream, only a merge
/// against the other encodings' equally-sorted outputs.
pub(crate) fn scan(
    file: &File,
    chunk: &Chunk,
    cfg: &Config,
    temp_path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<(u64, File)> {
    let temp_file = create_temp_file(temp_path, cfg.keep_temp())?;
    let mut out = BufWriter::with_capacity(crate::WRITE_BUFFER_SIZE, temp_file);
    // Read buffer, capped at the chunk length so tiny chunks don't allocate
    // a full READ_BUFFER_SIZE buffer for no reason.
    let mut buf = vec![0u8; min(READ_BUFFER_SIZE, chunk.len.max(1) as usize)];
    // State for the run currently being accumulated (empty when `run_data`
    // is empty -- there is no separate "in a run" flag, emptiness of
    // `run_data` doubles as that flag throughout this function).
    //
    // Accumulated as a `String` of decoded characters rather than as raw
    // bytes later validated by `String::from_utf8`. The bytes this scanner
    // admits are *not* all valid standalone UTF-8: `CharacterFilter::
    // Latin1` accepts 0xA0-0xFF, each of which is a lone continuation or
    // lead byte on its own, so collecting raw bytes and validating at the
    // end would fail (previously: panic) the moment `--filter latin1` was
    // used. Pushing `b as char` maps byte N to U+00N, which is exactly the
    // ISO-8859-1 -> Unicode mapping this scanner's byte-oriented filters
    // imply, and `String::push` encodes it as proper UTF-8. `cb` is
    // unaffected -- it counts source bytes, and each of these characters
    // still came from exactly one.
    let mut run_data = String::with_capacity(64);
    let mut run_offset = 0u64;
    let mut run_cb = 0u64;
    let mut run_cch = 0u64;
    // Whether the current run began exactly at the chunk's start offset,
    // meaning it may be the continuation of a run that started in the
    // previous chunk (a boundary fragment the merger needs to stitch back
    // together), rather than a run that genuinely starts here.
    let mut run_started = false;
    let mut pos = 0u64;
    let mut records = 0u64;

    while pos < chunk.len {
        // Cooperative cancellation: checked once per block read rather than
        // once per byte, since checking an atomic per-byte would be needless
        // overhead on a hot loop; a block is still small enough that
        // cancellation remains responsive.
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let want = min(buf.len() as u64, chunk.len - pos) as usize;
        read_exact_at(file, &mut buf[..want], chunk.offset + pos)?;
        for (i, &b) in buf[..want].iter().enumerate() {
            let abs = chunk.offset + pos + i as u64;
            if cfg.filter().allows_u8(b) {
                // Byte extends (or starts) the current run.
                if run_data.is_empty() {
                    run_offset = abs;
                    run_started = run_offset == chunk.offset;
                }
                run_data.push(b as char);
                run_cb += 1;
                run_cch += 1;
            } else if !run_data.is_empty() {
                // Non-matching byte closes out an in-progress run: emit it
                // (subject to emit_record's length/boundary filter) and
                // reset run state for the next run. `ends_at_chunk` is
                // always `false` here because the run ended mid-chunk on a
                // real non-matching byte, not because the chunk itself ran
                // out -- so this run cannot be a fragment continuing into
                // the next chunk.
                let rec = MatchRecord {
                    offset: run_offset,
                    cb: run_cb,
                    cch: run_cch,
                    encoding: InputEncoding::Ascii,
                    starts_at_chunk: run_started,
                    ends_at_chunk: false,
                    data: RecordData::Text(std::mem::take(&mut run_data)),
                };
                // Counted only if `emit_record` actually wrote it: runs
                // below `min_cch` that touch no chunk boundary are dropped
                // there, and counting them anyway would badly inflate the
                // record totals reported by `--stats` (binary input
                // produces vast numbers of such sub-threshold runs).
                if emit_record(&mut out, rec, cfg.min_cch())? {
                    records += 1;
                }
                run_cb = 0;
                run_cch = 0;
                run_started = false;
                run_data.reserve(64);
            }
        }
        pos += want as u64;
    }

    // If the chunk ended while still inside a run (rather than the run
    // being closed by a non-matching byte above), flush that trailing run
    // now. It's unconditionally marked `ends_at_chunk: true` because the
    // only way to reach this point with non-empty `run_data` is for the
    // scan to have run off the end of the chunk mid-run -- so this fragment
    // may continue into the next chunk and the merger needs to know that.
    // Skipped entirely if cancelled, since a cancelled scan's results are
    // discarded/partial by design and there's no guarantee this "trailing"
    // run is actually adjacent to the chunk boundary at the point of
    // cancellation.
    if !cancelled.load(Ordering::Relaxed) && !run_data.is_empty() {
        let rec = MatchRecord {
            offset: run_offset,
            cb: run_cb,
            cch: run_cch,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: run_started,
            ends_at_chunk: true,
            data: RecordData::Text(run_data),
        };
        if emit_record(&mut out, rec, cfg.min_cch())? {
            records += 1;
        }
    }

    out.flush()?;
    let mut temp_file = out.into_inner().map_err(|e| e.into_error())?;
    // Rewind so the caller (the merger) can read this scanner's output from
    // the beginning without having to know it was just written.
    temp_file.seek(io::SeekFrom::Start(0))?;
    Ok((records, temp_file))
}
