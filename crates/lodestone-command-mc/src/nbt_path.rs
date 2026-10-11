//! `minecraft:nbt_path` — the reference NBT path grammar and its engine over
//! [`SnbtValue`] trees — and `minecraft:resource_location` (the latter scoped
//! to `/data storage`'s own `<target>`).
//!
//! # Grammar
//!
//! A path is nodes joined by `.`: a key (`foo`, or quoted `"a b"`), a key with
//! a filter (`foo{bar:1}`, matching when the value there contains that compound),
//! an element selector after a key (`[0]`, negative from the end; `[]`, every
//! element; `[{id:"x"}]`, the elements containing that compound) and, as the
//! first node only, a root filter (`{bar:1}`). `foo[0]bar` needs no dot after a
//! bracket.
//!
//! # Engine
//!
//! [`NbtPath::get`] walks a tree and fails with the "found no elements" message
//! naming the path up to the node that matched nothing. [`NbtPath::set`],
//! [`NbtPath::insert`], [`NbtPath::merge`] and [`NbtPath::remove`] mutate in
//! place with the reference's create-missing-parents rules and return how many
//! places changed (zero is the caller's "nothing changed" refusal). Element
//! selectors reach into lists; the typed arrays are readable by index but not
//! mutable through a path.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;
use crate::snbt::{SnbtValue, compare_nbt, merge_compound, read_compound_value};

/// The deepest tree a path write accepts.
const MAX_DEPTH: usize = 512;

#[derive(Debug, Clone, PartialEq)]
enum PathNode {
    Key(String),
    KeyMatching(String, SnbtValue),
    Index(i32),
    AllElements,
    ElementMatching(SnbtValue),
    RootMatching(SnbtValue),
}

/// A parsed path. Keeps its source text for messages.
#[derive(Debug, Clone, PartialEq)]
pub struct NbtPath {
    original: String,
    nodes: Vec<PathNode>,
    /// For each node, the length of `original` up to and including it.
    ends: Vec<usize>,
}

impl std::fmt::Display for NbtPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.original)
    }
}

