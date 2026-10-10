//! The protobuf wire format, by hand: the export's messages are proto1 (App Engine's
//! `EntityProto`), whose groups and field order prost does not reproduce.

/// Appends fields to a buffer, in the order they are written.
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

const VARINT: u32 = 0;
const FIXED64: u32 = 1;
const LEN: u32 = 2;
const START_GROUP: u32 = 3;
const END_GROUP: u32 = 4;
const FIXED32: u32 = 5;

impl Writer {
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    fn varint(&mut self, mut value: u64) {
        while value >= 0x80 {
            #[allow(clippy::cast_possible_truncation)]
            self.buf.push((value as u8) | 0x80);
            value >>= 7;
        }
        #[allow(clippy::cast_possible_truncation)]
        self.buf.push(value as u8);
    }

    fn tag(&mut self, field: u32, wire_type: u32) {
        self.varint(u64::from(field << 3 | wire_type));
    }

    pub fn uint(&mut self, field: u32, value: u64) {
        self.tag(field, VARINT);
        self.varint(value);
    }

    /// An `int64`: negative values take ten bytes, as in protobuf.
    pub fn int(&mut self, field: u32, value: i64) {
        #[allow(clippy::cast_sign_loss)]
        self.uint(field, value as u64);
    }

    pub fn double(&mut self, field: u32, value: f64) {
        self.tag(field, FIXED64);
        self.buf.extend_from_slice(&value.to_bits().to_le_bytes());
    }

    pub fn bytes(&mut self, field: u32, value: &[u8]) {
        self.tag(field, LEN);
        self.varint(value.len() as u64);
        self.buf.extend_from_slice(value);
    }

    /// A length-delimited message.
    pub fn message(&mut self, field: u32, write: impl FnOnce(&mut Self)) {
        let mut inner = Self::default();
        write(&mut inner);
        self.bytes(field, &inner.buf);
    }

    /// A proto1 group: the fields between a start and an end tag.
    pub fn group(&mut self, field: u32, write: impl FnOnce(&mut Self)) {
        self.tag(field, START_GROUP);
        write(self);
        self.tag(field, END_GROUP);
    }
}

/// A field's value as read.
#[derive(Debug, Clone, Copy)]
pub enum Field<'a> {
    Varint(u64),
    Fixed64(u64),
    /// Skipped: no field of the export is a `fixed32`.
    Fixed32,
    /// A length-delimited field: bytes, a string or a message.
    Bytes(&'a [u8]),
    /// The fields of a group.
    Group(&'a [u8]),
}

impl<'a> Field<'a> {
    pub fn as_varint(self) -> Option<u64> {
        match self {
            Self::Varint(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_fixed64(self) -> Option<u64> {
        match self {
            Self::Fixed64(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_bytes(self) -> Option<&'a [u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// The fields of a message or of a group.
    pub fn fields(self) -> Option<Reader<'a>> {
        match self {
            Self::Bytes(b) | Self::Group(b) => Some(Reader::new(b)),
            _ => None,
        }
    }
}

/// Malformed protobuf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

/// Reads fields one by one, in the order they were written.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn varint(&mut self) -> Result<u64, Malformed> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.data.get(self.pos).ok_or(Malformed)?;
            self.pos += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(value);
            }
        }
        Err(Malformed)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], Malformed> {
        let end = self.pos.checked_add(len).ok_or(Malformed)?;
        let slice = self.data.get(self.pos..end).ok_or(Malformed)?;
        self.pos = end;
        Ok(slice)
    }

    /// The next field number and wire type, or `None` at the end.
    fn tag(&mut self) -> Result<Option<(u32, u32)>, Malformed> {
        if self.pos == self.data.len() {
            return Ok(None);
        }
        let tag = u32::try_from(self.varint()?).map_err(|_| Malformed)?;
        if tag >> 3 == 0 {
            return Err(Malformed);
        }
        Ok(Some((tag >> 3, tag & 7)))
    }

    fn value(&mut self, field: u32, wire_type: u32) -> Result<Field<'a>, Malformed> {
        Ok(match wire_type {
            VARINT => Field::Varint(self.varint()?),
            FIXED64 => Field::Fixed64(u64::from_le_bytes(
                self.take(8)?.try_into().map_err(|_| Malformed)?,
            )),
            FIXED32 => {
                self.take(4)?;
                Field::Fixed32
            }
            LEN => {
                let len = usize::try_from(self.varint()?).map_err(|_| Malformed)?;
                Field::Bytes(self.take(len)?)
            }
            START_GROUP => {
                let start = self.pos;
                loop {
                    let end = self.pos;
                    let (inner, inner_type) = self.tag()?.ok_or(Malformed)?;
                    if inner_type == END_GROUP {
                        if inner != field {
                            return Err(Malformed);
                        }
                        return Ok(Field::Group(&self.data[start..end]));
                    }
                    self.value(inner, inner_type)?;
                }
            }
            _ => return Err(Malformed),
        })
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<(u32, Field<'a>), Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        let result = (|| {
            let Some((field, wire_type)) = self.tag()? else {
                return Ok(None);
            };
            Ok(Some((field, self.value(field, wire_type)?)))
        })();
        match result {
            Ok(Some(item)) => Some(Ok(item)),
            Ok(None) => None,
            Err(err) => {
                // Stop after an error.
                self.pos = self.data.len();
                Some(Err(err))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_nest_and_round_trip() {
        let mut w = Writer::default();
        w.uint(1, 300);
        w.group(12, |g| {
            g.bytes(13, b"app");
            g.group(14, |e| {
                e.bytes(15, b"kind");
                e.int(16, -3);
            });
        });
        w.double(4, 1.5);
        let bytes = w.into_bytes();

        let fields: Vec<_> = Reader::new(&bytes).collect::<Result<_, _>>().unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].1.as_varint(), Some(300));
        let reference: Vec<_> = fields[1]
            .1
            .fields()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(reference[0].1.as_bytes(), Some(&b"app"[..]));
        let element: Vec<_> = reference[1]
            .1
            .fields()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        #[allow(clippy::cast_possible_wrap)]
        let id = element[1].1.as_varint().unwrap() as i64;
        assert_eq!(id, -3);
        assert_eq!(fields[2].1.as_fixed64(), Some(1.5f64.to_bits()));
    }

    #[test]
    fn truncated_input_is_malformed() {
        let mut w = Writer::default();
        w.bytes(3, b"hello");
        let bytes = w.into_bytes();
        assert!(Reader::new(&bytes[..4]).any(|f| f.is_err()));
        // A group without its end tag.
        assert!(Reader::new(&[0x63, 0x08, 0x01]).any(|f| f.is_err()));
    }
}
