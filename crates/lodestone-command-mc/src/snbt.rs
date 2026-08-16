//! A textual SNBT parser — `TagParser` (`net.minecraft.nbt.TagParser`), the
//! grammar behind `minecraft:nbt_tag`/`minecraft:nbt_compound_tag` and every
//! `<nbt>` argument vanilla's tree has that this workspace did not, until
//! now, have any way to parse. **No textual SNBT parser existed anywhere in
//! this workspace before this module** — `lodestone-model`'s NBT support is
//! *binary* decode (a different, already-solved problem: see
//! `lodestone_command_mc::item`'s own doc for why `read_component_patch`
//! does not answer this).
//!
//! # What this unblocks, and what it does not
//!
//! This is the grammar half only — text to [`SnbtValue`], with no target to
//! write into and no source to read from. It is the named prerequisite for
//! `/execute if data`/`store … <path>` and for `[minecraft:custom_data=…]`
//! item-component patches, **not** those features themselves: `if data`
//! additionally needs an NBT-storage engine this server does not have (see
//! `crate::commands::execute`'s "what is not built" list), and a component
//! patch additionally needs `lodestone_model::ItemStack` to carry components
//! at all, which it does not (`lodestone-model` is outside this crate's
//! remit to change — see `item`'s own module doc). [`NbtTagArg`]/
//! [`NbtCompoundArg`] exist so a future unit reaches for an existing, tested
//! parser rather than writing a second one.
//!
//! # The grammar
//!
//! `TagParser.readValue`, ported clause by clause:
//!
//! * A compound `{key: value, "quoted key": value, …}` — an unquoted key is
//!   `TagParser`'s own more permissive `readString` charset (letters,
//!   digits, `_+.-`), a quoted key is a normal quoted string.
//! * A list `[value, value, …]`, or one of the three typed arrays `[B; 1b,
//!   2b]`/`[I; 1, 2]`/`[L; 1l, 2l]` — the `;` immediately after `[` and
//!   before any value is what distinguishes a typed array from a plain
//!   list, exactly as vanilla's own lookahead does.
//! * A quoted string (`"…"`/`'…'`, both quote characters, with `\"`/`\\`/…
//!   escapes) or an unquoted one (`TagParser`'s `isAllowedInUnquotedString`:
//!   `[0-9A-Za-z_.+-]`, the flatter set `lodestone_command`'s own
//!   `StringReader::is_allowed_in_unquoted_string` already models).
//! * A number: a run of `[0-9-]` (and at most one `.`, for a float/double),
//!   optionally suffixed `b`/`s`/`l`/`f`/`d` (byte/short/long/float/double,
//!   case-insensitive), unsuffixed integral text becomes [`SnbtValue::Int`]
//!   and unsuffixed text with a `.` becomes [`SnbtValue::Double`] — vanilla's
//!   own `NUMBER_PATTERN`/suffix-dispatch table in
//!   `TagParser.readTypedValue`. `true`/`false` are vanilla's own
//!   `Byte`-valued literals (`1b`/`0b`), not a distinct boolean tag — NBT has
//!   no boolean type.
//! * Anything else unquoted and not a legal number is a bare string
//!   (`TagParser`'s own fallback), which is what lets `{a: hello}` work
//!   without quotes.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;

