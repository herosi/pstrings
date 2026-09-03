use super::{emit_record, read_exact_at, ResolvedFragment, READ_BUFFER_SIZE};
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

/// ESC (0x1B), the byte that introduces every ISO-2022-JP designation
/// sequence. Also re-exported for tests.
pub(crate) const ESC: u8 = 0x1b;

/// The active character set ("designation") of an ISO-2022-JP byte stream.
///
/// # Why this scanner tracks mode itself rather than deferring to `encoding_rs`
///
/// The previous implementation kept this same enum *and* fed every byte to
/// an `encoding_rs` decoder, treating the latter as authoritative and this
/// as mere bookkeeping for byte spans. That does not work, and the reason
/// is worth recording so it isn't reintroduced.
///
/// `encoding_rs`'s ISO-2022-JP decoder buffers internally. Feeding it one
/// byte at a time returns *no characters at all* -- measured directly on
/// `AB ESC $ B 日本 ESC ( B CD`, every single one of the fourteen
/// `decode_to_string_without_replacement` calls produced an empty string,
/// regardless of the `last` flag. The old scanner's `if decoded.is_empty()
/// { continue; }` branch therefore swallowed the entire input and
/// `append_char` was never reached even once, so no candidate was ever
/// built. The module could not find a single string.
///
/// Feeding it larger blocks instead would fix the emptiness but not the
/// real problem: this scanner needs to know, for each decoded character,
/// *which input bytes produced it*, because `MatchRecord::cb` is measured
/// in original-encoding bytes and `offset` must point at the character's
/// true file position. `encoding_rs` reports only how many bytes it
/// consumed in total per call, so recovering a per-character byte span
/// means re-deriving the character boundaries anyway -- i.e. writing
/// exactly the state machine below, but now with a second, independently
/// buffering component that has to be kept in lockstep with it. Two
/// parallel decoders that must agree is precisely the "second source of
/// truth that could drift" problem `scanner::dbcs` avoids.
///
/// ISO-2022-JP's structure makes the direct approach easy in any case:
/// it is a small, fully specified escape-driven state machine over
/// seven-bit bytes. The only part that genuinely needs a table -- whether
/// a given JIS X 0208 two-byte pair is actually *assigned* -- is still
/// delegated to `encoding_rs` (see `is_defined_jis_pair`), exactly as
/// `scanner::dbcs` delegates the same question via
/// `dbcs::is_defined_seq`. So there is one authority on which pairs
/// exist, and the state machine here only decides where the pairs are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// `ESC ( B` -- plain ASCII. Also the initial state of any stream.
    Ascii,
    /// `ESC ( J` -- JIS X 0201 Roman. ASCII except that 0x5C is ¥
    /// (U+00A5) and 0x7E is ‾ (U+203E).
    Roman,
    /// `ESC ( I` -- half-width katakana, 0x21..=0x5F mapping to
    /// U+FF61..=U+FF9F.
    Katakana,
    /// `ESC $ @` (JIS X 0208-1978) or `ESC $ B` (JIS X 0208-1983) --
    /// two bytes per character.
    Kanji,
}

/// Every escape sequence `encoding_rs`'s ISO-2022-JP decoder accepts,
/// paired with the mode it selects.
///
/// This list is deliberately *exactly* the accepted set, verified by
/// probing the decoder directly rather than copied from the standard:
/// `ESC ( H` (an obsolete alias for Roman), `ESC $ A` (GB2312) and
/// `ESC $ ( D` (JIS X 0212) are all **rejected** by `encoding_rs` and so
/// are rejected here too. Accepting a sequence the final decode step
/// would reject is the one way this table could cause trouble, so it errs
/// toward the decoder's actual behavior.
const ESCAPES: &[(&[u8], Mode)] = &[
    (b"\x1b(B", Mode::Ascii),
    (b"\x1b(J", Mode::Roman),
    (b"\x1b(I", Mode::Katakana),
    (b"\x1b$@", Mode::Kanji),
    (b"\x1b$B", Mode::Kanji),
];

/// The longest escape sequence in `ESCAPES`, and therefore the most bytes
/// that may need to be held back while deciding whether an escape is
/// valid.
const MAX_ESCAPE_LEN: usize = 3;

