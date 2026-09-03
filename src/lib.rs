mod chunk;
mod config;
mod encoding;
mod filter;
mod merger;
mod outputter;
mod record;
mod scanner;
mod stats;
mod tempfile_helper;
mod cpu_prio;

#[cfg(test)]
mod tests;

pub use chunk::{auto_chunk_size, parse_size, Chunk};
pub use config::{Args, Config};
pub use encoding::{InputEncoding, DEFAULT_ENCODINGS};
pub use filter::{CharacterFilter, DEFAULT_FILTERS};
pub use merger::merge_chunk_encodings;
pub use outputter::{
    begin_emitted_counts, flush_pending, output_merged_chunk, take_emitted_counts,
    write_output_record,
};
pub use record::MatchRecord;
pub use scanner::scan;
pub use stats::{format_bytes, peak_rss_bytes, DetailedStats};
pub use tempfile_helper::TempDirGuard;
pub use cpu_prio::set_cpu_prio;

/// Buffer size for writes: every `BufWriter` the scanners, merger and
/// outputter wrap around a temp file or the final output stream.
pub const WRITE_BUFFER_SIZE: usize = 8 * 1024 * 1024;

/// Buffer size for reads: the block each scanner pulls from the input
/// file at a time, and the `BufReader` capacity the merger and outputter
/// use when reading temp files back.
pub const READ_BUFFER_SIZE: usize = 8 * 1024 * 1024;