impl NbtPath {
    /// Parses `text` as a whole path.
    ///
    /// # Errors
    ///
    /// The [`ParseError`] of the first malformed node.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut reader = StringReader::new(text);
        let path = read_path(&mut reader)?;
        if reader.can_read() {
            return Err(ParseError::new(reader.cursor(), ParseErrorKind::UnknownArgument));
        }
        Ok(path)
    }

    /// The path as typed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.original
    }

    /// Every value the path selects in `root`.
    ///
    /// # Errors
    ///
    /// "Found no elements matching" the path prefix that selected nothing.
    pub fn get(&self, root: &SnbtValue) -> Result<Vec<SnbtValue>, String> {
        let mut level = vec![root.clone()];
        for (node, &end) in self.nodes.iter().zip(&self.ends) {
            level = level.iter().flat_map(|parent| node.select(parent)).collect();
            if level.is_empty() {
                return Err(self.not_found(end));
            }
        }
        Ok(level)
    }

    /// How many values the path selects, zero when it dead-ends.
    #[must_use]
    pub fn count_matching(&self, root: &SnbtValue) -> usize {
        self.get(root).map_or(0, |found| found.len())
    }

    /// Writes `value` at every place the path selects, creating missing
    /// parents, and returns how many places changed.
    ///
    /// # Errors
    ///
    /// The too-deep refusal, or a path that dead-ends while creating parents.
    pub fn set(&self, root: &mut SnbtValue, value: &SnbtValue) -> Result<usize, String> {
        if too_deep(value, self.nodes.len()) {
            return Err(TOO_DEEP.to_owned());
        }
        let last = self.nodes.last().ok_or_else(|| self.not_found(0))?;
        let mut changed = 0;
        for parent in self.parents_mut(root)? {
            changed += last.set_in(parent, value);
        }
        Ok(changed)
    }

    /// Inserts `values` into every list the path selects (creating an empty
    /// one when the path ends nowhere), at `index` (negative from the end, `-1`
    /// appending), and returns how many lists changed.
    ///
    /// # Errors
    ///
    /// The too-deep refusal, a target that is not a list, or an index outside
    /// the list.
    pub fn insert(&self, index: i32, root: &mut SnbtValue, values: &[SnbtValue]) -> Result<usize, String> {
        if values.iter().any(|v| too_deep(v, self.nodes.len())) {
            return Err(TOO_DEEP.to_owned());
        }
        let mut modified = 0;
        for target in self.get_or_create(root, &|| SnbtValue::List(Vec::new()))? {
            let SnbtValue::List(items) = target else {
                return Err(format!("Expected a list: got {target}"));
            };
            let at = if index < 0 { items.len() as i64 + i64::from(index) + 1 } else { i64::from(index) };
            if at < 0 || at > items.len() as i64 {
                return Err(format!("Invalid list index: {at}"));
            }
            let at = at as usize;
            for (offset, value) in values.iter().enumerate() {
                items.insert(at + offset, value.clone());
            }
            modified += usize::from(!values.is_empty());
        }
        Ok(modified)
    }

    /// Merges `other` into every compound the path selects, creating an empty
    /// one when the path ends nowhere, and returns how many compounds changed.
    ///
    /// # Errors
    ///
    /// A target that is not a compound.
    pub fn merge(&self, root: &mut SnbtValue, other: &[(String, SnbtValue)]) -> Result<usize, String> {
        let mut changed = 0;
        for target in self.get_or_create(root, &|| SnbtValue::Compound(Vec::new()))? {
            let SnbtValue::Compound(held) = target else {
                return Err(format!("Expected an object: got {target}"));
            };
            let before = held.clone();
            merge_compound(held, other);
            changed += usize::from(*held != before);
        }
        Ok(changed)
    }

    /// Removes every value the path selects and returns how many were removed.
    pub fn remove(&self, root: &mut SnbtValue) -> usize {
        let Some((last, upper)) = self.nodes.split_last() else { return 0 };
        let mut level = vec![root];
        for node in upper {
            level = level.into_iter().flat_map(|parent| node.select_mut(parent, None)).collect();
        }
        level.into_iter().map(|parent| last.remove_from(parent)).sum()
    }

    fn not_found(&self, end: usize) -> String {
        format!("Found no elements matching {}", &self.original[..end])
    }

    /// The nodes of every level but the last, created on the way.
    fn parents_mut<'a>(&self, root: &'a mut SnbtValue) -> Result<Vec<&'a mut SnbtValue>, String> {
        let mut level = vec![root];
        for (i, node) in self.nodes.iter().enumerate().take(self.nodes.len().saturating_sub(1)) {
            let make = self.nodes[i + 1].preferred_parent();
            level = level.into_iter().flat_map(|parent| node.select_mut(parent, Some(&|| make.clone()))).collect();
            if level.is_empty() {
                return Err(self.not_found(self.ends[i]));
            }
        }
        Ok(level)
    }

    fn get_or_create<'a>(
        &self,
        root: &'a mut SnbtValue,
        leaf: &dyn Fn() -> SnbtValue,
    ) -> Result<Vec<&'a mut SnbtValue>, String> {
        let last = self.nodes.last().ok_or_else(|| self.not_found(0))?;
        let parents = self.parents_mut(root)?;
        Ok(parents.into_iter().flat_map(|parent| last.select_mut(parent, Some(leaf))).collect())
    }
}

const TOO_DEEP: &str = "Tag is too deeply nested";

fn too_deep(value: &SnbtValue, depth: usize) -> bool {
    if depth >= MAX_DEPTH {
        return true;
    }
    match value {
        SnbtValue::Compound(entries) => entries.iter().any(|(_, v)| too_deep(v, depth + 1)),
        SnbtValue::List(items) => items.iter().any(|v| too_deep(v, depth + 1)),
        _ => false,
    }
}

/// The elements of a list or typed array, as values.
fn elements_of(value: &SnbtValue) -> Option<Vec<SnbtValue>> {
    Some(match value {
        SnbtValue::List(items) => items.clone(),
        SnbtValue::ByteArray(items) => items.iter().map(|&v| SnbtValue::Byte(v)).collect(),
        SnbtValue::IntArray(items) => items.iter().map(|&v| SnbtValue::Int(v)).collect(),
        SnbtValue::LongArray(items) => items.iter().map(|&v| SnbtValue::Long(v)).collect(),
        _ => return None,
    })
}

