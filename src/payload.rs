//! File-backed payloads for records that outgrow a bounded in-memory buffer.
use crate::encoding::InputEncoding;
use crate::record::{MatchRecord, RecordData};
use crate::scanner::{dbcs, iso2022jp};
use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub(crate) const MEMORY_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct Spilled {
    pub file: File,
    pub start: u64,
    pub len: u64,
    pub raw: bool,
    pub ascii: bool,
}

impl Spilled {
    pub fn copy_to(&self, out: &mut impl Write) -> io::Result<()> {
        let mut buf = vec![0; self.len.min(64 * 1024) as usize];
        let mut pos = 0;
        while pos < self.len {
            let len = (self.len - pos).min(buf.len() as u64) as usize;
            crate::scanner::read_exact_at(&self.file, &mut buf[..len], self.start + pos)?;
            out.write_all(&buf[..len])?;
            pos += len as u64;
        }
        Ok(())
    }
}

impl RecordData {
    pub fn payload_len(&self) -> u64 {
        match self {
            Self::Text(s) => s.len() as u64,
            Self::Raw(b) => b.len() as u64,
            Self::Spilled(s) => s.len,
        }
    }

    pub fn is_raw(&self) -> bool {
        match self {
            Self::Text(_) => false,
            Self::Raw(_) => true,
            Self::Spilled(s) => s.raw,
        }
    }

    pub fn is_ascii(&self) -> bool {
        match self {
            Self::Text(s) => s.bytes().all(|b| b == b'\t' || (0x20..=0x7e).contains(&b)),
            Self::Raw(_) => false,
            Self::Spilled(s) => s.ascii,
        }
    }

    pub fn write_payload(&self, out: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Text(s) => out.write_all(s.as_bytes()),
            Self::Raw(b) => out.write_all(b),
            Self::Spilled(s) => s.copy_to(out),
        }
    }

    pub fn spill(&mut self, dir: Option<&Path>) -> io::Result<()> {
        if matches!(self, Self::Spilled(_)) || self.payload_len() <= MEMORY_LIMIT as u64 {
            return Ok(());
        }
        let mut file = temporary_file(dir)?;
        self.write_payload(&mut file)?;
        *self = Self::Spilled(Spilled {
            file,
            start: 0,
            len: self.payload_len(),
            raw: self.is_raw(),
            ascii: self.is_ascii(),
        });
        Ok(())
    }

    pub fn append(&mut self, other: Self, dir: Option<&Path>) -> io::Result<()> {
        assert_eq!(
            self.is_raw(),
            other.is_raw(),
            "inconsistent record payload kinds"
        );
        if !matches!(self, Self::Spilled(_))
            && (self.payload_len() + other.payload_len() > MEMORY_LIMIT as u64
                || matches!(&other, Self::Spilled(_)))
        {
            let mut file = temporary_file(dir)?;
            self.write_payload(&mut file)?;
            *self = Self::Spilled(Spilled {
                file,
                start: 0,
                len: self.payload_len(),
                raw: self.is_raw(),
                ascii: self.is_ascii(),
            });
        }
        match (self, other) {
            (Self::Text(dst), Self::Text(src)) => dst.push_str(&src),
            (Self::Raw(dst), Self::Raw(src)) => dst.extend_from_slice(&src),
            (Self::Spilled(dst), src) => {
                dst.file.seek(SeekFrom::Start(dst.start + dst.len))?;
                src.write_payload(&mut dst.file)?;
                dst.len += src.payload_len();
                dst.ascii &= src.is_ascii();
            }
            _ => unreachable!("spilled sources require a spilled destination"),
        }
        Ok(())
    }
}

pub(crate) fn temporary_file(dir: Option<&Path>) -> io::Result<File> {
    match dir {
        Some(dir) => tempfile::tempfile_in(dir),
        None => tempfile::tempfile(),
    }
}

enum Unit<'a> {
    Text(Cow<'a, str>, usize),
    Invalid,
    Incomplete,
}

fn dbcs_unit<E: dbcs::Dbcs>(bytes: &[u8]) -> Unit<'_> {
    match dbcs::decode_step::<E>(bytes) {
        dbcs::Step::Complete { len } => Unit::Text(
            E::decoder().decode_without_bom_handling(&bytes[..len]).0,
            len,
        ),
        dbcs::Step::Invalid => Unit::Invalid,
        dbcs::Step::Incomplete => Unit::Incomplete,
    }
}

