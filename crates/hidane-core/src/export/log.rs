//! The LevelDB log format the export files are written in.
//!
//! A file is a sequence of 32 KiB blocks. Each record is stored as one or more fragments, each
//! with a 7-byte header: the masked CRC-32C of the fragment's type and data (4 bytes, little
//! endian), the data length (2 bytes, little endian) and the type: FULL (1), or FIRST (2),
//! MIDDLE (3) and LAST (4) for a record split across blocks. A fragment never crosses a block
//! boundary; a block's last 1 to 6 bytes, too few for a header, are zeros.

use std::{
    borrow::Cow,
    fmt,
    io::{self, Write},
};

const BLOCK: usize = 32 * 1024;
const HEADER: usize = 7;

const FULL: u8 = 1;
const FIRST: u8 = 2;
const MIDDLE: u8 = 3;
const LAST: u8 = 4;

/// CRC-32C (Castagnoli), as LevelDB checksums its records.
pub fn crc32c(data: &[u8]) -> u32 {
    crc32c_extend(0, data)
}

fn crc32c_extend(crc: u32, data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            #[allow(clippy::cast_possible_truncation)]
            let mut crc = i as u32;
            let mut bit = 0;
            while bit < 8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0x82F6_3B78
                } else {
                    crc >> 1
                };
                bit += 1;
            }
            table[i] = crc;
            i += 1;
        }
        table
    };
    let mut crc = !crc;
    for &byte in data {
        crc = TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

/// LevelDB stores checksums masked, so that a checksum of data holding checksums stays useful.
fn mask(crc: u32) -> u32 {
    crc.rotate_right(15).wrapping_add(0xa282_ead8)
}

fn checksum(kind: u8, data: &[u8]) -> u32 {
    mask(crc32c_extend(crc32c(&[kind]), data))
}

/// Writes records to `out`, splitting them across blocks as LevelDB does.
pub struct Writer<W: Write> {
    out: W,
    /// Bytes already written in the current block.
    offset: usize,
    written: u64,
}

impl<W: Write> Writer<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            offset: 0,
            written: 0,
        }
    }

    /// Bytes written so far.
    pub fn len(&self) -> u64 {
        self.written
    }

    pub fn is_empty(&self) -> bool {
        self.written == 0
    }

    pub fn add_record(&mut self, mut data: &[u8]) -> io::Result<()> {
        let mut first = true;
        loop {
            let left = BLOCK - self.offset;
            if left < HEADER {
                self.emit(&[0; HEADER][..left])?;
                self.offset = 0;
            }
            let room = BLOCK - self.offset - HEADER;
            let len = data.len().min(room);
            let last = len == data.len();
            let kind = match (first, last) {
                (true, true) => FULL,
                (true, false) => FIRST,
                (false, false) => MIDDLE,
                (false, true) => LAST,
            };
            let (fragment, rest) = data.split_at(len);
            let mut header = [0u8; HEADER];
            header[..4].copy_from_slice(&checksum(kind, fragment).to_le_bytes());
            #[allow(clippy::cast_possible_truncation)]
            header[4..6].copy_from_slice(&(len as u16).to_le_bytes());
            header[6] = kind;
            self.emit(&header)?;
            self.emit(fragment)?;
            self.offset += HEADER + len;
            data = rest;
            first = false;
            if last {
                return Ok(());
            }
        }
    }

    fn emit(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

/// Encodes `records` as a log in memory.
pub fn write_all<'a>(records: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut writer = Writer::new(Vec::new());
    for record in records {
        writer
            .add_record(record)
            .expect("writing to a Vec cannot fail");
    }
    writer.into_inner()
}

/// Why a log could not be read. The messages are the official emulator's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogError {
    /// A fragment's checksum does not match its data.
    Checksum,
    /// A truncated or misplaced fragment.
    Invalid,
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Checksum => "Checksum doesn't validate.",
            Self::Invalid => "Invalid record",
        })
    }
}

impl std::error::Error for LogError {}

/// The records of a log, in order. Stops after the first error.
pub fn records(data: &[u8]) -> Records<'_> {
    Records { data, pos: 0 }
}

