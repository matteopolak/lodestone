//! A read-only Java class-file constant-pool parser.
//!
//! # What it is
//!
//! Enough of the JVM class-file format (JVMS chapter 4) to answer one question:
//! *which members of which classes does this class file reference?* It parses
//! the magic, version, constant pool, and method `Code` attributes. The pool
//! records every symbolic possibility; the instructions name the members code
//! actually uses. `Code` attributes hold *indices* into that pool, not names.
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
//! The reader walks every method attribute, parsing `Code` and safely skipping
//! every other attribute by its declared length. Add a new static-use kind
//! in [`MemberUseKind`] and its opcode in `walk_code`; do not search the raw
//! byte stream, because instruction operands and switch payloads can equal an
//! opcode value.
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
    /// is referring*: a call from the replaced layer to itself is internal,
    /// while the same call from a wrapper or extension class is a member the
    /// bridge must actually provide.
    pub this_class: u16,
    /// Static member instruction sites decoded from method bytecode.
    member_uses: Vec<MemberUse>,
    /// `invokedynamic` pool indices, retained only to validate their required
    /// constant-pool kind without treating a bootstrap site as a member use.
    invoke_dynamic_uses: Vec<(usize, u16)>,
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
        // `access_flags u2`, then `this_class u2` — four bytes past the pool.
        cursor.skip(2)?;
        let this_class = cursor.u16()?;
        let mut class = Self {
            major,
            minor,
            pool,
            this_class,
            member_uses: Vec::new(),
            invoke_dynamic_uses: Vec::new(),
        };
        class.parse_body(&mut cursor)?;
        class.validate_member_uses()?;
        Ok(class)
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

    /// Actual `get*`, `put*`, and invocation instructions from every method.
    ///
    /// A pool reference without an instruction remains symbolic only and is
    /// therefore absent here.
    #[must_use]
    pub fn member_uses(&self) -> &[MemberUse] {
        &self.member_uses
    }

    fn parse_body(&mut self, cursor: &mut Cursor<'_>) -> Result<(), ClassFileError> {
        cursor.skip(2)?; // super_class
        let interfaces = usize::from(cursor.u16()?);
        cursor.skip(interfaces.checked_mul(2).ok_or(ClassFileError::Truncated { at: cursor.at })?)?;
        let fields = usize::from(cursor.u16()?);
        for _ in 0..fields {
            cursor.skip(6)?; // access_flags, name_index, descriptor_index
            skip_attributes(cursor, &self.pool)?;
        }
        let methods = usize::from(cursor.u16()?);
        for _ in 0..methods {
            cursor.skip(6)?; // access_flags, name_index, descriptor_index
            self.parse_method_attributes(cursor)?;
        }
        skip_attributes(cursor, &self.pool)?;
        Ok(())
    }

    fn parse_method_attributes(&mut self, cursor: &mut Cursor<'_>) -> Result<(), ClassFileError> {
        let count = usize::from(cursor.u16()?);
        for _ in 0..count {
            let name_index = cursor.u16()?;
            let length = usize::try_from(cursor.u32()?)
                .map_err(|_| ClassFileError::Truncated { at: cursor.at })?;
            let info = cursor.take(length)?;
            if self.pool.utf8(name_index)? == "Code" {
                parse_code_attribute(info, &mut self.member_uses, &mut self.invoke_dynamic_uses)?;
            }
        }
        Ok(())
    }

    fn validate_member_uses(&self) -> Result<(), ClassFileError> {
        for use_ in &self.member_uses {
            let member = self.pool.member_ref(use_.pool_index)?;
            let valid = match use_.kind {
                MemberUseKind::GetField
                | MemberUseKind::PutField
                | MemberUseKind::GetStatic
                | MemberUseKind::PutStatic => member.kind == RefKind::Field,
                MemberUseKind::InvokeVirtual => member.kind == RefKind::Method,
                MemberUseKind::InvokeSpecial | MemberUseKind::InvokeStatic => {
                    matches!(member.kind, RefKind::Method | RefKind::InterfaceMethod)
                }
                MemberUseKind::InvokeInterface => member.kind == RefKind::InterfaceMethod,
            };
            if !valid {
                return Err(ClassFileError::WrongInstructionReferenceKind {
                    at: use_.at,
                    index: use_.pool_index,
                    operation: use_.kind.label(),
                });
            }
        }
        for &(at, index) in &self.invoke_dynamic_uses {
            if !self.pool.is_invoke_dynamic(index) {
                return Err(ClassFileError::WrongInvokeDynamicReference { at, index });
            }
        }
        Ok(())
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
    /// A `CONSTANT_Long` or `CONSTANT_Double` occupies the final declared pool
    /// slot, leaving no second unusable slot for the format to reserve.
    TwoSlotConstantAtPoolEnd {
        /// The first slot of the invalid two-slot constant.
        index: u16,
    },
    /// An opcode reserved or undefined by the class-file format appeared in a
    /// method's `Code` bytes.
    InvalidOpcode {
        /// Offset within the `Code` byte array.
        at: usize,
        /// The invalid byte.
        opcode: u8,
    },
    /// A fixed-width instruction did not carry all of its operands.
    TruncatedInstruction {
        /// Offset within the `Code` byte array.
        at: usize,
        /// The instruction byte.
        opcode: u8,
        /// Bytes required by that complete instruction.
        needed: usize,
    },
    /// An otherwise complete instruction has an invalid fixed operand.
    MalformedInstruction {
        /// Offset within the `Code` byte array.
        at: usize,
        /// The instruction byte.
        opcode: u8,
        /// Why the operand layout is invalid.
        reason: &'static str,
    },
    /// A `wide` prefix was applied to an instruction it cannot modify.
    MalformedWide {
        /// Offset of the `wide` prefix within the `Code` byte array.
        at: usize,
        /// The modified opcode.
        opcode: u8,
    },
    /// A variable-width switch has an invalid range or pair count.
    MalformedSwitch {
        /// Offset of the switch opcode within the `Code` byte array.
        at: usize,
        /// The switch opcode.
        opcode: u8,
        /// Why its layout is impossible.
        reason: &'static str,
    },
    /// An instruction's pool index does not have the reference kind its opcode
    /// requires.
    WrongInstructionReferenceKind {
        /// Offset within the `Code` byte array.
        at: usize,
        /// The referenced pool index.
        index: u16,
        /// The decoded operation name.
        operation: &'static str,
    },
    /// An `invokedynamic` instruction did not point at an `InvokeDynamic`
    /// constant-pool entry.
    WrongInvokeDynamicReference {
        /// Offset within the `Code` byte array.
        at: usize,
        /// The referenced pool index.
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
            Self::TwoSlotConstantAtPoolEnd { index } => write!(
                f,
                "two-slot constant at pool index {index} has no declared second slot"
            ),
            Self::InvalidOpcode { at, opcode } => {
                write!(f, "invalid bytecode opcode {opcode:#04x} at Code offset {at}")
            }
            Self::TruncatedInstruction { at, opcode, needed } => write!(
                f,
                "truncated bytecode instruction {opcode:#04x} at Code offset {at}; need {needed} bytes"
            ),
            Self::MalformedInstruction { at, opcode, reason } => write!(
                f,
                "malformed bytecode instruction {opcode:#04x} at Code offset {at}: {reason}"
            ),
            Self::MalformedWide { at, opcode } => write!(
                f,
                "malformed wide instruction at Code offset {at}: {opcode:#04x} cannot be widened"
            ),
            Self::MalformedSwitch { at, opcode, reason } => write!(
                f,
                "malformed switch {opcode:#04x} at Code offset {at}: {reason}"
            ),
            Self::WrongInstructionReferenceKind {
                at,
                index,
                operation,
            } => write!(
                f,
                "{operation} at Code offset {at} refers to incompatible constant pool entry {index}"
            ),
            Self::WrongInvokeDynamicReference { at, index } => write!(
                f,
                "invokedynamic at Code offset {at} refers to non-InvokeDynamic constant pool entry {index}"
            ),
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
        /// Pool index of the `Utf8` holding e.g. `example/target/World`.
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
    /// `CONSTANT_InvokeDynamic` (tag 18), whose bootstrap target is not an
    /// static member use but whose tag must agree with `invokedynamic`.
    InvokeDynamic,
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

/// The direction or invocation form of one static member instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberUseKind {
    /// `getfield` (`0xb4`).
    GetField,
    /// `putfield` (`0xb5`).
    PutField,
    /// `getstatic` (`0xb2`).
    GetStatic,
    /// `putstatic` (`0xb3`).
    PutStatic,
    /// `invokevirtual` (`0xb6`).
    InvokeVirtual,
    /// `invokespecial` (`0xb7`).
    InvokeSpecial,
    /// `invokestatic` (`0xb8`).
    InvokeStatic,
    /// `invokeinterface` (`0xb9`).
    InvokeInterface,
}

impl MemberUseKind {
    /// A short, stable report label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::GetField => "getfield",
            Self::PutField => "putfield",
            Self::GetStatic => "getstatic",
            Self::PutStatic => "putstatic",
            Self::InvokeVirtual => "invokevirtual",
            Self::InvokeSpecial => "invokespecial",
            Self::InvokeStatic => "invokestatic",
            Self::InvokeInterface => "invokeinterface",
        }
    }
}

