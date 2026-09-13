//! `minecraft:nbt_path` and `minecraft:resource_location` (the latter scoped
//! to `/data storage`'s own `<target>`) — the two argument types `/data
//! storage` and `/execute if data storage` need on top of the SNBT grammar
//! `crate::snbt` already carries.
//!
//! # `NbtPathArg` is a v1 reduction, and says so
//!
//! Vanilla's real nbt-path argument grammar allows a compound-key path
//! segment, an array index (`[3]`), and a *filter* compound after either
//! (`foo{bar:1}`, matching only entries whose sub-tree contains that
//! compound) — enough machinery that it is its own
//! multi-hundred-line class in vanilla. This models the one shape that
//! covers the dominant real use of `/data storage` (a dotted chain of
//! compound keys, `foo.bar.baz`) and refuses `[`, `]`, `{` outright rather
//! than silently mis-parsing them — the same "v1 without a property list"
//! reduction [`crate::BlockArg`] already takes, disclosed rather than
//! silent. `crate::commands::execute`'s module doc (in `lodestone-server`)
//! names this as the reason `if data`'s *storage* form can exist while
//! `if items` (which needs array-indexed paths into an inventory) still
//! cannot.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;

/// `minecraft:nbt_path` — a dot-separated chain of compound keys. Never
/// empty: at least one segment is required (`/execute if data storage <id>
/// <path>`'s `<path>` is mandatory; the no-path form of `/data get storage
/// <id>` is the *absence* of this argument at the tree level, not an empty
/// one parsed by it).
#[derive(Debug, Clone, Copy, Default)]
pub struct NbtPathArg;

impl ArgumentType for NbtPathArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let mut segments = Vec::new();
        loop {
            let segment_start = reader.cursor();
            match reader.peek() {
                Some('[' | ']' | '{' | '}' | '"' | '\'') => {
                    reader.set_cursor(start);
                    return Err(ParseError::new(
                        segment_start,
                        ParseErrorKind::InvalidBool(
                            "array indices and filter compounds are not supported yet".to_string(),
                        ),
                    ));
                }
                _ => {}
            }
            // Not `reader.read_unquoted_string()`: that reader's own allowed
            // set includes `.`, which is this grammar's separator, so it
            // would swallow the whole dotted chain as one segment. Read the
            // narrower charset by hand instead, stopping at `.` or a space.
            let mut segment = String::new();
            while let Some(c) = reader.peek() {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+') {
                    segment.push(c);
                    reader.skip();
                } else {
                    break;
                }
            }
            if segment.is_empty() {
                reader.set_cursor(start);
                return Err(ParseError::new(segment_start, ParseErrorKind::ExpectedInt));
            }
            segments.push(segment);
            if reader.peek() == Some('.') {
                reader.skip();
                continue;
            }
            break;
        }
        // A trailing `[`/`{` right after the last segment (`foo[0]`,
        // `foo{bar:1}`) is the same unsupported grammar the loop's own guard
        // refuses at the *start* of a segment — checked here too so this
        // returns a refusal instead of silently truncating to `foo` and
        // leaving `[0]` for whatever reads the rest of the command to choke
        // on unexplained.
        if matches!(reader.peek(), Some('[' | '{')) {
            reader.set_cursor(start);
            return Err(ParseError::new(
                reader.cursor(),
                ParseErrorKind::InvalidBool(
                    "array indices and filter compounds are not supported yet".to_string(),
                ),
            ));
        }
        Ok(ParsedValue::dynamic(segments))
    }
}

impl McArg for NbtPathArg {
    type Value = Vec<String>;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::NbtPath
    }
}

/// `minecraft:resource_location`, scoped to `/data storage <target>` — a
/// bare namespaced id, defaulted to `minecraft:` with no namespace given,
/// same rule [`crate::DimensionArg`] applies, but with no census to check
/// against: a storage id is created by *use*, not registered ahead of time
/// (vanilla's own command-storage accessor creates the backing tag on first write).
#[derive(Debug, Clone, Copy, Default)]
pub struct StorageIdArg;

impl ArgumentType for StorageIdArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let mut id = String::new();
        while reader.can_read() {
            match reader.peek() {
                Some(c)
                    if c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || matches!(c, '_' | ':' | '/' | '.' | '-') =>
                {
                    id.push(c);
                    reader.skip();
                }
                _ => break,
            }
        }
        if id.is_empty() {
            return Err(ParseError::new(start, ParseErrorKind::ExpectedInt));
        }
        let qualified = if id.contains(':') { id } else { format!("minecraft:{id}") };
        Ok(ParsedValue::dynamic(qualified))
    }
}

impl McArg for StorageIdArg {
    type Value = String;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::ResourceLocation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> Vec<String> {
        let mut reader = StringReader::new(text);
        NbtPathArg.parse(&mut reader).unwrap().downcast_ref::<Vec<String>>().unwrap().clone()
    }

    #[test]
    fn a_single_segment_and_a_dotted_chain_both_parse() {
        assert_eq!(path("foo"), vec!["foo".to_string()]);
        assert_eq!(path("foo.bar.baz"), vec!["foo".to_string(), "bar".to_string(), "baz".to_string()]);
    }

    #[test]
    fn array_indices_and_filter_compounds_are_refused_not_mis_parsed() {
        for bad in ["foo[0]", "foo{bar:1}", "[0]"] {
            let mut reader = StringReader::new(bad);
            assert!(NbtPathArg.parse(&mut reader).is_err(), "{bad:?} must be refused, not silently truncated");
            assert_eq!(reader.cursor(), 0, "a failed parse rewinds");
        }
    }

    #[test]
    fn an_empty_path_is_refused() {
        let mut reader = StringReader::new("");
        assert!(NbtPathArg.parse(&mut reader).is_err());
    }

    #[test]
    fn a_storage_id_defaults_to_the_minecraft_namespace() {
        let mut reader = StringReader::new("foo:bar");
        let id = StorageIdArg.parse(&mut reader).unwrap().downcast_ref::<String>().unwrap().clone();
        assert_eq!(id, "foo:bar");

        let mut reader = StringReader::new("bar");
        let id = StorageIdArg.parse(&mut reader).unwrap().downcast_ref::<String>().unwrap().clone();
        assert_eq!(id, "minecraft:bar");
    }

    #[test]
    fn the_wire_identities_are_nbt_path_and_resource_location() {
        assert_eq!(NbtPathArg.wire(), ArgumentParser::NbtPath);
        assert_eq!(StorageIdArg.wire(), ArgumentParser::ResourceLocation);
    }
}
