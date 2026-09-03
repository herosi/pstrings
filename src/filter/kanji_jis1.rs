/// JIS X 0208 level 1 kanji (第一水準漢字): 2,965 characters, ordered by
/// reading, covering all 2,136 jōyō kanji.
///
/// This is the narrow alternative to `Kanji`, which admits all 27,584 code
/// points of CJK Unified Ideographs + Extension A. Because
/// `scanner::utf16le`'s false-positive rate scales as `p^min_cch`, cutting
/// the admitted set by ~9x cuts spurious matches by several orders of
/// magnitude -- see `filter::jis`'s module doc comment for the measured
/// numbers and for why the table is derived from `encoding_rs` rather than
/// hard-coded.
///
/// Has no single-byte representation, so `allows_u8` always returns
/// `false`; this filter only ever matters for the UTF-16LE scanners.
#[inline]
pub(crate) fn allows_u8(_b: u8) -> bool {
    false
}

#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    super::jis::is_level1(u)
}

#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    // Every JIS X 0208 kanji is in the BMP, so anything above U+FFFF is
    // definitionally not in this set and the truncating cast below is
    // never reached for it.
    let scalar = ch as u32;
    scalar <= 0xFFFF && super::jis::is_level1(scalar as u16)
}
