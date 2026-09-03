//! Merges pre-sorted intermediate record streams (one per encoding, or the
//! two parity streams a scanner produces internally) into a single stream
//! sorted by (offset, encoding code).
//!
//! This module is intentionally encoding-agnostic: it only ever sees
//! `Vec<File>` and the crate's intermediate record format, never an
//! `InputEncoding` match. Adding a new scanner therefore requires no changes
//! here at all, as long as that scanner's output file is sorted by offset.
//! For most scanners a single left-to-right pass makes that automatic; the
//! UTF-16LE ones, which interleave two parity streams, get there by
//! merging those streams through `merge_sorted_files` first.

use crate::READ_BUFFER_SIZE;
use crate::config::Config;
use crate::record::{read_record, write_record, MatchRecord};
use crate::tempfile_helper::create_temp_file;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// K-way merge over already-open, already-sorted record streams.
///
/// Each input `File` is expected to contain `MatchRecord`s written in
/// non-decreasing `offset` order (ties broken arbitrarily by the writer).
/// `ChunkStream` keeps one buffered reader and one "lookahead" record
/// (`heads[i]`) per input, so it can repeatedly pick the globally smallest
/// head without re-reading from disk more than once per emitted record.
struct ChunkStream {
    /// One buffered reader per input file.
    readers: Vec<BufReader<File>>,
    /// `heads[i]` is the next unread record from `readers[i]`, or `None`
    /// once that reader has been exhausted (EOF). This is the classic
    /// "lookahead buffer" used to implement a merge without needing a
    /// full priority queue for small numbers of streams.
    heads: Vec<Option<MatchRecord>>,
}

impl ChunkStream {
    /// Opens every input file for buffered reading and primes `heads` with
    /// the first record of each (or `None` if a file is empty).
    fn open(inputs: Vec<File>) -> io::Result<Self> {
        let mut readers = Vec::with_capacity(inputs.len());
        let mut heads = Vec::with_capacity(inputs.len());
        for file in inputs {
            let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, file);
            let head = read_record(&mut reader)?;
            readers.push(reader);
            heads.push(head);
        }
        Ok(Self { readers, heads })
    }

    /// Returns the next record in global sort order across all streams,
    /// advancing whichever stream that record came from.
    ///
    /// Complexity is O(n) per call in the number of input streams, which is
    /// fine here since the number of streams equals the number of encodings
    /// (or parity streams) being merged -- always a small, bounded count,
    /// never proportional to the data size.
    fn next_record(&mut self) -> io::Result<Option<MatchRecord>> {
        // Find the index of the stream whose current head record sorts
        // first. Sort key is (offset, encoding code): offset is the primary
        // ordering, and encoding code is a deterministic tiebreaker so that
        // merge order is stable and reproducible when two encodings report
        // a match at the exact same offset.
        let next = self
            .heads
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.as_ref().map(|r| (i, r)))
            .min_by_key(|(_, r)| (r.offset, r.encoding as u16));

        // All streams exhausted -> merge is done.
        let Some((idx, _)) = next else {
            return Ok(None);
        };

        // Take ownership of the winning record and refill that stream's
        // lookahead slot from disk (or leave it as None at EOF).
        let rec = self.heads[idx].take().unwrap();
        self.heads[idx] = read_record(&mut self.readers[idx])?;
        Ok(Some(rec))
    }
}

/// Merges every encoding's scan result for one chunk into a single,
/// offset-sorted temp file.
///
/// This is the public entry point used by the chunk-processing pipeline:
/// each `InputEncoding` scanner writes its own sorted intermediate file for
/// a chunk, and this function fans them all into one sorted output file
/// that downstream reporting code can consume as a single stream.
pub fn merge_chunk_encodings(
    inputs: Vec<File>,
    output_path: &Path,
    cancelled: &AtomicBool,
    cfg: &Config,
) -> io::Result<File> {
    let mut stream = ChunkStream::open(inputs)?;
    let out_file = create_temp_file(output_path, cfg.keep_temp())?;
    let mut out = BufWriter::with_capacity(crate::WRITE_BUFFER_SIZE, out_file);

    // Drive the merge one record at a time. The cancellation check happens
    // inside the `while let` condition (rather than in the loop body) so
    // that it's re-evaluated before *every* record is pulled, giving fast
    // response to cancellation even on very large chunks.
    while let Some(rec) = {
        if cancelled.load(Ordering::Relaxed) {
            // Cooperative cancellation: bail out early without treating
            // this as an error. Rewind the (possibly partially written)
            // output file to the start before handing it back, matching
            // the contract of the normal-completion path below, since the
            // caller doesn't distinguish a cancelled result from a
            // completed one at the type level -- it just gets a `File`
            // positioned at offset 0.
            let mut f = out.into_inner().map_err(|e| e.into_error())?;
            f.seek(io::SeekFrom::Start(0))?;
            return Ok(f);
        }
        stream.next_record()?
    } {
        write_record(&mut out, &rec)?;
    }

    // Normal completion: flush the buffered writer, unwrap it back to the
    // raw `File`, and rewind so the caller can read the merged result from
    // the beginning.
    out.flush()?;
    let mut f = out.into_inner().map_err(|e| e.into_error())?;
    f.seek(io::SeekFrom::Start(0))?;
    Ok(f)
}

/// Same merge algorithm as `merge_chunk_encodings`, used internally by
/// scanners that need to combine multiple sorted streams of their own
/// (`scanner::utf16le_ascii`'s even/odd parity streams) before handing a
/// single result file
/// back up to the caller.
///
/// This is a separate, non-cancellable copy of the merge loop rather than a
/// call into `ChunkStream`/`merge_chunk_encodings`, because:
///   - it's `pub(crate)` and only ever merges a scanner's own small number
///     of internal streams, so cancellation mid-merge isn't wired in here;
///   - inlining avoids needing to thread an `AtomicBool` through every
///     scanner just to satisfy this internal helper.
/// If this duplication grows, consider factoring the shared min-by-key loop
/// out of both functions.
pub(crate) fn merge_sorted_record_files(
    inputs: Vec<File>,
    output_path: &Path,
    cfg: &Config,
) -> io::Result<File> {
    // Manually open + prime lookahead heads here instead of going through
    // `ChunkStream::open`, since this function doesn't need the struct's
    // reusable `next_record` stepping API -- it just runs the merge to
    // completion in one shot below.
    let mut readers: Vec<BufReader<File>> = inputs
        .into_iter()
        .map(|f| BufReader::with_capacity(READ_BUFFER_SIZE, f))
        .collect();
    let mut heads = Vec::<Option<MatchRecord>>::with_capacity(readers.len());
    for reader in &mut readers {
        heads.push(read_record(reader)?);
    }

    let out_file = create_temp_file(output_path, cfg.keep_temp())?;
    let mut out = BufWriter::with_capacity(crate::WRITE_BUFFER_SIZE, out_file);

    // Same (offset, encoding code) k-way merge as `ChunkStream::next_record`,
    // but run to completion unconditionally (no cancellation check) since
    // internal parity-stream merges are expected to be small and fast.
    loop {
        let next = heads
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.as_ref().map(|r| (i, r)))
            .min_by_key(|(_, r)| (r.offset, r.encoding as u16));

        let Some((idx, _)) = next else { break };
        let rec = heads[idx].take().unwrap();
        write_record(&mut out, &rec)?;
        heads[idx] = read_record(&mut readers[idx])?;
    }

    out.flush()?;
    let mut f = out.into_inner().map_err(|e| e.into_error())?;
    f.seek(io::SeekFrom::Start(0))?;
    Ok(f)
}
