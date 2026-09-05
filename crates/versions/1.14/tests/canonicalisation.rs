//! Generator + drift guard for this era's three flat block-state id ->
//! canonical 26.2 block-state id tables (`src/generated/canonical{,_498,_578}.rs`),
//! plus hermetic, hardcoded-value tests that run on every `cargo test`.
//! Modelled directly on `crates/lodestone-data/tests/block_states.rs` and
//! `crates/lodestone-canonical/tests/flattening.rs`'s generate-or-assert
//! pattern.
//!
//! # Why three tables and not one
//!
//! Each release inserts blocks into vanilla's global palette, so the same
//! numeric flat state id names a different block in each of the era's three
//! protocols: the dumps below carry 11,271 states at 498, 11,337 at 578 and
//! 17,112 at 754. A shared table would be the silent wrong-terrain defect
//! `src/canonical.rs` exists to prevent, one protocol removed.
//!
//! # Data provenance
//!
//! `tests/support/blocks_1_{14_4,15_2,16_5}_jar.json` are **not** a community
//! dataset — each is the unmodified output of Mojang's own data generator, run
//! in its `--reports` mode against the real `.cache/mc/<version>/server.jar`,
//! the same tool and report shape
//! `crates/lodestone-data/tests/block_states.rs` reads for the 26.2 side
//! (`.cache/mc/26.2/generated/reports/blocks.json`). Every state lists its
//! own `id` and `properties` explicitly, so no combinatorial re-derivation
//! of vanilla's state-id numbering is needed on either side.
//!
//! The bridge from a source `(name, properties)` pair to a 26.2 state id
//! reuses exactly the technique `lodestone_canonical::canonical` already
//! uses for the pre-Flattening families: build a `(name, properties) ->
//! 26.2 id` reverse index from `lodestone_data::block_states` (itself
//! jar-derived, not this crate's own encoder/decoder), try a direct match,
//! then a small hand-verified rename table, then a generic fallback that
//! appends a boolean property the source version does not yet have. Every
//! rule below was checked against all three corpora: after applying them,
//! zero states are left unmapped in any of them (see
//! `committed_tables_match_dumps`'s assertion).
//!
//! Two rules the pre-1.16 protocols need and 754 does not — walls' four side
//! properties turning from booleans into a three-valued enum, and the jigsaw
//! block's `facing` becoming `orientation` — are not read off any table.
//! They are what vanilla itself produced when a real 1.15.2 world carrying
//! those exact states was booted under the real 26.2 server jar and read
//! back over RCON; the probes, the procedure and the answers are committed
//! verbatim in `tests/support/state_upgrade_1_15_2_to_26_2.txt`. Without
//! them 902 of each pre-1.16 dump's states have no mapping at all.
//!
//! Unlike the pre-Flattening bridge, each is baked into a flat generated
//! `[u32; SOURCE_STATE_COUNT]` array **at regeneration time** rather than
//! resolved lazily at runtime: no protocol in this era has ambiguous
//! "requires additional context" cases (that was a pre-Flattening `id:meta`
//! problem — a flat state id already carries full block identity), so there
//! is nothing left for the runtime path to compute. `src/canonical.rs` is
//! therefore just an `O(1)` index into the negotiated protocol's table plus
//! the wire-corruption fallback (a state id past that table's
//! `SOURCE_STATE_COUNT`, which no real server of that version sends).
//!
//! # Refreshing after a source jar changes
//!
//! 1. Re-run the data generator against the (possibly updated) server jar
//!    for the version in question under Apple `container` (see
//!    `docs/oracles-and-benchmarks.md`):
//!
//! ```text
//! container run --rm --memory 3g \
//!     -v "$PWD/.cache/mc/<version>/server.jar:/server.jar:ro" \
//!     -v "$PWD/out:/out" \
//!     eclipse-temurin:8-jdk \
//!     java -cp /server.jar <vanilla data-generator entry point> --reports --output /out
//! ```
//!
//! (the entry point is documented in Mojang's own server-jar usage notes,
//! not reproduced here)
//!
//!    then copy `out/reports/blocks.json` over that version's
//!    `tests/support/blocks_*_jar.json`.
//!
//! 2. Regenerate the committed tables:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-v1-14 --test canonicalisation \
//!     committed_tables_match_dumps -- --ignored --nocapture
//! ```
//!
//! If the 26.2 registry itself changed (`lodestone-data` regenerated), rerun
//! step 2 alone — no source-side dump needs to change.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::block_states;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// One protocol's source of truth: the committed jar-generated report — an
/// external anchor, not gitignored, exactly like `lodestone-canonical`'s
/// `flattening_1_13_2_jvm.txt` — and where its table is committed.
struct Source {
    /// Minecraft version the dump came from, for messages and headers.
    minecraft: &'static str,
    /// Protocol this table serves.
    protocol: i32,
    /// The unmodified `--reports` `blocks.json` for that jar.
    dump: &'static str,
    /// That dump's own filename under `tests/support/`, for the generated
    /// header's provenance line.
    dump_file: &'static str,
    /// Path under `src/generated/` the rendered table is committed at.
    committed: &'static str,
}