fn decode_unit(encoding: InputEncoding, bytes: &[u8]) -> Unit<'_> {
    use crate::scanner::{big5::Big5, cp932::Cp932, euckr::EucKr, gb18030::Gb18030, gbk::Gbk};
    match encoding {
        InputEncoding::Cp932 => dbcs_unit::<Cp932>(bytes),
        InputEncoding::Gbk => dbcs_unit::<Gbk>(bytes),
        InputEncoding::Gb18030 => dbcs_unit::<Gb18030>(bytes),
        InputEncoding::EucKr => dbcs_unit::<EucKr>(bytes),
        InputEncoding::Big5 => dbcs_unit::<Big5>(bytes),
        _ => unreachable!("self-synchronizing encoding has no raw payload"),
    }
}

/// Resolve a spilled raw run using the same character and escape decisions as
/// the scanners. Only a four-byte lookahead and the current bounded text run
/// are in memory; finished runs are delivered to the output queue immediately.
pub(crate) fn decode_spilled(
    record: MatchRecord,
    dir: Option<&Path>,
    mut emit: impl FnMut(MatchRecord) -> io::Result<()>,
) -> io::Result<Option<MatchRecord>> {
    let MatchRecord {
        offset,
        encoding,
        data,
        ..
    } = record;
    let RecordData::Spilled(data) = data else {
        unreachable!()
    };
    assert!(data.raw);
    let mut input = data.file.try_clone()?;
    input.seek(SeekFrom::Start(data.start))?;
    let mut input = BufReader::with_capacity(64 * 1024, input.take(data.len));
    let mut window = [0; 4];
    let mut filled = 0;
    let mut pos = 0u64;
    let mut read = 0u64;
    let mut run: Option<MatchRecord> = None;
    let mut text_buffer = String::new();
    let mut mode = iso2022jp::Mode::Ascii;
    let mut escape_start: Option<u64> = None;
    let mut tail = None;
    loop {
        while filled < window.len() && read < data.len {
            input.read_exact(&mut window[filled..filled + 1])?;
            filled += 1;
            read += 1;
        }
        if filled == 0 {
            break;
        }
        let bytes = &window[..filled];
        let mut consumed = 1;
        if encoding == InputEncoding::Iso2022Jp && bytes[0] == iso2022jp::ESC {
            match iso2022jp::read_escape(bytes) {
                iso2022jp::EscapeStep::Complete { mode: next, len } => {
                    escape_start.get_or_insert(pos);
                    mode = next;
                    consumed = len;
                    window.copy_within(consumed..filled, 0);
                    filled -= consumed;
                    pos += consumed as u64;
                    continue;
                }
                iso2022jp::EscapeStep::Incomplete => {
                    tail = Some(escape_start.unwrap_or(pos));
                    break;
                }
                iso2022jp::EscapeStep::Invalid => {
                    finish_decoded(&mut run, &mut text_buffer, dir, &mut emit)?;
                    mode = iso2022jp::Mode::Ascii;
                    escape_start = None;
                    window.copy_within(1..filled, 0);
                    filled -= 1;
                    pos += 1;
                    continue;
                }
            }
        }
        let unit = if encoding == InputEncoding::Iso2022Jp {
            match iso2022jp::decode_char(mode, bytes) {
                iso2022jp::Step::Complete { ch, len } if iso2022jp::char_allowed(ch) => Unit::Text(
                    if ch.is_ascii() {
                        Cow::Borrowed(std::str::from_utf8(&bytes[..len]).unwrap())
                    } else {
                        Cow::Owned(ch.to_string())
                    },
                    len,
                ),
                iso2022jp::Step::Complete { .. } | iso2022jp::Step::Invalid => Unit::Invalid,
                iso2022jp::Step::Incomplete => Unit::Incomplete,
            }
        } else {
            decode_unit(encoding, bytes)
        };
        match unit {
            Unit::Text(text, len) => {
                consumed = len;
                let start = escape_start.take().unwrap_or(pos);
                let record = run.get_or_insert_with(|| MatchRecord {
                    offset: offset + start,
                    cb: 0,
                    cch: 0,
                    encoding,
                    starts_at_chunk: false,
                    ends_at_chunk: false,
                    data: RecordData::Text(String::new()),
                });
                record.cb = offset + pos + len as u64 - record.offset;
                record.cch += text.chars().count() as u64;
                text_buffer.push_str(&text);
                if text_buffer.len() >= 64 * 1024 {
                    record
                        .data
                        .append(RecordData::Text(std::mem::take(&mut text_buffer)), dir)?;
                }
            }
            Unit::Invalid => {
                finish_decoded(&mut run, &mut text_buffer, dir, &mut emit)?;
                mode = iso2022jp::Mode::Ascii;
                escape_start = None;
            }
            Unit::Incomplete => {
                tail = Some(escape_start.unwrap_or(pos));
                break;
            }
        }
        window.copy_within(consumed..filled, 0);
        filled -= consumed;
        pos += consumed as u64;
    }
    finish_decoded(&mut run, &mut text_buffer, dir, &mut emit)?;
    Ok(tail.map(|start| MatchRecord {
        offset: offset + start,
        cb: data.len - start,
        cch: 0,
        encoding,
        starts_at_chunk: false,
        ends_at_chunk: true,
        data: RecordData::Spilled(Spilled {
            file: data.file,
            start: data.start + start,
            len: data.len - start,
            raw: true,
            ascii: false,
        }),
    }))
}

