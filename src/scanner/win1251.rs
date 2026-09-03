//! windows-1251 (Cyrillic) scanner.
//!
//! # Why this is a separate module rather than a filter on `scanner::ascii`
//!
//! At a glance windows-1251 looks like it should need no scanner at all:
//! it is single-byte, so `scanner::ascii` already walks the file one byte
//! at a time deciding "does this byte pass the filter", and adding a
//! `cyrillic` filter looks like it would be enough.
//!
//! It is not, and the reason is one line in `scanner::ascii`:
//!
//! ```text
//! run_data.push(b as char);
//! ```
//!
//! That maps byte N to U+00N, which is the ISO-8859-1 table -- hardcoded,
//! and correct only for ISO-8859-1 (and, over 0xA0-0xFF, windows-1252,
//! which agrees with it there). `scanner::ascii` has no notion of a
//! codepage; its filters decide *whether* a byte is text, never *which
//! character* it is.
//!
//! For windows-1251 that mapping is wrong for 81 of the 96 bytes in
//! 0xA0-0xFF. Byte 0xC0 is U+0410 CYRILLIC CAPITAL LETTER A, not U+00C0
//! LATIN CAPITAL LETTER A WITH GRAVE. So bolting a `cyrillic` filter onto
//! `scanner::ascii` would produce the worst possible outcome: it would
//! *match* Cyrillic text and then emit Latin letters -- i.e. the tool
//! would detect "Привет" and print "Ïðèâåò". Silently wrong output is
//! worse than no output, so the byte-to-character table has to live
//! somewhere that knows which codepage is in play. That is here.
//!
//! # Consequences of having a real table
//!
//! Because this scanner decodes properly, two things follow that
//! distinguish it from `scanner::ascii`:
//!
//!  * It filters on the decoded `char` (`FilterSet::allows_char`), not on
//!    the raw byte (`allows_u8`). This is what lets `--filter cyrillic`
//!    work at all: a
//!    byte-oriented filter cannot answer "is this Cyrillic?" without
//!    knowing the codepage, whereas a character-oriented one needs no such
//!    knowledge. (`scanner::utf8` also decodes, but does not consult
//!    `--filter` at all -- see below.)
//!  * The 0x80-0x9F range is usable. `filter::latin1` deliberately stops
//!    at 0xA0 because in ISO-8859-1 that range is C1 controls, but in
//!    windows-1251 31 of those 32 bytes are printable (Cyrillic letters
//!    Ђ Ѓ ѓ Љ Њ Ќ Ћ Џ ђ љ њ ќ ћ џ, typographic quotes, dashes, €, ‰, ™).
//!    Only 0x98 is unassigned. In practice only the Cyrillic ones are
//!    reachable, since no filter currently defined admits the punctuation
//!    and symbols there.
//!
//! # Why `--filter` applies here at all
//!
//! Unlike the double-byte scanners (`scanner::dbcs`) and `scanner::utf8`,
//! this encoding performs *no* structural validation -- there is no such
//! thing as an invalid windows-1251 byte sequence, because every byte is
//! independently a character. That puts it in the same category as
//! `scanner::ascii` and `scanner::utf16le`: the only thing standing
//! between the user and a flood of binary noise is the character-class
//! filter. Measured against uniform random data, 223 of 256 bytes are
//! printable, so with no filtering at all this scanner would report
//! essentially every non-control byte in the file.
//!
//! The default (`ascii` alone) therefore behaves exactly like
//! `scanner::ascii`, and `--filter ascii,cyrillic` is the combination that
//! actually finds Russian text. `ascii` is kept in that pair on purpose:
//! real Cyrillic documents are full of ASCII digits, punctuation and Latin
//! fragments, and dropping it fragments every sentence at the first comma.
//!
//! # Structure
//!
//! Deliberately written as a self-contained scanner rather than as a
//! generic single-byte engine. windows-1251 is currently the only
//! table-driven single-byte encoding here; if others follow (KOI8-R,
//! ISO-8859-5, windows-1252's full range), the right move is to lift this
//! into a `scanner::sbcs` parameterised by the 256-entry table, exactly as
//! `scanner::dbcs` was lifted out of `scanner::cp932` once GBK and EUC-KR
//! needed the same algorithm. Until then a second abstraction layer would
//! cost more than it saves.

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