/// Whether a JIS X 0208 two-byte pair is actually assigned a character.
///
/// Both bytes being in the structural 0x21..=0x7E range is necessary but
/// not sufficient -- there are unassigned points in the code space (e.g.
/// `0x22 0x2F`, which is structurally fine but undefined). Rather than
/// hand-maintain a table of the assigned pairs, this asks the same
/// decoder that will ultimately produce the text, so "is this pair valid"
/// and "what character is it" can never disagree. This is the identical
/// arrangement `scanner::dbcs::is_defined_seq` uses, for the same
/// reason.
///
/// The synthetic `ESC $ B` prefix puts a fresh decoder into kanji mode so
/// the pair can be judged in isolation, with no dependence on any
/// surrounding stream state.
#[inline]
pub(crate) fn is_defined_jis_pair(b0: u8, b1: u8) -> bool {
    let (_, had_errors) =
        encoding_rs::ISO_2022_JP.decode_without_bom_handling(&[ESC, b'$', b'B', b0, b1]);
    !had_errors
}

/// Decodes one character in `mode` from the front of `bytes`, assuming
/// `bytes` contains no escape sequence at position 0 (callers check that
/// first).
///
/// Returns the decoded character and how many bytes it consumed, or
/// `Incomplete` when `bytes` is too short to decide, or `Invalid` when
/// these bytes cannot form a character in this mode.
pub(crate) fn decode_char(mode: Mode, bytes: &[u8]) -> Step {
    let Some(&b0) = bytes.first() else {
        return Step::Incomplete;
    };

    match mode {
        Mode::Ascii => {
            // Tab is admitted alongside the printable range for the same
            // reason `filter::ascii` admits it: it occurs in genuine text
            // and does not break the crate's line-oriented output.
            if b0 == b'\t' || (0x20..=0x7e).contains(&b0) {
                Step::Complete { ch: b0 as char, len: 1 }
            } else {
                Step::Invalid
            }
        }

        Mode::Roman => {
            // JIS X 0201 Roman is ASCII with two substitutions, both
            // confirmed against `encoding_rs`.
            match b0 {
                0x5c => Step::Complete { ch: '\u{00a5}', len: 1 },
                0x7e => Step::Complete { ch: '\u{203e}', len: 1 },
                b'\t' => Step::Complete { ch: '\t', len: 1 },
                0x20..=0x7e => Step::Complete { ch: b0 as char, len: 1 },
                _ => Step::Invalid,
            }
        }

        Mode::Katakana => {
            // 0x21..=0x5F maps linearly onto U+FF61..=U+FF9F. 0x60 and
            // 0x20 are both errors, verified against `encoding_rs`.
            if (0x21..=0x5f).contains(&b0) {
                let cp = 0xff61 + (b0 - 0x21) as u32;
                match char::from_u32(cp) {
                    Some(ch) => Step::Complete { ch, len: 1 },
                    None => Step::Invalid,
                }
            } else {
                Step::Invalid
            }
        }

        Mode::Kanji => {
            if !(0x21..=0x7e).contains(&b0) {
                return Step::Invalid;
            }
            let Some(&b1) = bytes.get(1) else {
                return Step::Incomplete;
            };
            if !(0x21..=0x7e).contains(&b1) || !is_defined_jis_pair(b0, b1) {
                return Step::Invalid;
            }
            // The pair is assigned, so this decode cannot fail; ask
            // `encoding_rs` for the actual character.
            let seq = [ESC, b'$', b'B', b0, b1];
            let (text, _) = encoding_rs::ISO_2022_JP.decode_without_bom_handling(&seq);
            match text.chars().next() {
                Some(ch) => Step::Complete { ch, len: 2 },
                None => Step::Invalid,
            }
        }
    }
}

/// The outcome of trying to read one character from the front of a slice.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// A complete character was decoded, consuming `len` bytes.
    Complete { ch: char, len: usize },
    /// The slice ran out mid-character (or mid-escape-sequence); more
    /// bytes are needed before a verdict is possible.
    Incomplete,
    /// These bytes cannot form a character in the current mode. The
    /// caller ends any in-progress run and resyncs forward one byte.
    Invalid,
}