pub struct Records<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Records<'a> {
    /// The next fragment, or `None` at the end of the data.
    fn fragment(&mut self) -> Result<Option<(u8, &'a [u8])>, LogError> {
        loop {
            let left_in_block = BLOCK - self.pos % BLOCK;
            let rest = &self.data[self.pos..];
            if rest.is_empty() {
                return Ok(None);
            }
            if left_in_block < HEADER || rest.len() < HEADER {
                // A block's zero trailer; anything else is a cut-off header.
                let trailer = left_in_block.min(rest.len());
                if rest[..trailer].iter().any(|&b| b != 0) {
                    return Err(LogError::Invalid);
                }
                self.pos += trailer;
                continue;
            }
            let crc = u32::from_le_bytes(rest[..4].try_into().expect("four bytes"));
            let len = usize::from(u16::from_le_bytes([rest[4], rest[5]]));
            let kind = rest[6];
            if HEADER + len > left_in_block || HEADER + len > rest.len() {
                return Err(LogError::Invalid);
            }
            let data = &rest[HEADER..HEADER + len];
            if kind == 0 && len == 0 && crc == 0 {
                // Zeros up to the end of the block (a preallocated file).
                self.pos += left_in_block.min(rest.len());
                continue;
            }
            if crc != checksum(kind, data) {
                return Err(LogError::Checksum);
            }
            self.pos += HEADER + len;
            return Ok(Some((kind, data)));
        }
    }

    fn record(&mut self) -> Result<Option<Cow<'a, [u8]>>, LogError> {
        let Some((kind, data)) = self.fragment()? else {
            return Ok(None);
        };
        match kind {
            FULL => Ok(Some(Cow::Borrowed(data))),
            FIRST => {
                let mut record = data.to_vec();
                loop {
                    match self.fragment()? {
                        Some((MIDDLE, data)) => record.extend_from_slice(data),
                        Some((LAST, data)) => {
                            record.extend_from_slice(data);
                            return Ok(Some(Cow::Owned(record)));
                        }
                        _ => return Err(LogError::Invalid),
                    }
                }
            }
            _ => Err(LogError::Invalid),
        }
    }
}

impl<'a> Iterator for Records<'a> {
    type Item = Result<Cow<'a, [u8]>, LogError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.record() {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => None,
            Err(err) => {
                self.pos = self.data.len();
                Some(Err(err))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(data: &[u8]) -> Result<Vec<Vec<u8>>, LogError> {
        records(data).map(|r| r.map(Cow::into_owned)).collect()
    }

    #[test]
    fn crc32c_matches_the_standard_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn the_leading_record_of_an_overall_metadata_file() {
        // Byte for byte what the official emulator writes first.
        assert_eq!(
            write_all([&[0x33u8][..]]),
            [0xb8, 0x6d, 0x44, 0x4e, 0x01, 0x00, 0x01, 0x33]
        );
        assert_eq!(write_all([&[][..]]), [0x05, 0x2b, 0x28, 0x43, 0, 0, 1]);
    }

    #[test]
    fn records_split_across_blocks_round_trip() {
        let big = vec![7u8; 2 * BLOCK + 100];
        let mut writer = Writer::new(Vec::new());
        writer.add_record(b"a").unwrap();
        writer.add_record(&big).unwrap();
        // Leaves 3 bytes in the block, too few for a header: they become a zero trailer.
        let fill = vec![1u8; BLOCK - writer.offset - HEADER - 3];
        writer.add_record(&fill).unwrap();
        assert_eq!(BLOCK - writer.offset, 3);
        writer.add_record(b"").unwrap();
        writer.add_record(b"after the trailer").unwrap();
        let log = writer.into_inner();
        let records: Vec<&[u8]> = vec![b"a", &big, &fill, b"", b"after the trailer"];
        assert_eq!(read(&log).unwrap(), records);
        assert_eq!(log.len() % BLOCK, 2 * HEADER + "after the trailer".len());
    }

    #[test]
    fn every_record_size_near_a_block_boundary_round_trips() {
        for size in BLOCK - 20..BLOCK + 5 {
            let records = [vec![1u8; size], vec![2u8; 9], vec![3u8; size]];
            let log = write_all(records.iter().map(Vec::as_slice));
            assert_eq!(read(&log).unwrap(), records, "size {size}");
        }
    }

    #[test]
    fn damage_is_reported() {
        let log = write_all([&b"hello world"[..], b"second"]);
        let mut flipped = log.clone();
        flipped[9] ^= 0xff;
        assert_eq!(read(&flipped), Err(LogError::Checksum));
        assert_eq!(read(&log[..log.len() - 3]), Err(LogError::Invalid));
        assert_eq!(read(&log[..3]), Err(LogError::Invalid));
        // A FIRST fragment without the rest of its record.
        let split = write_all([&vec![0u8; BLOCK][..]]);
        assert_eq!(read(&split[..BLOCK]), Err(LogError::Invalid));
    }
}
