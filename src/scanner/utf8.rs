use super::{emit_record, read_exact_at, READ_BUFFER_SIZE};
use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::InputEncoding;
use crate::filter;
use crate::record::{MatchRecord, RecordData};
use crate::tempfile_helper::create_temp_file;
use std::cmp::min;
use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether a decoded *multi-byte* (i.e. genuinely non-ASCII) character may
/// be included in a run. Single-byte (ASCII-range) candidates go through
/// `filter::is_ascii_char` instead, at the two call sites in `scan` below.
///
/// Note that *neither* path consults the user's `--filter` selection: see
/// the "Which scanners this actually affects" section on
/// `filter::CharacterFilter` for why this scanner is exempt. That leaves
/// this function with a single job -- guarding against characters that
/// would corrupt the crate's line-oriented text output (one match per
/// line) if they were allowed straight through.
///
/// `char::is_control` covers the C0/C1 control block (0x00..=0x1F,
/// 0x7F..=0x9F) -- including U+0085 NEL, itself line-breaking -- but
/// Unicode also defines two single-codepoint categories *outside* that
/// block that are line-breaking in exactly the same sense: U+2028 LINE
/// SEPARATOR (category Zl) and U+2029 PARAGRAPH SEPARATOR (category Zp).
/// Both are well-formed, non-control UTF-8 and would otherwise sail
/// straight through `is_control`, silently splitting a record's `data`
/// across multiple output lines. Ordinary Unicode space characters
/// (category Zs -- e.g. NBSP, em space, ideographic space) are neither
/// control characters nor line/paragraph separators, so they remain
/// allowed here without any special-casing.
#[inline]
fn multibyte_char_allowed(ch: char) -> bool {
    !ch.is_control() && ch != '\u{2028}' && ch != '\u{2029}'
}

/// The longest a single well-formed UTF-8 encoded scalar value can be.
const MAX_UTF8_CHAR_LEN: usize = 4;

/// The outcome of trying to decode one scalar value starting at the front
/// of a byte slice.
#[derive(Debug, PartialEq, Eq)]
enum Utf8Step {
    /// A complete, well-formed scalar value was decoded, consuming `len`
    /// bytes from the front of the slice.
    Complete { ch: char, len: usize },
    /// Every byte examined so far is a valid *prefix* of a longer
    /// sequence, but the slice ran out before the sequence could be
    /// confirmed complete. The caller needs more bytes (from a later read,
    /// or peeked from beyond a chunk boundary) before it can decide.
    Incomplete,
    /// The bytes at this position can never be extended into valid UTF-8:
    /// a bad lead byte, a continuation byte outside the range that lead
    /// byte allows (which also rules out overlong encodings and encoded
    /// surrogates -- see the lead-byte table below), or an assembled
    /// codepoint above U+10FFFF. The caller should end any in-progress run
    /// here and resync by skipping forward exactly one byte, same as a
    /// standard UTF-8 decoder recovering from corrupt input.
    Invalid,
}