fn resolve_index(index: i32, len: usize) -> Option<usize> {
    let actual = if index < 0 { len as i64 + i64::from(index) } else { i64::from(index) };
    (0..len as i64).contains(&actual).then_some(actual as usize)
}

fn compound_get<'a>(entries: &'a [(String, SnbtValue)], key: &str) -> Option<&'a SnbtValue> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

impl PathNode {
    /// What an unmatched step creates to continue into this node.
    fn preferred_parent(&self) -> SnbtValue {
        match self {
            Self::Key(_) | Self::KeyMatching(..) | Self::RootMatching(_) => SnbtValue::Compound(Vec::new()),
            Self::Index(_) | Self::AllElements | Self::ElementMatching(_) => SnbtValue::List(Vec::new()),
        }
    }

    fn select(&self, parent: &SnbtValue) -> Vec<SnbtValue> {
        match self {
            Self::Key(name) => match parent {
                SnbtValue::Compound(entries) => compound_get(entries, name).cloned().into_iter().collect(),
                _ => Vec::new(),
            },
            Self::KeyMatching(name, pattern) => match parent {
                SnbtValue::Compound(entries) => compound_get(entries, name)
                    .filter(|held| compare_nbt(pattern, held))
                    .cloned()
                    .into_iter()
                    .collect(),
                _ => Vec::new(),
            },
            Self::Index(index) => elements_of(parent)
                .and_then(|items| resolve_index(*index, items.len()).map(|i| items[i].clone()))
                .into_iter()
                .collect(),
            Self::AllElements => elements_of(parent).unwrap_or_default(),
            Self::ElementMatching(pattern) => match parent {
                SnbtValue::List(items) => items.iter().filter(|v| compare_nbt(pattern, v)).cloned().collect(),
                _ => Vec::new(),
            },
            Self::RootMatching(pattern) => match parent {
                SnbtValue::Compound(_) if compare_nbt(pattern, parent) => vec![parent.clone()],
                _ => Vec::new(),
            },
        }
    }