/// The windows-1251 byte-to-Unicode table.
///
/// Generated from `encoding_rs::WINDOWS_1251` rather than transcribed by
/// hand -- a 256-entry table is exactly the kind of thing that acquires a
/// silent typo -- and pinned by `table_matches_encoding_rs` in
/// `src/tests/scanner_win1251_tests.rs`, which re-derives it and compares.
///
/// Held as a table rather than calling `encoding_rs` per byte because this
/// is the innermost loop of a scan over potentially gigabytes: a 1 KiB
/// array lookup is a single L1 hit, whereas a decoder call per byte is
/// several orders of magnitude more expensive.
///
/// 0x98 is the one unassigned byte; `encoding_rs` maps it to U+0098, a C1
/// control, which `is_text` then rejects. That is the desired behaviour,
/// so it needs no special case.
#[rustfmt::skip]
pub(crate) const TABLE: [char; 256] = [
    '\u{0000}', '\u{0001}', '\u{0002}', '\u{0003}', '\u{0004}', '\u{0005}', '\u{0006}', '\u{0007}',
    '\u{0008}', '\u{0009}', '\u{000a}', '\u{000b}', '\u{000c}', '\u{000d}', '\u{000e}', '\u{000f}',
    '\u{0010}', '\u{0011}', '\u{0012}', '\u{0013}', '\u{0014}', '\u{0015}', '\u{0016}', '\u{0017}',
    '\u{0018}', '\u{0019}', '\u{001a}', '\u{001b}', '\u{001c}', '\u{001d}', '\u{001e}', '\u{001f}',
    '\u{0020}', '\u{0021}', '\u{0022}', '\u{0023}', '\u{0024}', '\u{0025}', '\u{0026}', '\u{0027}',
    '\u{0028}', '\u{0029}', '\u{002a}', '\u{002b}', '\u{002c}', '\u{002d}', '\u{002e}', '\u{002f}',
    '\u{0030}', '\u{0031}', '\u{0032}', '\u{0033}', '\u{0034}', '\u{0035}', '\u{0036}', '\u{0037}',
    '\u{0038}', '\u{0039}', '\u{003a}', '\u{003b}', '\u{003c}', '\u{003d}', '\u{003e}', '\u{003f}',
    '\u{0040}', '\u{0041}', '\u{0042}', '\u{0043}', '\u{0044}', '\u{0045}', '\u{0046}', '\u{0047}',
    '\u{0048}', '\u{0049}', '\u{004a}', '\u{004b}', '\u{004c}', '\u{004d}', '\u{004e}', '\u{004f}',
    '\u{0050}', '\u{0051}', '\u{0052}', '\u{0053}', '\u{0054}', '\u{0055}', '\u{0056}', '\u{0057}',
    '\u{0058}', '\u{0059}', '\u{005a}', '\u{005b}', '\u{005c}', '\u{005d}', '\u{005e}', '\u{005f}',
    '\u{0060}', '\u{0061}', '\u{0062}', '\u{0063}', '\u{0064}', '\u{0065}', '\u{0066}', '\u{0067}',
    '\u{0068}', '\u{0069}', '\u{006a}', '\u{006b}', '\u{006c}', '\u{006d}', '\u{006e}', '\u{006f}',
    '\u{0070}', '\u{0071}', '\u{0072}', '\u{0073}', '\u{0074}', '\u{0075}', '\u{0076}', '\u{0077}',
    '\u{0078}', '\u{0079}', '\u{007a}', '\u{007b}', '\u{007c}', '\u{007d}', '\u{007e}', '\u{007f}',
    '\u{0402}', '\u{0403}', '\u{201a}', '\u{0453}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{20ac}', '\u{2030}', '\u{0409}', '\u{2039}', '\u{040a}', '\u{040c}', '\u{040b}', '\u{040f}',
    '\u{0452}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{0098}', '\u{2122}', '\u{0459}', '\u{203a}', '\u{045a}', '\u{045c}', '\u{045b}', '\u{045f}',
    '\u{00a0}', '\u{040e}', '\u{045e}', '\u{0408}', '\u{00a4}', '\u{0490}', '\u{00a6}', '\u{00a7}',
    '\u{0401}', '\u{00a9}', '\u{0404}', '\u{00ab}', '\u{00ac}', '\u{00ad}', '\u{00ae}', '\u{0407}',
    '\u{00b0}', '\u{00b1}', '\u{0406}', '\u{0456}', '\u{0491}', '\u{00b5}', '\u{00b6}', '\u{00b7}',
    '\u{0451}', '\u{2116}', '\u{0454}', '\u{00bb}', '\u{0458}', '\u{0405}', '\u{0455}', '\u{0457}',
    '\u{0410}', '\u{0411}', '\u{0412}', '\u{0413}', '\u{0414}', '\u{0415}', '\u{0416}', '\u{0417}',
    '\u{0418}', '\u{0419}', '\u{041a}', '\u{041b}', '\u{041c}', '\u{041d}', '\u{041e}', '\u{041f}',
    '\u{0420}', '\u{0421}', '\u{0422}', '\u{0423}', '\u{0424}', '\u{0425}', '\u{0426}', '\u{0427}',
    '\u{0428}', '\u{0429}', '\u{042a}', '\u{042b}', '\u{042c}', '\u{042d}', '\u{042e}', '\u{042f}',
    '\u{0430}', '\u{0431}', '\u{0432}', '\u{0433}', '\u{0434}', '\u{0435}', '\u{0436}', '\u{0437}',
    '\u{0438}', '\u{0439}', '\u{043a}', '\u{043b}', '\u{043c}', '\u{043d}', '\u{043e}', '\u{043f}',
    '\u{0440}', '\u{0441}', '\u{0442}', '\u{0443}', '\u{0444}', '\u{0445}', '\u{0446}', '\u{0447}',
    '\u{0448}', '\u{0449}', '\u{044a}', '\u{044b}', '\u{044c}', '\u{044d}', '\u{044e}', '\u{044f}',
];

