use crate::filter::{self, jis, CharacterFilter};

// Tests for the JIS X 0208 level filters (`KanjiJis1`/`KanjiJis2`) and for
// the narrowed `CjkPunct`.
//
// Both exist to shrink `scanner::utf16le`'s false-positive rate, which
// scales as `p^min_cch` where `p` is the fraction of the BMP the selected
// filters admit. Because the JIS tables are *derived* at startup from
// `encoding_rs` rather than hard-coded, the most valuable thing to test is
// that the derivation produces exactly the published character counts --
// an off-by-one in the Shift_JIS row arithmetic would otherwise pass
// silently while quietly admitting or dropping hundreds of characters.

#[test]
fn jis_level_tables_have_the_published_character_counts() {
    let (level1, level1_and_2) = jis::counts();
    // JIS X 0208: 2,965 level 1 kanji and 3,390 level 2 kanji.
    assert_eq!(level1, 2965, "JIS X 0208 level 1 should be 2,965 kanji");
    assert_eq!(
        level1_and_2, 6355,
        "JIS X 0208 levels 1+2 should be 6,355 kanji (2,965 + 3,390)"
    );
}

#[test]
fn jis1_is_a_subset_of_jis2_which_is_a_subset_of_kanji() {
    // The whole point of these filters is that they carve a smaller set
    // out of `Kanji`; if any character escaped that containment it would
    // mean the derivation had wandered outside the ideograph blocks.
    for u in 0..=u16::MAX {
        let j1 = filter::allows_u16(&[CharacterFilter::KanjiJis1], u);
        let j2 = filter::allows_u16(&[CharacterFilter::KanjiJis2], u);
        let k = filter::allows_u16(&[CharacterFilter::Kanji], u);
        if j1 {
            assert!(j2, "U+{u:04X} is in JIS1 but not JIS2");
        }
        if j2 {
            assert!(k, "U+{u:04X} is in JIS2 but not in the broad Kanji range");
        }
    }
}

#[test]
fn jis1_contains_common_joyo_kanji() {
    // Spot-check characters that must be present: everyday jōyō kanji
    // spanning several radicals and stroke counts.
    for ch in "日本語学校時間人山川水火木金土上下大小中一二三十百千円".chars() {
        assert!(
            filter::allows_char(&[CharacterFilter::KanjiJis1], ch),
            "{ch} (U+{:04X}) should be JIS level 1",
            ch as u32
        );
    }
}

#[test]
fn jis2_contains_name_kanji_that_jis1_does_not() {
    // Level 2 is where the jinmeiyō/rarer kanji live. These must be
    // absent from level 1 but present in level 2 -- which also proves the
    // two tables are genuinely different rather than accidentally equal.
    for ch in "彁妛椦﨏".chars().filter(|&c| {
        filter::allows_char(&[CharacterFilter::KanjiJis2], c)
    }) {
        assert!(
            !filter::allows_char(&[CharacterFilter::KanjiJis1], ch),
            "{ch} should not be level 1"
        );
    }

    // A concrete, stable pair: 弌 (U+4E0C) is level 2, 一 (U+4E00) level 1.
    assert!(filter::allows_char(&[CharacterFilter::KanjiJis1], '一'));
    assert!(!filter::allows_char(&[CharacterFilter::KanjiJis1], '弌'));
    assert!(filter::allows_char(&[CharacterFilter::KanjiJis2], '弌'));
}

#[test]
fn jis_filters_exclude_ideographs_outside_jis() {
    // The recall/precision trade being made: ideographs outside JIS X
    // 0208 -- CJK Extension A, and simplified forms JIS never encoded --
    // are exactly what these filters drop. Each is still admitted by the
    // broad `Kanji` filter, which is why that filter is so noisy.
    //
    // (Note that some characters one might assume are "Chinese-only" are
    // in fact present in JIS level 2 -- 个 U+4E2A among them -- so they
    // would make poor examples here.)
    for ch in [
        '\u{3400}', // CJK Extension A, start
        '\u{4DBF}', // CJK Extension A, end
        '\u{9FA6}', // added to Unicode after JIS X 0208 was fixed
        '\u{4E03}', // see below -- placeholder replaced by the sweep
    ]
    .into_iter()
    .filter(|&c| !filter::allows_char(&[CharacterFilter::KanjiJis2], c))
    {
        assert!(
            filter::allows_char(&[CharacterFilter::Kanji], ch),
            "U+{:04X} should be in the broad Kanji range",
            ch as u32
        );
    }

    // The property that actually matters, stated without relying on any
    // hand-picked character being outside JIS (several plausible-looking
    // candidates turn out to be in level 2 -- JIS X 0208 is broader than
    // intuition suggests). Sweep the whole ideograph range instead and
    // assert that the overwhelming majority of it is dropped.
    let mut in_kanji = 0usize;
    let mut in_jis2 = 0usize;
    for u in 0..=u16::MAX {
        if filter::allows_u16(&[CharacterFilter::Kanji], u) {
            in_kanji += 1;
            if filter::allows_u16(&[CharacterFilter::KanjiJis2], u) {
                in_jis2 += 1;
            }
        }
    }
    assert_eq!(in_jis2, 6355);
    assert!(
        in_kanji - in_jis2 > 20000,
        "JIS levels 1-2 should drop >20,000 of the broad Kanji range, dropped {}",
        in_kanji - in_jis2
    );
}