    /// The selected children of `parent` by reference, creating (from `make`)
    /// what is missing when `make` is given.
    fn select_mut<'a>(
        &self,
        parent: &'a mut SnbtValue,
        make: Option<&dyn Fn() -> SnbtValue>,
    ) -> Vec<&'a mut SnbtValue> {
        match (self, parent) {
            (Self::Key(name), SnbtValue::Compound(entries)) => {
                let at = match entries.iter().position(|(k, _)| k == name) {
                    Some(at) => at,
                    None => {
                        let Some(make) = make else { return Vec::new() };
                        entries.push((name.clone(), make()));
                        entries.len() - 1
                    }
                };
                vec![&mut entries[at].1]
            }
            (Self::KeyMatching(name, pattern), SnbtValue::Compound(entries)) => {
                let at = match entries.iter().position(|(k, _)| k == name) {
                    Some(at) => at,
                    None => {
                        if make.is_none() {
                            return Vec::new();
                        }
                        entries.push((name.clone(), pattern.clone()));
                        return vec![&mut entries.last_mut().expect("just pushed").1];
                    }
                };
                if compare_nbt(pattern, &entries[at].1) { vec![&mut entries[at].1] } else { Vec::new() }
            }
            (Self::Index(index), SnbtValue::List(items)) => match resolve_index(*index, items.len()) {
                Some(at) => vec![&mut items[at]],
                None => Vec::new(),
            },
            (Self::AllElements, SnbtValue::List(items)) => {
                if items.is_empty() {
                    let Some(make) = make else { return Vec::new() };
                    items.push(make());
                }
                items.iter_mut().collect()
            }
            (Self::ElementMatching(pattern), SnbtValue::List(items)) => {
                let any = items.iter().any(|v| compare_nbt(pattern, v));
                if !any {
                    if make.is_none() {
                        return Vec::new();
                    }
                    items.push(pattern.clone());
                    return vec![items.last_mut().expect("just pushed")];
                }
                items.iter_mut().filter(|v| compare_nbt(pattern, v)).collect()
            }
            (Self::RootMatching(pattern), parent @ SnbtValue::Compound(_)) => {
                if compare_nbt(pattern, parent) { vec![parent] } else { Vec::new() }
            }
            _ => Vec::new(),
        }
    }

    /// Writes `value` into `parent` at this node; 1 when something changed.
    fn set_in(&self, parent: &mut SnbtValue, value: &SnbtValue) -> usize {
        match (self, parent) {
            (Self::Key(name), SnbtValue::Compound(entries)) => match entries.iter_mut().find(|(k, _)| k == name) {
                Some((_, held)) if held == value => 0,
                Some((_, held)) => {
                    *held = value.clone();
                    1
                }
                None => {
                    entries.push((name.clone(), value.clone()));
                    1
                }
            },
            (Self::KeyMatching(name, pattern), SnbtValue::Compound(entries)) => {
                match entries.iter_mut().find(|(k, _)| k == name) {
                    Some((_, held)) if compare_nbt(pattern, held) && *held != *value => {
                        *held = value.clone();
                        1
                    }
                    _ => 0,
                }
            }
            (Self::Index(index), SnbtValue::List(items)) => match resolve_index(*index, items.len()) {
                Some(at) if items[at] != *value => {
                    items[at] = value.clone();
                    1
                }
                _ => 0,
            },
            (Self::AllElements, SnbtValue::List(items)) => {
                if items.is_empty() {
                    items.push(value.clone());
                    return 1;
                }
                let changed = items.iter().filter(|held| **held != *value).count();
                if changed == 0 {
                    return 0;
                }
                items.iter_mut().for_each(|held| *held = value.clone());
                changed
            }
            (Self::ElementMatching(pattern), SnbtValue::List(items)) => {
                if items.is_empty() {
                    items.push(value.clone());
                    return 1;
                }
                let mut changed = 0;
                for held in items.iter_mut() {
                    if compare_nbt(pattern, held) && *held != *value {
                        *held = value.clone();
                        changed += 1;
                    }
                }
                changed
            }
            _ => 0,
        }
    }

    /// Removes this node's selection from `parent`; how many were removed.
    fn remove_from(&self, parent: &mut SnbtValue) -> usize {
        match (self, parent) {
            (Self::Key(name), SnbtValue::Compound(entries)) => {
                let before = entries.len();
                entries.retain(|(k, _)| k != name);
                before - entries.len()
            }
            (Self::KeyMatching(name, pattern), SnbtValue::Compound(entries)) => {
                let before = entries.len();
                entries.retain(|(k, v)| !(k == name && compare_nbt(pattern, v)));
                before - entries.len()
            }
            (Self::Index(index), SnbtValue::List(items)) => match resolve_index(*index, items.len()) {
                Some(at) => {
                    items.remove(at);
                    1
                }
                None => 0,
            },
            (Self::AllElements, SnbtValue::List(items)) => std::mem::take(items).len(),
            (Self::ElementMatching(pattern), SnbtValue::List(items)) => {
                let before = items.len();
                items.retain(|v| !compare_nbt(pattern, v));
                before - items.len()
            }
            _ => 0,
        }
    }
}

fn invalid_node(position: usize) -> ParseError {
    ParseError::new(position, ParseErrorKind::InvalidArgument("Invalid NBT path element".to_owned()))
}

fn read_path(reader: &mut StringReader) -> Result<NbtPath, ParseError> {
    let start = reader.cursor();
    let mut nodes = Vec::new();
    let mut ends = Vec::new();
    let mut first = true;
    while reader.can_read() && reader.peek() != Some(' ') {
        nodes.push(read_node(reader, first)?);
        ends.push(reader.cursor() - start);
        first = false;
        if let Some(next) = reader.peek() {
            if !matches!(next, ' ' | '[' | '{') {
                if next != '.' {
                    return Err(ParseError::new(reader.cursor(), ParseErrorKind::ExpectedArgumentSeparator));
                }
                reader.skip();
            }
        }
    }
    if nodes.is_empty() {
        return Err(invalid_node(reader.cursor()));
    }
    let original: String = reader.source().chars().skip(start).take(reader.cursor() - start).collect();
    // The `.` consumed after the last node is part of the text but not of any node.
    let ends = ends.into_iter().map(|e| e.min(original.len())).collect();
    Ok(NbtPath { original, nodes, ends })
}

