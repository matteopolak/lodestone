//! The `minecraft:tool` census: hermetic checks over the committed tables, plus
//! an `#[ignore]`d drift guard that regenerates them from the committed JVM dump
//! and asserts byte-for-byte equality (modelled on `hardness.rs`). The generator
//! lives here so the checked-in tables can never silently drift from game data.
//!
//! # Data provenance
//!
//! `tests/support/tool_jvm.txt` is an authoritative dump produced by booting the
//! real 26.2 server (`oracle-java/ToolOracle.java`), binding the vanilla data
//! pack's tags, running the item component initializers, and then reading:
//!
//! * vanilla's own block registry's registration order (`B` lines),
//! * every bound `minecraft:block` tag's membership (`T` lines),
//! * every item's prototype `minecraft:tool` component (`I`/`R` lines).
//!
//! None of the three can be read off the wire: see `src/tool.rs`'s module docs.
//! The dump is committed as the external anchor (§ "an expected value must
//! originate outside the code under test").
//!
//! The item half of the dump is **independently corroborated** by Mojang's own
//! `generated/reports/minecraft/components/item/*.json` (the
//! `RegistryComponentsReport` output shipped with the version), which agrees
//! with it item-for-item, rule-for-rule and bit-for-bit — see
//! [`dump_agrees_with_mojangs_own_components_report`], which re-checks that from
//! `.cache` when it is present.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump from the server:
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/versions/26.2/oracle-java && pwd)"
//! docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
//!   CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
//!   cp /oracle/ToolOracle.java /work/ && javac -cp "$CP" -d /work /work/ToolOracle.java
//!   java -cp "/work:$CP" ToolOracle 2>/dev/null'
//! ```
//!
//!    then copy its stdout over `tests/support/tool_jvm.txt` (keeping the `#`
//!    header).
//!
//! 2. Regenerate the committed tables:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test tools \
//!     committed_tables_match_dump -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::{
    block::Block,
    block_states::{self, StateId},
    hardness,
    item::Item,
    tool,
};
use lodestone_model::{ItemStack, ItemTool, ToolBlocks, ToolMining, ToolPatch, ToolRule};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn block_registry_path() -> PathBuf {
    manifest_dir().join("src/generated/block_registry.rs")
}

fn tools_path() -> PathBuf {
    manifest_dir().join("src/generated/tools.rs")
}

fn block_enum_path() -> PathBuf {
    manifest_dir().join("src/generated/block_enum.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/tool_jvm.txt");

// ---------------------------------------------------------------------------
// Dump parsing
// ---------------------------------------------------------------------------

/// A tool rule's block set, exactly as the dump spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DumpBlocks {
    /// `#minecraft:mineable/pickaxe`
    Tag(String),
    /// `=minecraft:cobweb,minecraft:…`
    Blocks(Vec<String>),
}

#[derive(Debug, Clone)]
struct DumpRule {
    blocks: DumpBlocks,
    speed_bits: Option<u32>,
    correct_for_drops: Option<bool>,
}

#[derive(Debug, Clone)]
struct DumpTool {
    item: String,
    default_speed_bits: u32,
    damage_per_block: u32,
    can_destroy_blocks_in_creative: bool,
    rules: Vec<DumpRule>,
}

#[derive(Debug, Default)]
struct Dump {
    /// Block registry id → canonical name, dense over `0..len`.
    blocks: Vec<String>,
    /// Tag name → member block names.
    tags: BTreeMap<String, Vec<String>>,
    /// Items carrying a prototype tool component, sorted by item name.
    tools: Vec<DumpTool>,
}

fn parse_optional_bits(token: &str) -> Option<u32> {
    (token != "-").then(|| u32::from_str_radix(token, 16).expect("speed bits are hex"))
}

fn parse_dump(text: &str) -> Dump {
    let mut dump = Dump::default();
    let mut pending_rules: usize = 0;
    let mut blocks_by_id: BTreeMap<usize, String> = BTreeMap::new();

    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split(' ');
        match tok.next().expect("record kind") {
            "B" => {
                let id: usize = tok.next().expect("block id").parse().expect("block id");
                let name = tok.next().expect("block name").to_owned();
                assert!(tok.next().is_none(), "trailing tokens on {line:?}");
                assert!(
                    blocks_by_id.insert(id, name).is_none(),
                    "duplicate block registry id {id}"
                );
            }
            "T" => {
                let name = tok.next().expect("tag name").to_owned();
                let members: Vec<String> = tok.map(str::to_owned).collect();
                assert!(
                    dump.tags.insert(name, members).is_none(),
                    "duplicate block tag on {line:?}"
                );
            }
            "I" => {
                assert_eq!(pending_rules, 0, "previous item is missing rule lines");
                let item = tok.next().expect("item name").to_owned();
                let default_speed_bits =
                    u32::from_str_radix(tok.next().expect("default speed"), 16)
                        .expect("default speed bits are hex");
                let damage_per_block: u32 =
                    tok.next().expect("damage per block").parse().expect("u32");
                let creative: u8 = tok.next().expect("creative flag").parse().expect("0 or 1");
                let rule_count: usize = tok.next().expect("rule count").parse().expect("usize");
                assert!(tok.next().is_none(), "trailing tokens on {line:?}");
                assert!(creative <= 1, "creative flag must be 0 or 1 on {line:?}");
                pending_rules = rule_count;
                dump.tools.push(DumpTool {
                    item,
                    default_speed_bits,
                    damage_per_block,
                    can_destroy_blocks_in_creative: creative == 1,
                    rules: Vec::with_capacity(rule_count),
                });
            }
            "R" => {
                assert!(pending_rules > 0, "rule line without a preceding item");
                pending_rules -= 1;
                let spec = tok.next().expect("blocks spec");
                let blocks = match spec.split_at(1) {
                    ("#", tag) => DumpBlocks::Tag(tag.to_owned()),
                    ("=", list) => {
                        DumpBlocks::Blocks(list.split(',').map(str::to_owned).collect())
                    }
                    _ => panic!("unrecognised blocks spec {spec:?}"),
                };
                let speed_bits = parse_optional_bits(tok.next().expect("speed column"));
                let correct_for_drops = match tok.next().expect("correct column") {
                    "-" => None,
                    "1" => Some(true),
                    "0" => Some(false),
                    other => panic!("correct_for_drops must be 1/0/-, got {other:?}"),
                };
                assert!(tok.next().is_none(), "trailing tokens on {line:?}");
                dump.tools
                    .last_mut()
                    .expect("rule belongs to an item")
                    .rules
                    .push(DumpRule {
                        blocks,
                        speed_bits,
                        correct_for_drops,
                    });
            }
            other => panic!("unrecognised dump record {other:?}"),
        }
    }
    assert_eq!(pending_rules, 0, "dump ended mid-item");

    for (index, (&id, name)) in blocks_by_id.iter().enumerate() {
        assert_eq!(id, index, "block registry ids are not a dense 0..N");
        dump.blocks.push(name.clone());
    }
    dump
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// Resolves a block name to its registry id, failing loudly on an unknown name
/// rather than silently dropping it from a tag.
fn block_id(dump: &Dump, name: &str) -> u16 {
    let index = dump
        .blocks
        .iter()
        .position(|candidate| candidate == name)
        .unwrap_or_else(|| panic!("block {name} is not in the dump's registry listing"));
    u16::try_from(index).expect("block registry id fits u16")
}

