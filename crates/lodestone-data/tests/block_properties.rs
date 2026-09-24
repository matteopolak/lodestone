//! Generated typed block-property enums and the private Properties value.
//!
//! Ordinary tests exhaust every committed block-state id. The ignored drift
//! gate reads the authoritative blocks.json report and regenerates enum source
//! through a temporary file followed by an atomic rename.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use lodestone_data::block_properties::{
    BuiltinPropertyValue, ExtensionId, Properties, PropertyKey, PropertyValue, MAX_PROPERTIES,
};
use lodestone_data::block::Block;
use lodestone_data::block_states::{self, StateId};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn report_path() -> PathBuf {
    manifest_dir().join("../../.cache/mc/26.2/generated/reports/blocks.json")
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/block_property_tables.rs")
}

fn pascal_name(text: &str, prefix: &str) -> String {
    let mut output = String::new();
    if text.bytes().next().is_some_and(|byte| byte.is_ascii_digit()) {
        output.push_str(prefix);
    }
    for part in text.split(|character: char| !character.is_ascii_alphanumeric()) {
        if part.is_empty() {
            continue;
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            output.extend(first.to_uppercase());
            output.push_str(chars.as_str());
        }
    }
    if output.is_empty() {
        format!("{prefix}Value")
    } else {
        output
    }
}

fn unique_variants(
    names: impl IntoIterator<Item = String>,
    prefix: &str,
) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    names
        .into_iter()
        .map(|name| {
            let base = pascal_name(&name, prefix);
            let mut variant = base.clone();
            let mut suffix = 2;
            while !seen.insert(variant.clone()) {
                variant = format!("{base}{suffix}");
                suffix += 1;
            }
            (name, variant)
        })
        .collect()
}

type StateRow = (usize, u16, Vec<(String, String)>);

fn collect_report(
    doc: &serde_json::Value,
) -> (
    Vec<String>,
    Vec<String>,
    Vec<(String, String)>,
    Vec<Vec<(String, String)>>,
    Vec<(u16, u16)>,
    Vec<(u32, u32)>,
    usize,
    usize,
) {
    let object = doc.as_object().expect("blocks.json is an object");
    let mut keys = BTreeSet::new();
    let mut values = BTreeSet::new();
    let mut pairs = BTreeSet::new();
    let mut rows: Vec<StateRow> = Vec::new();
    let mut property_sets = BTreeSet::new();
    let mut max_properties = 0;
    for (block_name, block) in object {
        let block_id = Block::from_name(block_name)
            .unwrap_or_else(|| panic!("state report block {block_name:?} is not generated"));
        for state in block["states"].as_array().expect("states is an array") {
            let properties = state
                .get("properties")
                .and_then(serde_json::Value::as_object);
            let count = properties.map_or(0, serde_json::Map::len);
            max_properties = max_properties.max(count);
            let mut state_properties = Vec::with_capacity(count);
            if let Some(properties) = properties {
                for (key, value) in properties {
                    let value = value.as_str().expect("property values are strings");
                    keys.insert(key.clone());
                    values.insert(value.to_owned());
                    pairs.insert((key.clone(), value.to_owned()));
                    state_properties.push((key.clone(), value.to_owned()));
                }
            }
            state_properties.sort_unstable();
            property_sets.insert(state_properties.clone());
            let state_id = state["id"].as_u64().expect("state id is an integer") as usize;
            rows.push((state_id, block_id.registry_id(), state_properties));
        }
    }
    rows.sort_unstable_by_key(|row| row.0);
    let states = rows.len();
    assert_eq!(
        rows.iter().map(|row| row.0).collect::<Vec<_>>(),
        (0..states).collect::<Vec<_>>(),
        "state ids must be dense"
    );
    let property_sets: Vec<Vec<(String, String)>> = property_sets.into_iter().collect();
    let set_ids: BTreeMap<_, _> = property_sets
        .iter()
        .enumerate()
        .map(|(id, properties)| (properties.clone(), id))
        .collect();
    let mut state_sets = vec![None; states];
    let mut first = vec![u32::MAX; Block::COUNT as usize];
    let mut last = vec![0u32; Block::COUNT as usize];
    let mut counts = vec![0u32; Block::COUNT as usize];
    for (state_id, block_id, properties) in rows {
        let set_id = set_ids[&properties];
        state_sets[state_id] = Some((set_id as u16, block_id));
        let block_index = block_id as usize;
        first[block_index] = first[block_index].min(state_id as u32);
        last[block_index] = last[block_index].max(state_id as u32);
        counts[block_index] += 1;
    }
    let state_sets: Vec<(u16, u16)> = state_sets
        .into_iter()
        .map(|row| row.expect("every state id has a generated row"))
        .collect();
    let spans: Vec<(u32, u32)> = first
        .into_iter()
        .zip(last)
        .zip(counts)
        .map(|((first, last), count)| {
            assert_ne!(first, u32::MAX, "every generated block has a state span");
            assert_eq!(
                last - first + 1,
                count,
                "generated state ids for a block must form one contiguous span"
            );
            (first, last)
        })
        .collect();
    (
        keys.into_iter().collect(),
        values.into_iter().collect(),
        pairs.into_iter().collect(),
        property_sets,
        state_sets,
        spans,
        states,
        max_properties,
    )
}

