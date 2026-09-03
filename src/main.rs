use clap::Parser;
use std::{
    cmp::min,
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
    fs::File,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
        mpsc::{sync_channel, RecvTimeoutError, Receiver, SyncSender},
    },
    thread,
};

use pstrings::*;

/// Checks that `path` is a non-empty regular file and returns its length.
fn validate_input_file(path: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(format!("not a regular file: {}", path.display()).into());
    }

    let file_len = metadata.len();
    if file_len == 0 {
        return Err(format!("empty file: {}", path.display()).into());
    }

    Ok(file_len)
}

/// Turns raw CLI args into a validated `Config`, applying defaults (job
/// count, chunk size, encodings) and cross-field checks (e.g. chunk size
/// must be even when UTF-16LE is selected).
fn build_config(args: &Args, file_len: u64) -> Result<Config, Box<dyn std::error::Error>> {
    let jobs = args
        .jobs()
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));

    if jobs == 0 {
        return Err("--jobs must be >= 1".into());
    }

    let chunk_size = if args.chunk_size().eq_ignore_ascii_case("auto") {
        auto_chunk_size(file_len, jobs)
    } else {
        let size = parse_size(&args.chunk_size())?.min(file_len);

        if size == 0 {
            return Err("--chunk-size must be > 0".into());
        }

        size
    };

    let encodings = args.encoding().unwrap_or_else(|| {
        DEFAULT_ENCODINGS.to_vec()
    });

    if encodings.contains(&InputEncoding::Utf16le) && chunk_size % 2 != 0 {
        return Err("--chunk-size must be even when UTF-16LE is enabled".into());
    }

    let filters = args.filter().unwrap_or_else(|| {
        DEFAULT_FILTERS.to_vec()
    });

    if args.min_length() == 0 {
        return Err("--min-length must be >= 1".into());
    }

    Ok(Config::new(
        encodings,
        filters,
        args.min_length(),
        jobs,
        chunk_size,
        args.keep_temp(),
        args.temp_dir().clone(),
        args.str_only(),
    ))
}

/// Installs the Ctrl+C handler and returns the shared flag it sets.
fn install_cancel_handler() -> Result<Arc<AtomicBool>, Box<dyn std::error::Error>> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let handler_flag = Arc::clone(&cancelled);
    ctrlc::set_handler(move || {
        handler_flag.store(true, Ordering::SeqCst);
    })?;
    Ok(cancelled)
}

fn make_output_writer(output_path: &Option<PathBuf>) -> io::Result<Box<dyn Write>> {
    Ok(match output_path {
        Some(path) => Box::new(BufWriter::with_capacity(WRITE_BUFFER_SIZE, File::create(path)?)),
        None => Box::new(BufWriter::with_capacity(WRITE_BUFFER_SIZE, io::stdout())),
    })
}

/// Scans every configured encoding over one chunk and merges the results
/// into a single offset-sorted temp file.
fn process_chunk(
    index: u64,
    file: &File,
    file_len: u64,
    cfg: &Config,
    temp_dir: &Path,
    cancelled: &AtomicBool,
    detailed_stats: &DetailedStats,
) -> io::Result<File> {
    let offset = index * cfg.chunk_size();
    let len = min(cfg.chunk_size(), file_len - offset);
    let chunk = Chunk::new(offset, len);

    let mut files = Vec::new();

    for encoding in cfg.encodings() {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        let path = if cfg.encodings().len() == 1 {
            temp_dir.join(format!("chunk-{:020}.bin", index))
        } else {
            temp_dir.join(format!("chunk-{:020}-{}.bin", index, encoding.name()))
        };

        let scan_started = Instant::now();
        let (record_len, file) = scan(*encoding, file, file_len, &chunk, cfg, &path, cancelled)?;
        detailed_stats.add_scan_time(*encoding, scan_started.elapsed());
        detailed_stats.add_record_count(*encoding, record_len);

        files.push(file);
    }

    let merged_path = temp_dir.join(format!("chunk-{:020}-merged.bin", index));
    let merge_started = Instant::now();
    let merged_file = merge_chunk_encodings(files, &merged_path, cancelled, cfg)?;
    detailed_stats.add_merge(merge_started.elapsed());

    Ok(merged_file)
}

