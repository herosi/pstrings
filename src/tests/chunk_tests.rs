use crate::chunk::{auto_chunk_size, AUTO_MIN_CHUNK_SIZE};

// Tests for `auto_chunk_size`, which picks a chunk size (in bytes) from
// (file_len, job_count) when the user hasn't specified one explicitly.
//
// `chunk.rs` itself isn't available alongside this test file, but the
// exact numbers below are consistent with a single underlying rule:
//
//   desired_chunks = jobs * 16
//   target         = ceil(file_len / desired_chunks)
//   chunk_size     = next_power_of_two(target), then
//                    clamped to [AUTO_MIN_CHUNK_SIZE, 256 MiB]
//
// i.e. roughly "aim for about 16 chunks per worker, rounded up to a
// power-of-two chunk size for friendly buffer sizing, but never smaller
// than `AUTO_MIN_CHUNK_SIZE` (16 MiB, per `auto_chunk_size_has_minimum`)
// or larger than 256 MiB (per the saturation seen in
// `auto_chunk_size_is_clamped`/the third case of
// `auto_chunk_size_scales_with_file_size`)." The comments below describe
// each test's intent based on that inferred rule; they don't reflect
// confirmed internal names or implementation from `chunk.rs` itself.

#[test]
fn auto_chunk_size_scales_with_file_size() {
    // Fixed jobs=8, increasing file size: chunk size grows in power-of-two
    // steps as the file gets bigger (64 MiB -> 128 MiB), until the largest
    // case (100 GiB) saturates at the 256 MiB upper bound -- the "raw"
    // target for that case would be much larger than 256 MiB, so this case
    // is implicitly also exercising the max clamp, confirmed explicitly by
    // `auto_chunk_size_is_clamped` below.
    let mib = 1024 * 1024;
    assert_eq!(auto_chunk_size(5 * 1024 * mib + 512 * mib, 8), 64 * mib);
    assert_eq!(auto_chunk_size(10 * 1024 * mib, 8), 128 * mib);
    assert_eq!(auto_chunk_size(100 * 1024 * mib, 8), 256 * mib);
}

#[test]
fn auto_chunk_size_is_clamped() {
    // Exercises both ends of the clamp range with the same jobs count (8):
    // a tiny 1 MiB file would otherwise compute a tiny chunk size, but
    // must floor out at the minimum (16 MiB) -- and a comically huge
    // 100 TiB file would otherwise compute a huge chunk size, but must
    // ceiling out at the maximum (256 MiB). The middle case (5.5 GiB) is
    // the same "normal, unclamped" value as in the previous test, included
    // here as a sanity midpoint between the two extremes.
    let mib = 1024 * 1024;
    assert_eq!(auto_chunk_size(1 * mib, 8), 16 * mib);
    assert_eq!(auto_chunk_size(5 * 1024 * mib + 512 * mib, 8), 64 * mib);
    assert_eq!(auto_chunk_size(100 * 1024 * 1024 * mib, 8), 256 * mib);
}

#[test]
fn auto_chunk_size_scales_with_jobs() {
    // Fixed file size, varying job count: doubling the job count halves
    // the chunk size (and vice versa), since more workers means each one
    // should get proportionally smaller pieces to keep the total chunk
    // count (and thus parallelism) scaling with the worker count.
    let mib = 1024 * 1024;
    let file_len = 5 * 1024 * mib + 512 * mib;
    assert_eq!(auto_chunk_size(file_len, 4), 128 * mib);
    assert_eq!(auto_chunk_size(file_len, 8), 64 * mib);
    assert_eq!(auto_chunk_size(file_len, 16), 32 * mib);
}

#[test]
fn auto_chunk_size_has_minimum() {
    // Narrower, more targeted version of one case from
    // `auto_chunk_size_is_clamped`: pins down `AUTO_MIN_CHUNK_SIZE`'s
    // actual value (16 MiB) as its own explicit, independent assertion
    // rather than relying on the reader to infer it from the clamp test.
    let mib = 1024 * 1024;
    assert_eq!(auto_chunk_size(1 * mib, 8), 16 * mib);
}

#[test]
fn auto_chunk_size_prefers_at_least_one_chunk_per_worker() {
    // A case where the minimum clamp kicks in (file_len=256 MiB, jobs=16)
    // but still happens to leave at least one chunk available per worker:
    // chunk_size clamps down to AUTO_MIN_CHUNK_SIZE, and the resulting
    // chunk count (`file_len.div_ceil(chunk_size)`) is still >= jobs, so
    // every worker can get at least one chunk to process. This is the
    // "good" outcome of the minimum clamp -- contrast with the next test,
    // where the same clamp produces the opposite result.
    let file_len = 256 * 1024 * 1024;
    let jobs = 16;
    let chunk_size = auto_chunk_size(file_len, jobs);
    assert_eq!(chunk_size, AUTO_MIN_CHUNK_SIZE);
    assert!((file_len.div_ceil(chunk_size) as usize) >= jobs);
}

#[test]
fn auto_chunk_size_does_not_go_below_minimum_for_many_jobs() {
    // The flip side of the previous test: with a smaller file (62 MiB) and
    // more jobs (32), the minimum clamp still applies, but this time it's
    // NOT enough chunks to give every worker one (`div_ceil` here is 4,
    // well under 32 jobs). This documents, as an accepted trade-off rather
    // than a bug, that `AUTO_MIN_CHUNK_SIZE` is a hard floor: for small
    // enough files, requesting more jobs than the file can usefully be
    // split into at that floor simply leaves some workers idle, rather
    // than shrinking chunks further to keep everyone busy.
    let file_len = 62 * 1024 * 1024;
    let jobs = 32;
    let chunk_size = auto_chunk_size(file_len, jobs);
    assert_eq!(chunk_size, AUTO_MIN_CHUNK_SIZE);
    assert!((file_len.div_ceil(chunk_size) as usize) < jobs);
}