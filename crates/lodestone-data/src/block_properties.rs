//! Typed block-state properties for the generated 26.2 block census.
//!
//! [`PropertyKey`] and [`BuiltinPropertyValue`] are generated from the
//! authoritative block-state report. [`Properties`] keeps its compact
//! fixed-capacity representation private so callers work with domain types
//! instead of depending on a `SmallVec` or another storage choice.
//!
//! Text is accepted only at the resource-loading boundary. Once parsed, a
//! property key and built-in value are enums, while an extension value is an
//! opaque [`ExtensionId`] supplied by the registry that owns it.

use std::fmt;

use crate::block::Block;
use crate::block_states::StateId;
use crate::generated_block_property_tables as generated;

pub use generated::{BuiltinPropertyValue, PropertyKey};

/// A compact host-owned handle for a property value supplied by a plugin or
/// data pack. The registry that allocated it owns the corresponding text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtensionId(u32);

impl ExtensionId {
    /// Wraps a host-assigned extension index without storing its spelling.
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }

    /// Returns the host-assigned extension index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A packed property value.
///
/// Built-in values occupy the low byte and extension handles use the high bit
/// plus a 31-bit registry index. The built-in Properties storage uses the
/// generated one-byte enum directly, so extension support does not enlarge
/// every resident block state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PropertyValue(u32);

const EXTENSION_FLAG: u32 = 1 << 31;

impl PropertyValue {
    /// Wraps a generated built-in value.
    #[must_use]
    pub const fn builtin(value: BuiltinPropertyValue) -> Self {
        Self(value as u32)
    }

    /// Wraps an extension handle.
    ///
    /// Panics if the host supplied an index that cannot fit in the compact
    /// 31-bit extension encoding.
    #[must_use]
    pub const fn extension(id: ExtensionId) -> Self {
        assert!(id.0 < EXTENSION_FLAG, "extension id exceeds packed range");
        Self(EXTENSION_FLAG | id.0)
    }

    /// Returns the generated built-in value, or None for an extension.
    #[must_use]
    pub const fn builtin_value(self) -> Option<BuiltinPropertyValue> {
        if self.0 & EXTENSION_FLAG != 0 {
            None
        } else {
            BuiltinPropertyValue::from_id(self.0 as u8)
        }
    }

    /// Returns the extension handle, or None for a built-in value.
    #[must_use]
    pub const fn extension_id(self) -> Option<ExtensionId> {
        if self.0 & EXTENSION_FLAG == 0 {
            None
        } else {
            Some(ExtensionId(self.0 & !EXTENSION_FLAG))
        }
    }

    /// Whether this value is an extension handle.
    #[must_use]
    pub const fn is_extension(self) -> bool {
        self.extension_id().is_some()
    }

    /// Parses one generated built-in spelling.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        BuiltinPropertyValue::from_name(name).map(Self::builtin)
    }

    /// The generated built-in spelling, or None for an extension.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        self.builtin_value().map(BuiltinPropertyValue::name)
    }
}

/// One validated key/value pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Property {
    key: PropertyKey,
    value: BuiltinPropertyValue,
}

impl Property {
    /// Validates a built-in key/value pair. Extension values are deliberately
    /// rejected here: the resident
    /// Properties representation is the compact built-in form. An extension
    /// registry can retain its opaque value at the text boundary.
    pub fn new(key: PropertyKey, value: PropertyValue) -> Result<Self, PropertyError> {
        let Some(value) = value.builtin_value() else {
            return Err(PropertyError::ExtensionValue);
        };
        Self::from_builtin(key, value)
    }

    fn from_builtin(
        key: PropertyKey,
        value: BuiltinPropertyValue,
    ) -> Result<Self, PropertyError> {
        if generated::is_valid_pair(key, value) {
            Ok(Self { key, value })
        } else {
            Err(PropertyError::ValueNotAllowed {
                key,
                value: PropertyValue::builtin(value),
            })
        }
    }

    /// The typed property key.
    #[must_use]
    pub const fn key(self) -> PropertyKey {
        self.key
    }