/// One parsed SNBT value. Deliberately not `lodestone_model`'s binary NBT
/// type — see this module's doc for why the two stay separate — so this has
/// its own small, self-contained shape rather than reaching into a crate
/// this one may not depend on further than it already does.
#[derive(Debug, Clone, PartialEq)]
pub enum SnbtValue {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(String),
    List(Vec<SnbtValue>),
    ByteArray(Vec<i8>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
    /// Insertion order preserved — vanilla's own `CompoundTag` is
    /// insertion-ordered too (`LinkedHashMap`), and preserving it is free
    /// here and occasionally load-bearing for round-trip display.
    Compound(Vec<(String, SnbtValue)>),
}

/// Parses the whole of `text` as one SNBT value, refusing trailing input —
/// `TagParser.parseAsArgument`'s own top-level entry point, which is what
/// [`NbtTagArg`]/[`NbtCompoundArg`] call.
///
/// # Errors
///
/// A [`ParseError`] naming the byte offset of the first thing that could not
/// be parsed, or (for [`parse_compound`]) that parsed but was not a
/// compound.
pub fn parse_value(text: &str) -> Result<SnbtValue, ParseError> {
    let mut reader = StringReader::new(text);
    let value = read_value(&mut reader)?;
    skip_whitespace(&mut reader);
    if reader.can_read() {
        return Err(ParseError::new(reader.cursor(), ParseErrorKind::UnknownArgument));
    }
    Ok(value)
}

/// [`parse_value`], refusing anything that is not a [`SnbtValue::Compound`]
/// at the top level — what `minecraft:nbt_compound_tag` (id argument to a
/// block entity, an item's `minecraft:custom_data`, …) actually accepts.
///
/// # Errors
///
/// See [`parse_value`]; additionally refuses a syntactically valid non-compound.
pub fn parse_compound(text: &str) -> Result<Vec<(String, SnbtValue)>, ParseError> {
    match parse_value(text)? {
        SnbtValue::Compound(entries) => Ok(entries),
        _ => Err(ParseError::new(0, ParseErrorKind::UnknownArgument)),
    }
}

fn skip_whitespace(reader: &mut StringReader) {
    while reader.can_read() && reader.peek().is_some_and(char::is_whitespace) {
        reader.skip();
    }
}

fn read_value(reader: &mut StringReader) -> Result<SnbtValue, ParseError> {
    skip_whitespace(reader);
    match reader.peek() {
        Some('{') => read_compound(reader),
        Some('[') => read_list_or_array(reader),
        Some('"' | '\'') => reader.read_string().map(SnbtValue::String),
        Some(_) => read_unquoted(reader),
        None => Err(ParseError::new(reader.cursor(), ParseErrorKind::ExpectedInt)),
    }
}

fn expect(reader: &mut StringReader, c: char) -> Result<(), ParseError> {
    skip_whitespace(reader);
    if reader.peek() == Some(c) {
        reader.skip();
        Ok(())
    } else {
        Err(ParseError::new(reader.cursor(), ParseErrorKind::ExpectedArgumentSeparator))
    }
}

fn read_compound(reader: &mut StringReader) -> Result<SnbtValue, ParseError> {
    expect(reader, '{')?;
    let mut entries = Vec::new();
    skip_whitespace(reader);
    if reader.peek() == Some('}') {
        reader.skip();
        return Ok(SnbtValue::Compound(entries));
    }
    loop {
        skip_whitespace(reader);
        let key = read_key(reader)?;
        expect(reader, ':')?;
        let value = read_value(reader)?;
        entries.push((key, value));
        skip_whitespace(reader);
        match reader.peek() {
            Some(',') => {
                reader.skip();
            }
            Some('}') => {
                reader.skip();
                break;
            }
            _ => return Err(ParseError::new(reader.cursor(), ParseErrorKind::ExpectedArgumentSeparator)),
        }
    }
    Ok(SnbtValue::Compound(entries))
}

fn read_key(reader: &mut StringReader) -> Result<String, ParseError> {
    match reader.peek() {
        Some('"' | '\'') => reader.read_string(),
        _ => {
            let start = reader.cursor();
            let word = reader.read_unquoted_string();
            if word.is_empty() {
                return Err(ParseError::new(start, ParseErrorKind::ExpectedInt));
            }
            Ok(word)
        }
    }
}

/// `[…]`, dispatching on the `X;` lookahead that distinguishes a typed array
/// from a plain list — checked *before* consuming a first element, exactly
/// as vanilla's own `TagParser.readList` peeks two characters ahead.
fn read_list_or_array(reader: &mut StringReader) -> Result<SnbtValue, ParseError> {
    expect(reader, '[')?;
    skip_whitespace(reader);
    if let Some(kind) = reader.peek() {
        if matches!(kind, 'B' | 'I' | 'L') && peek_at(reader, 1) == Some(';') {
            reader.skip();
            reader.skip();
            return read_typed_array(reader, kind);
        }
    }
    let mut items = Vec::new();
    skip_whitespace(reader);
    if reader.peek() == Some(']') {
        reader.skip();
        return Ok(SnbtValue::List(items));
    }
    loop {
        items.push(read_value(reader)?);
        skip_whitespace(reader);
        match reader.peek() {
            Some(',') => {
                reader.skip();
            }
            Some(']') => {
                reader.skip();
                break;
            }
            _ => return Err(ParseError::new(reader.cursor(), ParseErrorKind::ExpectedArgumentSeparator)),
        }
    }
    Ok(SnbtValue::List(items))
}

fn read_typed_array(reader: &mut StringReader, kind: char) -> Result<SnbtValue, ParseError> {
    let mut bytes = Vec::new();
    let mut ints = Vec::new();
    let mut longs = Vec::new();
    skip_whitespace(reader);
    if reader.peek() == Some(']') {
        reader.skip();
        return Ok(match kind {
            'B' => SnbtValue::ByteArray(bytes),
            'I' => SnbtValue::IntArray(ints),
            _ => SnbtValue::LongArray(longs),
        });
    }
    loop {
        skip_whitespace(reader);
        let start = reader.cursor();
        let text = read_number_text(reader);
        if text.is_empty() {
            return Err(ParseError::new(start, ParseErrorKind::ExpectedInt));
        }
        // A typed array's own elements may carry the matching suffix
        // (`1b`) or not (`1`) — vanilla accepts both; the suffix, if
        // present, must agree with the array's own kind or vanilla itself
        // refuses (`TagParser`'s own `createTypedArray` type check). Kept
        // simple: consume an optional matching suffix, otherwise none.
        let suffix = reader.peek();
        let consumed_suffix = match (kind, suffix) {
            ('B', Some('b' | 'B')) | ('L', Some('l' | 'L')) => {
                reader.skip();
                true
            }
            _ => false,
        };
        let _ = consumed_suffix;
        match kind {
            'B' => bytes.push(text.parse::<i8>().map_err(|_| invalid_number(start, &text))?),
            'I' => ints.push(text.parse::<i32>().map_err(|_| invalid_number(start, &text))?),
            _ => longs.push(text.parse::<i64>().map_err(|_| invalid_number(start, &text))?),
        }
        skip_whitespace(reader);
        match reader.peek() {
            Some(',') => {
                reader.skip();
            }
            Some(']') => {
                reader.skip();
                break;
            }
            _ => return Err(ParseError::new(reader.cursor(), ParseErrorKind::ExpectedArgumentSeparator)),
        }
    }
    Ok(match kind {
        'B' => SnbtValue::ByteArray(bytes),
        'I' => SnbtValue::IntArray(ints),
        _ => SnbtValue::LongArray(longs),
    })
}

fn read_number_text(reader: &mut StringReader) -> String {
    let start = reader.cursor();
    while reader.can_read() && reader.peek().is_some_and(|c| c.is_ascii_digit() || c == '-') {
        reader.skip();
    }
    reader.source().chars().skip(start).take(reader.cursor() - start).collect()
}

fn invalid_number(position: usize, text: &str) -> ParseError {
    ParseError::new(position, ParseErrorKind::InvalidInt(text.to_string()))
}

/// An unquoted token: a legal number (with its own optional suffix and at
/// most one `.`) becomes the matching numeric variant, `true`/`false`
/// become `Byte(1)`/`Byte(0)` (NBT has no boolean type — these are exactly
/// vanilla's own two `Byte`-valued literals), and anything else becomes a
/// bare [`SnbtValue::String`].
fn read_unquoted(reader: &mut StringReader) -> Result<SnbtValue, ParseError> {
    let start = reader.cursor();
    let word = reader.read_unquoted_string();
    if word.is_empty() {
        return Err(ParseError::new(start, ParseErrorKind::ExpectedInt));
    }
    if word.eq_ignore_ascii_case("true") {
        return Ok(SnbtValue::Byte(1));
    }
    if word.eq_ignore_ascii_case("false") {
        return Ok(SnbtValue::Byte(0));
    }
    if let Some(value) = try_number(&word) {
        return Ok(value);
    }
    Ok(SnbtValue::String(word))
}

/// `word` as a number, honouring an optional single-letter suffix and
/// falling back to `Int`/`Double` (by whether a `.` is present) when there is
/// none. `None` for anything that is not legal numeric text at all — the
/// caller's fallback to a bare string.
fn try_number(word: &str) -> Option<SnbtValue> {
    let (body, suffix) = match word.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => (&word[..word.len() - 1], Some(c.to_ascii_lowercase())),
        _ => (word, None),
    };
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '.') {
        return None;
    }
    if body == "-" {
        return None;
    }
    match suffix {
        Some('b') => body.parse::<i8>().ok().map(SnbtValue::Byte),
        Some('s') => body.parse::<i16>().ok().map(SnbtValue::Short),
        Some('l') => body.parse::<i64>().ok().map(SnbtValue::Long),
        Some('f') => body.parse::<f32>().ok().map(SnbtValue::Float),
        Some('d') => body.parse::<f64>().ok().map(SnbtValue::Double),
        Some(_) => None,
        None if body.contains('.') => body.parse::<f64>().ok().map(SnbtValue::Double),
        None => body.parse::<i32>().ok().map(SnbtValue::Int),
    }
}

