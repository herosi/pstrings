use crate::encoding::InputEncoding;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

#[cfg(unix)]
use std::mem::MaybeUninit;

#[derive(Default)]
pub struct DetailedStats {
    scan_by_encoding: Mutex<HashMap<InputEncoding, Duration>>,
    records_by_encoding: Mutex<HashMap<InputEncoding, u64>>,
    emitted_by_encoding: Mutex<HashMap<InputEncoding, u64>>,
    merge_time: Mutex<Duration>,
    output_processing_time: Mutex<Duration>,
    output_wait_time: Mutex<Duration>,
}

impl DetailedStats {
    pub fn scan_by_encoding(&self) -> &Mutex<HashMap<InputEncoding, Duration>> {
        &self.scan_by_encoding
    }

    /// Intermediate records produced by the scanners, summed over every
    /// chunk.
    ///
    /// This is a measure of *scanner work*, not of results. It is
    /// deliberately not the number of lines the tool prints, and the two
    /// diverge sharply as `--chunk-size` shrinks:
    ///
    /// - A string spanning N chunks is emitted as N separate fragments,
    ///   one per chunk, which `outputter` later joins back into a single
    ///   output line. Every extra boundary a string crosses adds one to
    ///   this count without adding anything to the output.
    /// - A fragment touching a chunk boundary is emitted even when it is
    ///   shorter than `--min-length`, because the next chunk may extend it
    ///   past the threshold. If it turns out not to, it is dropped at
    ///   output time -- counted here, never printed.
    ///
    /// So this number legitimately grows without bound as chunks get
    /// smaller, while the output stays identical. Use
    /// `emitted_by_encoding` for "how many strings did this encoding
    /// actually find".
    pub fn records_by_encoding(&self) -> &Mutex<HashMap<InputEncoding, u64>> {
        &self.records_by_encoding
    }

    /// Records actually written to the final output, per encoding.
    ///
    /// Unlike `records_by_encoding`, this is counted after boundary
    /// fragments have been rejoined and after `--min-length` has been
    /// applied to the joined result, so it is invariant under
    /// `--chunk-size` -- the same input always yields the same number
    /// here, which is exactly the property `--chunk-size` is supposed to
    /// have.
    pub fn emitted_by_encoding(&self) -> &Mutex<HashMap<InputEncoding, u64>> {
        &self.emitted_by_encoding
    }

    pub fn merge_time(&self) -> &Mutex<Duration> {
        &self.merge_time
    }

    pub fn output_processing_time(&self) -> &Mutex<Duration> {
        &self.output_processing_time
    }

    pub fn output_wait_time(&self) -> &Mutex<Duration> {
        &self.output_wait_time
    }

    pub fn add_scan_time(&self, encoding: InputEncoding, elapsed: Duration) {
        let mut map = self.scan_by_encoding.lock().unwrap();
        *map.entry(encoding).or_default() += elapsed;
    }

    pub fn add_record_count(&self, encoding: InputEncoding, count: u64) {
        let mut counts = self.records_by_encoding.lock().unwrap();
        *counts.entry(encoding).or_default() += count;
    }

    pub fn add_emitted_count(&self, encoding: InputEncoding, count: u64) {
        let mut counts = self.emitted_by_encoding.lock().unwrap();
        *counts.entry(encoding).or_default() += count;
    }

    pub fn add_merge(&self, elapsed: Duration) {
        *self.merge_time.lock().unwrap() += elapsed;
    }

    pub fn add_output_processing(&self, elapsed: Duration) {
        *self.output_processing_time.lock().unwrap() += elapsed;
    }

    pub fn add_output_wait(&self, elapsed: Duration) {
        *self.output_wait_time.lock().unwrap() += elapsed;
    }
}

#[cfg(windows)]
pub fn peak_rss_bytes() -> u64 {
    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
    }

    let mut counters = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };

    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    if ok != 0 {
        counters.peak_working_set_size as u64
    } else {
        0
    }
}

#[cfg(unix)]
pub fn peak_rss_bytes() -> u64 {
    let mut usage = MaybeUninit::<libc::rusage>::zeroed();
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };

    if result == 0 {
        let usage = unsafe { usage.assume_init() };
        let raw_rss = usage.ru_maxrss;

        if cfg!(target_os = "linux") {
            raw_rss as u64 * 1024
        } else {
            raw_rss as u64
        }
    } else {
        0
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}