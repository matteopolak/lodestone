//! Core protocol codecs for Lodestone's Minecraft Java Edition client.

/// Protocol context threaded through every codec call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ctx {
    /// Minecraft protocol version.
    pub version: i32,
}

/// Convenient result alias for core codec operations.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors returned by protocol readers, writers, and codecs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A read needed more bytes than were available.
    #[error("unexpected end of input")]
    UnexpectedEof,
    /// A VarInt or VarLong used more bytes than Minecraft permits.
    #[error("varint is too long")]
    VarIntTooLong,
    /// A string payload was not valid UTF-8.
    #[error("invalid utf-8")]
    InvalidUtf8,
    /// An integer discriminant did not match a valid enum value.
    #[error("invalid {name} variant {value}")]
    InvalidEnumVariant {
        /// Name of the decoded enum-like value.
        name: &'static str,
        /// Invalid integer discriminant.
        value: i32,
    },
    /// A decoded value exceeded a caller-provided limit.
    #[error("limit exceeded: limit {limit}, actual {actual}")]
    LimitExceeded {
        /// Maximum permitted size.
        limit: usize,
        /// Actual decoded size.
        actual: usize,
    },
    /// A reader still contains trailing bytes after a complete decode.
    #[error("{0} trailing bytes")]
    TrailingBytes(usize),
    /// Catch-all error for validation failures without a dedicated variant.
    #[error("{0}")]
    Custom(String),
    /// A length prefix was negative.
    #[error("negative length {0}")]
    NegativeLength(i32),
    /// An NBT value exceeded the maximum permitted nesting depth.
    #[error("NBT depth exceeded limit {limit}")]
    NbtDepthExceeded {
        /// Maximum permitted nesting depth.
        limit: usize,
    },
}

/// Connection state (protocol phase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    /// Initial handshake phase.
    Handshaking,
    /// Server list ping phase.
    Status,
    /// Login and authentication phase.
    Login,
    /// Post-login configuration phase.
    Configuration,
    /// Main gameplay phase.
    Play,
}

/// Packet direction relative to an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bound {
    /// Packet is bound for the client endpoint.
    Client,
    /// Packet is bound for the server endpoint.
    Server,
}

/// Compile-time metadata for a protocol packet.
pub trait Packet {
    /// Stable Mojang resource name, e.g. `"minecraft:intention"`.
    const NAME: &'static str;

    /// Protocol phase this packet belongs to.
    const STATE: State;

    /// Direction this packet travels.
    const BOUND: Bound;
}

/// Maximum nesting depth permitted when decoding untrusted NBT input.
pub const NBT_MAX_DEPTH: usize = 512;

/// Standard binary NBT tag identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NbtTag {
    /// Marks the end of a compound.
    End = 0,
    /// Signed 8-bit integer tag.
    Byte = 1,
    /// Signed 16-bit integer tag.
    Short = 2,
    /// Signed 32-bit integer tag.
    Int = 3,
    /// Signed 64-bit integer tag.
    Long = 4,
    /// IEEE-754 32-bit float tag.
    Float = 5,
    /// IEEE-754 64-bit float tag.
    Double = 6,
    /// Length-prefixed array of signed bytes.
    ByteArray = 7,
    /// Modified UTF-8 string tag.
    String = 8,
    /// Homogeneous list tag.
    List = 9,
    /// Name/value compound tag.
    Compound = 10,
    /// Length-prefixed array of signed 32-bit integers.
    IntArray = 11,
    /// Length-prefixed array of signed 64-bit integers.
    LongArray = 12,
}

impl NbtTag {
    /// Returns the numeric NBT tag id.
    #[must_use]
    pub const fn id(self) -> u8 {
        self as u8
    }

    fn from_id(id: u8, name: &'static str) -> Result<Self> {
        match id {
            0 => Ok(Self::End),
            1 => Ok(Self::Byte),
            2 => Ok(Self::Short),
            3 => Ok(Self::Int),
            4 => Ok(Self::Long),
            5 => Ok(Self::Float),
            6 => Ok(Self::Double),
            7 => Ok(Self::ByteArray),
            8 => Ok(Self::String),
            9 => Ok(Self::List),
            10 => Ok(Self::Compound),
            11 => Ok(Self::IntArray),
            12 => Ok(Self::LongArray),
            value => Err(Error::InvalidEnumVariant {
                name,
                value: i32::from(value),
            }),
        }
    }

    fn minimum_payload_bytes(self) -> usize {
        match self {
            Self::End => 0,
            Self::Byte => 1,
            Self::Short => 2,
            Self::Int | Self::Float | Self::ByteArray | Self::List | Self::IntArray => 4,
            Self::Long | Self::Double | Self::LongArray => 8,
            Self::String => 2,
            Self::Compound => 1,
        }
    }
}

/// Owned binary NBT value.
#[derive(Debug, Clone, PartialEq)]
pub enum Nbt {
    /// End marker.
    End,
    /// Signed 8-bit integer.
    Byte(i8),
    /// Signed 16-bit integer.
    Short(i16),
    /// Signed 32-bit integer.
    Int(i32),
    /// Signed 64-bit integer.
    Long(i64),
    /// IEEE-754 32-bit float.
    Float(f32),
    /// IEEE-754 64-bit float.
    Double(f64),
    /// Signed byte array.
    ByteArray(Vec<i8>),
    /// Modified UTF-8 string.
    String(String),
    /// Homogeneous list.
    List {
        /// Element tag shared by every list item.
        element_type: NbtTag,
        /// List element payloads.
        elements: Vec<Nbt>,
    },
    /// Compound fields in wire order.
    Compound(Vec<(String, Nbt)>),
    /// Signed 32-bit integer array.
    IntArray(Vec<i32>),
    /// Signed 64-bit integer array.
    LongArray(Vec<i64>),
}

/// Reads network-form NBT: root tag id followed immediately by its payload.
pub fn read_network_nbt(r: &mut Reader<'_>) -> Result<Nbt> {
    let tag = NbtTag::from_id(r.u8()?, "nbt tag")?;
    read_nbt_payload(r, tag, 0)
}

/// Reads named-form NBT: root tag id, root name, then payload.
pub fn read_named_nbt(r: &mut Reader<'_>) -> Result<(String, Nbt)> {
    let tag = NbtTag::from_id(r.u8()?, "nbt tag")?;
    if tag == NbtTag::End {
        return Ok((String::new(), Nbt::End));
    }

    let name = read_modified_utf8_string(r)?;
    let value = read_nbt_payload(r, tag, 0)?;
    Ok((name, value))
}

/// Writes network-form NBT: root tag id followed immediately by its payload.
pub fn write_network_nbt(w: &mut Writer, value: &Nbt) -> Result<()> {
    let tag = value.tag();
    w.u8(tag.id());
    write_nbt_payload(w, value, 0)
}

/// Writes named-form NBT: root tag id, root name, then payload.
pub fn write_named_nbt(w: &mut Writer, name: &str, value: &Nbt) -> Result<()> {
    let tag = value.tag();
    w.u8(tag.id());
    if tag != NbtTag::End {
        write_modified_utf8_string(w, name)?;
        write_nbt_payload(w, value, 0)?;
    }
    Ok(())
}

/// Extracts plain text from a decoded Minecraft text-component NBT value.
#[must_use]
pub fn plain_text_from_nbt_component(component: &Nbt) -> String {
    let mut out = String::new();
    append_nbt_component_text(component, &mut out);
    out
}

