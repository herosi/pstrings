/// Katakana (U+30A0-U+30FF).
///
/// Deliberately *excludes* the Katakana Phonetic Extensions block
/// (U+31F0-U+31FF). Those 16 small kana exist for writing Ainu, are
/// vanishingly rare in the material this tool scans, and have no glyph in
/// most fonts -- so in practice they showed up only as boxes. Every code
/// point admitted costs something for `scanner::utf16le`, where any byte
/// pair is a candidate code unit (see `filter::jis`'s module doc comment),
/// so a range that only ever produced noise is not worth keeping.
///
/// The main block is admitted in full: all of U+30A0-U+30FF is assigned,
/// including U+30A0 double hyphen ゠, U+30F7-U+30FA (ヷヸヹヺ) and
/// U+30FF ヿ, none of which Shift_JIS can encode but all of which are
/// real characters that appear in Unicode text.
///
/// Has no single-byte representation, so `allows_u8` always returns
/// `false`; this filter only ever matters for the UTF-16LE scanners.
/// (Note that *halfwidth* katakana, U+FF61-U+FF9F, is covered by
/// `CjkPunct` alongside the other halfwidth/fullwidth forms.)
#[inline]
pub(crate) fn allows_u8(_b: u8) -> bool {
    false
}

#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    (0x30A0..=0x30FF).contains(&u)
}

#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    ('\u{30A0}'..='\u{30FF}').contains(&ch)
}