//! Big5 (Traditional Chinese) support.
//!
//! All of the scanning machinery lives in `scanner::dbcs`, which this
//! module parameterizes with Big5's byte ranges. See that module's doc
//! comment for why these CJK encodings share one engine, and
//! `dbcs::scan` for the chunk-boundary deferral design that Big5's
//! ASCII-overlapping trail bytes force.
//!
//! # The four two-scalar sequences
//!
//! Big5 is the one encoding here with sequences that decode to more than
//! one Unicode scalar. Four two-byte sequences produce a Latin letter plus
//! a combining diacritic:
//!
//! | bytes   | decodes to        |
//! |---------|-------------------|
//! | `88 62` | U+00CA U+0304  Ê̄ |
//! | `88 64` | U+00CA U+030C  Ê̌ |
//! | `88 a3` | U+00EA U+0304  ê̄ |
//! | `88 a5` | U+00EA U+030C  ê̌ |
//!
//! These are circumflex-vowel-plus-tone-mark forms used for Minnan and
//! Hakka. They were once a reason to think Big5 could not share the
//! engine, because `dbcs::count_chars` used to count *encoded sequences*
//! rather than Unicode scalars and would therefore have reported these as
//! one character each.
//!
//! That turned out to be a defect in `count_chars` rather than a property
//! of Big5: every other scanner in this crate defines `cch` as a count of
//! Unicode scalars, so counting sequences made `--min-length` mean
//! something subtly different for the double-byte encodings. `count_chars`
//! now derives the count from the decoded string, which is correct for
//! Big5 and was verified to be a no-op for CP932, GBK and EUC-KR
//! (exhaustively, over all 9,763 / 24,036 / 17,144 valid sequences --
//! zero disagreements). So Big5 needs no special handling at all, and
//! this module is as thin as the others.
//!
//! Note this means `cb` (2) and `cch` (2) coincide for those four
//! sequences by accident, not by rule; they remain independent
//! quantities, as they already were for every ASCII-plus-double-byte mix.

use super::dbcs::{self, Dbcs};
use super::ResolvedFragment;
use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::InputEncoding;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// First byte of a two-byte Big5 sequence.
///
/// Measured against `encoding_rs`: exactly 120 lead bytes, spanning
/// 0x87..=0xFE with **no gaps**. The range starts higher than GBK's
/// (0x81) because Big5 leaves 0x81..=0x86 unassigned.
#[inline]
fn is_big5_lead(b: u8) -> bool {
    matches!(b, 0x87..=0xFE)
}

/// Second (trailing) byte of a two-byte Big5 sequence.
///
/// Measured: 157 distinct trail bytes over 0x40..=0xFE, with a single
/// contiguous gap at 0x7F..=0xA0 -- i.e. two clean ranges. **63 of them
/// fall inside printable ASCII**, which is what makes Big5
/// non-self-synchronizing and therefore a `RecordData::Raw` producer at
/// chunk boundaries, exactly like CP932, GBK, EUC-KR and GB18030.
#[inline]
fn is_big5_trail(b: u8) -> bool {
    matches!(b, 0x40..=0x7E | 0xA1..=0xFE)
}

/// A single byte that stands on its own: printable ASCII plus tab.
///
/// Big5 has no single-byte characters above 0x7F -- measured, exactly the
/// 128 bytes 0x00..=0x7F decode standalone and nothing else, so unlike
/// CP932 (half-width katakana) there is no high-byte standalone range to
/// admit here.
///
/// Like every other decision in this module, this does not consult the
/// user's `--filter` selection -- Big5 validates structurally, so it has
/// no false-positive problem for `--filter` to solve. See the "Which
/// scanners this actually affects" section on `filter::CharacterFilter`.
#[inline]
fn is_big5_single(b: u8) -> bool {
    matches!(b, 0x20..=0x7E | b'\t')
}

/// Big5's parameterization of the shared double-byte engine.
pub(crate) struct Big5;

impl Dbcs for Big5 {
    const ENCODING: InputEncoding = InputEncoding::Big5;

    #[inline]
    fn decoder() -> &'static encoding_rs::Encoding {
        encoding_rs::BIG5
    }

    #[inline]
    fn is_lead(b: u8) -> bool {
        is_big5_lead(b)
    }

    #[inline]
    fn is_trail(b: u8) -> bool {
        is_big5_trail(b)
    }

    #[inline]
    fn is_single(b: u8) -> bool {
        is_big5_single(b)
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
    dbcs::scan::<Big5>(file, file_len, chunk, cfg, temp_path, cancelled)
}

pub(crate) fn segment_raw(bytes: &[u8]) -> (Vec<ResolvedFragment>, Vec<u8>) {
    dbcs::segment_raw::<Big5>(bytes)
}