fn finish_decoded(
    run: &mut Option<MatchRecord>,
    text: &mut String,
    dir: Option<&Path>,
    emit: &mut impl FnMut(MatchRecord) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(mut record) = run.take() {
        record
            .data
            .append(RecordData::Text(std::mem::take(text)), dir)?;
        emit(record)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spilled_raw_decoding_matches_the_existing_segmenters() {
        for encoding in [
            InputEncoding::Cp932,
            InputEncoding::Gbk,
            InputEncoding::Gb18030,
            InputEncoding::EucKr,
            InputEncoding::Big5,
            InputEncoding::Iso2022Jp,
        ] {
            let bytes = if encoding == InputEncoding::Iso2022Jp {
                b"ABC\x1b$BF|K\x5c8l\x1b(BDEF\0GHI\x1b$".to_vec()
            } else {
                b"ABC\x81\x40DEF\0GHI\x81\x30\x81\x35JKL\xff\x81".to_vec()
            };
            let (expected, expected_tail) = crate::scanner::segment_raw(encoding, &bytes);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&bytes).unwrap();
            let record = MatchRecord {
                offset: 17,
                cb: bytes.len() as u64,
                cch: 0,
                encoding,
                starts_at_chunk: false,
                ends_at_chunk: false,
                data: RecordData::Spilled(Spilled {
                    file,
                    start: 0,
                    len: bytes.len() as u64,
                    raw: true,
                    ascii: false,
                }),
            };
            let mut actual = Vec::new();
            let tail = decode_spilled(record, None, |record| {
                actual.push(record);
                Ok(())
            })
            .unwrap();
            assert_eq!(actual.len(), expected.len(), "{encoding:?}");
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(
                    (actual.offset, actual.cb, actual.cch, actual.data.text_of()),
                    (
                        17 + expected.start,
                        expected.cb,
                        expected.cch,
                        expected.data.as_str()
                    ),
                    "{encoding:?}"
                );
            }
            let mut actual_tail = Vec::new();
            if let Some(tail) = tail {
                tail.data.write_payload(&mut actual_tail).unwrap();
            }
            assert_eq!(actual_tail, expected_tail, "{encoding:?}");
        }
    }

    #[test]
    fn appending_a_long_run_spills_without_changing_bytes() {
        let mut data = RecordData::Text("A".repeat(MEMORY_LIMIT));
        data.append(RecordData::Text("BC".into()), None).unwrap();
        assert!(matches!(&data, RecordData::Spilled(_)));
        assert_eq!(data.payload_len(), MEMORY_LIMIT as u64 + 2);
        let mut bytes = Vec::new();
        data.write_payload(&mut bytes).unwrap();
        assert!(bytes[..MEMORY_LIMIT].iter().all(|&b| b == b'A'));
        assert_eq!(&bytes[MEMORY_LIMIT..], b"BC");
    }
}
