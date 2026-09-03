mod ascii;
mod cjkpunct;
mod cyrillic;
mod hangul;
mod hiragana;
#[cfg(test)]
pub(crate) mod jis;
#[cfg(not(test))]
mod jis;
mod kanji;
mod kanji_jis1;
mod kanji_jis2;
mod katakana;
mod latin1;
mod kanji_ext_b;
mod printable;

use clap::ValueEnum;
use std::sync::Arc;

/// Which characters count as part of a candidate string. Like
/// `InputEncoding`, this is a closed set selected from the CLI -- but
/// unlike `InputEncoding`, more than one can be active at once (see
/// `Config::filter`, a `FilterSet` compiled from a `Vec<CharacterFilter>`):
/// a character is admitted if *any* selected filter allows it.
///
/// # Which scanners this actually affects
///
/// `--filter` is **not** a global "only show me these characters" switch.
/// It exists to solve one specific problem -- false positives in encodings
/// that cannot validate themselves -- and so it applies only where that
/// problem exists:
///
/// * `scanner::utf16le` -- **the main consumer.** Any even-aligned byte
///   pair is a syntactically valid UTF-16LE code unit, so without a
///   character-class restriction this scanner would report enormous
///   amounts of binary noise. Narrowing to the scripts actually expected
///   (e.g. `kanji,hiragana,katakana`) is what makes its output usable.
/// * `scanner::ascii` and `scanner::utf16le_ascii` -- used only to pick
///   between "printable ASCII" and "printable ASCII + Latin-1 supplement"
///   (i.e. `ascii` vs `ascii,latin1`). The CJK filters have no single-byte
///   representation and no effect on `scanner::ascii` at all.
/// * `scanner::win1251` -- **the other genuine consumer.** windows-1251
///   performs no structural validation whatsoever (every one of the 256
///   bytes is independently a character, 223 of them printable), so like
///   the UTF-16LE scanner it depends entirely on the filter to separate
///   text from binary noise. `--filter ascii,cyrillic` is the combination
///   that finds Russian text; `ascii` alone makes it behave like
///   `scanner::ascii`.
///
///   Note this scanner filters on the *decoded character*
///   (`FilterSet::allows_char`), not on the raw byte. That is forced by
///   the encoding: whether byte 0xC0 is a Cyrillic letter depends on the
///   codepage, so only a character-oriented filter can answer the
///   question. It is also why `Cyrillic::allows_u8` is always false --
///   see `filter::cyrillic`.
/// * `scanner::utf8`, the CJK scanners built on `scanner::dbcs`
///   (`scanner::cp932`, `scanner::gbk`, `scanner::gb18030`,
///   `scanner::euckr`, `scanner::big5`), and `scanner::iso2022jp` --
///   **deliberately unaffected.** All of them
///   validate structurally while decoding (UTF-8 by its own
///   well-formedness rules; the `dbcs` set by lead/trail ranges plus
///   an `encoding_rs` lookup per sequence; ISO-2022-JP by its
///   escape-sequence state machine), so none of them has a false-positive
///   problem for `--filter` to fix. Applying the filter there would mean
///   that dropping `ascii` (a perfectly reasonable thing to do to quiet
///   the UTF-16LE scanner) would also silently stop these scanners from
///   matching plain ASCII text. `scanner::utf8` and `scanner::iso2022jp`
///   therefore judge ASCII with `filter::is_ascii_char`, reached
///   unconditionally rather than through the user's selection, while each
///   `dbcs` scanner has its own `is_single` predicate
///   (`cp932::is_valid_ascii_or_kana` additionally covers half-width
///   katakana). Their wider characters are judged only on whether
///   they would corrupt the line-oriented output.
///
/// # Adding a new filter
///
/// 1. Add a variant here.
/// 2. Add a `filter/<name>.rs` module implementing `allows_u8`/`allows_u16`/
///    `allows_char`.
/// 3. Add one match arm in each dispatch function below.
///
/// Note that step 3 is all that's needed even though scanners no longer
/// call those dispatch functions on their hot paths: `FilterSet` builds its
/// lookup tables *by calling them*, so a new filter is picked up
/// automatically with no separate table-construction code to keep in sync.
///
/// `#[derive(ValueEnum)]` is what lets `clap` parse this directly from a CLI
/// argument (e.g. `--filter ascii,latin1`), so adding a variant here
/// automatically exposes it as a new accepted CLI value with no separate
/// parsing code needed.
///
/// Not every filter is meaningful for every scanner (e.g. `Kanji` has no
/// single-byte representation, so it's a no-op for `scanner::ascii`; see
/// each filter module's `allows_u8` for specifics). This is intentionally
/// not enforced by the CLI -- an unhelpful combination just has no effect,
/// rather than being a hard error -- since which filters are useful for
/// which scanner is guidance, not a strict constraint that's worth the
/// added validation complexity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CharacterFilter {
    /// 96 points. Printable ASCII plus tab.
    ///
    /// Single-byte, so it works with every encoding that consults
    /// filters at all.
    ///
    /// See "Which scanners this actually affects" above.
    Ascii,
    /// 96 points. U+00A0-U+00FF, accented Latin and symbols.
    ///
    /// The only other single-byte filter, so the only other one that
    /// does anything for -e ascii and -e utf16le-ascii.
    ///
    /// That single-byte form is ISO-8859-1's, which maps byte N directly
    /// to U+00N in this range.
    ///
    /// Note that under `-e windows-1251` it admits only the 15 bytes
    /// whose windows-1251 meaning happens to coincide with ISO-8859-1
    /// (0xA0, 0xA4, 0xA6, 0xA7, 0xA9, 0xAB-0xAE, 0xB0, 0xB1, 0xB5-0xB7,
    /// 0xBB) -- punctuation and symbols, no letters.
    Latin1,
    /// 256 points. U+0400-U+04FF, Russian and its neighbours.
    ///
    /// The one non-Latin filter that also applies to -e windows-1251.
    ///
    /// Unlike `Latin1`, this has *no* single-byte form and so does
    /// nothing for `scanner::ascii`: which byte means which Cyrillic
    /// letter depends entirely on the codepage (byte 0xC0 is U+0410 in
    /// windows-1251 but U+044E in KOI8-R), so that mapping lives in the
    /// scanner's table rather than here.
    Cyrillic,
    /// 27,584 points. Han U+4E00-U+9FFF + Ext A U+3400-U+4DBF.
    ///
    /// 42% of the BMP, so under -e utf16le it matches large amounts of
    /// random data -- for Japanese, prefer kanji-jis1.
    ///
    /// BMP only: ideographs outside the BMP (reached via surrogate pairs)
    /// are not covered here. Extension B is covered separately by
    /// `KanjiExtB`; Extensions C onward are not covered at all. See
    /// `filter::jis`'s module doc comment for false-positive
    /// measurements.
    Kanji,
    /// 2,965 points. JIS X 0208 level 1, all 2,136 joyo kanji.
    ///
    /// The kanji filter to reach for: a ninth the width of kanji, for
    /// little loss of recall on ordinary Japanese.
    ///
    /// 第一水準漢字.
    #[value(name = "kanji-jis1")]
    KanjiJis1,
    /// 6,355 points. JIS X 0208 lv 1+2, adds jinmeiyo kanji.
    ///
    /// A superset of kanji-jis1: the added level 2 covers the jinmeiyo
    /// (name) kanji and other rarer forms. Still 4x narrower than kanji.
    ///
    /// 第一・第二水準漢字.
    #[value(name = "kanji-jis2")]
    KanjiJis2,
    /// 42,720 points. Han Ext B, U+20000-U+2A6DF.
    ///
    /// The only script filter reaching outside the BMP, so it matches
    /// only via surrogate pairs under -e utf16le. (`Printable` also
    /// admits astral scalars, but as a catch-all rather than a script.)
    ///
    /// Astral scalars have no `u16` form, so this filter is meaningful
    /// only through `allows_char`. It is also what makes
    /// `FilterSet::has_astral` non-trivial -- see that type's doc.
    KanjiExtB,
    /// 91 points. U+3041-U+3096 and U+309B-U+309F.
    ///
    /// Deliberately not the whole U+3040-U+309F block; see
    /// `filter::hiragana`.
    Hiragana,
    /// 96 points. U+30A0-U+30FF, fullwidth only.
    ///
    /// Halfwidth katakana lives in cjkpunct, and the Phonetic
    /// Extensions block (U+31F0-U+31FF) is not covered.
    ///
    /// See `filter::katakana`.
    Katakana,
    /// 11,585 points. Syllables U+AC00-U+D7A3 + 3 jamo blocks.
    ///
    /// The jamo blocks are U+1100-U+11FF, U+3130-U+318F and halfwidth
    /// U+FFA0-U+FFDC.
    Hangul,
    /// 196 points. CJK punct, fullwidth ASCII, halfwidth kana.
    ///
    /// U+3000-U+303F and U+FF00-U+FFEF pruned down to what actually
    /// occurs in Japanese text; fullwidth currency is in here too.
    ///
    /// Deliberately excludes three groups that the raw block boundaries
    /// would otherwise admit and that were observed as unrenderable boxes
    /// in real output: halfwidth *hangul* jamo (U+FFA0-U+FFDC),
    /// unassigned code points, and halfwidth box-drawing/arrow forms.
    /// Use `CjkPunctAll` if you need those (e.g. for Korean material).
    #[value(name = "cjkpunct")]
    CjkPunct,
    /// 304 points. All of U+3000-U+303F and U+FF00-U+FFEF.
    ///
    /// The same two blocks cjkpunct draws from, unpruned: this adds
    /// halfwidth hangul jamo and unassigned points. Prefer cjkpunct
    /// unless you need the jamo; the rest renders as boxes in most
    /// fonts.
    ///
    /// This is what `CjkPunct` used to mean.
    #[value(name = "cjkpunct-all")]
    CjkPunctAll,
    /// 253,592 points. All but controls, surrogates, private.
    ///
    /// The opposite of every other filter: instead of naming a script, it
    /// admits anything that could plausibly be a character, for the
    /// workflow where pstrings is a first pass and the real selection
    /// happens downstream. Newline stays excluded so records cannot be
    /// split; tab is allowed, as in ascii.
    ///
    /// 87% of the BMP, so under -e utf16le roughly half of any random
    /// binary region matches at the default --min-length 4. Raising
    /// --min-length matters far more here than with a narrow filter.
    /// Combining it with another filter has no effect -- it is already a
    /// superset. See `filter::printable`.
    Printable,
}

