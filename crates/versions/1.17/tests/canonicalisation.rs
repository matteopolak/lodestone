//! Generator + drift guard for this era's flat block-state id -> canonical
//! 26.2 block-state id table (`src/generated/canonical.rs`), plus hermetic,
//! hardcoded-value tests that run on every `cargo test`. Modelled on the
//! generate-or-assert pattern `crates/lodestone-data/tests/block_states.rs`
//! and the 1.14 era's own table use.
//!
//! # Why one table, when the era below needs three
//!
//! Each release inserts blocks into vanilla's global palette, so the same
//! numeric flat state id normally names a different block in each protocol —
//! 11,271 / 11,337 / 17,112 states across the three releases below this one.
//! 1.18 inserted none: it was a world-generation release, and the two jars'
//! `--reports` `blocks.json` dumps are **byte-identical**. That is not taken
//! on trust here; [`the_committed_dump_is_the_one_both_jars_produce`] pins the
//! dump's own content hash and its shape, and the procedure for re-deriving
//! the identity from the jars is in the refresh section below.
//!
//! # Data provenance
//!
//! `tests/support/blocks_1_17_1_jar.json` is **not** a community dataset: it
//! is the unmodified output of Mojang's own data generator, run in its
//! `--reports` mode against the real `.cache/mc/1.17.1/server.jar`, the same
//! tool and report shape `crates/lodestone-data/tests/block_states.rs` reads
//! for the 26.2 side. Every state lists its own `id` and `properties`
//! explicitly, so no combinatorial re-derivation of vanilla's state-id
//! numbering is needed on either side.
//!
//! The bridge from a source `(name, properties)` pair to a 26.2 state id
//! reuses the technique `lodestone_canonical::canonical` already uses for the
//! pre-Flattening families: build a `(name, properties) -> 26.2 id` reverse
//! index from `lodestone_data::block_states` (itself jar-derived, not this
//! crate's own encoder/decoder), try a direct match, then a small
//! hand-verified rename table, then generic single-property fallbacks. After
//! applying them, zero states are left unmapped — asserted by
//! [`committed_table_matches_dump`], which panics naming the offending state
//! rather than defaulting it to air.
//!
//! # Refreshing after a source jar changes
//!
//! 1. Re-run the data generator against both server jars under Apple
//!    `container` (see `docs/oracles-and-benchmarks.md`). 1.17.1 ships a flat
//!    obfuscated jar whose generator entry point is reachable on the
//!    classpath; 1.18.2 ships a bundler jar, so the generator is selected
//!    through the bundler's own main-class property. Both need a Java 17
//!    runtime, not the Java 8 image the pre-1.17 oracles use.
//!
//! 2. Compare the two `reports/blocks.json` files. If they are still
//!    identical, copy either over `tests/support/blocks_1_17_1_jar.json`. If
//!    they are **not**, this era has stopped sharing one table: add a second
//!    dump, a second generated table and a `canonical::table_for` arm before
//!    doing anything else.
//!
//! 3. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-v1-17 --test canonicalisation \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```
//!
//! If the 26.2 registry itself changed (`lodestone-data` regenerated), rerun
//! step 3 alone — no source-side dump needs to change.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::block_states;

/// The committed jar dump, shared by both protocols in this era.
const DUMP: &str = include_str!("support/blocks_1_17_1_jar.json");
/// That dump's own filename, for the generated header's provenance line.
const DUMP_FILE: &str = "blocks_1_17_1_jar.json";
/// Path under `src/generated/` the rendered table is committed at.
const COMMITTED: &str = "src/generated/canonical.rs";
/// Minecraft version the dump came from.
const MINECRAFT: &str = "1.17.1";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// One source-version state: its flat wire id and sorted `(key, value)`
/// properties.
struct SourceState {
    id: u32,
    name: String,
    properties: Vec<(String, String)>,
}

