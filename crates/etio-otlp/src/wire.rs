//! A minimal, zero-copy reader for the protobuf wire format.
//!
//! Everything returned borrows from the input buffer. All reads are bounds
//! checked and malformed input yields a [`WireError`], never a panic: the
//! reader sits directly behind a network socket.

/// Wire types (groups are deprecated and rejected).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WireType {
    /// Variable-length integer.
    Varint,
    /// Eight fixed bytes.
    I64,
    /// Length-delimited bytes.
    Len,
    /// Four fixed bytes.
    I32,
}

/// Malformed protobuf input.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The buffer ended in the middle of a value.
    #[error("truncated message")]
    Truncated,
    /// A varint longer than ten bytes or overflowing 64 bits.
    #[error("malformed varint")]
    Varint,
    /// An unknown or unsupported wire type.
    #[error("unsupported wire type {0}")]
    WireType(u8),
    /// Field number zero.
    #[error("invalid field number")]
    FieldNumber,
    /// Nested messages deeper than the configured limit.
    #[error("message nesting too deep")]
    TooDeep,
    /// A string field is not valid UTF-8.
    #[error("invalid UTF-8 in string field")]
    Utf8,
    /// A field has the wrong wire type for its declared type.
    #[error("field {0} has an unexpected wire type")]
    Unexpected(u32),
}

/// Result alias for wire reads.
pub type Result<T> = std::result::Result<T, WireError>;

/// A cursor over a protobuf-encoded message.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    /// A reader over `buf`.
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    /// Whether the message has been fully read.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Reads a varint.
    ///
    /// # Errors
    /// Fails on truncation or on encodings longer than ten bytes.
    #[inline]
    pub fn varint(&mut self) -> Result<u64> {
        // Fast path: most varints in OTLP (tags, enums, lengths) are one byte.
        if let Some(&b) = self.buf.first()
            && b < 0x80
        {
            self.buf = &self.buf[1..];
            return Ok(u64::from(b));
        }
        let mut value: u64 = 0;
        for i in 0..10 {
            let &b = self.buf.get(i).ok_or(WireError::Truncated)?;
            if i == 9 && b > 1 {
                return Err(WireError::Varint);
            }
            value |= u64::from(b & 0x7f) << (7 * i);
            if b < 0x80 {
                self.buf = &self.buf[i + 1..];
                return Ok(value);
            }
        }
        Err(WireError::Varint)
    }

    /// Reads a field key.
    ///
    /// # Errors
    /// Fails on malformed keys, field number zero, and group wire types.
    #[inline]
    pub fn key(&mut self) -> Result<(u32, WireType)> {
        let key = self.varint()?;
        let field = u32::try_from(key >> 3).map_err(|_| WireError::FieldNumber)?;
        if field == 0 {
            return Err(WireError::FieldNumber);
        }
        #[allow(clippy::cast_possible_truncation)]
        let wt = match (key & 7) as u8 {
            0 => WireType::Varint,
            1 => WireType::I64,
            2 => WireType::Len,
            5 => WireType::I32,
            other => return Err(WireError::WireType(other)),
        };
        Ok((field, wt))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.buf.len() < n {
            return Err(WireError::Truncated);
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    /// Reads eight little-endian bytes.
    ///
    /// # Errors
    /// Fails on truncation.
    #[inline]
    pub fn fixed64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(b.try_into().map_err(|_| WireError::Truncated)?))
    }

    /// Reads four little-endian bytes.
    ///
    /// # Errors
    /// Fails on truncation.
    #[inline]
    pub fn fixed32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes(b.try_into().map_err(|_| WireError::Truncated)?))
    }

    /// Reads a length-delimited field.
    ///
    /// # Errors
    /// Fails if the length exceeds the remaining input.
    #[inline]
    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let len = usize::try_from(self.varint()?).map_err(|_| WireError::Truncated)?;
        self.take(len)
    }

    /// Reads a UTF-8 string field.
    ///
    /// # Errors
    /// Fails on truncation or invalid UTF-8.
    #[inline]
    pub fn str(&mut self) -> Result<&'a str> {
        std::str::from_utf8(self.bytes()?).map_err(|_| WireError::Utf8)
    }

    /// Skips a field of the given wire type.
    ///
    /// # Errors
    /// Fails on truncation.
    #[inline]
    pub fn skip(&mut self, wt: WireType) -> Result<()> {
        match wt {
            WireType::Varint => self.varint().map(|_| ()),
            WireType::I64 => self.take(8).map(|_| ()),
            WireType::I32 => self.take(4).map(|_| ()),
            WireType::Len => self.bytes().map(|_| ()),
        }
    }

    /// Reads a length-delimited field, checking its wire type.
    ///
    /// # Errors
    /// Fails if the field is not length-delimited or is truncated.
    #[inline]
    pub fn expect_bytes(&mut self, field: u32, wt: WireType) -> Result<&'a [u8]> {
        if wt != WireType::Len {
            return Err(WireError::Unexpected(field));
        }
        self.bytes()
    }

    /// Reads a varint field, checking its wire type.
    ///
    /// # Errors
    /// Fails if the field is not a varint.
    #[inline]
    pub fn expect_varint(&mut self, field: u32, wt: WireType) -> Result<u64> {
        if wt != WireType::Varint {
            return Err(WireError::Unexpected(field));
        }
        self.varint()
    }

    /// Reads a 64-bit fixed field, checking its wire type.
    ///
    /// # Errors
    /// Fails if the field is not 64-bit fixed.
    #[inline]
    pub fn expect_fixed64(&mut self, field: u32, wt: WireType) -> Result<u64> {
        if wt != WireType::I64 {
            return Err(WireError::Unexpected(field));
        }
        self.fixed64()
    }
}

