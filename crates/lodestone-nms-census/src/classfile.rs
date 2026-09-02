//! A read-only Java class-file constant-pool parser.
//!
//! # What it is
//!
//! Enough of the JVM class-file format (JVMS chapter 4) to answer one question:
//! *which members of which classes does this class file reference?* It parses
//! the magic, the version, and the constant pool, and then stops — the field,
//! method and attribute tables that follow are never read, because every
//! symbolic reference a class makes already lives in the constant pool by
//! construction. `Code` attributes hold *indices* into that pool, not names.
//!
//! No JVM is involved and none is needed: this is a byte-format reader, the
//! same way `lodestone-anvil` reads region files without a server.
//!
//! # How it works
//!
//! [`ConstantPool::parse`] walks the pool once, storing each entry's raw shape,
//! then resolution ([`ConstantPool::class_name`], [`ConstantPool::member_ref`])
//! chases the index links lazily. Two details of the format cause almost every
//! bug written against it, and both are handled here:
//!
//! - **The pool is 1-based and has `count - 1` entries.** Index 0 is not a
//!   valid constant and is never stored.
//! - **`CONSTANT_Long` and `CONSTANT_Double` occupy *two* slots.** The entry
//!   after one of them is unusable and must be skipped. A parser that
//!   increments by one per entry desynchronises from the first `long` constant
//!   onward and then misresolves every later index — which does not error, it
//!   silently returns the *wrong name*. [`Entry::Unusable`] is that second slot,
//!   made explicit rather than left as an off-by-one.
//!
//! Strings are **modified UTF-8** (JVMS 4.4.7), not UTF-8: a NUL is encoded as
//! the two bytes `C0 80`, and a supplementary character as a six-byte surrogate
//! pair rather than a four-byte sequence. [`decode_modified_utf8`] implements
//! that, because `String::from_utf8` rejects both forms. In practice class and
//! member names are ASCII and this never fires, but a string constant in the
//! same pool is not, and one bad decode would abort a whole class.
//!
//! # How to change it
//!
//! If a future need requires the method table (for example, to distinguish a
//! member a class *declares* from one it *calls*), parse it after the pool: the
//! layout is `access_flags u2`, `this_class u2`, `super_class u2`, then the
//! interface, field, method and attribute tables. Nothing here consumes past
//! the pool, so the cursor is left at exactly that point — [`ClassFile::rest`]
//! exposes it.
//!
//! Unknown constant tags are a hard error rather than a skip: an unrecognised
//! tag has an unknown *width*, so continuing past one would desynchronise the
//! cursor and produce confident nonsense. A new JVMS tag therefore shows up as
//! a named parse failure on the class that uses it, which is the honest
//! outcome.
//!
//! # Dependencies
//!
//! None. `std` only.

use std::fmt;

/// A parsed class file: its version, its constant pool, and its own name.
#[derive(Debug, Clone)]
pub struct ClassFile {
    /// The class-file major version (`61` = Java 17, `65` = Java 21, …).
    pub major: u16,
    /// The class-file minor version.
    pub minor: u16,
    /// Every constant, resolvable by index.
    pub pool: ConstantPool,
    /// Pool index of this class's own `CONSTANT_Class`.
    ///
    /// Read because a reference census is only meaningful when it knows *who
    /// is referring*: a call from `net.minecraft` to `net.minecraft` is
    /// internal to the layer being replaced, while the same call from
    /// `org.bukkit.craftbukkit` is a member the bridge must actually provide.
    pub this_class: u16,
    /// Byte offset just past the constant pool — where `access_flags` begins.
    rest: usize,
}

impl ClassFile {
    /// Parse `bytes` as a class file.
    ///
    /// # Errors
    ///
    /// If the magic is not `0xCAFEBABE`, the input is truncated, or the pool
    /// carries a constant tag this parser does not know the width of.
    pub fn parse(bytes: &[u8]) -> Result<Self, ClassFileError> {
        let mut cursor = Cursor::new(bytes);
        let magic = cursor.u32()?;
        if magic != 0xCAFE_BABE {
            return Err(ClassFileError::NotAClassFile { magic });
        }
        let minor = cursor.u16()?;
        let major = cursor.u16()?;
        let pool = ConstantPool::parse(&mut cursor)?;
        let rest = cursor.at;
        // `access_flags u2`, then `this_class u2` — four bytes past the pool.
        cursor.skip(2)?;
        let this_class = cursor.u16()?;
        Ok(Self {
            major,
            minor,
            pool,
            this_class,
            rest,
        })
    }

