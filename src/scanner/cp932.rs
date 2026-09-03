//! CP932 (Microsoft's Shift_JIS) support.
//!
//! All of the scanning machinery lives in `scanner::dbcs`, which this
//! module parameterizes with CP932's byte ranges. See that module's doc
//! comment for why these CJK encodings share one engine, and
//! `dbcs::scan` for the chunk-boundary deferral design that CP932's
//! ASCII-overlapping trail bytes force.

use super::dbcs::{self, Dbcs};
use super::ResolvedFragment;
use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::InputEncoding;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// First byte of a two-byte CP932 (Shift_JIS-family) sequence. Covers the
/// standard JIS X 0208 lead-byte range (0x81..=0x9F, 0xE0..=0xEF) as well
/// as Microsoft's CP932 extensions (NEC row 13, IBM extensions) which push
/// the upper end out to 0xFC.
#[inline]
fn is_sjis_first_byte(b: u8) -> bool {
    matches!(b, 0x81..=0x9F | 0xE0..=0xFC)
}

/// Second (trailing) byte of a two-byte sequence.
///
/// Notably, this range overlaps *both* the printable-ASCII range
/// (0x40..=0x7E falls inside it) *and* the lead-byte range above (every
/// lead byte, 0x81..=0x9F and 0xE0..=0xFC, is also a valid trail). That
/// overlap is *why* this encoding
/// has to defer boundary decisions instead of guessing at scan time -- see
/// `dbcs::scan`'s doc comment for the full story.
#[inline]
fn is_sjis_second_byte(b: u8) -> bool {
    matches!(b, 0x40..=0x7E | 0x80..=0xFC)
}

/// A single byte that stands on its own: printable ASCII (plus tab) or
/// half-width katakana (0xA1..=0xDF).
///
/// Deliberately self-contained: this does *not* consult the user's
/// `--filter` selection, and neither does any other decision this scanner
/// makes. That is the same exemption `scanner::utf8` has, for the same
/// reason -- see the "Which scanners this actually affects" section on
/// `filter::CharacterFilter`. In short, `--filter` exists to suppress
/// false positives in scanners that cannot validate their own input
/// (overwhelmingly `scanner::utf16le`, where any even-aligned byte pair is
/// a syntactically valid code unit). CP932 validates structurally instead:
/// lead/trail byte ranges are checked here and every two-byte pair is
/// confirmed against `encoding_rs`, so a CP932 match is already
/// trustworthy without any character-class narrowing.
///
/// Applying the filter here would also be actively harmful in two ways.
/// First, a user scanning a Japanese binary would reasonably write
/// `--filter kanji,hiragana,katakana` -- dropping `ascii` precisely to
/// quiet the UTF-16LE scanner -- and would be surprised to find CP932 had
/// silently stopped reporting ASCII strings too. Second, CP932's natural
/// single-byte set includes half-width katakana, which no filter variant
/// can express as a *byte* (they are 0xA1..=0xDF here, but U+FF61..=U+FF9F
/// as characters, so `Latin1`'s byte range would wrongly admit them while
/// `Katakana`'s would wrongly reject them); routing this through
/// `allows_u8` would silently defeat half the reason to choose this
/// encoding.
#[inline]
fn is_valid_ascii_or_kana(b: u8) -> bool {
    matches!(b, 0x20..=0x7E | 0xA1..=0xDF | b'\t')
}

/// CP932's parameterization of the shared double-byte engine.
pub(crate) struct Cp932;

impl Dbcs for Cp932 {
    const ENCODING: InputEncoding = InputEncoding::Cp932;

    #[inline]
    fn decoder() -> &'static encoding_rs::Encoding {
        encoding_rs::SHIFT_JIS
    }

    #[inline]
    fn is_lead(b: u8) -> bool {
        is_sjis_first_byte(b)
    }

    #[inline]
    fn is_trail(b: u8) -> bool {
        is_sjis_second_byte(b)
    }

    #[inline]
    fn is_single(b: u8) -> bool {
        is_valid_ascii_or_kana(b)
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
    dbcs::scan::<Cp932>(file, file_len, chunk, cfg, temp_path, cancelled)
}

pub(crate) fn segment_raw(bytes: &[u8]) -> (Vec<ResolvedFragment>, Vec<u8>) {
    dbcs::segment_raw::<Cp932>(bytes)
}
