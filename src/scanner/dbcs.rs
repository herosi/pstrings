//! The shared scanning engine for **multi-byte, non-self-synchronizing**
//! legacy CJK encodings: CP932 (Shift_JIS), GBK, EUC-KR, Big5 and
//! GB18030.
//!
//! # Why this module exists
//!
//! These encodings are structurally identical from a scanner's point of
//! view. Each one:
//!
//!   * has a set of bytes that stand alone as characters,
//!   * has a set of *lead* bytes that must be followed by a *trail* byte,
//!   * has a trail-byte range that **overlaps printable ASCII** and the
//!     lead-byte range, and
//!   * has holes in its two-byte code space, so passing the structural
//!     range checks is necessary but not sufficient.
//!
//! Measured against `encoding_rs`, for each encoding's lowest lead byte
//! and the 95 printable-ASCII bytes:
//!
//! | encoding | lead range              | leads | trails for that lead | overlapping ASCII |
//! |----------|-------------------------|-------|----------------------|-------------------|
//! | CP932    | 0x81..=0x9F, 0xE0..=0xFC |  55  | 147                  | 63 / 95           |
//! | GBK      | 0x81..=0xFE             | 126   | 190                  | 63 / 95           |
//! | GB18030  | 0x81..=0xFE             | 126   | 190                  | 63 / 95           |
//! | EUC-KR   | 0x81..=0xFD             | 124   | 178                  | 52 / 95           |
//! | Big5     | 0x87..=0xFE             | 120   | 125                  | 62 / 95           |
//!
//! (The trail counts are for the *lowest* lead specifically. Taken over
//! every lead, the union is 188 trails for CP932 and 157 for Big5 -- both
//! wider than any single lead admits -- while the other three are uniform.
//! EUC-KR's 124 leads sit inside a 125-byte range: 0xC9 is the unassigned
//! user-defined row.)
//!
//! That third property is the important one: it is precisely what makes
//! them all non-self-synchronizing, and therefore what forces the
//! deferred-boundary design described at length on `scan` below.
//!
//! # "Double-byte" is now approximate
//!
//! GB18030 also has a four-byte form, which is why the name above says
//! "multi-byte". It required no change to the engine beyond `decode_step`
//! learning a third length: the scanning loops here treat a character's
//! length as opaque (`i += len`), and `carry` holds *everything*
//! unconsumed at the end of a block, so a two- or three-byte carry works
//! exactly as a one-byte one always did. See `Dbcs::starts_four_byte` for
//! why one byte of lookahead suffices to choose between the forms.
//!
//! The alternative to this module was to copy `scanner/cp932.rs` once per
//! encoding and change the byte-range predicates. That would have produced
//! five near-identical copies of some of the most subtle code in the crate
//! -- the leading/trailing boundary deferral and the multi-chunk raw
//! chaining. This crate has already been bitten once by exactly that
//! shape of duplication: `main.rs` carried its own hand-rolled copy of
//! `outputter::flush_pending`'s logic, the two drifted, and the result was
//! that *every* CP932 match reaching end-of-file was silently discarded --
//! invisible to the test suite, because the tests exercised the correct
//! copy. Duplicated logic is where divergence hides, so the engine lives
//! here once and the per-encoding differences are expressed as data.

use super::{emit_record, read_exact_at, ResolvedFragment, READ_BUFFER_SIZE};
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

/// Everything that differs between CP932, GBK, EUC-KR, Big5 and GB18030.
///
/// Implementors are zero-sized types; every method is `#[inline]`-able and
/// dispatched statically through the generic engine below, so routing
/// through this trait costs nothing at runtime compared to the previous
/// hand-written CP932 scanner.
pub(crate) trait Dbcs {
    /// The `InputEncoding` variant records are tagged with.
    const ENCODING: InputEncoding;

    /// The `encoding_rs` decoder used both to validate candidate
    /// sequences and to produce the final text, so "is this sequence
    /// valid" and "what character is it" can never disagree.
    fn decoder() -> &'static encoding_rs::Encoding;

    /// Whether `b` can begin a multi-byte sequence, of any of the lengths
    /// this encoding defines. It gates the two- and four-byte forms
    /// alike; `starts_four_byte` then picks between them.
    fn is_lead(b: u8) -> bool;

    /// Whether `b` can be the second byte of a two-byte sequence.
    ///
    /// For all of these encodings this range overlaps printable ASCII,
    /// which is the whole reason for the deferred-boundary design.
    fn is_trail(b: u8) -> bool;

