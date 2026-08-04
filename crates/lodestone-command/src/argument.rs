//! Brigadier's primitive argument types, plus registration of custom ones.
//!
//! Every built-in type here has a direct counterpart in
//! `com.mojang.brigadier.arguments` (brigadier 1.3.10): `IntegerArgumentType`,
//! `LongArgumentType`, `FloatArgumentType`, `DoubleArgumentType`,
//! `BoolArgumentType`, `StringArgumentType`. Minecraft-flavoured types
//! (player name, block id, entity selector, ...) are deliberately **not**
//! here — issue #119 lists them as depending on this substrate, not part of
//! it, and building one would mean guessing at a wire format nobody has
//! decoded yet (see the crate doc for `COMMANDS`/`COMMAND_SUGGESTIONS`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{ParseError, ParseErrorKind};
use crate::node::ParsedValue;
use crate::reader::StringReader;

/// A parser for one argument slot: how to consume it from a [`StringReader`],
/// and how to suggest completions for a partially-typed value.
///
/// Object-safe by design — nodes hold `Arc<dyn ArgumentType>`, and
/// [`ArgumentTypeRegistry`] is exactly this trait's "register a custom type"
/// half of issue #119's scope: a plugin builds one, hands it to `register`,
/// and any tree built from that registry's `get()` can use it exactly like a
/// built-in.
pub trait ArgumentType: Send + Sync {
    /// Consume this argument's value from the reader, starting at the
    /// cursor. On success, the cursor must be left immediately after the
    /// consumed text (mid-token is fine — the tree adds the
    /// separator/end-of-input check around every argument node, not the
    /// argument type itself, matching `CommandDispatcher::parseNodes`). On
    /// failure, returning without moving the cursor is *not* required — the
    /// caller (`CommandTree::parse`) always restores its own checkpoint.
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError>;

    /// Static completion candidates for this type, given the partially-typed
    /// text for this slot (not yet filtered — [`crate::node::CommandTree::suggest`]
    /// does the case-insensitive prefix filter uniformly for every node kind,
    /// mirroring `SharedSuggestionProvider.suggest`). Most primitive types
    /// have none; [`BoolArgument`] is the one built-in exception.
    fn suggest(&self, _partial: &str) -> Vec<String> {
        Vec::new()
    }
}

/// `IntegerArgumentType`. Default range is the full `i32` domain.
#[derive(Debug)]
pub struct IntegerArgument {
    pub min: i32,
    pub max: i32,
}

impl IntegerArgument {
    pub fn new() -> Self {
        Self { min: i32::MIN, max: i32::MAX }
    }

    pub fn bounded(min: i32, max: i32) -> Self {
        Self { min, max }
    }
}

impl Default for IntegerArgument {
    fn default() -> Self {
        Self::new()
    }
}

impl ArgumentType for IntegerArgument {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let result = reader.read_int()?;
        if result < self.min {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::IntegerTooLow { found: result, min: self.min }));
        }
        if result > self.max {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::IntegerTooHigh { found: result, max: self.max }));
        }
        Ok(ParsedValue::Integer(result))
    }
}

/// `LongArgumentType`.
#[derive(Debug)]
pub struct LongArgument {
    pub min: i64,
    pub max: i64,
}

impl LongArgument {
    pub fn new() -> Self {
        Self { min: i64::MIN, max: i64::MAX }
    }

    pub fn bounded(min: i64, max: i64) -> Self {
        Self { min, max }
    }
}

impl Default for LongArgument {
    fn default() -> Self {
        Self::new()
    }
}

impl ArgumentType for LongArgument {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let result = reader.read_long()?;
        if result < self.min {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::LongTooLow { found: result, min: self.min }));
        }
        if result > self.max {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::LongTooHigh { found: result, max: self.max }));
        }
        Ok(ParsedValue::Long(result))
    }
}

/// `FloatArgumentType`.
#[derive(Debug)]
pub struct FloatArgument {
    pub min: f32,
    pub max: f32,
}

impl FloatArgument {
    pub fn new() -> Self {
        Self { min: f32::NEG_INFINITY, max: f32::INFINITY }
    }

    pub fn bounded(min: f32, max: f32) -> Self {
        Self { min, max }
    }
}

