//! Generator + drift guard for this era's flat block-state id -> canonical
//! 26.2 block-state id table (`src/generated/canonical.rs`), plus hermetic,
//! hardcoded-value tests that run on every `cargo test`. Modelled on the
//! generate-or-assert pattern `crates/lodestone-data/tests/block_states.rs`
//! and the other post-Flattening eras use.
//!
//! # One protocol, one table
//!
//! This era serves a single protocol (766, covering Minecraft 1.20.5 and
//! 1.20.6), so there is no question of whether one table can be shared
//! across protocols: the era's global palette is its own, and
//! [`the_committed_dump_is_the_one_the_jar_produces`] pins the dump's own
//! content hash and its shape so a different version's dump swapped in under
//! the same filename fails loudly rather than silently regenerating a wrong
//! table.
//!
//! # Data provenance
//!
//! `tests/support/blocks_1_20_6_jar.json` is **not** a community dataset: it
//! is the unmodified output of the server jar's own data generator, run in
//! its reports mode against the real `.cache/mc/1.20.6/server.jar` under
//! Apple `container` with a JDK 21 image — the same tool and report shape
//! `crates/lodestone-data/tests/block_states.rs` reads for the 26.2 side.
//! Every state lists its own `id` and `properties` explicitly, so no
//! combinatorial re-derivation of the state-id numbering is needed on either
//! side.
//!
//! The bridge from a source `(name, properties)` pair to a 26.2 state id
//! reuses the technique `lodestone_canonical::canonical` already uses for the
//! pre-Flattening families: build a `(name, properties) -> 26.2 id` reverse
//! index from `lodestone_data::block_states` (itself jar-derived, not this
//! crate's own encoder/decoder), try a direct match, then a small
//! hand-verified rename table.
//!
//! What this era needs is much smaller than the older eras': of its 26,684
//! states, 26,678 match 26.2 by name and property set exactly, and the
//! remaining 6 are one block renamed after this version ([`bridge_name`]).
//! Nothing needs a property fallback, because no block gained or narrowed a
//! property between this palette and 26.2 without also being renamed —
//! established by diffing the two registries, not assumed. There is
//! deliberately no generic "add the missing property" fallback here for that
//! reason: an unbridgeable state must reach the panic in [`generate`], which
//! names it, rather than be quietly defaulted.
//!
//! # Refreshing after the source jar changes
//!
//! 1. Re-run the data generator against the server jar under Apple
//!    `container` (see `docs/oracles-and-benchmarks.md`). This version ships
//!    a bundler jar, so the generator is selected through the bundler's own
//!    main-class property, and it needs a Java 21 runtime.
//!
//! 2. Copy `generated/reports/blocks.json` over
//!    `tests/support/blocks_1_20_6_jar.json`, and update the shape and hash
//!    pinned in [`the_committed_dump_is_the_one_the_jar_produces`].
//!
//! 3. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-v1-20-6 --test canonicalisation \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```
//!
//! If the 26.2 registry itself changed (`lodestone-data` regenerated), rerun
//! step 3 alone — no source-side dump needs to change.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::block_states;