/// One validated static instruction use of a constant-pool member entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberUse {
    /// Offset within the containing method's `Code` byte array.
    pub at: usize,
    /// The one-based constant-pool index read by this instruction.
    pub pool_index: u16,
    /// Field direction or invocation opcode.
    pub kind: MemberUseKind,
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
            if matches!(tag, 5 | 6) && index == declared {
                return Err(ClassFileError::TwoSlotConstantAtPoolEnd { index });
            }
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
                // Dynamic (17): u2 bootstrap index + u2
                // name_and_type index. The name/type half *can* name an NMS
                // descriptor, so it is reachable, but the call target is a
                // bootstrap method rather than a member of the named class —
                // counted through descriptors, not as a member reference.
                17 => {
                    cursor.skip(4)?;
                    Entry::Other
                }
                18 => {
                    cursor.skip(4)?;
                    Entry::InvokeDynamic
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
    /// `example/target/World`.
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

    fn is_invoke_dynamic(&self, index: u16) -> bool {
        matches!(self.get(index), Some(Entry::InvokeDynamic))
    }
}

/// A resolved symbolic reference: "this class file calls/reads
/// `class.name descriptor`".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberRef<'a> {
    /// Field, method or interface method.
    pub kind: RefKind,
    /// Owning class, internal form (`example/target/World`).
    pub class: &'a str,
    /// The member's simple name (`readState`, `<init>`).
    pub name: &'a str,
    /// The member's descriptor (`(Lexample/target/Point;)V`).
    pub descriptor: &'a str,
}

