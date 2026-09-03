/// Latin-1 Supplement (U+00A0-U+00FF): accented Latin letters and common
/// Western European punctuation/symbols beyond ASCII. Excludes U+0080-U+009F
/// (the C1 control block that precedes this range), matching `ascii`'s
/// exclusion of C0 controls.
///
/// Unlike the other non-ASCII filters, this one has a well-defined
/// single-byte form: ISO-8859-1 maps byte N directly to U+00N for
/// 0xA0..=0xFF, so `allows_u8` is meaningful here (used by
/// `scanner::ascii`), not just `allows_u16`/`allows_char`.
#[inline]
pub(crate) fn allows_u8(b: u8) -> bool {
    (0xA0..=0xFF).contains(&b)
}

#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    (0x00A0..=0x00FF).contains(&u)
}

#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    ('\u{00A0}'..='\u{00FF}').contains(&ch)
}