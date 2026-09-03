use super::read_exact_at;
use crate::chunk::Chunk;
use crate::config::Config;
use crate::encoding::InputEncoding;
use crate::record::{write_record, MatchRecord, RecordData};
use crate::filter;
use crate::tempfile_helper::create_temp_file;
use std::cmp::min;
use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Scans one chunk for UTF-16LE strings.
///
/// Unlike `scanner::utf16le_ascii`, this decodes the *full* UTF-16LE
/// character set governed by `cfg.filter()` (see `filter::CharacterFilter`):
/// any allowed Basic Multilingual Plane code unit (checked via
/// `FilterSet::allows_u16`), plus astral-plane characters via surrogate pairs
/// (see `decode_char_at`, checked via `FilterSet::allows_char`).
///
/// Code units can start at either byte parity within a chunk, so, like
/// `scanner::utf16le_ascii`, this scans both parities (see `scan_parity`)
/// and combines the results. Unlike `utf16le_ascii`, the two parity results
/// are *not* simply merged as-is: with a wide enough filter selection, real
/// text read at the wrong parity can reinterpret as a same-length run of
/// *other* allowed characters (see the KNOWN LIMITATION note below), so
/// before merging, `resolve_parity_overlap` discards whichever side of each
/// byte-range conflict looks less likely to be genuine.
///
/// KNOWN LIMITATION -- dual-parity false positives: genuine ASCII/Latin
/// text read at the *wrong* parity reinterprets each pair of source bytes
/// as one u16 of the form `(next_ascii_byte << 8) | ascii_byte`, and for
/// printable ASCII (0x20-0x7E) that puts the result somewhere in
/// U+2020-U+7E7E -- a span whose upper half sits inside CJK Unified
/// Ideographs (U+4E00-U+9FFF) and whose middle sits inside Extension A
/// (U+3400-U+4DBF), both of which `CharacterFilter::Kanji` admits.
/// Measured empirically: misaligned real text produces a
/// "noise" run of almost the same length as the genuine one (an
/// 11-character ASCII string produced a spurious ~10-character run at the
/// other parity when `Kanji` was selected), so neither narrowing individual
/// filters nor a minimum-run-length threshold reliably filters it out --
/// both were tried and rejected. `resolve_parity_overlap` (this scanner's
/// actual mitigation for this specific failure mode) helps a lot here,
/// but two independent measurements are also worth keeping in mind when
/// choosing which filters to enable together:
///   - Scanning raw/random binary data (e.g. disk free space) with the
///     full BMP admitted, 1 MiB of pure random bytes produced matches
///     covering ~73% of all code units. Restricting to a narrow filter set
///     (e.g. `Ascii` + `Latin1` only) reduced that to effectively zero on
///     the same data, since the odds of several consecutive random code
///     units all landing in a narrow allowed range are very low. The
///     broader a filter combination is, the more it trades detection
///     recall for this kind of false-positive risk -- there's no filter
///     selection that eliminates this tradeoff, only ways to tune where
///     you sit on it.
///   - Because of this, "throw every filter at it" is actively
///     counterproductive: prefer the narrowest combination that covers
///     what you're actually looking for.
pub(crate) fn scan(
    file: &File,
    file_len: u64,
    chunk: &Chunk,
    cfg: &Config,
    temp_path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<(u64, File)> {
    let mut records0 = scan_parity(file, file_len, chunk, cfg, 0, cancelled)?;
    let mut records1 = scan_parity(file, file_len, chunk, cfg, 1, cancelled)?;

    resolve_parity_overlap(&mut records0, &mut records1);

    // Counted from the records that actually survive to be written, which
    // is the same policy `emit_record` enforces for every other scanner:
    // `close_run!` already applied the `min_cch`/boundary test before
    // pushing, so everything still here gets written below.
    let total_records = (records0.len() + records1.len()) as u64;

    let out_file = create_temp_file(temp_path, cfg.keep_temp())?;
    let mut out = BufWriter::with_capacity(crate::WRITE_BUFFER_SIZE, out_file);

    // `records0`/`records1` are each individually offset-sorted (a single
    // left-to-right scan per parity) and, after `resolve_parity_overlap`,
    // no longer overlap each other -- so a plain two-way merge by offset
    // is enough to produce the fully sorted result `scan`'s caller expects.
    // (`merger::merge_sorted_record_files` isn't used here: it merges an
    // arbitrary, File-backed number of streams, which is unnecessary
    // machinery/I-O for exactly two in-memory `Vec`s.)
    let mut i = 0;
    let mut j = 0;
    while i < records0.len() || j < records1.len() {
        let take0 = match (records0.get(i), records1.get(j)) {
            (Some(a), Some(b)) => a.offset <= b.offset,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => unreachable!("loop condition guarantees at least one side remains"),
        };
        if take0 {
            write_record(&mut out, &records0[i])?;
            i += 1;
        } else {
            write_record(&mut out, &records1[j])?;
            j += 1;
        }
    }

    out.flush()?;
    let mut f = out.into_inner().map_err(|e| e.into_error())?;
    f.seek(io::SeekFrom::Start(0))?;

    Ok((total_records, f))
}

/// Attempts to decode one Unicode scalar value starting at `buf[pos]`,
/// where `buf` holds raw UTF-16LE bytes, and checks it against `filters`.
///
/// Returns `Some((char, byte_len))`:
/// - `byte_len == 2` for a lone, non-surrogate BMP code unit that
///   `filter::allows_u16` admits.
/// - `byte_len == 4` for a complete, valid surrogate pair (high surrogate
///   at `buf[pos]`, matching low surrogate at `buf[pos + 2]`) whose decoded
///   astral-plane character `filter::allows_char` admits.
///
/// Note the two concerns this function handles are independent: whether a
/// surrogate pair is *structurally valid UTF-16LE* (a high surrogate
/// immediately followed by a matching low surrogate) is checked
/// unconditionally, regardless of `filters` -- that's a property of the
/// encoding itself, not of which characters are of interest. Only once a
/// pair decodes to a real scalar value does *that* get run past `filters`,
/// same as any BMP code unit. Concretely: if `filters` contains no
/// astral-plane filter (as of writing, only `KanjiExtB` is), every
/// well-formed surrogate pair in the input is still correctly recognized
/// as one -- it just never survives the `filter::allows_char` check that
/// follows, so nothing is ever emitted for it.
///
/// Returns `None` if nothing allowed starts at `pos`: a code unit/character
/// the selected filters reject, a lone/mismatched surrogate, or a high
/// surrogate whose low surrogate isn't present in `buf` (`read_scan_block`
/// guarantees this only happens when the underlying file itself has no
/// more bytes there -- i.e. a genuinely truncated trailing surrogate, not
/// a buffering artifact).
///
/// The BMP case is checked via `FilterSet::allows_u16` (operating on the
/// raw `u16`) rather than converting to `char` first and using
/// `allows_char`, since this runs once per code unit scanned -- avoiding
/// that conversion for the common case matters at scale. That check is a
/// branch-free bitset lookup whose cost doesn't grow with the number of
/// selected filters (see `filter::FilterSet`). Astral characters are
/// decoded first regardless (the conversion is unavoidable there, since
/// deciding which astral characters are allowed necessarily means
/// comparing a full scalar value, not a `u16`).
#[inline]
fn decode_char_at(filters: &filter::FilterSet, buf: &[u8], pos: usize) -> Option<(char, usize)> {
    let hi = u16::from_le_bytes([buf[pos], buf[pos + 1]]);
    match hi {
        0xD800..=0xDBFF => {
            if pos + 3 >= buf.len() {
                return None;
            }
            let lo = u16::from_le_bytes([buf[pos + 2], buf[pos + 3]]);
            if !(0xDC00..=0xDFFF).contains(&lo) {
                return None;
            }
            let scalar = 0x10000u32 + (((hi as u32) - 0xD800) << 10) + ((lo as u32) - 0xDC00);
            let ch = char::from_u32(scalar)?;
            filters.allows_char(ch).then_some((ch, 4))
        }
        // Lone low surrogate: never valid on its own.
        0xDC00..=0xDFFF => None,
        _ => {
            if !filters.allows_u16(hi) {
                return None;
            }
            char::from_u32(hi as u32).map(|c| (c, 2))
        }
    }
}

/// UTF-16LE code units can start at either byte parity within a chunk. Each
/// parity is scanned as an independent stream (its own runs, its own
/// boundary bookkeeping); `scan` combines the two afterward. Returns the
/// matches that survived `emit_record`'s min-length-or-boundary rule,
/// applied inline (there's no `BufWriter<File>` to funnel through the
/// shared `scanner::emit_record` helper here -- see `scan`'s doc comment
/// for why results are collected in memory instead).
fn scan_parity(
    file: &File,
    file_len: u64,
    chunk: &Chunk,
    cfg: &Config,
    parity: u64,
    cancelled: &AtomicBool,
) -> io::Result<Vec<MatchRecord>> {
    let mut out: Vec<MatchRecord> = Vec::new();

    // Clamp to file_len in case this is the final chunk and its nominal
    // length would otherwise run past EOF.
    let chunk_end = min(chunk.offset + chunk.len, file_len);
    // First absolute byte offset >= chunk.offset whose parity (offset % 2)
    // matches the requested `parity`. Derived arithmetically (rather than
    // with a branch) as: if chunk.offset already has the right parity, stay
    // put; otherwise step forward one byte.
    let first = chunk.offset + ((parity + 2 - chunk.offset % 2) % 2);

    // No room for even one 2-byte code unit of this parity within the
    // chunk/file bounds -> nothing to scan.
    if first >= chunk_end || first + 1 >= file_len {
        return Ok(out);
    }
    // Last byte offset at which a code unit could validly *start*.
    // - If this chunk is *not* the last one in the file (chunk_end <
    //   file_len), a code unit is allowed to start as late as chunk_end - 1,
    //   i.e. its remaining bytes may spill into the next chunk. That's
    //   intentional: it lets a code unit (or, now, a surrogate pair -- see
    //   `read_scan_block`) that straddles the chunk boundary still be
    //   recognized here (as a fragment, via starts/ends_at_chunk), instead
    //   of being silently missed by both neighboring chunks. Note this only
    //   needs to account for a 2-byte spill, even though a surrogate pair
    //   is 4 bytes: `read_scan_block` independently extends its own read,
    //   bounded by `file_len`, whenever the *last* code unit it would
    //   otherwise return turns out to be a high surrogate -- so the actual
    //   byte range read can already extend further than `max_start` alone
    //   would suggest.
    // - If this chunk *is* the last one (chunk_end == file_len), there is no
    //   next chunk to spill into, so the code unit must fit entirely before
    //   file_len, hence file_len - 2.
    let max_start = if chunk_end < file_len {
        chunk_end.saturating_sub(1)
    } else {
        file_len.saturating_sub(2)
    };
    // Snap max_start down to the requested parity if it isn't already
    // aligned to it.
    let last_start = if max_start % 2 == parity {
        max_start
    } else {
        max_start.saturating_sub(1)
    };
    if last_start < first {
        return Ok(out);
    }
    // Number of code units of this parity between `first` and `last_start`
    // inclusive, stepping by 2 bytes each.
    let unit_count = (last_start - first) / 2 + 1;

    // Accumulated directly as a `String` rather than as a `Vec<u8>` that is
    // later handed to `String::from_utf8`: every byte appended here comes
    // from encoding a `char` we already decoded, so the result is valid
    // UTF-8 by construction and re-validating it would be a second full
    // pass over every matched run for no benefit. Closing a run is then
    // just a move (`mem::take`) instead of a scan.
    let mut run_data = String::new();
    let mut run_offset = 0u64;
    let mut run_cb = 0u64;
    let mut run_cch = 0u64;
    let mut run_started = false;
    // Read in blocks of whole code units sized to roughly READ_BUFFER_SIZE
    // bytes, so I/O stays batched instead of one read() per 2-byte unit.
    let block_units = (crate::READ_BUFFER_SIZE / 2).max(1) as u64;
    let mut processed = 0u64;
    // Block buffer allocated once and reused for every iteration below,
    // rather than per block. `READ_BUFFER_SIZE` is measured in megabytes,
    // and `vec![0u8; n]` zero-fills memory that `read_exact_at` overwrites
    // immediately afterwards -- doing that per block burns a full pass of
    // memory bandwidth per block on writes nobody ever reads. Sized to the
    // largest block this scan can request (capped by `unit_count` so small
    // chunks don't reserve the full buffer), plus the 2 spare bytes
    // `read_scan_block` may append to complete a trailing surrogate pair.
    let max_block_units = min(block_units, unit_count);
    let mut buf = vec![0u8; (max_block_units * 2 + 2) as usize];

    macro_rules! close_run {
        ($ends_at_chunk:expr) => {
            if !run_data.is_empty() {
                let rec = MatchRecord {
                    offset: run_offset,
                    cb: run_cb,
                    cch: run_cch,
                    encoding: InputEncoding::Utf16le,
                    starts_at_chunk: run_started,
                    ends_at_chunk: $ends_at_chunk,
                    data: RecordData::Text(std::mem::take(&mut run_data)),
                };
                if rec.cch >= cfg.min_cch() || rec.starts_at_chunk || rec.ends_at_chunk {
                    out.push(rec);
                }
            }
        };
    }

    while processed < unit_count {
        // Same per-block (not per-unit) cancellation check as ascii::scan,
        // for the same reason: cheap enough to stay responsive, without
        // paying atomic-load overhead per 2-byte unit.
        if cancelled.load(Ordering::Relaxed) {
            return Ok(out);
        }
        let n_units = min(block_units, unit_count - processed);
        let block_start = first + processed * 2;
        let filled = read_scan_block(file, &mut buf, block_start, n_units, file_len)?;
        // `read_scan_block` may have read one extra code unit beyond
        // `n_units` to complete a trailing high surrogate's pair (see its
        // doc comment). Track that so `processed` still advances by the
        // true number of code units consumed from the file this iteration
        // -- same "borrow a little past the nominal budget" spirit as the
        // single-byte chunk-boundary spillover above, just applied per
        // block instead of only at the very end of the range.
        let extra_unit = filled > (n_units * 2) as usize;
        let block = &buf[..filled];

        let mut i = 0usize;
        while i < block.len() {
            let abs = block_start + i as u64;

            match decode_char_at(cfg.filter(), block, i) {
                Some((ch, len)) => {
                    if run_data.is_empty() {
                        run_offset = abs;
                        // Only true if this run's first unit is the very
                        // first possible unit of this parity in the chunk
                        // -- i.e. it may be a continuation of a run from
                        // the previous chunk, not a run that genuinely
                        // starts here.
                        run_started = abs == first;
                    }
                    // `String::push` encodes straight into the string's own
                    // buffer; the previous form round-tripped through a
                    // 4-byte scratch array and `extend_from_slice`, which
                    // only existed because `run_data` used to be a
                    // `Vec<u8>`.
                    run_data.push(ch);
                    run_cb += len as u64;
                    run_cch += 1;
                    i += len;
                }
                None => {
                    // Run closed by a non-matching/invalid code unit
                    // mid-chunk, so it cannot be continuing into the next
                    // chunk here.
                    close_run!(false);
                    run_cb = 0;
                    run_cch = 0;
                    run_started = false;
                    // Resync by exactly one code unit (2 bytes), preserving
                    // this parity's stepping, even for a lone/invalid
                    // surrogate -- reinterpreting its second byte as the
                    // start of the next unit would desync parity.
                    i += 2;
                }
            }
        }
        processed += n_units + u64::from(extra_unit);
    }
    // Trailing run still open when the scan range was exhausted. Unlike
    // ascii::scan (where reaching the end of the loop always means the
    // chunk boundary itself was hit), `unit_count` here was derived from
    // `max_start`, which may extend past `chunk_end` to allow
    // boundary-straddling characters (see above) -- so `ends_at_chunk` is
    // computed explicitly by comparing the run's end against `chunk_end`,
    // rather than being unconditionally `true`.
    close_run!(run_offset + run_cb >= chunk_end);

    Ok(out)
}

/// Reads `n_units` code units (2 bytes each, of the parity `scan_parity` is
/// currently scanning) starting at `block_start` into `buf`, returning how
/// many bytes of `buf` were actually filled.
///
/// `buf` is the caller's reusable block buffer and must be at least
/// `n_units * 2 + 2` bytes long -- the trailing 2 bytes leave room for the
/// surrogate completion described below. Writing into a caller-owned buffer
/// (rather than returning a fresh `Vec`) keeps the per-block cost to just
/// the read itself: a freshly allocated `vec![0u8; n]` would zero-fill
/// megabytes that `read_exact_at` immediately overwrites.
///
/// If the *last* of those units looks like it could be a high surrogate,
/// this also reads its 2-byte low-surrogate half immediately -- bounded by
/// `file_len` -- so a surrogate pair is never split between two separate
/// reads. That split could otherwise happen at two different boundaries
/// that look identical from here: this function's own I/O-batching block
/// size (`block_units` in the caller), or the very end of `scan_parity`'s
/// whole nominal scan range (`unit_count`, which is itself already
/// extended past the chunk boundary for exactly this kind of
/// boundary-straddling case -- see the comment on `max_start` in
/// `scan_parity`). Both are just "the buffer about to be returned ends
/// right after a potential high surrogate," so one mechanism covers both.
///
/// If `file_len` doesn't have room for the extra 2 bytes, no extra read
/// happens; `decode_char_at` then correctly treats that trailing high
/// surrogate as unresolved (there's nothing left in the file to pair with).
fn read_scan_block(
    file: &File,
    buf: &mut [u8],
    block_start: u64,
    n_units: u64,
    file_len: u64,
) -> io::Result<usize> {
    let mut filled = (n_units * 2) as usize;
    read_exact_at(file, &mut buf[..filled], block_start)?;

    if n_units > 0 {
        let last_unit_start = filled - 2;
        let last_unit = u16::from_le_bytes([buf[last_unit_start], buf[last_unit_start + 1]]);
        if (0xD800..=0xDBFF).contains(&last_unit) {
            let low_surrogate_offset = block_start + n_units * 2;
            if low_surrogate_offset + 2 <= file_len {
                read_exact_at(file, &mut buf[filled..filled + 2], low_surrogate_offset)?;
                filled += 2;
            }
        }
    }

    Ok(filled)
}

/// Whether every character in `data` is admitted by `CharacterFilter::
/// Ascii` alone. Used by `resolve_parity_overlap` to recognize the
/// specific dual-parity false-positive pattern described in `scan`'s doc
/// comment: a genuine ASCII run at one parity versus its same-byte-range
/// misreading at the other (typically landing in `Kanji`'s range, when
/// that filter is selected).
///
/// Deliberately checks against `Ascii` specifically, not "whatever filters
/// are currently selected": the ASCII-vs-misread pattern is a fixed,
/// well-understood signal regardless of which other filters happen to be
/// active, so tying this to the selected filter set would make the
/// heuristic's behavior depend on unrelated `--filter` choices.
fn is_ascii_printable_only(data: &str) -> bool {
    data.chars()
        .all(|ch| filter::allows_char(&[filter::CharacterFilter::Ascii], ch))
}

/// Given two same-length-ish overlapping candidates -- one of which is
/// often the dual-parity false-positive misreading of the other (see
/// `scan`'s KNOWN LIMITATION note) -- decides which is more likely
/// genuine.
///
/// The dominant real-world case (misaligned ASCII text) always produces
/// the *genuine* run in the printable-ASCII range and the *spurious* one
/// outside it, so "prefer whichever side is ASCII-only" directly targets
/// that. When both sides are ASCII-only or both are non-ASCII -- e.g. two
/// independent short matches that happen to overlap by coincidence, or a
/// conflict this heuristic's targeted failure mode doesn't explain --
/// there's no strong signal either way, so this just keeps the longer one
/// (ties favor `a`, i.e. parity 0), rather than guessing further.
///
/// "Longer" means *bytes covered* (`cb`), not characters (`cch`). Those
/// two agree for BMP-only runs, where every character is two bytes, but
/// diverge as soon as surrogate pairs are involved: a run of astral
/// characters spends four bytes per character, so counting characters
/// would score it at half the weight of the wrong-parity misreading laid
/// over the same bytes -- which decodes those same bytes as twice as many
/// BMP code units. Since the whole point here is to pick between two
/// readings of *one byte range*, the byte range is the fair comparison.
fn prefer_a_over_b(a: &MatchRecord, b: &MatchRecord) -> bool {
    match (is_ascii_printable_only(a.data.text_of()), is_ascii_printable_only(b.data.text_of())) {
        (true, false) => true,
        (false, true) => false,
        _ => a.cb >= b.cb,
    }
}

/// Drops one side of every byte-range conflict between the two parity scan
/// results, using `prefer_a_over_b` to decide which side survives.
///
/// Both inputs are individually offset-sorted (each is one left-to-right
/// scan), so a standard two-pointer interval sweep finds every overlapping
/// pair in O(records0.len() + records1.len()) without needing to compare
/// every record against every other record.
///
/// Scoped to conflicts *within one chunk*: each side's records reflect
/// only what this chunk's scan produced, before `outputter` joins any
/// `ends_at_chunk`/`starts_at_chunk` fragment with its continuation in a
/// neighboring chunk. A conflict that only becomes visible after such a
/// join (e.g. the genuine run is short in this chunk but a long
/// continuation elsewhere would have tipped `prefer_a_over_b`'s tie-break)
/// isn't caught here. Resolving that fully would mean tracking parity
/// through `outputter`'s cross-chunk pending/boundary-joining machinery
/// (currently keyed only by `InputEncoding`, with no notion of parity) --
/// left as future work if within-chunk resolution alone isn't enough in
/// practice.
fn resolve_parity_overlap(records0: &mut Vec<MatchRecord>, records1: &mut Vec<MatchRecord>) {
    let mut drop0 = vec![false; records0.len()];
    let mut drop1 = vec![false; records1.len()];
    let mut i = 0usize;
    let mut j = 0usize;

    while i < records0.len() && j < records1.len() {
        let a = &records0[i];
        let b = &records1[j];
        let a_end = a.offset + a.cb;
        let b_end = b.offset + b.cb;

        if a_end <= b.offset {
            i += 1;
            continue;
        }
        if b_end <= a.offset {
            j += 1;
            continue;
        }

        // Byte ranges overlap: exactly the situation this function exists
        // to resolve. Two independent, unrelated matches essentially never
        // occupy the same bytes by coincidence (there's only one way to
        // align real UTF-16LE text over a given byte range), so this is a
        // strong signal one side is the wrong-parity misreading of the
        // other.
        if prefer_a_over_b(a, b) {
            drop1[j] = true;
        } else {
            drop0[i] = true;
        }

        // Advance whichever record(s) are now fully behind the other's
        // range -- standard interval-sweep bookkeeping so a long record on
        // one side can still be checked against several shorter records on
        // the other.
        if a_end < b_end {
            i += 1;
        } else if b_end < a_end {
            j += 1;
        } else {
            i += 1;
            j += 1;
        }
    }

    let mut idx = 0;
    records0.retain(|_| {
        let keep = !drop0[idx];
        idx += 1;
        keep
    });
    idx = 0;
    records1.retain(|_| {
        let keep = !drop1[idx];
        idx += 1;
        keep
    });
}