    /// The typed built-in value.
    #[must_use]
    pub const fn value(self) -> PropertyValue {
        PropertyValue::builtin(self.value)
    }
}

/// A failure while constructing one typed property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyError {
    /// The value is not part of the generated domain for this key.
    ValueNotAllowed {
        /// The key whose domain rejected the value.
        key: PropertyKey,
        /// The rejected value.
        value: PropertyValue,
    },
    /// An extension value cannot be stored in the compact built-in set.
    ExtensionValue,
}

impl fmt::Display for PropertyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValueNotAllowed { key, value } => write!(
                formatter,
                "property value {:?} is not allowed for {:?}",
                value,
                key
            ),
            Self::ExtensionValue => formatter.write_str(
                "extension property values require an extension-owned boundary representation",
            ),
        }
    }
}

impl std::error::Error for PropertyError {}

/// The maximum number of properties on any generated block state.
pub const MAX_PROPERTIES: usize = generated::MAX_PROPERTIES;

const EMPTY_PROPERTY: Property = Property {
    key: generated::PROPERTY_KEYS[0],
    value: generated::PROPERTY_VALUES[0],
};

/// A block state's properties in canonical key order.
///
/// The backing array and its capacity are deliberately private. New callers
/// should use [`Self::try_from_pairs`], [`Self::from_state_id`], [`Self::get`]
/// and [`Self::iter`] rather than coupling to the current fixed-array layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Properties {
    len: u8,
    entries: [Property; MAX_PROPERTIES],
}

impl Properties {
    /// An empty property set.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            len: 0,
            entries: [EMPTY_PROPERTY; MAX_PROPERTIES],
        }
    }

    /// Builds a canonical property set, rejecting duplicate keys, invalid
    /// built-in values and a set larger than the generated maximum.
    pub fn try_from_pairs(pairs: &[(PropertyKey, PropertyValue)]) -> Result<Self, PropertiesError> {
        let mut properties = Self::empty();
        for &(key, value) in pairs {
            properties.push(Property::new(key, value).map_err(PropertiesError::InvalidProperty)?)?;
        }
        Ok(properties)
    }

    /// Resolves the generated properties for one validated block-state id.
    ///
    /// The generated numeric property-set table is copied into the private
    /// compact representation. No state-name or property-name text is parsed.
    #[must_use]
    pub fn from_state_id(state: StateId) -> Self {
        let mut properties = Self::empty();
        for &(key_id, value_id) in generated::property_set_for_state(state.raw()) {
            let key = PropertyKey::from_id(key_id).expect("generated property key id is valid");
            let value = BuiltinPropertyValue::from_id(value_id)
                .expect("generated property value id is valid");
            properties
                .push(
                    Property::from_builtin(key, value)
                        .expect("generated block-state pair is valid"),
                )
                .expect("generated block state exceeds MAX_PROPERTIES");
        }
        properties
    }

    /// Resolves a typed property set to a state of the supplied block.
    ///
    /// The generated block spans make this a bounded numeric lookup. A valid
    /// key/value pair from another block therefore cannot be mistaken for a
    /// state of this block.
    #[must_use]
    pub fn state_for_block(block: Block, properties: &Self) -> Option<StateId> {
        let (first, last) = generated::block_state_span(block.registry_id())?;
        (first..=last).find_map(|raw| {
            properties.matches_generated_pairs(generated::property_set_for_state(raw))
                .then(|| StateId::new(raw).expect("generated state id is valid"))
        })
    }

    fn matches_generated_pairs(&self, pairs: &[(u8, u8)]) -> bool {
        self.len as usize == pairs.len()
            && self.entries[..self.len as usize]
                .iter()
                .zip(pairs)
                .all(|(property, &(key, value))| {
                    property.key as u8 == key && property.value as u8 == value
                })
    }

    /// Parses a bracketed property list such as `[facing=north,waterlogged=false]`.
    ///
    /// Parsing is intentionally strict: unknown keys, unknown built-in values,
    /// duplicate keys and malformed pairs are rejected rather than coerced.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let body = text
            .strip_prefix('[')
            .and_then(|text| text.strip_suffix(']'))
            .ok_or(ParseError::MissingBrackets)?;
        if body.is_empty() {
            return Ok(Self::empty());
        }

        let mut pairs = [
            (
                generated::PROPERTY_KEYS[0],
                PropertyValue::builtin(generated::PROPERTY_VALUES[0]),
            );
            MAX_PROPERTIES
        ];
        let mut length = 0usize;
        for pair in body.split(',') {
            let (key, value) = pair.split_once('=').ok_or(ParseError::MalformedPair)?;
            let key = PropertyKey::from_name(key).ok_or(ParseError::UnknownKey)?;
            let value = PropertyValue::from_name(value).ok_or(ParseError::UnknownValue)?;
            if length == MAX_PROPERTIES {
                return Err(ParseError::TooManyProperties);
            }
            pairs[length] = (key, value);
            length += 1;
        }
        Self::try_from_pairs(&pairs[..length]).map_err(ParseError::InvalidProperties)
    }

    /// The number of properties in this set.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether this set contains no properties.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Looks up a key without constructing a string or allocating.
    #[must_use]
    pub fn get(&self, key: PropertyKey) -> Option<PropertyValue> {
        self.entries[..self.len as usize]
            .iter()
            .find(|property| property.key == key)
            .map(|property| PropertyValue::builtin(property.value))
    }

    /// Iterates over the canonical key-sorted properties by value.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = Property> + '_ {
        self.entries[..self.len as usize].iter().copied()
    }

    /// Inserts one already validated property in canonical key order.
    fn push(&mut self, property: Property) -> Result<(), PropertiesError> {
        let length = self.len as usize;
        if length == MAX_PROPERTIES {
            return Err(PropertiesError::TooManyProperties);
        }
        let position = self.entries[..length]
            .iter()
            .position(|existing| existing.key >= property.key)
            .unwrap_or(length);
        if position < length && self.entries[position].key == property.key {
            return Err(PropertiesError::DuplicateKey(property.key));
        }
        for index in (position..length).rev() {
            self.entries[index + 1] = self.entries[index];
        }
        self.entries[position] = property;
        self.len += 1;
        Ok(())
    }
}

