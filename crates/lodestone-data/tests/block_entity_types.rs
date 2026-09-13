//! Per-block-state block-entity-type table: hermetic checks over the committed
//! table, plus an `#[ignore]`d drift guard that regenerates it from the committed
//! JVM dump and asserts byte-for-byte equality (modelled on `hardness.rs`). The
//! generator lives here so the checked-in table can never silently drift from the
//! game data.
//!
//! # Data provenance
//!
//! `tests/support/block_entity_types_jvm.txt` is an authoritative dump produced
//! by booting the real 26.2 server and, for each of the 32,366 states in
//! vanilla's own block-state registry, finding the registered block-entity type whose
//! own valid-blocks set claims it (`BlockEntityTypeOracle.java`). Neither of
//! Mojang's reports carries this pairing: `blocks.json` is block *properties*
//! only (no has-block-entity flag, no type), and while `registries.json` does carry
//! the `minecraft:block_entity_type` registry's 49 ids it says nothing about
//! which blocks each type covers. So "boot the jar and ask it" is the only
//! authoritative source, exactly as for hardness and collision shapes.
//!
//! # Two independent anchors, not one
//!
//! The dump is the anchor for the *state → type* half. The *type id → name* half
//! is independently cross-checked against Mojang's own `registries.json` report
//! by [`type_ids_match_mojangs_registry_report`], which is `#[ignore]`d only
//! because it reads `.cache/mc`. Two sources agreeing on the id numbering is
//! what makes `block_entity_type(state) == 1` mean `minecraft:chest` rather than
//! "whatever our own oracle happened to number first".
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump from the server (writes to a scratch file; keep the `#` header
//!    when copying over the committed dump):
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/lodestone-data/oracle-java && pwd)"
//! docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
//!   CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
//!   cp /oracle/BlockEntityTypeOracle.java /work/ && javac -cp "$CP" -d /work /work/BlockEntityTypeOracle.java
//!   java -cp "/work:$CP" BlockEntityTypeOracle'
//! ```
//!
//!    then copy its stdout over `tests/support/block_entity_types_jvm.txt`
//!    (keeping the `#` header).
//!
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test block_entity_types \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::block_entity_types;
use lodestone_data::block_states;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/block_entity_types.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/block_entity_types_jvm.txt");

/// One authoritative row: global state id, block name, and the block-entity type
/// (`None` for a state that owns no block entity).
struct Row {
    id: usize,
    name: String,
    block_entity: Option<(u32, String)>,
}

fn parse_dump(text: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        let id: usize = tok.next().expect("id column").parse().expect("id is a usize");
        let name = tok.next().expect("name column").to_owned();
        let type_id: i64 = tok
            .next()
            .expect("block entity type id column")
            .parse()
            .expect("type id is an integer");
        let type_name = tok.next().expect("block entity type name column").to_owned();
        assert!(tok.next().is_none(), "unexpected trailing tokens on {line:?}");
        // `-1 -` is the oracle's "no block entity"; the two columns must agree, or
        // the dump was truncated or mis-joined and every downstream check would be
        // measuring a half-parsed row.
        let block_entity = match (type_id, type_name.as_str()) {
            (-1, "-") => None,
            (-1, other) => panic!("type id -1 but name {other:?} on {line:?}"),
            (_, "-") => panic!("type id {type_id} but no name on {line:?}"),
            (id, other) => {
                let id = u32::try_from(id).expect("non-negative type id");
                assert!(
                    other.starts_with("minecraft:"),
                    "unnamespaced block entity type {other:?} on {line:?}"
                );
                Some((id, other.to_owned()))
            }
        };
        rows.push(Row {
            id,
            name,
            block_entity,
        });
    }
    rows.sort_by_key(|row| row.id);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.id, index, "dump ids are not a dense 0..N (gap at {index})");
    }
    rows
}