/// Body of one worker thread: repeatedly claims the next chunk index,
/// processes it, and sends the merged result to the outputter. Returns as
/// soon as chunks run out, cancellation is observed, or the outputter has
/// gone away.
#[allow(clippy::too_many_arguments)]
fn worker_loop(
    file: Arc<File>,
    file_len: u64,
    chunk_count: u64,
    cfg: Config,
    temp_dir: PathBuf,
    cancelled: Arc<AtomicBool>,
    next_chunk: Arc<AtomicU64>,
    tx: SyncSender<(u64, File)>,
    detailed_stats: Arc<DetailedStats>,
) -> io::Result<()> {
    loop {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        let index = next_chunk.fetch_add(1, Ordering::Relaxed);
        if index >= chunk_count {
            break;
        }

        let merged_file = process_chunk(index, &file, file_len, &cfg, &temp_dir, &cancelled, &detailed_stats)?;

        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        if tx.send((index, merged_file)).is_err() {
            break;
        }
    }

    Ok(())
}

/// Spawns `cfg.jobs()` worker threads sharing one chunk-index counter and
/// one result channel. If a worker returns an error, it sets `cancelled` so
/// its siblings wind down too.
fn spawn_workers(
    file: Arc<File>,
    file_len: u64,
    chunk_count: u64,
    cfg: Config,
    temp_dir: PathBuf,
    cancelled: Arc<AtomicBool>,
    detailed_stats: Arc<DetailedStats>,
) -> (Vec<thread::JoinHandle<io::Result<()>>>, Receiver<(u64, File)>) {
    let next_chunk = Arc::new(AtomicU64::new(0));
    let (tx, rx): (SyncSender<(u64, File)>, Receiver<(u64, File)>) = sync_channel(cfg.jobs() * 2);

    let mut handles = Vec::with_capacity(cfg.jobs());

    for _ in 0..cfg.jobs() {
        let file = Arc::clone(&file);
        let cancelled = Arc::clone(&cancelled);
        let next_chunk = Arc::clone(&next_chunk);
        let tx = tx.clone();
        let cfg = cfg.clone();
        let temp_dir = temp_dir.clone();
        let detailed_stats = Arc::clone(&detailed_stats);

        handles.push(thread::spawn(move || -> io::Result<()> {
            let result = worker_loop(
                file,
                file_len,
                chunk_count,
                cfg,
                temp_dir,
                Arc::clone(&cancelled),
                next_chunk,
                tx,
                detailed_stats,
            );

            if result.is_err() {
                cancelled.store(true, Ordering::SeqCst);
            }
            result
        }));
    }
    drop(tx);

    (handles, rx)
}