fn generate(doc: &serde_json::Value) -> String {
    let (
        keys,
        values,
        pairs,
        property_sets,
        state_sets,
        spans,
        states,
        max_properties,
    ) = collect_report(doc);
    assert_eq!(states, block_states::STATE_COUNT as usize);
    assert!(keys.len() <= u8::MAX as usize, "property key ids must fit u8");
    assert!(values.len() <= u8::MAX as usize, "property value ids must fit u8");
    assert!(pairs.len() <= u16::MAX as usize, "generated pair table must fit u16");
    assert!(
        property_sets.len() <= u16::MAX as usize,
        "generated property-set ids must fit u16"
    );
    assert_eq!(
        max_properties, 7,
        "the generated census must exercise the seven-entry capacity"
    );

    let key_variants = unique_variants(keys.clone(), "Key");
    let value_variants = unique_variants(values.clone(), "Value");
    let key_ids: std::collections::BTreeMap<_, _> = keys
        .iter()
        .enumerate()
        .map(|(id, name)| (name.as_str(), id))
        .collect();
    let value_ids: std::collections::BTreeMap<_, _> = values
        .iter()
        .enumerate()
        .map(|(id, name)| (name.as_str(), id))
        .collect();
    let numeric_pairs: Vec<_> = pairs
        .iter()
        .map(|(key, value)| (key_ids[key.as_str()], value_ids[value.as_str()]))
        .collect();
    assert!(
        numeric_pairs.windows(2).all(|window| window[0] < window[1]),
        "valid pair ids must be strictly sorted for binary search"
    );
    for properties in &property_sets {
        let numeric_properties: Vec<_> = properties
            .iter()
            .map(|(key, value)| (key_ids[key.as_str()], value_ids[value.as_str()]))
            .collect();
        assert!(
            numeric_properties.windows(2).all(|window| window[0].0 < window[1].0),
            "property-set keys must be strictly sorted"
        );
    }

    let mut output = String::new();
    output.push_str(
        "// @generated by cargo test block_properties -- --ignored\n\
         // from .cache/mc/26.2/generated/reports/blocks.json. DO NOT EDIT BY HAND.\n\
         // Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n\
         //! Generated typed block-state property domains for protocol 776.\n\
         //! Built-in keys and values are enums; text is retained only in\n\
         //! match arms used at parse/display boundaries.\n\n",
    );
    let _ = writeln!(output, "pub const MAX_PROPERTIES: usize = {max_properties};\n");
    output.push_str(
        "/// A generated built-in block-state property key.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]\n\
         #[repr(u8)]\n\
         pub enum PropertyKey {\n",
    );
    for (id, (name, variant)) in key_variants.iter().enumerate() {
        let _ = writeln!(output, "    /// {name}\n    {variant} = {id},");
    }
    output.push_str("}\n\n");
    let _ = writeln!(
        output,
        "pub const PROPERTY_KEY_COUNT: usize = {};\n",
        keys.len()
    );
    output.push_str("pub const PROPERTY_KEYS: [PropertyKey; PROPERTY_KEY_COUNT] = [\n");
    for (_, variant) in &key_variants {
        let _ = writeln!(output, "    PropertyKey::{variant},");
    }
    output.push_str("];\n\n");
    output.push_str(
        "impl PropertyKey {\n\
         #[must_use]\n\
         pub const fn name(self) -> &'static str {\n\
         match self {\n",
    );
    for (name, variant) in &key_variants {
        let _ = writeln!(output, "    Self::{variant} => {name:?},");
    }
    output.push_str(
        " }\n }\n\n\
         #[must_use]\n\
         pub fn from_name(name: &str) -> Option<Self> {\n\
         match name {\n",
    );
    for (name, variant) in &key_variants {
        let _ = writeln!(output, "    {name:?} => Some(Self::{variant}),");
    }
    output.push_str(
        "    _ => None,\n }\n }\n\n\
         #[must_use]\n\
         pub const fn from_id(id: u8) -> Option<Self> {\n\
         match id {\n",
    );
    for (id, (_, variant)) in key_variants.iter().enumerate() {
        let _ = writeln!(output, "    {id} => Some(Self::{variant}),");
    }
    output.push_str(
        "    _ => None,\n }\n }\n\n\
         pub fn all() -> impl ExactSizeIterator<Item = Self> + Clone {\n\
         PROPERTY_KEYS.iter().copied()\n\
         }\n\
         }\n\n",
    );

    output.push_str(
        "/// A generated built-in block-state property value.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]\n\
         #[repr(u8)]\n\
         pub enum BuiltinPropertyValue {\n",
    );
    for (name, variant) in &value_variants {
        let _ = writeln!(output, "    /// {name}\n    {variant},");
    }
    output.push_str("}\n\n");
    let _ = writeln!(
        output,
        "pub const PROPERTY_VALUE_COUNT: usize = {};\n",
        values.len()
    );
    output.push_str(
        "pub const PROPERTY_VALUES: [BuiltinPropertyValue; PROPERTY_VALUE_COUNT] = [\n",
    );
    for (_, variant) in &value_variants {
        let _ = writeln!(output, "    BuiltinPropertyValue::{variant},");
    }
    output.push_str("];\n\n");
    output.push_str(
        "impl BuiltinPropertyValue {\n\
         #[must_use]\n\
         pub fn name(self) -> &'static str {\n\
         match self {\n",
    );
    for (name, variant) in &value_variants {
        let _ = writeln!(output, "    Self::{variant} => {name:?},");
    }
    output.push_str(" }\n }\n\n");
    output.push_str(
        " #[must_use]\n\
          pub fn from_name(name: &str) -> Option<Self> {\n\
          match name {\n",
    );
    for (name, variant) in &value_variants {
        let _ = writeln!(output, "    {name:?} => Some(Self::{variant}),");
    }
    output.push_str("    _ => None,\n }\n }\n\n");
    output.push_str(
        " #[must_use]\n\
          pub const fn from_id(id: u8) -> Option<Self> {\n\
          match id {\n",
    );
    for (id, (_, variant)) in value_variants.iter().enumerate() {
        let _ = writeln!(output, "    {id} => Some(Self::{variant}),");
    }
    output.push_str("    _ => None,\n }\n }\n\n");
    output.push_str(
        " pub fn all() -> impl ExactSizeIterator<Item = Self> + Clone {\n\
          PROPERTY_VALUES.iter().copied()\n\
          }\n\
          }\n\n",
    );

    output.push_str("pub static VALID_PAIRS: [(u8, u8); ");
    let _ = writeln!(output, "{}] = [", pairs.len());
    for (key, value) in &pairs {
        let _ = writeln!(
            output,
            "    ({}, {}),",
            key_ids[key.as_str()],
            value_ids[value.as_str()]
        );
    }
    output.push_str(
        "];\n\n\
         pub(super) fn is_valid_pair(key: PropertyKey, value: BuiltinPropertyValue) -> bool {\n\
         VALID_PAIRS.binary_search(&(key as u8, value as u8)).is_ok()\n\
         }\n\n",
    );
    output.push_str("pub static PROPERTY_SETS: [&[(u8, u8)]; ");
    let _ = writeln!(output, "{}] = [", property_sets.len());
    for properties in &property_sets {
        output.push_str("    &[");
        for (index, (key, value)) in properties.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            let _ = write!(
                output,
                "({}, {})",
                key_ids[key.as_str()],
                value_ids[value.as_str()]
            );
        }
        output.push_str("],\n");
    }
    output.push_str("];\n\n");
    output.push_str("pub static STATE_PROPERTY_SET_IDS: [u16; ");
    let _ = writeln!(output, "{}] = [", state_sets.len());
    for chunk in state_sets.chunks(24) {
        output.push_str("    ");
        for (index, &(set_id, _)) in chunk.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            let _ = write!(output, "{set_id}");
        }
        output.push(',');
        output.push('\n');
    }
    output.push_str("];\n\n");
    output.push_str("pub static BLOCK_STATE_SPANS: [(u32, u32); ");
    let _ = writeln!(output, "{}] = [", spans.len());
    for chunk in spans.chunks(8) {
        output.push_str("    ");
        for (index, &(first, last)) in chunk.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            let _ = write!(output, "({first}, {last})");
        }
        output.push(',');
        output.push('\n');
    }
    output.push_str(
        "];\n\n\
         pub(super) fn property_set_for_state(raw: u32) -> &'static [(u8, u8)] {\n\
         &PROPERTY_SETS[STATE_PROPERTY_SET_IDS[raw as usize] as usize]\n\
         }\n\n\
         pub(super) fn block_state_span(block_id: u16) -> Option<(u32, u32)> {\n\
         BLOCK_STATE_SPANS.get(block_id as usize).copied()\n\
         }\n",
    );
    output
}

