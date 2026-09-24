//! Generator + drift guard for this era's flat block-state id -> canonical
//! 26.2 block-state id table (`src/generated/canonical.rs`), plus hermetic,
//! hardcoded-value tests that run on every `cargo test`. Modelled on the
//! generate-or-assert pattern `crates/lodestone-data/tests/block_states.rs`
//! and the eras below use.
//!
//! # One protocol, one table
//!
//! This era serves a single protocol, so unlike the multi-version eras below
//! there is no question of whether one table can be shared: 1.19.4's global
//! palette is its own, and [`the_committed_dump_is_the_one_the_jar_produces`]
//! pins the dump's own content hash and its shape so a different version's
//! dump swapped in under the same filename fails loudly rather than silently
//! regenerating a wrong table.
//!
//! # Data provenance
//!
//! `tests/support/blocks_1_19_4_jar.json` is **not** a community dataset: it
//! is the unmodified output of the jar's own data generator, run in its
//! `--reports` mode against the real `.cache/mc/1.19.4/server.jar`, the same
//! tool and report shape `crates/lodestone-data/tests/block_states.rs` reads
//! for the 26.2 side. Every state lists its own `id` and `properties`
//! explicitly, so no combinatorial re-derivation of the state-id numbering is
//! needed on either side.
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
//! # Refreshing after the source jar changes
//!
//! 1. Re-run the data generator against the server jar under Apple
//!    `container` (see `docs/oracles-and-benchmarks.md`). 1.19.4 ships a
//!    bundler jar, so the generator is selected through the bundler's own
//!    main-class property, and it needs a Java 17 runtime rather than the
//!    Java 8 image the pre-1.17 oracles use.
//!
//! 2. Copy `generated/reports/blocks.json` over
//!    `tests/support/blocks_1_19_4_jar.json`.
//!
//! 3. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-v1-19 --test canonicalisation \
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
const DUMP: &str = include_str!("support/blocks_1_19_4_jar.json");
/// That dump's own filename, for the generated header's provenance line.
const DUMP_FILE: &str = "blocks_1_19_4_jar.json";
/// Path under `src/generated/` the rendered table is committed at.
const COMMITTED: &str = "src/generated/canonical.rs";
/// Minecraft version the dump came from.
const MINECRAFT: &str = "1.19.4";

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

/// Renames a 1.19.4 block name to its 26.2 name.
///
/// Each entry is a rename that happened **after** 1.19.4 and is verified
/// against the 26.2 registry: the old name resolves to nothing there and the
/// new one carries the same property set. Note what is *absent* — `grass_path`
/// -> `dirt_path` and the cauldron split are rules the 1.14 era needs and this
/// one does not, because both landed before 1.19 and the dump already carries
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