/// Whether a single byte (used by byte-oriented scanners, e.g. ASCII) is
/// allowed by *any* of `filters`.
///
/// This is the byte-oriented counterpart to `allows_u16`/`allows_char`
/// below: it answers "does this raw file byte pass the selected filters?"
///
/// Scanners do *not* call this directly on their hot paths -- they go
/// through `FilterSet`, which precomputes the answer for all 256 bytes
/// once. This remains the single source of truth those tables are built
/// from (and is what tests assert against), so per-filter logic still lives
/// in exactly one place. Most filters have no single-byte representation
/// and simply never match here (see each filter module's `allows_u8`);
/// `Latin1` is the one non-ASCII exception.
#[inline]
pub(crate) fn allows_u8(filters: &[CharacterFilter], b: u8) -> bool {
    filters.iter().any(|&filter| match filter {
        CharacterFilter::Ascii => ascii::allows_u8(b),
        CharacterFilter::Latin1 => latin1::allows_u8(b),
        CharacterFilter::Cyrillic => cyrillic::allows_u8(b),
        CharacterFilter::Kanji => kanji::allows_u8(b),
        CharacterFilter::KanjiJis1 => kanji_jis1::allows_u8(b),
        CharacterFilter::KanjiJis2 => kanji_jis2::allows_u8(b),
        CharacterFilter::KanjiExtB => kanji_ext_b::allows_u8(b),
        CharacterFilter::Hiragana => hiragana::allows_u8(b),
        CharacterFilter::Katakana => katakana::allows_u8(b),
        CharacterFilter::Hangul => hangul::allows_u8(b),
        CharacterFilter::CjkPunct | CharacterFilter::CjkPunctAll => cjkpunct::allows_u8(b),
        CharacterFilter::Printable => printable::allows_u8(b),
    })
}