impl Default for FloatArgument {
    fn default() -> Self {
        Self::new()
    }
}

impl ArgumentType for FloatArgument {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let result = reader.read_float()?;
        if result < self.min {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::FloatTooLow { found: result, min: self.min }));
        }
        if result > self.max {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::FloatTooHigh { found: result, max: self.max }));
        }
        Ok(ParsedValue::Float(result))
    }
}

/// `DoubleArgumentType`.
#[derive(Debug)]
pub struct DoubleArgument {
    pub min: f64,
    pub max: f64,
}

impl DoubleArgument {
    pub fn new() -> Self {
        Self { min: f64::NEG_INFINITY, max: f64::INFINITY }
    }

    pub fn bounded(min: f64, max: f64) -> Self {
        Self { min, max }
    }
}

impl Default for DoubleArgument {
    fn default() -> Self {
        Self::new()
    }
}

impl ArgumentType for DoubleArgument {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let result = reader.read_double()?;
        if result < self.min {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::DoubleTooLow { found: result, min: self.min }));
        }
        if result > self.max {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::DoubleTooHigh { found: result, max: self.max }));
        }
        Ok(ParsedValue::Double(result))
    }
}

/// `BoolArgumentType`. The one built-in type with non-empty suggestions:
/// vanilla's `listSuggestions` offers `"true"`/`"false"` unconditionally and
/// relies on the generic prefix filter to narrow them — this does the same.
#[derive(Debug, Default)]
pub struct BoolArgument;

impl ArgumentType for BoolArgument {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        Ok(ParsedValue::Bool(reader.read_bool()?))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        vec!["true".to_string(), "false".to_string()]
    }
}

/// The three flavours of `StringArgumentType`. **This is the classic
/// Brigadier trap**: `Word` and `Quotable` are indistinguishable from
/// `Greedy` on a single-token input, and from each other on any unquoted
/// input — they only diverge once the input contains a space (word vs.
/// greedy) or a quote (quotable vs. word). See
/// `tests/brigadier_spec.rs::greedy_vs_single_word_disagree_on_multi_token_input`
/// for the input chosen specifically to make them disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringKind {
    /// `StringArgumentType.word()`: `reader.readUnquotedString()`.
    Word,
    /// `StringArgumentType.string()`: `reader.readString()` — quoted if the
    /// next char is a quote, otherwise identical to `Word`.
    Quotable,
    /// `StringArgumentType.greedyString()`: everything from the cursor to
    /// the end of input, unconditionally, including spaces and quotes.
    Greedy,
}

#[derive(Debug)]
pub struct StringArgument {
    pub kind: StringKind,
}

impl StringArgument {
    pub fn word() -> Self {
        Self { kind: StringKind::Word }
    }

    pub fn quotable() -> Self {
        Self { kind: StringKind::Quotable }
    }

    pub fn greedy() -> Self {
        Self { kind: StringKind::Greedy }
    }
}

impl ArgumentType for StringArgument {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        match self.kind {
            StringKind::Word => Ok(ParsedValue::String(reader.read_unquoted_string())),
            StringKind::Quotable => Ok(ParsedValue::String(reader.read_string()?)),
            StringKind::Greedy => {
                let text = reader.remaining();
                reader.set_cursor(reader.len());
                Ok(ParsedValue::String(text))
            }
        }
    }
}

/// A lookup table a plugin populates with its own [`ArgumentType`]
/// implementations, keyed by name — issue #119's "a way for a plugin to
/// register a custom `ArgumentType` with the same two functions [parse and
/// suggest]". This is a convenience for *sharing* named types across a
/// plugin's own command declarations; a tree never needs one, since
/// [`crate::node::CommandTree::add_argument`] takes an `Arc<dyn ArgumentType>`
/// directly.
#[derive(Default)]
pub struct ArgumentTypeRegistry {
    types: HashMap<String, Arc<dyn ArgumentType>>,
}

impl ArgumentTypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: impl Into<String>, argument_type: Arc<dyn ArgumentType>) {
        self.types.insert(name.into(), argument_type);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ArgumentType>> {
        self.types.get(name).cloned()
    }
}

impl std::fmt::Debug for ArgumentTypeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArgumentTypeRegistry")
            .field("registered", &self.types.keys().collect::<Vec<_>>())
            .finish()
    }
}