/// Tries to read an escape sequence from the front of `bytes`.
///
/// Returns `Complete` with the selected mode when `bytes` starts with a
/// recognized sequence, `Incomplete` when it starts with a *prefix* of one
/// but is too short to tell, and `Invalid` when it starts with ESC
/// followed by something no sequence allows.
pub(crate) fn read_escape(bytes: &[u8]) -> EscapeStep {
    debug_assert_eq!(bytes.first(), Some(&ESC), "read_escape called without ESC");

    for &(seq, mode) in ESCAPES {
        if bytes.len() >= seq.len() {
            if &bytes[..seq.len()] == seq {
                return EscapeStep::Complete { mode, len: seq.len() };
            }
        } else if seq.starts_with(bytes) {
            // `bytes` is a proper prefix of this sequence, so it might
            // still turn into it once more bytes arrive.
            return EscapeStep::Incomplete;
        }
    }
    EscapeStep::Invalid
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EscapeStep {
    Complete { mode: Mode, len: usize },
    Incomplete,
    Invalid,
}

/// Whether `b` could appear anywhere inside an ISO-2022-JP run -- as ESC,
/// as an escape-sequence body byte, or as character data in any mode.
///
/// ISO-2022-JP is a seven-bit encoding: every byte of every mode's
/// character range, and every byte of every escape sequence, falls in
/// 0x20..=0x7E, plus ESC itself and tab. A byte outside that set (any
/// other control byte, 0x7F, or anything >= 0x80) cannot be part of a run
/// under *any* reading, which is exactly the property
/// `leading_run_len` needs to bound the deferred region -- see its doc
/// comment.
#[inline]
pub(crate) fn is_iso2022jp_byte(b: u8) -> bool {
    b == ESC || b == b'\t' || (0x20..=0x7e).contains(&b)
}

/// Whether a decoded character may appear in a run.
///
/// ASCII-range characters go through `filter::is_ascii_char`; anything
/// wider is admitted unless it would corrupt the crate's line-oriented
/// output. Neither path consults `cfg.filter()`: like `scanner::utf8` and
/// `scanner::cp932`, this scanner validates structurally (via the escape
/// state machine and the assigned-pair lookup) and so has no
/// false-positive problem for `--filter` to solve. See the "Which scanners
/// this actually affects" section on `filter::CharacterFilter`.
#[inline]
fn char_allowed(ch: char) -> bool {
    if (ch as u32) <= 0x7f {
        filter::is_ascii_char(ch)
    } else {
        !ch.is_control() && ch != '\u{2028}' && ch != '\u{2029}'
    }
}

/// How many bytes at the very start of a chunk must be deferred as an
/// unsegmented raw prefix.
///
/// The answer is "every leading byte that could belong to a run at all"
/// (`is_iso2022jp_byte`), which is the same rule -- and holds for the same
/// two reasons -- as `scanner::dbcs::leading_run_len`:
///
///   - *Sufficient*: a byte failing `is_iso2022jp_byte` cannot be part of
///     any run under any reading, so no run can extend across it and the
///     byte after it is an unambiguously fresh start.
///
///   - *Necessary*: stopping earlier would split one continuous run into a
///     `Raw` prefix record plus a separate `Text` record starting
///     mid-run. `outputter` can only join a pending fragment with a record
///     sitting exactly at the chunk's start, and `record::append_data`
///     refuses to mix `Raw` with `Text`, so the two pieces could never be
///     stitched back together.
fn leading_run_len(data: &[u8]) -> usize {
    data.iter().take_while(|&&b| is_iso2022jp_byte(b)).count()
}

/// Bookkeeping for the run currently being accumulated.
///
/// Holds the run's original ISO-2022-JP bytes rather than decoded text,
/// so that a run touching a chunk boundary can be handed off undecoded
/// (`into_raw_record`) and a run that closes inside the chunk can be
/// decoded in one shot (`close_as_text`). Keeping only the raw bytes also
/// means there is exactly one place -- `segment_raw` -- where bytes turn
/// into characters, whichever path a run takes.
struct Run {
    /// The run's own bytes, always starting from ASCII mode (see
    /// `Scanner::mode`'s doc comment on run-local mode semantics).
    data: Vec<u8>,
    offset: u64,
    cch: u64,
}

impl Run {
    fn new() -> Self {
        Self { data: Vec::new(), offset: 0, cch: 0 }
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Appends bytes that produce no character of their own (an escape
    /// sequence) to an already-open run. Callers must not use this to
    /// *start* a run: a run always begins at a character, never at an
    /// escape (see `Scanner::esc_buf`).
    fn push_bytes(&mut self, bytes: &[u8]) {
        debug_assert!(!self.data.is_empty(), "push_bytes would start a run with an escape");
        self.data.extend_from_slice(bytes);
    }

    /// Appends one decoded character's original bytes. `abs` is the
    /// absolute file offset those bytes begin at, and `pending_esc` holds
    /// any escape sequence(s) immediately preceding them that have been
    /// held back until it was known whether a character would follow.
    fn push_char(&mut self, abs: u64, pending_esc: &[u8], char_bytes: &[u8]) {
        if self.data.is_empty() {
            // The run starts at the escape sequence, not the character,
            // so that the recorded byte span is self-contained: decoding
            // `data` from a fresh ASCII state reproduces this run exactly.
            self.offset = abs - pending_esc.len() as u64;
        }
        self.data.extend_from_slice(pending_esc);
        self.data.extend_from_slice(char_bytes);
        self.cch += 1;
    }

    /// Closes a run that does not touch the chunk end, writing it as
    /// fully decoded text. Returns whether a record was actually written
    /// (`emit_record` drops runs below `min_cch`).
    fn close_as_text(&mut self, out: &mut BufWriter<File>, min_cch: u64) -> io::Result<bool> {
        if self.data.is_empty() {
            return Ok(false);
        }
        let bytes = std::mem::take(&mut self.data);
        let cch = std::mem::take(&mut self.cch);
        let (fragments, tail) = segment_raw(&bytes);
        debug_assert!(
            tail.is_empty() && fragments.len() == 1,
            "an interior run should resolve to exactly one fragment, got {} + {} tail bytes",
            fragments.len(),
            tail.len()
        );
        let Some(fragment) = fragments.into_iter().next() else {
            return Ok(false);
        };
        debug_assert_eq!(fragment.cch, cch, "run character count disagrees with segment_raw");
        let rec = MatchRecord {
            offset: self.offset,
            cb: fragment.cb,
            cch: fragment.cch,
            encoding: InputEncoding::Iso2022Jp,
            starts_at_chunk: false,
            ends_at_chunk: false,
            data: RecordData::Text(fragment.data),
        };
        emit_record(out, rec, min_cch)
    }

    /// Converts a run touching the chunk end into a deferred `Raw`
    /// record. Returns `None` if nothing was accumulated.
    fn into_raw_record(self, chunk_offset: u64) -> Option<MatchRecord> {
        if self.data.is_empty() {
            return None;
        }
        Some(MatchRecord {
            offset: self.offset,
            cb: self.data.len() as u64,
            // Segmentation hasn't happened yet, so `cch` is the documented
            // placeholder until `outputter` resolves this via `segment_raw`.
            cch: 0,
            encoding: InputEncoding::Iso2022Jp,
            starts_at_chunk: self.offset == chunk_offset,
            ends_at_chunk: true,
            data: RecordData::Raw(self.data),
        })
    }
}

/// Scans one chunk for runs of printable ISO-2022-JP text.
///
/// # Run-local mode semantics
///
/// A run always begins in ASCII mode, and only escape sequences *inside*
/// that run change its mode. Mode does not persist across a run boundary:
/// when a run ends (at a byte that can't continue it), the next run starts
/// from ASCII again.
///
/// This is a deliberate design decision, and it is what makes the scanner
/// correct. The alternative -- treating mode as a property of the whole
/// file, so that an `ESC $ B` thousands of bytes back still governs the
/// bytes here -- forces every chunk to know the mode inherited at its
/// start. The previous implementation did exactly that, scanning
/// *backward* from the chunk start in 64 KiB windows, potentially all the
/// way to the beginning of the file, hunting for the most recent ESC and
/// replaying from there. That is unbounded work per chunk, it defeats the
/// parallelism the chunking exists to provide, and it makes a chunk's
/// result depend on arbitrarily distant bytes.
///
/// Run-local mode makes every run's meaning depend only on the run's own
/// bytes. That in turn is what lets a run be recorded as a self-contained
/// byte span (see `Run::push_char`, which includes a leading escape in the
/// run) and decoded later, in isolation, by `segment_raw` -- and it is why
/// results are independent of `--chunk-size`.
///
/// The practical cost is nil for the data this tool looks for. An
/// ISO-2022-JP string embedded in a binary carries its own designation
/// escape, because the standard requires a stream to return to ASCII
/// before it ends and any self-contained string must therefore establish
/// its own mode. A run of raw JIS bytes with its `ESC $ B` sitting on the
/// far side of unrelated binary data isn't a string this tool can
/// meaningfully attribute to ISO-2022-JP in the first place.
///
/// # Chunk boundaries
///
/// ISO-2022-JP is not self-synchronizing in the sense
/// `record::RecordData` means: a byte at a chunk's start may be the trail
/// byte of a kanji pair the previous chunk began, a byte in the middle of
/// a three-byte escape sequence, or an ordinary fresh character -- and
/// which it is depends on bytes the previous chunk holds. So this scanner
/// takes exactly the approach `scanner::dbcs` documents: boundary-
/// touching runs are deferred as undecoded `RecordData::Raw` and resolved
/// by `outputter` (via `scanner::segment_raw`) once it knows what they
/// join with. Everything strictly between the leading and trailing
/// boundary regions is decoded immediately.
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

    let finish = |mut out: BufWriter<File>, records: u64| -> io::Result<(u64, File)> {
        out.flush()?;
        let mut f = out.into_inner().map_err(|e| e.into_error())?;
        f.seek(io::SeekFrom::Start(0))?;
        Ok((records, f))
    };

    if chunk.offset >= chunk_end {
        return finish(out, 0);
    }

    // A block must be able to hold the longest single indivisible unit
    // (an escape sequence) several times over so progress is always
    // possible.
    let block_cap = READ_BUFFER_SIZE.max(MAX_ESCAPE_LEN * 4);
    let mut buf = vec![0u8; block_cap];

    // Only chunks after the first can have a predecessor to join with.
    let is_first_chunk = chunk.offset == 0;

    let mut block_start = chunk.offset;
    let mut filled = min(block_cap as u64, chunk_end - block_start) as usize;
    read_exact_at(file, &mut buf[..filled], block_start)?;

    // --- Leading boundary region: deferred, undecoded ---
    let mut lead_bytes: Vec<u8> = Vec::new();
    let mut lead_end = 0usize;
    if !is_first_chunk {
        loop {
            let n = leading_run_len(&buf[..filled]);
            lead_bytes.extend_from_slice(&buf[..n]);
            if n < filled {
                lead_end = n;
                break;
            }
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
            encoding: InputEncoding::Iso2022Jp,
            starts_at_chunk: true,
            ends_at_chunk: lead_reaches_chunk_end,
            data: RecordData::Raw(lead_bytes),
        };
        if emit_record(&mut out, rec, cfg.min_cch())? {
            records += 1;
        }
    }

    if lead_reaches_chunk_end {
        return finish(out, records);
    }

    // --- Interior scanning ---
    let mut st = Scanner::new();
    let mut carry: Vec<u8> = Vec::new();

    // Finish the already-loaded block before entering the read loop.
    let leftover = st.consume(&buf[lead_end..filled], block_start + lead_end as u64, &mut out, cfg.min_cch(), &mut records)?;
    carry.extend_from_slice(leftover);

    let mut pos = block_start + filled as u64;

    while pos < chunk_end {
        if cancelled.load(Ordering::Relaxed) {
            // Cooperative cancellation: stop without flushing anything in
            // progress, matching the other scanners.
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

        let leftover = st.consume(&buf[..filled], block_start_abs, &mut out, cfg.min_cch(), &mut records)?;
        carry.extend_from_slice(leftover);
    }

    // --- Trailing boundary region: deferred, undecoded ---
    //
    // Whatever is left touches chunk_end: an in-progress run, escape
    // bytes held back pending a character, and/or an undecidable partial
    // character. All of it goes into one `Raw` record, in file order.
    st.flush_into_run_for_deferral(chunk_end - (st.esc_buf.len() + carry.len()) as u64, &carry);

    if let Some(rec) = st.run.into_raw_record(chunk.offset) {
        if emit_record(&mut out, rec, cfg.min_cch())? {
            records += 1;
        }
    }

    finish(out, records)
}

/// The interior-scanning state machine, shared by `scan` (which drives it
/// block by block over a chunk) and `segment_raw` (which drives it once
/// over an already-resolved buffer).
struct Scanner {
    run: Run,
    /// The run's current mode. Reset to `Ascii` every time a run closes --
    /// see `scan`'s "Run-local mode semantics" section.
    mode: Mode,
    /// Escape sequences read but not yet attributed to a run, because it
    /// isn't yet known whether a character follows them.
    ///
    /// This is what keeps a *trailing* escape out of a run's byte span. A
    /// string like `日本 ESC ( B` ends with a designation sequence that
    /// belongs to the stream, not to the string: including it would
    /// inflate `cb` by three bytes past the last character and make the
    /// record claim territory it doesn't occupy. Escapes are therefore
    /// buffered here and folded into the run only when a character
    /// actually arrives (`Run::push_char`), so interior escapes are
    /// counted and trailing ones are not.
    esc_buf: Vec<u8>,
    /// Absolute file offset of `esc_buf[0]`, used to place a run that
    /// starts with an escape.
    esc_offset: u64,
}

impl Scanner {
    fn new() -> Self {
        Self { run: Run::new(), mode: Mode::Ascii, esc_buf: Vec::new(), esc_offset: 0 }
    }

    /// Ends the current run at a byte that cannot continue it.
    ///
    /// Buffered escapes are discarded rather than emitted: with no
    /// character following them they designate nothing, and a run always
    /// begins at a character.
    fn break_run(&mut self, out: &mut BufWriter<File>, min_cch: u64, records: &mut u64) -> io::Result<()> {
        if self.run.close_as_text(out, min_cch)? {
            *records += 1;
        }
        self.mode = Mode::Ascii;
        self.esc_buf.clear();
        Ok(())
    }

    /// Consumes as much of `bytes` as can be decided, returning the
    /// undecidable tail (a partial escape sequence or a partial kanji
    /// pair) for the caller to carry into the next block.
    ///
    /// `base` is the absolute file offset of `bytes[0]`.
    fn consume<'a>(
        &mut self,
        bytes: &'a [u8],
        base: u64,
        out: &mut BufWriter<File>,
        min_cch: u64,
        records: &mut u64,
    ) -> io::Result<&'a [u8]> {
        let mut i = 0usize;
        while i < bytes.len() {
            let abs = base + i as u64;

            if bytes[i] == ESC {
                match read_escape(&bytes[i..]) {
                    EscapeStep::Complete { mode, len } => {
                        if self.esc_buf.is_empty() {
                            self.esc_offset = abs;
                        }
                        self.esc_buf.extend_from_slice(&bytes[i..i + len]);
                        self.mode = mode;
                        i += len;
                    }
                    EscapeStep::Incomplete => return Ok(&bytes[i..]),
                    EscapeStep::Invalid => {
                        self.break_run(out, min_cch, records)?;
                        i += 1;
                    }
                }
                continue;
            }

            match decode_char(self.mode, &bytes[i..]) {
                Step::Complete { ch, len } => {
                    if char_allowed(ch) {
                        // A run that starts here starts at the buffered
                        // escape, if any, so its byte span stays
                        // self-contained.
                        let esc = std::mem::take(&mut self.esc_buf);
                        if self.run.is_empty() {
                            self.run.push_char(abs, &esc, &bytes[i..i + len]);
                        } else {
                            self.run.push_bytes(&esc);
                            self.run.push_char(abs, &[], &bytes[i..i + len]);
                        }
                    } else {
                        self.break_run(out, min_cch, records)?;
                    }
                    i += len;
                }
                Step::Incomplete => return Ok(&bytes[i..]),
                Step::Invalid => {
                    self.break_run(out, min_cch, records)?;
                    i += 1;
                }
            }
        }
        Ok(&bytes[bytes.len()..])
    }

    /// Folds any held-back escape bytes and the undecidable tail into the
    /// run so the whole trailing boundary region can be deferred as one
    /// `Raw` record.
    ///
    /// `tail_start` is the absolute offset the combined
    /// `esc_buf + tail` region begins at, needed in case the run is empty
    /// and these bytes are all there is.
    fn flush_into_run_for_deferral(&mut self, tail_start: u64, tail: &[u8]) {
        if self.esc_buf.is_empty() && tail.is_empty() {
            return;
        }
        if self.run.is_empty() {
            self.run.offset = if self.esc_buf.is_empty() { tail_start } else { self.esc_offset };
        }
        let esc = std::mem::take(&mut self.esc_buf);
        self.run.data.extend_from_slice(&esc);
        self.run.data.extend_from_slice(tail);
    }
}

/// Decodes and character-segments a buffer of raw ISO-2022-JP bytes that
/// `outputter` has determined it needs to resolve. See
/// `scanner::segment_raw`'s doc comment for the general contract.
///
/// By the time this runs, every cross-chunk join decision has already been
/// made, so `bytes` is an ordinary contiguous span of the original file
/// and can be scanned left to right with no remaining ambiguity -- and,
/// thanks to run-local mode semantics, with no dependence on anything
/// outside the buffer. It starts in ASCII mode exactly as `scan` does.
pub(crate) fn segment_raw(bytes: &[u8]) -> (Vec<ResolvedFragment>, Vec<u8>) {
    let mut out: Vec<ResolvedFragment> = Vec::new();
    let mut st = Segmenter::default();

    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == ESC {
            match read_escape(&bytes[i..]) {
                EscapeStep::Complete { mode, len } => {
                    st.note_escape(i, len, mode);
                    i += len;
                }
                EscapeStep::Incomplete => {
                    // The buffer ends mid-escape. Close what's already
                    // complete and hand the partial sequence back as the
                    // tail for the caller to carry forward.
                    let tail_from = st.tail_start(i);
                    st.close_run(&mut out);
                    return (out, bytes[tail_from..].to_vec());
                }
                EscapeStep::Invalid => {
                    st.close_run(&mut out);
                    i += 1;
                }
            }
            continue;
        }

        match decode_char(st.mode, &bytes[i..]) {
            Step::Complete { ch, len } => {
                if char_allowed(ch) {
                    st.push_char(i, len, ch);
                } else {
                    st.close_run(&mut out);
                }
                i += len;
            }
            Step::Incomplete => {
                let tail_from = st.tail_start(i);
                st.close_run(&mut out);
                return (out, bytes[tail_from..].to_vec());
            }
            Step::Invalid => {
                st.close_run(&mut out);
                i += 1;
            }
        }
    }

    st.close_run(&mut out);
    (out, Vec::new())
}

