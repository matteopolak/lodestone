//! Generator + drift guard for this era's flat block-state id -> canonical
//! 26.2 block-state id table (`src/generated/canonical.rs`), plus hermetic,
//! hardcoded-value tests that run on every `cargo test`. Modelled on the
//! generate-or-assert pattern `crates/lodestone-data/tests/block_states.rs`
//! and the other post-Flattening eras use.
//!
//! # One protocol, one table
//!
//! This era serves a single protocol (774, covering Minecraft 1.21.11), so
//! there is no question of whether one table can be shared across protocols.
//! The dump is still pinned by content hash so a different version's dump
//! swapped in under the same filename fails loudly rather than silently
//! regenerating a wrong table.
//!
//! # Data provenance
//!
//! `tests/support/blocks_1_21_11_jar.json` is **not** a community dataset: it
//! is the unmodified output of the server jar's own data generator, run in its
//! reports mode against the real `.cache/mc/1.21.11/server.jar` under Apple
//! `container` with a JDK 21 image — the same tool and report shape
//! `crates/lodestone-data/tests/block_states.rs` reads for the 26.2 side.
//! Every state lists its own `id` and `properties` explicitly, so no
//! combinatorial re-derivation of the state-id numbering is needed on either
//! side.
//!
//! The bridge from a source `(name, properties)` pair to a 26.2 state id
//! builds a `(name, properties) -> 26.2 id` reverse index from
//! `lodestone_data::block_states` (itself jar-derived, not this crate's own
//! encoder/decoder) and looks each source state up in it.
//!
//! # Why there is no rename table here at all
//!
//! Every one of this era's 29,671 states matches a 26.2 state by name and
//! property set exactly. That is a *measurement*, not an assumption — the
//! census in [`the_mapping_classes_have_the_counts_the_generator_claims`]
//! counts the direct matches and the misses separately, and
//! [`the_reverse_index_rejects_a_name_26_2_does_not_carry`] is its control:
//! it shows the index really can miss, so a direct-match count equal to the
//! state count means something.
//!
//! The table is still necessary. The two palettes have different *sizes*
//! (29,671 against 26.2's 32,366), so the same name occupies a different flat
//! id in each, and an untranslated wire id names a different block — asserted
//! in [`discriminating_states_resolve_to_their_26_2_ids_not_their_wire_ids`].
//!
//! # Refreshing after the source jar changes
//!
//! 1. Re-run the data generator against the server jar under Apple
//!    `container` (see `docs/oracles-and-benchmarks.md`). This version ships a
//!    bundler jar, so the generator is selected through the bundler's own
//!    main-class property, and it needs a Java 21 runtime.
//!
//! 2. Copy `generated/reports/blocks.json` over
//!    `tests/support/blocks_1_21_11_jar.json`, and update the shape and hash
//!    pinned in [`the_committed_dump_is_the_one_the_jar_produces`].
//!
//! 3. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-v1-21-11 --test canonicalisation \
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
const DUMP: &str = include_str!("support/blocks_1_21_11_jar.json");
/// That dump's own filename, for the generated header's provenance line.
const DUMP_FILE: &str = "blocks_1_21_11_jar.json";
/// Path under `src/generated/` the rendered table is committed at.
const COMMITTED: &str = "src/generated/canonical.rs";
/// Minecraft version the dump came from.
const MINECRAFT: &str = "1.21.11";
/// Number of block states in this era's global palette, counted from the
/// dump.
const STATES: usize = 29_671;
/// Number of blocks in this era's block registry, counted from the dump.
const BLOCKS: usize = 1_166;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `(block name, sorted (key, value) properties) -> 26.2 state id`, the shape
/// [`canonical_reverse_index`] builds and every lookup reads.
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
                                value
                                    .as_str()
                                    .expect("property value is a string")
                                    .to_owned(),
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

/// Bridges one source `(name, properties)` pair to a 26.2 state id, or
/// [`None`].
///
/// Deliberately total in its strictness — a source state whose name or
/// property set 26.2 does not carry verbatim has no answer here, so it
/// surfaces in [`generate`]'s panic instead of being approximated.
fn resolve(name: &str, properties: &[(String, String)], reverse: &ReverseIndex) -> Option<u32> {
    reverse.get(&(name.to_owned(), properties.to_vec())).copied()
}