/// Attempts to decode one UTF-8 scalar value from the start of `bytes`.
///
/// This mirrors the well-known UTF-8 validation table (the same shape used
/// by, e.g., the WHATWG encoding standard and Rust's own `core::str`
/// validation): every lead byte implies both a total sequence length and a
/// *narrowed* valid range for the second byte specifically. That narrowing
/// is what rules out overlong encodings (e.g. encoding `/` as a 2-byte
/// sequence) and encoded UTF-16 surrogate halves (U+D800..=U+DFFF, which
/// are not valid scalar values on their own) without needing a separate
/// check after assembling the codepoint.
fn decode_step(bytes: &[u8]) -> Utf8Step {
    let Some(&b0) = bytes.first() else {
        // Nothing to decode. Callers only invoke this on a non-empty
        // slice; treated as "need more bytes" for safety rather than
        // panicking.
        return Utf8Step::Incomplete;
    };

    // 1-byte case: plain ASCII, 0xxxxxxx.
    if b0 < 0x80 {
        return Utf8Step::Complete { ch: b0 as char, len: 1 };
    }

    // Multi-byte lead byte: determine the total sequence length and the
    // valid range for byte 1 specifically (byte 2 and 3, when present,
    // are always plain continuation bytes in 0x80..=0xBF).
    let (len, b1_lo, b1_hi): (usize, u8, u8) = match b0 {
        0xC2..=0xDF => (2, 0x80, 0xBF),
        // E0's second byte can't start below 0xA0: anything lower would
        // be an overlong 3-byte encoding of a codepoint that fits in 2
        // bytes.
        0xE0 => (3, 0xA0, 0xBF),
        0xE1..=0xEC => (3, 0x80, 0xBF),
        // ED's second byte can't reach 0xA0..=0xBF: that range is exactly
        // where the encoded UTF-16 surrogate halves (U+D800..=U+DFFF)
        // would fall.
        0xED => (3, 0x80, 0x9F),
        0xEE..=0xEF => (3, 0x80, 0xBF),
        // F0's second byte can't start below 0x90, for the same overlong
        // reason as E0.
        0xF0 => (4, 0x90, 0xBF),
        0xF1..=0xF3 => (4, 0x80, 0xBF),
        // F4's second byte can't exceed 0x8F: anything higher would
        // assemble a codepoint above U+10FFFF, the maximum valid scalar
        // value.
        0xF4 => (4, 0x80, 0x8F),
        // 0x80..=0xC1: stray continuation byte or an overlong 2-byte lead
        // (C0/C1 can only encode codepoints below U+0080). 0xF5..=0xFF:
        // would only ever encode codepoints above U+10FFFF.
        _ => return Utf8Step::Invalid,
    };

    if bytes.len() < len {
        // Not enough bytes yet to reach a final verdict -- but validate
        // whatever continuation bytes ARE present so a sequence that's
        // already provably broken is rejected immediately, rather than
        // waiting on bytes that could never make it valid anyway.
        for (i, &b) in bytes.iter().enumerate().skip(1) {
            let (lo, hi) = if i == 1 { (b1_lo, b1_hi) } else { (0x80, 0xBF) };
            if b < lo || b > hi {
                return Utf8Step::Invalid;
            }
        }
        return Utf8Step::Incomplete;
    }

    if bytes[1] < b1_lo || bytes[1] > b1_hi {
        return Utf8Step::Invalid;
    }
    for &b in &bytes[2..len] {
        if b < 0x80 || b > 0xBF {
            return Utf8Step::Invalid;
        }
    }

    // Every byte checked out -- assemble the scalar value by taking the
    // low bits of the lead byte and appending 6 bits from each
    // continuation byte.
    let mut cp: u32 = match len {
        2 => (b0 & 0x1F) as u32,
        3 => (b0 & 0x0F) as u32,
        4 => (b0 & 0x07) as u32,
        _ => unreachable!("decode_step only ever assigns len in 2..=4"),
    };
    for &b in &bytes[1..len] {
        cp = (cp << 6) | (b as u32 & 0x3F);
    }
    match char::from_u32(cp) {
        Some(ch) => Utf8Step::Complete { ch, len },
        // The range checks above already rule out surrogates and
        // out-of-range codepoints, so this should be unreachable in
        // practice; treated as Invalid rather than panicking, since
        // "reject and resync" is always a safe fallback for scanner code
        // that must never crash on untrusted input.
        None => Utf8Step::Invalid,
    }
}

/// Bookkeeping for the run currently being accumulated. Pulled into its
/// own small type (rather than five loose local variables mutated inline,
/// as `scanner::ascii`/`scanner::utf16le` do) because UTF-8 scanning has
/// more distinct places a run can end -- a disallowed character, invalid
/// bytes, a real chunk boundary, and a boundary-completion attempt that
/// itself fails -- and repeating the same five-field record-assembly logic
/// at each of those would be easy to let drift out of sync.
struct Run {
    data: Vec<u8>,
    offset: u64,
    cb: u64,
    cch: u64,
    started: bool,
}