    /// This class's own internal-form name, e.g.
    /// `org/bukkit/craftbukkit/CraftWorld`.
    ///
    /// # Errors
    ///
    /// If `this_class` does not resolve to a `CONSTANT_Class`.
    pub fn name(&self) -> Result<&str, ClassFileError> {
        self.pool.class_name(self.this_class)
    }

    /// Byte offset of `access_flags`, immediately after the constant pool.
    ///
    /// Nothing in this module reads past here; this is the hook for a caller
    /// that needs the field/method tables.
    #[must_use]
    pub const fn rest(&self) -> usize {
        self.rest
    }
}

/// Why a class file could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassFileError {
    /// The first four bytes were not `0xCAFEBABE`.
    NotAClassFile {
        /// What was there instead.
        magic: u32,
    },
    /// The input ended before a field this parser needed.
    Truncated {
        /// Byte offset at which more input was required.
        at: usize,
    },
    /// A constant tag this parser does not know. Fatal rather than skipped,
    /// because an unknown tag has an unknown width.
    UnknownConstantTag {
        /// The tag byte.
        tag: u8,
        /// The 1-based pool index it appeared at.
        index: u16,
    },
    /// A pool index that is zero, past the end, or points at the second slot of
    /// a `long`/`double`.
    BadPoolIndex {
        /// The offending index.
        index: u16,
    },
    /// A constant was reached through a link that expected a different kind —
    /// e.g. a `Class`'s `name_index` not pointing at a `Utf8`.
    WrongConstantKind {
        /// The index whose contents were unexpected.
        index: u16,
    },
}

impl fmt::Display for ClassFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAClassFile { magic } => {
                write!(f, "not a class file: magic is {magic:#010x}, want 0xcafebabe")
            }
            Self::Truncated { at } => write!(f, "truncated class file at byte {at}"),
            Self::UnknownConstantTag { tag, index } => {
                write!(f, "unknown constant pool tag {tag} at index {index}")
            }
            Self::BadPoolIndex { index } => write!(f, "bad constant pool index {index}"),
            Self::WrongConstantKind { index } => {
                write!(f, "constant at index {index} is not the expected kind")
            }
        }
    }
}

impl std::error::Error for ClassFileError {}

/// One constant-pool entry, in the shapes this scanner needs.
///
/// Constants whose *contents* are irrelevant to a reference census (integers,
/// floats, string bodies, method handles) are kept as [`Entry::Other`] so the
/// pool stays index-accurate without carrying data nothing reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// `CONSTANT_Utf8` (tag 1), decoded from modified UTF-8.
    Utf8(String),
    /// `CONSTANT_Class` (tag 7): an index to the internal-form name.
    Class {
        /// Pool index of the `Utf8` holding e.g. `net/minecraft/world/level/Level`.
        name_index: u16,
    },
    /// `CONSTANT_Fieldref` / `Methodref` / `InterfaceMethodref` (tags 9, 10, 11).
    Ref {
        /// Which of the three.
        kind: RefKind,
        /// Pool index of the owning `Class`.
        class_index: u16,
        /// Pool index of the `NameAndType`.
        name_and_type_index: u16,
    },
    /// `CONSTANT_NameAndType` (tag 12).
    NameAndType {
        /// Pool index of the member's simple name.
        name_index: u16,
        /// Pool index of the member's descriptor.
        descriptor_index: u16,
    },
    /// A constant whose contents this scanner does not read, but whose slot
    /// must exist so later indices resolve correctly.
    Other,
    /// The dead second slot of a `CONSTANT_Long` or `CONSTANT_Double`.
    ///
    /// Stored rather than elided so that an index landing here is a *reported*
    /// [`ClassFileError::BadPoolIndex`] instead of a silent off-by-one that
    /// resolves to the neighbouring constant.
    Unusable,
}

/// Which of the three symbolic reference constants a [`Entry::Ref`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind {
    /// `CONSTANT_Fieldref` — a field read or write.
    Field,
    /// `CONSTANT_Methodref` — a call to a class method.
    Method,
    /// `CONSTANT_InterfaceMethodref` — a call through an interface.
    InterfaceMethod,
}

impl RefKind {
    /// A short, stable label for reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Field => "field",
            Self::Method => "method",
            Self::InterfaceMethod => "interface-method",
        }
    }
}

