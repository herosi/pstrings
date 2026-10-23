//! A FIFO of complete records backed by one file, with only its head in memory.
use crate::encoding::InputEncoding;
use crate::payload::{Spilled, temporary_file};
use crate::record::{MatchRecord, RecordData, read_record_header, write_record};
use std::fs::File;
use std::io::{self, BufWriter, Cursor, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Clone, Copy)]
pub(crate) struct RecordMeta {
    pub offset: u64,
    pub cb: u64,
    pub cch: u64,
    pub encoding: InputEncoding,
    pub flags: u16,
    pub len: u64,
}

pub(crate) struct RecordQueue {
    writer: BufWriter<File>,
    read: u64,
    written: u64,
    reposition_before_write: bool,
    pub front: Option<RecordMeta>,
}

impl RecordQueue {
    pub fn new(dir: Option<&Path>) -> io::Result<Self> {
        Ok(Self {
            writer: BufWriter::with_capacity(64 * 1024, temporary_file(dir)?),
            read: 0,
            written: 0,
            reposition_before_write: false,
            front: None,
        })
    }

    pub fn push(&mut self, record: &MatchRecord) -> io::Result<()> {
        // Windows positioned reads can move the shared file cursor. A view
        // returned by pop may have been read since the last append.
        if self.reposition_before_write {
            self.writer.seek(SeekFrom::Start(self.written))?;
            self.reposition_before_write = false;
        }
        let meta = RecordMeta {
            offset: record.offset,
            cb: record.cb,
            cch: record.cch,
            encoding: record.encoding,
            flags: u16::from(record.data.is_ascii()) * 8,
            len: record.data.payload_len(),
        };
        write_record(&mut self.writer, record)?;
        self.written += 40 + meta.len;
        if self.front.is_none() {
            self.front = Some(meta);
        }
        Ok(())
    }

    pub fn pop(&mut self) -> io::Result<MatchRecord> {
        let meta = self.front.expect("pop on an empty record queue");
        self.writer.flush()?;
        self.reposition_before_write = true;
        let result = MatchRecord {
            offset: meta.offset,
            cb: meta.cb,
            cch: meta.cch,
            encoding: meta.encoding,
            starts_at_chunk: false,
            ends_at_chunk: false,
            data: RecordData::Spilled(Spilled {
                file: self.writer.get_ref().try_clone()?,
                start: self.read + 40,
                len: meta.len,
                raw: false,
                ascii: meta.flags & 8 != 0,
            }),
        };
        self.read += 40 + meta.len;
        self.front = if self.read < self.written {
            let mut bytes = [0; 40];
            crate::scanner::read_exact_at(self.writer.get_ref(), &mut bytes, self.read)?;
            let (offset, cb, cch, encoding, flags, len) =
                read_record_header(&mut Cursor::new(bytes))?.expect("queued record header");
            Some(RecordMeta {
                offset,
                cb,
                cch,
                encoding,
                flags,
                len,
            })
        } else {
            None
        };
        Ok(result)
    }

    /// Call after consuming the returned record: its payload borrows this file
    /// through another handle, so truncation must wait until the copy is done.
    pub fn reset_if_empty(&mut self) -> io::Result<()> {
        if self.front.is_none() {
            self.writer.flush()?;
            self.writer.get_mut().set_len(0)?;
            self.writer.get_mut().seek(SeekFrom::Start(0))?;
            self.read = 0;
            self.written = 0;
            self.reposition_before_write = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(offset: u64, text: &str) -> MatchRecord {
        MatchRecord {
            offset,
            cb: text.len() as u64,
            cch: text.len() as u64,
            encoding: InputEncoding::Ascii,
            starts_at_chunk: false,
            ends_at_chunk: false,
            data: RecordData::Text(text.into()),
        }
    }

    #[test]
    fn appends_survive_a_shared_cursor_move_after_reading() {
        let mut queue = RecordQueue::new(None).unwrap();
        queue.push(&record(0, "FIRST")).unwrap();
        queue.push(&record(10, "SECOND")).unwrap();
        let first = queue.pop().unwrap();
        if let RecordData::Spilled(data) = &first.data {
            let mut shared = &data.file;
            shared.seek(SeekFrom::Start(0)).unwrap();
        }
        drop(first);
        queue.push(&record(20, "THIRD")).unwrap();
        for text in ["SECOND", "THIRD"] {
            let record = queue.pop().unwrap();
            let mut actual = Vec::new();
            record.data.write_payload(&mut actual).unwrap();
            assert_eq!(actual, text.as_bytes());
            drop(record);
            queue.reset_if_empty().unwrap();
        }
        queue.push(&record(30, "REUSED")).unwrap();
        let record = queue.pop().unwrap();
        let mut actual = Vec::new();
        record.data.write_payload(&mut actual).unwrap();
        assert_eq!(actual, b"REUSED");
    }
}
