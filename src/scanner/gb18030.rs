//! GB18030 (Chinese, the mandatory PRC standard) support.
//!
//! All of the scanning machinery lives in `scanner::dbcs`, which this
//! module parameterizes with GB18030's byte ranges. See that module's doc
//! comment for why these CJK encodings share one engine, and
//! `dbcs::scan` for the chunk-boundary deferral design that GB18030's
//! ASCII-overlapping trail bytes force.
//!
//! # Relationship to GBK
//!
//! GB18030's two-byte form is *identical* to GBK's. Measured against
//! `encoding_rs` over all 65,536 byte pairs, both accept exactly the same
//! 24,069 of 32,768 candidate pairs, and both accept exactly the same 129
//! of 256 single bytes. So on any input containing no four-byte
//! sequences, `-e gbk` and `-e gb18030` find exactly the same runs at
//! exactly the same offsets, differing only in the encoding label each
//! record carries.
//!
//! The difference is the **four-byte form**, which is what makes GB18030 a
//! full Unicode encoding rather than a legacy code page: it covers
//! everything GBK leaves out, including the astral planes.
//!
//! # The four-byte form
//!
//! Structurally `0x81-0xFE, 0x30-0x39, 0x81-0xFE, 0x30-0x39` -- a lead
//! byte, a digit, a lead byte, a digit. Measured against `encoding_rs`:
//!
//! | property | measured |
//! |---|---|
//! | valid four-byte sequences | 1,087,996 |
//! | distinct 1st bytes | 88, within 0x81..=0xE3 (with a gap at 0x85..=0x8F) |
//! | distinct 2nd bytes | 10, exactly 0x30..=0x39 |
//! | distinct 3rd bytes | 126, spanning 0x81..=0xFE |
//! | distinct 4th bytes | 10, exactly 0x30..=0x39 |
//! | sequences decoding to more than one scalar | **0** |
//! | sequences ambiguous between the 2- and 4-byte readings | **0** |
//! | valid *two*-byte pairs whose trail byte is a digit | **0** |
//!
//! Those last two rows are the important ones. Because no valid two-byte
//! pair ends in a digit, the second byte *alone* decides which form is
//! being read -- there is never a case where the scanner must try one
//! length, fail, and back up. That is what let the four-byte form slot
//! into the existing single-pass engine (see `dbcs::decode_step`) instead
//! of needing a backtracking one.
//!
//! # Why the ranges here are deliberately loose
//!
//! `starts_four_byte` checks only "is the second byte a digit" -- the
//! first byte being a lead is a precondition guaranteed by its only
//! caller, `dbcs::decode_step`, and even that is the full 0x81..=0xFE lead
//! range rather than the
//! narrower measured 0x81..=0xE3. That is intentional
//! and matches how the rest of this module family works: the structural
//! predicates are a fast pre-filter, and `encoding_rs` is the sole
//! authority on whether a sequence is actually assigned. Tightening the
//! range here would buy nothing (the decoder rejects those sequences
//! anyway) while adding a second source of truth that could drift.

use super::dbcs::{self, Dbcs};
use super::ResolvedFragment;
use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::InputEncoding;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// First byte of a multi-byte GB18030 sequence, of either length.
///
/// Identical to GBK's lead range: verified against `encoding_rs`, every
/// byte in 0x81..=0xFE begins at least one valid pair, and no byte outside
/// it does. The four-byte form's first byte is drawn from the narrower
/// 0x81..=0xE3, which is a subset, so one predicate covers both.
#[inline]
fn is_gb18030_lead(b: u8) -> bool {
    matches!(b, 0x81..=0xFE)
}

/// Second (trailing) byte of a *two*-byte GB18030 sequence.
///
/// 0x40..=0xFE excluding 0x7F -- identical to GBK. As there, this range
/// overlaps both printable ASCII and the lead-byte range, which is what
/// forces the deferred-boundary design.
///
/// Note that it excludes 0x30..=0x39: digits are never two-byte trail
/// bytes, which is precisely the property `starts_four_byte` relies on.
#[inline]
fn is_gb18030_trail(b: u8) -> bool {
    matches!(b, 0x40..=0x7E | 0x80..=0xFE)
}

/// A single byte that stands on its own: printable ASCII plus tab.
///
/// As with GBK, the lone byte 0x80 (which GB18030 maps to the euro sign)
/// is deliberately excluded, because 0x80 is also an ordinary trail byte
/// and admitting it would let a run start in the middle of a two-byte
/// sequence.
///
/// Like every other decision in this module, this does not consult the
/// user's `--filter` selection -- GB18030 validates structurally, so it
/// has no false-positive problem for `--filter` to solve. See the "Which
/// scanners this actually affects" section on `filter::CharacterFilter`.
#[inline]
fn is_gb18030_single(b: u8) -> bool {
    matches!(b, 0x20..=0x7E | b'\t')
}

/// GB18030's parameterization of the shared multi-byte engine.
pub(crate) struct Gb18030;

impl Dbcs for Gb18030 {
    const ENCODING: InputEncoding = InputEncoding::Gb18030;

    #[inline]
    fn decoder() -> &'static encoding_rs::Encoding {
        encoding_rs::GB18030
    }

    #[inline]
    fn is_lead(b: u8) -> bool {
        is_gb18030_lead(b)
    }

    #[inline]
    fn is_trail(b: u8) -> bool {
        is_gb18030_trail(b)
    }

    #[inline]
    fn is_single(b: u8) -> bool {
        is_gb18030_single(b)
    }

    /// The only override in the family. See the module doc comment for the
    /// measurements that justify deciding on one byte of lookahead.
    ///
    /// The caller guarantees `bytes.len() >= 2` and that `bytes[0]` is a
    /// lead byte, so this only has to inspect the second byte.
    #[inline]
    fn starts_four_byte(bytes: &[u8]) -> bool {
        matches!(bytes.get(1), Some(0x30..=0x39))
    }
}

pub(crate) fn scan(
    file: &File,
    file_len: u64,
    chunk: &Chunk,
    cfg: &Config,
    temp_path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<(u64, File)> {
    dbcs::scan::<Gb18030>(file, file_len, chunk, cfg, temp_path, cancelled)
}

pub(crate) fn segment_raw(bytes: &[u8]) -> (Vec<ResolvedFragment>, Vec<u8>) {
    dbcs::segment_raw::<Gb18030>(bytes)
}