fn peek_at(reader: &StringReader, offset: usize) -> Option<char> {
    let index = reader.cursor() + offset;
    reader.source().chars().nth(index)
}

/// `minecraft:nbt_tag` — any [`SnbtValue`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NbtTagArg;

impl ArgumentType for NbtTagArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        match read_value(reader) {
            Ok(value) => Ok(ParsedValue::dynamic(value)),
            Err(e) => {
                reader.set_cursor(start);
                Err(e)
            }
        }
    }
}

impl McArg for NbtTagArg {
    type Value = SnbtValue;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::NbtTag
    }
}

/// `minecraft:nbt_compound_tag` — refused unless the parsed value is a
/// [`SnbtValue::Compound`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NbtCompoundArg;

impl ArgumentType for NbtCompoundArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        match read_value(reader) {
            Ok(SnbtValue::Compound(entries)) => Ok(ParsedValue::dynamic(entries)),
            Ok(_) => {
                reader.set_cursor(start);
                Err(ParseError::new(start, ParseErrorKind::UnknownArgument))
            }
            Err(e) => {
                reader.set_cursor(start);
                Err(e)
            }
        }
    }
}

impl McArg for NbtCompoundArg {
    type Value = Vec<(String, SnbtValue)>;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::NbtCompoundTag
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_take_their_suffixes_and_default_to_int_or_double() {
        assert_eq!(parse_value("5").unwrap(), SnbtValue::Int(5));
        assert_eq!(parse_value("-5").unwrap(), SnbtValue::Int(-5));
        assert_eq!(parse_value("5b").unwrap(), SnbtValue::Byte(5));
        assert_eq!(parse_value("5s").unwrap(), SnbtValue::Short(5));
        assert_eq!(parse_value("5l").unwrap(), SnbtValue::Long(5));
        assert_eq!(parse_value("5.5f").unwrap(), SnbtValue::Float(5.5));
        assert_eq!(parse_value("5.5d").unwrap(), SnbtValue::Double(5.5));
        assert_eq!(parse_value("5.5").unwrap(), SnbtValue::Double(5.5));
    }