/// Decodes one byte. Total: every byte is a character in this encoding,
/// which is precisely why it needs `--filter` (see the module doc).
#[inline(always)]
pub(crate) fn decode_byte(b: u8) -> char {
    TABLE[b as usize]
}

/// Whether a decoded character may belong to a run.
///
/// Two conditions, and the split between them is deliberate:
///
///  * The character must pass the user's `--filter` selection. This is the
///    knob that makes the scanner usable at all.
///  * It must not be a control character. This is enforced *regardless* of
///    the filter, because control characters would corrupt the
///    line-oriented output format -- a stray CR or LF in a match would
///    split one record across two output lines. Every filter currently
///    defined happens to exclude controls already, so this guard is
///    redundant today; it is kept so that the output format stays
///    well-formed no matter what filter is added later, rather than
///    resting on a property each filter must remember to preserve.
///
/// Tab is the one control character allowed through, matching
/// `filter::ascii`, and only when the filter admits it.
#[inline(always)]
fn is_text(cfg: &Config, ch: char) -> bool {
    if !cfg.filter().allows_char(ch) {
        return false;
    }
    ch == '\t' || !ch.is_control()
}

/// Scans one chunk for runs of windows-1251 text.
///
/// Structurally identical to `scanner::ascii`: a single linear pass, runs
/// accumulated until a non-text character or the end of the chunk closes
/// them, records emitted in offset order so no downstream sort is needed.
///
/// The encoding is self-synchronizing in the sense
/// `InputEncoding::is_self_synchronizing` means it -- one byte is always
/// exactly one character, so a chunk boundary can never fall inside a
/// character. Boundary fragments are therefore emitted as decoded
/// `RecordData::Text` and simply concatenated by the merger; there is no
/// `segment_raw` arm for this encoding and none is needed.
pub(crate) fn scan(
    file: &File,
    chunk: &Chunk,
    cfg: &Config,
    temp_path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<(u64, File)> {
    let temp_file = create_temp_file(temp_path, cfg.keep_temp())?;
    let mut out = BufWriter::with_capacity(crate::WRITE_BUFFER_SIZE, temp_file);
    let mut buf = vec![0u8; min(READ_BUFFER_SIZE, chunk.len.max(1) as usize)];

    // Accumulated as decoded characters, so `run_data` is already valid
    // UTF-8 and needs no validation step. Note `cb` and `cch` genuinely
    // differ for this encoding once the run leaves ASCII: one source byte
    // becomes a two-byte UTF-8 sequence for Cyrillic, so `cb` (source
    // bytes) is not `run_data.len()` (output bytes). Both are tracked
    // explicitly for that reason.
    let mut run_data = String::with_capacity(64);
    let mut run_offset = 0u64;
    let mut run_cb = 0u64;
    let mut run_cch = 0u64;
    let mut run_started = false;
    let mut pos = 0u64;
    let mut records = 0u64;

    while pos < chunk.len {
        // Cancellation is checked once per block rather than per byte, for
        // the same reason as in `scanner::ascii`: an atomic load per byte
        // would be pure overhead on the hot loop, and a block is small
        // enough to stay responsive.
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let want = min(buf.len() as u64, chunk.len - pos) as usize;
        read_exact_at(file, &mut buf[..want], chunk.offset + pos)?;
        for (i, &b) in buf[..want].iter().enumerate() {
            let abs = chunk.offset + pos + i as u64;
            let ch = decode_byte(b);
            if is_text(cfg, ch) {
                if run_data.is_empty() {
                    run_offset = abs;
                    run_started = run_offset == chunk.offset;
                }
                run_data.push(ch);
                run_cb += 1;
                run_cch += 1;
            } else if !run_data.is_empty() {
                let rec = MatchRecord {
                    offset: run_offset,
                    cb: run_cb,
                    cch: run_cch,
                    encoding: InputEncoding::Windows1251,
                    starts_at_chunk: run_started,
                    ends_at_chunk: false,
                    data: RecordData::Text(std::mem::take(&mut run_data)),
                };
                // Counted only if `emit_record` actually wrote it -- runs
                // below `min_cch` that touch no boundary are dropped there,
                // and counting them would badly inflate `--stats`.
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

    // A run still open when the chunk ends may continue into the next
    // chunk, so it is flushed with `ends_at_chunk: true` for the merger to
    // stitch. Skipped when cancelled: a cancelled scan's output is partial
    // by design, and this run is not necessarily adjacent to the boundary.
    if !cancelled.load(Ordering::Relaxed) && !run_data.is_empty() {
        let rec = MatchRecord {
            offset: run_offset,
            cb: run_cb,
            cch: run_cch,
            encoding: InputEncoding::Windows1251,
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
    temp_file.seek(io::SeekFrom::Start(0))?;
    Ok((records, temp_file))
}
