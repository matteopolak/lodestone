//! A Brigadier-compatible cursor over command text.
//!
//! Ported from `com.mojang.brigadier.StringReader` (brigadier 1.3.10, decompiled
//! source consulted at `github.com/Mojang/brigadier`). Every method here mirrors
//! the Java method of the same name, including two easy-to-miss details that
//! change *where* a parse error is reported:
//!
//! - [`StringReader::read_int`] (and its long/float/double siblings) resets the
//!   cursor back to the start of the number **before** raising an "invalid"
//!   error. The candidate text has already been consumed by the time the parse
//!   fails, but the reported position is the start of the token, not its end.
//! - [`StringReader::read_string_until`] steps the cursor back by one before
//!   raising an invalid-escape error, so the reported position is the bad
//!   character itself rather than one past it.
//!
//! Positions are counted in `char`s, not bytes — the tree only ever sees short
//! ASCII-ish command text, so this is simpler than tracking UTF-8 boundaries and
//! never disagrees with vanilla in practice.

use crate::error::{ParseError, ParseErrorKind};

/// A cursor over command input, with the exact primitive readers Brigadier's
/// argument types are built on.
#[derive(Debug, Clone)]
pub struct StringReader {
    chars: Vec<char>,
    cursor: usize,
}

impl StringReader {
    pub fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            cursor: 0,
        }
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor;
    }

    /// Total length, in `char`s.
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn can_read(&self) -> bool {
        self.cursor < self.chars.len()
    }

    pub fn can_read_n(&self, n: usize) -> bool {
        self.cursor + n <= self.chars.len()
    }

    pub fn peek(&self) -> Option<char> {
        self.chars.get(self.cursor).copied()
    }

    /// Advance the cursor by one `char` without reading it.
    pub fn skip(&mut self) {
        self.cursor += 1;
    }

    /// Advance the cursor by `n` `char`s.
    pub fn advance(&mut self, n: usize) {
        self.cursor += n;
    }

    /// Read and consume one `char`.
    pub fn read(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.cursor += 1;
        }
        c
    }

    /// Everything from the cursor to the end, without consuming it.
    pub fn remaining(&self) -> String {
        self.chars[self.cursor..].iter().collect()
    }

    /// The whole source text, regardless of cursor position.
    pub fn source(&self) -> String {
        self.chars.iter().collect()
    }

    /// The token starting at the cursor, up to (but not including) the next
    /// `' '` or the end of input. Does not consume anything — callers that
    /// match it against a literal advance by `token.chars().count()`
    /// themselves. This is Brigadier's literal-matching rule restated as a
    /// tokenizer: `LiteralCommandNode::parse` checks
    /// `remaining().starts_with(literal)` plus a following-separator-or-EOF
    /// check, which is equivalent to "the token up to the next space equals
    /// the literal" for any literal that itself contains no space — true by
    /// construction here, since [`crate::node::CommandTree::add_literal`]
    /// rejects names containing `' '`.
    pub fn peek_token(&self) -> String {
        let mut end = self.cursor;
        while end < self.chars.len() && self.chars[end] != ' ' {
            end += 1;
        }
        self.chars[self.cursor..end].iter().collect()
    }

    /// `StringReader.isAllowedInUnquotedString`: `[0-9A-Za-z_.+-]`.
    pub fn is_allowed_in_unquoted_string(c: char) -> bool {
        c.is_ascii_digit()
            || c.is_ascii_uppercase()
            || c.is_ascii_lowercase()
            || matches!(c, '_' | '-' | '.' | '+')
    }

    /// `StringReader.isAllowedNumber`: digits, `.` and `-` — deliberately
    /// permissive (a lone `-` or `1.2.3` is "allowed number" text that then
    /// fails to actually parse, which is exactly vanilla's behaviour: the
    /// invalid-number error reports the *start* of the token, per the module
    /// doc above, not "no such character").
    fn is_allowed_number(c: char) -> bool {
        c.is_ascii_digit() || c == '.' || c == '-'
    }

    fn is_quote(c: char) -> bool {
        c == '"' || c == '\''
    }

    /// `StringReader.readUnquotedString`.
    pub fn read_unquoted_string(&mut self) -> String {
        let start = self.cursor;
        while self.can_read() && Self::is_allowed_in_unquoted_string(self.peek().unwrap()) {
            self.skip();
        }
        self.chars[start..self.cursor].iter().collect()
    }

    /// `StringReader.readString`: a quoted string if the next char is `"` or
    /// `'`, otherwise a plain unquoted word.
    pub fn read_string(&mut self) -> Result<String, ParseError> {
        if !self.can_read() {
            return Ok(String::new());
        }
        let next = self.peek().unwrap();
        if Self::is_quote(next) {
            self.skip();
            self.read_string_until(next)
        } else {
            Ok(self.read_unquoted_string())
        }
    }

    /// `StringReader.readStringUntil`: consumes up to and including
    /// `terminator`, unescaping `\\` + (`terminator` | `\\`). Any other
    /// escaped character is an error at *its own* position (the cursor is
    /// stepped back by one before raising it). Running off the end of input
    /// before the terminator is an unclosed-quote error at the final
    /// position.
    pub fn read_string_until(&mut self, terminator: char) -> Result<String, ParseError> {
        let mut result = String::new();
        let mut escaped = false;
        while self.can_read() {
            let c = self.read().unwrap();
            if escaped {
                if c == terminator || c == '\\' {
                    result.push(c);
                    escaped = false;
                } else {
                    self.set_cursor(self.cursor() - 1);
                    return Err(ParseError::new(self.cursor(), ParseErrorKind::InvalidEscape(c)));
                }
            } else if c == '\\' {
                escaped = true;
            } else if c == terminator {
                return Ok(result);
            } else {
                result.push(c);
            }
        }
        Err(ParseError::new(self.cursor(), ParseErrorKind::UnclosedQuote))
    }

    /// Shared shape of `readInt`/`readLong`/`readFloat`/`readDouble`: consume
    /// the "allowed number" run, and only *then* find out whether it's
    /// actually a valid `T` — on failure the cursor is reset to `start`, so
    /// both the empty-token and the failed-parse error report `start`, never
    /// the end of the bad token.
    fn read_number<T>(
        &mut self,
        expected: ParseErrorKind,
        parse: impl Fn(&str) -> Option<T>,
        invalid: impl Fn(String) -> ParseErrorKind,
    ) -> Result<T, ParseError> {
        let start = self.cursor;
        while self.can_read() && Self::is_allowed_number(self.peek().unwrap()) {
            self.skip();
        }
        let text: String = self.chars[start..self.cursor].iter().collect();
        if text.is_empty() {
            return Err(ParseError::new(start, expected));
        }
        match parse(&text) {
            Some(value) => Ok(value),
            None => {
                self.set_cursor(start);
                Err(ParseError::new(start, invalid(text)))
            }
        }
    }

    pub fn read_int(&mut self) -> Result<i32, ParseError> {
        self.read_number(ParseErrorKind::ExpectedInt, |s| s.parse::<i32>().ok(), ParseErrorKind::InvalidInt)
    }

    pub fn read_long(&mut self) -> Result<i64, ParseError> {
        self.read_number(ParseErrorKind::ExpectedLong, |s| s.parse::<i64>().ok(), ParseErrorKind::InvalidLong)
    }

    pub fn read_float(&mut self) -> Result<f32, ParseError> {
        self.read_number(ParseErrorKind::ExpectedFloat, |s| s.parse::<f32>().ok(), ParseErrorKind::InvalidFloat)
    }

    pub fn read_double(&mut self) -> Result<f64, ParseError> {
        self.read_number(ParseErrorKind::ExpectedDouble, |s| s.parse::<f64>().ok(), ParseErrorKind::InvalidDouble)
    }

    /// `StringReader.readBoolean`: reads a string (quoted or not) and accepts
    /// exactly `"true"`/`"false"`. Like the numeric readers, an invalid value
    /// resets the cursor to the start of the token before reporting the
    /// error there.
    pub fn read_bool(&mut self) -> Result<bool, ParseError> {
        let start = self.cursor;
        let value = self.read_string()?;
        if value.is_empty() {
            return Err(ParseError::new(start, ParseErrorKind::ExpectedBool));
        }
        match value.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => {
                self.set_cursor(start);
                Err(ParseError::new(start, ParseErrorKind::InvalidBool(value)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Oracle: com.mojang.brigadier.StringReader, brigadier 1.3.10.

    #[test]
    fn read_unquoted_string_stops_at_space() {
        let mut r = StringReader::new("hello world");
        assert_eq!(r.read_unquoted_string(), "hello");
        assert_eq!(r.cursor(), 5);
    }

    #[test]
    fn read_int_empty_token_reports_start_position() {
        let mut r = StringReader::new("abc");
        let err = r.read_int().unwrap_err();
        assert_eq!(err.position, 0);
        assert!(matches!(err.kind, ParseErrorKind::ExpectedInt));
    }

    #[test]
    fn read_int_overflow_resets_cursor_and_reports_start() {
        // "999999999999" is all `is_allowed_number` chars (digits), so the
        // full run is consumed before parse::<i32>() fails and the cursor is
        // reset to 0 — position is the *start* of the token, not its end (12).
        let mut r = StringReader::new("999999999999");
        let err = r.read_int().unwrap_err();
        assert_eq!(err.position, 0);
        assert!(matches!(err.kind, ParseErrorKind::InvalidInt(ref s) if s == "999999999999"));
        assert_eq!(r.cursor(), 0);
    }

    #[test]
    fn read_string_until_invalid_escape_points_at_bad_char() {
        // `"a\qb"` : the escaped `q` is invalid (only `\"`/`\\` are legal).
        let mut r = StringReader::new("\"a\\qb\"");
        r.skip(); // consume opening quote
        let err = r.read_string_until('"').unwrap_err();
        // chars: 0='"' 1='a' 2='\\' 3='q' 4='b' 5='"'
        assert_eq!(err.position, 3);
        assert!(matches!(err.kind, ParseErrorKind::InvalidEscape('q')));
    }

    #[test]
    fn read_string_until_unclosed_quote_points_at_end() {
        let mut r = StringReader::new("\"abc");
        r.skip();
        let err = r.read_string_until('"').unwrap_err();
        assert_eq!(err.position, 4);
        assert!(matches!(err.kind, ParseErrorKind::UnclosedQuote));
    }

    #[test]
    fn read_string_until_unescapes_quote_and_backslash() {
        // `"a\"b"` -> the value is a " b (3 chars: a, ", b).
        let mut r = StringReader::new("\"a\\\"b\"");
        r.skip();
        let value = r.read_string_until('"').unwrap();
        assert_eq!(value, "a\"b");
        assert_eq!(r.cursor(), 6);
    }
}