impl Run {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            offset: 0,
            cb: 0,
            cch: 0,
            started: false,
        }
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Extends the run with one decoded character's raw UTF-8 bytes.
    /// `abs` is that character's absolute file offset; `is_chunk_start`
    /// tells this call whether, if this push begins a brand new run, that
    /// run should be considered to touch this chunk's logical start (see
    /// `scan`'s `still_at_chunk_start` tracking for what decides that).
    fn push(&mut self, abs: u64, char_bytes: &[u8], is_chunk_start: bool) {
        if self.data.is_empty() {
            self.offset = abs;
            self.started = is_chunk_start;
        }
        self.data.extend_from_slice(char_bytes);
        self.cb += char_bytes.len() as u64;
        self.cch += 1;
    }

    /// Emits the run (if non-empty) via `emit_record` and resets run
    /// state for the next one. Returns whether a record was actually
    /// *written* -- a non-empty run that `emit_record` drops for being
    /// below `min_cch` reports `false`, so callers' counts stay in step
    /// with what was emitted.
    fn close(&mut self, out: &mut BufWriter<File>, ends_at_chunk: bool, min_cch: u64) -> io::Result<bool> {
        if self.data.is_empty() {
            return Ok(false);
        }
        let rec = MatchRecord {
            offset: self.offset,
            cb: self.cb,
            cch: self.cch,
            encoding: InputEncoding::Utf8,
            starts_at_chunk: self.started,
            ends_at_chunk,
            data: RecordData::Text(String::from_utf8(std::mem::take(&mut self.data))
                .expect("UTF-8 scanner assembled invalid UTF-8")),
        };
        let written = emit_record(out, rec, min_cch)?;
        self.cb = 0;
        self.cch = 0;
        self.started = false;
        Ok(written)
    }
}