impl Nbt {
    fn tag(&self) -> NbtTag {
        match self {
            Self::End => NbtTag::End,
            Self::Byte(_) => NbtTag::Byte,
            Self::Short(_) => NbtTag::Short,
            Self::Int(_) => NbtTag::Int,
            Self::Long(_) => NbtTag::Long,
            Self::Float(_) => NbtTag::Float,
            Self::Double(_) => NbtTag::Double,
            Self::ByteArray(_) => NbtTag::ByteArray,
            Self::String(_) => NbtTag::String,
            Self::List { .. } => NbtTag::List,
            Self::Compound(_) => NbtTag::Compound,
            Self::IntArray(_) => NbtTag::IntArray,
            Self::LongArray(_) => NbtTag::LongArray,
        }
    }
}

fn read_nbt_payload(r: &mut Reader<'_>, tag: NbtTag, depth: usize) -> Result<Nbt> {
    if depth > NBT_MAX_DEPTH {
        return Err(Error::NbtDepthExceeded {
            limit: NBT_MAX_DEPTH,
        });
    }

    match tag {
        NbtTag::End => Ok(Nbt::End),
        NbtTag::Byte => Ok(Nbt::Byte(r.i8()?)),
        NbtTag::Short => Ok(Nbt::Short(r.i16()?)),
        NbtTag::Int => Ok(Nbt::Int(r.i32()?)),
        NbtTag::Long => Ok(Nbt::Long(r.i64()?)),
        NbtTag::Float => Ok(Nbt::Float(r.f32()?)),
        NbtTag::Double => Ok(Nbt::Double(r.f64()?)),
        NbtTag::ByteArray => read_nbt_byte_array(r),
        NbtTag::String => Ok(Nbt::String(read_modified_utf8_string(r)?)),
        NbtTag::List => read_nbt_list(r, depth),
        NbtTag::Compound => read_nbt_compound(r, depth),
        NbtTag::IntArray => read_nbt_int_array(r),
        NbtTag::LongArray => read_nbt_long_array(r),
    }
}

fn read_nbt_byte_array(r: &mut Reader<'_>) -> Result<Nbt> {
    let len = read_nbt_length(r)?;
    ensure_nbt_length_fits_remaining(r, len, 1)?;
    Ok(Nbt::ByteArray(
        r.bytes(len)?.iter().map(|&byte| byte as i8).collect(),
    ))
}

fn read_nbt_int_array(r: &mut Reader<'_>) -> Result<Nbt> {
    let len = read_nbt_length(r)?;
    ensure_nbt_length_fits_remaining(r, len, 4)?;
    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        values.push(r.i32()?);
    }
    Ok(Nbt::IntArray(values))
}

fn read_nbt_long_array(r: &mut Reader<'_>) -> Result<Nbt> {
    let len = read_nbt_length(r)?;
    ensure_nbt_length_fits_remaining(r, len, 8)?;
    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        values.push(r.i64()?);
    }
    Ok(Nbt::LongArray(values))
}

fn read_nbt_list(r: &mut Reader<'_>, depth: usize) -> Result<Nbt> {
    let element_type = NbtTag::from_id(r.u8()?, "nbt list element type")?;
    let len = read_nbt_length(r)?;

    if element_type == NbtTag::End && len != 0 {
        return Err(Error::InvalidEnumVariant {
            name: "nbt list element type",
            value: i32::from(NbtTag::End.id()),
        });
    }

    ensure_nbt_length_fits_remaining(r, len, element_type.minimum_payload_bytes())?;

    let mut elements = Vec::with_capacity(len);
    for _ in 0..len {
        elements.push(read_nbt_payload(r, element_type, depth + 1)?);
    }

    Ok(Nbt::List {
        element_type,
        elements,
    })
}

fn read_nbt_compound(r: &mut Reader<'_>, depth: usize) -> Result<Nbt> {
    let mut fields = Vec::new();

    loop {
        let tag = NbtTag::from_id(r.u8()?, "nbt tag")?;
        if tag == NbtTag::End {
            break;
        }

        let name = read_modified_utf8_string(r)?;
        let value = read_nbt_payload(r, tag, depth + 1)?;
        fields.push((name, value));
    }

    Ok(Nbt::Compound(fields))
}

fn write_nbt_payload(w: &mut Writer, value: &Nbt, depth: usize) -> Result<()> {
    if depth > NBT_MAX_DEPTH {
        return Err(Error::NbtDepthExceeded {
            limit: NBT_MAX_DEPTH,
        });
    }

    match value {
        Nbt::End => {}
        Nbt::Byte(value) => w.i8(*value),
        Nbt::Short(value) => w.i16(*value),
        Nbt::Int(value) => w.i32(*value),
        Nbt::Long(value) => w.i64(*value),
        Nbt::Float(value) => w.f32(*value),
        Nbt::Double(value) => w.f64(*value),
        Nbt::ByteArray(values) => {
            write_nbt_len(w, values.len())?;
            for value in values {
                w.u8(*value as u8);
            }
        }
        Nbt::String(value) => write_modified_utf8_string(w, value)?,
        Nbt::List {
            element_type,
            elements,
        } => write_nbt_list(w, *element_type, elements, depth)?,
        Nbt::Compound(fields) => {
            for (name, value) in fields {
                let tag = value.tag();
                if tag == NbtTag::End {
                    return Err(Error::InvalidEnumVariant {
                        name: "nbt tag",
                        value: i32::from(NbtTag::End.id()),
                    });
                }
                w.u8(tag.id());
                write_modified_utf8_string(w, name)?;
                write_nbt_payload(w, value, depth + 1)?;
            }
            w.u8(NbtTag::End.id());
        }
        Nbt::IntArray(values) => {
            write_nbt_len(w, values.len())?;
            for value in values {
                w.i32(*value);
            }
        }
        Nbt::LongArray(values) => {
            write_nbt_len(w, values.len())?;
            for value in values {
                w.i64(*value);
            }
        }
    }

    Ok(())
}

fn write_nbt_list(
    w: &mut Writer,
    element_type: NbtTag,
    elements: &[Nbt],
    depth: usize,
) -> Result<()> {
    if element_type == NbtTag::End && !elements.is_empty() {
        return Err(Error::InvalidEnumVariant {
            name: "nbt list element type",
            value: i32::from(NbtTag::End.id()),
        });
    }

    for element in elements {
        let actual = element.tag();
        if actual != element_type {
            return Err(Error::InvalidEnumVariant {
                name: "nbt list element type",
                value: i32::from(actual.id()),
            });
        }
    }

    w.u8(element_type.id());
    write_nbt_len(w, elements.len())?;
    for element in elements {
        write_nbt_payload(w, element, depth + 1)?;
    }
    Ok(())
}

fn write_nbt_len(w: &mut Writer, len: usize) -> Result<()> {
    let len = i32::try_from(len).map_err(|_| Error::LimitExceeded {
        limit: i32::MAX as usize,
        actual: len,
    })?;
    w.i32(len);
    Ok(())
}

fn read_nbt_length(r: &mut Reader<'_>) -> Result<usize> {
    let len = r.i32()?;
    if len < 0 {
        return Err(Error::NegativeLength(len));
    }
    usize::try_from(len).map_err(|_| Error::LimitExceeded {
        limit: r.remaining(),
        actual: usize::MAX,
    })
}

fn ensure_nbt_length_fits_remaining(
    r: &Reader<'_>,
    len: usize,
    minimum_element_bytes: usize,
) -> Result<()> {
    let required = len
        .checked_mul(minimum_element_bytes)
        .ok_or(Error::LimitExceeded {
            limit: r.remaining(),
            actual: len,
        })?;
    if required > r.remaining() {
        return Err(Error::LimitExceeded {
            limit: r.remaining(),
            actual: if minimum_element_bytes == 1 {
                len
            } else {
                required
            },
        });
    }
    Ok(())
}