/// The `type id -> name` table the dump implies, asserting the mapping is a
/// bijection over a dense `0..N`.
///
/// A dump where two ids share a name (or one id has two names) would produce a
/// table that indexes fine and lies, so this is checked rather than assumed.
fn type_names(rows: &[Row]) -> Vec<String> {
    let mut by_id: BTreeMap<u32, &str> = BTreeMap::new();
    for row in rows {
        let Some((id, name)) = &row.block_entity else {
            continue;
        };
        if let Some(existing) = by_id.insert(*id, name) {
            assert_eq!(
                existing, name,
                "block entity type id {id} carries two different names"
            );
        }
    }
    let count = by_id.len();
    let names: Vec<String> = by_id
        .iter()
        .enumerate()
        .map(|(index, (&id, &name))| {
            assert_eq!(
                id as usize, index,
                "block entity type ids are not a dense 0..{count} (gap at {index})"
            );
            name.to_owned()
        })
        .collect();
    let mut distinct: Vec<&String> = names.iter().collect();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), names.len(), "duplicate block entity type names");
    names
}

/// Renders the committed `block_entity_types.rs` source from the parsed dump.
fn generate(rows: &[Row]) -> String {
    let count = rows.len();
    let names = type_names(rows);

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test block_entity_types -- --ignored`\n\
         // from tests/support/block_entity_types_jvm.txt (a headless 26.2 server dump of\n\
         // which BlockEntityType's validBlocks set claims each BlockState, protocol 776 /\n\
         // Minecraft 26.2). DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see\n\
         // the test module docs).\n",
    );
    out.push_str(
        "//! Generated per-block-state block-entity-type table for protocol 776\n\
         //! (Minecraft 26.2), indexed by global block-state id. Consumed by\n\
         //! [`crate::block_entity_types`].\n\n",
    );

    let _ = writeln!(out, "/// Number of block states (ids are `0..STATE_COUNT`).");
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {count};\n");

    let _ = writeln!(
        out,
        "/// Number of `minecraft:block_entity_type` registry entries (ids are\n\
         /// `0..TYPE_COUNT`)."
    );
    let _ = writeln!(out, "pub const TYPE_COUNT: u32 = {};\n", names.len());

    let _ = writeln!(
        out,
        "/// `minecraft:block_entity_type` registry keys, indexed by registry id."
    );
    let _ = writeln!(out, "pub static TYPE_NAMES: [&str; {}] = [", names.len());
    for name in &names {
        let _ = writeln!(out, "    \"{name}\",");
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Per-state block-entity type id, indexed by global block-state id.\n\
         /// `u16::MAX` (`crate::block_entity_types::NO_BLOCK_ENTITY`) means the state\n\
         /// owns no block entity."
    );
    let _ = writeln!(out, "pub static STATE_TYPE: [u16; {count}] = [");
    for chunk in rows.chunks(16) {
        out.push_str("    ");
        for row in chunk {
            match &row.block_entity {
                Some((id, _)) => {
                    let _ = write!(out, "{id}, ");
                }
                None => out.push_str("u16::MAX, "),
            }
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n");

    out
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed table (anchored to the committed dump)
// ---------------------------------------------------------------------------

/// Finds the first state id whose block name matches `name`, via the committed
/// block-state table — robust to id shifts across data bumps.
fn first_id_named(name: &str) -> Option<u32> {
    (0..block_states::STATE_COUNT).find(|&id| block_states::block_name(id) == Some(name))
}

fn validated(id: u32) -> block_states::StateId {
    block_states::StateId::new(id).expect("known census state")
}

#[test]
fn committed_table_matches_the_committed_dump_row_for_row() {
    let rows = parse_dump(DUMP);
    assert_eq!(
        rows.len(),
        block_entity_types::STATE_COUNT as usize,
        "dump/table state count mismatch"
    );
    let mut with_block_entity = 0usize;
    for row in &rows {
        let actual = block_entity_types::block_entity_type(validated(row.id as u32));
        match &row.block_entity {
            Some((type_id, type_name)) => {
                with_block_entity += 1;
                assert_eq!(
                    actual.map(|kind| kind.raw()),
                    Some(*type_id),
                    "{} (state {}) should own block entity type {type_id} ({type_name})",
                    row.name,
                    row.id
                );
                assert_eq!(
                    block_entity_types::block_entity_type_name(
                        block_entity_types::BlockEntityType::new(*type_id).expect("dump type validates"),
                    ),
                    type_name.as_str(),
                    "type {type_id}'s name disagrees with the dump"
                );
            }
            None => assert_eq!(
                actual, None,
                "{} (state {}) owns no block entity in the dump but the table says {actual:?}",
                row.name, row.id
            ),
        }
    }
    // Non-vacuous by construction: a table of all-`None` would pass every
    // per-row `None` check above, so the population is pinned too.
    assert_eq!(
        rows.len(),
        32_366,
        "expected 26.2's 32,366 block states in the dump"
    );
    assert_eq!(
        with_block_entity, 4_567,
        "expected 4,567 states owning a block entity"
    );
}

#[test]
fn count_matches_block_state_table() {
    assert_eq!(
        block_entity_types::STATE_COUNT,
        block_states::STATE_COUNT,
        "the census must cover exactly the block-state id space"
    );
}

#[test]
fn validated_state_and_block_entity_type_are_distinct_domains() {
    use block_entity_types::BlockEntityType;

    let lookup: fn(block_states::StateId) -> Option<BlockEntityType> =
        block_entity_types::block_entity_type;
    let name: fn(BlockEntityType) -> &'static str = block_entity_types::block_entity_type_name;
    let furnace = BlockEntityType::new(0).expect("furnace type exists");
    assert_eq!(name(furnace), "minecraft:furnace");
    assert_eq!(furnace.raw(), 0);
    let chest = block_states::StateId::new(first_id_named("minecraft:chest").expect("chest exists"))
        .expect("chest state validates");
    let kind = lookup(chest).expect("chest owns a block entity");
    assert_eq!(kind.raw(), 1);
    assert_eq!(name(kind), "minecraft:chest");
    assert!(BlockEntityType::new(block_entity_types::TYPE_COUNT).is_none());
    assert!(BlockEntityType::new(u32::MAX).is_none());
}

#[test]
fn out_of_range_ids_are_none_rather_than_a_panic() {
    assert_eq!(
        block_states::StateId::new(block_entity_types::STATE_COUNT),
        None
    );
    assert_eq!(block_states::StateId::new(u32::MAX), None);
    assert_eq!(
        block_entity_types::BlockEntityType::new(block_entity_types::TYPE_COUNT),
        None
    );
}

/// Air and plain terrain must own *nothing*. This is the control on the whole
/// table: a sentinel bug (say `0` instead of `u16::MAX`) would give every stone
/// block `minecraft:furnace` and the removal half of `sync_block_entity` would
/// then never fire.
#[test]
fn plain_terrain_owns_no_block_entity() {
    for name in [
        "minecraft:air",
        "minecraft:stone",
        "minecraft:dirt",
        "minecraft:oak_planks",
        "minecraft:water",
    ] {
        let id = first_id_named(name).unwrap_or_else(|| panic!("{name} is in the 26.2 table"));
        assert_eq!(
            block_entity_types::block_entity_type(validated(id)),
            None,
            "{name} (state {id}) must own no block entity"
        );
    }
}

/// The chest family, which is what motivated this census in the first place.
///
/// Asserted through the *name* rather than a remembered id, and asserted that
/// chest and trapped chest are **different** types while the copper variants
/// share `minecraft:chest` — the latter is exactly why a renderer cannot use the
/// type to pick a texture.
#[test]
fn the_chest_family_maps_the_way_the_renderer_assumes() {
    let expect = |block: &str, type_name: &str| {
        let id = first_id_named(block).unwrap_or_else(|| panic!("{block} is in the 26.2 table"));
        let type_id = block_entity_types::block_entity_type(validated(id))
            .unwrap_or_else(|| panic!("{block} (state {id}) must own a block entity"));
        assert_eq!(
            block_entity_types::block_entity_type_name(type_id),
            type_name,
            "{block} (state {id}) resolved to type {type_id:?}"
        );
        type_id
    };

    let chest = expect("minecraft:chest", "minecraft:chest");
    let trapped = expect("minecraft:trapped_chest", "minecraft:trapped_chest");
    let ender = expect("minecraft:ender_chest", "minecraft:ender_chest");
    assert_ne!(chest, trapped, "chest and trapped chest are distinct types");
    assert_ne!(chest, ender, "chest and ender chest are distinct types");

    // All four copper chests are the *same* type as a plain chest: the type
    // cannot tell them apart, so `block_entities.rs` reads the block state.
    for block in [
        "minecraft:copper_chest",
        "minecraft:exposed_copper_chest",
        "minecraft:weathered_copper_chest",
        "minecraft:oxidized_copper_chest",
    ] {
        assert_eq!(
            expect(block, "minecraft:chest"),
            chest,
            "{block} must share minecraft:chest's type id"
        );
    }
}

/// Every state of a block that owns a block entity owns the *same* one.
///
/// Vanilla's own block-entity-type "is valid" check is `validBlocks.contains(state.getBlock())` — a
/// block-level test — so this must hold, and if a future version makes it
/// per-state this test is what says so out loud instead of the table quietly
/// depending on it.
#[test]
fn the_type_is_constant_across_a_blocks_states() {
    let mut seen: BTreeMap<&'static str, Option<block_entity_types::BlockEntityType>> = BTreeMap::new();
    for id in 0..block_entity_types::STATE_COUNT {
        let Some(name) = block_states::block_name(id) else {
            continue;
        };
        let type_id = block_entity_types::block_entity_type(validated(id));
        match seen.get(name) {
            Some(existing) => assert_eq!(
                *existing, type_id,
                "{name} state {id} owns {type_id:?} but a sibling state owns {existing:?}"
            ),
            None => {
                seen.insert(name, type_id);
            }
        }
    }
    let with = seen.values().filter(|t| t.is_some()).count();
    assert!(with > 40, "only {with} blocks own a block entity — table looks empty");
}

/// Every type id in the table names a real type, and every named type is
/// reachable from at least one state.
///
/// The second half is the interesting one: an unreachable type would mean the
/// oracle's registry walk and its state walk disagree.
#[test]
fn every_type_is_named_and_every_name_is_reachable() {
    let mut reached = vec![false; block_entity_types::TYPE_COUNT as usize];
    for id in 0..block_entity_types::STATE_COUNT {
        if let Some(type_id) = block_entity_types::block_entity_type(validated(id)) {
            assert!(
                !block_entity_types::block_entity_type_name(type_id).is_empty(),
                "state {id} names type {type_id:?}, which has no registry key"
            );
            reached[type_id.raw() as usize] = true;
        }
    }
    let unreachable: Vec<&str> = reached
        .iter()
        .enumerate()
        .filter(|&(_, &hit)| !hit)
        .map(|(index, _)| {
            block_entity_types::block_entity_type_name(
                block_entity_types::BlockEntityType::new(index as u32).expect("type validates"),
            )
        })
        .collect();
    assert!(
        unreachable.is_empty(),
        "block entity types no state can reach: {unreachable:?}"
    );
}

// ---------------------------------------------------------------------------
// Drift guards
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates/verifies the committed table; run explicitly"]
fn committed_table_matches_dump() {
    let rows = parse_dump(DUMP);
    let generated = generate(&rows);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated, committed,
        "src/generated/block_entity_types.rs is stale vs the JVM dump; regenerate with LODESTONE_REGEN=1"
    );
}

/// The second, independent anchor: Mojang's own `registries.json` report must
/// agree with our oracle on the `block_entity_type` id numbering.
///
/// `#[ignore]`d because it reads `.cache/mc`, which is not repo state. Without
/// this, `block_entity_type(chest_state) == 1` would rest entirely on our own
/// dump's ordering.
#[test]
#[ignore = "reads .cache/mc/26.2/generated/reports/registries.json"]
fn type_ids_match_mojangs_registry_report() {
    let path = manifest_dir().join("../../.cache/mc/26.2/generated/reports/registries.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let json: serde_json::Value = serde_json::from_str(&text).expect("registries.json parses");
    let entries = json["minecraft:block_entity_type"]["entries"]
        .as_object()
        .expect("minecraft:block_entity_type registry present");
    assert_eq!(
        entries.len(),
        block_entity_types::TYPE_COUNT as usize,
        "registries.json has {} block entity types, our table has {}",
        entries.len(),
        block_entity_types::TYPE_COUNT
    );
    for (name, value) in entries {
        let id = value["protocol_id"].as_u64().expect("protocol_id") as u32;
        assert_eq!(
            block_entity_types::block_entity_type_name(
                block_entity_types::BlockEntityType::new(id).expect("report type validates"),
            ),
            name.as_str(),
            "registries.json numbers {name} as {id}; our table disagrees"
        );
    }
}
