/// Hiragana (U+3041-U+3096 and U+309B-U+309F), i.e. the assigned,
/// non-combining characters of the Hiragana block.
///
/// Deliberately *not* the whole U+3040-U+309F block: five of its code
/// points are unassigned (U+3040, U+3097, U+3098) or are combining marks
/// that never stand alone in extracted text (U+3099 COMBINING VOICED
/// SOUND MARK, U+309A). Unassigned code points can never appear in
/// genuine text, so admitting them contributes only false positives --
/// and for `scanner::utf16le`, where any byte pair is a candidate code
/// unit, that cost is real: they were observed rendering as boxes in
/// output. See `filter::jis`'s module doc comment for why the size of the
/// admitted set matters so much to that scanner.
///
/// U+3094 ゔ and U+3095-U+3096 (small ゕ/ゖ) are kept: they are assigned
/// and do occur, even though Shift_JIS has no encoding for them.
///
/// Has no single-byte representation, so `allows_u8` always returns
/// `false`; this filter only ever matters for the UTF-16LE scanners.
#[inline]
pub(crate) fn allows_u8(_b: u8) -> bool {
    false
}

#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    // U+3040 unassigned; U+3097-U+309A unassigned or combining.
    (0x3041..=0x3096).contains(&u) || (0x309B..=0x309F).contains(&u)
}

#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    let scalar = ch as u32;
    scalar <= 0xFFFF && allows_u16(scalar as u16)
}