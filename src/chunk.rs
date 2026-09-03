//! Splitting the input file into fixed-size byte ranges ("chunks"), the
//! unit of parallelism for the whole scan.
//!
//! Every worker thread takes one chunk at a time and scans it for every
//! selected encoding independently, so chunk size is the main knob
//! trading off parallelism against per-chunk overhead. This module owns
//! that decision: the `Chunk` type itself, the `--chunk-size` parser, and
//! the "auto" heuristic used when the user does not pick a size.
//!
//! Chunks are a purely positional split, made with no regard for
//! character boundaries -- a chunk edge routinely falls in the middle of a
//! multi-byte character. Reassembling what that cuts apart is not handled
//! here at all; it is the scanners' and `outputter`'s job (see
//! `scanner::dbcs`'s module doc comment for the hardest version of that
//! problem).

/// How many chunks the "auto" heuristic aims to give each worker.
///
/// More chunks than workers is deliberate: chunks vary widely in how long
/// they take (a chunk full of matches does far more work than a chunk of
/// zeroes), so handing each worker several smaller units lets a thread
/// that finishes early pick up more work instead of idling while one
/// unlucky thread finishes a huge chunk.
const AUTO_CHUNKS_PER_WORKER: u64 = 8;

/// Floor on the auto-selected chunk size.
///
/// Below this, per-chunk fixed costs start to dominate: each chunk means a
/// temp file per encoding, a merge pass over them, and a
/// boundary-reassembly step. This is a *hard* floor -- it wins even when
/// honouring it means some workers get no chunk at all (see
/// `auto_chunk_size`), on the grounds that a small file is fast regardless
/// of how well it parallelizes.
pub(crate) const AUTO_MIN_CHUNK_SIZE: u64 = 16 * 1024 * 1024;

/// Ceiling on the auto-selected chunk size.
///
/// Matches are streamed out to temp files rather than accumulated, so this
/// is not about total memory. Two other things scale with chunk size: a
/// worker's in-progress run buffer, which in the worst case (a chunk that
/// is entirely matching text) grows to the size of the chunk itself; and
/// the granularity of the work queue, since a very large chunk is an
/// indivisible unit one thread must finish alone.
const AUTO_MAX_CHUNK_SIZE: u64 = 256 * 1024 * 1024;

/// One contiguous byte range of the input file, identified by absolute
/// file offset.
///
/// Deliberately just a range and not a buffer: scanners read their own
/// bytes from the file with positioned reads (see `scanner::read_at_once`)
/// rather than being handed data, so chunks can be processed concurrently
/// against one shared `File` with no seeking and no shared cursor.
#[derive(Debug)]
pub struct Chunk {
    /// Absolute offset of the first byte, from the start of the file.
    pub(crate) offset: u64,
    /// Length in bytes. The final chunk of a file is short: callers clamp
    /// it against the file length rather than letting it run past EOF.
    pub(crate) len: u64,
}

impl Chunk {
    pub fn new(offset: u64, len: u64) -> Self {
        Chunk { offset, len }
    }
}

/// Largest power of two that is `<= x`, or 0 for `x == 0`.
fn floor_power_of_two(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }

    1u64 << (63 - x.leading_zeros())
}

/// Picks a chunk size for `--chunk-size auto`, given the file length and
/// the worker count.
///
/// The target is `file_len / (jobs * AUTO_CHUNKS_PER_WORKER)`, rounded
/// *down* to a power of two and clamped to
/// `[AUTO_MIN_CHUNK_SIZE, AUTO_MAX_CHUNK_SIZE]`. Rounding down rather than
/// to the nearest power of two errs toward more, smaller chunks, which is
/// the safer direction for load balancing. Both bounds are themselves
/// powers of two, so clamping preserves that property -- as does the
/// halving below.
///
/// # Why the trailing loop
///
/// Flooring to a power of two can overshoot badly: a file just under a
/// power-of-two multiple of the target can land on a size that yields
/// fewer chunks than there are workers, leaving threads with nothing to
/// do. The loop halves until there is at least one chunk per worker.
///
/// It stops at `AUTO_MIN_CHUNK_SIZE` even if workers are still left idle,
/// which is intentional rather than an oversight: with a small file and a
/// high `--jobs`, full occupancy would mean chunks so small that the
/// per-chunk overhead costs more than the parallelism gains. See
/// `auto_chunk_size_does_not_go_below_minimum_for_many_jobs` in
/// `src/tests/chunk_tests.rs`, which pins that.
pub fn auto_chunk_size(file_len: u64, jobs: usize) -> u64 {
    if file_len == 0 {
        return AUTO_MIN_CHUNK_SIZE;
    }

    let jobs = jobs.max(1) as u64;

    let target = file_len / (jobs * AUTO_CHUNKS_PER_WORKER);

    let mut size = floor_power_of_two(target.max(1)).clamp(AUTO_MIN_CHUNK_SIZE, AUTO_MAX_CHUNK_SIZE);

    // Prefer at least one chunk per worker, but never go below
    // AUTO_MIN_CHUNK_SIZE.
    while size > AUTO_MIN_CHUNK_SIZE && file_len.div_ceil(size) < jobs {
        size /= 2;
    }

    size
}

/// Parses a byte count with an optional binary unit suffix, as accepted by
/// `--chunk-size`.
///
/// A single trailing `K`, `M`, `G` or `T` (either case) multiplies by the
/// corresponding power of 1024 -- these are binary units, so `1K` is 1024
/// bytes, not 1000. With no suffix the value is a plain byte count.
/// Surrounding whitespace is ignored.
///
/// # Errors
///
/// Returns a human-readable message, suitable for printing straight to the
/// user, if the input is empty, if the numeric part does not parse as a
/// `u64`, or if applying the multiplier would overflow `u64`.
///
/// Note this does not reject 0, and does not clamp to the file length;
/// `main::build_config` does both, since only it knows the file length.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size".into());
    }

    let (num, mul) = match s.as_bytes().last().copied() {
        Some(b'k') | Some(b'K') => (&s[..s.len() - 1], 1024u64),
        Some(b'm') | Some(b'M') => (&s[..s.len() - 1], 1024u64 * 1024),
        Some(b'g') | Some(b'G') => (&s[..s.len() - 1], 1024u64 * 1024 * 1024),
        Some(b't') | Some(b'T') => (&s[..s.len() - 1], 1024u64 * 1024 * 1024 * 1024),
        _ => (s, 1),
    };

    let n: u64 = num.parse().map_err(|_| format!("invalid size: {s}"))?;

    n.checked_mul(mul)
        .ok_or_else(|| format!("size overflow: {s}"))
}