    /// Whether `b` is a character in its own right (printable ASCII, plus
    /// whatever single-byte extras the encoding defines -- e.g. CP932's
    /// half-width katakana at 0xA1..=0xDF).
    ///
    /// Deliberately does *not* consult the user's `--filter` selection,
    /// and neither does any other decision made here. These encodings
    /// validate structurally, so they have no false-positive problem for
    /// `--filter` to solve; see the "Which scanners this actually
    /// affects" section on `filter::CharacterFilter`.
    fn is_single(b: u8) -> bool;

    /// Whether this encoding also has a four-byte form, and if so whether
    /// `bytes` (which is at least 2 long and starts with a byte that
    /// `is_lead` accepts) begins one.
    ///
    /// # Why this is shaped as "is it four bytes", not "how long is it"
    ///
    /// Only GB18030 overrides this. Its four-byte form is
    /// `0x81-0xFE, 0x30-0x39, 0x81-0xFE, 0x30-0x39`, and crucially the
    /// second byte alone decides between the two forms: a digit in that
    /// position means four bytes, anything else means two. Measured
    /// against `encoding_rs`, **zero** byte sequences are ambiguous
    /// between the two readings, and **zero** valid two-byte pairs have a
    /// digit as their trail byte. So a one-byte lookahead is sufficient
    /// and no backtracking is ever required -- which is what lets the
    /// four-byte form slot into the existing engine rather than needing a
    /// different one.
    ///
    /// The default is `false`, so the encodings that are purely
    /// double-byte inherit exactly their previous behaviour with no
    /// per-encoding code and no runtime cost (the branch folds away, since
    /// the implementation is statically known to be `false`).
    #[inline]
    fn starts_four_byte(_bytes: &[u8]) -> bool {
        false
    }
}

/// Whether `bytes` form a sequence the encoding actually assigns a
/// character to.
///
/// The structural `is_lead`/`is_trail` checks are necessary but not
/// sufficient: every one of these encodings has unassigned points inside
/// its nominal ranges (CP932's `0xFC 0x4C`, for instance, is structurally
/// in-range on both bytes but undefined). Rather than hand-maintain a
/// second table of assigned sequences -- thousands of entries per
/// encoding, easy to get subtly wrong, and a second source of truth that
/// could drift from the actual decoder -- this defers entirely to
/// `encoding_rs`.
///
/// Takes a slice rather than two bytes so the same check covers GB18030's
/// four-byte form.
#[inline]
fn is_defined_seq<E: Dbcs>(bytes: &[u8]) -> bool {
    let (_, had_errors) = E::decoder().decode_without_bom_handling(bytes);
    !had_errors
}

/// Loose, *role-agnostic* membership test: could `b` be any part of a
/// sequence at all -- lead, trail, or standalone -- without committing to
/// which? Used only to bound how far a chunk-boundary raw collection
/// extends, never to decide how to segment or decode anything.
#[inline]
fn is_shaped<E: Dbcs>(b: u8) -> bool {
    E::is_lead(b) || E::is_trail(b) || E::is_single(b)
}

/// The outcome of trying to classify one character at the front of a slice.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// A structurally valid, assigned character consuming `len` bytes.
    Complete { len: usize },
    /// The bytes available so far are a valid prefix, but there are not
    /// enough of them to confirm or reject the character. For the
    /// double-byte encodings this only ever means "a lead byte with no
    /// second byte yet"; for GB18030 it can also mean one or two bytes
    /// short of a complete four-byte sequence (the form is only selected
    /// once the second byte is in hand, so it is never three short).
    Incomplete,
    /// These bytes cannot form a valid character. The caller ends any
    /// in-progress run and resyncs forward by one byte.
    Invalid,
}