fn read_modified_utf8_string(r: &mut Reader<'_>) -> Result<String> {
    let len = usize::from(r.u16()?);
    decode_modified_utf8(r.bytes(len)?)
}

fn write_modified_utf8_string(w: &mut Writer, value: &str) -> Result<()> {
    let mut encoded = Vec::with_capacity(value.len());
    for unit in value.encode_utf16() {
        match unit {
            0 => encoded.extend_from_slice(&[0xc0, 0x80]),
            0x0001..=0x007f => encoded.push(unit as u8),
            0x0080..=0x07ff => {
                encoded.push((0xc0 | ((unit >> 6) & 0x1f)) as u8);
                encoded.push((0x80 | (unit & 0x3f)) as u8);
            }
            _ => {
                encoded.push((0xe0 | ((unit >> 12) & 0x0f)) as u8);
                encoded.push((0x80 | ((unit >> 6) & 0x3f)) as u8);
                encoded.push((0x80 | (unit & 0x3f)) as u8);
            }
        }
    }

    let len = u16::try_from(encoded.len()).map_err(|_| Error::LimitExceeded {
        limit: u16::MAX as usize,
        actual: encoded.len(),
    })?;
    w.u16(len);
    w.bytes(&encoded);
    Ok(())
}

fn decode_modified_utf8(bytes: &[u8]) -> Result<String> {
    let mut units = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let first = bytes[i];
        if first == 0 {
            return Err(Error::InvalidUtf8);
        }

        if first & 0x80 == 0 {
            units.push(u16::from(first));
            i += 1;
        } else if first & 0xe0 == 0xc0 {
            let second = *bytes.get(i + 1).ok_or(Error::InvalidUtf8)?;
            if second & 0xc0 != 0x80 {
                return Err(Error::InvalidUtf8);
            }
            let unit = (u16::from(first & 0x1f) << 6) | u16::from(second & 0x3f);
            if unit != 0 && unit < 0x80 {
                return Err(Error::InvalidUtf8);
            }
            units.push(unit);
            i += 2;
        } else if first & 0xf0 == 0xe0 {
            let second = *bytes.get(i + 1).ok_or(Error::InvalidUtf8)?;
            let third = *bytes.get(i + 2).ok_or(Error::InvalidUtf8)?;
            if second & 0xc0 != 0x80 || third & 0xc0 != 0x80 {
                return Err(Error::InvalidUtf8);
            }
            let unit = (u16::from(first & 0x0f) << 12)
                | (u16::from(second & 0x3f) << 6)
                | u16::from(third & 0x3f);
            if unit < 0x800 {
                return Err(Error::InvalidUtf8);
            }
            units.push(unit);
            i += 3;
        } else {
            return Err(Error::InvalidUtf8);
        }
    }

    String::from_utf16(&units).map_err(|_| Error::InvalidUtf8)
}

fn append_nbt_component_text(component: &Nbt, out: &mut String) {
    match component {
        Nbt::String(value) => out.push_str(value),
        Nbt::List { elements, .. } => {
            for element in elements {
                append_nbt_component_text(element, out);
            }
        }
        Nbt::Compound(fields) => {
            for (_, value) in fields.iter().filter(|(name, _)| name == "text") {
                if let Nbt::String(text) = value {
                    out.push_str(text);
                }
            }
            for (_, value) in fields.iter().filter(|(name, _)| name == "extra") {
                append_nbt_component_text(value, out);
            }
        }
        Nbt::End
        | Nbt::Byte(_)
        | Nbt::Short(_)
        | Nbt::Int(_)
        | Nbt::Long(_)
        | Nbt::Float(_)
        | Nbt::Double(_)
        | Nbt::ByteArray(_)
        | Nbt::IntArray(_)
        | Nbt::LongArray(_) => {}
    }
}

/// Borrowed, bounds-checked protocol byte reader.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    /// Creates a reader over `bytes`.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    /// Returns the number of unread bytes.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.cursor)
    }

    /// Returns true when there are no unread bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Returns the current byte offset.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.cursor
    }

    /// Reads one unsigned byte.
    pub fn u8(&mut self) -> Result<u8> {
        let bytes = self.bytes(1)?;
        Ok(bytes[0])
    }

    /// Reads one signed byte.
    pub fn i8(&mut self) -> Result<i8> {
        Ok(i8::from_be_bytes([self.u8()?]))
    }

    /// Reads a big-endian unsigned 16-bit integer.
    pub fn u16(&mut self) -> Result<u16> {
        let mut bytes = [0; 2];
        bytes.copy_from_slice(self.bytes(2)?);
        Ok(u16::from_be_bytes(bytes))
    }

    /// Reads a big-endian signed 16-bit integer.
    pub fn i16(&mut self) -> Result<i16> {
        let mut bytes = [0; 2];
        bytes.copy_from_slice(self.bytes(2)?);
        Ok(i16::from_be_bytes(bytes))
    }

    /// Reads a big-endian unsigned 32-bit integer.
    pub fn u32(&mut self) -> Result<u32> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.bytes(4)?);
        Ok(u32::from_be_bytes(bytes))
    }

    /// Reads a big-endian signed 32-bit integer.
    pub fn i32(&mut self) -> Result<i32> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.bytes(4)?);
        Ok(i32::from_be_bytes(bytes))
    }

    /// Reads a big-endian unsigned 64-bit integer.
    pub fn u64(&mut self) -> Result<u64> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.bytes(8)?);
        Ok(u64::from_be_bytes(bytes))
    }

    /// Reads a big-endian signed 64-bit integer.
    pub fn i64(&mut self) -> Result<i64> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.bytes(8)?);
        Ok(i64::from_be_bytes(bytes))
    }

    /// Reads a big-endian IEEE-754 32-bit float.
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }

    /// Reads a big-endian IEEE-754 64-bit float.
    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    /// Reads a Minecraft boolean (`0` is false, `1` is true).
    pub fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(Error::InvalidEnumVariant {
                name: "bool",
                value: i32::from(value),
            }),
        }
    }

    /// Reads a Minecraft VarInt.
    pub fn var_i32(&mut self) -> Result<i32> {
        let mut result = 0_u32;

        for shift in (0..35).step_by(7).take(5) {
            let byte = self.u8()?;
            result |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result as i32);
            }
        }

        Err(Error::VarIntTooLong)
    }

    /// Reads a Minecraft VarLong.
    pub fn var_i64(&mut self) -> Result<i64> {
        let mut result = 0_u64;

        for shift in (0..70).step_by(7).take(10) {
            let byte = self.u8()?;
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result as i64);
            }
        }

        Err(Error::VarIntTooLong)
    }

    /// Reads a length-prefixed UTF-8 string with a maximum character count.
    pub fn string(&mut self, max_chars: usize) -> Result<String> {
        let byte_len = self.non_negative_var_len()?;
        let byte_limit = max_chars.saturating_mul(4);
        if byte_len > byte_limit {
            return Err(Error::LimitExceeded {
                limit: byte_limit,
                actual: byte_len,
            });
        }

        let bytes = self.bytes(byte_len)?;
        let value = core::str::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)?;
        let char_count = value.chars().count();
        if char_count > max_chars {
            return Err(Error::LimitExceeded {
                limit: max_chars,
                actual: char_count,
            });
        }

        Ok(value.to_owned())
    }

    /// Reads a VarInt length-prefixed borrowed byte slice bounded by `max_len`.
    pub fn var_bytes(&mut self, max_len: usize) -> Result<&'a [u8]> {
        let len = self.non_negative_var_len()?;
        if len > max_len {
            return Err(Error::LimitExceeded {
                limit: max_len,
                actual: len,
            });
        }
        self.bytes(len)
    }

    /// Reads exactly `n` borrowed bytes.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .cursor
            .checked_add(n)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(Error::UnexpectedEof)?;
        let start = self.cursor;
        self.cursor = end;
        Ok(&self.bytes[start..end])
    }

    /// Returns all unread bytes without advancing the cursor.
    #[must_use]
    pub fn remaining_bytes(&self) -> &'a [u8] {
        &self.bytes[self.cursor..]
    }

    /// Reads a UUID encoded as two big-endian u64 values.
    pub fn uuid(&mut self) -> Result<uuid::Uuid> {
        let most = self.u64()?;
        let least = self.u64()?;
        Ok(uuid::Uuid::from_u128(
            (u128::from(most) << 64) | u128::from(least),
        ))
    }

    /// Takes exactly `n` bytes and returns a reader over that sub-slice.
    pub fn take_reader(&mut self, n: usize) -> Result<Reader<'a>> {
        Ok(Self::new(self.bytes(n)?))
    }

    /// Returns an error when unread bytes remain.
    pub fn ensure_empty(&self) -> Result<()> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(Error::TrailingBytes(self.remaining()))
        }
    }

    fn non_negative_var_len(&mut self) -> Result<usize> {
        let len = self.var_i32()?;
        if len < 0 {
            return Err(Error::NegativeLength(len));
        }
        usize::try_from(len).map_err(|_| Error::LimitExceeded {
            limit: self.remaining(),
            actual: usize::MAX,
        })
    }
}