    #[test]
    fn true_and_false_are_byte_one_and_zero() {
        assert_eq!(parse_value("true").unwrap(), SnbtValue::Byte(1));
        assert_eq!(parse_value("false").unwrap(), SnbtValue::Byte(0));
    }

    #[test]
    fn quoted_and_unquoted_strings_both_parse() {
        assert_eq!(parse_value("hello").unwrap(), SnbtValue::String("hello".to_string()));
        assert_eq!(parse_value("\"hello world\"").unwrap(), SnbtValue::String("hello world".to_string()));
        assert_eq!(parse_value("'single quoted'").unwrap(), SnbtValue::String("single quoted".to_string()));
    }

    #[test]
    fn a_list_and_the_three_typed_arrays_all_parse() {
        assert_eq!(
            parse_value("[1, 2, 3]").unwrap(),
            SnbtValue::List(vec![SnbtValue::Int(1), SnbtValue::Int(2), SnbtValue::Int(3)])
        );
        assert_eq!(parse_value("[B; 1b, 2b]").unwrap(), SnbtValue::ByteArray(vec![1, 2]));
        assert_eq!(parse_value("[I; 1, 2, 3]").unwrap(), SnbtValue::IntArray(vec![1, 2, 3]));
        assert_eq!(parse_value("[L; 1l, 2l]").unwrap(), SnbtValue::LongArray(vec![1, 2]));
        assert_eq!(parse_value("[]").unwrap(), SnbtValue::List(vec![]));
    }