/// Bridges one 1.19.4 `(name, properties)` pair to a 26.2 state id. Order:
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
    // 1.19.4, because the concept did not exist for them yet — not "unknown,
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
    // 1.19.4 has no redstone-signal concept for skulls at all.
    let mut with_powered = props.clone();
    insert_sorted(&mut with_powered, "powered", "false");
    if let Some(&id) = reverse.get(&(bridged_name.clone(), with_powered)) {
        return Some(id);
    }

    // The decorated pot gained a `cracked` property in 1.20; every 1.19.4
    // pot is uncracked, because the concept did not exist for it yet. Same
    // justification as the two above — checked against the 26.2 registry,
    // where the block's own default for this property is `false`.
    let mut with_cracked = props.clone();
    insert_sorted(&mut with_cracked, "cracked", "false");
    if let Some(&id) = reverse.get(&(bridged_name.clone(), with_cracked)) {
        return Some(id);
    }

    // Both of the first two fallbacks together, for a block that needs each.
    let mut with_both = props.clone();
    insert_sorted(&mut with_both, "powered", "false");
    insert_sorted(&mut with_both, "waterlogged", "false");
    if let Some(&id) = reverse.get(&(bridged_name.clone(), with_both)) {
        return Some(id);
    }

    // The one property this era has that 26.2 has *narrowed* rather than
    // widened, and therefore the one case a "add the missing property"
    // fallback cannot cover. 1.19.4's torchflower crop has three growth
    // stages (`age` 0..=2); 26.2 has two (0..=1). Stage 2 is fully grown in
    // both registries — checked by reading 26.2's own property list, which
    // stops at 1 — so the mature state maps to the mature state and the two
    // immature ones already matched directly above.
    if bridged_name == "minecraft:torchflower_crop"
        && props.as_slice() == [("age".to_owned(), "2".to_owned())]
    {
        return reverse
            .get(&(bridged_name, vec![("age".to_owned(), "1".to_owned())]))
            .copied();
    }

    None
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
    let mut unmapped = Vec::new();
    for state in states {
        match resolve(&state.name, &state.properties, reverse) {
            Some(id) => mapped.push(id),
            None => {
                unmapped.push(format!("{} ({}, {:?})", state.id, state.name, state.properties));
                mapped.push(0);
            }
        }
    }
    assert!(
        unmapped.is_empty(),
        "{} {MINECRAFT} states have no canonical 26.2 mapping: {unmapped:#?}",
        unmapped.len()
    );

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-v1-19 --test canonicalisation -- --ignored`\n",
    );
    let _ = writeln!(
        out,
        "// from tests/support/{DUMP_FILE} (protocol 762 / Minecraft {MINECRAFT}) against"
    );
    out.push_str(
        "// the 26.2 block-state registry (`lodestone_data::block_states`).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated 1.19 era (protocol 762) -> canonical 26.2 block-state id table.\n//!\n\
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

/// The committed dump is the one the jar produces, by shape and by content.
///
/// The content check is a hash of the dump's own bytes, recorded when the
/// generator was run. It is not a checksum of the table under test — it pins
/// the *input*, so an unrelated dump swapped in under the same filename fails
/// here rather than silently regenerating a wrong table.
#[test]
fn the_committed_dump_is_the_one_the_jar_produces() {
    let states = parse_dump(DUMP);
    assert_eq!(
        states.len(),
        23725,
        "1.19.4's global palette holds 23,725 states"
    );

    let value: serde_json::Value = serde_json::from_str(DUMP).expect("dump parses");
    assert_eq!(
        value.as_object().expect("object").len(),
        998,
        "1.19.4's block registry holds 998 blocks"
    );

    // FNV-1a over the raw dump bytes: cheap, dependency-free, and enough to
    // separate this dump from any other version's.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in DUMP.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    assert_eq!(
        hash, 0x0d1b_866a_7a58_bb70,
        "tests/support/{DUMP_FILE} is not the dump this table was generated from"
    );
}

/// The era's one protocol resolves to a table of the dump's own size.
#[test]
fn the_protocol_resolves_a_table_of_the_dumps_size() {
    use lodestone_v1_19::PROTOCOL_1_19_4;
    use lodestone_v1_19::canonical;

    let table = canonical::table_for(PROTOCOL_1_19_4);
    assert_eq!(table.state_count(), 23725);
    assert_eq!(
        table.resolve(23725),
        None,
        "one past the palette names no state"
    );
}

/// Discriminating states resolve to their **26.2** ids, not their wire ids.
///
/// Each expected value is looked up in `lodestone_data::block_states` by name
/// and properties — the 26.2 registry, generated from that jar — while the
/// wire id comes from the committed 1.19.4 dump. Neither side is this crate's
/// own table, so the test cannot pass by two symmetric mistakes.
#[test]
fn discriminating_states_resolve_to_their_26_2_ids_not_their_wire_ids() {
    use lodestone_v1_19::PROTOCOL_1_19_4;
    use lodestone_v1_19::canonical;

    let states = parse_dump(DUMP);
    let table = canonical::table_for(PROTOCOL_1_19_4);
    let reverse = canonical_reverse_index();

    let mut checked = 0;
    for probe in [
        "minecraft:diamond_block",
        "minecraft:bedrock",
        "minecraft:dirt",
        "minecraft:calcite",
        "minecraft:amethyst_block",
        "minecraft:copper_block",
        // Added in 1.19 itself — a state no era below this one can carry, so
        // its presence is evidence the dump really is this version's.
        "minecraft:sculk",
        "minecraft:reinforced_deepslate",
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
    assert_eq!(checked, 8, "every probe must have run");

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
