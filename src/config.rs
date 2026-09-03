use crate::encoding::InputEncoding;
use crate::filter::{CharacterFilter, FilterSet};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "pstrings", version, about = "Parallel strings extractor for very large files")]
pub struct Args {
    /// Input file.
    input: PathBuf,

    /// Omit the offset and encoding columns, printing only the matched
    /// text.
    #[arg(short = 's', long = "string-only", action = clap::ArgAction::SetTrue, default_value_t = false)]
    str_only: bool,

    /// Output file. [default: stdout].
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// Input encoding(s). Repeat the option or comma-separate to select
    /// multiple; each is scanned independently over the same input.
    /// [default: ascii, utf16le-ascii].
    #[arg(
        short = 'e',
        long = "encoding",
        value_enum,
        value_delimiter = ',',
        verbatim_doc_comment,
        help = "Input encoding(s), repeated or comma-separated. [default: ascii, utf16le-ascii]"
    )]
    encoding: Option<Vec<InputEncoding>>,

    /// Character filter(s): which characters may appear in a match.
    /// Repeat the option or comma-separate to select multiple; a
    /// character is kept if any selected filter allows it.
    /// [default: ascii].
    ///
    /// A "string" in a binary file is a guess, and most encodings can
    /// check the guess themselves: UTF-8 has strict well-formedness
    /// rules, and the CJK multi-byte encodings only accept sequences
    /// their standard assigns. Two cannot. In UTF-16LE any byte pair is a
    /// valid code unit, and in windows-1251 every byte is a character, so
    /// scanning either without restricting *which* characters count would
    /// report most of the file as text. Narrowing pays off sharply: if a
    /// fraction p of characters are admitted, false positives scale as
    /// p^min-length.
    ///
    ///   utf16le, windows-1251   essential, as above
    ///   ascii, utf16le-ascii    only picks ascii vs. ascii,latin1
    ///   all others              ignored (they validate structurally)
    ///
    /// So dropping ascii to quiet utf16le will not silently narrow utf8,
    /// cp932, gbk, gb18030, euc-kr, big5 or iso2022-jp -- they always
    /// match plain ASCII regardless.
    ///
    /// Only ascii and latin1 have a single-byte form and cyrillic is also
    /// wired into windows-1251; every other filter is useful only with
    /// -e utf16le. The three kanji filters are nested, narrowest first:
    /// kanji-jis1, kanji-jis2, kanji.
    ///
    /// printable goes the other way: it admits everything except
    /// controls, surrogates and private use, for pulling out all the text
    /// and narrowing it down afterwards. It admits 87% of the BMP, so
    /// expect roughly half of any random binary region to match at the
    /// default -m 4 -- raise -m well above that when using it.
    ///
    /// EXAMPLES
    ///
    ///   Japanese in a UTF-16LE binary:
    ///     -e utf16le -f kanji-jis1,hiragana,katakana,cjkpunct
    ///
    ///   Russian text in a windows-1251 file:
    ///     -e windows-1251 -f ascii,cyrillic
    ///
    ///   Western European text, single-byte:
    ///     -e ascii -f ascii,latin1
    ///
    ///   Everything, to filter yourself later:
    ///     -e utf16le -f printable -m 12
    #[arg(
        short = 'f',
        long = "filter",
        value_enum,
        value_delimiter = ',',
        verbatim_doc_comment,
        help = "Allowed characters, repeated or comma-separated. [default: ascii]"
    )]
    filter: Option<Vec<CharacterFilter>>,

    /// Minimum number of decoded characters (cch). Must be at least 1.
    #[arg(short = 'm', long = "min-length", default_value_t = 4)]
    min_length: u64,

    /// Number of worker threads. [default: ncpus].
    #[arg(short = 'j', long = "jobs")]
    jobs: Option<usize>,

    /// Chunk size, the unit of parallel work. Accepts K/M/G/T suffixes
    /// (binary, so 1K is 1024), or "auto" to derive one from the input
    /// size and thread count. Clamped to the file size. Must be even when
    /// -e utf16le is selected, so a chunk boundary cannot fall inside a
    /// code unit.
    #[arg(
        short = 'c',
        long = "chunk-size",
        default_value = "auto",
        verbatim_doc_comment,
        help = "Chunk size: K/M/G/T suffixes, or \"auto\""
    )]
    chunk_size: String,

    /// Keep intermediate chunk result files for debugging.
    #[arg(long = "keep-temp")]
    keep_temp: bool,

    /// Directory in which the temporary result directory is created.
    #[arg(long = "temp-dir")]
    temp_dir: Option<PathBuf>,

    /// Print processing statistics, including peak RSS. Same as -v.
    #[arg(long = "stats")]
    stats: bool,

    /// Print processing statistics (as --stats). Repeat (-vv) to add a
    /// per-phase breakdown: scan time and record counts per encoding,
    /// plus merge and output timings.
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    verbose: u8,
}