/// Classifies the character at the front of `bytes`.
///
/// # On the four-byte case
///
/// GB18030 aside, there is only one sequence length to consider, so the
/// only question is whether the bytes are valid. The four-byte form does
/// not disturb that, because its second byte is a
/// digit and no valid two-byte pair has a digit as its trail byte
/// (measured exhaustively against `encoding_rs`: 0 such pairs, and 0
/// sequences ambiguous between the two readings). So the second byte
/// alone selects the form -- there is never a need to try one length,
/// fail, and back up.
///
/// Note that `Step::Incomplete` is returned for *any* truncated prefix,
/// not just a lone lead byte. The callers already treat `Incomplete` as
/// "stop here and carry every remaining byte over to the next block",
/// with no assumption about how many bytes that is, so a two- or
/// three-byte carry works exactly like a one-byte one.
fn decode_step<E: Dbcs>(bytes: &[u8]) -> Step {
    let Some(&b0) = bytes.first() else {
        return Step::Incomplete;
    };
    if E::is_single(b0) {
        return Step::Complete { len: 1 };
    }
    if E::is_lead(b0) {
        let Some(&b1) = bytes.get(1) else {
            return Step::Incomplete;
        };
        if E::starts_four_byte(bytes) {
            // The second byte has committed us to the four-byte form.
            if bytes.len() < 4 {
                return Step::Incomplete;
            }
            if is_defined_seq::<E>(&bytes[..4]) {
                return Step::Complete { len: 4 };
            }
            return Step::Invalid;
        }
        if E::is_trail(b1) && is_defined_seq::<E>(&[b0, b1]) {
            return Step::Complete { len: 2 };
        }
        return Step::Invalid;
    }
    Step::Invalid
}

/// Counts how many characters `bytes` decodes into, assuming (as is always
/// true for how this module builds `bytes`) that every byte in it was
/// already confirmed part of a `Complete` step.
///
/// # Why this decodes rather than counting steps
///
/// The obvious implementation is to walk `decode_step` and add one per
/// `Complete`. That is wrong in general, because a `Complete` step is one
/// *encoded sequence*, which is not always one *Unicode scalar*: BIG5 has
/// four two-byte sequences that decode to two scalars each (`88 62` ->
/// U+00CA U+0304, and likewise `88 64`, `88 a3`, `88 a5` -- a Latin letter
/// plus a combining mark). Counting steps would report those as one
/// character.
///
/// `cch` is compared against `--min-length` and reported by `--stats`, and
/// every other scanner here defines it as the number of Unicode scalars
/// (`scanner::utf8`, `scanner::win1251`, and the UTF-16LE pair all count
/// scalars). Counting sequences instead would silently make `-m` mean
/// something different depending on `-e`, which is exactly the kind of
/// inconsistency that is invisible until it produces a wrong answer.
///
/// Deriving the count from the decode also removes the possibility of the
/// two disagreeing: both callers decode these same bytes immediately
/// afterwards, so this now measures the very string that gets emitted.
///
/// This was verified to be a no-op for the four encodings that have no
/// four-byte form -- exhaustively, over every valid sequence (9,763 for
/// Shift_JIS, 24,036 for GBK, 17,144 for EUC-KR, and Big5's likewise):
/// zero disagreements, because none
/// of them has a one-sequence-to-many-scalars mapping. See
/// `count_chars_agrees_with_sequence_counting_for_simple_encodings` in
/// `src/tests/scanner_dbcs_tests.rs`, which pins that.
fn count_chars<E: Dbcs>(bytes: &[u8]) -> u64 {
    E::decoder()
        .decode_without_bom_handling(bytes)
        .0
        .chars()
        .count() as u64
}

/// Counts how many bytes at the very start of a chunk must be deferred as
/// an unsegmented raw prefix, because whether they end up joined with the
/// *previous* chunk's pending fragment can change how *they themselves*
/// need to be split into characters.
///
/// The answer is "every leading byte that is loosely shaped like this
/// encoding" (`is_shaped`), i.e. the whole run, right through into what
/// would otherwise be scanned as ordinary interior content. Two properties
/// make that both necessary and sufficient:
///
///   - *Sufficient*: a byte failing `is_shaped` cannot be a lead, a trail,
///     or a standalone character under *any* reading. So no run can extend
///     across it, and the byte after it is an unambiguously fresh start --
///     exactly where normal interior scanning can safely resume.
///
///   - *Necessary*: stopping any earlier -- e.g. the moment the
///     join-vs-fresh ambiguity happens to resolve itself -- would split
///     one continuous run into a `Raw` prefix record plus a separate
///     `Text` record starting mid-run. `outputter` can only join a pending
///     fragment with a record sitting exactly at the chunk's start, and
///     `record::append_data` cannot mix a `Raw` payload with a `Text` one,
///     so those two pieces could never be stitched back together and the
///     string would be reported broken in half.
///
/// A run collected this way may of course contain a structurally in-range
/// but *undefined* pair; that's fine, since `segment_raw` breaks the run
/// there when the fragment is finally resolved, exactly as interior
/// scanning would have.
fn leading_run_len<E: Dbcs>(data: &[u8]) -> usize {
    data.iter().take_while(|&&b| is_shaped::<E>(b)).count()
}