/// The committed jar dump this era's table is generated from.
const DUMP: &str = include_str!("support/blocks_1_20_6_jar.json");
/// That dump's own filename, for the generated header's provenance line.
const DUMP_FILE: &str = "blocks_1_20_6_jar.json";
/// Path under `src/generated/` the rendered table is committed at.
const COMMITTED: &str = "src/generated/canonical.rs";
/// Minecraft version the dump came from.
const MINECRAFT: &str = "1.20.6";
/// Number of block states in this era's global palette, counted from the
/// dump.
const STATES: usize = 26_684;
/// Number of blocks in this era's block registry, counted from the dump.
const BLOCKS: usize = 1_060;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `(block name, sorted (key, value) properties) -> 26.2 state id`, the shape
/// [`canonical_reverse_index`] builds and every bridging step reads.
type ReverseIndex = HashMap<(String, Vec<(String, String)>), u32>;

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
fn canonical_reverse_index() -> ReverseIndex {
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

/// Renames a 1.20.6 block name to its 26.2 name.
///
/// The whole rename set between this palette and 26.2 is one entry, obtained
/// by diffing the two registries' name sets: `minecraft:chain` is the only
/// name in this era's 1,060 blocks that resolves to nothing in 26.2, and
/// every other block matches by name and property set directly.
///
/// Note what is *absent*. The pre-1.20.3 renames the older eras need —
/// `grass` -> `short_grass`, `grass_path` -> `dirt_path`, the cauldron
/// split — are all rules this era does not: this dump already carries the
/// modern names, checked by looking them up in it rather than inferred from
/// the release they landed in.
fn bridge_name(name: &str) -> &str {
    match name {
        // The plain, uncoloured chain became one metal variant among several
        // when copper chains and their weathering stages joined the registry.
        // Its `axis` + `waterlogged` property set carries over unchanged, and
        // 26.2 defines no `minecraft:chain` at all — both asserted by
        // `the_renamed_chain_maps_to_the_iron_one`.
        "minecraft:chain" => "minecraft:iron_chain",
        other => other,
    }
}

/// Bridges one 1.20.6 `(name, properties)` pair to a 26.2 state id: an exact
/// match on the [`bridge_name`]d name and the source property set, or
/// [`None`].
///
/// Deliberately total in its strictness — a source state whose property set
/// 26.2 does not carry verbatim has no answer here, so it surfaces in
/// [`generate`]'s panic instead of being approximated.
fn resolve(name: &str, properties: &[(String, String)], reverse: &ReverseIndex) -> Option<u32> {
    reverse
        .get(&(bridge_name(name).to_owned(), properties.to_vec()))
        .copied()
}

/// Renders the committed table source.
///
/// Panics naming the offending source state if [`resolve`] cannot bridge it:
/// a future occurrence means a jar or registry update introduced a case this
/// generator does not cover, and that must be loud, not silently defaulted to
/// air at generation time.
fn generate(states: &[SourceState], reverse: &ReverseIndex) -> String {
    let air_id = *reverse
        .get(&("minecraft:air".to_owned(), Vec::new()))
        .expect("26.2 registry always defines minecraft:air with no properties");

    let mut mapped = Vec::with_capacity(states.len());
    let mut unmapped = Vec::new();
    for state in states {
        match resolve(&state.name, &state.properties, reverse) {
            Some(id) => mapped.push(id),
            None => {
                unmapped.push(format!(
                    "{} ({}, {:?})",
                    state.id, state.name, state.properties
                ));
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
        "// @generated by `cargo test -p lodestone-v1-20-6 --test canonicalisation -- --ignored`\n",
    );
    let _ = writeln!(
        out,
        "// from tests/support/{DUMP_FILE} (protocol 766 / Minecraft {MINECRAFT}) against"
    );
    out.push_str(
        "// the 26.2 block-state registry (`lodestone_data::block_states`).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated 1.20.6 era (protocol 766) -> canonical 26.2 block-state id table.\n//!\n\
         //! `STATE_TO_CANONICAL[wire_state_id]` is the 26.2\n\
         //! `lodestone_data::block_states` id that wire state carries. Pure rodata,\n\
         //! zero heap; see `src/canonical.rs` for the lookup wrapper.\n\n",
    );
    out.push_str(
        "/// Number of block states in this era's own global palette (source ids are\n\
         /// `0..SOURCE_STATE_COUNT`).\n",
    );
    let _ = writeln!(
        out,
        "pub const SOURCE_STATE_COUNT: u32 = {};\n",
        mapped.len()
    );
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

/// Every mapping class the generator applies is accounted for, by count.
///
/// The census is the evidence behind [`bridge_name`]'s single entry and
/// [`resolve`]'s lack of any property fallback: if a jar or registry update
/// makes a second block need bridging, the direct-match count moves and this
/// fails, rather than the rename table quietly growing a rule nobody counted.
#[test]
#[ignore = "builds the full 26.2 reverse index; run with the drift guard"]
fn the_mapping_classes_have_the_counts_the_generator_claims() {
    let reverse = canonical_reverse_index();
    let states = parse_dump(DUMP);

    let mut direct = 0usize;
    let mut renamed = 0usize;
    let mut unmapped = 0usize;
    for state in &states {
        let props = state.properties.clone();
        if reverse.contains_key(&(state.name.clone(), props.clone())) {
            direct += 1;
        } else if reverse.contains_key(&(bridge_name(&state.name).to_owned(), props)) {
            renamed += 1;
        } else {
            unmapped += 1;
        }
    }

    assert_eq!(direct, 26_678, "states matching 26.2 by name and properties");
    assert_eq!(renamed, 6, "states needing the rename table (the chain block)");
    assert_eq!(unmapped, 0, "states with no canonical 26.2 mapping");
    assert_eq!(direct + renamed + unmapped, STATES);
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
        STATES,
        "{MINECRAFT}'s global palette holds 26,684 states"
    );

    let value: serde_json::Value = serde_json::from_str(DUMP).expect("dump parses");
    assert_eq!(
        value.as_object().expect("object").len(),
        BLOCKS,
        "{MINECRAFT}'s block registry holds 1,060 blocks"
    );

    // FNV-1a over the raw dump bytes: cheap, dependency-free, and enough to
    // separate this dump from any other version's.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in DUMP.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    assert_eq!(
        hash, 0x9bd4_27d8_0723_1534,
        "tests/support/{DUMP_FILE} is not the dump this table was generated from"
    );
}

/// The era's one protocol resolves to a table of the dump's own size.
#[test]
fn the_protocol_resolves_a_table_of_the_dumps_size() {
    use lodestone_v1_20_6::PROTOCOL_1_20_6;
    use lodestone_v1_20_6::canonical;

    let table = canonical::table_for(PROTOCOL_1_20_6);
    assert_eq!(table.state_count() as usize, STATES);
    assert_eq!(
        table.resolve(STATES as u32),
        None,
        "one past the palette names no state"
    );
    // The baked air id is checked against the 26.2 registry rather than
    // against a literal, so a registry regeneration that moves air is a
    // failure here instead of a wrong fallback block at runtime.
    assert_eq!(
        table.air_state_id().name(),
        "minecraft:air",
        "the baked air id must name 26.2's air block"
    );
    assert_eq!(
        table.air_state_id().properties().len(),
        0,
        "26.2's air block carries no properties"
    );
}

/// Discriminating states resolve to their **26.2** ids, not their wire ids.
///
/// Each expected value is looked up in `lodestone_data::block_states` by name
/// and properties — the 26.2 registry, generated from that jar — while the
/// wire id comes from the committed 1.20.6 dump. Neither side is this crate's
/// own table, so the test cannot pass by two symmetric mistakes.
#[test]
fn discriminating_states_resolve_to_their_26_2_ids_not_their_wire_ids() {
    use lodestone_v1_20_6::PROTOCOL_1_20_6;
    use lodestone_v1_20_6::canonical;

    let states = parse_dump(DUMP);
    let table = canonical::table_for(PROTOCOL_1_20_6);
    let reverse = canonical_reverse_index();

    let mut checked = 0;
    for probe in [
        "minecraft:diamond_block",
        "minecraft:bedrock",
        "minecraft:dirt",
        "minecraft:calcite",
        "minecraft:amethyst_block",
        "minecraft:copper_block",
        "minecraft:sculk",
        "minecraft:reinforced_deepslate",
        // Present in this dump and in no era below it: the tuff decoration
        // set, and the ground cover under the name it took when it was
        // disambiguated from the ground block. Their presence is evidence the
        // dump really is this version's rather than an older one's.
        "minecraft:chiseled_tuff",
        "minecraft:polished_tuff",
        "minecraft:short_grass",
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
    assert_eq!(checked, 11, "every probe must have run");

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

/// The one rename [`bridge_name`] carries, checked end to end against both
/// registries.
///
/// Three separate facts, none of them this crate's own table: 26.2 defines no
/// `minecraft:chain`, it defines `minecraft:iron_chain` with the same `axis` +
/// `waterlogged` property set, and every one of the dump's six chain states
/// resolves through the committed table to the matching iron-chain state.
#[test]
fn the_renamed_chain_maps_to_the_iron_one() {
    use lodestone_v1_20_6::PROTOCOL_1_20_6;
    use lodestone_v1_20_6::canonical;

    let reverse = canonical_reverse_index();
    let table = canonical::table_for(PROTOCOL_1_20_6);
    let states = parse_dump(DUMP);

    let chain: Vec<&SourceState> = states
        .iter()
        .filter(|s| s.name == "minecraft:chain")
        .collect();
    assert_eq!(chain.len(), 6, "the dump's chain has six states");

    for state in chain {
        assert!(
            !reverse.contains_key(&("minecraft:chain".to_owned(), state.properties.clone())),
            "26.2 must not define minecraft:chain, or this is not a rename"
        );
        let expected = *reverse
            .get(&(
                "minecraft:iron_chain".to_owned(),
                state.properties.clone(),
            ))
            .unwrap_or_else(|| {
                panic!(
                    "26.2 iron_chain must carry {:?} for the rename to be property-preserving",
                    state.properties
                )
            });
        assert_eq!(
            table.resolve(state.id),
            Some(block_states::StateId::new(expected).expect("oracle id is canonical")),
            "chain wire state {} ({:?}) must map to 26.2 state {expected}",
            state.id,
            state.properties
        );
    }
}