const SOURCES: &[Source] = &[
    Source {
        minecraft: "1.14.4",
        protocol: 498,
        dump: include_str!("support/blocks_1_14_4_jar.json"),
        dump_file: "blocks_1_14_4_jar.json",
        committed: "src/generated/canonical_498.rs",
    },
    Source {
        minecraft: "1.15.2",
        protocol: 578,
        dump: include_str!("support/blocks_1_15_2_jar.json"),
        dump_file: "blocks_1_15_2_jar.json",
        committed: "src/generated/canonical_578.rs",
    },
    Source {
        minecraft: "1.16.5",
        protocol: 754,
        dump: include_str!("support/blocks_1_16_5_jar.json"),
        dump_file: "blocks_1_16_5_jar.json",
        committed: "src/generated/canonical.rs",
    },
];

/// One source-version state: its flat wire id and sorted `(key, value)`
/// properties.
struct SourceState {
    id: u32,
    name: String,
    properties: Vec<(String, String)>,
}

/// Parses one `tests/support/blocks_*_jar.json` into one entry per state id,
/// indexed `0..SOURCE_STATE_COUNT` (the report's own per-state `id` field is
/// authoritative; this asserts density rather than assuming it).
fn parse_dump(minecraft: &str, doc: &str) -> Vec<SourceState> {
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
            assert!(previous.is_none(), "duplicate {minecraft} state id {id}");
        }
    }

    let count = by_id.len();
    assert_eq!(
        max_id as usize + 1,
        count,
        "{minecraft} state ids are not dense: max id {max_id}, count {count}"
    );

    let mut states: Vec<SourceState> = by_id.into_values().collect();
    states.sort_by_key(|s| s.id);
    states
}

/// `(name, sorted properties) -> 26.2 state id`, built once from
/// `lodestone_data::block_states` — the same construction
/// `lodestone_canonical::canonical::canonical_reverse_index` uses.
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

/// Renames a 1.16.5 block name to its 26.2 name, for the three families this
/// pass found renamed between the two versions (verified against the 26.2
/// registry, not jar-traced — same standard of evidence
/// `lodestone_canonical::canonical::bridge_name` uses for its later-than-1.13.2
/// entries).
fn bridge_name(name: &str) -> &str {
    match name {
        // Renamed `grass` -> `short_grass` in 1.20.3 to disambiguate from the
        // ground block (identical entry already exists in
        // `lodestone_canonical::canonical::bridge_name` for the pre-1.13
        // families; 1.16.5 hits the same rename).
        "minecraft:grass" => "minecraft:short_grass",
        // Renamed `grass_path` -> `dirt_path` in 1.17.
        "minecraft:grass_path" => "minecraft:dirt_path",
        // The plain (uncolored, non-copper) chain was renamed `chain` ->
        // `iron_chain` when 1.21's copper chains were added; `axis` +
        // `waterlogged` carry over unchanged.
        "minecraft:chain" => "minecraft:iron_chain",
        other => other,
    }
}