fn read_node(reader: &mut StringReader, first: bool) -> Result<PathNode, ParseError> {
    match reader.peek() {
        Some('"' | '\'') => {
            let name = reader.read_string()?;
            read_key_node(reader, name)
        }
        Some('[') => {
            reader.skip();
            match reader.peek() {
                Some('{') => {
                    let pattern = SnbtValue::Compound(read_compound_value(reader)?);
                    expect_close(reader)?;
                    Ok(PathNode::ElementMatching(pattern))
                }
                Some(']') => {
                    reader.skip();
                    Ok(PathNode::AllElements)
                }
                _ => {
                    let index = reader.read_int()?;
                    expect_close(reader)?;
                    Ok(PathNode::Index(index))
                }
            }
        }
        Some('{') => {
            if !first {
                return Err(invalid_node(reader.cursor()));
            }
            Ok(PathNode::RootMatching(SnbtValue::Compound(read_compound_value(reader)?)))
        }
        _ => {
            let start = reader.cursor();
            let mut name = String::new();
            while let Some(c) = reader.peek() {
                if matches!(c, ' ' | '"' | '\'' | '[' | ']' | '.' | '{' | '}') {
                    break;
                }
                name.push(c);
                reader.skip();
            }
            if name.is_empty() {
                return Err(invalid_node(start));
            }
            read_key_node(reader, name)
        }
    }
}

fn expect_close(reader: &mut StringReader) -> Result<(), ParseError> {
    if reader.peek() == Some(']') {
        reader.skip();
        Ok(())
    } else {
        Err(ParseError::new(reader.cursor(), ParseErrorKind::ExpectedArgumentSeparator))
    }
}

fn read_key_node(reader: &mut StringReader, name: String) -> Result<PathNode, ParseError> {
    if name.is_empty() {
        return Err(invalid_node(reader.cursor()));
    }
    if reader.peek() == Some('{') {
        let pattern = SnbtValue::Compound(read_compound_value(reader)?);
        return Ok(PathNode::KeyMatching(name, pattern));
    }
    Ok(PathNode::Key(name))
}

/// `minecraft:nbt_path`; see the module doc for the grammar.
#[derive(Debug, Clone, Copy, Default)]
pub struct NbtPathArg;

impl ArgumentType for NbtPathArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        match read_path(reader) {
            Ok(path) => Ok(ParsedValue::dynamic(path)),
            Err(e) => {
                reader.set_cursor(start);
                Err(e)
            }
        }
    }
}

impl McArg for NbtPathArg {
    type Value = NbtPath;

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
    use crate::snbt::parse_value;

    fn value(text: &str) -> SnbtValue {
        parse_value(text).expect("test snbt parses")
    }

    fn path(text: &str) -> NbtPath {
        NbtPath::parse(text).unwrap_or_else(|e| panic!("{text:?}: {e:?}"))
    }

    #[test]
    fn keys_indices_filters_and_quoted_keys_parse() {
        for ok in ["foo", "foo.bar.baz", "foo[0]", "[0]", "[]", "{a:1}", "foo{a:1}.bar", "foo[{a:1}].x", "\"a b\".c", "foo[-1]"] {
            assert_eq!(path(ok).as_str(), ok);
        }
    }

    #[test]
    fn malformed_paths_are_refused_and_rewind() {
        for bad in ["", "foo..bar", "foo.{a:1}", "foo[x]", "foo[0", "foo bar.baz"] {
            let mut reader = StringReader::new(bad);
            let parsed = NbtPathArg.parse(&mut reader);
            // `foo bar.baz` parses `foo` and leaves the rest to the next argument.
            if bad == "foo bar.baz" {
                assert!(parsed.is_ok());
                continue;
            }
            assert!(parsed.is_err(), "{bad:?} must be refused");
            assert_eq!(reader.cursor(), 0, "a failed parse rewinds");
        }
    }