/// Sorted, de-duplicated registry ids for a set of block names.
fn block_id_set(dump: &Dump, names: &[String]) -> Vec<u16> {
    let mut ids: Vec<u16> = names.iter().map(|name| block_id(dump, name)).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Resolves tool dump names through the canonical item registry and orders the
/// generated table by the resulting wire ids. Each input may spell an item
/// differently, so uniqueness is checked *after* resolution rather than by
/// comparing source strings.
fn item_tool_entries(dump: &Dump) -> Vec<(u16, &DumpTool)> {
    let mut entries: Vec<(u16, &DumpTool)> = dump
        .tools
        .iter()
        .map(|tool| {
            let item = Item::from_name(&tool.item).unwrap_or_else(|| {
                panic!(
                    "tool item {:?} is not in the built-in registry",
                    tool.item
                )
            });
            (item.registry_id(), tool)
        })
        .collect();
    entries.sort_by_key(|&(id, _)| id);
    for pair in entries.windows(2) {
        assert!(
            pair[0].0 < pair[1].0,
            "duplicate resolved item registry id {}",
            pair[0].0
        );
    }
    entries
}

/// A `f32` literal that round-trips to exactly `bits`.
fn float_literal(bits: u32) -> String {
    let value = f32::from_bits(bits);
    assert_eq!(
        value.to_bits(),
        bits,
        "float literal {value:?} does not round-trip"
    );
    format!("{value:?}")
}

/// Renders `src/generated/block_registry.rs`: the registry-id ↔ block-state-id
/// reconciliation the wire needs.
fn generate_block_registry(dump: &Dump) -> String {
    let block_count = dump.blocks.len();
    let state_count = block_states::STATE_COUNT as usize;

    let mut state_block: Vec<u16> = Vec::with_capacity(state_count);
    for state in 0..block_states::STATE_COUNT {
        let name = block_states::block_name(state)
            .unwrap_or_else(|| panic!("block-state {state} has no name in the committed table"));
        state_block.push(block_id(dump, name));
    }

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test tools -- --ignored`\n\
         // from tests/support/tool_jvm.txt (the captured block registration order from\n\
         // the headless 26.2 dump, protocol 776 / Minecraft 26.2)\n\
         // joined with the committed block-state table. DO NOT EDIT BY HAND. Regenerate\n\
         // with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated `minecraft:block` registry tables for protocol 776 (Minecraft\n\
         //! 26.2).\n\
         //!\n\
         //! The wire uses **two** block id spaces and they are not the same order: a\n\
         //! chunk palette or `block_update` carries a *block-state* id, while a\n\
         //! `Holder<Block>` (a `block_event` target, a `minecraft:tool` rule's explicit\n\
         //! block set) carries a *block registry* id in registration order — `air` is 0.\n\
         //! [`crate::generated_block_states`]'s block table is sorted by name instead, so\n\
         //! the two must be reconciled explicitly rather than assumed equal.\n\
         //!\n\
         //! Consumed by [`crate::block_states`] and [`crate::tool`].\n\n",
    );

    let _ = writeln!(
        out,
        "/// Number of blocks in the `minecraft:block` registry (ids are `0..BLOCK_COUNT`)."
    );
    let _ = writeln!(out, "pub const BLOCK_COUNT: u32 = {block_count};\n");

    let _ = writeln!(
        out,
        "/// Canonical block identifier, indexed by `minecraft:block` **registry** id\n\
         /// (registration order)."
    );
    let _ = writeln!(
        out,
        "pub static BLOCK_REGISTRY_NAMES: [&str; {block_count}] = ["
    );
    for (id, name) in dump.blocks.iter().enumerate() {
        let _ = writeln!(out, "    {name:?}, // {id}");
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// The `minecraft:block` registry id owning each block **state**, indexed by\n\
         /// global block-state id."
    );
    let _ = writeln!(out, "pub static STATE_BLOCK: [u16; {state_count}] = [");
    for chunk in state_block.chunks(16) {
        out.push_str("    ");
        for id in chunk {
            let _ = write!(out, "{id}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n");
    out
}

/// The Rust enum variant name for a registry path: `polished_granite` →
/// `PolishedGranite`.
///
/// Callers must have already checked the path against
/// [`assert_path_is_variant_safe`]; this function only transforms.
fn variant_name(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for word in path.split('_') {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// Fails the generator — loudly, naming the offender — if any block name cannot
/// become a distinct, legal Rust variant.
///
/// Every one of these is silent-corruption-shaped rather than merely
/// inconvenient. A path outside `[a-z0-9_]` or one starting with a digit yields
/// a variant that does not compile, which is the harmless case. The dangerous
/// one is a **collision**: two registry entries whose paths camel-case to the
/// same identifier would produce a duplicate-variant compile error today, but a
/// generator that "helpfully" de-duplicated them would silently alias two
/// blocks. Measured against the committed 26.2 dump: 1,196 paths, 1,196
/// distinct variants, all `minecraft:`, none digit-leading, none outside
/// `[a-z0-9_]`. Asserting it here is what keeps that true after a version bump
/// rather than a fact that happened to hold once.
fn assert_path_is_variant_safe(names: &[String]) -> Vec<(String, String)> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut pairs = Vec::with_capacity(names.len());
    for name in names {
        let (namespace, path) = name
            .split_once(':')
            .unwrap_or_else(|| panic!("registry name {name:?} has no namespace"));
        assert_eq!(
            namespace, "minecraft",
            "the generated enum covers the built-in registry only; {name:?} is not `minecraft:`"
        );
        assert!(
            !path.is_empty()
                && path
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "block path {path:?} is outside [a-z0-9_] and has no obvious variant spelling"
        );
        assert!(
            !path.as_bytes()[0].is_ascii_digit(),
            "block path {path:?} starts with a digit, which is not a legal Rust variant"
        );
        let variant = variant_name(path);
        assert_ne!(
            variant, "Self",
            "block path {path:?} camel-cases to the reserved identifier `Self`"
        );
        if let Some(previous) = seen.insert(variant.clone(), path.to_owned()) {
            panic!(
                "block paths {previous:?} and {path:?} both camel-case to `{variant}`; the \
                 generator must be taught a disambiguation rather than silently aliasing them"
            );
        }
        pairs.push((variant, name.clone()));
    }
    pairs
}

/// Renders `src/generated/block_enum.rs`: the `Block` enum whose discriminant
/// **is** the `minecraft:block` registry id, plus the two index tables that make
/// id → enum and name → enum branch-free lookups.
fn generate_block_enum(dump: &Dump) -> String {
    let block_count = dump.blocks.len();
    let variants = assert_path_is_variant_safe(&dump.blocks);

    // The default block state of each block, joined in from the committed
    // snow-support census captured from the real 26.2 server. Exactly one state
    // per block carries the mark; the assertion below stops a silently-missing
    // or duplicated default from becoming a wrong `Block::default_state`.
    let mut default_state: Vec<Option<u32>> = vec![None; block_count];
    for state in 0..block_states::STATE_COUNT {
        let typed = lodestone_data::block_states::StateId::new(state)
            .expect("generated state-table index is valid");
        if typed.is_default() {
            let name = block_states::block_name(state)
                .unwrap_or_else(|| panic!("block-state {state} has no name"));
            let block = block_id(dump, name) as usize;
            assert!(
                default_state[block].is_none(),
                "block {name} has more than one state marked default ({:?} and {state})",
                default_state[block]
            );
            default_state[block] = Some(state);
        }
    }
    let default_state: Vec<u32> = default_state
        .into_iter()
        .enumerate()
        .map(|(block, state)| {
            state.unwrap_or_else(|| {
                panic!(
                    "block {} has no default state in the committed census",
                    dump.blocks[block]
                )
            })
        })
        .collect();

    let mut by_name: Vec<u16> = (0..block_count as u16).collect();
    by_name.sort_unstable_by(|&a, &b| dump.blocks[a as usize].cmp(&dump.blocks[b as usize]));

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test tools -- --ignored`\n\
         // from tests/support/tool_jvm.txt (the captured block registration order from\n\
         // the headless 26.2 dump, protocol 776 / Minecraft 26.2)\n\
         // joined with the committed block-state and default-state censuses. DO NOT EDIT\n\
         // BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! The generated `minecraft:block` registry as a Rust enum.\n\
         //!\n\
         //! One variant per built-in block, in **registration** order, with the\n\
         //! discriminant written out explicitly so that `block as u16` *is* the registry\n\
         //! id a `Holder<Block>` carries on the wire — no lookup, no branch, no table.\n\
         //! That identity is the whole point of the representation: every per-block\n\
         //! census in this crate is a plain array indexed by it.\n\
         //!\n\
         //! A block **state** is a different and much larger space (32,366 in 26.2) and is\n\
         //! deliberately *not* an enum — see [`crate::block_states::StateId`].\n\
         //!\n\
         //! The accessors live in [`crate::block`]; this file is data only.\n\n",
    );

    let _ = writeln!(
        out,
        "/// A built-in block of Minecraft 26.2, one variant per `minecraft:block`\n\
         /// registry entry.\n\
         ///\n\
         /// `Block as u16` is the registry id. Ordering is **registration** order, not\n\
         /// alphabetical.\n\
         ///\n\
         /// This enum is intentionally *not* `#[non_exhaustive]` and carries no `Custom`\n\
         /// variant: a match over it is exhaustive, so a version bump that adds a block\n\
         /// fails the compile of every incomplete match instead of falling into a\n\
         /// wildcard. Blocks a plugin adds are represented by\n\
         /// [`crate::block::BlockRef`], one level out.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]\n\
         #[repr(u16)]\n\
         pub enum Block {{"
    );
    for (id, (variant, name)) in variants.iter().enumerate() {
        let _ = writeln!(out, "    /// `{name}`\n    {variant} = {id},");
    }
    out.push_str("}\n\n");

    let _ = writeln!(
        out,
        "/// Every [`Block`], indexed by its registry id — the safe inverse of\n\
         /// `block as u16`, and the iteration order of the registry.\n\
         ///\n\
         /// An array rather than a 1,196-arm `match` because this crate forbids `unsafe`\n\
         /// (so the transmute is unavailable) and because a table index is one bounds\n\
         /// check where the match is a jump table plus 1,196 lines for rustc to chew.\n\
         pub static BLOCKS_BY_REGISTRY_ID: [Block; {block_count}] = ["
    );
    for chunk in variants.chunks(4) {
        out.push_str("    ");
        for (variant, _) in chunk {
            let _ = write!(out, "Block::{variant}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Registry ids sorted by canonical name, for `O(log {block_count})` name lookup\n\
         /// against [`crate::generated_block_registry::BLOCK_REGISTRY_NAMES`].\n\
         ///\n\
         /// A permutation of `u16` rather than a `[(&str, Block)]` pairs table on purpose:\n\
         /// the pairs table would re-introduce {block_count} fat pointers and their\n\
         /// relocations for names that already exist once in rodata.\n\
         pub static REGISTRY_IDS_BY_NAME: [u16; {block_count}] = ["
    );
    for chunk in by_name.chunks(16) {
        out.push_str("    ");
        for id in chunk {
            let _ = write!(out, "{id}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// The global block-state id of each block's default block state, indexed by\n\
         /// registry id.\n\
         ///\n\
         /// The default is **not** the block's lowest state id — it differs for 661 of the\n\
         /// 797 multi-state blocks — so this column is read from the captured\n\
         /// default-state mark, never inferred.\n\
         pub static DEFAULT_STATE: [u32; {block_count}] = ["
    );
    for chunk in default_state.chunks(16) {
        out.push_str("    ");
        for state in chunk {
            let _ = write!(out, "{state}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n");
    out
}

/// Renders `src/generated/tools.rs`: block tag membership plus the per-item
/// prototype `minecraft:tool` components.
fn generate_tools(dump: &Dump) -> String {
    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test tools -- --ignored`\n\
         // from tests/support/tool_jvm.txt (a headless 26.2 server dump of the vanilla\n\
         // block tags and every item's prototype `minecraft:tool` component, protocol\n\
         // 776 / Minecraft 26.2). DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1\n\
         // (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated `minecraft:tool` census for protocol 776 (Minecraft 26.2): block\n\
         //! tag membership, and the built-in tool component of every item that has one.\n\
         //!\n\
         //! Raw rodata arrays consumed by [`crate::tool`] — tag names are\n\
         //! `&'static str` and registry ids are `u16`, so the whole census lives in rodata\n\
         //! with zero heap. Both tables are sorted by their key for binary search.\n\n",
    );
    out.push_str("use crate::tool::{ToolBlocksDef, ToolDef, ToolRuleDef};\n\n");

    let _ = writeln!(out, "/// Number of block tags in the census.");
    let _ = writeln!(out, "pub const BLOCK_TAG_COUNT: usize = {};\n", dump.tags.len());
    let _ = writeln!(out, "/// Number of items carrying a built-in tool component.");
    let _ = writeln!(
        out,
        "pub const ITEM_TOOL_COUNT: usize = {};\n",
        dump.tools.len()
    );

    let _ = writeln!(
        out,
        "/// Every bound `minecraft:block` tag, sorted by name; each value is that tag's\n\
         /// members as sorted `minecraft:block` registry ids."
    );
    let _ = writeln!(
        out,
        "pub static BLOCK_TAGS: [(&str, &[u16]); BLOCK_TAG_COUNT] = ["
    );
    for (name, members) in &dump.tags {
        let ids = block_id_set(dump, members);
        let rendered: Vec<String> = ids.iter().map(u16::to_string).collect();
        let _ = writeln!(out, "    ({name:?}, &[{}]),", rendered.join(", "));
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Items whose *prototype* component map carries `minecraft:tool`, sorted by\n\
         /// `minecraft:item` registry id. A clientbound component patch is a delta\n\
         /// against it: a plain vanilla pickaxe sends an empty patch, so without this table\n\
         /// it would mine at bare-hand speed."
    );
    let _ = writeln!(
        out,
        "pub static ITEM_TOOLS: [(u16, ToolDef); ITEM_TOOL_COUNT] = ["
    );
    for (item_id, tool) in item_tool_entries(dump) {
        let _ = writeln!(out, "    (");
        let _ = writeln!(out, "        {item_id},");
        let _ = writeln!(out, "        ToolDef {{");
        if tool.rules.is_empty() {
            let _ = writeln!(out, "            rules: &[],");
        } else {
            let _ = writeln!(out, "            rules: &[");
            for rule in &tool.rules {
                let blocks = match &rule.blocks {
                    DumpBlocks::Tag(tag) => format!("ToolBlocksDef::Tag({tag:?})"),
                    DumpBlocks::Blocks(names) => {
                        let ids = block_id_set(dump, names);
                        let rendered: Vec<String> = ids.iter().map(u16::to_string).collect();
                        format!("ToolBlocksDef::Blocks(&[{}])", rendered.join(", "))
                    }
                };
                let speed = rule
                    .speed_bits
                    .map_or_else(|| "None".to_owned(), |bits| {
                        format!("Some({})", float_literal(bits))
                    });
                let correct = rule
                    .correct_for_drops
                    .map_or_else(|| "None".to_owned(), |value| format!("Some({value})"));
                let _ = writeln!(out, "                ToolRuleDef {{");
                let _ = writeln!(out, "                    blocks: {blocks},");
                let _ = writeln!(out, "                    speed: {speed},");
                let _ = writeln!(out, "                    correct_for_drops: {correct},");
                let _ = writeln!(out, "                }},");
            }
            let _ = writeln!(out, "            ],");
        }
        let _ = writeln!(
            out,
            "            default_mining_speed: {},",
            float_literal(tool.default_speed_bits)
        );
        let _ = writeln!(
            out,
            "            damage_per_block: {},",
            tool.damage_per_block
        );
        let _ = writeln!(
            out,
            "            can_destroy_blocks_in_creative: {},",
            tool.can_destroy_blocks_in_creative
        );
        let _ = writeln!(out, "        }},");
        let _ = writeln!(out, "    ),");
    }
    out.push_str("];\n");
    out
}

/// Extracts the generated outer item keys so the order check remains about the
/// emitted Rust, not an intermediate collection that could later be rendered
/// differently.
fn generated_item_tool_ids(rendered: &str) -> Vec<u16> {
    let table = rendered
        .split_once("pub static ITEM_TOOLS")
        .expect("generated item-tool table")
        .1;
    table
        .lines()
        .filter_map(|line| line.trim().strip_suffix(',')?.parse().ok())
        .collect()
}

#[test]
#[should_panic(expected = "tool item \"minecraft:not_an_item\" is not in the built-in registry")]
fn generator_rejects_unknown_tool_items() {
    let mut dump = parse_dump(DUMP);
    dump.tools[0].item = "minecraft:not_an_item".to_owned();
    let _ = generate_tools(&dump);
}

#[test]
#[should_panic(expected = "duplicate resolved item registry id")]
fn generator_rejects_duplicate_resolved_tool_item_ids() {
    let mut dump = parse_dump(DUMP);
    dump.tools.push(dump.tools[0].clone());
    let _ = generate_tools(&dump);
}

#[test]
fn generator_emits_strictly_ascending_u16_item_tool_ids() {
    let rendered = generate_tools(&parse_dump(DUMP));
    let ids = generated_item_tool_ids(&rendered);
    assert_eq!(ids.len(), tool::ITEM_TOOL_COUNT, "every tool gets one outer id");
    assert!(
        ids.windows(2).all(|pair| pair[0] < pair[1]),
        "generated item-tool ids must be strictly ascending: {ids:?}"
    );
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed tables (anchored to the committed dump)
// ---------------------------------------------------------------------------

/// Finds the first state id whose block name matches `name`, via the committed
/// block-state table — robust to id shifts across data bumps.
fn state_named(name: &str) -> StateId {
    (0..block_states::STATE_COUNT)
        .find(|&id| block_states::block_name(id) == Some(name))
        .and_then(StateId::new)
        .unwrap_or_else(|| panic!("{name} is not in the committed block-state table"))
}

fn stack(item: &str) -> ItemStack {
    ItemStack::new(item.parse().expect("item key"), 1)
}

/// Vanilla's break-time tick count for a block, so the assertions below can be
/// stated in the units a player experiences. Mirrors
/// `lodestone-game`'s `BreakInputs::progress_per_tick` for the default
/// on-ground, unenchanted, unsubmerged case; `lodestone-game` is not a
/// dependency of this crate, so the two-line formula is restated rather than
/// imported.
fn ticks_to_break(hardness: f32, speed: f32, correct_tool: bool) -> u32 {
    let divider = if correct_tool { 30.0 } else { 100.0 };
    let per_tick = speed / hardness / divider;
    let mut progress = 0.0f32;
    let mut ticks = 0u32;
    while progress < 1.0 {
        progress += per_tick;
        ticks += 1;
        assert!(ticks < 100_000, "break never completes");
    }
    ticks
}

#[test]
fn committed_tables_cover_the_committed_dump() {
    let dump = parse_dump(DUMP);

    assert_eq!(
        dump.tags.len(),
        tool::BLOCK_TAG_COUNT,
        "block tag count drifted from the dump"
    );
    assert_eq!(
        dump.tools.len(),
        tool::ITEM_TOOL_COUNT,
        "item tool count drifted from the dump"
    );

    // Every tag's membership, block for block, against the server's own answer.
    let mut checked_members = 0usize;
    for (name, members) in &dump.tags {
        let table = tool::block_tag_members(name)
            .unwrap_or_else(|| panic!("block tag {name} missing from the committed table"));
        assert_eq!(
            table.to_vec(),
            block_id_set(&dump, members),
            "membership mismatch for block tag {name}"
        );
        checked_members += table.len();
    }
    assert_eq!(
        checked_members, 4126,
        "expected 4,126 tag memberships in 26.2, got {checked_members} — \
         non-vacuity guard for the loop above"
    );

    // Every prototype tool component, field for field.
    for entry in &dump.tools {
        let table = tool::default_tool(&entry.item)
            .unwrap_or_else(|| panic!("item {} missing from the committed table", entry.item));
        assert_eq!(
            table.default_mining_speed.to_bits(),
            entry.default_speed_bits,
            "default_mining_speed mismatch for {}",
            entry.item
        );
        assert_eq!(
            table.damage_per_block, entry.damage_per_block,
            "damage_per_block mismatch for {}",
            entry.item
        );
        assert_eq!(
            table.can_destroy_blocks_in_creative, entry.can_destroy_blocks_in_creative,
            "can_destroy_blocks_in_creative mismatch for {}",
            entry.item
        );
        assert_eq!(
            table.rules.len(),
            entry.rules.len(),
            "rule count mismatch for {}",
            entry.item
        );
        for (index, (got, want)) in table.rules.iter().zip(&entry.rules).enumerate() {
            assert_eq!(
                got.speed.map(f32::to_bits),
                want.speed_bits,
                "rule {index} speed mismatch for {}",
                entry.item
            );
            assert_eq!(
                got.correct_for_drops, want.correct_for_drops,
                "rule {index} correct_for_drops mismatch for {}",
                entry.item
            );
            match (&got.blocks, &want.blocks) {
                (tool::ToolBlocksDef::Tag(got_tag), DumpBlocks::Tag(want_tag)) => {
                    assert_eq!(got_tag, want_tag, "rule {index} tag mismatch for {}", entry.item);
                }
                (tool::ToolBlocksDef::Blocks(got_ids), DumpBlocks::Blocks(want_names)) => {
                    assert_eq!(
                        got_ids.to_vec(),
                        block_id_set(&dump, &want_names),
                        "rule {index} block set mismatch for {}",
                        entry.item
                    );
                }
                (got_blocks, want_blocks) => panic!(
                    "rule {index} block-set kind mismatch for {}: {got_blocks:?} vs {want_blocks:?}",
                    entry.item
                ),
            }
        }
    }
}

/// The public lookup is intentionally narrower than [`Item::from_name`]: its
/// callers provide a component key, whose built-in form is always canonical.
/// Keeping bare paths out prevents this compatibility-facing boundary from
/// silently acquiring identifier parsing semantics while its table switches to
/// typed registry ids.
#[test]
fn default_tool_accepts_only_canonical_builtin_item_names() {
    assert!(tool::default_tool("minecraft:diamond_pickaxe").is_some());
    assert!(tool::default_tool("diamond_pickaxe").is_none());
    assert!(tool::default_tool("example:diamond_pickaxe").is_none());
    assert!(tool::default_tool("minecraft:not_an_item").is_none());
}

#[test]
fn block_registry_ids_match_the_dump() {
    let dump = parse_dump(DUMP);
    // The whole point of the registry table: registration order, `air` first —
    // *not* the alphabetical order the block-state table is sorted in.
    assert_eq!(dump.blocks[0], "minecraft:air");
    for (id, name) in dump.blocks.iter().enumerate() {
        let id = u16::try_from(id).expect("registry id fits u16");
        assert_eq!(
            Block::from_registry_id(id).map(Block::name),
            Some(name.as_str()),
            "block registry id {id} resolved to the wrong name"
        );
    }
    assert_eq!(
        Block::from_registry_id(u16::try_from(dump.blocks.len()).expect("fits")),
        None,
        "an out-of-range registry id must not resolve"
    );
}

#[test]
fn every_block_state_maps_to_its_own_block() {
    let dump = parse_dump(DUMP);
    for state in 0..block_states::STATE_COUNT {
        let registry_id = StateId::new(state)
            .expect("state is in range")
            .block()
            .registry_id();
        assert_eq!(
            dump.blocks[registry_id as usize].as_str(),
            block_states::block_name(state).expect("state has a name"),
            "state {state} maps to the wrong block"
        );
    }
}

#[test]
fn typed_block_tag_membership_uses_the_state_block_identity() {
    let short_grass = state_named("minecraft:short_grass");
    let stone = state_named("minecraft:stone");

    assert!(
        tool::block_tag_contains("minecraft:edible_for_sheep", short_grass.block()),
        "short grass is a member of the grazing tag"
    );
    assert!(
        !tool::block_tag_contains("minecraft:edible_for_sheep", stone.block()),
        "stone is not a member of the grazing tag"
    );
}

/// The headline number: a diamond pickaxe on stone. Every input is read from a
/// committed table that came out of the real server; only the arithmetic is ours.
#[test]
fn diamond_pickaxe_mines_stone_in_six_ticks() {
    let stone = state_named("minecraft:stone");
    let pickaxe = stack("minecraft:diamond_pickaxe");
    let mining = tool::mining(Some(&pickaxe), stone);

    assert_eq!(mining.speed, 8.0, "diamond tier mines pickaxe blocks at 8x");
    assert!(mining.correct_tool, "a pickaxe drops stone");
    assert_eq!(mining.damage_per_block, 1);

    let hardness = hardness::hardness(stone).hardness;
    assert_eq!(
        ticks_to_break(hardness, mining.speed, mining.correct_tool),
        6,
        "diamond pickaxe on stone must be 6 ticks"
    );
}

/// The regression this whole seam exists to prevent: bare-handed stone is 151
/// ticks, not 45. 45 is what feeding `BlockHardness::requires_correct_tool`
/// straight into `BreakInputs::correct_tool` produces.
#[test]
fn bare_hand_on_stone_is_151_ticks_not_45() {
    let stone = state_named("minecraft:stone");
    let mining = tool::mining(None, stone);

    assert_eq!(mining.speed, 1.0, "a bare hand mines at 1x");
    assert!(
        !mining.correct_tool,
        "stone requires a correct tool, so a bare hand is not correct for its drops"
    );
    assert_eq!(mining.damage_per_block, 0, "a bare hand has no durability cost");

    let hardness = hardness::hardness(stone).hardness;
    assert_eq!(
        ticks_to_break(hardness, mining.speed, mining.correct_tool),
        151,
        "bare-handed stone must be 151 ticks (f32 accumulation over 150 additions)"
    );
    assert_eq!(
        ticks_to_break(hardness, mining.speed, true),
        45,
        "…and 45 is the number the inversion bug produces, pinned so the two \
         can never be confused again"
    );
}

/// Dirt does not require a correct tool, so a bare hand *is* "correct" for it —
/// the inverse of the stone case, and the reason `correct_tool` cannot simply be
/// "am I holding a tool".
#[test]
fn bare_hand_is_correct_for_dirt() {
    let dirt = state_named("minecraft:dirt");
    let mining = tool::mining(None, dirt);
    assert!(mining.correct_tool, "dirt drops for anything, including a fist");
    assert_eq!(mining.speed, 1.0);
}

/// A pickaxe is the wrong tool for dirt: no rule matches, so the speed falls
/// back to `default_mining_speed` (1.0) — but dirt drops anyway, so
/// `correct_tool` is still true. Speed and drops are independent axes.
#[test]
fn a_pickaxe_on_dirt_is_slow_but_still_drops() {
    let dirt = state_named("minecraft:dirt");
    let pickaxe = stack("minecraft:diamond_pickaxe");
    let mining = tool::mining(Some(&pickaxe), dirt);
    assert_eq!(mining.speed, 1.0, "dirt is not in #mineable/pickaxe");
    assert!(mining.correct_tool, "dirt does not require a correct tool");
}

/// A shovel on stone: stone demands a correct tool and a shovel is not it, so
/// the drop flag is false *and* the speed is the 1.0 default.
#[test]
fn a_shovel_does_not_drop_stone() {
    let stone = state_named("minecraft:stone");
    let shovel = stack("minecraft:diamond_shovel");
    let mining = tool::mining(Some(&shovel), stone);
    assert_eq!(mining.speed, 1.0);
    assert!(!mining.correct_tool, "a shovel must not drop stone");
}

/// The tier gate, via `#incorrect_for_<material>_tool`: a wooden pickaxe mines
/// obsidian faster than a fist but still never drops it, because the first rule
/// denies drops without supplying a speed and the second supplies a speed
/// without a verdict. Getting the two walks entangled collapses this case.
#[test]
fn a_wooden_pickaxe_speeds_up_obsidian_but_never_drops_it() {
    let obsidian = state_named("minecraft:obsidian");
    let wooden = tool::mining(Some(&stack("minecraft:wooden_pickaxe")), obsidian);
    assert_eq!(wooden.speed, 2.0, "wood tier still applies its #mineable/pickaxe speed");
    assert!(
        !wooden.correct_tool,
        "obsidian is in #incorrect_for_wooden_tool, so wood never drops it"
    );

    let diamond = tool::mining(Some(&stack("minecraft:diamond_pickaxe")), obsidian);
    assert!(diamond.correct_tool, "diamond drops obsidian");

    let hardness = hardness::hardness(obsidian).hardness;
    assert_eq!(
        ticks_to_break(hardness, wooden.speed, wooden.correct_tool),
        2500,
        "wooden pickaxe on obsidian"
    );
    assert_eq!(
        ticks_to_break(hardness, diamond.speed, diamond.correct_tool),
        188,
        "diamond pickaxe on obsidian"
    );
    assert_eq!(
        ticks_to_break(hardness, 1.0, false),
        5001,
        "…versus the bare hand's 5,001 ticks (~4m10s), the defect that motivated \
         this seam. 5,001 and not the round 5,000: the accumulator is f32 and \
         5,000 additions of 0.0002 land just short of 1.0, the same drift that \
         makes bare-handed stone 151 rather than 150"
    );
}

/// A rule can name blocks directly rather than by tag; vanilla's only such rule
/// is the sword/shears cobweb entry, and it is the one shape a tag-only
/// implementation would silently miss.
#[test]
fn shears_match_cobweb_through_an_explicit_block_set() {
    let cobweb = state_named("minecraft:cobweb");
    let mining = tool::mining(Some(&stack("minecraft:shears")), cobweb);
    assert_eq!(mining.speed, 15.0, "shears cut cobweb at 15x");
    assert!(mining.correct_tool);
}

/// A wire-supplied `minecraft:tool` overrides the item's prototype wholesale —
/// including making a pickaxe *worse*, which proves the prototype is not being
/// silently merged in underneath.
#[test]
fn a_wire_supplied_tool_overrides_the_prototype() {
    let stone = state_named("minecraft:stone");
    let mut pickaxe = stack("minecraft:diamond_pickaxe");
    pickaxe.components.tool = ToolPatch::Set(ItemTool::new(
        vec![ToolRule::new(
            ToolBlocks::Tag("minecraft:mineable/pickaxe".parse().expect("tag key")),
            Some(2.0),
            Some(false),
        )],
        1.0,
        3,
        true,
    ));
    let mining = tool::mining(Some(&pickaxe), stone);
    assert_eq!(mining.speed, 2.0, "the patch's speed must win over the prototype's 8.0");
    assert!(!mining.correct_tool, "the patch says this tool does not drop stone");
    assert_eq!(mining.damage_per_block, 3);
}

/// A wire rule can also carry an explicit block set, as version-scoped registry
/// ids. Those ids are the *block* registry's, not the block-state palette's.
#[test]
fn a_wire_supplied_rule_can_name_blocks_by_registry_id() {
    let stone = state_named("minecraft:stone");
    let stone_block = stone.block().registry_id();
    let mut wand = stack("minecraft:stick");
    wand.components.tool = ToolPatch::Set(ItemTool::new(
        vec![ToolRule::new(
            ToolBlocks::Blocks(vec![i32::from(stone_block)]),
            Some(100.0),
            Some(true),
        )],
        1.0,
        0,
        true,
    ));
    let mining = tool::mining(Some(&wand), stone);
    assert_eq!(mining.speed, 100.0);
    assert!(mining.correct_tool);
}

/// `/give …[!minecraft:tool]` strips the prototype: the pickaxe mines like a
/// fist. Modelling a removal as "inherit" would leave it at 8x.
#[test]
fn removing_the_tool_component_reverts_to_bare_hands() {
    let stone = state_named("minecraft:stone");
    let mut pickaxe = stack("minecraft:diamond_pickaxe");
    pickaxe.components.tool = ToolPatch::Removed;
    let mining = tool::mining(Some(&pickaxe), stone);
    assert_eq!(mining.speed, 1.0);
    assert!(!mining.correct_tool);
    assert_eq!(mining.damage_per_block, 0);
}

/// An item with no tool component at all behaves exactly like an empty hand.
#[test]
fn a_non_tool_item_mines_like_a_bare_hand() {
    let stone = state_named("minecraft:stone");
    let held = tool::mining(Some(&stack("minecraft:dirt")), stone);
    let empty = tool::mining(None, stone);
    assert_eq!(held, empty);
}

/// A rule naming a tag this build's census does not know matches nothing, rather
/// than panicking or matching everything — the datapack-retag gap documented in
/// `tool.rs`.
#[test]
fn an_unknown_tag_matches_nothing() {
    let stone = state_named("minecraft:stone");
    let mut odd = stack("minecraft:stick");
    odd.components.tool = ToolPatch::Set(ItemTool::new(
        vec![ToolRule::new(
            ToolBlocks::Tag("example:not_a_real_tag".parse().expect("tag key")),
            Some(50.0),
            Some(true),
        )],
        1.0,
        1,
        true,
    ));
    let mining = tool::mining(Some(&odd), stone);
    assert_eq!(mining.speed, 1.0, "no rule matched, so the default applies");
    assert!(!mining.correct_tool);
}

#[test]
fn mining_accepts_only_validated_state_ids() {
    let lookup: fn(Option<&ItemStack>, StateId) -> ToolMining = tool::mining;
    let stone = state_named("minecraft:stone");

    assert_eq!(lookup(None, stone).speed, 1.0);
    assert!(StateId::new(block_states::STATE_COUNT).is_none());
    assert!(StateId::new(u32::MAX).is_none());
}

#[test]
fn every_state_resolves_for_a_pickaxe_and_a_fist() {
    let pickaxe = stack("minecraft:diamond_pickaxe");
    for state in 0..block_states::STATE_COUNT {
        let state_id = StateId::new(state).expect("loop only visits known states");
        let _ = tool::mining(None, state_id);
        let _ = tool::mining(Some(&pickaxe), state_id);
    }
}

// ---------------------------------------------------------------------------
// Cross-source corroboration + drift guard
// ---------------------------------------------------------------------------

/// `.cache/mc/26.2`, which holds the Mojang-generated reports and the extracted
/// vanilla data pack. Absent on a fresh clone.
fn cache_dir() -> PathBuf {
    manifest_dir().join("../../.cache/mc/26.2")
}

/// `minecraft:block` registry id → name, read from Mojang's own
/// `generated/reports/registries.json`. `None` when the cache is absent.
///
/// This is the *external* anchor for the registry order: it is produced by the
/// game's own data generator, not by our JVM oracle and not by our tables.
fn registries_report_blocks() -> Option<Vec<String>> {
    let path = cache_dir().join("generated/reports/registries.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).expect("registries.json parses");
    let entries = doc["minecraft:block"]["entries"]
        .as_object()
        .expect("minecraft:block has an entries object");
    let mut by_id: BTreeMap<u64, String> = BTreeMap::new();
    for (name, info) in entries {
        let id = info["protocol_id"]
            .as_u64()
            .expect("protocol_id is an integer");
        assert!(
            by_id.insert(id, name.clone()).is_none(),
            "duplicate block protocol_id {id} in registries.json"
        );
    }
    let names: Vec<String> = by_id
        .iter()
        .enumerate()
        .map(|(index, (&id, name))| {
            assert_eq!(id as usize, index, "block protocol_ids are not a dense 0..N");
            name.clone()
        })
        .collect();
    Some(names)
}

/// The **registry-order** half of the census, re-derived from Mojang's own
/// `generated/reports/registries.json` rather than from our JVM dump.
///
/// This test distinguishes registration order from the alphabetical block order
/// in name-keyed `blocks.json`: `air` is registry 0 but alphabetical 19, and
/// `stone` is registry 1 but alphabetical 975. A lookup that uses the latter
/// as a registry id resolves every id to an unrelated block.
///
/// Skipped (not failed) when the cache is absent, as its sibling report test is;
/// the committed dump remains the anchor either way.
#[test]
fn block_registry_order_agrees_with_mojangs_registries_report() {
    let Some(names) = registries_report_blocks() else {
        eprintln!("skipping: .cache/mc/26.2 registries.json is absent (needs the extracted jar)");
        return;
    };
    assert_eq!(
        names.len(),
        block_states::BLOCK_COUNT as usize,
        "block count drifted from Mojang's registry report"
    );
    assert_eq!(names[0], "minecraft:air", "registration order starts at air");
    for (id, name) in names.iter().enumerate() {
        let id = u16::try_from(id).expect("registry id fits u16");
        assert_eq!(
            Block::from_registry_id(id).map(Block::name),
            Some(name.as_str()),
            "registry id {id} resolves to the wrong block name"
        );
    }

    // The negative control. The loop above only means something if registration
    // order and the alphabetical order of the *same* names are actually
    // different orderings — otherwise it compares an ordering with itself and
    // could never fail. They are: `air` is registry 0 but 19th alphabetically,
    // and `stone` is registry 1 but 975th.
    //
    // (Note `block_name` cannot serve as the control: it takes a block-*state*
    // id, not a block index, and state 0/1 really are air/stone. The alphabetical
    // block index is not publicly reachable, which is why the original defect
    // could only be seen from the registry side.)
    let mut alphabetical = names.clone();
    alphabetical.sort();
    assert_ne!(
        alphabetical, names,
        "registration order and alphabetical order coincide, so this test cannot \
         distinguish them — the negative control failed"
    );
    assert_eq!(
        alphabetical.iter().position(|n| n == "minecraft:air"),
        Some(19),
        "air is registry 0 and alphabetical 19"
    );
    assert_eq!(
        alphabetical.iter().position(|n| n == "minecraft:stone"),
        Some(975),
        "stone is registry 1 and alphabetical 975"
    );
}

/// The **tag-membership** half of the census, re-derived from the extracted
/// vanilla data pack (`.cache/mc/26.2/src/data/minecraft/tags/block/**.json`)
/// rather than from our JVM dump.
///
/// The dump reads vanilla's own registry "get tags" accessor *after* its own
/// tag loader has resolved nested
/// `#tag` references and dropped optional entries; this walks the raw JSON and
/// resolves those references independently, so a mistake in the oracle's tag
/// binding (the failure mode where every tag comes back empty, or a nested
/// reference is silently skipped) shows up as a disagreement rather than as a
/// plausible-looking table. Block names are mapped to ids through
/// `registries.json`, not through our own tables.
///
/// Skipped (not failed) when the cache is absent.
#[test]
fn block_tag_membership_agrees_with_the_vanilla_datapack() {
    let tag_root = cache_dir().join("src/data/minecraft/tags/block");
    let Some(registry) = registries_report_blocks() else {
        eprintln!("skipping: .cache/mc/26.2 registries.json is absent");
        return;
    };
    if !tag_root.is_dir() {
        eprintln!(
            "skipping: {} is absent (needs the extracted 26.2 data pack)",
            tag_root.display()
        );
        return;
    }
    let ids: BTreeMap<&str, u16> = registry
        .iter()
        .enumerate()
        .map(|(id, name)| (name.as_str(), u16::try_from(id).expect("fits u16")))
        .collect();

    // Every tag file, keyed the way the wire and vanilla's own tag-key location accessor write it.
    let mut raw: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    collect_tag_files(&tag_root, &mut String::new(), &mut raw);
    assert_eq!(
        raw.len(),
        tool::BLOCK_TAG_COUNT,
        "the data pack and the census disagree on how many block tags exist"
    );

    let mut checked = 0usize;
    for name in raw.keys() {
        let mut members: Vec<u16> = resolve_tag(&raw, &ids, name, &mut Vec::new())
            .into_iter()
            .collect();
        members.sort_unstable();
        let table = tool::block_tag_members(name)
            .unwrap_or_else(|| panic!("block tag {name} missing from the committed table"));
        assert_eq!(
            table.to_vec(),
            members,
            "membership mismatch for block tag {name}"
        );
        checked += members.len();
    }
    assert_eq!(
        checked, 4126,
        "expected 4,126 tag memberships in 26.2, got {checked} — non-vacuity \
         guard: an unresolved `#tag` reference would silently shrink this"
    );
}

/// Recursively collects `<dir>/**/<stem>.json` as `minecraft:<prefix><stem>`.
fn collect_tag_files(
    dir: &std::path::Path,
    prefix: &mut String,
    out: &mut BTreeMap<String, serde_json::Value>,
) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read tag dir")
        .map(|entry| entry.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let stem = path
            .file_stem()
            .expect("stem")
            .to_str()
            .expect("utf8")
            .to_owned();
        if path.is_dir() {
            let restore = prefix.len();
            prefix.push_str(&stem);
            prefix.push('/');
            collect_tag_files(&path, prefix, out);
            prefix.truncate(restore);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read tag file");
        let value: serde_json::Value = serde_json::from_str(&text).expect("tag json parses");
        out.insert(format!("minecraft:{prefix}{stem}"), value);
    }
}

/// Resolves one tag's `values` to block registry ids, following nested `#tag`
/// references and honouring `{"id": …, "required": false}` entries the way
/// vanilla's own tag loader does.
fn resolve_tag(
    raw: &BTreeMap<String, serde_json::Value>,
    ids: &BTreeMap<&str, u16>,
    tag: &str,
    seen: &mut Vec<String>,
) -> std::collections::BTreeSet<u16> {
    let mut out = std::collections::BTreeSet::new();
    if seen.iter().any(|visited| visited == tag) {
        return out;
    }
    seen.push(tag.to_owned());
    let Some(doc) = raw.get(tag) else {
        panic!("tag {tag} is referenced but has no file");
    };
    let empty = Vec::new();
    for entry in doc["values"].as_array().unwrap_or(&empty) {
        let (id, required) = match entry {
            serde_json::Value::String(id) => (id.clone(), true),
            serde_json::Value::Object(map) => (
                map["id"].as_str().expect("entry id is a string").to_owned(),
                map.get("required")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
            ),
            other => panic!("unrecognised tag entry {other:?} in {tag}"),
        };
        // A leading `#` makes the entry a reference to another tag; anything
        // else is a block. Either way an unqualified name is `minecraft:`.
        let bare = id.strip_prefix('#').unwrap_or(&id);
        let qualified = if bare.contains(':') {
            bare.to_owned()
        } else {
            format!("minecraft:{bare}")
        };
        if id.starts_with('#') {
            out.extend(resolve_tag(raw, ids, &qualified, seen));
            continue;
        }
        match ids.get(qualified.as_str()) {
            Some(&block) => {
                out.insert(block);
            }
            None => assert!(
                !required,
                "tag {tag} requires block {qualified}, which is not in the registry"
            ),
        }
    }
    seen.pop();
    out
}

/// The item half of the dump, re-derived from a *different* Mojang artifact:
/// `generated/reports/minecraft/components/item/<item>.json`, the version's own
/// `RegistryComponentsReport` output. Two independent renderings of the same
/// game data agreeing is what makes the committed table trustworthy.
///
/// Skipped (not failed) when `.cache/mc/26.2` is absent, since the report is a
/// build artifact rather than a committed one; the committed dump remains the
/// anchor either way.
#[test]
fn dump_agrees_with_mojangs_own_components_report() {
    let report_dir = manifest_dir()
        .join("../../.cache/mc/26.2/generated/reports/minecraft/components/item");
    let Ok(entries) = std::fs::read_dir(&report_dir) else {
        eprintln!(
            "skipping: {} is absent (needs the extracted 26.2 jar)",
            report_dir.display()
        );
        return;
    };

    let dump = parse_dump(DUMP);
    let mut seen = 0usize;
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let item = format!(
            "minecraft:{}",
            path.file_stem().expect("stem").to_str().expect("utf8")
        );
        let text = std::fs::read_to_string(&path).expect("read report");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse report");
        let Some(reported) = value.get("components").and_then(|c| c.get("minecraft:tool")) else {
            assert!(
                !dump.tools.iter().any(|t| t.item == item),
                "{item} has a tool in the dump but none in Mojang's report"
            );
            continue;
        };
        seen += 1;

        let entry = dump
            .tools
            .iter()
            .find(|t| t.item == item)
            .unwrap_or_else(|| panic!("{item} has a tool in Mojang's report but none in the dump"));

        let default_speed = reported
            .get("default_mining_speed")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0) as f32;
        assert_eq!(
            default_speed.to_bits(),
            entry.default_speed_bits,
            "{item}: default_mining_speed disagrees with Mojang's report"
        );
        let damage = reported
            .get("damage_per_block")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1) as u32;
        assert_eq!(damage, entry.damage_per_block, "{item}: damage_per_block");
        let creative = reported
            .get("can_destroy_blocks_in_creative")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        assert_eq!(
            creative, entry.can_destroy_blocks_in_creative,
            "{item}: can_destroy_blocks_in_creative"
        );

        let empty = Vec::new();
        let rules = reported
            .get("rules")
            .and_then(serde_json::Value::as_array)
            .unwrap_or(&empty);
        assert_eq!(rules.len(), entry.rules.len(), "{item}: rule count");
        for (index, (reported_rule, dump_rule)) in rules.iter().zip(&entry.rules).enumerate() {
            let blocks = &reported_rule["blocks"];
            let expected = match &dump_rule.blocks {
                DumpBlocks::Tag(tag) => serde_json::Value::String(format!("#{tag}")),
                DumpBlocks::Blocks(names) if names.len() == 1 => {
                    serde_json::Value::String(names[0].clone())
                }
                DumpBlocks::Blocks(names) => serde_json::Value::Array(
                    names
                        .iter()
                        .map(|n| serde_json::Value::String(n.clone()))
                        .collect(),
                ),
            };
            assert_eq!(blocks, &expected, "{item}: rule {index} blocks");
            let speed = reported_rule
                .get("speed")
                .and_then(serde_json::Value::as_f64)
                .map(|s| (s as f32).to_bits());
            assert_eq!(speed, dump_rule.speed_bits, "{item}: rule {index} speed");
            let correct = reported_rule
                .get("correct_for_drops")
                .and_then(serde_json::Value::as_bool);
            assert_eq!(
                correct, dump_rule.correct_for_drops,
                "{item}: rule {index} correct_for_drops"
            );
        }
    }
    assert_eq!(
        seen,
        dump.tools.len(),
        "Mojang's report and the JVM dump must name the same set of tool items"
    );
}

#[test]
#[ignore = "regenerates/verifies the committed tables; run explicitly"]
fn committed_tables_match_dump() {
    let dump = parse_dump(DUMP);
    let block_registry = generate_block_registry(&dump);
    let block_enum = generate_block_enum(&dump);
    let tools = generate_tools(&dump);

    assert!(
        tools.contains("pub static ITEM_TOOLS: [(u16, ToolDef); ITEM_TOOL_COUNT] = ["),
        "the generated item-tool table must use numeric minecraft:item registry ids"
    );
    assert!(
        !tools.contains("(&str, ToolDef)"),
        "the generated item-tool table must not retain string item keys"
    );

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(block_registry_path(), &block_registry).expect("write block registry");
        std::fs::write(block_enum_path(), &block_enum).expect("write block enum");
        std::fs::write(tools_path(), &tools).expect("write tools");
        eprintln!("regenerated {}", block_registry_path().display());
        eprintln!("regenerated {}", block_enum_path().display());
        eprintln!("regenerated {}", tools_path().display());
        return;
    }

    let committed_registry =
        std::fs::read_to_string(block_registry_path()).expect("committed block registry present");
    assert_eq!(
        block_registry, committed_registry,
        "src/generated/block_registry.rs is stale vs the JVM dump; \
         regenerate with LODESTONE_REGEN=1"
    );
    let committed_enum =
        std::fs::read_to_string(block_enum_path()).expect("committed block enum present");
    assert_eq!(
        block_enum, committed_enum,
        "src/generated/block_enum.rs is stale vs the JVM dump; regenerate with LODESTONE_REGEN=1"
    );
    let committed_tools = std::fs::read_to_string(tools_path()).expect("committed tools present");
    assert_eq!(
        tools, committed_tools,
        "src/generated/tools.rs is stale vs the JVM dump; regenerate with LODESTONE_REGEN=1"
    );
}