/// Whether a single UTF-16 code unit (used by UTF-16LE-family scanners) is
/// allowed by *any* of `filters`.
#[inline]
pub(crate) fn allows_u16(filters: &[CharacterFilter], u: u16) -> bool {
    filters.iter().any(|&filter| match filter {
        CharacterFilter::Ascii => ascii::allows_u16(u),
        CharacterFilter::Latin1 => latin1::allows_u16(u),
        CharacterFilter::Cyrillic => cyrillic::allows_u16(u),
        CharacterFilter::Kanji => kanji::allows_u16(u),
        CharacterFilter::KanjiJis1 => kanji_jis1::allows_u16(u),
        CharacterFilter::KanjiJis2 => kanji_jis2::allows_u16(u),
        CharacterFilter::KanjiExtB => kanji_ext_b::allows_u16(u),
        CharacterFilter::Hiragana => hiragana::allows_u16(u),
        CharacterFilter::Katakana => katakana::allows_u16(u),
        CharacterFilter::Hangul => hangul::allows_u16(u),
        CharacterFilter::CjkPunct => cjkpunct::allows_u16(u),
        CharacterFilter::CjkPunctAll => cjkpunct::allows_u16_all(u),
        CharacterFilter::Printable => printable::allows_u16(u),
    })
}

