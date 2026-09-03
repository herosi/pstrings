/// JIS X 0208 levels 1 and 2 (第一・第二水準漢字): 6,355 characters,
/// covering essentially all kanji used in ordinary Japanese text including
/// the jinmeiyō (name) kanji.
///
/// Deliberately a superset of `KanjiJis1` rather than "level 2 only":
/// asking for level 2 while excluding level 1 would exclude the jōyō kanji
/// and is not a combination anyone wants. Selecting both this and
/// `KanjiJis1` is therefore harmless and simply redundant.
///
/// Still ~4x narrower than `Kanji`, which admits all of CJK Unified
/// Ideographs + Extension A -- see `filter::jis`'s module doc comment for
/// why that matters so much to `scanner::utf16le`'s false-positive rate.
///
/// Has no single-byte representation, so `allows_u8` always returns
/// `false`; this filter only ever matters for the UTF-16LE scanners.
#[inline]
pub(crate) fn allows_u8(_b: u8) -> bool {
    false
}

#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    super::jis::is_level1_or_2(u)
}

#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    let scalar = ch as u32;
    scalar <= 0xFFFF && super::jis::is_level1_or_2(scalar as u16)
}
