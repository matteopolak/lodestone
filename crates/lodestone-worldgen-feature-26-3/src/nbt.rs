//! A minimal reader for the gzip-compressed binary tag format structure templates are stored in.

use std::io::Read;

#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Bytes(Vec<u8>),
    Str(String),
    List(Vec<Tag>),
    Compound(Vec<(String, Tag)>),
    Ints(Vec<i32>),
    Longs(Vec<i64>),
}

impl Tag {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(c) => c.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_list(&self) -> &[Tag] {
        match self {
            Tag::List(l) => l,
            _ => &[],
        }
    }

    #[must_use]
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Tag::Int(i) => Some(*i),
            Tag::Short(i) => Some(i32::from(*i)),
            Tag::Byte(i) => Some(i32::from(*i)),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::Str(s) => Some(s),
            _ => None,
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.data.len()).ok_or("nbt: truncated")?;
        let out = &self.data[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn i8(&mut self) -> Result<i8, String> {
        Ok(self.take(1)?[0] as i8)
    }

    fn i16(&mut self) -> Result<i16, String> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().expect("two bytes")))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().expect("four bytes")))
    }

    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().expect("eight bytes")))
    }

    fn len(&mut self) -> Result<usize, String> {
        usize::try_from(self.i32()?).map_err(|_| "nbt: negative length".to_owned())
    }

    fn string(&mut self) -> Result<String, String> {
        let n = usize::from(self.i16()? as u16);
        String::from_utf8(self.take(n)?.to_vec()).map_err(|e| format!("nbt: {e}"))
    }

    fn payload(&mut self, kind: i8) -> Result<Tag, String> {
        Ok(match kind {
            1 => Tag::Byte(self.i8()?),
            2 => Tag::Short(self.i16()?),
            3 => Tag::Int(self.i32()?),
            4 => Tag::Long(self.i64()?),
            5 => Tag::Float(f32::from_bits(self.i32()? as u32)),
            6 => Tag::Double(f64::from_bits(self.i64()? as u64)),
            7 => {
                let n = self.len()?;
                Tag::Bytes(self.take(n)?.to_vec())
            }
            8 => Tag::Str(self.string()?),
            9 => {
                let element = self.i8()?;
                let n = self.len()?;
                Tag::List((0..n).map(|_| self.payload(element)).collect::<Result<_, _>>()?)
            }
            10 => {
                let mut out = Vec::new();
                loop {
                    let k = self.i8()?;
                    if k == 0 {
                        break Tag::Compound(out);
                    }
                    let name = self.string()?;
                    out.push((name, self.payload(k)?));
                }
            }
            11 => {
                let n = self.len()?;
                Tag::Ints((0..n).map(|_| self.i32()).collect::<Result<_, _>>()?)
            }
            12 => {
                let n = self.len()?;
                Tag::Longs((0..n).map(|_| self.i64()).collect::<Result<_, _>>()?)
            }
            other => return Err(format!("nbt: unknown tag type {other}")),
        })
    }
}

/// Decompresses and parses one gzip-compressed document; returns its root compound.
///
/// # Errors
/// On a malformed stream.
pub fn parse_gzip(bytes: &[u8]) -> Result<Tag, String> {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(bytes).read_to_end(&mut raw).map_err(|e| format!("nbt: {e}"))?;
    let mut r = Reader { data: &raw, at: 0 };
    let kind = r.i8()?;
    r.string()?;
    r.payload(kind)
}