/// Consumes merged chunks from `rx` in chunk order as they become available,
/// writing final output and resolving cross-chunk boundary strings, then
/// flushes any string still pending once the input is exhausted. Records
/// timing into `detailed_stats` and returns the first I/O error encountered
/// (if any) rather than propagating it immediately, since the caller still
/// needs to join the worker threads regardless.
fn run_output_stage(
    rx: Receiver<(u64, File)>,
    chunk_count: u64,
    file_len: u64,
    cfg: &Config,
    output: &mut Box<dyn Write>,
    cancelled: &AtomicBool,
    detailed_stats: &DetailedStats,
) -> Option<io::Error> {
    let mut pending_files = BTreeMap::<u64, File>::new();
    let mut next_output_chunk = 0u64;
    let mut pending_output: HashMap<InputEncoding, MatchRecord> = HashMap::new();
    let output_started = Instant::now();
    let mut output_processing = Duration::ZERO;
    let mut output_error: Option<io::Error> = None;

    while next_output_chunk < chunk_count {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        while !pending_files.contains_key(&next_output_chunk) {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok((index, file)) => {
                    pending_files.insert(index, file);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    output_error = Some(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!(
                            "worker result channel closed before chunk {next_output_chunk} was completed"
                        ),
                    ));
                    cancelled.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }

        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        let merged_file = pending_files.remove(&next_output_chunk).unwrap();
        let output_processing_started = Instant::now();

        if let Err(e) = output_merged_chunk(
            merged_file,
            next_output_chunk * cfg.chunk_size(),
            // The final chunk may be shorter than `chunk_size` when
            // `file_len` isn't an exact multiple of it.
            cfg.chunk_size().min(file_len - next_output_chunk * cfg.chunk_size()),
            &mut pending_output,
            cfg.min_cch(),
            output,
            cfg.str_only(),
            cancelled,
        ) {
            output_error = Some(e);
            cancelled.store(true, Ordering::SeqCst);
            break;
        }

        output_processing += output_processing_started.elapsed();

        next_output_chunk += 1;
    }

    if !cancelled.load(Ordering::Relaxed) {
        let output_processing_started = Instant::now();
        // Delegate to `outputter::flush_pending` rather than draining
        // `pending_output` here.
        //
        // The ordering matters and is easy to get wrong: a `RecordData::
        // Raw` fragment's `cch` is a placeholder `0` (see `RecordData`'s
        // doc comment), because its real character count isn't known until
        // the bytes are decoded and segmented by `scanner::segment_raw`.
        // So the leftovers must be *resolved first and filtered against
        // `min_cch` second*. Filtering first drops every Raw fragment --
        // which is to say, the final trailing run of every
        // non-self-synchronizing encoding, since a run that reaches EOF is
        // necessarily deferred as Raw.
        //
        // `flush_pending` does the resolve-then-filter in the right order,
        // and is the same function every test helper uses.
        if let Err(e) = flush_pending(&mut pending_output, cfg.min_cch(), output, cfg.str_only()) {
            output_error = Some(e);
            cancelled.store(true, Ordering::SeqCst);
        }
        output_processing += output_processing_started.elapsed();
    }

    let output_wall = output_started.elapsed();
    detailed_stats.add_output_processing(output_processing);
    detailed_stats.add_output_wait(output_wall.saturating_sub(output_processing));

    output_error
}

fn join_workers(handles: Vec<thread::JoinHandle<io::Result<()>>>) -> Result<(), Box<dyn std::error::Error>> {
    for handle in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(Box::new(e)),
            Err(_) => return Err("worker thread panicked".into()),
        }
    }
    Ok(())
}

