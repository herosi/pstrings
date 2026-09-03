//! GBK (Chinese, GB2312 superset) support.
//!
//! All of the scanning machinery lives in `scanner::dbcs`, which this
//! module parameterizes with GBK's byte ranges. See that module's doc
//! comment for why these CJK encodings share one engine, and
//! `dbcs::scan` for the chunk-boundary deferral design that GBK's
//! ASCII-overlapping trail bytes force.
//!
//! GBK is structurally a Shift_JIS-shaped encoding: single-byte ASCII,
//! plus two-byte sequences whose trail range overlaps printable ASCII.
//! Measured against `encoding_rs`: 126 distinct lead bytes spanning
//! 0x81..=0xFE, and for lead 0x81, 190 valid trail bytes of which **63
//! fall inside printable ASCII** -- which is what makes GBK
//! non-self-synchronizing and therefore a `RecordData::Raw` producer at
//! chunk boundaries.
//!
//! Note that GBK, unlike CP932, has **no single-byte characters above
//! 0x7F**: measured, exactly 129 of 256 single bytes decode on their own
//! (the 128 ASCII bytes plus 0x80, which GBK maps to the euro sign
//! U+20AC). See `is_single` for why 0x80 is nonetheless excluded here.

use super::dbcs::{self, Dbcs};
use super::ResolvedFragment;
use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::InputEncoding;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// First byte of a two-byte GBK sequence.
///
/// GBK's lead range is a single contiguous span, wider than CP932's (which
/// has a hole at 0xA0..=0xDF, where it puts half-width katakana at
/// 0xA1..=0xDF instead). Verified against
/// `encoding_rs`: every byte in 0x81..=0xFE begins at least one valid
/// pair, and no byte outside it does.
#[inline]
fn is_gbk_lead(b: u8) -> bool {
    matches!(b, 0x81..=0xFE)
}

/// Second (trailing) byte of a two-byte GBK sequence.
///
/// 0x40..=0xFE excluding 0x7F. As with CP932, this range overlaps *both*
/// printable ASCII and the lead-byte range, which is exactly what forces
/// the deferred-boundary design; 0x7F is excluded because GBK never uses
/// DEL as a trail byte.
#[inline]
fn is_gbk_trail(b: u8) -> bool {
    matches!(b, 0x40..=0x7E | 0x80..=0xFE)
}

/// A single byte that stands on its own: printable ASCII plus tab.
///
/// Unlike CP932 there are no high single-byte characters to admit. GBK
/// does map the lone byte 0x80 to the euro sign, but that is deliberately
/// *not* accepted here: 0x80 is also a perfectly ordinary trail byte, so
/// treating it as a standalone character would let a run start in the
/// middle of a two-byte sequence. Excluding it costs nothing real (a bare
/// euro sign is not a string worth reporting) and keeps single-byte and
/// trail-byte roles from overlapping.
///
/// Like every other decision in this module, this does not consult the
/// user's `--filter` selection -- GBK validates structurally, so it has no
/// false-positive problem for `--filter` to solve. See the "Which scanners
/// this actually affects" section on `filter::CharacterFilter`.
#[inline]
fn is_gbk_single(b: u8) -> bool {
    matches!(b, 0x20..=0x7E | b'\t')
}

/// GBK's parameterization of the shared double-byte engine.
pub(crate) struct Gbk;

impl Dbcs for Gbk {
    const ENCODING: InputEncoding = InputEncoding::Gbk;

    #[inline]
    fn decoder() -> &'static encoding_rs::Encoding {
        encoding_rs::GBK
    }

    #[inline]
    fn is_lead(b: u8) -> bool {
        is_gbk_lead(b)
    }

    #[inline]
    fn is_trail(b: u8) -> bool {
        is_gbk_trail(b)
    }

    #[inline]
    fn is_single(b: u8) -> bool {
        is_gbk_single(b)
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
    dbcs::scan::<Gbk>(file, file_len, chunk, cfg, temp_path, cancelled)
}

pub(crate) fn segment_raw(bytes: &[u8]) -> (Vec<ResolvedFragment>, Vec<u8>) {
    dbcs::segment_raw::<Gbk>(bytes)
}
