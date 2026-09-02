//! Brigadier's primitive argument types, plus registration of custom ones.
//!
//! Every built-in type here has a direct counterpart in the upstream
//! command-parser library's own primitive argument types: integer, long,
//! float, double, bool, and string. Minecraft-flavoured types
//! (player name, block id, entity selector, ...) are deliberately **not**
//! here — they depend on this substrate, not part of
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
/// half of the plugin extension point: a plugin builds one, hands it to `register`,
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

/// The three flavours of string argument. **This is the classic
/// Brigadier trap**: `Word` and `Quotable` are indistinguishable from
/// `Greedy` on a single-token input, and from each other on any unquoted
/// input — they only diverge once the input contains a space (word vs.
/// greedy) or a quote (quotable vs. word). See
/// `tests/brigadier_spec.rs::greedy_vs_single_word_disagree_on_multi_token_input`
/// for the input chosen specifically to make them disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringKind {
    /// Reads a single unquoted token, stopping at the first space.
    Word,
    /// Reads a quoted string if the next char is a quote, otherwise
    /// identical to `Word`.
    Quotable,
    /// Everything from the cursor to
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

/// Supplies completion candidates that are not known until the moment of the
/// query — the primitive the Minecraft-flavoured argument types need.
///
/// [`ArgumentType::suggest`] takes only the partial token, so a type whose
/// candidates are *live state* (the online player list, the loaded worlds, a
/// plugin's own registry keys) has nowhere to read them from. Rather than
/// widening the trait with a context parameter every built-in would ignore,
/// such a type holds an `Arc<dyn SuggestionProvider>` and asks it.
///
/// # Why this rather than a context argument on `suggest`
///
/// A context parameter would have to be typed, and the only honest type is
/// something ECS-shaped — which this crate must not depend on. A provider is
/// a closure the *caller* built while it still had access to whatever it
/// needed, so the dependency points the right way:
/// `lodestone_ecs::commands` captures what it needs and hands the closure in.
///
/// The cost, which is real: a provider is called during suggestion and must
/// not block or take a lock the caller already holds. `lodestone_ecs::commands`
/// snapshots player names into a plain `Vec` rather than closing over the
/// `World`, for exactly that reason.
pub trait SuggestionProvider: Send + Sync {
    fn candidates(&self) -> Vec<String>;
}

impl<F> SuggestionProvider for F
where
    F: Fn() -> Vec<String> + Send + Sync,
{
    fn candidates(&self) -> Vec<String> {
        self()
    }
}

/// A string argument whose completions come from a [`SuggestionProvider`] —
/// the "player name, block id" shape the plugin layer will need, without
/// this crate having to
/// know what a player or a block is.
///
/// Parsing is a plain unquoted word (`StringReader::readUnquotedString`), so
/// this is `StringArgument::word()` plus live suggestions. `strict` decides
/// whether a value *outside* the candidate list is rejected:
///
/// - `strict: false` — anything word-shaped parses. Right for a player name,
///   because vanilla accepts an offline player's name in most commands and the
///   candidate list is only who happens to be online.
/// - `strict: true` — the value must be in the candidate list. Right for a
///   closed set like a block id, where a typo should fail at parse rather than
///   reach the executor.
///
/// The `strict` distinction is the whole reason this is not just
/// `StringArgument::word()` with suggestions bolted on, and getting it wrong is
/// silent: a non-strict block id would hand the executor `"stnoe"` and make the
/// mistake look like a bug in the executor.
pub struct ChoicesArgument {
    provider: Arc<dyn SuggestionProvider>,
    strict: bool,
}

impl ChoicesArgument {
    /// Suggest from `provider`, but accept any word-shaped value.
    pub fn lenient(provider: Arc<dyn SuggestionProvider>) -> Self {
        Self { provider, strict: false }
    }

    /// Suggest from `provider` and reject anything not in it.
    pub fn strict(provider: Arc<dyn SuggestionProvider>) -> Self {
        Self { provider, strict: true }
    }

    /// A fixed candidate list — the common case for a closed set that does not
    /// change at runtime.
    pub fn fixed(candidates: Vec<String>, strict: bool) -> Self {
        Self {
            provider: Arc::new(move || candidates.clone()),
            strict,
        }
    }
}

impl std::fmt::Debug for ChoicesArgument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChoicesArgument")
            .field("strict", &self.strict)
            .field("candidates", &self.provider.candidates())
            .finish()
    }
}

impl ArgumentType for ChoicesArgument {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let value = reader.read_unquoted_string();
        if self.strict && !self.provider.candidates().iter().any(|c| c == &value) {
            reader.set_cursor(start);
            // `InvalidBool`-style "found this, expected one of" is the closest
            // built-in shape; a dedicated variant would be nicer but this crate
            // keeps `ParseErrorKind` aligned with Brigadier's own set plus the
            // one addition permission gating forced (`NoPermission`).
            return Err(ParseError::new(
                start,
                ParseErrorKind::InvalidBool(value),
            ));
        }
        Ok(ParsedValue::String(value))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        // Unfiltered on purpose: `CommandTree::suggest` applies the
        // case-insensitive prefix filter uniformly for every node kind, exactly
        // as `SharedSuggestionProvider.suggest` does. Filtering here as well
        // would be harmless but would put the same rule in two places.
        self.provider.candidates()
    }
}

/// A lookup table a plugin populates with its own [`ArgumentType`]
/// implementations, keyed by name — a way for a plugin to
/// register a custom `ArgumentType` with the same two functions (parse and
/// suggest). This is a convenience for *sharing* named types across a
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