/// Bridges one 1.16.5 `(name, properties)` pair to a 26.2 state id. Order:
/// direct match, then [`bridge_name`] alone, then two single-property
/// fallbacks — each tried only if the exact match fails, so neither can
/// override a block that already resolves without it.
fn resolve(
    name: &str,
    properties: &[(String, String)],
    reverse: &HashMap<(String, Vec<(String, String)>), u32>,
) -> Option<u32> {
    // 1.16.5's `cauldron` already carries a `level` property (0-3), unlike
    // the pre-1.13 single-level block, but 26.2 still splits it by identity:
    // level=0 is bare `cauldron` (no properties), level 1-3 is
    // `water_cauldron` with `level` unchanged. Same split
    // `lodestone_canonical::canonical::bridge`'s cauldron arm performs for
    // the pre-1.13 case, and independently confirmed here against the 26.2
    // registry: `water_cauldron` states are exactly `level in 1..=3`.
    if name == "minecraft:cauldron" {
        let level = properties
            .iter()
            .find(|(key, _)| key == "level")
            .map_or("0", |(_, value)| value.as_str());
        return if level == "0" {
            reverse.get(&("minecraft:cauldron".to_owned(), Vec::new())).copied()
        } else {
            reverse
                .get(&(
                    "minecraft:water_cauldron".to_owned(),
                    vec![("level".to_owned(), level.to_owned())],
                ))
                .copied()
        };
    }

    // The jigsaw block's single `facing` (6 values) became `orientation`
    // (12) in 1.16, so a pre-1.16 state has no direct match at all. The
    // mapping is not read off a table: it is what vanilla itself produced
    // when a 1.15.2 world carrying all six `facing` values was upgraded by
    // the real 26.2 server jar (`tests/support/state_upgrade_1_15_2_to_26_2.txt`).
    // Guarded on `facing` being present, so 1.16.5 — which already carries
    // `orientation` — never enters this arm.
    if name == "minecraft:jigsaw"
        && let Some((_, facing)) = properties.iter().find(|(key, _)| key == "facing")
    {
        let orientation = match facing.as_str() {
            "north" => "north_up",
            "south" => "south_up",
            "east" => "east_up",
            "west" => "west_up",
            "up" => "up_north",
            "down" => "down_south",
            other => panic!("jigsaw facing {other:?} is outside the six the oracle covers"),
        };
        return reverse
            .get(&(
                "minecraft:jigsaw".to_owned(),
                vec![("orientation".to_owned(), orientation.to_owned())],
            ))
            .copied();
    }

    let bridged_name = bridge_name(name).to_owned();
    let mut props = properties.to_vec();

    // A wall's four side properties were booleans before 1.16 and a
    // three-valued `none`/`low`/`tall` from 1.16 on. Same oracle as the
    // jigsaw arm above, and the same guard: 1.16.5's values are already
    // `none`/`low`/`tall`, which `wall_side` leaves alone, so 754's rendered
    // table is byte-for-byte what it was before this arm existed. `tall` is
    // a state the pre-1.16 wire cannot express and vanilla never produces
    // from one.
    if bridged_name.ends_with("_wall") {
        for (key, value) in &mut props {
            if matches!(key.as_str(), "east" | "north" | "south" | "west")
                && let Some(mapped) = wall_side(value)
            {
                *value = mapped.to_owned();
            }
        }
    }
    let props = props;

    if let Some(&id) = reverse.get(&(bridged_name.clone(), props.clone())) {
        return Some(id);
    }

    // Leaves, rails (all four) and barrier gained a `waterlogged` property
    // after 1.16.5. Confirmed against the decompiled 26.2 source
    // (`LeavesBlock`/`BaseRailBlock`/`BarrierBlock` all
    // `registerDefaultState(...WATERLOGGED, false)`): every one of these
    // blocks is unambiguously not waterlogged in 1.16.5, because the concept
    // did not exist for them yet — not "unknown, assume false". Same
    // justification `lodestone_canonical::canonical::resolve_canonical`
    // already documents for its own generic `waterlogged=false` fallback.
    let mut with_waterlogged = props.clone();
    insert_sorted(&mut with_waterlogged, "waterlogged", "false");
    if let Some(&id) = reverse.get(&(bridged_name.clone(), with_waterlogged)) {
        return Some(id);
    }

    // Every mob-head/skull block gained a `powered` redstone-signal property
    // after 1.16.5. Confirmed against the decompiled 26.2 source, where the
    // skull base class's own default-state registration sets `POWERED` to
    // `false`: that is the block's own registry default, and 1.16.5 has no
    // redstone-signal concept for skulls at all, so this is exact rather
    // than a guess.
    let mut with_powered = props;
    insert_sorted(&mut with_powered, "powered", "false");
    reverse.get(&(bridged_name, with_powered)).copied()
}

