use clap::ValueEnum;
use std::io::{self};

/// The set of encodings pstrings can scan for. This enum is intentionally a
/// closed, compile-time-known set (it is also a `clap::ValueEnum`, so it can
/// only ever be chosen from the CLI, never constructed dynamically at
/// runtime). Adding a new encoding means:
///
/// 1. Add a variant here, with the next unused explicit discriminant (see
///    below -- never renumber an existing one), and a `#[value(name =
///    "...")]` if the CLI spelling differs from the variant name.
/// 2. Add an arm to `name`, to `ALL`, to `TryFrom<u16>`, and to
///    `is_self_synchronizing`.
/// 3. Add a `scanner/<name>.rs` module implementing `scan(...)`.
/// 4. Add a `pub(crate) mod <name>;` and one `scanner::scan` match arm in
///    `scanner/mod.rs`.
///
/// Nothing outside this file and `scanner/` needs to change: `merger` and
/// `outputter` are already generic over the set of encodings in play.
///
/// One additional step applies specifically to non-self-synchronizing
/// multi-byte encodings (CP932/Shift_JIS, GBK, GB18030, EUC-KR, Big5,
/// ISO-2022-JP): also add a match arm in `scanner::segment_raw`. Those
/// encodings can't safely decide chunk-boundary character splits at scan
/// time (see
/// scanner/dbcs.rs's module doc comment), so they defer both the split
/// and the decode to `outputter` via `record::RecordData::Raw`, and
/// `scanner::segment_raw` is where `outputter` goes to finish that job once
/// it knows how the fragment resolves. Self-synchronizing encodings
/// (Ascii, Utf16le*, Utf8, Windows1251) never produce `Raw` records and
/// don't need an arm there.
///
/// # Discriminants are explicit, and must stay that way
///
/// The discriminant is not merely an implementation detail: it is written
/// into every intermediate record on disk (see `record::write_record`) and
/// read back by the `TryFrom<u16>` below. The two therefore have to agree
/// exactly.
///
/// With implicit discriminants they silently *stopped* agreeing the moment
/// a variant was inserted anywhere but the end -- adding `Big5` before
/// `Windows1251` renumbered the latter from 8 to 9 while `TryFrom` still
/// mapped 8 to it, so records were written as one encoding and read back
/// as another. The symptom was Cyrillic text emitted under a `BIG5` label,
/// and a `segment_raw` call for `Windows1251` (which is
/// self-synchronizing and has no arm there) hitting `unreachable!`.
///
/// Numbering each variant explicitly makes that class of mistake
/// impossible to make by accident: a new encoding takes the next unused
/// number, existing ones never move, and `encoding_discriminants_round_trip`
/// in `src/tests/record_tests.rs` fails if any variant is missing from
/// `TryFrom`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, ValueEnum)]
#[repr(u16)]
pub enum InputEncoding {
    Ascii = 0,
    Utf16leAscii = 1,
    Utf16le = 2,
    Utf8 = 3,
    Iso2022Jp = 4,
    Cp932 = 5,
    #[value(name = "gbk")]
    Gbk = 6,
    #[value(name = "euc-kr")]
    EucKr = 7,
    #[value(name = "windows-1251")]
    Windows1251 = 8,
    #[value(name = "big5")]
    Big5 = 9,
    #[value(name = "gb18030")]
    Gb18030 = 10,
}

impl InputEncoding {
    /// Every variant, for exhaustive tests.
    ///
    /// Kept next to the enum so that adding a variant and forgetting to
    /// list it here is a visible omission rather than a silent gap in
    /// coverage -- `encoding_discriminants_round_trip` uses this to check
    /// that `TryFrom<u16>` handles every encoding, which is what stops the
    /// enum and its on-disk numbering drifting apart.
    #[cfg(test)]
    pub(crate) const ALL: &'static [InputEncoding] = &[
        Self::Ascii,
        Self::Utf16leAscii,
        Self::Utf16le,
        Self::Utf8,
        Self::Iso2022Jp,
        Self::Cp932,
        Self::Gbk,
        Self::EucKr,
        Self::Windows1251,
        Self::Big5,
        Self::Gb18030,
    ];

    /// Human-readable label used in reporting output (e.g. next to each
    /// match, so the user can tell which encoding a given string was found
    /// as).
    ///
    /// Note that `Utf16leAscii` and `Utf16le` deliberately share the label
    /// `UTF16LE`: they are two scanning strategies for the same encoding,
    /// and the distinction is an input-side choice the user already made,
    /// not a property of the text that was found.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ascii => "ASCII",
            Self::Utf16leAscii => "UTF16LE",
            Self::Utf16le => "UTF16LE",
            Self::Utf8 => "UTF8",
            Self::Iso2022Jp => "ISO2022JP",
            Self::Cp932 => "CP932",
            Self::Gbk => "GBK",
            Self::EucKr => "EUCKR",
            Self::Big5 => "BIG5",
            Self::Windows1251 => "CP1251",
            Self::Gb18030 => "GB18030",
        }
    }

    /// Whether this encoding is self-synchronizing: whether every byte
    /// position unambiguously tells you where a character starts, so a
    /// chunk-boundary fragment can always be decoded immediately at scan
    /// time and joined as plain text (see `record::RecordData`).
    ///
    /// `false` for encodings where a byte's role (lead vs. trail vs.
    /// standalone) can only be determined in the context of what comes
    /// before it -- for those, `scanner` defers both the character-
    /// boundary decision and the decode to `outputter` via
    /// `RecordData::Raw` rather than guessing at scan time.
    pub(crate) fn is_self_synchronizing(self) -> bool {
        match self {
            // Windows1251 is single-byte: one byte is always exactly one
            // character, so a chunk boundary can never fall inside a
            // character and fragments are always decodable at scan time.
            Self::Ascii
            | Self::Utf16leAscii
            | Self::Utf16le
            | Self::Utf8
            | Self::Windows1251 => true,
            Self::Iso2022Jp
            | Self::Cp932
            | Self::Gbk
            | Self::EucKr
            | Self::Big5
            | Self::Gb18030 => false,
        }
    }
}

impl TryFrom<u16> for InputEncoding {
    type Error = io::Error;

    /// The inverse of the explicit discriminants above. `ALL` plus
    /// `encoding_discriminants_round_trip` guarantee this stays complete;
    /// do not renumber, only append.
    fn try_from(value: u16) -> io::Result<Self> {
        match value {
            0 => Ok(Self::Ascii),
            1 => Ok(Self::Utf16leAscii),
            2 => Ok(Self::Utf16le),
            3 => Ok(Self::Utf8),
            4 => Ok(Self::Iso2022Jp),
            5 => Ok(Self::Cp932),
            6 => Ok(Self::Gbk),
            7 => Ok(Self::EucKr),
            8 => Ok(Self::Windows1251),
            9 => Ok(Self::Big5),
            10 => Ok(Self::Gb18030),
            _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown encoding in intermediate record",
                )),
        }
    }
}


pub const DEFAULT_ENCODINGS: &[InputEncoding] = &[InputEncoding::Ascii, InputEncoding::Utf16leAscii];
