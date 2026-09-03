/// CJK punctuation and fullwidth forms, restricted to characters that
/// actually occur in Japanese text.
///
/// # Why this is narrower than the blocks it comes from
///
/// The obvious definition -- "all of U+3000-U+303F plus all of
/// U+FF00-U+FFEF" -- turns out to admit a lot of things that are neither
/// punctuation nor Japanese, and those were observed in real output as
/// unrenderable boxes:
///
/// * **U+FFA0-U+FFDC are halfwidth *hangul* jamo**, not Japanese at all,
///   and have no glyph in a typical Japanese font. These have been moved
///   to `filter::hangul`, where they belong, rather than dropped.
/// * **U+FF00, U+FFBF-U+FFC1, U+FFC8-U+FFC9, U+FFD0-U+FFD1,
///   U+FFD8-U+FFD9, U+FFDD-U+FFDF are unassigned.** Admitting unassigned
///   code points is strictly harmful: they can never appear in genuine
///   text, so they contribute only false positives.
/// * **U+FFE8-U+FFEE are halfwidth box-drawing and arrow forms**
///   (￨￩￪￫￬￭￮), not punctuation.
/// * **Roughly half of U+3000-U+303F is not Japanese punctuation** (31 of
///   its 64 code points are excluded): it holds
///   Suzhou/Hangzhou numerals used in Chinese accounting
///   (U+3021-U+3029, U+3038-U+303A), combining tone marks
///   (U+302A-U+302F), Hangul tone marks, and rare vertical-writing repeat
///   marks (U+3031-U+3035). Also excluded are a handful of symbols that
///   are assigned but vanishingly rare in running text and were
///   specifically observed as noise: U+3004 JIS symbol 〄,
///   U+3012-U+3013 postal mark and geta 〒〓, U+3020 postal mark face 〠,
///   U+3036 circled postal mark 〶, and U+303E-U+303F.
///
/// Every code point removed here shrinks `scanner::utf16le`'s
/// false-positive rate, which scales as `p^min_cch` -- so even small
/// reductions compound. See `filter::jis`'s module doc comment for the
/// wider argument about why filter breadth matters so much for that
/// scanner.
///
/// `CharacterFilter::CjkPunctAll` keeps the original unrestricted
/// behavior for anyone who needs it (e.g. scanning Korean or Chinese
/// material, where the halfwidth jamo are genuinely wanted).
///
/// Has no single-byte representation, so `allows_u8` always returns
/// `false`; this filter only ever matters for the UTF-16LE scanners.
#[inline]
pub(crate) fn allows_u8(_b: u8) -> bool {
    false
}

/// The unrestricted form: both blocks in full. Kept so
/// `CharacterFilter::CjkPunctAll` has something to call, and so the
/// narrowing above reads as an explicit, reviewable difference from the
/// raw block boundaries rather than as an unrelated set of ranges.
#[inline]
pub(crate) fn allows_u16_all(u: u16) -> bool {
    (0x3000..=0x303F).contains(&u) || (0xFF00..=0xFFEF).contains(&u)
}

#[inline]
pub(crate) fn allows_u16(u: u16) -> bool {
    match u {
        // --- CJK Symbols and Punctuation (U+3000-U+303F) ---------------
        //
        // Admitted individually rather than as a range, because only
        // about half of this block is punctuation Japanese text actually
        // uses. The rest is Suzhou/Hangzhou numerals (Chinese
        // accounting), combining tone marks, Hangul tone marks, and rare
        // vertical-writing repeat marks -- all of which were observed as
        // boxes in real output.
        0x3000            // ideographic space
        | 0x3001..=0x3003 // 、。〃 comma, full stop, ditto
        | 0x3005..=0x3011 // 々 iteration mark through 【】 lenticular brackets
        | 0x3014..=0x301F // 〔-〛 brackets, 〜 wave dash, 〝〞〟 quotes
        | 0x3030          // 〰 wavy dash
        | 0x303B..=0x303D // 〻〼〽 iteration/masu/part-alternation marks
        => true,

        // U+3004 〄 JIS symbol, U+3012-U+3013 〒〓 postal mark and geta,
        // U+3020 〠 postal mark face, U+3021-U+302F numerals and
        // combining marks, U+3031-U+303A repeat marks and Hangzhou
        // numerals (including U+3036 〶 circled postal mark),
        // U+303E-U+303F variation indicator and fill space.
        0x3001..=0x303F => false,

        // --- Halfwidth and Fullwidth Forms (U+FF00-U+FFEF) -------------

        // Fullwidth ASCII (！ through ～): the bulk of what this filter
        // is for, and by far the most common in real Japanese text.
        0xFF01..=0xFF5E => true,

        // Halfwidth katakana and its punctuation (｡｢｣､･ and ｦ
        // through ﾟ). Note `Katakana` covers the *fullwidth* block
        // U+30A0-U+30FF; these halfwidth forms live here instead,
        // alongside the other halfwidth/fullwidth forms.
        0xFF61..=0xFF9F => true,

        // Fullwidth currency and symbols: ￠￡￢￣￤￥. U+FFE6 ￦ (won
        // sign) is excluded as Korean, matching the halfwidth jamo now
        // living in `Hangul`.
        0xFFE0..=0xFFE5 => true,

        // Everything else in U+FF00-U+FFEF: halfwidth hangul jamo (moved
        // to `Hangul`), unassigned code points, and halfwidth box-drawing
        // and arrow forms.
        _ => false,
    }
}

#[inline]
pub(crate) fn allows_char(ch: char) -> bool {
    let scalar = ch as u32;
    scalar <= 0xFFFF && allows_u16(scalar as u16)
}

#[inline]
pub(crate) fn allows_char_all(ch: char) -> bool {
    let scalar = ch as u32;
    scalar <= 0xFFFF && allows_u16_all(scalar as u16)
}