/// Whether a single decoded `char` (used by `scanner::win1251`, which
/// decodes through a codepage table, and by `scanner::utf16le` for
/// astral-plane characters decoded from a surrogate pair) is allowed by
/// *any* of `filters`.
///
/// `KanjiExtB` and `Printable` are the only filters admitting
/// astral-plane (outside-BMP) scalars -- U+20000-U+2A6DF and planes 1-3
/// respectively; every other filter is a BMP-range check. So unless one
/// of those two is selected, `scanner::utf16le` never emits an astral
/// character at all. This is intentional: enabling only e.g.
/// `kanji`/`hiragana` doesn't reopen the astral-plane matching surface
/// for `--filter` combinations that never asked for it, and it is what
/// lets `FilterSet` short-circuit on `has_astral`.
#[inline]
pub(crate) fn allows_char(filters: &[CharacterFilter], ch: char) -> bool {
    filters.iter().any(|&filter| match filter {
        CharacterFilter::Ascii => ascii::allows_char(ch),
        CharacterFilter::Latin1 => latin1::allows_char(ch),
        CharacterFilter::Cyrillic => cyrillic::allows_char(ch),
        CharacterFilter::Kanji => kanji::allows_char(ch),
        CharacterFilter::KanjiJis1 => kanji_jis1::allows_char(ch),
        CharacterFilter::KanjiJis2 => kanji_jis2::allows_char(ch),
        CharacterFilter::KanjiExtB => kanji_ext_b::allows_char(ch),
        CharacterFilter::Hiragana => hiragana::allows_char(ch),
        CharacterFilter::Katakana => katakana::allows_char(ch),
        CharacterFilter::Hangul => hangul::allows_char(ch),
        CharacterFilter::CjkPunct => cjkpunct::allows_char(ch),
        CharacterFilter::CjkPunctAll => cjkpunct::allows_char_all(ch),
        CharacterFilter::Printable => printable::allows_char(ch),
    })
}

/// Whether a decoded `char` is printable ASCII (or tab), *independently of
/// which filters the user selected*.
///
/// This exists for `scanner::utf8`, whose single-byte (ASCII-range)
/// candidates are deliberately not subject to `--filter`. UTF-8 is
/// self-synchronizing and its multi-byte sequences are structurally
/// validated by `decode_step`, so a UTF-8 match is already trustworthy
/// without any character-class narrowing -- there is no false-positive
/// problem for `--filter` to solve there. Routing its ASCII bytes through
/// the configured `FilterSet` instead had the surprising consequence that
/// e.g. `--filter kanji` (dropping `ascii` to suppress UTF-16LE noise)
/// would also stop the UTF-8 scanner from matching plain ASCII, silently
/// making UTF-8 output a strict subset of what the user expected.
///
/// See `CharacterFilter`'s doc comment for the division of
/// responsibilities `--filter` is actually meant to express.
#[inline]
pub(crate) fn is_ascii_char(ch: char) -> bool {
    ascii::allows_char(ch)
}