/// Offset of `slice` inside `base`, if it is a sub-slice of it.
#[must_use]
pub fn offset_in(base: &[u8], slice: &[u8]) -> Option<std::ops::Range<usize>> {
    let start = (slice.as_ptr() as usize).checked_sub(base.as_ptr() as usize)?;
    let end = start.checked_add(slice.len())?;
    (end <= base.len()).then_some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            #[allow(clippy::cast_possible_truncation)]
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    #[test]
    fn varints_round_trip() {
        for v in [0u64, 1, 127, 128, 300, 16_384, u64::from(u32::MAX), u64::MAX] {
            let mut buf = Vec::new();
            encode_varint(v, &mut buf);
            let mut r = Reader::new(&buf);
            assert_eq!(r.varint(), Ok(v));
            assert!(r.is_empty());
        }
    }

    #[test]
    fn malformed_varints_are_rejected() {
        assert_eq!(Reader::new(&[0x80]).varint(), Err(WireError::Truncated));
        let too_long = [0xff; 11];
        assert_eq!(Reader::new(&too_long).varint(), Err(WireError::Varint));
        let overflow = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02];
        assert_eq!(Reader::new(&overflow).varint(), Err(WireError::Varint));
    }

    #[test]
    fn keys_and_skips() {
        // field 1 varint 150, field 2 bytes "hi", field 3 fixed64, field 4 fixed32
        let mut buf = vec![0x08, 0x96, 0x01, 0x12, 0x02, b'h', b'i', 0x19];
        buf.extend_from_slice(&7u64.to_le_bytes());
        buf.push(0x25);
        buf.extend_from_slice(&9u32.to_le_bytes());
        let mut r = Reader::new(&buf);
        assert_eq!(r.key(), Ok((1, WireType::Varint)));
        assert_eq!(r.varint(), Ok(150));
        assert_eq!(r.key(), Ok((2, WireType::Len)));
        assert_eq!(r.str(), Ok("hi"));
        assert_eq!(r.key(), Ok((3, WireType::I64)));
        r.skip(WireType::I64).unwrap();
        assert_eq!(r.key(), Ok((4, WireType::I32)));
        assert_eq!(r.fixed32(), Ok(9));
        assert!(r.is_empty());
    }

    #[test]
    fn rejects_groups_zero_fields_and_bad_lengths() {
        assert_eq!(Reader::new(&[0x0b]).key(), Err(WireError::WireType(3)));
        assert_eq!(Reader::new(&[0x00]).key(), Err(WireError::FieldNumber));
        assert_eq!(Reader::new(&[0x05, b'a']).bytes(), Err(WireError::Truncated));
        assert_eq!(Reader::new(&[0x02, 0xff, 0xfe]).str(), Err(WireError::Utf8));
    }

    #[test]
    fn offsets_of_subslices() {
        let base = b"hello world";
        assert_eq!(offset_in(base, &base[6..]), Some(6..11));
        assert_eq!(offset_in(base, b"other"), None);
    }
}