/// Skip an attribute table without interpreting its payloads.
fn skip_attributes(cursor: &mut Cursor<'_>, pool: &ConstantPool) -> Result<(), ClassFileError> {
    let count = usize::from(cursor.u16()?);
    for _ in 0..count {
        let name_index = cursor.u16()?;
        // Resolving the name catches a malformed table before its length can
        // make us appear to have consumed a valid following member.
        pool.utf8(name_index)?;
        let length = usize::try_from(cursor.u32()?)
            .map_err(|_| ClassFileError::Truncated { at: cursor.at })?;
        cursor.skip(length)?;
    }
    Ok(())
}

/// Parse one method `Code` attribute and collect its static member uses.
fn parse_code_attribute(
    bytes: &[u8],
    uses: &mut Vec<MemberUse>,
    dynamic_uses: &mut Vec<(usize, u16)>,
) -> Result<(), ClassFileError> {
    let mut cursor = Cursor::new(bytes);
    cursor.skip(4)?; // max_stack, max_locals
    let code_length = usize::try_from(cursor.u32()?)
        .map_err(|_| ClassFileError::Truncated { at: cursor.at })?;
    let code = cursor.take(code_length)?;
    walk_code(code, uses, dynamic_uses)?;
    let handlers = usize::from(cursor.u16()?);
    cursor.skip(
        handlers
            .checked_mul(8)
            .ok_or(ClassFileError::Truncated { at: cursor.at })?,
    )?;
    // Nested attributes have no static member instructions; their declared lengths
    // still need checking so a truncated stack-map table is never accepted.
    let nested = usize::from(cursor.u16()?);
    for _ in 0..nested {
        cursor.skip(2)?; // attribute_name_index; no pool access is needed here
        let length = usize::try_from(cursor.u32()?)
            .map_err(|_| ClassFileError::Truncated { at: cursor.at })?;
        cursor.skip(length)?;
    }
    if cursor.at != bytes.len() {
        return Err(ClassFileError::MalformedSwitch {
            at: cursor.at,
            opcode: 0,
            reason: "Code attribute has trailing bytes",
        });
    }
    Ok(())
}