/// A class file's constant pool, indexable by its 1-based indices.
#[derive(Debug, Clone, Default)]
pub struct ConstantPool {
    /// Entry `n` of the pool lives at `entries[n - 1]`.
    entries: Vec<Entry>,
}

impl ConstantPool {
    fn parse(cursor: &mut Cursor<'_>) -> Result<Self, ClassFileError> {
        let count = cursor.u16()?;
        // A pool of `count` declares `count - 1` usable entries; `count == 0`
        // is malformed but is treated as empty rather than underflowing.
        let declared = count.saturating_sub(1);
        let mut entries = Vec::with_capacity(usize::from(declared));
        let mut index: u16 = 1;
        while index <= declared {
            let tag = cursor.u8()?;
            let entry = match tag {
                1 => {
                    let len = usize::from(cursor.u16()?);
                    Entry::Utf8(decode_modified_utf8(cursor.take(len)?))
                }
                // Integer, Float: u4 payload.
                3 | 4 => {
                    cursor.skip(4)?;
                    Entry::Other
                }
                // Long, Double: u8 payload, and they eat the *next* slot too.
                5 | 6 => {
                    cursor.skip(8)?;
                    Entry::Other
                }
                7 => Entry::Class {
                    name_index: cursor.u16()?,
                },
                // String (8), MethodType (16), Module (19), Package (20): one u2.
                8 | 16 | 19 | 20 => {
                    cursor.skip(2)?;
                    Entry::Other
                }
                9 | 10 | 11 => {
                    let kind = match tag {
                        9 => RefKind::Field,
                        10 => RefKind::Method,
                        _ => RefKind::InterfaceMethod,
                    };
                    Entry::Ref {
                        kind,
                        class_index: cursor.u16()?,
                        name_and_type_index: cursor.u16()?,
                    }
                }
                12 => Entry::NameAndType {
                    name_index: cursor.u16()?,
                    descriptor_index: cursor.u16()?,
                },
                // MethodHandle: u1 reference_kind + u2 reference_index.
                15 => {
                    cursor.skip(3)?;
                    Entry::Other
                }
                // Dynamic (17), InvokeDynamic (18): u2 bootstrap index + u2
                // name_and_type index. The name/type half *can* name an NMS
                // descriptor, so it is reachable, but the call target is a
                // bootstrap method rather than a member of the named class —
                // counted through descriptors, not as a member reference.
                17 | 18 => {
                    cursor.skip(4)?;
                    Entry::Other
                }
                other => {
                    return Err(ClassFileError::UnknownConstantTag { tag: other, index });
                }
            };
            let wide = matches!(tag, 5 | 6);
            entries.push(entry);
            index += 1;
            if wide {
                // JVMS 4.4.5: the constant immediately following a Long or
                // Double is unusable. Push a placeholder so indices stay true.
                entries.push(Entry::Unusable);
                index += 1;
            }
        }
        Ok(Self { entries })
    }

    /// How many slots the pool declares (including `Unusable` ones).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the pool has no slots at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry at 1-based `index`, if it is in range and usable.
    #[must_use]
    pub fn get(&self, index: u16) -> Option<&Entry> {
        let slot = usize::from(index).checked_sub(1)?;
        match self.entries.get(slot)? {
            Entry::Unusable => None,
            entry => Some(entry),
        }
    }

    /// Iterate every usable entry with its 1-based index.
    pub fn iter(&self) -> impl Iterator<Item = (u16, &Entry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !matches!(e, Entry::Unusable))
            .filter_map(|(slot, entry)| {
                let index = u16::try_from(slot + 1).ok()?;
                Some((index, entry))
            })
    }

    /// The `Utf8` at `index`.
    ///
    /// # Errors
    ///
    /// If the index is out of range or the constant is not a `Utf8`.
    pub fn utf8(&self, index: u16) -> Result<&str, ClassFileError> {
        match self.get(index) {
            Some(Entry::Utf8(s)) => Ok(s),
            Some(_) => Err(ClassFileError::WrongConstantKind { index }),
            None => Err(ClassFileError::BadPoolIndex { index }),
        }
    }

    /// The internal-form name of the `CONSTANT_Class` at `index`, e.g.
    /// `net/minecraft/server/level/ServerLevel`.
    ///
    /// # Errors
    ///
    /// If the index is out of range or does not name a class.
    pub fn class_name(&self, index: u16) -> Result<&str, ClassFileError> {
        match self.get(index) {
            Some(Entry::Class { name_index }) => self.utf8(*name_index),
            Some(_) => Err(ClassFileError::WrongConstantKind { index }),
            None => Err(ClassFileError::BadPoolIndex { index }),
        }
    }

    /// Resolve the `Fieldref`/`Methodref`/`InterfaceMethodref` at `index` into
    /// its owning class, member name and descriptor.
    ///
    /// # Errors
    ///
    /// If any link in the chain is out of range or the wrong kind.
    pub fn member_ref(&self, index: u16) -> Result<MemberRef<'_>, ClassFileError> {
        let Some(Entry::Ref {
            kind,
            class_index,
            name_and_type_index,
        }) = self.get(index)
        else {
            return Err(ClassFileError::BadPoolIndex { index });
        };
        let class = self.class_name(*class_index)?;
        let Some(Entry::NameAndType {
            name_index,
            descriptor_index,
        }) = self.get(*name_and_type_index)
        else {
            return Err(ClassFileError::WrongConstantKind {
                index: *name_and_type_index,
            });
        };
        Ok(MemberRef {
            kind: *kind,
            class,
            name: self.utf8(*name_index)?,
            descriptor: self.utf8(*descriptor_index)?,
        })
    }
}

