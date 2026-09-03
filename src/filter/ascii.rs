/// Printable ASCII plus tab. Kept as a direct range comparison on the raw
/// byte/code-unit (no `char` conversion) since scanners call this once per
/// input byte/code-unit; see scanner/ascii.rs and scanner/utf16le_ascii.rs 
/// for why that matters at scale.
///
/// Deliberately excludes other C0 control characters (newline, CR, etc.) as
/// well as anything >= 0x7F (DEL and all high-bit-set bytes), so a "run" of
/// allowed bytes corresponds to a single printable line fragment rather than
/// spanning arbitrary binary/control content -- that's what makes these
/// runs meaningful as candidate human-readable strings.
#[inline]
pub(crate) fn allows_u8(b: u8) -> bool {
    // 0x20..=0x7e is the printable ASCII range (space through '~');
    // tab (0x09) is allowed separately since it's a common, meaningful
    // whitespace character in real strings despite being a control code.
    b == b'\t' || (0x20..=0x7e).contains(&b)
}

/// Same rule as `allows_u8`, but operating on a UTF-16 code unit (as used by
/// `scanner::utf16le`) instead of a raw byte. Note the range checked is
/// identical (0x09, 0x20..=0x7e) since this filter only ever recognizes
/// UTF-16LE code units in the ASCII subset -- see the `u as u8` truncation
/// in `scanner::utf16le_ascii::scan_parity`, which relies on the selected
/// filters never admitting a code unit outside the single-byte-safe range.
#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    u == 0x09 || (0x20..=0x7e).contains(&u)
}

/// Same rule again, this time for a fully-decoded `char`. Deliberately
/// identical in spirit to `allows_u8`: an "Ascii" filter should only admit
/// ASCII characters, regardless of which encoding produced them.
///
/// Note that `scanner::utf8` and `scanner::iso2022jp` reach this function
/// *unconditionally*, through `filter::is_ascii_char`, rather than through
/// the user's `--filter` selection -- they judge their wider characters
/// with their own rules instead. So `--filter ascii` does **not** narrow a
/// UTF-8 scan to ASCII; see `filter::CharacterFilter` for why those
/// scanners are exempt. The scanners that reach this through an actual
/// filter selection are `scanner::win1251` and `scanner::utf16le`.
#[inline]
pub(crate) fn allows_char(c: char) -> bool {
    c == '\t' || ('\u{20}'..='\u{7e}').contains(&c)
}