/// Scans one chunk for runs of well-formed, printable UTF-8 characters.
///
/// The admitted set is fixed and does *not* depend on `cfg.filter()`:
/// ASCII-range characters are judged by `filter::is_ascii_char` and
/// everything wider by `multibyte_char_allowed`. See the "Which scanners
/// this actually affects" section on `filter::CharacterFilter` for the
/// reasoning.
///
/// Unlike `scanner::ascii` (1 byte per character) and `scanner::utf16le`
/// (a fixed 2 bytes per code unit, handled by scanning two byte parities),
/// UTF-8 characters are 1 to 4 bytes each, and there's only one possible
/// byte alignment -- so there's no parity-stream split here. The
/// complexity that *does* exist is entirely about a single character's
/// bytes potentially straddling either an internal I/O read-block boundary
/// (handled by carrying leftover bytes into the next block, same idea as
/// UTF-16LE's block reads) or the chunk boundary itself (handled below by
/// peeking up to `MAX_UTF8_CHAR_LEN - 1` bytes past `chunk_end`, the
/// variable-length generalization of `scanner::utf16le`'s `max_start`
/// logic for a fixed 2-byte code unit).
///
/// Peeked boundary bytes are never double-counted: the next chunk begins
/// reading at its own `chunk.offset`, which is exactly where this chunk's
/// peek left off, and any continuation bytes (0x80..=0xBF) encountered
/// there with no preceding lead byte in that chunk's own context are
/// correctly rejected by `decode_step` as `Invalid` -- they simply don't
/// start a new run, rather than being (incorrectly) re-emitted.
///
/// One consequence of that rejection: the next chunk's first *usable*
/// character can end up sitting a few bytes after its nominal
/// `chunk.offset` (past however many orphaned continuation bytes needed
/// skipping), at a position that isn't knowable in advance the way
/// `scanner::ascii`/`scanner::utf16le` can compute theirs. `starts_at_chunk`
/// is set accordingly -- see `still_at_chunk_start` below -- so that a run
/// beginning right after such orphaned bytes still correctly reports
/// itself as touching the chunk boundary and can be joined with the
/// previous chunk's pending fragment by the merger.
pub(crate) fn scan(
    file: &File,
    file_len: u64,
    chunk: &Chunk,
    cfg: &Config,
    temp_path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<(u64, File)> {
    let chunk_end = min(chunk.offset + chunk.len, file_len);
    let temp_file = create_temp_file(temp_path, cfg.keep_temp())?;
    let mut out = BufWriter::with_capacity(crate::WRITE_BUFFER_SIZE, temp_file);
    let mut records = 0u64;
    let mut run = Run::new();
    // Tracks whether every byte examined *since `chunk.offset`* has been
    // part of an `Invalid` decode step -- i.e. whether we're still inside
    // a possible run of orphaned continuation bytes at the very start of
    // this chunk, left behind by the previous chunk's boundary-completion
    // peek (see `scan`'s doc comment above). Flips to `false` the
    // moment any `Complete` character is decoded, allowed or not.
    //
    // This is UTF-8's replacement for `scanner::ascii`'s simple
    // `run_offset == chunk.offset` / `scanner::utf16le`'s parity-adjusted
    // `abs == first` check: those work because the "true start" of a
    // chunk's content is statically known ahead of time (a fixed byte
    // offset, or a fixed parity-aligned offset). For UTF-8, a straddling
    // character can leave anywhere from 0 to `MAX_UTF8_CHAR_LEN - 1`
    // orphaned continuation bytes at the head of this chunk, and there's
    // no way to know how many without actually decoding -- so instead of
    // comparing against a precomputed position, this chunk's first
    // *decodable* character is treated as "at chunk start" as long as
    // nothing but such orphaned bytes preceded it.
    let mut still_at_chunk_start = true;

    let finish = |mut out: BufWriter<File>, records: u64| -> io::Result<(u64, File)> {
        out.flush()?;
        let mut f = out.into_inner().map_err(|e| e.into_error())?;
        f.seek(io::SeekFrom::Start(0))?;
        Ok((records, f))
    };

    if chunk.offset >= chunk_end {
        return finish(out, 0);
    }

    // `carry` holds a not-yet-decodable trailing sequence from the
    // previous block read (at most `MAX_UTF8_CHAR_LEN - 1` bytes -- a
    // full char's worth would have decoded already). It's re-prepended to
    // the next block so decoding is oblivious to internal I/O chunking.
    let mut carry: Vec<u8> = Vec::new();
    let block_cap = READ_BUFFER_SIZE.max(MAX_UTF8_CHAR_LEN * 2);
    let mut buf = vec![0u8; block_cap];
    let mut pos = chunk.offset;

    while pos < chunk_end {
        if cancelled.load(Ordering::Relaxed) {
            // Cooperative cancellation: stop without flushing any
            // in-progress run or attempting boundary completion, same as
            // scanner::ascii -- a cancelled scan's output is discarded or
            // treated as partial by the caller, so there's no need for
            // the trailing state to be consistent.
            return finish(out, records);
        }

        let carry_len = carry.len();
        buf[..carry_len].copy_from_slice(&carry);
        let want = min(block_cap - carry_len, (chunk_end - pos) as usize);
        read_exact_at(file, &mut buf[carry_len..carry_len + want], pos)?;
        let filled = carry_len + want;
        // Absolute file offset corresponding to buf[0] in this iteration:
        // `carry`'s bytes sit immediately before the freshly-read span, so
        // this is just `pos` backed up by however many carry bytes there
        // are.
        let block_start_abs = pos - carry_len as u64;
        pos += want as u64;
        carry.clear();

        let mut i = 0usize;
        while i < filled {
            match decode_step(&buf[i..filled]) {
                Utf8Step::Complete { ch, len } => {
                    let abs = block_start_abs + i as u64;
                    // Neither branch consults `cfg.filter()`: this scanner
                    // is deliberately exempt from `--filter`. UTF-8 is
                    // self-synchronizing and `decode_step` has already
                    // structurally validated this character, so there is
                    // no false-positive problem here for the filter to
                    // solve -- whereas applying it would mean that
                    // dropping `ascii` from `--filter` (a normal way to
                    // quiet scanner::utf16le) would silently stop UTF-8
                    // from matching plain ASCII too. See the
                    // "Which scanners this actually affects" section on
                    // `filter::CharacterFilter`.
                    let allowed = if len == 1 {
                        filter::is_ascii_char(ch)
                    } else {
                        multibyte_char_allowed(ch)
                    };
                    if allowed {
                        run.push(abs, &buf[i..i + len], still_at_chunk_start);
                    } else if run.close(&mut out, false, cfg.min_cch())? {
                        records += 1;
                    }
                    // Any successfully decoded character -- whether pushed
                    // into a run or filtered out -- proves this position
                    // is genuine content, not leading boundary garbage, so
                    // no run starting after this point can claim to touch
                    // the chunk's start.
                    still_at_chunk_start = false;
                    i += len;
                }
                Utf8Step::Invalid => {
                    // The invalid byte itself is dropped (never included
                    // in any record), same treatment as a disallowed
                    // ASCII byte in scanner::ascii; decoding resumes one
                    // byte later.
                    if run.close(&mut out, false, cfg.min_cch())? {
                        records += 1;
                    }
                    i += 1;
                }
                Utf8Step::Incomplete => {
                    // Not enough bytes left in this block to decide.
                    // Carry the remainder into the next block (or into
                    // the post-loop boundary-completion step, if this was
                    // the block that reached chunk_end).
                    break;
                }
            }
        }
        if i < filled {
            carry.extend_from_slice(&buf[i..filled]);
        }
    }

    // Main body of the chunk is fully scanned. If a multi-byte sequence
    // was left dangling right at the boundary, try to complete it using
    // bytes from just beyond chunk_end -- but only if there *is* a beyond
    // (i.e. this isn't the file's last chunk). The completed character,
    // if any, belongs to this chunk's output; the next chunk will never
    // re-read it as anything meaningful (see the module-level doc comment
    // above).
    if !carry.is_empty() {
        let carry_start_abs = chunk_end - carry.len() as u64;
        if chunk_end < file_len {
            let need = MAX_UTF8_CHAR_LEN - carry.len();
            let avail = min(need as u64, file_len - chunk_end) as usize;
            let mut extended = carry.clone();
            if avail > 0 {
                let mut peek = vec![0u8; avail];
                read_exact_at(file, &mut peek, chunk_end)?;
                extended.extend_from_slice(&peek);
            }
            if let Utf8Step::Complete { ch, len } = decode_step(&extended) {
                debug_assert!(len <= extended.len());
                // Same len==1-vs-multibyte split as the main loop above.
                // In practice `len` is never 1 here -- a single ASCII byte
                // always decodes successfully within the main loop and
                // never ends up dangling in `carry` -- but this mirrors
                // the main loop's rule exactly rather than assuming that.
                let allowed = if len == 1 {
                    filter::is_ascii_char(ch)
                } else {
                    multibyte_char_allowed(ch)
                };
                if allowed {
                    run.push(carry_start_abs, &extended[..len], still_at_chunk_start);
                } else if run.close(&mut out, false, cfg.min_cch())? {
                    records += 1;
                }
            }
            // Still Incomplete (ran out of real file bytes to peek) or
            // Invalid (genuinely malformed): the dangling bytes are
            // truncated/corrupt input and are simply dropped, same as
            // any other invalid sequence.
        }
        // If this chunk reaches file_len, a dangling partial sequence
        // here means the file itself ends mid-character -- nothing to
        // peek, so it's dropped as truncated input.
    }

    // Flush a run still open when the chunk ended. `ends_at_chunk` is
    // computed by comparing the run's end against `chunk_end` rather than
    // being unconditionally `true` (contrast scanner::ascii), because the
    // boundary-completion step above may have extended the run's byte
    // range up to (but not past) `chunk_end` using peeked bytes that
    // physically belong to the next chunk's region -- the same reasoning
    // `scanner::utf16le` uses for the same comparison.
    if !run.is_empty() {
        let ends_at_chunk = run.offset + run.cb >= chunk_end;
        if run.close(&mut out, ends_at_chunk, cfg.min_cch())? {
            records += 1;
        }
    }

    finish(out, records)
}