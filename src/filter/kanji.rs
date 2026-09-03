/// CJK Unified Ideographs (U+4E00-U+9FFF) plus Extension A (U+3400-U+4DBF):
/// Han characters (kanji/hanzi/hanja), shared across Japanese, Chinese, and
/// Korean text. BMP only -- see the `Kanji` variant's doc comment on
/// `CharacterFilter` for why Extension B onward (astral-plane ideographs)
/// isn't covered.
///
/// Has no single-byte representation, so `allows_u8` always returns
/// `false`; this filter only ever matters for the UTF-16LE scanners.
#[inline]
pub(crate) fn allows_u8(_b: u8) -> bool {
    false
}

#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    (0x4E00..=0x9FFF).contains(&u) || (0x3400..=0x4DBF).contains(&u)
}

#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&ch) || ('\u{3400}'..='\u{4DBF}').contains(&ch)
}