/// Owned byte writer for Minecraft protocol values.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    /// Consumes the writer and returns its accumulated bytes.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the accumulated bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the number of accumulated bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns true when the writer contains no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Clears all accumulated bytes.
    pub fn clear(&mut self) {
        self.bytes.clear();
    }

    /// Writes one unsigned byte.
    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    /// Writes one signed byte.
    pub fn i8(&mut self, value: i8) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes a big-endian unsigned 16-bit integer.
    pub fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes a big-endian signed 16-bit integer.
    pub fn i16(&mut self, value: i16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes a big-endian unsigned 32-bit integer.
    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes a big-endian signed 32-bit integer.
    pub fn i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes a big-endian unsigned 64-bit integer.
    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes a big-endian signed 64-bit integer.
    pub fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes a big-endian IEEE-754 32-bit float.
    pub fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }

    /// Writes a big-endian IEEE-754 64-bit float.
    pub fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }

    /// Writes a Minecraft boolean.
    pub fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    /// Writes a Minecraft VarInt.
    pub fn var_i32(&mut self, value: i32) {
        let mut value = value as u32;
        loop {
            if value & !0x7f == 0 {
                self.u8(value as u8);
                return;
            }

            self.u8(((value & 0x7f) | 0x80) as u8);
            value >>= 7;
        }
    }

    /// Writes a Minecraft VarLong.
    pub fn var_i64(&mut self, value: i64) {
        let mut value = value as u64;
        loop {
            if value & !0x7f == 0 {
                self.u8(value as u8);
                return;
            }

            self.u8(((value & 0x7f) | 0x80) as u8);
            value >>= 7;
        }
    }

    /// Writes a VarInt length-prefixed UTF-8 string.
    pub fn string(&mut self, value: &str) {
        let len = i32::try_from(value.len()).expect("string byte length exceeds i32::MAX");
        self.var_i32(len);
        self.bytes(value.as_bytes());
    }

    /// Writes a VarInt length-prefixed byte slice.
    pub fn var_bytes(&mut self, value: &[u8]) -> Result<()> {
        let len = i32::try_from(value.len()).map_err(|_| Error::LimitExceeded {
            limit: i32::MAX as usize,
            actual: value.len(),
        })?;
        self.var_i32(len);
        self.bytes(value);
        Ok(())
    }

    /// Writes raw bytes without a length prefix.
    pub fn bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    /// Writes a UUID as two big-endian u64 values.
    pub fn uuid(&mut self, value: uuid::Uuid) {
        let value = value.as_u128();
        self.u64((value >> 64) as u64);
        self.u64(value as u64);
    }
}

/// Encodes a value to a protocol writer.
pub trait Encode {
    /// Encodes `self` into `w` using `ctx`.
    fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()>;
}

/// Decodes an owned value from a protocol reader.
pub trait Decode: Sized {
    /// Decodes `Self` from `r` using `ctx`.
    fn decode(r: &mut Reader<'_>, ctx: Ctx) -> Result<Self>;
}

/// Returns the number of bytes `v` occupies as a Minecraft VarInt.
#[must_use]
pub fn var_i32_len(v: i32) -> usize {
    let mut len = 1;
    let mut value = v as u32;
    while value & !0x7f != 0 {
        len += 1;
        value >>= 7;
    }
    len
}

macro_rules! fixed_codec {
    ($ty:ty, $method:ident) => {
        impl Encode for $ty {
            fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
                w.$method(*self);
                Ok(())
            }
        }

        impl Decode for $ty {
            fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
                r.$method()
            }
        }
    };
}

fixed_codec!(u8, u8);
fixed_codec!(i8, i8);
fixed_codec!(u16, u16);
fixed_codec!(i16, i16);
fixed_codec!(u32, u32);
fixed_codec!(i32, i32);
fixed_codec!(u64, u64);
fixed_codec!(i64, i64);
fixed_codec!(f32, f32);
fixed_codec!(f64, f64);
fixed_codec!(bool, bool);

impl Encode for String {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        encode_string(self, w)
    }
}

impl Encode for str {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        encode_string(self, w)
    }
}

impl Decode for String {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        r.string(DEFAULT_STRING_MAX_CHARS)
    }
}

impl Encode for uuid::Uuid {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        w.uuid(*self);
        Ok(())
    }
}

impl Decode for uuid::Uuid {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        r.uuid()
    }
}

impl Encode for Vec<u8> {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        let len = i32::try_from(self.len()).map_err(|_| Error::LimitExceeded {
            limit: i32::MAX as usize,
            actual: self.len(),
        })?;
        w.var_i32(len);
        w.bytes(self);
        Ok(())
    }
}

impl Decode for Vec<u8> {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let len = r.non_negative_var_len()?;
        Ok(r.bytes(len)?.to_vec())
    }
}

impl<T: Encode> Encode for Option<T> {
    fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()> {
        match self {
            Some(value) => {
                w.bool(true);
                value.encode(w, ctx)
            }
            None => {
                w.bool(false);
                Ok(())
            }
        }
    }
}

impl<T: Decode> Decode for Option<T> {
    fn decode(r: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        if r.bool()? {
            Ok(Some(T::decode(r, ctx)?))
        } else {
            Ok(None)
        }
    }
}

const DEFAULT_STRING_MAX_CHARS: usize = 32_767;

fn encode_string(value: &str, w: &mut Writer) -> Result<()> {
    let char_count = value.chars().count();
    if char_count > DEFAULT_STRING_MAX_CHARS {
        return Err(Error::LimitExceeded {
            limit: DEFAULT_STRING_MAX_CHARS,
            actual: char_count,
        });
    }

    let byte_limit = DEFAULT_STRING_MAX_CHARS * 4;
    if value.len() > byte_limit {
        return Err(Error::LimitExceeded {
            limit: byte_limit,
            actual: value.len(),
        });
    }

    w.string(value);
    Ok(())
}

