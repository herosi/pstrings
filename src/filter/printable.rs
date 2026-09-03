/// Everything that could plausibly be a character, for users who want to
/// pull out all the text and narrow it down themselves afterwards.
///
/// This is the deliberate opposite of every other filter in this module.
/// The rest exist to *narrow* -- they name a script or a block and admit
/// only that -- because `--filter`'s whole purpose is to keep the
/// self-unvalidating scanners (`scanner::utf16le` and `scanner::win1251`)
/// from reporting binary noise as text. This one gives that up on purpose,
/// for the workflow where pstrings is a first pass and the real selection
/// happens downstream in grep, a script, or a human reading the output.
///
/// # What is excluded, and why
///
/// The exclusions are only the things that are not characters at all, or
/// that would break the output:
///
/// * **C0 and C1 controls** (U+0000-U+001F, U+007F-U+009F), except tab.
///   Newline is excluded for the same reason `filter::ascii` excludes it:
///   the output is one record per line (`offset\tencoding\ttext`), so an
///   admitted newline would split a record in half and corrupt the format.
///   Tab is admitted to match `filter::ascii`, which has always allowed it.
/// * **Surrogates** (U+D800-U+DFFF). Not characters -- they are the
///   UTF-16 encoding mechanism itself. `scanner::utf16le` pairs them into
///   an astral scalar before consulting any filter, so an unpaired
///   surrogate reaching here means the data was not text.
/// * **Private use** (U+E000-U+F8FF in the BMP, and planes 15-16). By
///   definition these have no assigned meaning outside a private
///   agreement, so admitting them would add 137,468 code points of pure
///   noise for no benefit to anyone who does not already know they are
///   there.
/// * **Noncharacters** (U+FDD0-U+FDEF and the U+xFFFE/U+xFFFF pairs).
///   Permanently reserved as never-a-character by Unicode.
/// * **Planes 4-13 and 14.** Planes 4-13 are entirely unassigned, so
///   nothing is lost. Plane 14 holds only 337 assigned code points, all
///   of them invisible: 97 deprecated tag characters (category `Cf`) and
///   240 variation selectors (`Mn`), which modify a preceding character
///   rather than printing anything of their own.
///
/// # What is deliberately *not* excluded
///
/// Unassigned code points inside planes 0-3 are admitted. Excluding them
/// would mean hardcoding the Unicode category table -- 345 separate
/// ranges, which would silently go stale every time Unicode assigns new
/// characters, exactly the kind of maintenance burden the rest of this
/// module avoids. Keeping them costs 1,463 code points in the BMP, 2.2%
/// of it, which is negligible against a filter that already admits 87%.
///
/// # Cost
///
/// 253,592 code points: 56,990 in the BMP (87% of it) and 196,602 across
/// planes 1-3. Under `-e utf16le` that means roughly 87% of all possible
/// code units are admitted, so with the default `--min-length 4` about
/// half of any random binary region will be reported as a match. That is
/// the intended trade -- but it is why this filter is not the default, and
/// why raising `--min-length` matters much more here than with a narrow
/// filter.
///
/// Combining this with another filter is pointless: it is a superset of
/// every other filter except the private-use and unassigned-astral
/// regions none of them cover either, so `-f printable,kanji` admits
/// exactly what `-f printable` alone does.

/// Single bytes, for the byte-oriented scanners. This is exactly
/// `ascii` plus `latin1` (192 of the 256 byte values): printable ASCII,
/// tab, and the Latin-1 supplement, excluding both control blocks.
///
/// A raw byte carries no encoding, so there is nothing wider to admit --
/// `scanner::ascii` with this filter behaves as `-f ascii,latin1` does.
#[inline]
pub(crate) fn allows_u8(b: u8) -> bool {
    b == b'\t' || (0x20..=0x7E).contains(&b) || (0xA0..=0xFF).contains(&b)
}

/// BMP code units, for `scanner::utf16le`. Admits 56,990 of the 65,536.
///
/// Note the surrogate rejection here is not what stops astral characters
/// from matching: `scanner::utf16le` recognises a well-formed surrogate
/// pair and decodes it before asking any filter, so pairs are judged by
/// `allows_char` below. This arm only rejects *unpaired* surrogates.
#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    match u {
        // Tab first: it falls inside the C0 range rejected just below.
        0x09 => true,
        // C0 controls, DEL and C1 controls.
        0x0000..=0x001F | 0x007F..=0x009F => false,
        // Surrogates -- an encoding mechanism, not characters.
        0xD800..=0xDFFF => false,
        // Private Use Area.
        0xE000..=0xF8FF => false,
        // Noncharacters: the Arabic Presentation Forms-A block reserves
        // U+FDD0-U+FDEF, and every plane reserves its last two scalars.
        0xFDD0..=0xFDEF | 0xFFFE | 0xFFFF => false,
        _ => true,
    }
}

/// Decoded scalars, for `scanner::win1251` and for the astral characters
/// `scanner::utf16le` builds from a surrogate pair.
///
/// BMP scalars defer to `allows_u16`; above the BMP only planes 1-3 are
/// admitted, less their six noncharacters, for 196,602 code points. See
/// the module doc for why planes 4-16 are dropped.
#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    let c = ch as u32;
    if c <= 0xFFFF {
        return allows_u16(c as u16);
    }
    // Planes 1-3 (U+10000-U+3FFFF). The mask rejects U+1FFFE/F, U+2FFFE/F
    // and U+3FFFE/F in one test: a scalar is a noncharacter exactly when
    // its low 16 bits are 0xFFFE or 0xFFFF, and clearing bit 0 maps both
    // onto 0xFFFE.
    c <= 0x3FFFF && (c & 0xFFFE) != 0xFFFE
}
