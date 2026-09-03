//! Shared JIS X 0208 level (水準) tables used by `filter::kanji_jis1` and
//! `filter::kanji_jis2`.
//!
//! # Why these filters exist
//!
//! `filter::kanji` admits all of CJK Unified Ideographs (U+4E00-U+9FFF)
//! plus Extension A (U+3400-U+4DBF): 27,584 code points, or **42% of the
//! entire BMP** on its own. Because
//! `scanner::utf16le` treats any 2-byte pair as a candidate code unit, a
//! filter that wide effectively admits random data: the probability that
//! `min_cch` consecutive random code units are *all* allowed is
//! `p^min_cch`, and at `p = 0.43` even a 12-character threshold leaves
//! enormous numbers of false positives (measured: ~118 million matches
//! over a 36 GiB image that contained no genuine UTF-16LE Japanese text at
//! all).
//!
//! The fix is to shrink `p`, which helps *exponentially* rather than
//! linearly. JIS X 0208 level 1 is 2,965 kanji and level 2 a further
//! 3,390 -- together 6,355, or roughly a quarter of what `Kanji` admits,
//! and level 1 alone about a ninth. Since the false-positive rate scales
//! as `p^min_cch`, a 9x reduction in `p` is a ~6500x reduction in matches
//! at `min_cch = 4`.
//!
//! The recall cost is small in practice: level 1 is ordered by reading and
//! covers all 2,136 jōyō kanji, and level 1 + level 2 together cover
//! essentially all kanji used in ordinary Japanese text, including the
//! jinmeiyō (name) kanji. Characters outside both levels are overwhelmingly
//! Chinese-only or historical forms that would not appear in a Japanese
//! string anyway.
//!
//! # Why the table is derived rather than written out
//!
//! Hard-coding 6,355 code points -- or the several hundred disjoint ranges
//! they form within U+4E00-U+9FFF -- would be a second source of truth
//! that could silently drift from reality, and impossible to review by
//! eye. Instead the tables are *derived at startup* from `encoding_rs`,
//! the same decoder the CJK scanners already trust to decide which
//! two-byte sequences are real (see `scanner::dbcs::is_defined_seq`):
//! every
//! Shift_JIS byte pair is converted to its JIS row (区/ku), decoded to a
//! `char`, and recorded in whichever level's bitset its row falls in.
//!
//! This costs one pass over ~11,000 byte pairs, once per process, and
//! produces a lookup that is a single bit test at scan time -- so unlike a
//! range-comparison chain, an arbitrarily scattered character set costs
//! exactly as much as a contiguous one. (That property is also why
//! `FilterSet`'s BMP bitmap can represent these filters with no loss: see
//! `filter::FilterSet`.)

use std::sync::OnceLock;
use super::BMP_WORDS;

/// JIS X 0208 rows (区) holding level 1 kanji. Rows 1-8 are symbols,
/// kana, Latin, Greek and Cyrillic, and rows 9-15 are unassigned in JIS X
/// 0208 proper -- neither holds kanji -- so restricting to rows 16 and
/// above yields kanji only, with no extra filtering needed.
const LEVEL1_ROWS: std::ops::RangeInclusive<u8> = 16..=47;

/// JIS X 0208 rows holding level 2 kanji. Row 84 is the last one that
/// contains any; rows 85-94 are unassigned in JIS X 0208 proper (CP932
/// reuses some of that space for vendor extensions, which are deliberately
/// excluded here).
const LEVEL2_ROWS: std::ops::RangeInclusive<u8> = 48..=84;