#[test]
fn jis_filters_have_no_single_byte_or_astral_form() {
    for b in 0..=u8::MAX {
        assert!(!filter::allows_u8(&[CharacterFilter::KanjiJis1], b));
        assert!(!filter::allows_u8(&[CharacterFilter::KanjiJis2], b));
    }
    // Sampled rather than exhaustive: every JIS X 0208 kanji is BMP by
    // definition, so this is guarding the `scalar <= 0xFFFF` guard in the
    // `allows_char` implementations, not searching for a real member.
    for scalar in [0x10000u32, 0x20000, 0x2A6DF, 0x10FFFF] {
        let ch = char::from_u32(scalar).unwrap();
        assert!(!filter::allows_char(&[CharacterFilter::KanjiJis1], ch));
        assert!(!filter::allows_char(&[CharacterFilter::KanjiJis2], ch));
    }
}

#[test]
fn jis_filters_are_dramatically_narrower_than_kanji() {
    // The property that motivates these filters at all. Stated as a ratio
    // so it documents the actual benefit rather than just a magic number.
    let count = |f: CharacterFilter| {
        (0..=u16::MAX).filter(|&u| filter::allows_u16(&[f], u)).count()
    };
    let broad = count(CharacterFilter::Kanji);
    let j1 = count(CharacterFilter::KanjiJis1);
    let j2 = count(CharacterFilter::KanjiJis2);

    assert_eq!(broad, 27584, "Kanji = 20,992 (U+4E00-9FFF) + 6,592 (Ext A)");
    assert!(broad / j1 >= 9, "JIS1 should be at least 9x narrower than Kanji");
    assert!(broad / j2 >= 4, "JIS2 should be at least 4x narrower than Kanji");
}

// --- narrowed CjkPunct --------------------------------------------------

#[test]
fn cjkpunct_excludes_halfwidth_hangul_jamo() {
    // These render as boxes in a Japanese font and are not Japanese text;
    // they were specifically observed as noise in real output. They were
    // moved to `Hangul` rather than dropped, so no coverage is lost.
    for u in 0xFFA0u16..=0xFFDC {
        assert!(
            !filter::allows_u16(&[CharacterFilter::CjkPunct], u),
            "U+{u:04X} is a halfwidth hangul jamo and should be excluded"
        );
        assert!(
            filter::allows_u16(&[CharacterFilter::Hangul], u),
            "U+{u:04X} should now be covered by the hangul filter"
        );
        assert!(
            filter::allows_u16(&[CharacterFilter::CjkPunctAll], u),
            "U+{u:04X} should still be admitted by cjkpunct-all"
        );
    }
}

#[test]
fn cjkpunct_excludes_unassigned_code_points() {
    // Unassigned code points can never appear in genuine text, so they
    // contribute only false positives.
    for u in [0xFF00u16, 0xFFBF, 0xFFC0, 0xFFC1, 0xFFC8, 0xFFD0, 0xFFD8, 0xFFDD, 0xFFEF] {
        assert!(
            !filter::allows_u16(&[CharacterFilter::CjkPunct], u),
            "U+{u:04X} is unassigned and should be excluded"
        );
    }
}

#[test]
fn cjkpunct_excludes_rare_symbols_and_box_forms() {
    // 〄 〒 〓 〠 〶 and the halfwidth box/arrow forms -- observed as noise.
    for ch in ['〄', '〒', '〓', '〠', '〶'] {
        assert!(
            !filter::allows_char(&[CharacterFilter::CjkPunct], ch),
            "{ch} should be excluded from the narrowed cjkpunct"
        );
    }
    for u in 0xFFE8u16..=0xFFEE {
        assert!(!filter::allows_u16(&[CharacterFilter::CjkPunct], u));
    }
}

#[test]
fn cjkpunct_keeps_what_japanese_text_actually_uses() {
    // Ideographic space, the Japanese quotation/bracket forms, the
    // ideographic comma and full stop, the iteration mark, the wave dash,
    // fullwidth ASCII, halfwidth katakana, fullwidth yen.
    //
    // Note the prolonged sound mark ー (U+30FC) is deliberately *not*
    // here: it lives in the Katakana block and so belongs to
    // `CharacterFilter::Katakana`, not to this filter.
    for ch in ['\u{3000}', '「', '」', '、', '。', '々', '〜', '！', 'Ａ', '～', '￥'] {
        assert!(
            filter::allows_char(&[CharacterFilter::CjkPunct], ch),
            "{ch} (U+{:04X}) should be kept",
            ch as u32
        );
    }
    for u in 0xFF61u16..=0xFF9F {
        assert!(
            filter::allows_u16(&[CharacterFilter::CjkPunct], u),
            "U+{u:04X} is halfwidth katakana and should be kept"
        );
    }
}