/// The run-building state used by `segment_raw`.
///
/// This mirrors `Scanner` exactly -- same run-local mode semantics, same
/// rule that a leading escape joins the run while a trailing one does not
/// -- but works over a fully-resolved in-memory buffer and produces
/// `ResolvedFragment`s instead of writing records. Keeping the two in step
/// matters, so `Run::close_as_text` routes its own interior runs through
/// `segment_raw` rather than decoding them separately: there is one
/// bytes-to-characters implementation, used by both paths.
#[derive(Default)]
struct Segmenter {
    mode: ModeState,
    /// Byte offset and length of escape sequence(s) seen but not yet
    /// attributed to a run.
    esc_start: usize,
    esc_len: usize,
    run_start: usize,
    run_end: usize,
    run_cch: u64,
    run_text: String,
    in_run: bool,
}

/// `Mode` needs a `Default` for `Segmenter`; a stream always begins in
/// ASCII.
type ModeState = Mode;

impl Default for Mode {
    fn default() -> Self {
        Mode::Ascii
    }
}

impl Segmenter {
    fn note_escape(&mut self, at: usize, len: usize, mode: Mode) {
        if self.esc_len == 0 {
            self.esc_start = at;
        }
        self.esc_len += len;
        self.mode = mode;
    }

    fn push_char(&mut self, at: usize, len: usize, ch: char) {
        if !self.in_run {
            // The run starts at the pending escape, if there is one, so
            // its byte span is self-contained -- matching
            // `Run::push_char`.
            self.run_start = self.tail_start(at);
            self.in_run = true;
        }
        self.esc_len = 0;
        self.run_text.push(ch);
        self.run_cch += 1;
        self.run_end = at + len;
    }

    /// Where an undecidable region starting at `at` really begins: at the
    /// held-back escape, if one is pending, otherwise at `at` itself.
    fn tail_start(&self, at: usize) -> usize {
        if self.esc_len > 0 { self.esc_start } else { at }
    }

    fn close_run(&mut self, out: &mut Vec<ResolvedFragment>) {
        if self.in_run {
            out.push(ResolvedFragment {
                start: self.run_start as u64,
                cb: (self.run_end - self.run_start) as u64,
                cch: self.run_cch,
                data: std::mem::take(&mut self.run_text),
            });
            self.in_run = false;
            self.run_cch = 0;
        }
        // Mode is a property of a run, not of the buffer.
        self.mode = Mode::Ascii;
        self.esc_len = 0;
    }
}