/// Renders the committed table source.
///
/// Panics naming the offending source state if [`resolve`] cannot bridge it: a
/// future occurrence means a jar or registry update introduced a case this
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
        "// @generated by `cargo test -p lodestone-v1-21-11 --test canonicalisation -- --ignored`\n",
    );
    let _ = writeln!(
        out,
        "// from tests/support/{DUMP_FILE} (protocol 774 / Minecraft {MINECRAFT}) against"
    );
    out.push_str(
        "// the 26.2 block-state registry (`lodestone_data::block_states`).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated 1.21.11 era (protocol 774) -> canonical 26.2 block-state id table.\n//!\n\
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
/// The census is the evidence behind [`resolve`]'s lack of any rename table or
/// property fallback: if a jar or registry update makes one block need
/// bridging, the direct-match count moves and this fails, rather than a
/// silently-defaulted state reaching the table.
#[test]
#[ignore = "builds the full 26.2 reverse index; run with the drift guard"]
fn the_mapping_classes_have_the_counts_the_generator_claims() {
    let reverse = canonical_reverse_index();
    let states = parse_dump(DUMP);

    let mut direct = 0usize;
    let mut unmapped = 0usize;
    for state in &states {
        if reverse.contains_key(&(state.name.clone(), state.properties.clone())) {
            direct += 1;
        } else {
            unmapped += 1;
        }
    }

    assert_eq!(direct, 29_671, "states matching 26.2 by name and properties");
    assert_eq!(unmapped, 0, "states with no canonical 26.2 mapping");
    assert_eq!(direct + unmapped, STATES);
}

/// The control for the census above: the reverse index really can miss.
///
/// Without this, "every state matched" is equally consistent with a lookup
/// that answers for anything. The two probes are a name 26.2 does not carry
/// and a real name with a property set it does not carry.
#[test]
#[ignore = "builds the full 26.2 reverse index; run with the drift guard"]
fn the_reverse_index_rejects_a_name_26_2_does_not_carry() {
    let reverse = canonical_reverse_index();
    assert_eq!(
        resolve("minecraft:not_a_real_block", &[], &reverse),
        None,
        "a fabricated block name must not resolve"
    );
    assert_eq!(
        resolve(
            "minecraft:air",
            &[("axis".to_owned(), "y".to_owned())],
            &reverse
        ),
        None,
        "a real block with a property set it does not carry must not resolve"
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
        STATES,
        "{MINECRAFT}'s global palette holds 29,671 states"
    );

    let value: serde_json::Value = serde_json::from_str(DUMP).expect("dump parses");
    assert_eq!(
        value.as_object().expect("object").len(),
        BLOCKS,
        "{MINECRAFT}'s block registry holds 1,166 blocks"
    );

    // FNV-1a over the raw dump bytes: cheap, dependency-free, and enough to
    // separate this dump from any other version's.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in DUMP.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    assert_eq!(
        hash, 0xeb7b_897b_211f_65db,
        "tests/support/{DUMP_FILE} is not the dump this table was generated from"
    );
}

/// The era's one protocol resolves to a table of the dump's own size.
#[test]
fn the_protocol_resolves_a_table_of_the_dumps_size() {
    use lodestone_v1_21_11::PROTOCOL_1_21_11;
    use lodestone_v1_21_11::canonical;

    let table = canonical::table_for(PROTOCOL_1_21_11);
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

/// The two palettes really are differently sized, which is the whole reason
/// the table exists.
///
/// Both numbers come from outside this crate: one from the committed jar dump,
/// the other from `lodestone_data`'s own 26.2 registry.
#[test]
fn the_two_palettes_are_differently_sized() {
    assert_eq!(parse_dump(DUMP).len(), STATES);
    assert_ne!(
        block_states::STATE_COUNT as usize,
        STATES,
        "a same-sized palette would still need checking, but the id spaces would coincide \
         and every probe below would stop discriminating"
    );
}

/// Discriminating states resolve to their **26.2** ids, not their wire ids.
///
/// Each expected value is looked up in `lodestone_data::block_states` by name
/// and properties — the 26.2 registry, generated from that jar — while the
/// wire id comes from the committed 1.21.11 dump. Neither side is this crate's
/// own table, so the test cannot pass by two symmetric mistakes.
#[test]
fn discriminating_states_resolve_to_their_26_2_ids_not_their_wire_ids() {
    use lodestone_v1_21_11::PROTOCOL_1_21_11;
    use lodestone_v1_21_11::canonical;

    let states = parse_dump(DUMP);
    let table = canonical::table_for(PROTOCOL_1_21_11);
    let reverse = canonical_reverse_index_from_probes();

    let mut checked = 0;
    for probe in [
        "minecraft:diamond_block",
        "minecraft:bedrock",
        "minecraft:dirt",
        "minecraft:calcite",
        "minecraft:sculk",
        "minecraft:reinforced_deepslate",
        // Present in this dump and in no era below it: the resin set and the
        // pale-garden ground cover. Their presence is evidence the dump really
        // is this version's rather than an older one's.
        "minecraft:resin_block",
        "minecraft:resin_bricks",
        "minecraft:chiseled_resin_bricks",
        "minecraft:pale_moss_block",
    ] {
        let Some(state) = states
            .iter()
            .find(|s| s.name == probe && s.properties.is_empty())
        else {
            panic!("{probe} has no property-free state in the dump");
        };
        let expected = *reverse
            .get(probe)
            .unwrap_or_else(|| panic!("{probe} is absent from the 26.2 registry"));
        assert_eq!(
            table.resolve(state.id),
            Some(block_states::StateId::new(expected).expect("oracle id is canonical")),
            "{probe}: wire state {} must map to 26.2 state {expected}",
            state.id
        );
        checked += 1;
    }
    assert_eq!(checked, 10, "every probe must have run");

    // The point of the table: at least one of those wire ids means a different
    // block if left untranslated. Asserted rather than assumed.
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

/// `name -> 26.2 state id` for property-free blocks only.
///
/// A narrower index than [`canonical_reverse_index`] on purpose: the probe
/// test above only needs property-free blocks, and scanning for those is a
/// fraction of the cost of building the whole reverse map, which keeps the
/// probe test out of the `#[ignore]`d set.
fn canonical_reverse_index_from_probes() -> HashMap<&'static str, u32> {
    let mut index = HashMap::new();
    for id in 0..block_states::STATE_COUNT {
        if block_states::properties(id).is_some_and(<[_]>::is_empty) {
            if let Some(name) = block_states::block_name(id) {
                index.entry(name).or_insert(id);
            }
        }
    }
    index
}