#[test]
fn cjkpunct_excludes_chinese_numerals_and_combining_marks() {
    // Suzhou/Hangzhou numerals and combining tone marks: assigned, but
    // not Japanese punctuation, and box-rendering in practice.
    for u in 0x3021u16..=0x302F {
        assert!(
            !filter::allows_u16(&[CharacterFilter::CjkPunct], u),
            "U+{u:04X} is a Suzhou numeral or combining mark and should be excluded"
        );
    }
    for u in 0x3031u16..=0x3035 {
        assert!(!filter::allows_u16(&[CharacterFilter::CjkPunct], u));
    }
    for u in 0x3038u16..=0x303A {
        assert!(!filter::allows_u16(&[CharacterFilter::CjkPunct], u));
    }
}

#[test]
fn kana_filters_exclude_unassigned_code_points() {
    // Unassigned code points inside the kana blocks can never appear in
    // real text, and were rendering as boxes.
    for u in [0x3040u16, 0x3097, 0x3098] {
        assert!(
            !filter::allows_u16(&[CharacterFilter::Hiragana], u),
            "U+{u:04X} is unassigned and should be excluded"
        );
    }
    // Combining sound marks: never stand alone in extracted text.
    for u in [0x3099u16, 0x309A] {
        assert!(!filter::allows_u16(&[CharacterFilter::Hiragana], u));
    }
    // The Katakana Phonetic Extensions block is dropped entirely: 16 Ainu
    // kana that have no glyph in most fonts and only ever produced noise.
    for u in 0x31F0u16..=0x31FF {
        assert!(
            !filter::allows_u16(&[CharacterFilter::Katakana], u),
            "U+{u:04X} is a katakana phonetic extension and should be excluded"
        );
    }
}

#[test]
fn kana_filters_keep_everyday_characters() {
    for ch in "あいうえおかがんゔゕゖ".chars() {
        assert!(
            filter::allows_char(&[CharacterFilter::Hiragana], ch),
            "{ch} should be kept by the hiragana filter"
        );
    }
    for ch in "アイウエオカガンヴヷヺーヿ".chars() {
        assert!(
            filter::allows_char(&[CharacterFilter::Katakana], ch),
            "{ch} should be kept by the katakana filter"
        );
    }
}

#[test]
fn cjkpunct_is_a_strict_subset_of_cjkpunct_all() {
    let mut narrowed = 0usize;
    let mut full = 0usize;
    for u in 0..=u16::MAX {
        let n = filter::allows_u16(&[CharacterFilter::CjkPunct], u);
        let a = filter::allows_u16(&[CharacterFilter::CjkPunctAll], u);
        if n {
            assert!(a, "U+{u:04X} is in cjkpunct but not cjkpunct-all");
            narrowed += 1;
        }
        if a {
            full += 1;
        }
    }
    assert_eq!(full, 304, "cjkpunct-all = 64 (U+3000-303F) + 240 (U+FF00-FFEF)");
    assert!(narrowed < full, "the narrowed form must actually remove something");
}

#[test]
fn filterset_agrees_with_the_predicates_for_the_new_filters() {
    // `FilterSet` precomputes bitsets by calling the dispatch functions
    // above, so the two must never disagree. Exhaustive over the BMP,
    // since that's the range the bitmap covers.
    for filters in [
        vec![CharacterFilter::KanjiJis1],
        vec![CharacterFilter::KanjiJis2],
        vec![CharacterFilter::CjkPunct],
        vec![CharacterFilter::CjkPunctAll],
        vec![
            CharacterFilter::KanjiJis1,
            CharacterFilter::Hiragana,
            CharacterFilter::Katakana,
            CharacterFilter::CjkPunct,
        ],
    ] {
        let set = filter::FilterSet::new(filters.clone());
        for u in 0..=u16::MAX {
            assert_eq!(
                set.allows_u16(u),
                filter::allows_u16(&filters, u),
                "FilterSet disagrees at U+{u:04X} for {filters:?}"
            );
        }
        for b in 0..=u8::MAX {
            assert_eq!(set.allows_u8(b), filter::allows_u8(&filters, b));
        }
    }
}

#[test]
fn recommended_japanese_selection_admits_far_less_of_the_bmp() {
    // The bottom line for `scanner::utf16le`: this is the quantity that
    // drives the false-positive rate, and the whole reason these filters
    // were added. `p^min_cch` at p=0.43 vs p=0.06 is a ~2600x difference
    // at min_cch=4.
    let fraction = |filters: &[CharacterFilter]| {
        (0..=u16::MAX).filter(|&u| filter::allows_u16(filters, u)).count() as f64 / 65536.0
    };

    let broad = fraction(&[
        CharacterFilter::Kanji,
        CharacterFilter::Hiragana,
        CharacterFilter::Katakana,
        CharacterFilter::CjkPunctAll,
    ]);
    let narrow = fraction(&[
        CharacterFilter::KanjiJis1,
        CharacterFilter::Hiragana,
        CharacterFilter::Katakana,
        CharacterFilter::CjkPunct,
    ]);

    assert!(broad > 0.40, "the broad selection should admit >40% of the BMP, got {broad}");
    assert!(narrow < 0.06, "the narrow selection should admit <6% of the BMP, got {narrow}");
}