/// Bookkeeping for the run currently being accumulated. Always holds raw
/// bytes (never decodes incrementally) -- interior runs get decoded once,
/// in one shot, when they close (`close_as_text`); a run touching
/// chunk_end instead gets handed off undecoded (`into_raw_record`), since
/// its true resolution depends on the next chunk.
struct Run {
    data: Vec<u8>,
    offset: u64,
}

impl Run {
    fn new() -> Self {
        Self { data: Vec::new(), offset: 0 }
    }

    fn push(&mut self, abs: u64, char_bytes: &[u8]) {
        if self.data.is_empty() {
            self.offset = abs;
        }
        self.data.extend_from_slice(char_bytes);
    }

    /// Closes an interior run (one that does NOT touch chunk_end, so its
    /// resolution is unambiguous) as fully-decoded text. Returns whether a
    /// record was actually *written* -- an accumulated run that
    /// `emit_record` drops for being below `min_cch` reports `false`, so
    /// callers' record counts stay in step with what was emitted.
    fn close_as_text<E: Dbcs>(
        &mut self,
        out: &mut BufWriter<File>,
        min_cch: u64,
    ) -> io::Result<bool> {
        if self.data.is_empty() {
            return Ok(false);
        }
        let bytes = std::mem::take(&mut self.data);
        let cb = bytes.len() as u64;
        let cch = count_chars::<E>(&bytes);
        let (decoded, had_errors) = E::decoder().decode_without_bom_handling(&bytes);
        debug_assert!(
            !had_errors,
            "{:?} scanner accumulated a byte sequence encoding_rs rejects",
            E::ENCODING
        );
        let rec = MatchRecord {
            offset: self.offset,
            cb,
            cch,
            encoding: E::ENCODING,
            starts_at_chunk: false,
            ends_at_chunk: false,
            data: RecordData::Text(decoded.into_owned()),
        };
        emit_record(out, rec, min_cch)
    }

    /// Converts the run into a `RecordData::Raw` record for a run that
    /// touches chunk_end (regardless of whether it closed cleanly right at
    /// the boundary or is a genuinely dangling incomplete lead byte --
    /// either way, its true resolution depends on what the next chunk
    /// reports, so it's never decoded here). Returns `None` if nothing was
    /// accumulated.
    fn into_raw_record<E: Dbcs>(self, chunk_offset: u64) -> Option<MatchRecord> {
        if self.data.is_empty() {
            return None;
        }
        Some(MatchRecord {
            offset: self.offset,
            cb: self.data.len() as u64,
            // Segmentation hasn't happened yet, so `cch` is the documented
            // placeholder until `outputter` resolves this via `segment_raw`.
            cch: 0,
            encoding: E::ENCODING,
            starts_at_chunk: self.offset == chunk_offset,
            ends_at_chunk: true,
            data: RecordData::Raw(self.data),
        })
    }
}

