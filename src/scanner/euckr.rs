//! EUC-KR (Korean, Unified Hangul Code / CP949) support.
//!
//! All of the scanning machinery lives in `scanner::dbcs`, which this
//! module parameterizes with EUC-KR's byte ranges. See that module's doc
//! comment for why these CJK encodings share one engine, and
//! `dbcs::scan` for the chunk-boundary deferral design that EUC-KR's
//! ASCII-overlapping trail bytes force.
//!
//! What `encoding_rs` labels `EUC_KR` is in practice the extended
//! Unified Hangul Code (Windows CP949), which is why the lead range below
//! reaches down to 0x81 rather than starting at 0xA1 as textbook EUC-KR
//! would. Measured against `encoding_rs`: 124 distinct lead bytes spanning
//! 0x81..=0xFD, and for lead 0x81, 178 valid trail bytes of which **52
//! fall inside printable ASCII** -- which is what makes EUC-KR
//! non-self-synchronizing and therefore a `RecordData::Raw` producer at
//! chunk boundaries.

use super::dbcs::{self, Dbcs};
use super::ResolvedFragment;
use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::InputEncoding;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// First byte of a two-byte EUC-KR / UHC sequence.
///
/// Verified against `encoding_rs`: no byte outside 0x81..=0xFD begins a
/// valid pair. Within it, every byte does except 0xC9, the unassigned
/// user-defined row -- so 124 of the 125 in-range bytes are real leads.
/// Admitting 0xC9 here costs nothing, since `dbcs::is_defined_seq`
/// rejects every pair it could start. (0xFE and 0xFF never lead, unlike
/// GBK which reaches 0xFE; 0xFE *is* a valid trail byte here, though.)
#[inline]
fn is_euckr_lead(b: u8) -> bool {
    matches!(b, 0x81..=0xFD)
}

/// Second (trailing) byte of a two-byte EUC-KR / UHC sequence.
///
/// The ASCII-range portion is narrower than GBK's: only the letter ranges
/// 0x41..=0x5A and 0x61..=0x7A are used, not the full 0x40..=0x7E. That
/// still overlaps printable ASCII in 52 places, which is more than enough
/// to make a chunk-boundary byte's role ambiguous, so the deferred
/// design applies here exactly as it does for CP932 and GBK.
#[inline]
fn is_euckr_trail(b: u8) -> bool {
    matches!(b, 0x41..=0x5A | 0x61..=0x7A | 0x81..=0xFE)
}

/// A single byte that stands on its own: printable ASCII plus tab.
///
/// EUC-KR has no single-byte characters above 0x7F -- measured, exactly
/// 128 of 256 single bytes decode on their own, i.e. plain ASCII and
/// nothing else.
///
/// Like every other decision in this module, this does not consult the
/// user's `--filter` selection -- EUC-KR validates structurally, so it has
/// no false-positive problem for `--filter` to solve. See the "Which
/// scanners this actually affects" section on `filter::CharacterFilter`.
#[inline]
fn is_euckr_single(b: u8) -> bool {
    matches!(b, 0x20..=0x7E | b'\t')
}

/// EUC-KR's parameterization of the shared double-byte engine.
pub(crate) struct EucKr;

impl Dbcs for EucKr {
    const ENCODING: InputEncoding = InputEncoding::EucKr;

    #[inline]
    fn decoder() -> &'static encoding_rs::Encoding {
        encoding_rs::EUC_KR
    }

    #[inline]
    fn is_lead(b: u8) -> bool {
        is_euckr_lead(b)
    }

    #[inline]
    fn is_trail(b: u8) -> bool {
        is_euckr_trail(b)
    }

    #[inline]
    fn is_single(b: u8) -> bool {
        is_euckr_single(b)
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
    dbcs::scan::<EucKr>(file, file_len, chunk, cfg, temp_path, cancelled)
}

pub(crate) fn segment_raw(bytes: &[u8]) -> (Vec<ResolvedFragment>, Vec<u8>) {
    dbcs::segment_raw::<EucKr>(bytes)
}