/// Number of `u64` words needed to give every BMP code point (U+0000
/// through U+FFFF) its own bit: 65536 / 64.
///
/// Shared with `filter::jis`, which builds its own bitsets over the same
/// domain with the same `>> 6` / `& 63` indexing, so the two cannot
/// disagree about the layout.
const BMP_WORDS: usize = 1024;

/// The selected filters, precompiled into lookup tables.
///
/// WHY THIS EXISTS
///
/// The `allows_*` functions above take a `&[CharacterFilter]` and answer a
/// query by looping over it, running one range check per selected filter.
/// That's fine for a one-off call, but scanners call these *once per input
/// byte or code unit* -- and since the slice's length and contents are
/// runtime data, the compiler can neither unroll the loop nor constant-fold
/// the range checks. On a multi-gigabyte input that's billions of
/// iterations of a loop whose answer only ever depends on 256 (or 65536)
/// distinct inputs.
///
/// So the answers are computed once, up front, and stored as bitsets:
/// membership then costs one shift, one index, and one mask, with no
/// branching and no dependence on how many filters are selected. Selecting
/// eight filters is exactly as fast as selecting one.
///
/// WHY BITSETS RATHER THAN A COMPACTED RANGE LIST
///
/// A merged/sorted `Vec<RangeInclusive<u16>>` would be smaller, but it
/// would reintroduce a data-dependent loop (or a binary search) per lookup,
/// and its cost would still scale with how fragmented the selected filters'
/// combined coverage happens to be. A bitset is flat: uniform cost, no
/// branches, and -- importantly for future filters -- it can represent
/// *any* subset of its domain, not just a tidy set of contiguous ranges.
/// A new filter covering scattered, non-contiguous code points costs
/// exactly the same to look up as one covering a single range.
///
/// WHY ASTRAL CHARACTERS AREN'T IN A BITSET
///
/// Extending the same treatment to the full Unicode range would need
/// 0x110000 bits (136 KiB), which no longer fits comfortably in L1 cache
/// and would slow down the BMP lookups that actually dominate. Astral
/// scalars are reached only via a surrogate pair -- a rare path -- so
/// `allows_astral_char` keeps the original per-filter dispatch. The `Vec`
/// of selected filters is retained purely to serve that path.
///
/// SIZE
///
/// 8 KiB (BMP) + 32 bytes (bytes) per `Config`, built once per run. The BMP
/// table is small enough to stay resident in L1/L2, and real text touches
/// only a handful of its cache lines (all of ASCII lives in one 64-byte
/// line).
#[derive(Debug)]
pub struct FilterSet {
    /// One bit per byte value: bit N set means byte N is admitted.
    byte_bits: [u64; 4],
    /// One bit per BMP code point: bit N set means the scalar with value
    /// N is admitted.
    /// Boxed so that constructing/cloning a `Config` doesn't memcpy 8 KiB
    /// through the stack.
    bmp_bits: Box<[u64; BMP_WORDS]>,
    /// The originally-selected filters, kept for the astral path only (see
    /// `allows_astral_char`). Shared via `Arc` so `Config::clone` -- which
    /// happens per worker thread -- stays cheap.
    selected: Arc<Vec<CharacterFilter>>,
    /// Whether *any* selected filter admits at least one astral scalar.
    /// Lets `allows_astral_char` reject the overwhelmingly common case
    /// (no astral-capable filter selected) with a single bool test,
    /// instead of walking the filter list to reach the same conclusion.
    has_astral: bool,
}