/// Encodes a packet body into a fresh byte buffer.
///
/// Shared plumbing for every version crate's adapter: each crate wraps this in
/// a same-named local `encode_body` that maps the `String` error into its own
/// `AdapterError::Encode`, because `AdapterError` lives in `lodestone-model`
/// and `lodestone-core` cannot depend on it (`lodestone-model` already depends
/// on `lodestone-core`; the reverse edge would be a cycle). The stringified
/// error keeps the wrapper a one-line `.map_err` with no behavior change.
pub fn encode_body<T: Encode>(packet: &T, ctx: Ctx) -> std::result::Result<Vec<u8>, String> {
    let mut writer = Writer::default();
    packet.encode(&mut writer, ctx).map_err(|err| err.to_string())?;
    Ok(writer.into_vec())
}

/// Decodes a packet body from raw bytes.
///
/// See [`encode_body`] for why callers wrap this rather than depending on it
/// directly with an `AdapterError`-typed return.
pub fn decode_body<T: Decode>(payload: &[u8], ctx: Ctx) -> std::result::Result<T, String> {
    let mut reader = Reader::new(payload);
    T::decode(&mut reader, ctx).map_err(|err| err.to_string())
}

/// Like [`decode_body`] but additionally requires the payload to be fully
/// consumed. Used for packets whose whole body is decoded (e.g. an entity
/// destroy id list), where trailing bytes signal a wrong layout and must be
/// rejected rather than silently ignored. Packets that deliberately leave a
/// tail unread (metadata terminators, fields not yet modeled) keep using the
/// lenient [`decode_body`].
pub fn decode_body_exact<T: Decode>(payload: &[u8], ctx: Ctx) -> std::result::Result<T, String> {
    let mut reader = Reader::new(payload);
    let body = T::decode(&mut reader, ctx).map_err(|err| err.to_string())?;
    reader.ensure_empty().map_err(|err| err.to_string())?;
    Ok(body)
}