impl Args {
    pub fn input(&self) -> &PathBuf {
        &self.input
    }

    pub fn str_only(&self) -> bool {
        self.str_only
    }

    pub fn output(&self) -> &Option<PathBuf> {
        &self.output
    }

    pub fn encoding(&self) -> Option<Vec<InputEncoding>> {
        self.encoding.clone()
    }

    pub fn filter(&self) -> Option<Vec<CharacterFilter>> {
        self.filter.clone()
    }

    pub fn min_length(&self) -> u64 {
        self.min_length
    }

    pub fn jobs(&self) -> &Option<usize> {
        &self.jobs
    }

    pub fn chunk_size(&self) -> &String {
        &self.chunk_size
    }

    pub fn keep_temp(&self) -> bool {
        self.keep_temp
    }

    pub fn temp_dir(&self) -> &Option<PathBuf> {
        &self.temp_dir
    }

    pub fn stats(&self) -> bool {
        self.stats
    }

    pub fn verbose(&self) -> u8 {
        self.verbose
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    encodings: Vec<InputEncoding>,
    /// The selected character filters, precompiled into lookup tables.
    /// Built once here rather than consulted as a `Vec<CharacterFilter>`
    /// per input byte -- see `FilterSet`'s doc comment for why.
    filter: FilterSet,
    min_cch: u64,
    jobs: usize,
    chunk_size: u64,
    keep_temp: bool,
    temp_dir: Option<PathBuf>,
    str_only: bool,
}

impl Config {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        encodings: Vec<InputEncoding>,
        filter: Vec<CharacterFilter>,
        min_cch: u64,
        jobs: usize,
        chunk_size: u64,
        keep_temp: bool,
        temp_dir: Option<PathBuf>,
        str_only: bool,
    ) -> Self {
        Config {
            encodings,
            // Compiling the filter set here (rather than storing the raw
            // `Vec` and compiling it per scanner) means every scanner and
            // every worker thread shares one already-built table, and no
            // call site can accidentally skip the optimization.
            filter: FilterSet::new(filter),
            min_cch,
            jobs,
            chunk_size,
            keep_temp,
            temp_dir,
            str_only,
        }
    }

    pub fn encodings(&self) -> &Vec<InputEncoding> {
        &self.encodings
    }

    pub fn filter(&self) -> &FilterSet {
        &self.filter
    }

    pub fn min_cch(&self) -> u64 {
        self.min_cch
    }

    pub fn jobs(&self) -> usize {
        self.jobs
    }

    pub fn chunk_size(&self) -> u64 {
        self.chunk_size
    }

    pub fn keep_temp(&self) -> bool {
        self.keep_temp
    }

    pub fn temp_dir(&self) -> &Option<PathBuf> {
        &self.temp_dir
    }

    pub fn str_only(&self) -> bool {
        self.str_only
    }
}