struct InstallLock {
    path: PathBuf,
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct TemporaryInstall {
    path: PathBuf,
    installed: bool,
}

impl Drop for TemporaryInstall {
    fn drop(&mut self) {
        if !self.installed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn install_atomically(path: &Path, contents: &str) {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("generated destination has a file name");
    let lock_path = path.with_file_name(format!(".{file_name}.lock"));
    let _lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .unwrap_or_else(|error| panic!("lock {}: {error}", path.display()));
    let _lock_guard = InstallLock {
        path: lock_path.clone(),
    };
    let mut temporary = TemporaryInstall {
        path: path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id())),
        installed: false,
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary.path)
        .unwrap_or_else(|error| panic!("create {}: {error}", temporary.path.display()));
    file.write_all(contents.as_bytes())
        .unwrap_or_else(|error| panic!("write {}: {error}", temporary.path.display()));
    file.sync_all()
        .unwrap_or_else(|error| panic!("sync {}: {error}", temporary.path.display()));
    drop(file);
    fs::rename(&temporary.path, path).unwrap_or_else(|error| {
        panic!("install {}: {error}", path.display());
    });
    temporary.installed = true;
}

#[test]
fn generated_keys_and_values_round_trip_without_text_storage() {
    for key in PropertyKey::all() {
        assert_eq!(PropertyKey::from_name(key.name()), Some(key));
    }
    for value in BuiltinPropertyValue::all() {
        assert_eq!(
            PropertyValue::from_name(value.name()),
            Some(PropertyValue::builtin(value))
        );
    }
    assert_eq!(std::mem::size_of::<PropertyKey>(), 1);
    assert_eq!(std::mem::size_of::<BuiltinPropertyValue>(), 1);
    assert_eq!(std::mem::size_of::<PropertyValue>(), 4);
    assert_eq!(std::mem::size_of::<Properties>(), 15);
    assert_eq!(ExtensionId::from_index(41).index(), 41);
    let extension = PropertyValue::extension(ExtensionId::from_index(41));
    assert!(extension.is_extension());
    assert_eq!(extension.extension_id(), Some(ExtensionId::from_index(41)));
    assert_eq!(PropertyValue::builtin(BuiltinPropertyValue::False).builtin_value(),
        Some(BuiltinPropertyValue::False));
}

#[test]
fn every_generated_state_round_trips_through_typed_properties() {
    for raw in 0..block_states::STATE_COUNT {
        let state = StateId::new(raw).expect("generated state id is valid");
        let properties = Properties::from_state_id(state);
        let reparsed = Properties::parse(&properties.to_string()).expect("typed properties parse");
        assert_eq!(reparsed, properties, "state {raw} changed during typed round trip");
        assert_eq!(
            Properties::state_for_block(state.block(), &properties),
            Some(state),
            "state {raw} changed during typed block-schema round trip"
        );
    }
}

#[test]
fn parser_rejects_unknown_or_ambiguous_input() {
    assert!(Properties::parse("facing=north").is_err());
    assert!(Properties::parse("[not_a_key=north]").is_err());
    assert!(Properties::parse("[facing=not_a_value]").is_err());
    assert!(Properties::parse("[facing=north,facing=south]").is_err());
    assert!(Properties::parse("[facing]").is_err());
    let too_many = (0..=MAX_PROPERTIES)
        .map(|_| "age=0")
        .collect::<Vec<_>>()
        .join(",");
    assert!(Properties::parse(&format!("[{}]", too_many)).is_err());
}

#[test]
fn properties_validate_domains_and_duplicates() {
    let key = PropertyKey::from_name("facing").expect("generated key");
    let north = PropertyValue::from_name("north").expect("generated value");
    let south = PropertyValue::from_name("south").expect("generated value");
    let properties = Properties::try_from_pairs(&[(key, south), (key, north)]);
    assert!(properties.is_err(), "duplicate keys must be rejected");
    assert_eq!(Properties::from_state_id(StateId::new(9).unwrap()).len(), 1);
    let snowy = Properties::try_from_pairs(&[(
        PropertyKey::from_name("snowy").expect("generated key"),
        PropertyValue::from_name("true").expect("generated value"),
    )])
    .expect("snowy=true is a valid generated pair");
    assert_eq!(
        Properties::state_for_block(Block::GrassBlock, &snowy),
        Some(StateId::new(8).expect("generated state id"))
    );
    assert_eq!(
        Properties::state_for_block(Block::Stone, &snowy),
        None,
        "a pair valid for grass blocks must not resolve as a stone state"
    );
}

#[test]
#[ignore = "regenerates/verifies the typed property enums; run explicitly"]
fn committed_generated_properties_match_report() {
    let raw = fs::read_to_string(report_path()).expect("blocks.json is available");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("blocks.json parses");
    let generated = generate(&doc);
    if std::env::var_os("LODESTONE_REGEN").is_some() {
        let destination = std::env::var_os("LODESTONE_TYPED_PROPERTIES_OUTPUT")
            .map(PathBuf::from)
            .unwrap_or_else(committed_path);
        install_atomically(&destination, &generated);
        eprintln!("regenerated {}", destination.display());
        return;
    }
    let committed = fs::read_to_string(committed_path()).expect("generated properties exist");
    assert_eq!(generated, committed, "generated typed properties are stale");
}
