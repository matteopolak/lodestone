//! Parse and suggestion errors, positioned exactly as Brigadier positions them.

use std::fmt;

/// Where a parse failed, and why.
///
/// `position` is a `char` offset into the original input (see
/// [`crate::reader::StringReader`] for why `char`s rather than bytes). It is
/// **not always the end of the offending token** — see the module doc on
/// [`crate::reader`] for the two cases (invalid numbers/bools, invalid
/// escapes) where the oracle reports an earlier position than a naive port
/// would guess.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub position: usize,
    pub kind: ParseErrorKind,
}

impl ParseError {
    pub fn new(position: usize, kind: ParseErrorKind) -> Self {
        Self { position, kind }
    }

    /// Ten characters before the failure, followed by the unparsed input.
    /// Offsets stay in characters, including when the input contains Unicode.
    pub fn context(&self, input: &str) -> (String, String) {
        let characters: Vec<char> = input.chars().collect();
        let cursor = self.position.min(characters.len());
        let mut prefix = if cursor > 10 { "...".to_owned() } else { String::new() };
        prefix.extend(characters[cursor.saturating_sub(10)..cursor].iter());
        let remaining = characters[cursor..].iter().collect();
        (prefix, remaining)
    }

    /// Restore the caller's cursor after an alias was rewritten for parsing.
    pub fn remap_root(mut self, input: &str, canonical: &str) -> Self {
        let stripped = input.strip_prefix('/').unwrap_or(input).trim_start();
        let leading = input[..input.len() - stripped.len()].chars().count();
        let original_root = stripped.split(' ').next().unwrap_or("").chars().count();
        let canonical_root = canonical.split(' ').next().unwrap_or("").chars().count();
        self.position = leading + if self.position >= canonical_root {
            original_root + self.position - canonical_root
        } else {
            self.position.min(original_root)
        };
        self
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at position {}", self.kind, self.position)
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn root_remapping_accounts_for_short_long_and_unicode_aliases() {
        for (input, expected) in [("/t 99", 3), ("long_tools_alias 99", 17), ("/ é日🌍 99", 6)] {
            let error = ParseError::new(6, ParseErrorKind::IntegerTooHigh { found: 99, max: 9 })
                .remap_root(input, "tools 99");
            assert_eq!(error.position, expected, "{input}");
            assert_eq!(error.context(input).1, "99");
        }
    }
}

/// Mirrors `com.mojang.brigadier.exceptions.BuiltInExceptions` plus the
/// dispatcher-level errors from `CommandDispatcher::execute`/`parseNodes`,
/// restated as a plain enum (Brigadier uses a factory of exception
/// *templates*, which has no equivalent need here since nothing implements
/// `CommandSource` yet — see the crate doc for why).
#[derive(Debug, Clone, PartialEq)]
pub enum ParseErrorKind {
    /// Nothing at the root matched any token at all — `cursor == 0` and no
    /// node was ever entered. `CommandDispatcher.DISPATCHER_UNKNOWN_COMMAND`.
    UnknownCommand,
    /// At least one node matched, but there is leftover input with no child
    /// able to consume it. `CommandDispatcher.DISPATCHER_UNKNOWN_ARGUMENT`.
    UnknownArgument,
    /// A node matched but is not marked executable, and has no child able to
    /// take the (absent) remaining input.
    NotExecutable,
    /// Two tokens ran together with no separating space.
    /// `CommandDispatcher.DISPATCHER_EXPECTED_ARGUMENT_SEPARATOR`.
    ExpectedArgumentSeparator,
    /// A redirect was about to be followed to a `(node, cursor)` pair already
    /// visited on the current path. An *ordinary* Brigadier-shaped redirect
    /// cycle can't actually reach this — the separator-consumption gate in
    /// `CommandTree::parse` already bounds recursion depth by the input's
    /// length for any tree shape. This exists for the case that gate can't
    /// cover: a custom `ArgumentType` that moves the cursor backward. See the
    /// crate doc's "known simplifications" section and `tests/brigadier_spec.rs`.
    RedirectCycle,
    /// A token matched a node the [`crate::PermissionFilter`] denied, and
    /// nothing else in that position could take it.
    ///
    /// Deliberately distinct from [`ParseErrorKind::UnknownCommand`]: Bukkit
    /// answers a permission-gated command with "you do not have permission",
    /// so a caller needs to be able to tell "no such command" from "not
    /// yours". Note this has **no** counterpart in Brigadier's
    /// `BuiltInExceptions` — vanilla never needs it, because a node the sender
    /// cannot use was already pruned out of the tree they were sent by
    /// vanilla's own usable-command-tree builder, so by the time text
    /// arrives the node
    /// genuinely does not exist for them. We keep one tree and gate at parse
    /// time, so we need the distinction upstream does not.
    ///
    /// [`crate::CommandTree::suggest_filtered`] is silent about the same node —
    /// see [`crate::filter`] for why the two halves differ.
    NoPermission { permission: String },

