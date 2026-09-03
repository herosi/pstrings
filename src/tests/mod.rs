//! Test suite, organized to mirror `src/`'s module layout: one file per
//! production module (plus `support.rs` for helpers shared across all of
//! them). When adding a new scanner/filter/etc., its tests belong in the
//! matching file here (or a new one, declared below) rather than growing
//! any single file indefinitely.

mod support;

mod chunk_tests;
mod boundary_rejoin_tests;
mod chunk_size_invariance_tests;
mod filter_latin1_tests;
mod filter_jis_tests;
mod filter_printable_tests;
mod merger_tests;
mod record_count_tests;
mod outputter_tests;
mod record_tests;
mod scanner_ascii_tests;
mod scanner_utf16le_ascii_tests;
mod scanner_utf16le_tests;
mod scanner_utf8_tests;
mod scanner_win1251_tests;
mod scanner_cp932_tests;
mod scanner_dbcs_tests;
mod scanner_iso2022jp_tests;
mod tempfile_tests;
