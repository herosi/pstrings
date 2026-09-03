//! Tests for `CharacterFilter::Printable` (`filter::printable`).
//!
//! `Printable` is the first filter that *widens* rather than narrows: it
//! exists for the workflow where pstrings is a first pass and the real
//! selection happens downstream, so it admits everything that could
//! plausibly be a character instead of naming a script.
//!
//! That makes it the odd one out in two ways worth guarding:
//!
//! * It is the second filter to admit astral scalars (after `KanjiExtB`),
//!   so it must make `FilterSet::has_astral` true. Several doc comments
//!   used to assert that `KanjiExtB` was the only such filter, and
//!   `allows_astral_char` short-circuits on that flag -- a wrong answer
//!   here means astral characters silently stop matching.
//! * It is a superset of every other filter, so combining it with one
//!   must be a no-op. If that ever stops holding, one of the two
//!   definitions has drifted.
//!
//! The exact point counts are asserted rather than described, because the
//! `--help` text quotes them and they are easy to invalidate by accident.

use crate::filter::{self, CharacterFilter, FilterSet};

/// Every exclusion `filter::printable` claims to make, checked at the
/// boundary on both sides. A range check that is off by one at either end
/// would pass a "does it reject 0x00" style test but fail here.
#[test]
fn printable_excludes_exactly_the_documented_ranges() {
    let f = [CharacterFilter::Printable];
    let allows = |u: u16| filter::allows_u16(&f, u);

    // C0 controls, except tab.
    assert!(!allows(0x0000));
    assert!(!allows(0x0008));
    assert!(allows(0x0009), "tab is admitted, as in `ascii`");
    assert!(!allows(0x000A), "LF would split an output record");
    assert!(!allows(0x000D), "CR would split an output record");
    assert!(!allows(0x001F));
    assert!(allows(0x0020), "space is the first printable");

    // DEL and the C1 block.
    assert!(allows(0x007E));
    assert!(!allows(0x007F));
    assert!(!allows(0x0080));
    assert!(!allows(0x009F));
    assert!(allows(0x00A0), "Latin-1 supplement starts here");

    // Surrogates: the UTF-16 mechanism, not characters.
    assert!(allows(0xD7FF));
    assert!(!allows(0xD800));
    assert!(!allows(0xDFFF));

    // Private Use Area.
    assert!(!allows(0xE000));
    assert!(!allows(0xF8FF));
    assert!(allows(0xF900), "CJK Compatibility Ideographs");

    // Noncharacters: the reserved run, and the last two of the plane.
    assert!(allows(0xFDCF));
    assert!(!allows(0xFDD0));
    assert!(!allows(0xFDEF));
    assert!(allows(0xFDF0));
    assert!(allows(0xFFFD), "U+FFFD REPLACEMENT CHARACTER is a character");
    assert!(!allows(0xFFFE));
    assert!(!allows(0xFFFF));
}

/// Unassigned code points inside the BMP are deliberately admitted -- see
/// `filter::printable`'s "What is deliberately not excluded". This pins
/// that decision down so it cannot be quietly reversed: doing so would
/// mean hardcoding the Unicode category table.
#[test]
fn printable_admits_unassigned_code_points() {
    let f = [CharacterFilter::Printable];

    // U+0378 and U+0379 are unassigned holes in the Greek block, and
    // U+05EB-U+05EE are unassigned in Hebrew. All are ordinary code
    // points as far as this filter is concerned.
    assert!(filter::allows_u16(&f, 0x0378));
    assert!(filter::allows_u16(&f, 0x0379));
    assert!(filter::allows_u16(&f, 0x05EB));
}

/// Above the BMP only planes 1-3 are admitted. Plane 14 holds nothing but
/// invisible tag characters and variation selectors, planes 4-13 are
/// entirely unassigned, and planes 15-16 are private use.
#[test]
fn printable_admits_planes_1_to_3_only() {
    let f = [CharacterFilter::Printable];
    let allows = |c: u32| filter::allows_char(&f, char::from_u32(c).unwrap());

    assert!(allows(0x10000), "plane 1 starts here");
    assert!(allows(0x20000), "plane 2, CJK Ext B");
    assert!(allows(0x3FFFD), "last usable scalar of plane 3");

    // Each of planes 1-3 still reserves its final two scalars.
    assert!(!allows(0x1FFFE));
    assert!(!allows(0x1FFFF));
    assert!(!allows(0x2FFFE));
    assert!(!allows(0x3FFFE));
    assert!(!allows(0x3FFFF));

    assert!(!allows(0x40000), "plane 4 is entirely unassigned");
    assert!(!allows(0xE0001), "plane 14 holds only invisible characters");
    assert!(!allows(0xF0000), "plane 15 is private use");
    assert!(!allows(0x10FFFD), "plane 16 is private use");
}

/// Single bytes carry no encoding, so there is nothing wider to admit than
/// `ascii` plus `latin1`. Asserted against those two filters rather than
/// restating their ranges, so the three cannot drift apart.
#[test]
fn printable_matches_ascii_plus_latin1_on_bytes() {
    let printable = [CharacterFilter::Printable];
    let both = [CharacterFilter::Ascii, CharacterFilter::Latin1];

    for b in 0u8..=u8::MAX {
        assert_eq!(
            filter::allows_u8(&printable, b),
            filter::allows_u8(&both, b),
            "0x{b:02X}: printable disagrees with ascii+latin1"
        );
    }
}

