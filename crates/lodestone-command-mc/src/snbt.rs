//! A textual SNBT parser — vanilla's own tag parser, the
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
//! Vanilla's own value-reading routine, ported clause by clause:
//!
//! * A compound `{key: value, "quoted key": value, …}` — an unquoted key is
//!   vanilla's own more permissive unquoted-string charset (letters,
//!   digits, `_+.-`), a quoted key is a normal quoted string.
//! * A list `[value, value, …]`, or one of the three typed arrays `[B; 1b,
//!   2b]`/`[I; 1, 2]`/`[L; 1l, 2l]` — the `;` immediately after `[` and
//!   before any value is what distinguishes a typed array from a plain
//!   list, exactly as vanilla's own lookahead does.
//! * A quoted string (`"…"`/`'…'`, both quote characters, with `\"`/`\\`/…
//!   escapes) or an unquoted one (vanilla's own unquoted-string character class:
//!   `[0-9A-Za-z_.+-]`, the flatter set `lodestone_command`'s own
//!   `StringReader::is_allowed_in_unquoted_string` already models).
//! * A number: a run of `[0-9-]` (and at most one `.`, for a float/double),
//!   optionally suffixed `b`/`s`/`l`/`f`/`d` (byte/short/long/float/double,
//!   case-insensitive), unsuffixed integral text becomes [`SnbtValue::Int`]
//!   and unsuffixed text with a `.` becomes [`SnbtValue::Double`] — vanilla's
//!   own number-pattern/suffix-dispatch table. `true`/`false` are vanilla's own
//!   byte-valued literals (`1b`/`0b`), not a distinct boolean tag — NBT has
//!   no boolean type.
//! * Anything else unquoted and not a legal number is a bare string
//!   (vanilla's own fallback), which is what lets `{a: hello}` work
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
    /// Insertion order preserved — vanilla's own NBT compound tag is
    /// insertion-ordered too (backed by an order-preserving hash map), and
    /// preserving it is free here and occasionally load-bearing for
    /// round-trip display.
    Compound(Vec<(String, SnbtValue)>),
}

