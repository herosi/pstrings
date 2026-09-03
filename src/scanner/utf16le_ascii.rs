use super::{emit_record, read_exact_at, READ_BUFFER_SIZE};
use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::InputEncoding;
use crate::merger::merge_sorted_record_files;
use crate::record::{MatchRecord, RecordData};
use crate::tempfile_helper::create_temp_file;
use std::cmp::min;
use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};


/// Scans one chunk for UTF-16LE strings by scanning the two possible byte
/// parities (code units starting at an even byte offset vs. an odd byte
/// offset) as independent passes, then merging their outputs by offset into
/// a single sorted result. See `scan_parity` for why parity is scanned
/// separately rather than trying to auto-detect alignment.
pub(crate) fn scan(
    file: &File,
    file_len: u64,
    chunk: &Chunk,
    cfg: &Config,
    temp_path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<(u64, File)> {
    // Distinct temp file names per parity stream so the two scans (and the
    // subsequent merge) never collide on disk, even if `keep_temp` is set
    // and the files are left behind for inspection.
    let parity0 = temp_path.with_extension("utf16le-ascii-p0");
    let parity1 = temp_path.with_extension("utf16le-ascii-p1");

    let (records0, file0) = scan_parity(file, file_len, chunk, cfg, 0, &parity0, cancelled)?;
    let (records1, file1) = scan_parity(file, file_len, chunk, cfg, 1, &parity1, cancelled)?;
    // Both parity streams are individually offset-sorted (each is a single
    // sequential left-to-right scan, same as ascii::scan), so they can be
    // combined with a straight k-way merge rather than a full re-sort.
    let merged_file = merge_sorted_record_files(vec![file0, file1], temp_path, cfg)?;

    Ok((records0 + records1, merged_file))
}

/// UTF-16LE code units can start at either byte parity within a chunk. Each
/// parity is scanned as an independent stream (its own runs, its own
/// boundary bookkeeping) and the two resulting streams are merged by offset
/// afterwards, since interleaving them directly while scanning would be far
/// more complex for no benefit.
///
/// Note: this is meant for UTF-16LE characters in the Latin-1 range,
/// i.e. code units of the form 0x00XX, which the `u as u8` truncation
/// below reduces to a single byte. That truncation is only sound for the
/// filters this scanner is intended to be used with (`ascii`, `latin1`);
/// a wider selection such as `--filter kanji` will admit code units above
/// 0xFF and silently truncate them to garbage. Use `scanner::utf16le` for
/// anything beyond Latin-1 -- full UTF-16LE, including surrogate pairs,
/// is out of scope here.
fn scan_parity(
    file: &File,
    file_len: u64,
    chunk: &Chunk,
    cfg: &Config,
    parity: u64,
    temp_path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<(u64, File)> {
    // Clamp to file_len in case this is the final chunk and its nominal
    // length would otherwise run past EOF.
    let chunk_end = min(chunk.offset + chunk.len, file_len);
    let temp_file = create_temp_file(temp_path, cfg.keep_temp())?;
    let mut out = BufWriter::with_capacity(crate::WRITE_BUFFER_SIZE, temp_file);
    let mut records = 0u64;
    // First absolute byte offset >= chunk.offset whose parity (offset % 2)
    // matches the requested `parity`. Derived arithmetically (rather than
    // with a branch) as: if chunk.offset already has the right parity, stay
    // put; otherwise step forward one byte.
    let first = chunk.offset + ((parity + 2 - chunk.offset % 2) % 2);

    // Shared "flush the writer, rewind, return" tail used by every early
    // return below as well as the normal end of the function, so every
    // return path produces a `File` positioned at the start, ready for the
    // merge step in `scan`.
    let finish = |mut out: BufWriter<File>, records: u64| -> io::Result<(u64, File)> {
        out.flush()?;
        let mut f = out.into_inner().map_err(|e| e.into_error())?;
        f.seek(io::SeekFrom::Start(0))?;
        Ok((records, f))
    };

    // No room for even one 2-byte code unit of this parity within the
    // chunk/file bounds -> nothing to scan.
    if first >= chunk_end || first + 1 >= file_len {
        return finish(out, 0);
    }
    // Last byte offset at which a code unit could validly start.
    // - If this chunk is *not* the last one in the file (chunk_end <
    //   file_len), a code unit is allowed to start as late as chunk_end - 1,
    //   i.e. its second byte may spill one byte into the next chunk. That's
    //   intentional: it lets a code unit that straddles the chunk boundary
    //   still be recognized here (as a fragment, via starts/ends_at_chunk),
    //   instead of being silently missed by both neighboring chunks.
    // - If this chunk *is* the last one (chunk_end == file_len), there is no
    //   next chunk to spill into, so the code unit must fit entirely before
    //   file_len, hence file_len - 2.
    let max_start = if chunk_end < file_len {
        chunk_end.saturating_sub(1)
    } else {
        file_len.saturating_sub(2)
    };
    // Snap max_start down to the requested parity if it isn't already
    // aligned to it.
    let last_start = if max_start % 2 == parity {
        max_start
    } else {
        max_start.saturating_sub(1)
    };
    if last_start < first {
        return finish(out, 0);
    }
    // Number of code units of this parity between `first` and `last_start`
    // inclusive, stepping by 2 bytes each.
    let unit_count = (last_start - first) / 2 + 1;

    // Accumulated directly as a `String` rather than as a `Vec<u8>` that is
    // later handed to `String::from_utf8`, which was both a wasted second
    // pass over every matched run and outright wrong for `Latin1`: that
    // filter admits code units 0x00A0..=0x00FF, whose low bytes are not
    // valid standalone UTF-8, so the old `String::from_utf8(...).expect(...)`
    // panicked on any Latin-1 match. Pushing `char`s instead encodes each
    // code unit correctly (see the `as char` conversion below).
    let mut run_data = String::new();
    let mut run_offset = 0u64;
    let mut run_cb = 0u64;
    let mut run_cch = 0u64;
    let mut run_started = false;
    // Read in blocks of whole code units sized to roughly READ_BUFFER_SIZE
    // bytes, so I/O stays batched instead of one read() per 2-byte unit.
    let block_units = (READ_BUFFER_SIZE / 2).max(1) as u64;
    let mut processed = 0u64;
    // Block buffer allocated once and reused across iterations, rather than
    // per block: `vec![0u8; n]` zero-fills memory that `read_exact_at`
    // overwrites immediately afterwards, and at READ_BUFFER_SIZE scale that
    // wastes a full pass of memory bandwidth per block. Capped by
    // `unit_count` so small chunks don't reserve the whole buffer.
    let mut buf = vec![0u8; (min(block_units, unit_count) * 2) as usize];

    while processed < unit_count {
        // Same per-block (not per-unit) cancellation check as ascii::scan,
        // for the same reason: cheap enough to stay responsive, without
        // paying atomic-load overhead per 2-byte unit.
        if cancelled.load(Ordering::Relaxed) {
            return finish(out, records);
        }
        let n_units = min(block_units, unit_count - processed);
        let block_start = first + processed * 2;
        let want = (n_units * 2) as usize;
        read_exact_at(file, &mut buf[..want], block_start)?;
        let mut i = 0usize;
        while i < want {
            let abs = block_start + i as u64;
            let u = u16::from_le_bytes([buf[i], buf[i + 1]]);

            if cfg.filter().allows_u16(u) {
                if run_data.is_empty() {
                    run_offset = abs;
                    // Only true if this run's first unit is the very first
                    // possible unit of this parity in the chunk -- i.e. it
                    // may be a continuation of a run from the previous
                    // chunk, not a run that genuinely starts here.
                    run_started = abs == first;
                }
                // This scanner only ever sees code units whose value fits
                // in a single byte (the enclosing scanner is scoped to the
                // Latin-1 subset -- see the doc comment), and `u8 as char`
                // maps byte N to U+00N, which is exactly the Latin-1
                // mapping. That is *not* the same as pushing the raw byte:
                // code units 0xA0..=0xFF encode as two UTF-8 bytes, and
                // pushing the low byte alone would produce invalid UTF-8.
                run_data.push(u as u8 as char);
                run_cb += 2;
                run_cch += 1;
            } else if !run_data.is_empty() {
                // Run closed by a non-matching code unit mid-chunk, so it
                // cannot be continuing into the next chunk here.
                let rec = MatchRecord {
                    offset: run_offset,
                    cb: run_cb,
                    cch: run_cch,
                    encoding: InputEncoding::Utf16leAscii,
                    starts_at_chunk: run_started,
                    ends_at_chunk: false,
                    data: RecordData::Text(std::mem::take(&mut run_data)),
                };
                // Counted only if `emit_record` actually wrote it: runs
                // below `min_cch` that touch no chunk boundary are dropped
                // there. Counting them anyway made this scanner report
                // ~20000 records where 1 was written on realistic binary
                // input, which looked like a detection discrepancy against
                // `scanner::utf16le` (whose count is derived from its
                // surviving records and was correct all along).
                if emit_record(&mut out, rec, cfg.min_cch())? {
                    records += 1;
                }
                run_cb = 0;
                run_cch = 0;
                run_started = false;
            }
            i += 2;
        }
        processed += n_units;
    }
    // Trailing run still open when the scan range was exhausted. Unlike
    // ascii::scan (where reaching the end of the loop always means the
    // chunk boundary itself was hit), `unit_count` here was derived from
    // `max_start`, which may extend one byte past `chunk_end` to allow
    // boundary-straddling code units (see above) -- so `ends_at_chunk` is
    // computed explicitly by comparing the run's end against `chunk_end`,
    // rather than being unconditionally `true`.
    if !run_data.is_empty() {
        let rec = MatchRecord {
            offset: run_offset,
            cb: run_cb,
            cch: run_cch,
            encoding: InputEncoding::Utf16leAscii,
            starts_at_chunk: run_started,
            ends_at_chunk: run_offset + run_cb >= chunk_end,
            data: RecordData::Text(std::mem::take(&mut run_data)),
        };
        if emit_record(&mut out, rec, cfg.min_cch())? {
            records += 1;
        }
    }

    finish(out, records)
}