impl FilterSet {
    /// Builds the lookup tables by evaluating the `allows_*` dispatch
    /// functions across their entire input domains.
    ///
    /// Deliberately derived from those functions rather than from a
    /// hand-maintained list of ranges: adding a filter therefore requires
    /// no changes here at all, and the tables cannot drift out of sync
    /// with the per-filter modules that define the actual rules.
    ///
    /// Cost is 65536 + 256 evaluations, once per run -- microseconds,
    /// against a scan that reads gigabytes.
    pub fn new(filters: Vec<CharacterFilter>) -> Self {
        let mut byte_bits = [0u64; 4];
        for b in 0..=u8::MAX {
            if allows_u8(&filters, b) {
                byte_bits[(b >> 6) as usize] |= 1u64 << (b & 63);
            }
        }

        let mut bmp_bits = Box::new([0u64; BMP_WORDS]);
        for u in 0..=u16::MAX {
            if allows_u16(&filters, u) {
                bmp_bits[(u >> 6) as usize] |= 1u64 << (u & 63);
            }
        }

        // Whether *any* selected filter admits *any* scalar outside the
        // BMP. This is determined exhaustively rather than by sampling:
        // a stride-based probe would silently miss a future filter whose
        // astral range is narrower than the stride, and that would be a
        // false *negative* -- `allows_astral_char` short-circuits on this
        // flag, so a missed range means those characters stop matching
        // entirely. An exhaustive walk of planes 1-16 is 1,048,576
        // iterations of a handful of range comparisons, done once per
        // process at startup, and `any` short-circuits the moment a hit
        // is found (which is the case whenever an astral filter *is*
        // selected, so the full walk only happens in the cheap-to-be-
        // wrong direction). Every scalar in planes 1-16 is a valid
        // `char` -- surrogates live in the BMP -- so `from_u32` never
        // fails here, but it is used rather than an unchecked cast to
        // keep this free of unsafe assumptions.
        let has_astral = (0x10000u32..=0x10FFFF)
            .filter_map(char::from_u32)
            .any(|ch| allows_char(&filters, ch));

        FilterSet {
            byte_bits,
            bmp_bits,
            selected: Arc::new(filters),
            has_astral,
        }
    }

    /// Whether a raw file byte is admitted. Branch-free table lookup;
    /// replaces `allows_u8` on scanner hot paths.
    #[inline(always)]
    pub(crate) fn allows_u8(&self, b: u8) -> bool {
        self.byte_bits[(b >> 6) as usize] >> (b & 63) & 1 != 0
    }

    /// Whether a UTF-16 code unit is admitted. Branch-free table lookup;
    /// replaces `allows_u16` on scanner hot paths.
    ///
    /// Note this is defined over *all* `u16` values, including the
    /// surrogate range (0xD800-0xDFFF). No filter admits a surrogate --
    /// they aren't characters -- so those bits are always clear, and
    /// callers that handle surrogate pairs structurally (see
    /// `scanner::utf16le::decode_char_at`) do so before ever reaching
    /// here.
    #[inline(always)]
    pub(crate) fn allows_u16(&self, u: u16) -> bool {
        self.bmp_bits[(u >> 6) as usize] >> (u & 63) & 1 != 0
    }

    /// Whether a decoded `char` is admitted, for any scalar value.
    ///
    /// BMP characters take the bitset path; astral ones fall back to
    /// per-filter dispatch (see the type's doc comment). Used by
    /// `scanner::win1251`, which decodes each byte through a codepage
    /// table, and by `scanner::utf16le` for characters decoded from a
    /// surrogate pair -- i.e. the callers that have a `char` in hand
    /// rather than a raw byte or code unit.
    #[inline(always)]
    pub(crate) fn allows_char(&self, ch: char) -> bool {
        let scalar = ch as u32;
        if scalar <= 0xFFFF {
            self.allows_u16(scalar as u16)
        } else {
            self.allows_astral_char(ch)
        }
    }

    /// Whether an astral-plane (non-BMP) scalar is admitted.
    ///
    /// Not covered by the bitset, and deliberately so: astral characters
    /// only
    /// arise from a surrogate pair (UTF-16LE) or a 4-byte sequence
    /// (UTF-8), both rare in practice, so this trades speed for the 136
    /// KiB a full-Unicode bitset would have cost. `has_astral` still
    /// makes the common case a single bool test.
    #[inline]
    fn allows_astral_char(&self, ch: char) -> bool {
        self.has_astral && allows_char(&self.selected, ch)
    }
}

impl Clone for FilterSet {
    /// `Config` is cloned per worker thread, so this copies the 8 KiB BMP
    /// table rather than rebuilding it (65536 dispatch calls). The
    /// selected-filter list is `Arc`-shared and costs only a refcount
    /// bump.
    fn clone(&self) -> Self {
        FilterSet {
            byte_bits: self.byte_bits,
            bmp_bits: self.bmp_bits.clone(),
            selected: Arc::clone(&self.selected),
            has_astral: self.has_astral,
        }
    }
}


pub const DEFAULT_FILTERS: &[CharacterFilter] = &[CharacterFilter::Ascii];