/// Decode the instruction stream without ever searching its bytes by value.
fn walk_code(
    code: &[u8],
    uses: &mut Vec<MemberUse>,
    dynamic_uses: &mut Vec<(usize, u16)>,
) -> Result<(), ClassFileError> {
    let mut at = 0;
    while at < code.len() {
        let opcode = code[at];
        let length = match opcode {
            0x00..=0x0f | 0x1a..=0x35 | 0x3b..=0x83 | 0x85..=0x98 | 0xac..=0xb1
            | 0xbe | 0xbf | 0xc2 | 0xc3 => 1,
            0x10 | 0x12 | 0x15..=0x19 | 0x36..=0x3a | 0xa9 | 0xbc => 2,
            0x11 | 0x13 | 0x14 | 0x84 | 0x99..=0xa8 | 0xb2..=0xb8 | 0xbb | 0xbd
            | 0xc0 | 0xc1 | 0xc6 | 0xc7 => 3,
            0xb9 | 0xba | 0xc8 | 0xc9 => 5,
            0xc5 => 4,
            0xaa => switch_length(code, at, opcode, true)?,
            0xab => switch_length(code, at, opcode, false)?,
            0xc4 => wide_length(code, at)?,
            _ => return Err(ClassFileError::InvalidOpcode { at, opcode }),
        };
        ensure_instruction(code, at, opcode, length)?;
        validate_instruction_operands(code, at, opcode)?;
        let pool_index = |offset: usize| u16::from_be_bytes([code[at + offset], code[at + offset + 1]]);
        let kind = match opcode {
            0xb2 => Some(MemberUseKind::GetStatic),
            0xb3 => Some(MemberUseKind::PutStatic),
            0xb4 => Some(MemberUseKind::GetField),
            0xb5 => Some(MemberUseKind::PutField),
            0xb6 => Some(MemberUseKind::InvokeVirtual),
            0xb7 => Some(MemberUseKind::InvokeSpecial),
            0xb8 => Some(MemberUseKind::InvokeStatic),
            0xb9 => Some(MemberUseKind::InvokeInterface),
            _ => None,
        };
        if let Some(kind) = kind {
            uses.push(MemberUse {
                at,
                pool_index: pool_index(1),
                kind,
            });
        }
        if opcode == 0xba {
            dynamic_uses.push((at, pool_index(1)));
        }
        at += length;
    }
    Ok(())
}

fn validate_instruction_operands(code: &[u8], at: usize, opcode: u8) -> Result<(), ClassFileError> {
    let malformed = |reason| ClassFileError::MalformedInstruction { at, opcode, reason };
    match opcode {
        0xb9 if code[at + 3] == 0 => Err(malformed("invokeinterface count is zero")),
        0xb9 if code[at + 4] != 0 => Err(malformed("invokeinterface reserved byte is nonzero")),
        0xba if code[at + 3] != 0 || code[at + 4] != 0 => {
            Err(malformed("invokedynamic reserved bytes are nonzero"))
        }
        _ => Ok(()),
    }
}

fn ensure_instruction(
    code: &[u8],
    at: usize,
    opcode: u8,
    needed: usize,
) -> Result<(), ClassFileError> {
    if code.len().saturating_sub(at) < needed {
        return Err(ClassFileError::TruncatedInstruction { at, opcode, needed });
    }
    Ok(())
}

fn wide_length(code: &[u8], at: usize) -> Result<usize, ClassFileError> {
    ensure_instruction(code, at, 0xc4, 2)?;
    match code[at + 1] {
        0x15..=0x19 | 0x36..=0x3a | 0xa9 => Ok(4),
        0x84 => Ok(6),
        opcode => Err(ClassFileError::MalformedWide { at, opcode }),
    }
}

fn switch_length(
    code: &[u8],
    at: usize,
    opcode: u8,
    table: bool,
) -> Result<usize, ClassFileError> {
    let padding = (4 - ((at + 1) % 4)) % 4;
    let header = 1 + padding + if table { 12 } else { 8 };
    ensure_instruction(code, at, opcode, header)?;
    let read_i32 = |offset: usize| {
        i32::from_be_bytes([
            code[at + offset],
            code[at + offset + 1],
            code[at + offset + 2],
            code[at + offset + 3],
        ])
    };
    let entries = if table {
        // default, low, high follow padding. `high < low` is malformed rather
        // than a huge count after unsigned conversion.
        let low = read_i32(1 + padding + 4);
        let high = read_i32(1 + padding + 8);
        if high < low {
            return Err(ClassFileError::MalformedSwitch {
                at,
                opcode,
                reason: "tableswitch high is below low",
            });
        }
        let entries = high
            .checked_sub(low)
            .and_then(|span| span.checked_add(1))
            .ok_or(ClassFileError::MalformedSwitch {
                at,
                opcode,
                reason: "tableswitch entry count overflows",
            })?;
        usize::try_from(entries).map_err(|_| ClassFileError::MalformedSwitch {
            at,
            opcode,
            reason: "tableswitch entry count does not fit",
        })?
    } else {
        let pairs = read_i32(1 + padding + 4);
        usize::try_from(pairs).map_err(|_| ClassFileError::MalformedSwitch {
            at,
            opcode,
            reason: "lookupswitch pair count is negative or does not fit",
        })?
    };
    let per_entry = if table { 4 } else { 8 };
    let tail = entries.checked_mul(per_entry).ok_or(ClassFileError::MalformedSwitch {
        at,
        opcode,
        reason: "switch entry count overflows bytecode length",
    })?;
    header.checked_add(tail).ok_or(ClassFileError::MalformedSwitch {
        at,
        opcode,
        reason: "switch length overflows bytecode length",
    })
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