fn print_stats(
    args: &Args,
    cfg: &Config,
    file_len: u64,
    chunk_count: u64,
    started: Instant,
    peak_rss: u64,
    detailed_stats: &DetailedStats,
) {
    eprintln!("stats:");
    eprintln!("  input:       {}", format_bytes(file_len));
    eprintln!("  chunks:      {}", chunk_count);
    eprintln!("  workers:     {}", cfg.jobs());
    eprintln!("  chunk_size:  {}", format_bytes(cfg.chunk_size()));

    let elapsed = started.elapsed();
    let mib_per_sec = if elapsed.as_secs_f64() > 0.0 {
        file_len as f64 / 1024.0 / 1024.0 / elapsed.as_secs_f64()
    } else {
        0.0
    };
    eprintln!("  elapsed:     {:.3} s", elapsed.as_secs_f64());
    eprintln!("  throughput:  {:.2} MiB/s", mib_per_sec);

    if peak_rss != 0 {
        eprintln!("  peak_rss:    {}", format_bytes(peak_rss));
    } else {
        eprintln!("  peak_rss:    unavailable");
    }

    if args.verbose() >= 2 {
        eprintln!("phases:");
        let scan = detailed_stats.scan_by_encoding().lock().unwrap();
        let total_scan: Duration = scan.values().copied().sum();
        eprintln!("  scan (worker time): {:.3} s", total_scan.as_secs_f64());
        for encoding in cfg.encodings() {
            let elapsed = scan.get(encoding).copied().unwrap_or_default();

            let records = detailed_stats.records_by_encoding()
                .lock().unwrap()
                .get(&encoding)
                .copied()
                .unwrap_or(0);

            let emitted = detailed_stats.emitted_by_encoding()
                .lock().unwrap()
                .get(&encoding)
                .copied()
                .unwrap_or(0);

            let sec_per_1m = if records > 0 {
                elapsed.as_secs_f64() * 1024.0 * 1024.0 / records as f64
            } else {
                0.0
            };

            let average_scan: f64 = elapsed.as_secs_f64() / chunk_count as f64;
            // Two counts, because they answer two different questions and
            // only one of them is independent of `--chunk-size`:
            //
            // - `found` is how many strings ended up in the output. This
            //   is the result, and it does not change with `--chunk-size`.
            // - `scanned` is how many intermediate records the scanner
            //   produced across all chunks -- a measure of work, which
            //   grows as chunks shrink because every chunk boundary a
            //   string crosses splits it into another fragment (later
            //   rejoined), and because sub-`--min-length` fragments
            //   touching a boundary must be emitted just in case the next
            //   chunk extends them.
            //
            // Reporting only `scanned` (as this used to) made it look like
            // the tool found wildly different amounts of data at different
            // chunk sizes: 744 vs 45 for UTF-8 on a 2 KiB file, when the
            // output was 44 lines in both cases. The throughput figures
            // stay based on `scanned`, since that is the work actually
            // done.
            eprintln!(
                "    {:<8} {:>10} found  {:>10} scanned  {:.3} s  ({:.3} s / 1M records, average: {:.3} s / chunk)",
                encoding.name(),
                emitted,
                records,
                elapsed.as_secs_f64(),
                sec_per_1m,
                average_scan,
            );
        }

        eprintln!("  merge (worker time): {:.3} s", detailed_stats.merge_time().lock().unwrap().as_secs_f64());
        eprintln!("  output wait:         {:.3} s", detailed_stats.output_wait_time().lock().unwrap().as_secs_f64());
        eprintln!("  output processing:   {:.3} s", detailed_stats.output_processing_time().lock().unwrap().as_secs_f64());

        // Shown alongside the figures it explains, rather than behind a
        // further -v level: the scan and merge lines are sums over
        // workers, so on a parallel run they routinely exceed `elapsed`
        // above, which looks like a bug if unexplained.
        eprintln!("  note: scan and merge are sums of worker times and may exceed wall-clock elapsed time due to parallelism");
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();

    set_cpu_prio();

    let args = Args::parse();

    let file_len = validate_input_file(args.input())?;
    let cfg = build_config(&args, file_len)?;

    let detailed_stats = Arc::new(DetailedStats::default());
    let cancelled = install_cancel_handler()?;

    let temp_guard = TempDirGuard::new(cfg.temp_dir().as_deref(), cfg.keep_temp())?;
    let temp_dir = temp_guard.path().to_path_buf();

    let file = Arc::new(File::open(args.input())?);
    let chunk_count = (file_len + cfg.chunk_size() - 1) / cfg.chunk_size();

    let (handles, rx) = spawn_workers(
        Arc::clone(&file),
        file_len,
        chunk_count,
        cfg.clone(),
        temp_dir,
        Arc::clone(&cancelled),
        Arc::clone(&detailed_stats),
    );

    let mut output = make_output_writer(args.output())?;
    // Only tally output records when they'll actually be shown. When they
    // won't, the counters in `outputter`'s write path short-circuit on an
    // already-`None` check.
    if args.verbose() >= 2 {
        begin_emitted_counts();
    }
    let output_error = run_output_stage(rx, chunk_count, file_len, &cfg, &mut output, &cancelled, &detailed_stats);
    if let Some(counts) = take_emitted_counts() {
        for (encoding, count) in counts {
            detailed_stats.add_emitted_count(encoding, count);
        }
    }

    // `rx` was moved into run_output_stage and is dropped as that call
    // returns. Workers still parked on a full `tx.send(...)` see the
    // disconnected channel and unwind, so it's safe to join them now.
    join_workers(handles)?;

    if let Some(e) = output_error {
        return Err(Box::new(e));
    }

    if cancelled.load(Ordering::SeqCst) {
        return Err("cancelled by Ctrl+C".into());
    }

    output.flush()?;

    if cfg.keep_temp() {
        let kept = temp_guard.keep()?;
        eprintln!("temporary files kept at: {}", kept.display());
    }

    let peak_rss = peak_rss_bytes();
    if args.stats() || args.verbose() > 0 {
        print_stats(&args, &cfg, file_len, chunk_count, started, peak_rss, &detailed_stats);
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