    ExpectedInt,
    InvalidInt(String),
    ExpectedLong,
    InvalidLong(String),
    ExpectedFloat,
    InvalidFloat(String),
    ExpectedDouble,
    InvalidDouble(String),
    ExpectedBool,
    InvalidBool(String),
    /// An argument-specific explanation, without an unrelated scalar type.
    InvalidArgument(String),

    IntegerTooLow { found: i32, min: i32 },
    IntegerTooHigh { found: i32, max: i32 },
    LongTooLow { found: i64, min: i64 },
    LongTooHigh { found: i64, max: i64 },
    FloatTooLow { found: f32, min: f32 },
    FloatTooHigh { found: f32, max: f32 },
    DoubleTooLow { found: f64, min: f64 },
    DoubleTooHigh { found: f64, max: f64 },

    /// `StringReader.readStringUntil`'s "Expected quote to start a string" —
    /// currently unreachable from any built-in argument type (all of them
    /// tolerate an absent opening quote by falling back to unquoted), kept
    /// for a future type that requires one.
    ExpectedStartOfQuote,
    UnclosedQuote,
    InvalidEscape(char),

    /// A nested value in an argument's own syntax was deeper than the parser
    /// will walk. Distinct from a malformed value: the input is well-formed,
    /// there is simply more of it than any value the game constructs, and the
    /// nesting depth is the sender's choice with nothing in the grammar
    /// bounding it. Carries the limit so the message names it.
    NestingTooDeep { limit: usize },
}

impl ParseErrorKind {
    /// The explanation shown in chat, without diagnostic cursor metadata.
    pub fn player_message(&self) -> String {
        match self {
            Self::UnknownCommand | Self::NotExecutable =>
                "Unknown or incomplete command. See below for error".to_owned(),
            Self::UnknownArgument => "Incorrect argument for command".to_owned(),
            Self::ExpectedArgumentSeparator =>
                "Expected whitespace to end one argument, but found trailing data".to_owned(),
            Self::NoPermission { .. } => "You do not have permission to use this command".to_owned(),
            Self::ExpectedInt => "Expected integer".to_owned(),
            Self::ExpectedLong => "Expected long".to_owned(),
            Self::ExpectedFloat => "Expected float".to_owned(),
            Self::ExpectedDouble => "Expected double".to_owned(),
            Self::ExpectedBool => "Expected boolean".to_owned(),
            Self::InvalidInt(value) => format!("Invalid integer '{value}'"),
            Self::InvalidLong(value) => format!("Invalid long '{value}'"),
            Self::InvalidFloat(value) => format!("Invalid float '{value}'"),
            Self::InvalidDouble(value) => format!("Invalid double '{value}'"),
            Self::InvalidBool(value) =>
                format!("Invalid boolean: expected 'true' or 'false' but found '{value}'"),
            Self::IntegerTooLow { found, min } =>
                format!("Integer must not be less than {min}: found {found}"),
            Self::IntegerTooHigh { found, max } =>
                format!("Integer must not be more than {max}: found {found}"),
            Self::LongTooLow { found, min } =>
                format!("Long must not be less than {min}: found {found}"),
            Self::LongTooHigh { found, max } =>
                format!("Long must not be more than {max}: found {found}"),
            Self::FloatTooLow { found, min } =>
                format!("Float must not be less than {min}: found {found}"),
            Self::FloatTooHigh { found, max } =>
                format!("Float must not be more than {max}: found {found}"),
            Self::DoubleTooLow { found, min } =>
                format!("Double must not be less than {min}: found {found}"),
            Self::DoubleTooHigh { found, max } =>
                format!("Double must not be more than {max}: found {found}"),
            Self::ExpectedStartOfQuote => "Expected quote to start a string".to_owned(),
            Self::UnclosedQuote => "Unclosed quoted string".to_owned(),
            Self::InvalidEscape(value) =>
                format!("Invalid escape sequence '\\{value}' in quoted string"),
            Self::InvalidArgument(message) => message.clone(),
            Self::RedirectCycle => "Command redirects cannot be resolved".to_owned(),
            Self::NestingTooDeep { limit } =>
                format!("Value nests deeper than the permitted {limit} levels"),
        }
    }
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand => write!(f, "unknown command"),
            Self::UnknownArgument => write!(f, "unknown argument"),
            Self::NotExecutable => write!(f, "incomplete command"),
            Self::ExpectedArgumentSeparator => write!(f, "expected whitespace to end one argument, but found trailing data"),
            Self::RedirectCycle => write!(f, "redirect cycle detected"),
            // Bukkit's default `Command.permissionMessage`, which is
            // "I'm sorry, but you do not have permission to perform this
            // command." — reworded to vanilla's shorter register while keeping
            // the node available to a caller that wants to log it.
            Self::NoPermission { permission } => write!(f, "you do not have permission to use this command (requires '{permission}')"),
            Self::ExpectedInt => write!(f, "expected integer"),
            Self::InvalidInt(s) => write!(f, "invalid integer '{s}'"),
            Self::ExpectedLong => write!(f, "expected long"),
            Self::InvalidLong(s) => write!(f, "invalid long '{s}'"),
            Self::ExpectedFloat => write!(f, "expected float"),
            Self::InvalidFloat(s) => write!(f, "invalid float '{s}'"),
            Self::ExpectedDouble => write!(f, "expected double"),
            Self::InvalidDouble(s) => write!(f, "invalid double '{s}'"),
            Self::ExpectedBool => write!(f, "expected bool"),
            Self::InvalidBool(s) => write!(f, "invalid bool, expected 'true' or 'false' but found '{s}'"),
            Self::InvalidArgument(message) => f.write_str(message),
            Self::IntegerTooLow { found, min } => write!(f, "integer must not be less than {min}, found {found}"),
            Self::IntegerTooHigh { found, max } => write!(f, "integer must not be more than {max}, found {found}"),
            Self::LongTooLow { found, min } => write!(f, "long must not be less than {min}, found {found}"),
            Self::LongTooHigh { found, max } => write!(f, "long must not be more than {max}, found {found}"),
            Self::FloatTooLow { found, min } => write!(f, "float must not be less than {min}, found {found}"),
            Self::FloatTooHigh { found, max } => write!(f, "float must not be more than {max}, found {found}"),
            Self::DoubleTooLow { found, min } => write!(f, "double must not be less than {min}, found {found}"),
            Self::DoubleTooHigh { found, max } => write!(f, "double must not be more than {max}, found {found}"),
            Self::ExpectedStartOfQuote => write!(f, "expected quote to start a string"),
            Self::UnclosedQuote => write!(f, "unclosed quoted string"),
            Self::InvalidEscape(c) => write!(f, "invalid escape sequence '{c}' in quoted string"),
            Self::NestingTooDeep { limit } => {
                write!(f, "value nests deeper than the permitted {limit} levels")
            }
        }
    }
}