/// Maps one pre-1.16 boolean wall-side value to its 1.16+ enum value, or
/// [`None`] for a value that is already an enum member (every 1.16.5 state,
/// which is what keeps this a no-op there).
///
/// Both answers come from the committed world-upgrade oracle, not from a
/// reading of any table: see this module's provenance section.
fn wall_side(value: &str) -> Option<&'static str> {
    match value {
        "true" => Some("low"),
        "false" => Some("none"),
        _ => None,
    }
}

/// Inserts `(key, value)` into `properties`, kept sorted by key.
fn insert_sorted(properties: &mut Vec<(String, String)>, key: &str, value: &str) {
    let position = properties
        .iter()
        .position(|(existing, _)| existing.as_str() > key)
        .unwrap_or(properties.len());
    properties.insert(position, (key.to_owned(), value.to_owned()));
}

/// Renders one protocol's committed `src/generated/canonical*.rs` source from
/// its parsed dump, given the 26.2 reverse index (supplied by the caller so
/// rendering all three tables in one run builds that 32,366-entry index once
/// rather than three times).
///
/// Panics naming the offending source state if [`resolve`] cannot bridge it —
/// this pass found zero such states across all three corpora (see module
/// docs), so a future occurrence means a jar update introduced a case this
/// generator's bridging does not cover yet, and that must be loud, not
/// silently defaulted to air at generation time.
fn generate_with(
    source: &Source,
    states: &[SourceState],
    reverse: &HashMap<(String, Vec<(String, String)>), u32>,
) -> String {
    let Source {
        minecraft,
        protocol,
        dump_file,
        ..
    } = *source;

    let air_id = *reverse
        .get(&("minecraft:air".to_owned(), Vec::new()))
        .expect("26.2 registry always defines minecraft:air with no properties");

    let mut mapped = Vec::with_capacity(states.len());
    for state in states {
        let Some(id) = resolve(&state.name, &state.properties, reverse) else {
            panic!(
                "{minecraft} state {} ({}, {:?}) has no canonical 26.2 mapping",
                state.id, state.name, state.properties
            );
        };
        mapped.push(id);
    }

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-v1-14 --test canonicalisation -- --ignored`\n",
    );
    let _ = writeln!(
        out,
        "// from tests/support/{dump_file} (protocol {protocol} / Minecraft {minecraft}) against"
    );
    out.push_str(
        "// the 26.2 block-state registry (`lodestone_data::block_states`).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    let _ = writeln!(
        out,
        "//! Generated {minecraft} (protocol {protocol}) -> canonical 26.2 block-state id table.\n//!\n\
         //! `STATE_TO_CANONICAL[wire_state_id]` is the 26.2\n\
         //! `lodestone_data::block_states` id that {minecraft} wire state carries. Pure\n\
         //! rodata, zero heap; see `src/canonical.rs` for the lookup wrapper.\n"
    );

    let _ = writeln!(
        out,
        "/// Number of block states in {minecraft}'s own global palette (source ids are\n\
         /// `0..SOURCE_STATE_COUNT`)."
    );
    let _ = writeln!(out, "pub const SOURCE_STATE_COUNT: u32 = {};\n", mapped.len());

    let _ = writeln!(
        out,
        "/// The canonical 26.2 `minecraft:air` state id, baked at generation time\n\
         /// exactly like every other entry in [`STATE_TO_CANONICAL`] — a registry\n\
         /// regeneration that reorders 26.2 states requires regenerating this whole\n\
         /// file anyway, so this is no less current than the rest of the table."
    );
    let _ = writeln!(out, "pub const AIR_STATE_ID: u32 = {air_id};\n");

    let _ = writeln!(
        out,
        "/// `STATE_TO_CANONICAL[s]` is the canonical 26.2 state id for {minecraft} flat\n\
         /// wire state `s`."
    );
    let _ = writeln!(
        out,
        "pub static STATE_TO_CANONICAL: [u32; SOURCE_STATE_COUNT as usize] = ["
    );
    for id in &mapped {
        let _ = writeln!(out, "    {id},");
    }
    out.push_str("];\n");

    out
}

// ---------------------------------------------------------------------------
// Drift guard (heavy: builds the full 32,366-entry reverse index; ignored)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates/verifies the committed tables; run explicitly"]
fn committed_tables_match_dumps() {
    let reverse = canonical_reverse_index();
    let regen = std::env::var_os("LODESTONE_REGEN").is_some();

    for source in SOURCES {
        let states = parse_dump(source.minecraft, source.dump);
        let generated = generate_with(source, &states, &reverse);
        let path = manifest_dir().join(source.committed);

        if regen {
            std::fs::write(&path, &generated).expect("write committed table");
            eprintln!("regenerated {} ({} states)", path.display(), states.len());
            continue;
        }

        let committed = std::fs::read_to_string(&path).expect("committed table present");
        assert_eq!(
            generated,
            committed,
            "{} is stale vs tests/support/{} or the 26.2 registry; regenerate with \
             LODESTONE_REGEN=1 (see the test module docs)",
            source.committed,
            source.dump_file
        );
    }
}

/// The three tables are genuinely different mappings, not three copies.
///
/// Each protocol's dump has its own state count, and a state id that exists
/// in all three names three *different* 26.2 blocks — which is the whole
/// reason there are three tables. Asserted over the committed tables alone,
/// so it runs on every `cargo test`.
#[test]
fn the_three_eras_disagree_about_what_a_state_id_means() {
    use lodestone_v1_14::canonical;

    // Measured from the three jar dumps: each era's own palette size.
    assert_eq!(canonical::table_for(498).state_count(), 11271);
    assert_eq!(canonical::table_for(578).state_count(), 11337);
    assert_eq!(canonical::table_for(754).state_count(), 17112);

    // One wire id, three eras, three different canonical blocks — and a
    // fourth block again if it is left unmapped. The id is chosen for exactly
    // that: all four answers are distinct, so no pair of them can coincide
    // and make the test pass for the wrong reason.
    const PROBE: u32 = 11214;
    let names: Vec<&str> = [498, 578, 754]
        .into_iter()
        .map(|protocol| {
            let id = canonical::table_for(protocol)
                .resolve(PROBE)
                .expect("probe is inside every era's palette");
            id.name()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "minecraft:lantern",
            "minecraft:bell",
            "minecraft:prismarine_wall",
        ],
        "wire state {PROBE} must canonicalise per era"
    );
    // Negative control for the same probe: the *unmapped* path (storing the
    // wire id straight into the canonical space) names a fourth block again.
    assert_eq!(
        block_states::block_name(PROBE),
        Some("minecraft:trapped_chest"),
        "the unmapped direct-index path must name a block distinct from all three"
    );
}

// ---------------------------------------------------------------------------
// Hermetic tests (always run: no dump parsing, no reverse-index build)
// ---------------------------------------------------------------------------

/// Sanity checks over the *committed* tables alone, cheap enough to run on
/// every `cargo test`: every entry is a valid 26.2 state id, and the baked
/// air id really is `minecraft:air`.
#[test]
fn committed_tables_are_internally_consistent() {
    use lodestone_v1_14::canonical;

    for source in SOURCES {
        let table = canonical::table_for(source.protocol);
        for wire in 0..table.state_count() {
            let id = table.resolve(wire).expect("wire < state_count resolves");
            assert!(
                id.raw() < block_states::STATE_COUNT,
                "protocol {}: canonical id {} is not a valid 26.2 state (STATE_COUNT = {})",
                id.raw(),
                source.protocol,
                block_states::STATE_COUNT
            );
        }
        assert_eq!(table.resolve(table.state_count()), None);

        let air = table.air_state_id();
        assert_eq!(block_states::block_name(air.raw()), Some("minecraft:air"));
        assert_eq!(block_states::properties(air.raw()), Some(&[][..]));
    }
}

/// Discriminating states: each pair's 1.16.5 wire id and 26.2 id are
/// genuinely different numbers naming genuinely different registry slots —
/// the class CLAUDE.md's evidence standards call out as the only kind of
/// case that can actually distinguish "canonicalised" from "not". Expected
/// values were computed from the real jar dumps (`tests/support/…json` for
/// 1.16.5, `lodestone_data::block_states` for 26.2), not from this crate's
/// own encoder, and the accompanying negative control demonstrates what the
/// *unfixed* path (treating the wire id as if it already were a 26.2 id)
/// actually names for the same input — a different, unrelated block, not a
/// near-miss.
#[test]
fn discriminating_states_resolve_to_their_26_2_ids_not_their_wire_ids() {
    // (description, 1.16.5 wire state id, expected 26.2 state id, what the
    // *unfixed* direct-index path names instead — for a human reading a
    // failure, not asserted structurally beyond "it must differ").
    let cases: &[(&str, u32, u32, &str)] = &[
        // A block with no properties at all: still a different id, because
        // 26.2 inserted thousands of new blocks earlier in the registry.
        // Renamed `grass_path` -> `dirt_path` at 1.17, exercising
        // `bridge_name` too.
        ("grass_path (-> dirt_path)", 9227, 14815, "minecraft:resin_brick_wall"),
        // A plain, unrenamed block — demonstrates the defect is not limited
        // to renamed names; every id shifts.
        ("diamond_block", 3355, 5309, "minecraft:warped_shelf"),
        // Exercises the generic `powered=false` skull/head fallback.
        ("player_head rotation=5", 6559, 11056, "minecraft:mangrove_hanging_sign"),
        // Exercises the cauldron identity split for a non-zero level.
        ("cauldron level=2 (-> water_cauldron)", 5147, 9462, "minecraft:redstone_wire"),
        // Exercises the generic `waterlogged=false` leaves fallback.
        ("oak_leaves distance=3 persistent=true", 149, 261, "minecraft:acacia_log"),
    ];

    let table = lodestone_v1_14::canonical::table_for(754);
    let mut tally = lodestone_v1_14::canonical::FallbackTally::default();
    for &(label, wire_id, expected_26_2_id, wrong_name) in cases {
        let resolved = table.resolve_or_air(wire_id, &mut tally);
        assert_eq!(
            block_states::StateId::new(expected_26_2_id).expect("oracle id is canonical"),
            resolved,
            "{label}: 1.16.5 state {wire_id} should canonicalise to 26.2 state \
             {expected_26_2_id}, got {}",
            resolved.raw()
        );

        // Negative control: the *unfixed* path (this crate before this
        // change) stored the wire id straight into the canonical space, so
        // it would have named whatever 26.2 block happens to sit at that
        // same numeric id. Confirm that block is real and is not the one we
        // just asserted — otherwise this pair would be the coincidence
        // class CLAUDE.md warns about, and would prove nothing.
        let unfixed_name = block_states::block_name(wire_id);
        assert_eq!(
            unfixed_name,
            Some(wrong_name),
            "{label}: expected the unfixed direct-index path to name {wrong_name}",
        );
        assert_ne!(
            unfixed_name.unwrap(),
            block_states::block_name(expected_26_2_id).unwrap(),
            "{label}: chosen pair does not discriminate — wire id and canonical id name the \
             same 26.2 block, exactly the coincidence class this test exists to avoid"
        );
    }
    assert!(tally.is_empty(), "none of the discriminating cases should hit the fallback path");
}