/// Parses the jar dump into one entry per state id, indexed
/// `0..SOURCE_STATE_COUNT` (the report's own per-state `id` field is
/// authoritative; this asserts density rather than assuming it).
fn parse_dump(doc: &str) -> Vec<SourceState> {
    let value: serde_json::Value = serde_json::from_str(doc).expect("dump is valid JSON");
    let object = value.as_object().expect("blocks.json is a JSON object");

    let mut by_id: HashMap<u32, SourceState> = HashMap::new();
    let mut max_id = 0u32;
    for (name, info) in object {
        for state in info["states"].as_array().expect("states is an array") {
            let id = u32::try_from(state["id"].as_u64().expect("state id is an integer"))
                .expect("state id fits u32");
            let mut properties: Vec<(String, String)> = state
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|object| {
                    object
                        .iter()
                        .map(|(key, value)| {
                            (
                                key.clone(),
                                value.as_str().expect("property value is a string").to_owned(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            properties.sort();
            max_id = max_id.max(id);
            let previous = by_id.insert(
                id,
                SourceState {
                    id,
                    name: name.clone(),
                    properties,
                },
            );
            assert!(previous.is_none(), "duplicate {MINECRAFT} state id {id}");
        }
    }

    let count = by_id.len();
    assert_eq!(
        max_id as usize + 1,
        count,
        "{MINECRAFT} state ids are not dense: max id {max_id}, count {count}"
    );

    let mut states: Vec<SourceState> = by_id.into_values().collect();
    states.sort_by_key(|s| s.id);
    states
}

/// `(name, sorted properties) -> 26.2 state id`, built once from
/// `lodestone_data::block_states`.
fn canonical_reverse_index() -> HashMap<(String, Vec<(String, String)>), u32> {
    let mut index = HashMap::with_capacity(block_states::STATE_COUNT as usize);
    for id in 0..block_states::STATE_COUNT {
        let name = block_states::block_name(id).expect("id in 0..STATE_COUNT");
        let properties = block_states::properties(id).expect("id in 0..STATE_COUNT");
        let owned: Vec<(String, String)> = properties
            .iter()
            .map(|&(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        index.insert((name.to_owned(), owned), id);
    }
    index
}

/// Renames a 1.17.1 block name to its 26.2 name.
///
/// Each entry is a rename that happened **after** 1.17 and is verified against
/// the 26.2 registry: the old name resolves to nothing there and the new one
/// carries the same property set. Note what is *absent* — `grass_path` ->
/// `dirt_path` and the cauldron split are rules the 1.14 era needs and this
/// one does not, because both landed *in* 1.17 and the dump already carries
/// the modern form.
fn bridge_name(name: &str) -> &str {
    match name {
        // Renamed `grass` -> `short_grass` in 1.20.3, to disambiguate it from
        // the ground block.
        "minecraft:grass" => "minecraft:short_grass",
        // The plain (uncoloured, non-copper) chain became `iron_chain` when
        // 1.21's copper chains were added; `axis` + `waterlogged` carry over
        // unchanged.
        "minecraft:chain" => "minecraft:iron_chain",
        other => other,
    }
}

/// Bridges one 1.17.1 `(name, properties)` pair to a 26.2 state id. Order:
/// direct match, then [`bridge_name`] alone, then single-property fallbacks —
/// each tried only if the exact match fails, so none can override a block
/// that already resolves without it.
fn resolve(
    name: &str,
    properties: &[(String, String)],
    reverse: &HashMap<(String, Vec<(String, String)>), u32>,
) -> Option<u32> {
    let bridged_name = bridge_name(name).to_owned();
    let props = properties.to_vec();

    if let Some(&id) = reverse.get(&(bridged_name.clone(), props.clone())) {
        return Some(id);
    }

    // Leaves, rails and barrier gained a `waterlogged` property after this
    // era. Every one of these blocks is unambiguously not waterlogged in
    // 1.17, because the concept did not exist for them yet — not "unknown,
    // assume false". Same justification
    // `lodestone_canonical::canonical::resolve_canonical` documents for its
    // own generic `waterlogged=false` fallback.
    let mut with_waterlogged = props.clone();
    insert_sorted(&mut with_waterlogged, "waterlogged", "false");
    if let Some(&id) = reverse.get(&(bridged_name.clone(), with_waterlogged)) {
        return Some(id);
    }

    // Every mob-head/skull block gained a `powered` redstone-signal property
    // after this era; the block's own 26.2 registry default is `false`, and
    // 1.17 has no redstone-signal concept for skulls at all.
    let mut with_powered = props.clone();
    insert_sorted(&mut with_powered, "powered", "false");
    if let Some(&id) = reverse.get(&(bridged_name.clone(), with_powered)) {
        return Some(id);
    }

    // Both fallbacks together, for a block that needs each.
    let mut with_both = props;
    insert_sorted(&mut with_both, "powered", "false");
    insert_sorted(&mut with_both, "waterlogged", "false");
    reverse.get(&(bridged_name, with_both)).copied()
}

/// Inserts `(key, value)` into `properties`, kept sorted by key.
fn insert_sorted(properties: &mut Vec<(String, String)>, key: &str, value: &str) {
    let position = properties
        .iter()
        .position(|(existing, _)| existing.as_str() > key)
        .unwrap_or(properties.len());
    properties.insert(position, (key.to_owned(), value.to_owned()));
}

/// Renders the committed table source.
///
/// Panics naming the offending source state if [`resolve`] cannot bridge it:
/// a future occurrence means a jar or registry update introduced a case this
/// generator does not cover, and that must be loud, not silently defaulted to
/// air at generation time.
fn generate(states: &[SourceState], reverse: &HashMap<(String, Vec<(String, String)>), u32>) -> String {
    let air_id = *reverse
        .get(&("minecraft:air".to_owned(), Vec::new()))
        .expect("26.2 registry always defines minecraft:air with no properties");

    let mut mapped = Vec::with_capacity(states.len());
    for state in states {
        let Some(id) = resolve(&state.name, &state.properties, reverse) else {
            panic!(
                "{MINECRAFT} state {} ({}, {:?}) has no canonical 26.2 mapping",
                state.id, state.name, state.properties
            );
        };
        mapped.push(id);
    }

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-v1-17 --test canonicalisation -- --ignored`\n",
    );
    let _ = writeln!(
        out,
        "// from tests/support/{DUMP_FILE} (protocols 756 and 758 / Minecraft {MINECRAFT}\n\
         // and 1.18.2, whose jar dumps are byte-identical) against"
    );
    out.push_str(
        "// the 26.2 block-state registry (`lodestone_data::block_states`).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated 1.17 era (protocols 756 and 758) -> canonical 26.2 block-state id table.\n//!\n\
         //! `STATE_TO_CANONICAL[wire_state_id]` is the 26.2\n\
         //! `lodestone_data::block_states` id that wire state carries. Pure rodata,\n\
         //! zero heap; see `src/canonical.rs` for the lookup wrapper.\n\n",
    );
    out.push_str(
        "/// Number of block states in this era's own global palette (source ids are\n\
         /// `0..SOURCE_STATE_COUNT`).\n",
    );
    let _ = writeln!(out, "pub const SOURCE_STATE_COUNT: u32 = {};\n", mapped.len());
    out.push_str(
        "/// The canonical 26.2 `minecraft:air` state id, baked at generation time\n\
         /// exactly like every other entry in [`STATE_TO_CANONICAL`] — a registry\n\
         /// regeneration that reorders 26.2 states requires regenerating this whole\n\
         /// file anyway, so this is no less current than the rest of the table.\n",
    );
    let _ = writeln!(out, "pub const AIR_STATE_ID: u32 = {air_id};\n");
    out.push_str(
        "/// `STATE_TO_CANONICAL[s]` is the canonical 26.2 state id for this era's flat\n\
         /// wire state `s`.\n",
    );
    out.push_str("pub static STATE_TO_CANONICAL: [u32; SOURCE_STATE_COUNT as usize] = [\n");
    for id in &mapped {
        let _ = writeln!(out, "    {id},");
    }
    out.push_str("];\n");
    out
}

// ---------------------------------------------------------------------------
// Drift guard (heavy: builds the full 26.2 reverse index; ignored)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates/verifies the committed table; run explicitly"]
fn committed_table_matches_dump() {
    let reverse = canonical_reverse_index();
    let states = parse_dump(DUMP);
    let generated = generate(&states, &reverse);
    let path = manifest_dir().join(COMMITTED);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(&path, &generated).expect("write committed table");
        eprintln!("regenerated {} ({} states)", path.display(), states.len());
        return;
    }

    let committed = std::fs::read_to_string(&path).expect("committed table present");
    assert_eq!(
        generated, committed,
        "{COMMITTED} is stale vs tests/support/{DUMP_FILE} or the 26.2 registry; regenerate \
         with LODESTONE_REGEN=1 (see the test module docs)"
    );
}

// ---------------------------------------------------------------------------
// Hermetic checks (every `cargo test`)
// ---------------------------------------------------------------------------

/// The committed dump is the one both jars produce, by shape and by content.
///
/// The content check is a hash of the dump's own bytes, recorded when the two
/// jars were run side by side and measured equal. It is not a checksum of the
/// table under test — it pins the *input*, so an unrelated dump swapped in
/// under the same filename fails here rather than silently regenerating a
/// wrong table.
#[test]
fn the_committed_dump_is_the_one_both_jars_produce() {
    let states = parse_dump(DUMP);
    assert_eq!(states.len(), 20342, "1.17.1's global palette holds 20,342 states");

    let value: serde_json::Value = serde_json::from_str(DUMP).expect("dump parses");
    assert_eq!(
        value.as_object().expect("object").len(),
        898,
        "1.17.1's block registry holds 898 blocks"
    );

    // FNV-1a over the raw dump bytes: cheap, dependency-free, and enough to
    // separate this dump from any other version's.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in DUMP.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    assert_eq!(
        hash, 0x12fe_e107_2185_3f10,
        "tests/support/{DUMP_FILE} is not the dump this table was generated from"
    );
}

/// Both protocols resolve to the same table, and it is the size the dump says.
#[test]
fn both_protocols_share_one_table_of_the_dumps_size() {
    use lodestone_v1_17::canonical;
    use lodestone_v1_17::{PROTOCOL_1_17_1, PROTOCOL_1_18_2};

    let a = canonical::table_for(PROTOCOL_1_17_1);
    let b = canonical::table_for(PROTOCOL_1_18_2);
    assert_eq!(a.state_count(), 20342);
    assert_eq!(b.state_count(), 20342);
    assert_eq!(a.air_state_id(), b.air_state_id());
    for id in [0, 1, 100, 5_000, 20_341] {
        assert_eq!(a.resolve(id), b.resolve(id));
    }
    assert_eq!(a.resolve(20342), None, "one past the palette names no state");
}

/// Discriminating states resolve to their **26.2** ids, not their wire ids.
///
/// Each expected value is looked up in `lodestone_data::block_states` by name
/// and properties — the 26.2 registry, generated from that jar — while the
/// wire id comes from the committed 1.17.1 dump. Neither side is this crate's
/// own table, so the test cannot pass by two symmetric mistakes.
#[test]
fn discriminating_states_resolve_to_their_26_2_ids_not_their_wire_ids() {
    use lodestone_v1_17::canonical;
    use lodestone_v1_17::PROTOCOL_1_17_1;

    let states = parse_dump(DUMP);
    let table = canonical::table_for(PROTOCOL_1_17_1);
    let reverse = canonical_reverse_index();

    let mut checked = 0;
    for probe in [
        "minecraft:diamond_block",
        "minecraft:bedrock",
        "minecraft:dirt",
        "minecraft:calcite",
        "minecraft:amethyst_block",
        "minecraft:copper_block",
    ] {
        let Some(state) = states
            .iter()
            .find(|s| s.name == probe && s.properties.is_empty())
        else {
            panic!("{probe} has no property-free state in the dump");
        };
        let expected = *reverse
            .get(&(probe.to_owned(), Vec::new()))
            .unwrap_or_else(|| panic!("{probe} is absent from the 26.2 registry"));
        assert_eq!(
            table.resolve(state.id),
            Some(block_states::StateId::new(expected).expect("oracle id is canonical")),
            "{probe}: wire state {} must map to 26.2 state {expected}",
            state.id
        );
        checked += 1;
    }
    assert_eq!(checked, 6, "every probe must have run");

    // The point of the table: at least one of those wire ids means a
    // different block if left untranslated. Asserted rather than assumed.
    let diamond = states
        .iter()
        .find(|s| s.name == "minecraft:diamond_block")
        .expect("diamond_block is in the dump");
    assert_ne!(
        block_states::block_name(diamond.id),
        Some("minecraft:diamond_block"),
        "the probe is only discriminating if the untranslated wire id names something else"
    );
}