    #[test]
    fn a_compound_preserves_insertion_order_and_nests() {
        let parsed = parse_value("{a: 1, b: {c: 2}, d: \"e\"}").unwrap();
        assert_eq!(
            parsed,
            SnbtValue::Compound(vec![
                ("a".to_string(), SnbtValue::Int(1)),
                ("b".to_string(), SnbtValue::Compound(vec![("c".to_string(), SnbtValue::Int(2))])),
                ("d".to_string(), SnbtValue::String("e".to_string())),
            ])
        );
    }

    #[test]
    fn an_empty_compound_parses() {
        assert_eq!(parse_value("{}").unwrap(), SnbtValue::Compound(vec![]));
    }

    #[test]
    fn parse_compound_refuses_a_non_compound_top_level_value() {
        assert!(parse_compound("5").is_err());
        assert!(parse_compound("[1, 2]").is_err());
        assert_eq!(parse_compound("{a: 1}").unwrap(), vec![("a".to_string(), SnbtValue::Int(1))]);
    }

    #[test]
    fn trailing_input_after_a_complete_value_is_refused() {
        assert!(parse_value("5 6").is_err());
        assert!(parse_value("{a: 1} garbage").is_err());
    }

    #[test]
    fn quoted_keys_and_unquoted_keys_both_work() {
        let parsed = parse_value("{\"quoted key\": 1, plain_key: 2}").unwrap();
        assert_eq!(
            parsed,
            SnbtValue::Compound(vec![
                ("quoted key".to_string(), SnbtValue::Int(1)),
                ("plain_key".to_string(), SnbtValue::Int(2)),
            ])
        );
    }

    #[test]
    fn the_wire_identities_are_the_two_no_payload_nbt_parsers() {
        assert_eq!(NbtTagArg.wire(), ArgumentParser::NbtTag);
        assert_eq!(NbtCompoundArg.wire(), ArgumentParser::NbtCompoundTag);
    }

    #[test]
    fn nbt_compound_arg_refuses_a_bare_scalar_and_a_bare_list() {
        let mut reader = StringReader::new("5");
        assert!(NbtCompoundArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0, "a failed parse rewinds");

        let mut reader = StringReader::new("{a: 1}");
        let value = NbtCompoundArg.parse(&mut reader).unwrap();
        assert_eq!(
            *value.downcast_ref::<Vec<(String, SnbtValue)>>().unwrap(),
            vec![("a".to_string(), SnbtValue::Int(1))]
        );
    }
}