/// The three predicates must agree wherever their domains overlap. A `char`
/// in the BMP and the `u16` with the same value are the same character, so
/// a mismatch means one of the three arms of `filter::printable` is wrong.
#[test]
fn printable_predicates_agree_across_widths() {
    let f = [CharacterFilter::Printable];

    for b in 0u8..=u8::MAX {
        // Bytes are ISO-8859-1, so byte N is U+00N -- but only where the
        // byte form is meaningful. Above 0x7F the code unit and the byte
        // agree here because both ranges are the Latin-1 supplement.
        assert_eq!(
            filter::allows_u8(&f, b),
            filter::allows_u16(&f, u16::from(b)),
            "0x{b:02X}: allows_u8 vs allows_u16 disagree"
        );
    }

    for u in 0u16..=u16::MAX {
        // Surrogates are not scalars, so they have no `char` to compare.
        if (0xD800..=0xDFFF).contains(&u) {
            continue;
        }
        let ch = char::from_u32(u32::from(u)).unwrap();
        assert_eq!(
            filter::allows_u16(&f, u),
            filter::allows_char(&f, ch),
            "U+{u:04X}: allows_u16 vs allows_char disagree"
        );
    }
}

/// The counts quoted in `--help` and in `filter::printable`'s doc comment,
/// measured rather than asserted by hand. These are the numbers a user
/// reads to judge how noisy `-e utf16le -f printable` will be, so they are
/// worth keeping honest.
#[test]
fn printable_admits_the_documented_number_of_code_points() {
    let f = [CharacterFilter::Printable];

    let bmp = (0..=u16::MAX).filter(|&u| filter::allows_u16(&f, u)).count();
    assert_eq!(bmp, 56_990, "BMP count quoted in the docs");

    let astral = (0x10000u32..=0x10FFFF)
        .filter_map(char::from_u32)
        .filter(|&ch| filter::allows_char(&f, ch))
        .count();
    assert_eq!(astral, 196_602, "planes 1-3, less their noncharacters");

    assert_eq!(bmp + astral, 253_592, "total quoted in --help");
}

/// `Printable` is a superset of every other filter, so ORing it with one
/// must change nothing. This is what justifies telling users that
/// combining it is pointless -- and it doubles as a check that no other
/// filter admits something `Printable` rejects, which would mean that
/// filter admits a control character, a surrogate or a private-use scalar.
#[test]
fn printable_is_a_superset_of_every_other_filter() {
    for other in [
        CharacterFilter::Ascii,
        CharacterFilter::Latin1,
        CharacterFilter::Cyrillic,
        CharacterFilter::Kanji,
        CharacterFilter::KanjiJis1,
        CharacterFilter::KanjiJis2,
        CharacterFilter::KanjiExtB,
        CharacterFilter::Hiragana,
        CharacterFilter::Katakana,
        CharacterFilter::Hangul,
        CharacterFilter::CjkPunct,
        CharacterFilter::CjkPunctAll,
    ] {
        let one = [other];
        let printable = [CharacterFilter::Printable];

        for u in 0u16..=u16::MAX {
            if filter::allows_u16(&one, u) {
                assert!(
                    filter::allows_u16(&printable, u),
                    "{other:?} admits U+{u:04X} but printable does not"
                );
            }
        }
    }
}

/// `FilterSet` compiles the predicates into bitsets; if the two disagree
/// the scanners are using a different rule than the one under test above.
/// Checked exhaustively over both domains, as `filter_latin1_tests` does.
#[test]
fn filterset_tables_match_the_predicates() {
    let f = vec![CharacterFilter::Printable];
    let set = FilterSet::new(f.clone());

    for b in 0u8..=u8::MAX {
        assert_eq!(
            set.allows_u8(b),
            filter::allows_u8(&f, b),
            "0x{b:02X}: FilterSet byte table disagrees"
        );
    }

    for u in 0u16..=u16::MAX {
        assert_eq!(
            set.allows_u16(u),
            filter::allows_u16(&f, u),
            "U+{u:04X}: FilterSet BMP table disagrees"
        );
    }
}

/// `allows_astral_char` short-circuits on `has_astral`, so a filter that
/// admits astral scalars but leaves the flag false would match nothing
/// above the BMP. `KanjiExtB` used to be the only filter this mattered
/// for; `Printable` is the second.
///
/// Exercised through the public `FilterSet::allows_char`, which routes
/// astral scalars to that path -- so a false `has_astral` shows up here
/// as an astral character being rejected.
#[test]
fn printable_sets_has_astral() {
    let set = FilterSet::new(vec![CharacterFilter::Printable]);

    assert!(set.allows_char('\u{20000}'));
    assert!(!set.allows_char('\u{40000}'), "plane 4 is excluded");
    assert!(!set.allows_char('\u{100000}'), "plane 16 is private");
}
