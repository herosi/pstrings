/// CJK Unified Ideographs Extension B (U+20000-U+2A6DF): additional, much
/// rarer kanji/hanzi/hanja outside the BMP, reached only via a surrogate
/// pair. Kept separate from `Kanji` (which covers the much more common BMP
/// ideographs) rather than folding it in there, so that the common case
/// (`--filter kanji`) doesn't implicitly opt into astral-plane matching --
/// see `scanner::utf16le::decode_char_at`'s doc comment for why admitting
/// astral characters is a distinct, separately-decided concern from BMP
/// characters, not just "more of the same range."
///
/// Has no single-byte or single-code-unit representation (astral
/// characters are inherently a surrogate *pair*, decoded as a whole scalar
/// value before any filter ever sees them), so `allows_u8`/`allows_u16`
/// always return `false`; only `allows_char` is meaningful here.
#[inline]
pub(crate) fn allows_u8(_b: u8) -> bool {
    false
}

#[inline]
pub(crate) fn allows_u16(_u: u16) -> bool {
    false
}

#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    ('\u{20000}'..='\u{2A6DF}').contains(&ch)
}