/// Scans one chunk for characters decodable in `E`.
///
/// # Why chunk boundaries need special handling here
///
/// `scanner::utf8` and `scanner::utf16le` both reconstruct strings that
/// straddle a chunk boundary by having one chunk peek a few bytes into the
/// next chunk's territory, then flagging the result so the merger/
/// outputter can stitch it back together with whatever the next chunk
/// reports. That trick relies on the next chunk being able to tell,
/// unambiguously, that the bytes it starts with were already consumed by
/// its predecessor -- which holds for UTF-8 (continuation bytes are never
/// mistakable for a fresh lead byte or ASCII byte) and UTF-16LE (parity
/// alignment pins down unambiguously which byte a code unit starts on).
///
/// None of the encodings handled here has an equivalent guarantee: as the
/// module doc comment's table shows, the trail-byte range overlaps the
/// printable-ASCII range, and it overlaps the lead-byte range as well, so
/// the ASCII range and the lead-byte range. A byte sitting at the very
/// start of a chunk could genuinely be the leftover trail byte of a pair
/// the previous chunk already completed, an ordinary fresh single-byte
/// character, or the first byte of an unrelated new pair -- and, worse, if
/// that byte is itself a valid lead byte that *also* forms a valid pair
/// with the byte after it, the two possible readings of the chunk's own
/// content can diverge for a while, not just for one byte (see
/// `leading_run_len`).
///
/// So rather than guess (which risks silently producing wrong or garbled
/// characters near a boundary) or refuse to reconstruct anything at all
/// across a boundary, this scanner defers the decision: chunk-boundary-
/// touching runs are collected as raw, unsegmented, undecoded bytes
/// (`RecordData::Raw`) and handed to `outputter`, which resolves them (via
/// `segment_raw`) only once it knows whether -- and how -- they join with
/// their neighbor. The vast majority of each chunk's content (everything
/// strictly between the leading and trailing boundary-touching runs) is
/// scanned and decoded immediately; only at most two runs per chunk ever
/// take the deferred path.
pub(crate) fn scan<E: Dbcs>(
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

    let finish = |mut out: BufWriter<File>, records: u64| -> io::Result<(u64, File)> {
        out.flush()?;
        let mut f = out.into_inner().map_err(|e| e.into_error())?;
        f.seek(io::SeekFrom::Start(0))?;
        Ok((records, f))
    };

    if chunk.offset >= chunk_end {
        return finish(out, 0);
    }

    // No per-encoding minimum is enforced here. A character split across
    // the end of a block is handled by `decode_step` returning
    // `Step::Incomplete` and the loop carrying every unconsumed byte into
    // the next block, which works for any character length; the buffer
    // size only affects how many syscalls this takes.
    let block_cap = READ_BUFFER_SIZE;
    let mut buf = vec![0u8; block_cap];

    // --- Leading boundary handling ---
    // Only chunks other than the file's very first one can possibly have a
    // predecessor to join with; the first chunk always starts scanning
    // completely normally, exactly like a plain linear scan would.
    let is_first_chunk = chunk.offset == 0;

    // `block_start` is the absolute file offset of `buf[0]`; `filled` is
    // how much of `buf` currently holds data. Both are shared between the
    // leading-region loop below and the interior scan that follows, so the
    // interior scan can pick up in the *same* block the leading region
    // stopped in without re-reading it.
    let mut block_start = chunk.offset;
    let mut filled = min(block_cap as u64, chunk_end - block_start) as usize;
    read_exact_at(file, &mut buf[..filled], block_start)?;

    // Raw bytes of the deferred leading region, accumulated across as many
    // read blocks as the run happens to span, plus the position inside the
    // *current* block where it stopped (= where interior scanning resumes).
    let mut lead_bytes: Vec<u8> = Vec::new();
    let mut lead_end = 0usize;
    if !is_first_chunk {
        loop {
            let n = leading_run_len::<E>(&buf[..filled]);
            lead_bytes.extend_from_slice(&buf[..n]);
            if n < filled {
                // A byte that can't be part of any sequence ended the run;
                // everything past it is unambiguous.
                lead_end = n;
                break;
            }
            // The run ran to the end of this read block. Pull in the next
            // one (if the chunk has any left) and keep collecting.
            let next_start = block_start + filled as u64;
            if next_start >= chunk_end {
                lead_end = filled;
                break;
            }
            block_start = next_start;
            filled = min(block_cap as u64, chunk_end - block_start) as usize;
            read_exact_at(file, &mut buf[..filled], block_start)?;
        }
    }
    let lead_reaches_chunk_end = block_start + lead_end as u64 >= chunk_end;

    if !lead_bytes.is_empty() {
        let rec = MatchRecord {
            offset: chunk.offset,
            cb: lead_bytes.len() as u64,
            cch: 0,
            encoding: E::ENCODING,
            starts_at_chunk: true,
            ends_at_chunk: lead_reaches_chunk_end,
            data: RecordData::Raw(lead_bytes),
        };
        // Always written in practice (`starts_at_chunk` exempts it from
        // the `min_cch` drop), but counted from the return value anyway so
        // no call site can drift out of step with `emit_record`'s policy.
        if emit_record(&mut out, rec, cfg.min_cch())? {
            records += 1;
        }
    }

    if lead_reaches_chunk_end {
        // The leading run swallowed the whole chunk -- nothing left to
        // scan normally.
        return finish(out, records);
    }

    // --- Interior scanning, from just past the deferred leading region ---
    let mut run = Run::new();
    let mut carry: Vec<u8> = Vec::new();

    // Finish off whatever's left of the already-loaded block before
    // falling into the normal block-reading loop below for the rest of the
    // chunk.
    {
        let mut i = lead_end;
        while i < filled {
            match decode_step::<E>(&buf[i..filled]) {
                Step::Complete { len } => {
                    let abs = block_start + i as u64;
                    run.push(abs, &buf[i..i + len]);
                    i += len;
                }
                Step::Invalid => {
                    if run.close_as_text::<E>(&mut out, cfg.min_cch())? {
                        records += 1;
                    }
                    i += 1;
                }
                Step::Incomplete => break,
            }
        }
        if i < filled {
            carry.extend_from_slice(&buf[i..filled]);
        }
    }
    let mut pos = block_start + filled as u64;

    while pos < chunk_end {
        if cancelled.load(Ordering::Relaxed) {
            // Cooperative cancellation: stop without flushing any
            // in-progress run or attempting the trailing deferral,
            // matching scanner::ascii/scanner::utf8's behavior.
            return finish(out, records);
        }

        let carry_len = carry.len();
        buf[..carry_len].copy_from_slice(&carry);
        let want = min(block_cap - carry_len, (chunk_end - pos) as usize);
        read_exact_at(file, &mut buf[carry_len..carry_len + want], pos)?;
        let filled = carry_len + want;
        let block_start_abs = pos - carry_len as u64;
        pos += want as u64;
        carry.clear();

        let mut i = 0usize;
        while i < filled {
            match decode_step::<E>(&buf[i..filled]) {
                Step::Complete { len } => {
                    let abs = block_start_abs + i as u64;
                    run.push(abs, &buf[i..i + len]);
                    i += len;
                }
                Step::Invalid => {
                    if run.close_as_text::<E>(&mut out, cfg.min_cch())? {
                        records += 1;
                    }
                    i += 1;
                }
                Step::Incomplete => break,
            }
        }
        if i < filled {
            carry.extend_from_slice(&buf[i..filled]);
        }
    }

    // Whatever's left touches chunk_end (a dangling, not-yet-confirmed
    // lead byte in `carry`, and/or an already-accumulated run right before
    // it) and must be deferred as Raw -- see the doc comment above for why
    // this chunk never tries to resolve it itself.
    if !carry.is_empty() {
        let carry_start = chunk_end - carry.len() as u64;
        run.push(carry_start, &carry);
    }
    if let Some(rec) = run.into_raw_record::<E>(chunk.offset) {
        if emit_record(&mut out, rec, cfg.min_cch())? {
            records += 1;
        }
    }

    finish(out, records)
}

