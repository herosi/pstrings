/// Cyrillic (U+0400-U+04FF): the Russian, Ukrainian, Belarusian, Serbian,
/// Macedonian and Bulgarian alphabets, plus the historic and
/// non-Slavic-language letters that share the block.
///
/// # Why the whole block, and not just what windows-1251 uses
///
/// This filter was added for `scanner::win1251`, which reaches exactly 94
/// code points scattered across U+0401-U+0491 (51 gaps inside that
/// extent). It would therefore be possible to define this filter as those
/// 94 points precisely.
///
/// That is deliberately not done, for two reasons:
///
///  * A filter names a *script*, not a codepage. `latin1` is the existing
///    precedent: it admits all of U+00A0-U+00FF rather than only the
///    subset some particular encoding happens to produce. Tying this one
///    to windows-1251's repertoire would make it the odd one out, and
///    would make `--filter cyrillic` mean something different depending on
///    which `-e` it was paired with.
///  * The single-byte scanner cannot emit a code point outside its own
///    table anyway, so for `scanner::win1251` the extra 162 code points
///    are unreachable and cost nothing. Where they *are* reachable is
///    `scanner::utf16le`, and there the user asking for "Cyrillic"
///    plainly means the script. (`scanner::utf8` ignores `--filter`
///    entirely -- see `filter::CharacterFilter`.)
///
/// # No single-byte form
///
/// `allows_u8` is always false. Unlike `latin1`, Cyrillic has no
/// encoding-independent single-byte representation: byte 0xC0 is U+0410 in
/// windows-1251, U+044E in KOI8-R, and U+00C0 in ISO-8859-1. A byte only
/// means a Cyrillic letter *relative to a codepage*, so the decision
/// belongs to the scanner's table, not to a byte-oriented filter. This is
/// why `scanner::win1251` filters on the decoded `char` (`allows_char`)
/// rather than on the raw byte -- see that module's doc comment.
#[inline]
pub(crate) fn allows_u8(_b: u8) -> bool {
    false
}

#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    (0x0400..=0x04FF).contains(&u)
}

#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&ch)
}