/// Converts a Shift_JIS byte pair to its JIS X 0208 row and cell
/// (区点/ku-ten), or `None` if the pair isn't structurally a valid
/// two-byte Shift_JIS sequence.
///
/// This is the standard Shift_JIS <-> JIS conversion: lead bytes are
/// allocated two rows each (one for trail bytes up to 0x9E, one for the
/// rest), with a gap at 0xA0-0xDF reserved for half-width katakana, hence
/// the two disjoint lead-byte ranges.
fn sjis_to_ku_ten(s1: u8, s2: u8) -> Option<(u8, u8)> {
    // Lead byte must be in one of the two two-byte-sequence ranges; the
    // gap between them (0xA0-0xDF) is single-byte half-width katakana.
    let pair_index = match s1 {
        0x81..=0x9F => (s1 as u16) - 0x81,
        0xE0..=0xFC => (s1 as u16) - 0xC1,
        _ => return None,
    };
    if s2 < 0x40 || s2 == 0x7F || s2 > 0xFC {
        return None;
    }

    // Each lead byte covers two consecutive rows, split at trail byte
    // 0x9E/0x9F. 0x7F is skipped in the lower half (it's DEL), which is
    // why the lower half's offset changes across it.
    let (ku, ten) = if s2 <= 0x9E {
        let ten = (s2 as u16) - if s2 <= 0x7E { 0x3F } else { 0x40 };
        (pair_index * 2 + 1, ten)
    } else {
        (pair_index * 2 + 2, (s2 as u16) - 0x9E)
    };

    Some((ku as u8, ten as u8))
}

/// The two derived bitsets, built together in one pass since they come
/// from the same walk over the Shift_JIS code space.
struct Tables {
    level1: Box<[u64; BMP_WORDS]>,
    /// Level 1 *and* level 2, since `KanjiJis2` is defined as "level 2 as
    /// well as level 1" -- selecting it alone to mean "level 2 but not
    /// level 1" would be a strange thing to ask for and an easy footgun.
    level1_and_2: Box<[u64; BMP_WORDS]>,
}

static TABLES: OnceLock<Tables> = OnceLock::new();

fn tables() -> &'static Tables {
    TABLES.get_or_init(|| {
        let mut level1 = Box::new([0u64; BMP_WORDS]);
        let mut level1_and_2 = Box::new([0u64; BMP_WORDS]);

        for s1 in 0x81u8..=0xFC {
            for s2 in 0x40u8..=0xFC {
                let Some((ku, _ten)) = sjis_to_ku_ten(s1, s2) else {
                    continue;
                };
                let in_level1 = LEVEL1_ROWS.contains(&ku);
                let in_level2 = LEVEL2_ROWS.contains(&ku);
                if !in_level1 && !in_level2 {
                    continue;
                }

                // Ask the real decoder what (if anything) this pair means.
                // Structurally valid rows still contain unassigned cells,
                // and deferring to `encoding_rs` keeps this table and
                // `scanner::cp932`'s validity check from ever disagreeing.
                let bytes = [s1, s2];
                let (decoded, had_errors) =
                    encoding_rs::SHIFT_JIS.decode_without_bom_handling(&bytes);
                if had_errors {
                    continue;
                }
                let mut chars = decoded.chars();
                let (Some(ch), None) = (chars.next(), chars.next()) else {
                    continue;
                };
                let scalar = ch as u32;
                // Every JIS X 0208 kanji is in the BMP; anything else
                // would be a decoder surprise, and is skipped rather than
                // silently truncated.
                if scalar > 0xFFFF {
                    continue;
                }
                let (word, bit) = ((scalar >> 6) as usize, scalar & 63);

                if in_level1 {
                    level1[word] |= 1u64 << bit;
                }
                level1_and_2[word] |= 1u64 << bit;
            }
        }

        Tables { level1, level1_and_2 }
    })
}

/// Whether `u` is a JIS X 0208 level 1 kanji.
#[inline]
pub(crate) fn is_level1(u: u16) -> bool {
    let t = tables();
    t.level1[(u >> 6) as usize] >> (u & 63) & 1 != 0
}

/// Whether `u` is a JIS X 0208 level 1 *or* level 2 kanji.
#[inline]
pub(crate) fn is_level1_or_2(u: u16) -> bool {
    let t = tables();
    t.level1_and_2[(u >> 6) as usize] >> (u & 63) & 1 != 0
}

/// How many code points each table admits. Used by tests to pin the table
/// sizes against the published JIS X 0208 counts, which is the cheapest
/// way to catch a derivation that has gone subtly wrong.
#[cfg(test)]
pub(crate) fn counts() -> (u32, u32) {
    let t = tables();
    (
        t.level1.iter().map(|w| w.count_ones()).sum(),
        t.level1_and_2.iter().map(|w| w.count_ones()).sum(),
    )
}