/// Decodes and character-segments a buffer of raw bytes that `outputter`
/// has determined it needs to resolve -- either because it found nothing
/// to join a boundary fragment with, or because it just joined one and
/// needs to know what the combined bytes actually say. See
/// `scanner::segment_raw`'s doc comment for the general contract.
///
/// This is a plain, unambiguous left-to-right scan: by the time this is
/// called, whatever cross-chunk join decision needed making has already
/// been made (by the caller), so `bytes` is just an ordinary, contiguous
/// span of the original file with no remaining boundary ambiguity --
/// exactly as if it were being scanned by `scan` itself, just from an
/// in-memory buffer instead of a live file.
pub(crate) fn segment_raw<E: Dbcs>(bytes: &[u8]) -> (Vec<ResolvedFragment>, Vec<u8>) {
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut run_start = 0usize;
    let mut in_run = false;

    while i < bytes.len() {
        match decode_step::<E>(&bytes[i..]) {
            Step::Complete { len } => {
                if !in_run {
                    run_start = i;
                    in_run = true;
                }
                i += len;
            }
            Step::Invalid => {
                if in_run {
                    push_fragment::<E>(&mut out, bytes, run_start, i);
                    in_run = false;
                }
                i += 1;
            }
            Step::Incomplete => {
                // `bytes` itself ends mid-character. This can legitimately
                // happen: e.g. a chain of alternating single/double-byte
                // characters can keep deferring across more than one chunk
                // boundary in a row (see
                // `outputter::resolve_for_output`'s doc comment) -- so
                // this is not an error, just "not fully resolved yet".
                if in_run {
                    push_fragment::<E>(&mut out, bytes, run_start, i);
                }
                return (out, bytes[i..].to_vec());
            }
        }
    }
    if in_run {
        push_fragment::<E>(&mut out, bytes, run_start, i);
    }
    (out, Vec::new())
}

fn push_fragment<E: Dbcs>(
    out: &mut Vec<ResolvedFragment>,
    bytes: &[u8],
    start: usize,
    end: usize,
) {
    let raw = &bytes[start..end];
    let cch = count_chars::<E>(raw);
    let (decoded, had_errors) = E::decoder().decode_without_bom_handling(raw);
    debug_assert!(
        !had_errors,
        "segment_raw resolved a byte sequence encoding_rs rejects for {:?}",
        E::ENCODING
    );
    out.push(ResolvedFragment {
        start: start as u64,
        cb: (end - start) as u64,
        cch,
        data: decoded.into_owned(),
    });
}