    #[test]
    fn get_follows_keys_indices_and_filters() {
        let root = value("{Health: 20.0f, Pos: [1.0d, 64.0d, -3.5d], Items: [{id: \"a\", n: 1b}, {id: \"b\", n: 2b}]}");
        assert_eq!(path("Health").get(&root).unwrap(), [SnbtValue::Float(20.0)]);
        assert_eq!(path("Pos[2]").get(&root).unwrap(), [SnbtValue::Double(-3.5)]);
        assert_eq!(path("Pos[-3]").get(&root).unwrap(), [SnbtValue::Double(1.0)]);
        assert_eq!(path("Pos[]").get(&root).unwrap().len(), 3);
        assert_eq!(path("Items[{id:\"b\"}].n").get(&root).unwrap(), [SnbtValue::Byte(2)]);
        assert_eq!(path("Items[].id").get(&root).unwrap(), [SnbtValue::String("a".into()), SnbtValue::String("b".into())]);
        assert_eq!(path("{Health:20.0f}.Health").get(&root).unwrap(), [SnbtValue::Float(20.0)]);
        // a root filter that fails selects nothing
        assert_eq!(path("{Health:1.0f}").get(&root).unwrap_err(), "Found no elements matching {Health:1.0f}");
    }

    #[test]
    fn a_dead_end_names_the_prefix_that_matched_nothing() {
        let root = value("{a: {b: 1}}");
        assert_eq!(path("a.c.d").get(&root).unwrap_err(), "Found no elements matching a.c");
        assert_eq!(path("a.b[0]").get(&root).unwrap_err(), "Found no elements matching a.b[0]");
        assert_eq!(path("zz").get(&root).unwrap_err(), "Found no elements matching zz");
    }

    #[test]
    fn set_creates_parents_and_counts_changes() {
        let mut root = value("{a: 1}");
        assert_eq!(path("b.c").set(&mut root, &SnbtValue::Int(5)).unwrap(), 1);
        assert_eq!(root, value("{a: 1, b: {c: 5}}"));
        assert_eq!(path("b.c").set(&mut root, &SnbtValue::Int(5)).unwrap(), 0, "same value is no change");
        assert_eq!(path("a").set(&mut root, &SnbtValue::Int(2)).unwrap(), 1);
    }

    #[test]
    fn set_through_all_elements_counts_only_the_elements_that_differ() {
        let mut root = value("{l: [1, 2, 2]}");
        assert_eq!(path("l[]").set(&mut root, &SnbtValue::Int(2)).unwrap(), 1);
        assert_eq!(root, value("{l: [2, 2, 2]}"));
        let mut root = value("{l: [{k: 1}, {k: 1, z: 0}]}");
        assert_eq!(path("l[{k:1}].k").set(&mut root, &SnbtValue::Int(9)).unwrap(), 2);
    }

    #[test]
    fn insert_prepends_appends_and_refuses_a_bad_index() {
        let mut root = value("{l: [1, 2]}");
        assert_eq!(path("l").insert(-1, &mut root, &[SnbtValue::Int(3)]).unwrap(), 1);
        assert_eq!(path("l").insert(0, &mut root, &[SnbtValue::Int(0)]).unwrap(), 1);
        assert_eq!(root, value("{l: [0, 1, 2, 3]}"));
        assert_eq!(path("l").insert(9, &mut root, &[SnbtValue::Int(0)]).unwrap_err(), "Invalid list index: 9");
        assert_eq!(path("m").insert(0, &mut root, &[SnbtValue::Int(7)]).unwrap(), 1, "a missing list is created");
        assert!(path("l[0]").insert(0, &mut root, &[SnbtValue::Int(7)]).unwrap_err().starts_with("Expected a list"));
    }

    #[test]
    fn merge_and_remove_report_what_changed() {
        let mut root = value("{a: {x: 1}, l: [1, 2, 3]}");
        let SnbtValue::Compound(add) = value("{y: 2}") else { unreachable!() };
        assert_eq!(path("a").merge(&mut root, &add).unwrap(), 1);
        assert_eq!(path("a").merge(&mut root, &add).unwrap(), 0);
        assert_eq!(path("l[0]").remove(&mut root), 1);
        assert_eq!(path("l[]").remove(&mut root), 2);
        assert_eq!(path("nope").remove(&mut root), 0);
        assert_eq!(root, value("{a: {x: 1, y: 2}, l: []}"));
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
