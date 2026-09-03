/// Korean: Hangul syllables (U+AC00-U+D7A3), Hangul Jamo (U+1100-U+11FF),
/// Hangul Compatibility Jamo (U+3130-U+318F), and halfwidth Hangul jamo
/// (U+FFA0-U+FFDC).
///
/// The halfwidth jamo live in the Halfwidth and Fullwidth Forms block and
/// so used to be swept up by `CjkPunct`, where they were pure noise for
/// Japanese scanning (they have no glyph in a typical Japanese font and
/// showed up as boxes). They are genuinely Korean, so they belong here
/// instead -- moved rather than dropped, so no character coverage is lost
/// overall. See `filter::cjkpunct` for the rest of that narrowing.
///
/// Has no single-byte representation, so `allows_u8` always returns
/// `false`; this filter only ever matters for the UTF-16LE scanners.
#[inline]
pub(crate) fn allows_u8(_b: u8) -> bool {
    false
}

#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    (0xAC00..=0xD7A3).contains(&u)
        || (0x1100..=0x11FF).contains(&u)
        || (0x3130..=0x318F).contains(&u)
        || (0xFFA0..=0xFFDC).contains(&u)
}

#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&ch)
        || ('\u{1100}'..='\u{11FF}').contains(&ch)
        || ('\u{3130}'..='\u{318F}').contains(&ch)
        || ('\u{FFA0}'..='\u{FFDC}').contains(&ch)
}