impl fmt::Display for Properties {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[")?;
        for (index, property) in self.iter().enumerate() {
            if index != 0 {
                formatter.write_str(",")?;
            }
            write!(formatter, "{}={}", property.key().name(), property.value())?;
        }
        formatter.write_str("]")
    }
}

/// A failure while constructing a complete property set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertiesError {
    /// A pair was not valid for the generated built-in domain.
    InvalidProperty(PropertyError),
    /// The key appeared more than once.
    DuplicateKey(PropertyKey),
    /// The set exceeds the generated maximum.
    TooManyProperties,
}

impl fmt::Display for PropertiesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProperty(error) => error.fmt(formatter),
            Self::DuplicateKey(key) => write!(formatter, "duplicate property key {:?}", key),
            Self::TooManyProperties => write!(formatter, "too many block-state properties"),
        }
    }
}

impl std::error::Error for PropertiesError {}

/// A strict parser failure at a text boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// The input did not have the required surrounding brackets.
    MissingBrackets,
    /// One comma-separated item did not contain exactly a key/value separator.
    MalformedPair,
    /// The key is absent from the generated census.
    UnknownKey,
    /// The value is absent from the generated census.
    UnknownValue,
    /// The input contains more than [`MAX_PROPERTIES`] entries.
    TooManyProperties,
    /// A typed pair failed validation.
    InvalidProperties(PropertiesError),
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBrackets => formatter.write_str("property list must be bracketed"),
            Self::MalformedPair => formatter.write_str("property pair must contain `=`"),
            Self::UnknownKey => formatter.write_str("unknown block-state property key"),
            Self::UnknownValue => formatter.write_str("unknown block-state property value"),
            Self::TooManyProperties => formatter.write_str("too many block-state properties"),
            Self::InvalidProperties(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ParseError {}

impl fmt::Display for PropertyValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => formatter.write_str(name),
            None => write!(
                formatter,
                "extension({})",
                self.extension_id().expect("extension").index()
            ),
        }
    }
}