/// A resolved symbolic reference: "this class file calls/reads
/// `class.name descriptor`".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberRef<'a> {
    /// Field, method or interface method.
    pub kind: RefKind,
    /// Owning class, internal form (`net/minecraft/world/level/Level`).
    pub class: &'a str,
    /// The member's simple name (`getBlockState`, `<init>`).
    pub name: &'a str,
    /// The member's descriptor (`(Lnet/minecraft/core/BlockPos;)V`).
    pub descriptor: &'a str,
}

/// Decode JVMS 4.4.7 *modified* UTF-8.
///
/// Differs from real UTF-8 in exactly two ways, both of which
/// `String::from_utf8` rejects: NUL is `C0 80`, and a supplementary character
/// is a six-byte CESU-8 surrogate pair rather than a four-byte sequence.
///
/// Malformed input yields `U+FFFD` for the offending byte and keeps going: a
/// scanner that aborted a whole class over one bad string constant would lose
/// every good reference in it, and the strings this cares about (class and
/// member names) are ASCII by construction.
#[must_use]
pub fn decode_modified_utf8(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            // Note `0x00` is not legal here in modified UTF-8, but accepting it
            // costs nothing and keeps a hand-built fixture from being a trap.
            out.push(b as char);
            i += 1;
        } else if b & 0xE0 == 0xC0 {
            let Some(&b1) = bytes.get(i + 1) else {
                out.push('\u{FFFD}');
                break;
            };
            let code = (u32::from(b & 0x1F) << 6) | u32::from(b1 & 0x3F);
            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
            i += 2;
        } else if b & 0xF0 == 0xE0 {
            let (Some(&b1), Some(&b2)) = (bytes.get(i + 1), bytes.get(i + 2)) else {
                out.push('\u{FFFD}');
                break;
            };
            let code =
                (u32::from(b & 0x0F) << 12) | (u32::from(b1 & 0x3F) << 6) | u32::from(b2 & 0x3F);
            // A high surrogate here begins a six-byte pair; pair it with the
            // low surrogate that follows, which is what makes this CESU-8
            // rather than UTF-8.
            if (0xD800..0xDC00).contains(&code)
                && let (Some(&c0), Some(&c1), Some(&c2)) =
                    (bytes.get(i + 3), bytes.get(i + 4), bytes.get(i + 5))
                && c0 & 0xF0 == 0xE0
            {
                let low = (u32::from(c0 & 0x0F) << 12)
                    | (u32::from(c1 & 0x3F) << 6)
                    | u32::from(c2 & 0x3F);
                if (0xDC00..0xE000).contains(&low) {
                    let combined = 0x1_0000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                    out.push(char::from_u32(combined).unwrap_or('\u{FFFD}'));
                    i += 6;
                    continue;
                }
            }
            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
            i += 3;
        } else {
            out.push('\u{FFFD}');
            i += 1;
        }
    }
    out
}

/// A bounds-checked forward reader.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ClassFileError> {
        let end = self
            .at
            .checked_add(n)
            .ok_or(ClassFileError::Truncated { at: self.at })?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or(ClassFileError::Truncated { at: self.at })?;
        self.at = end;
        Ok(slice)
    }

    fn skip(&mut self, n: usize) -> Result<(), ClassFileError> {
        self.take(n).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8, ClassFileError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ClassFileError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, ClassFileError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}