/// Converts a signed-byte angle to degrees. The wire packs a full circle into
/// 256 steps, so a byte of `64` is 90 degrees.
#[must_use]
pub fn unpack_degrees(packed: i8) -> f32 {
    f32::from(packed) * 360.0 / 256.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_one<T: Decode>(bytes: &[u8]) -> T {
        let mut reader = Reader::new(bytes);
        let value = T::decode(&mut reader, Ctx { version: 0 }).expect("decode succeeds");
        assert!(reader.is_empty(), "decode left trailing bytes");
        value
    }

    #[test]
    fn var_i32_matches_vanilla_vectors_and_round_trips() {
        let cases: &[(i32, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (2, &[0x02]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (255, &[0xff, 0x01]),
            (25565, &[0xdd, 0xc7, 0x01]),
            (2_097_151, &[0xff, 0xff, 0x7f]),
            (2_147_483_647, &[0xff, 0xff, 0xff, 0xff, 0x07]),
            (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
            (-2_147_483_648, &[0x80, 0x80, 0x80, 0x80, 0x08]),
        ];

        for &(value, expected) in cases {
            let mut writer = Writer::default();
            writer.var_i32(value);
            assert_eq!(writer.as_slice(), expected, "encoded {value}");
            assert_eq!(var_i32_len(value), expected.len(), "length for {value}");

            let mut reader = Reader::new(expected);
            assert_eq!(reader.var_i32().expect("varint decodes"), value);
            assert!(reader.is_empty());
        }
    }

    #[test]
    fn var_i64_matches_vanilla_vectors_and_round_trips() {
        let cases: &[(i64, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (2, &[0x02]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (
                9_223_372_036_854_775_807,
                &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
            ),
            (
                -1,
                &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
            ),
            (
                -9_223_372_036_854_775_808,
                &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
            ),
        ];

        for &(value, expected) in cases {
            let mut writer = Writer::default();
            writer.var_i64(value);
            assert_eq!(writer.as_slice(), expected, "encoded {value}");

            let mut reader = Reader::new(expected);
            assert_eq!(reader.var_i64().expect("varlong decodes"), value);
            assert!(reader.is_empty());
        }
    }

    #[test]
    fn over_long_varints_are_rejected() {
        let mut i32_reader = Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
        assert!(matches!(i32_reader.var_i32(), Err(Error::VarIntTooLong)));

        let mut i64_reader = Reader::new(&[
            0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00,
        ]);
        assert!(matches!(i64_reader.var_i64(), Err(Error::VarIntTooLong)));
    }

    #[test]
    fn reading_past_end_returns_unexpected_eof_without_panicking() {
        let mut empty = Reader::new(&[]);
        assert!(matches!(empty.u8(), Err(Error::UnexpectedEof)));

        let mut partial = Reader::new(&[0x01, 0x02, 0x03]);
        assert!(matches!(partial.i32(), Err(Error::UnexpectedEof)));
        assert_eq!(
            partial.position(),
            0,
            "failed fixed-width read does not advance"
        );

        let mut partial_bytes = Reader::new(&[0xaa]);
        assert!(matches!(partial_bytes.bytes(2), Err(Error::UnexpectedEof)));
        assert_eq!(partial_bytes.remaining(), 1);
    }

    #[test]
    fn primitive_writers_use_big_endian_order() {
        let mut writer = Writer::default();
        writer.i32(0x0102_0304);
        writer.u64(0x0102_0304_0506_0708);
        writer.f32(f32::from_bits(0x0102_0304));
        assert_eq!(
            writer.as_slice(),
            &[
                0x01, 0x02, 0x03, 0x04, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x01, 0x02,
                0x03, 0x04
            ],
        );
    }

    #[test]
    fn every_primitive_encode_decode_round_trips() {
        let ctx = Ctx { version: 765 };

        macro_rules! round_trip {
            ($value:expr, $ty:ty) => {{
                let value: $ty = $value;
                let mut writer = Writer::default();
                value.encode(&mut writer, ctx).expect("encode succeeds");
                let decoded: $ty = decode_one(writer.as_slice());
                assert_eq!(decoded, value);
            }};
        }

        round_trip!(0xab_u8, u8);
        round_trip!(-12_i8, i8);
        round_trip!(0xabcd_u16, u16);
        round_trip!(-12_345_i16, i16);
        round_trip!(0x89ab_cdef_u32, u32);
        round_trip!(-123_456_789_i32, i32);
        round_trip!(0x0123_4567_89ab_cdef_u64, u64);
        round_trip!(-123_456_789_012_345_i64, i64);
        round_trip!(123.5_f32, f32);
        round_trip!(-9876.25_f64, f64);
        round_trip!(true, bool);
        round_trip!(false, bool);
        round_trip!(String::from("hello é"), String);

        let uuid = uuid::Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
        round_trip!(uuid, uuid::Uuid);
    }

    #[test]
    fn bool_rejects_values_other_than_zero_or_one() {
        let mut reader = Reader::new(&[2]);
        assert!(matches!(
            reader.bool(),
            Err(Error::InvalidEnumVariant {
                name: "bool",
                value: 2
            })
        ));
    }

    #[test]
    fn strings_enforce_byte_and_character_limits() {
        let mut writer = Writer::default();
        writer.string("abc");
        let mut reader = Reader::new(writer.as_slice());
        assert_eq!(reader.string(3).expect("string decodes"), "abc");

        let mut too_many_bytes = Writer::default();
        too_many_bytes.var_i32(5);
        too_many_bytes.bytes("hello".as_bytes());
        let mut reader = Reader::new(too_many_bytes.as_slice());
        assert!(matches!(
            reader.string(1),
            Err(Error::LimitExceeded {
                limit: 4,
                actual: 5
            })
        ));

        let mut too_many_chars = Writer::default();
        too_many_chars.string("éé");
        let mut reader = Reader::new(too_many_chars.as_slice());
        assert!(matches!(
            reader.string(1),
            Err(Error::LimitExceeded {
                limit: 1,
                actual: 2
            })
        ));

        let mut valid_multibyte = Writer::default();
        valid_multibyte.string("é");
        let mut reader = Reader::new(valid_multibyte.as_slice());
        assert_eq!(reader.string(1).expect("one multibyte char decodes"), "é");
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let mut reader = Reader::new(&[0x02, 0xff, 0xff]);
        assert!(matches!(reader.string(10), Err(Error::InvalidUtf8)));
    }

    #[test]
    fn bytes_and_remaining_bytes_are_borrowed_and_bounded() {
        let mut reader = Reader::new(&[1, 2, 3, 4]);
        assert_eq!(reader.bytes(2).expect("two bytes"), &[1, 2]);
        assert_eq!(reader.remaining_bytes(), &[3, 4]);
        assert_eq!(reader.position(), 2);
    }

    #[test]
    fn var_bytes_enforces_explicit_anti_dos_limit_before_borrowing() {
        let mut writer = Writer::default();
        writer.var_bytes(&[1, 2, 3]).expect("var bytes encodes");
        let mut reader = Reader::new(writer.as_slice());
        assert_eq!(reader.var_bytes(3).expect("var bytes decodes"), &[1, 2, 3]);

        let mut huge = Reader::new(&[0xff, 0xff, 0xff, 0xff, 0x07]);
        assert!(matches!(
            huge.var_bytes(16),
            Err(Error::LimitExceeded {
                limit: 16,
                actual: 2_147_483_647
            })
        ));

        let mut negative = Reader::new(&[0xff, 0xff, 0xff, 0xff, 0x0f]);
        assert!(matches!(
            negative.var_bytes(16),
            Err(Error::NegativeLength(-1))
        ));
    }

    #[test]
    fn uuid_reader_writer_use_two_big_endian_u64s() {
        let uuid = uuid::Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
        let mut writer = Writer::default();
        writer.uuid(uuid);
        assert_eq!(
            writer.as_slice(),
            &[
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ],
        );

        let mut reader = Reader::new(writer.as_slice());
        assert_eq!(reader.uuid().expect("uuid decodes"), uuid);
    }

    #[test]
    fn option_uses_bool_presence_prefix() {
        let ctx = Ctx { version: 0 };

        let present = Some(25565_i32);
        let mut writer = Writer::default();
        present.encode(&mut writer, ctx).expect("option encodes");
        assert_eq!(writer.as_slice(), &[1, 0x00, 0x00, 0x63, 0xdd]);
        let decoded: Option<i32> = decode_one(writer.as_slice());
        assert_eq!(decoded, present);

        let absent: Option<i32> = None;
        writer.clear();
        absent.encode(&mut writer, ctx).expect("option encodes");
        assert_eq!(writer.as_slice(), &[0]);
        let decoded: Option<i32> = decode_one(writer.as_slice());
        assert_eq!(decoded, absent);
    }

    #[test]
    fn vec_u8_codec_uses_varint_length_prefix() {
        let ctx = Ctx { version: 0 };
        let value = vec![1, 2, 3, 4, 5];
        let mut writer = Writer::default();
        value.encode(&mut writer, ctx).expect("vec encodes");
        assert_eq!(writer.as_slice(), &[5, 1, 2, 3, 4, 5]);

        let decoded: Vec<u8> = decode_one(writer.as_slice());
        assert_eq!(decoded, value);
    }

    #[test]
    fn reader_can_take_bounded_sub_reader_and_detect_trailing_bytes() {
        let mut reader = Reader::new(&[1, 2, 3]);
        let mut sub = reader.take_reader(2).expect("sub reader");
        assert_eq!(sub.u8().expect("first byte"), 1);
        assert_eq!(sub.remaining_bytes(), &[2]);
        assert_eq!(reader.remaining_bytes(), &[3]);

        assert!(matches!(sub.ensure_empty(), Err(Error::TrailingBytes(1))));
    }

    #[test]
    fn writer_len_clear_into_vec_and_as_slice_work() {
        let mut writer = Writer::default();
        assert_eq!(writer.len(), 0);
        writer.bytes(&[1, 2, 3]);
        assert_eq!(writer.len(), 3);
        assert_eq!(writer.as_slice(), &[1, 2, 3]);
        writer.clear();
        assert_eq!(writer.as_slice(), &[]);
        writer.u8(9);
        assert_eq!(writer.into_vec(), vec![9]);
    }

    #[test]
    fn state_and_bound_are_usable_in_const_context() {
        const STATE: State = State::Configuration;
        const BOUND: Bound = Bound::Server;

        assert_eq!(STATE, State::Configuration);
        assert_eq!(BOUND, Bound::Server);
    }

    #[test]
    fn packet_trait_exposes_compile_time_metadata() {
        struct SomeTestStruct;

        impl Packet for SomeTestStruct {
            const NAME: &'static str = "minecraft:intention";
            const STATE: State = State::Handshaking;
            const BOUND: Bound = Bound::Server;
        }

        assert_eq!(<SomeTestStruct as Packet>::NAME, "minecraft:intention");
        assert_eq!(<SomeTestStruct as Packet>::STATE, State::Handshaking);
        assert_eq!(<SomeTestStruct as Packet>::BOUND, Bound::Server);
    }

    fn push_name(bytes: &mut Vec<u8>, name: &str) {
        let encoded = test_modified_utf8(name);
        bytes.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&encoded);
    }

    fn push_string_payload(bytes: &mut Vec<u8>, value: &str) {
        push_name(bytes, value);
    }

    fn push_named_string(bytes: &mut Vec<u8>, name: &str, value: &str) {
        bytes.push(8);
        push_name(bytes, name);
        push_string_payload(bytes, value);
    }

    fn test_modified_utf8(value: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in value.encode_utf16() {
            match unit {
                0 => out.extend_from_slice(&[0xc0, 0x80]),
                0x0001..=0x007f => out.push(unit as u8),
                0x0080..=0x07ff => {
                    out.push((0xc0 | ((unit >> 6) & 0x1f)) as u8);
                    out.push((0x80 | (unit & 0x3f)) as u8);
                }
                _ => {
                    out.push((0xe0 | ((unit >> 12) & 0x0f)) as u8);
                    out.push((0x80 | ((unit >> 6) & 0x3f)) as u8);
                    out.push((0x80 | (unit & 0x3f)) as u8);
                }
            }
        }
        out
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        let compact: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(compact.len() % 2, 0);
        compact
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text = core::str::from_utf8(pair).expect("hex is ascii");
                u8::from_str_radix(text, 16).expect("valid hex byte")
            })
            .collect()
    }

    #[test]
    fn nbt_network_and_named_roots_decode_to_same_value() {
        let mut network = vec![10];
        push_named_string(&mut network, "text", "hi");
        network.push(0);

        let mut named = vec![10];
        push_name(&mut named, "root");
        push_named_string(&mut named, "text", "hi");
        named.push(0);

        let network_value =
            read_network_nbt(&mut Reader::new(&network)).expect("network NBT decodes");
        let (name, named_value) =
            read_named_nbt(&mut Reader::new(&named)).expect("named NBT decodes");

        assert_eq!(name, "root");
        assert_eq!(network_value, named_value);
    }

    #[test]
    fn nbt_compound_containing_every_tag_type_decodes() {
        let mut bytes = vec![10];

        bytes.push(1);
        push_name(&mut bytes, "byte");
        bytes.push(0x7f);

        bytes.push(2);
        push_name(&mut bytes, "short");
        bytes.extend_from_slice(&0x0102_i16.to_be_bytes());

        bytes.push(3);
        push_name(&mut bytes, "int");
        bytes.extend_from_slice(&0x0102_0304_i32.to_be_bytes());

        bytes.push(4);
        push_name(&mut bytes, "long");
        bytes.extend_from_slice(&0x0102_0304_0506_0708_i64.to_be_bytes());

        bytes.push(5);
        push_name(&mut bytes, "float");
        bytes.extend_from_slice(&1.5_f32.to_bits().to_be_bytes());

        bytes.push(6);
        push_name(&mut bytes, "double");
        bytes.extend_from_slice(&(-2.25_f64).to_bits().to_be_bytes());

        bytes.push(7);
        push_name(&mut bytes, "bytes");
        bytes.extend_from_slice(&3_i32.to_be_bytes());
        bytes.extend_from_slice(&[1, 0xff, 0x7f]);

        push_named_string(&mut bytes, "string", "hello");

        bytes.push(9);
        push_name(&mut bytes, "list");
        bytes.push(3);
        bytes.extend_from_slice(&2_i32.to_be_bytes());
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        bytes.extend_from_slice(&2_i32.to_be_bytes());

        bytes.push(10);
        push_name(&mut bytes, "compound");
        push_named_string(&mut bytes, "nested", "yes");
        bytes.push(0);

        bytes.push(11);
        push_name(&mut bytes, "ints");
        bytes.extend_from_slice(&2_i32.to_be_bytes());
        bytes.extend_from_slice(&7_i32.to_be_bytes());
        bytes.extend_from_slice(&8_i32.to_be_bytes());

        bytes.push(12);
        push_name(&mut bytes, "longs");
        bytes.extend_from_slice(&2_i32.to_be_bytes());
        bytes.extend_from_slice(&9_i64.to_be_bytes());
        bytes.extend_from_slice(&10_i64.to_be_bytes());

        bytes.push(0);

        let decoded = read_network_nbt(&mut Reader::new(&bytes)).expect("NBT decodes");
        assert_eq!(
            decoded,
            Nbt::Compound(vec![
                ("byte".to_owned(), Nbt::Byte(127)),
                ("short".to_owned(), Nbt::Short(0x0102)),
                ("int".to_owned(), Nbt::Int(0x0102_0304)),
                ("long".to_owned(), Nbt::Long(0x0102_0304_0506_0708)),
                ("float".to_owned(), Nbt::Float(1.5)),
                ("double".to_owned(), Nbt::Double(-2.25)),
                ("bytes".to_owned(), Nbt::ByteArray(vec![1, -1, 127])),
                ("string".to_owned(), Nbt::String("hello".to_owned())),
                (
                    "list".to_owned(),
                    Nbt::List {
                        element_type: NbtTag::Int,
                        elements: vec![Nbt::Int(1), Nbt::Int(2)],
                    },
                ),
                (
                    "compound".to_owned(),
                    Nbt::Compound(vec![("nested".to_owned(), Nbt::String("yes".to_owned()))]),
                ),
                ("ints".to_owned(), Nbt::IntArray(vec![7, 8])),
                ("longs".to_owned(), Nbt::LongArray(vec![9, 10])),
            ]),
        );
    }

    #[test]
    fn nbt_modified_utf8_decodes_embedded_nul_and_non_bmp_characters() {
        let nul_string = [8, 0, 4, b'a', 0xc0, 0x80, b'b'];
        let emoji_string = [8, 0, 6, 0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80];

        assert_eq!(
            read_network_nbt(&mut Reader::new(&nul_string)).expect("NUL string decodes"),
            Nbt::String("a\0b".to_owned()),
        );
        assert_eq!(
            read_network_nbt(&mut Reader::new(&emoji_string)).expect("emoji string decodes"),
            Nbt::String("😀".to_owned()),
        );
    }

    #[test]
    fn nbt_writer_emits_exact_network_bytes_for_live_dimension_type_payload() {
        // Captured from the 26.2 server on 127.0.0.1:25565 during Configuration:
        // the `minecraft:dimension_type` registry's `minecraft:overworld` entry.
        let live_dimension_type = hex_bytes(
            "\
            0a08000d64656661756c745f636c6f636b00136d696e6563726166743a6f766572776f726c64\
            0100166861735f656e6465725f647261676f6e5f66696768740005000d616d6269656e745f\
            6c696768740000000003001f6d6f6e737465725f737061776e5f626c6f636b5f6c69676874\
            5f6c696d69740000000008000a696e66696e696275726e001f236d696e6563726166743a69\
            6e66696e696275726e5f6f766572776f726c6401000c6861735f736b796c69676874010800\
            0974696d656c696e65730017236d696e6563726166743a696e5f6f766572776f726c640600\
            10636f6f7264696e6174655f7363616c653ff000000000000003000e6c6f676963616c5f68\
            6569676874000001800a000a617474726962757465730a00206d696e6563726166743a6175\
            64696f2f6261636b67726f756e645f6d757369630a000764656661756c740300096d61785f\
            64656c617900005dc0080005736f756e6400146d696e6563726166743a6d757369632e6761\
            6d650300096d696e5f64656c617900002ee0000a000863726561746976650300096d61785f\
            64656c617900005dc0080005736f756e6400186d696e6563726166743a6d757369632e6372\
            6561746976650300096d696e5f64656c617900002ee0000008001a6d696e6563726166743a\
            76697375616c2f666f675f636f6c6f7200072363306438666605001d6d696e656372616674\
            3a76697375616c2f636c6f75645f6865696768744340547b0800246d696e6563726166743a\
            76697375616c2f616d6269656e745f6c696768745f636f6c6f720007233061306130610800\
            1a6d696e6563726166743a76697375616c2f736b795f636f6c6f720007233738613766660a\
            001e6d696e6563726166743a617564696f2f616d6269656e745f736f756e64730a00046d6f\
            6f6403000a7469636b5f64656c6179000017700600066f6666736574400000000000000008\
            0005736f756e6400166d696e6563726166743a616d6269656e742e63617665030013626c6f\
            636b5f7365617263685f657874656e7400000008000008001c6d696e6563726166743a7669\
            7375616c2f636c6f75645f636f6c6f720009236363666666666666000300056d696e5f79ff\
            ffffc00a00196d6f6e737465725f737061776e5f6c696768745f6c6576656c03000d6d69\
            6e5f696e636c75736976650000000003000d6d61785f696e636c7573697665000000070800\
            047479706500116d696e6563726166743a756e69666f726d0001000b6861735f6365696c69\
            6e67000300066865696768740000018000",
        );
        let mut reader = Reader::new(&live_dimension_type);
        let value = read_network_nbt(&mut reader).expect("live registry NBT decodes");
        reader
            .ensure_empty()
            .expect("live registry NBT fully consumed");

        let mut writer = Writer::default();
        write_network_nbt(&mut writer, &value).expect("live registry NBT encodes");
        assert_eq!(writer.as_slice(), live_dimension_type.as_slice());
    }

    #[test]
    fn nbt_writer_distinguishes_named_and_network_roots() {
        let value = Nbt::Compound(vec![("text".to_owned(), Nbt::String("hi".to_owned()))]);

        let mut network = Writer::default();
        write_network_nbt(&mut network, &value).expect("network NBT encodes");
        assert_eq!(
            network.as_slice(),
            &[10, 8, 0, 4, b't', b'e', b'x', b't', 0, 2, b'h', b'i', 0]
        );

        let mut named = Writer::default();
        write_named_nbt(&mut named, "root", &value).expect("named NBT encodes");
        assert_eq!(
            named.as_slice(),
            &[
                10, 0, 4, b'r', b'o', b'o', b't', 8, 0, 4, b't', b'e', b'x', b't', 0, 2, b'h',
                b'i', 0,
            ],
        );
    }

    #[test]
    fn nbt_writer_preserves_empty_list_element_type_and_modified_utf8() {
        let value = Nbt::List {
            element_type: NbtTag::String,
            elements: vec![],
        };
        let mut writer = Writer::default();
        write_network_nbt(&mut writer, &value).expect("empty list encodes");
        assert_eq!(writer.as_slice(), &[9, 8, 0, 0, 0, 0]);

        let value = Nbt::String("a\0😀".to_owned());
        writer.clear();
        write_network_nbt(&mut writer, &value).expect("modified UTF-8 string encodes");
        assert_eq!(
            writer.as_slice(),
            &[
                8, 0, 9, b'a', 0xc0, 0x80, 0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80
            ],
        );
    }

    #[test]
    fn nbt_writer_rejects_mismatched_or_non_empty_end_lists() {
        let mut writer = Writer::default();
        let mismatched = Nbt::List {
            element_type: NbtTag::Int,
            elements: vec![Nbt::String("wrong".to_owned())],
        };
        assert!(matches!(
            write_network_nbt(&mut writer, &mismatched),
            Err(Error::InvalidEnumVariant {
                name: "nbt list element type",
                value: 8
            })
        ));

        let non_empty_end = Nbt::List {
            element_type: NbtTag::End,
            elements: vec![Nbt::End],
        };
        assert!(matches!(
            write_network_nbt(&mut writer, &non_empty_end),
            Err(Error::InvalidEnumVariant {
                name: "nbt list element type",
                value: 0
            })
        ));
    }

    #[test]
    fn nbt_empty_compound_and_list_of_compounds_decode() {
        assert_eq!(
            read_network_nbt(&mut Reader::new(&[10, 0])).expect("empty compound decodes"),
            Nbt::Compound(vec![]),
        );

        let mut bytes = vec![9, 10];
        bytes.extend_from_slice(&2_i32.to_be_bytes());
        push_named_string(&mut bytes, "text", "a");
        bytes.push(0);
        push_named_string(&mut bytes, "text", "b");
        bytes.push(0);

        assert_eq!(
            read_network_nbt(&mut Reader::new(&bytes)).expect("compound list decodes"),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: vec![
                    Nbt::Compound(vec![("text".to_owned(), Nbt::String("a".to_owned()))]),
                    Nbt::Compound(vec![("text".to_owned(), Nbt::String("b".to_owned()))]),
                ],
            },
        );
    }

    #[test]
    fn nbt_rejects_hostile_lengths_before_allocating() {
        let negative_byte_array = [7, 0xff, 0xff, 0xff, 0xff];
        assert!(matches!(
            read_network_nbt(&mut Reader::new(&negative_byte_array)),
            Err(Error::NegativeLength(-1))
        ));

        let huge_byte_array = [7, 0x7f, 0xff, 0xff, 0xff];
        assert!(matches!(
            read_network_nbt(&mut Reader::new(&huge_byte_array)),
            Err(Error::LimitExceeded {
                actual: 2_147_483_647,
                ..
            })
        ));

        let huge_list = [9, 1, 0x7f, 0xff, 0xff, 0xff];
        assert!(matches!(
            read_network_nbt(&mut Reader::new(&huge_list)),
            Err(Error::LimitExceeded {
                actual: 2_147_483_647,
                ..
            })
        ));
    }

    #[test]
    fn nbt_rejects_unknown_tags_bad_lists_depth_and_truncation() {
        assert!(matches!(
            read_network_nbt(&mut Reader::new(&[99])),
            Err(Error::InvalidEnumVariant {
                name: "nbt tag",
                value: 99
            })
        ));

        assert!(matches!(
            read_network_nbt(&mut Reader::new(&[3, 0x01])),
            Err(Error::UnexpectedEof)
        ));

        let non_empty_end_list = [9, 0, 0, 0, 0, 1];
        assert!(matches!(
            read_network_nbt(&mut Reader::new(&non_empty_end_list)),
            Err(Error::InvalidEnumVariant {
                name: "nbt list element type",
                value: 0
            })
        ));

        let mut too_deep = vec![10];
        for _ in 0..=NBT_MAX_DEPTH {
            too_deep.push(10);
            push_name(&mut too_deep, "x");
        }
        too_deep.extend(std::iter::repeat_n(0, NBT_MAX_DEPTH + 2));
        assert!(matches!(
            read_network_nbt(&mut Reader::new(&too_deep)),
            Err(Error::NbtDepthExceeded {
                limit: NBT_MAX_DEPTH
            })
        ));

        let mut too_deep_list = vec![9];
        for _ in 0..=NBT_MAX_DEPTH {
            too_deep_list.push(9);
            too_deep_list.extend_from_slice(&1_i32.to_be_bytes());
        }
        too_deep_list.push(0);
        too_deep_list.extend_from_slice(&0_i32.to_be_bytes());
        assert!(matches!(
            read_network_nbt(&mut Reader::new(&too_deep_list)),
            Err(Error::NbtDepthExceeded {
                limit: NBT_MAX_DEPTH
            })
        ));
    }

    #[test]
    fn nbt_plain_text_extraction_handles_strings_text_and_extra_children() {
        assert_eq!(
            plain_text_from_nbt_component(&Nbt::String("bare".to_owned())),
            "bare",
        );

        assert_eq!(
            plain_text_from_nbt_component(&Nbt::Compound(vec![(
                "text".to_owned(),
                Nbt::String("hi".to_owned()),
            )])),
            "hi",
        );

        let nested = Nbt::Compound(vec![
            ("text".to_owned(), Nbt::String("hi".to_owned())),
            (
                "extra".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Compound,
                    elements: vec![
                        Nbt::Compound(vec![("text".to_owned(), Nbt::String(" there".to_owned()))]),
                        Nbt::Compound(vec![
                            ("text".to_owned(), Nbt::String("!".to_owned())),
                            (
                                "extra".to_owned(),
                                Nbt::List {
                                    element_type: NbtTag::String,
                                    elements: vec![Nbt::String("!".to_owned())],
                                },
                            ),
                        ]),
                    ],
                },
            ),
        ]);
        assert_eq!(plain_text_from_nbt_component(&nested), "hi there!!");

        assert_eq!(plain_text_from_nbt_component(&Nbt::Compound(vec![])), "");
    }

    #[test]
    fn encode_decode_body_round_trip() {
        let ctx = Ctx { version: 0 };
        let encoded = encode_body(&42_i32, ctx).expect("encode succeeds");
        let decoded: i32 = decode_body(&encoded, ctx).expect("decode succeeds");
        assert_eq!(decoded, 42);
    }

    #[test]
    fn decode_body_exact_rejects_trailing_bytes() {
        let ctx = Ctx { version: 0 };
        let encoded = encode_body(&7_i32, ctx).expect("encode succeeds");
        let mut with_tail = encoded.clone();
        with_tail.push(0xff);

        let exact: i32 = decode_body_exact(&encoded, ctx).expect("exact decode of clean payload");
        assert_eq!(exact, 7);

        let err = decode_body_exact::<i32>(&with_tail, ctx)
            .expect_err("trailing byte must be rejected");
        assert!(err.contains("trailing"), "unexpected error: {err}");

        // decode_body, unlike decode_body_exact, is lenient about the tail.
        let lenient: i32 = decode_body(&with_tail, ctx).expect("lenient decode succeeds");
        assert_eq!(lenient, 7);
    }

    #[test]
    fn unpack_degrees_matches_vanilla_256_step_circle() {
        assert_eq!(unpack_degrees(0), 0.0);
        assert_eq!(unpack_degrees(64), 90.0);
        assert_eq!(unpack_degrees(-64), -90.0);
        assert_eq!(unpack_degrees(-128), -180.0);
    }
}
