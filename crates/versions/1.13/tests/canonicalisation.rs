//! Generator + drift guard for this era's flat block-state id -> canonical
//! 26.2 block-state id table (`src/generated/canonical.rs`), plus hermetic,
//! hardcoded-value tests that run on every `cargo test`. Modelled directly on
//! `crates/lodestone-data/tests/block_states.rs` and
//! `crates/lodestone-canonical/tests/flattening.rs`'s generate-or-assert
//! pattern.
//!
//! # Why a table at all
//!
//! 1.13 is the release that made a palette entry a flat state id, and the
//! release that renumbered every one of them. The dump below carries 8,599
//! states against 26.2's 32k, and even a block unchanged since 1.8 sits at a
//! different number in each, so storing a wire id straight into
//! `lodestone-world` is the silent wrong-terrain defect `src/canonical.rs`
//! exists to prevent.
//!
//! # Data provenance
//!
//! `tests/support/blocks_1_13_2_jar.json` is **not** a community dataset: it
//! is the unmodified output of Mojang's own data generator, run in its
//! `--reports` mode against the real `.cache/mc/1.13.2/server.jar`, the same
//! tool and report shape `crates/lodestone-data/tests/block_states.rs` reads
//! for the 26.2 side. Every state lists its own `id` and `properties`
//! explicitly, so no combinatorial re-derivation of vanilla's state-id
//! numbering is needed on either side.
//!
//! The bridge from a source `(name, properties)` pair to a 26.2 state id
//! reuses exactly the technique `lodestone_canonical::canonical` already uses
//! for the pre-Flattening families: build a `(name, properties) -> 26.2 id`
//! reverse index from `lodestone_data::block_states` (itself jar-derived, not
//! this crate's own encoder/decoder), try a direct match, then a small
//! hand-verified rename table, then generic fallbacks for properties 1.13.2
//! does not yet have. Every rule below was checked against this corpus: after
//! applying them, zero states are left unmapped (see
//! `committed_table_matches_dump`'s assertion).
//!
//! Two rules are not read off any table on either side: walls' four side
//! properties turning from booleans into a three-valued `none`/`low`/`tall`
//! in 1.16, and 1.13's unprefixed `sign`/`wall_sign` becoming `oak_sign`/
//! `oak_wall_sign` when 1.14 added the other five woods. Both are what
//! vanilla itself produced when a real 1.13.2 world carrying those exact
//! states was booted under the real 26.2 server jar and read back over RCON;
//! the probes, the procedure and the answers are committed verbatim in
//! `tests/support/state_upgrade_1_13_2_to_26_2.txt`. Without the wall rule
//! 128 of the dump's states have no mapping at all, and without the sign
//! rename another 40.
//!
//! Unlike the pre-Flattening bridge, the mapping is baked into a flat
//! generated `[u32; SOURCE_STATE_COUNT]` array **at regeneration time**
//! rather than resolved lazily at runtime: 404 has no ambiguous "requires
//! additional context" cases (that was a pre-Flattening `id:meta` problem --
//! a flat state id already carries full block identity), so there is nothing
//! left for the runtime path to compute. `src/canonical.rs` is therefore just
//! an `O(1)` index into the table plus the wire-corruption fallback (a state
//! id past `SOURCE_STATE_COUNT`, which no real 1.13.2 server sends).
//!
//! # Refreshing after the source jar changes
//!
//! 1. Re-run the data generator against the (possibly updated) server jar
//!    under Apple `container` (see `docs/oracles-and-benchmarks.md`), in its
//!    `--reports` mode, and copy `reports/blocks.json` over
//!    `tests/support/blocks_1_13_2_jar.json`.
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-v1-13 --test canonicalisation \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```
//!
//! If the 26.2 registry itself changed (`lodestone-data` regenerated), rerun
//! step 2 alone -- no source-side dump needs to change.

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

const SOURCES: &[Source] = &[Source {
    minecraft: "1.13.2",
    protocol: 404,
    dump: include_str!("support/blocks_1_13_2_jar.json"),
    dump_file: "blocks_1_13_2_jar.json",
    committed: "src/generated/canonical.rs",
}];

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

/// Renames a 1.13.2 block name to its 26.2 name (verified against the 26.2
/// registry, not jar-traced — the same standard of evidence
/// `lodestone_canonical::canonical::bridge_name` uses for its own entries).
fn bridge_name(name: &str) -> &str {
    match name {
        // Renamed `grass` -> `short_grass` in 1.20.3 to disambiguate from the
        // ground block (an identical entry exists in
        // `lodestone_canonical::canonical::bridge_name` for the pre-1.13
        // families; 1.13.2 hits the same rename).
        "minecraft:grass" => "minecraft:short_grass",
        // Renamed `grass_path` -> `dirt_path` in 1.17.
        "minecraft:grass_path" => "minecraft:dirt_path",
        // 1.13 has one sign wood, so its blocks are unprefixed; 1.14 added
        // the other five and renamed these two. `rotation`/`facing` and
        // `waterlogged` carry across unchanged. Both are measured, not
        // guessed at from the name: see
        // `tests/support/state_upgrade_1_13_2_to_26_2.txt`, where vanilla's
        // own upgrader turned each of these into the name below.
        "minecraft:sign" => "minecraft:oak_sign",
        "minecraft:wall_sign" => "minecraft:oak_wall_sign",
        other => other,
    }
}

/// Bridges one 1.13.2 `(name, properties)` pair to a 26.2 state id. Order:
/// direct match, then [`bridge_name`] alone, then two single-property
/// fallbacks — each tried only if the exact match fails, so neither can
/// override a block that already resolves without it.
fn resolve(
    name: &str,
    properties: &[(String, String)],
    reverse: &HashMap<(String, Vec<(String, String)>), u32>,
) -> Option<u32> {
    // 1.13.2's `cauldron` already carries a `level` property (0-3), unlike
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

    let bridged_name = bridge_name(name).to_owned();
    let mut props = properties.to_vec();

    // A wall's four side properties were booleans before 1.16 and a
    // three-valued `none`/`low`/`tall` from 1.16 on. Not read off any table:
    // `tests/support/state_upgrade_1_13_2_to_26_2.txt` records what vanilla's
    // own upgrader produced for three probes spanning both boolean values,
    // both `up` values and both `waterlogged` values. `tall` is a state the
    // 1.13.2 wire cannot express and vanilla never produces from one, and
    // `wall_side` returns `None` for any value that is already an enum
    // member, so a later era reusing this helper is unaffected.
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

    // Leaves, rails (all four), barrier and everything else that gained a
    // `waterlogged` property after 1.13.2 lands here. The block's own 26.2
    // registry default for that property is `false` in every case, and
    // 1.13.2 has no waterlogging concept for them at all, so this is exact
    // rather than "unknown, assume false" — the same justification
    // `lodestone_canonical::canonical::resolve_canonical` documents for its
    // own generic `waterlogged=false` fallback.
    let mut with_waterlogged = props.clone();
    insert_sorted(&mut with_waterlogged, "waterlogged", "false");
    if let Some(&id) = reverse.get(&(bridged_name.clone(), with_waterlogged)) {
        return Some(id);
    }

    // Every mob-head/skull block gained a `powered` redstone-signal property
    // after 1.13.2. The 26.2 registry's own default for that property is
    // `false`, and 1.13.2 has no redstone-signal concept for skulls at all,
    // so this is exact rather than a guess.
    let mut with_powered = props;
    insert_sorted(&mut with_powered, "powered", "false");
    reverse.get(&(bridged_name, with_powered)).copied()
}

/// Maps one pre-1.16 boolean wall-side value to its 1.16+ enum value, or
/// [`None`] for a value that is already an enum member.
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
    let mut unmapped: Vec<String> = Vec::new();
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
        "{minecraft}: {} state(s) have no canonical 26.2 mapping; sample: {:?}",
        unmapped.len(),
        &unmapped[..unmapped.len().min(40)]
    );

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-v1-13 --test canonicalisation -- --ignored`\n",
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

// ---------------------------------------------------------------------------
// Hermetic tests (always run: no dump parsing, no reverse-index build)
// ---------------------------------------------------------------------------

/// Sanity checks over the *committed* tables alone, cheap enough to run on
/// every `cargo test`: every entry is a valid 26.2 state id, and the baked
/// air id really is `minecraft:air`.
#[test]
fn committed_tables_are_internally_consistent() {
    use lodestone_v1_13::canonical;

    for source in SOURCES {
        let table = canonical::table_for(source.protocol);
        for wire in 0..table.state_count() {
            let id = table.resolve(wire).expect("wire < state_count resolves");
            assert!(
                id.raw() < block_states::STATE_COUNT,
                "protocol {}: canonical id {} is not a valid 26.2 state (STATE_COUNT = {})",
                source.protocol,
                id.raw(),
                block_states::STATE_COUNT
            );
        }
        assert_eq!(table.resolve(table.state_count()), None);

        let air = table.air_state_id();
        assert_eq!(block_states::block_name(air.raw()), Some("minecraft:air"));
        assert_eq!(air.properties(), &[][..]);
    }
}