/// Parses the whole of `text` as one SNBT value, refusing trailing input —
/// Vanilla's own top-level entry point, which is what
/// [`NbtTagArg`]/[`NbtCompoundArg`] call.
///
/// # Errors
///
/// A [`ParseError`] naming the byte offset of the first thing that could not
/// be parsed, or (for [`parse_compound`]) that parsed but was not a
/// compound.
pub fn parse_value(text: &str) -> Result<SnbtValue, ParseError> {
    let mut reader = StringReader::new(text);
    let value = read_value_at(&mut reader, 0)?;
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
#[cfg(test)]
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

/// Maximum nesting this parser will walk.
///
/// Compounds and lists nest, so the grammar recurses once per `{` or `[` and
/// nothing in it bounds the depth: the command text is the sender's, and the
/// string that carries a command is read with the protocol's default cap of
/// 32767 characters, which is 32767 available levels. Unbounded, one command
/// of nothing but open brackets exhausts the parsing thread's stack and aborts
/// the process — on a server that is a client crashing its host.
///
/// The bound is 512, the depth past which the game's own reader refuses a
/// serialized structure as too complex — read off the jar's own nesting limit
/// rather than off this codebase's NBT reader, which enforces the same number
/// from the same source. Nothing this parser produces is useful past it: the
/// value's whole purpose is to become a stored or transmitted structure, and a
/// deeper one would be refused at that point anyway. Measured on the platform
/// default 2 MiB stack, this parser walks 1024 levels and not 4096, so the
/// bound sits inside the measurement with room to spare.
const MAX_NESTING: usize = 512;

/// Reads one value, `depth` levels inside enclosing compounds and lists.
///
/// The bound is checked here rather than at the two call sites that descend,
/// because this is the one function both of them reach a nested value through:
/// a compound reads its entries' values through it and a list its items', so a
/// nesting form added to either inherits the bound.
fn read_value_at(reader: &mut StringReader, depth: usize) -> Result<SnbtValue, ParseError> {
    if depth > MAX_NESTING {
        return Err(ParseError::new(
            reader.cursor(),
            ParseErrorKind::NestingTooDeep { limit: MAX_NESTING },
        ));
    }
    skip_whitespace(reader);
    match reader.peek() {
        Some('{') => read_compound(reader, depth),
        Some('[') => read_list_or_array(reader, depth),
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

fn read_compound(reader: &mut StringReader, depth: usize) -> Result<SnbtValue, ParseError> {
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
        let value = read_value_at(reader, depth + 1)?;
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
/// as vanilla's own list reader peeks two characters ahead.
fn read_list_or_array(reader: &mut StringReader, depth: usize) -> Result<SnbtValue, ParseError> {
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
        items.push(read_value_at(reader, depth + 1)?);
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
        // refuses (vanilla's own NBT-text-parser type check on a typed
        // array). Kept
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

/// Renders back to SNBT text — not a byte-exact transcription of whatever
/// was typed (whitespace and key-quoting choices are not preserved, since
/// nothing here retains them), but `parse_value(&value.to_string())` always
/// reproduces `value`. The one production consumer is
/// `crate::commands::nbt_data`'s `/data get storage` feedback line, in
/// `lodestone-server` — this exists here rather than there because it is a
/// property of the type, not of that one call site.
/// The reference chat rendering of a tag: keys bare when they are plain words
/// and quoted otherwise, strings in whichever quote the text does not contain,
/// every numeric tag with its type suffix (floating point in the shortest
/// round-trip digits, scientific below `1e-3` and from `1e7`), lists and
/// arrays joined with `, `, arrays as `[B; 1b, 2b]`, and anything nested past
/// 64 levels or beyond 128 elements folded to `<...>`.
impl std::fmt::Display for SnbtValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_pretty(f, self, 0)
    }
}

const FOLD_DEPTH: usize = 64;
const FOLD_LENGTH: usize = 128;

fn write_pretty(f: &mut std::fmt::Formatter<'_>, value: &SnbtValue, depth: usize) -> std::fmt::Result {
    match value {
        SnbtValue::Byte(v) => write!(f, "{v}b"),
        SnbtValue::Short(v) => write!(f, "{v}s"),
        SnbtValue::Int(v) => write!(f, "{v}"),
        SnbtValue::Long(v) => write!(f, "{v}L"),
        SnbtValue::Float(v) => write!(f, "{}f", java_float(f64::from(*v), format!("{v:e}"))),
        SnbtValue::Double(v) => write!(f, "{}d", java_float(*v, format!("{v:e}"))),
        SnbtValue::String(s) => f.write_str(&quote_and_escape(s)),
        SnbtValue::List(items) if items.is_empty() => f.write_str("[]"),
        SnbtValue::List(_) if depth >= FOLD_DEPTH => f.write_str("[<...>]"),
        SnbtValue::List(items) => {
            f.write_str("[")?;
            for (i, item) in items.iter().take(FOLD_LENGTH).enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write_pretty(f, item, depth + 1)?;
            }
            if items.len() > FOLD_LENGTH {
                f.write_str(", <...>")?;
            }
            f.write_str("]")
        }
        SnbtValue::ByteArray(items) => write_array(f, "B", items, |v| format!("{v}b")),
        SnbtValue::IntArray(items) => write_array(f, "I", items, |v| v.to_string()),
        SnbtValue::LongArray(items) => write_array(f, "L", items, |v| format!("{v}L")),
        SnbtValue::Compound(entries) if entries.is_empty() => f.write_str("{}"),
        SnbtValue::Compound(_) if depth >= FOLD_DEPTH => f.write_str("{<...>}"),
        SnbtValue::Compound(entries) => {
            f.write_str("{")?;
            for (i, (key, item)) in entries.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                if is_simple_key(key) {
                    f.write_str(key)?;
                } else {
                    f.write_str(&quote_and_escape(key))?;
                }
                f.write_str(": ")?;
                write_pretty(f, item, depth + 1)?;
            }
            f.write_str("}")
        }
    }
}

fn write_array<T>(
    f: &mut std::fmt::Formatter<'_>,
    prefix: &str,
    items: &[T],
    show: impl Fn(&T) -> String,
) -> std::fmt::Result {
    write!(f, "[{prefix};")?;
    for (i, item) in items.iter().take(FOLD_LENGTH).enumerate() {
        write!(f, " {}", show(item))?;
        if i != items.len() - 1 {
            f.write_str(",")?;
        }
    }
    if items.len() > FOLD_LENGTH {
        f.write_str(" <...>")?;
    }
    f.write_str("]")
}

fn is_simple_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// Quotes `text` with `"`, or with `'` when the first quote character inside
/// it is a `"`; the chosen quote and the backslash are escaped, as are control
/// characters.
fn quote_and_escape(text: &str) -> String {
    let mut body = String::with_capacity(text.len() + 2);
    let mut quote = None;
    for c in text.chars() {
        match c {
            '\\' => body.push_str("\\\\"),
            '"' | '\'' => {
                let chosen = *quote.get_or_insert(if c == '"' { '\'' } else { '"' });
                if chosen == c {
                    body.push('\\');
                }
                body.push(c);
            }
            '\n' => body.push_str("\\n"),
            '\r' => body.push_str("\\r"),
            '\t' => body.push_str("\\t"),
            '\u{8}' => body.push_str("\\b"),
            '\u{c}' => body.push_str("\\f"),
            c if (c as u32) < 0x20 => body.push_str(&format!("\\x{:02x}", c as u32)),
            c => body.push(c),
        }
    }
    let quote = quote.unwrap_or('"');
    format!("{quote}{body}{quote}")
}

/// A floating-point number laid out as the reference prints it: plain decimal
/// with at least one fractional digit for magnitudes in `[1e-3, 1e7)`, else
/// `d.dddE<exp>`. `scientific` is Rust's shortest round-trip `{:e}` rendering of
/// the same number, which supplies the digits and the exponent.
fn java_float(value: f64, scientific: String) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-Infinity" } else { "Infinity" }.to_owned();
    }
    let (mantissa, exponent) = scientific.split_once('e').expect("{:e} always has an exponent");
    let exponent: i32 = exponent.parse().expect("{:e} exponent is an integer");
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if negative { "-" } else { "" };
    if value == 0.0 {
        return format!("{sign}0.0");
    }
    if (-3..7).contains(&exponent) {
        let point = exponent + 1;
        let body = if point <= 0 {
            format!("0.{}{digits}", "0".repeat((-point) as usize))
        } else if digits.len() as i32 <= point {
            format!("{digits}{}.0", "0".repeat((point as usize) - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        format!("{sign}{body}")
    } else {
        let fraction = if digits.len() > 1 { &digits[1..] } else { "0" };
        format!("{sign}{}.{fraction}E{exponent}", &digits[..1])
    }
}

/// The reference partial-match rule behind `nbt=` and path filters: every key
/// of an expected compound must be present in the actual one with a matching
/// value of the *same tag type*, an expected list matches when each of its
/// elements matches some element of the actual list (an empty expected list
/// matches only an empty one), and everything else compares for equality.
#[must_use]
pub fn compare_nbt(expected: &SnbtValue, actual: &SnbtValue) -> bool {
    if std::mem::discriminant(expected) != std::mem::discriminant(actual) {
        return false;
    }
    match (expected, actual) {
        (SnbtValue::Compound(want), SnbtValue::Compound(have)) => {
            have.len() >= want.len()
                && want.iter().all(|(key, value)| {
                    have.iter().find(|(k, _)| k == key).is_some_and(|(_, held)| compare_nbt(value, held))
                })
        }
        (SnbtValue::List(want), SnbtValue::List(have)) => {
            if want.is_empty() {
                return have.is_empty();
            }
            have.len() >= want.len() && want.iter().all(|w| have.iter().any(|h| compare_nbt(w, h)))
        }
        _ => expected == actual,
    }
}

/// Merges `incoming` into `target` as the reference compound merge does:
/// compounds on both sides merge recursively, everything else is replaced.
pub fn merge_compound(target: &mut Vec<(String, SnbtValue)>, incoming: &[(String, SnbtValue)]) {
    for (key, value) in incoming {
        match target.iter_mut().find(|(k, _)| k == key) {
            Some((_, SnbtValue::Compound(held))) if matches!(value, SnbtValue::Compound(_)) => {
                let SnbtValue::Compound(add) = value else { unreachable!() };
                merge_compound(held, add);
            }
            Some((_, held)) => *held = value.clone(),
            None => target.push((key.clone(), value.clone())),
        }
    }
}

/// Reads a compound at the reader's cursor, as the `nbt=` selector option and a
/// path filter do.
///
/// # Errors
///
/// The [`ParseError`] of the first malformed token.
pub(crate) fn read_compound_value(reader: &mut StringReader) -> Result<Vec<(String, SnbtValue)>, ParseError> {
    match read_compound(reader, 0)? {
        SnbtValue::Compound(entries) => Ok(entries),
        _ => unreachable!("read_compound yields a compound"),
    }
}

/// `minecraft:nbt_tag` — any [`SnbtValue`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NbtTagArg;

impl ArgumentType for NbtTagArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        match read_value_at(reader, 0) {
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
        match read_value_at(reader, 0) {
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

    /// `Display` is not a byte-exact transcription of what was typed (this
    /// module does not retain whitespace or key-quoting style), but
    /// `parse_value(&value.to_string())` must always reproduce `value` —
    /// checked across one of every variant, nested, so a broken bracket or a
    /// missing separator in any single arm shows up.
    #[test]
    fn display_round_trips_through_parse_value_for_every_variant() {
        let value = SnbtValue::Compound(vec![
            ("b".to_string(), SnbtValue::Byte(-5)),
            ("s".to_string(), SnbtValue::Short(300)),
            ("i".to_string(), SnbtValue::Int(-7)),
            ("l".to_string(), SnbtValue::Long(9_000_000_000)),
            ("f".to_string(), SnbtValue::Float(1.5)),
            ("d".to_string(), SnbtValue::Double(-2.25)),
            ("str".to_string(), SnbtValue::String("has \"quotes\"".to_string())),
            ("list".to_string(), SnbtValue::List(vec![SnbtValue::Int(1), SnbtValue::Int(2)])),
            ("ba".to_string(), SnbtValue::ByteArray(vec![1, -2, 3])),
            ("ia".to_string(), SnbtValue::IntArray(vec![10, -20])),
            ("la".to_string(), SnbtValue::LongArray(vec![100, -200])),
            ("nested".to_string(), SnbtValue::Compound(vec![("inner".to_string(), SnbtValue::Int(9))])),
        ]);
        let text = value.to_string();
        assert_eq!(parse_value(&text).unwrap_or_else(|e| panic!("{text:?} failed to re-parse: {e}")), value);
    }

    /// Expected strings are the reference's own renderings (a float always has
    /// a fractional digit, `L` is uppercase, keys quote only when not plain
    /// words, scientific notation outside `[1e-3, 1e7)`).
    #[test]
    fn display_matches_the_reference_chat_rendering() {
        let cases: [(SnbtValue, &str); 14] = [
            (SnbtValue::Float(20.0), "20.0f"),
            (SnbtValue::Double(0.5), "0.5d"),
            (SnbtValue::Double(-3.25), "-3.25d"),
            (SnbtValue::Float(0.001), "0.001f"),
            (SnbtValue::Float(0.0001), "1.0E-4f"),
            (SnbtValue::Double(1_234_567.0), "1234567.0d"),
            (SnbtValue::Double(12_345_678.0), "1.2345678E7d"),
            (SnbtValue::Double(100.0), "100.0d"),
            (SnbtValue::Long(7), "7L"),
            (SnbtValue::String("it's".to_owned()), "\"it's\""),
            (SnbtValue::String("say \"hi\"".to_owned()), "'say \"hi\"'"),
            (SnbtValue::IntArray(vec![1, -2]), "[I; 1, -2]"),
            (SnbtValue::ByteArray(vec![]), "[B;]"),
            (
                SnbtValue::Compound(vec![
                    ("plain_key".to_owned(), SnbtValue::List(vec![SnbtValue::Byte(1), SnbtValue::Byte(0)])),
                    ("a b".to_owned(), SnbtValue::Compound(vec![])),
                ]),
                "{plain_key: [1b, 0b], \"a b\": {}}",
            ),
        ];
        for (value, text) in cases {
            assert_eq!(value.to_string(), text);
        }
    }

    #[test]
    fn compare_nbt_is_partial_and_type_strict() {
        let actual = parse_value("{a: 1b, l: [1, 2, 3], c: {x: 1, y: 2}}").unwrap();
        assert!(compare_nbt(&parse_value("{a: 1b}").unwrap(), &actual));
        assert!(!compare_nbt(&parse_value("{a: 1}").unwrap(), &actual), "int is not byte");
        assert!(compare_nbt(&parse_value("{l: [3, 1]}").unwrap(), &actual), "any order, subset");
        assert!(!compare_nbt(&parse_value("{l: [4]}").unwrap(), &actual));
        assert!(!compare_nbt(&parse_value("{l: []}").unwrap(), &actual), "empty list matches only empty");
        assert!(compare_nbt(&parse_value("{c: {y: 2}}").unwrap(), &actual));
        assert!(!compare_nbt(&parse_value("{missing: 1}").unwrap(), &actual));
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

#[cfg(test)]
mod nesting_budget {
    //! Compounds and lists nest, the depth is the sender's choice, and the
    //! string that carries a command is read with the protocol's default cap
    //! of 32767 characters — so a command of nothing but open brackets is a
    //! 32767-level parse unless something bounds it. These gates pin both
    //! halves of [`MAX_NESTING`]: that a value nested to the bound still
    //! parses, so the bound is reachable rather than a stack overflow behind
    //! an accepted input, and that one level past it is refused by the bound
    //! rather than by running out of input.

    use super::{MAX_NESTING, parse_value};
    use lodestone_command::ParseErrorKind;

    /// Runs on a thread with the platform default stack the real parse gets,
    /// rather than the test harness's own, so the gate measures the stack that
    /// matters.
    fn on_a_default_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(f)
            .expect("spawn")
            .join()
            .expect("the parse overflowed the stack it will actually run on")
    }

    fn brackets(levels: usize) -> String {
        "[".repeat(levels) + &"]".repeat(levels)
    }

    #[test]
    fn a_value_nested_to_the_bound_still_parses() {
        let text = brackets(MAX_NESTING);
        let outcome = on_a_default_stack(move || parse_value(&text).map(|_| ()));
        assert!(
            outcome.is_ok(),
            "a value nested to the bound of {MAX_NESTING} was refused: {outcome:?} — a bound \
             the parser cannot itself reach is a stack overflow behind an accepted input"
        );
    }

    #[test]
    fn a_value_nested_past_the_bound_is_refused_by_the_bound() {
        let text = brackets(MAX_NESTING + 2);
        let error = on_a_default_stack(move || parse_value(&text).map(|_| ()))
            .expect_err("a value nested past the bound parsed");
        assert!(
            matches!(error.kind, ParseErrorKind::NestingTooDeep { limit } if limit == MAX_NESTING),
            "past the bound this failed for some other reason, so the input proves nothing \
             about the bound: {:?}",
            error.kind
        );
    }

    /// The control: the whole of a 32767-character command's worth of nesting
    /// is what the bound exists to refuse, and before it existed this input
    /// aborted the process rather than returning.
    #[test]
    fn a_full_length_command_of_open_brackets_returns_an_error() {
        let text = "[".repeat(32767);
        let error = on_a_default_stack(move || parse_value(&text).map(|_| ()))
            .expect_err("32767 open brackets parsed");
        assert!(
            matches!(error.kind, ParseErrorKind::NestingTooDeep { .. }),
            "{:?}",
            error.kind
        );
    }
}
