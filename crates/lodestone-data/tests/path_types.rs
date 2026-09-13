//! Path-type table: hermetic checks over the committed table, plus an
//! `#[ignore]`d drift guard that regenerates it from the authoritative oracle
//! dump and asserts byte-for-byte equality (modelled on the collision-shape and
//! block-state tables). The generator lives here so the checked-in table can
//! never silently drift from the game data. The large static is emitted as
//! deterministic, contiguous include shards so generated sources stay small;
//! the includes are compile-time only and do not add a runtime indirection.
//!
//! The dump (`oracle-java/pathtype_java.txt`, gitignored like `.cache/mc`) is
//! produced by this crate's `PathTypeOracle.java`, which boots the real 26.2
//! server, loads the vanilla data-pack tags, and dumps
//! vanilla's own "get path type from state" step for every one of the 32,366 states.
//!
//! Regenerate the committed table after a data bump with:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test path_types \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_model::{PathType, PathTypeRegistry};
use lodestone_data::block_states::{self, StateId};
use lodestone_data::path_types::{self, PathTypes};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The authoritative oracle dump (gitignored local artifact).
fn dump_path() -> PathBuf {
    manifest_dir().join("oracle-java/pathtype_java.txt")
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/path_types.rs")
}

fn generated_dir() -> PathBuf {
    committed_path()
        .parent()
        .expect("generated path has a parent")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// Generator (shared by regen and the drift check)
// ---------------------------------------------------------------------------

/// The path-type variant names vanilla's own "get path type from state" step can return.
/// Anything outside this set in the dump is a bug (or a game change) and must
/// fail loudly rather than emit an invalid Rust variant.
const KNOWN_TYPES: &[&str] = &[
    "BLOCKED",
    "OPEN",
    "WALKABLE",
    "WALKABLE_DOOR",
    "TRAPDOOR",
    "POWDER_SNOW",
    "ON_TOP_OF_POWDER_SNOW",
    "FENCE",
    "LAVA",
    "WATER",
    "WATER_BORDER",
    "RAIL",
    "UNPASSABLE_RAIL",
    "FIRE_IN_NEIGHBOR",
    "FIRE",
    "DAMAGING_IN_NEIGHBOR",
    "DAMAGING",
    "DOOR_OPEN",
    "DOOR_WOOD_CLOSED",
    "DOOR_IRON_CLOSED",
    "BREACH",
    "LEAVES",
    "STICKY_HONEY",
    "COCOA",
    "DAMAGE_CAUTIOUS",
    "ON_TOP_OF_TRAPDOOR",
    "BIG_MOBS_CLOSE_TO_DANGER",
];

/// `SCREAMING_SNAKE_CASE` -> `CamelCase`, matching the [`PathType`] variant
/// spelling (`WALKABLE_DOOR` -> `WalkableDoor`).
fn screaming_to_camel(name: &str) -> String {
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
                }
                None => String::new(),
            }
        })
        .collect()
}

/// Maximum number of state entries emitted into one include file.
///
/// This is deliberately a fixed generator constant rather than a choice based
/// on the current dump, so a data update can only append/repartition at known
/// boundaries and every regeneration is byte-for-byte deterministic.
const SHARD_ENTRIES: usize = 1024;

fn shard_file_name(index: usize) -> String {
    format!("path_types_{index:04}.rs")
}

struct GeneratedTables {
    root: String,
    shards: Vec<(String, String)>,
}

/// Parses the dump into `type_by_id[id]` = that state's path-type name, dense.
fn parse_dump(text: &str) -> Vec<String> {
    let mut rows: Vec<(usize, String)> = Vec::new();
    let mut max_id = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut tok = line.split_whitespace();
        let id: usize = tok.next().expect("id").parse().expect("id is a usize");
        let _name = tok.next().expect("block name");
        let ty = tok.next().expect("path type").to_string();
        assert!(
            KNOWN_TYPES.contains(&ty.as_str()),
            "unknown path type {ty:?} at id {id} — the game changed, update PathType/KNOWN_TYPES"
        );
        max_id = max_id.max(id);
        rows.push((id, ty));
    }
    let mut out = vec![String::new(); max_id + 1];
    let mut seen = vec![false; max_id + 1];
    for (id, ty) in rows {
        assert!(!seen[id], "duplicate id {id} in dump");
        seen[id] = true;
        out[id] = ty;
    }
    assert!(seen.iter().all(|&s| s), "dump has gaps in the id range");
    out
}

fn generate(text: &str) -> GeneratedTables {
    let types = parse_dump(text);
    let count = types.len();

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test path_types -- --ignored`\n\
         // from crates/versions/26.2/oracle-java/pathtype_java.txt (real 26.2 server dump,\n\
         // protocol 776). DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the\n\
         // test module docs).\n",
    );
    out.push_str(
        "//! Generated node-evaluator path-type table for protocol 776 (Minecraft 26.2).\n//!\n",
    );
    out.push_str(
        "//! Raw rodata array consumed by [`crate::path_types`]. Each entry is a\n\
         //! fieldless [`lodestone_model::PathType`] (one byte), so the whole table\n\
         //! lives in rodata with zero heap.\n\n",
    );
    out.push_str("use lodestone_model::PathType;\n\n");

    let _ = writeln!(
        out,
        "/// Number of block states (ids are `0..STATE_COUNT`)."
    );
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {count};\n");

    let _ = writeln!(out, "const fn copy_path_type_shard<const N: usize>(");
    let _ = writeln!(out, "    mut out: [PathType; {count}],");
    let _ = writeln!(out, "    shard: [PathType; N],");
    let _ = writeln!(out, "    offset: usize,");
    let _ = writeln!(out, ") -> [PathType; {count}] {{");
    let _ = writeln!(out, "    let mut index = 0;");
    let _ = writeln!(out, "    while index < N {{");
    let _ = writeln!(out, "        out[offset + index] = shard[index];");
    let _ = writeln!(out, "        index += 1;");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "    out");
    let _ = writeln!(out, "}}\n");
    let _ = writeln!(out, "const fn assemble_state_path_type() -> [PathType; {count}] {{");
    let _ = writeln!(out, "    let mut out = [PathType::Open; {count}];");

    let mut shards = Vec::new();
    for (index, chunk) in types.chunks(SHARD_ENTRIES).enumerate() {
        let file_name = shard_file_name(index);
        let start = index * SHARD_ENTRIES;
        let end = start + chunk.len();
        let mut shard = String::new();
        let _ = writeln!(
            shard,
            "// @generated by `cargo test -p lodestone-data --test path_types -- --ignored`"
        );
        let _ = writeln!(
            shard,
            "// Entries for contiguous block-state ids [{start}, {end}); DO NOT EDIT BY HAND."
        );
        shard.push_str("[\n");
        for entries in chunk.chunks(8) {
            shard.push_str("    ");
            for ty in entries {
                let _ = write!(shard, "PathType::{}, ", screaming_to_camel(ty));
            }
            shard.pop();
            shard.push('\n');
        }
        shard.push_str("]\n");
        let _ = writeln!(
            out,
            "    out = copy_path_type_shard(out, include!(\"{file_name}\"), {start});"
        );
        shards.push((file_name, shard));
    }
    out.push_str("    out\n}\n\n");
    let _ = writeln!(
        out,
        "/// Per-state base path type, indexed by block-state id."
    );
    out.push_str(&format!(
        "pub static STATE_PATH_TYPE: [PathType; {count}] = assemble_state_path_type();\n"
    ));

    GeneratedTables { root: out, shards }
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed table (no dump needed)
// ---------------------------------------------------------------------------

fn first_id_named(name: &str) -> Option<u32> {
    (0..block_states::STATE_COUNT).find(|&id| block_states::block_name(id) == Some(name))
}

fn states_named(name: &str) -> impl Iterator<Item = u32> + '_ {
    (0..block_states::STATE_COUNT).filter(move |&id| block_states::block_name(id) == Some(name))
}

fn validated(raw: u32) -> StateId {
    StateId::new(raw).unwrap_or_else(|| panic!("state id {raw} must be in the generated table"))
}

#[test]
fn count_matches_block_state_table() {
    assert_eq!(
        path_types::STATE_COUNT,
        block_states::STATE_COUNT,
        "path-type table must cover exactly the block-state id space"
    );
}

#[test]
fn ids_are_contiguous_and_out_of_range_is_rejected_at_the_boundary() {
    let count = path_types::STATE_COUNT;
    for id in 0..count {
        let state = StateId::new(id)
            .unwrap_or_else(|| panic!("id {id} in 0..{count} did not validate"));
        let _ = path_types::path_type(state);
    }
    assert!(StateId::new(count).is_none());
    assert!(StateId::new(u32::MAX).is_none());
}

#[test]
fn registry_impl_matches_free_function() {
    let reg = PathTypes;
    assert_eq!(reg.state_count(), path_types::STATE_COUNT);
    for id in [0, 1, 100, 1000, path_types::STATE_COUNT - 1] {
        assert_eq!(reg.path_type(id), Some(path_types::path_type(validated(id))));
    }
    assert_eq!(reg.path_type(path_types::STATE_COUNT), None);
}

#[test]
fn air_is_open_and_stone_is_blocked() {
    assert_eq!(path_types::path_type(validated(0)), PathType::Open, "air");
    assert_eq!(path_types::path_type(validated(1)), PathType::Blocked, "stone");
}

#[test]
fn fluids_classify_as_water_and_lava() {
    // These depend on FluidTags, which only bind once the data pack is loaded —
    // the exact trap the oracle guards against. Pin them per-block.
    for id in states_named("minecraft:water") {
        assert_eq!(
            path_types::path_type(validated(id)),
            PathType::Water,
            "water id {id}"
        );
    }
    for id in states_named("minecraft:lava") {
        assert_eq!(
            path_types::path_type(validated(id)),
            PathType::Lava,
            "lava id {id}"
        );
    }
}

#[test]
fn fences_and_walls_are_fence() {
    // is(FENCES)/is(WALLS) short-circuits before the collision check, so *every*
    // fence, wall and closed fence-gate state is FENCE — a mob cannot step over
    // it. (This is broader than the collision table, where degenerate no-connect
    // wall states have empty geometry.) A regression to OPEN/BLOCKED would let a
    // pathfinder route straight through a fence.
    for name in [
        "minecraft:oak_fence",
        "minecraft:cobblestone_wall",
        "minecraft:nether_brick_fence",
    ] {
        let mut checked = 0usize;
        for id in states_named(name) {
            assert_eq!(
                path_types::path_type(validated(id)),
                PathType::Fence,
                "{name} id {id}"
            );
            checked += 1;
        }
        assert!(checked > 0, "no states for {name}");
    }
    // A fence gate is FENCE when closed and OPEN when open — both must occur.
    let gate: HashSet<_> = states_named("minecraft:oak_fence_gate")
        .map(|id| path_types::path_type(validated(id)))
        .collect();
    assert_eq!(
        gate,
        HashSet::from([PathType::Fence, PathType::Open]),
        "fence gate must be FENCE (closed) and OPEN (open)"
    );
}

#[test]
fn doors_split_by_material_and_open_state() {
    let oak: HashSet<_> = states_named("minecraft:oak_door")
        .map(|id| path_types::path_type(validated(id)))
        .collect();
    assert_eq!(
        oak,
        HashSet::from([PathType::DoorOpen, PathType::DoorWoodClosed]),
        "wooden door: open -> DoorOpen, closed -> DoorWoodClosed"
    );
    let iron: HashSet<_> = states_named("minecraft:iron_door")
        .map(|id| path_types::path_type(validated(id)))
        .collect();
    assert_eq!(
        iron,
        HashSet::from([PathType::DoorOpen, PathType::DoorIronClosed]),
        "iron door: open -> DoorOpen, closed -> DoorIronClosed"
    );
}

#[test]
fn special_blocks_have_expected_types() {
    let cases = [
        ("minecraft:rail", PathType::Rail),
        ("minecraft:oak_leaves", PathType::Leaves),
        ("minecraft:cactus", PathType::Damaging),
        ("minecraft:sweet_berry_bush", PathType::Damaging),
        ("minecraft:powder_snow", PathType::PowderSnow),
        ("minecraft:honey_block", PathType::StickyHoney),
        ("minecraft:cocoa", PathType::Cocoa),
        ("minecraft:wither_rose", PathType::DamageCautious),
        ("minecraft:pointed_dripstone", PathType::DamageCautious),
        ("minecraft:magma_block", PathType::Fire),
        ("minecraft:lily_pad", PathType::Trapdoor),
        ("minecraft:big_dripleaf", PathType::Trapdoor),
        ("minecraft:soul_sand", PathType::Blocked),
    ];
    for (name, want) in cases {
        let id = first_id_named(name).unwrap_or_else(|| panic!("{name} present"));
        assert_eq!(path_types::path_type(validated(id)), want, "{name} (id {id})");
    }
    // Trapdoors themselves are TRAPDOOR (tag-based).
    for id in states_named("minecraft:oak_trapdoor") {
        assert_eq!(
            path_types::path_type(validated(id)),
            PathType::Trapdoor,
            "oak_trapdoor id {id}"
        );
    }
}

// ---------------------------------------------------------------------------
// Drift guard + corpus report (requires the oracle dump)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires the path-type oracle dump; regenerates and checks the committed table"]
fn committed_table_matches_dump() {
    let text = std::fs::read_to_string(dump_path())
        .expect("pathtype_java.txt present under oracle-java (run PathTypeOracle.java)");
    let generated = generate(&text);
    let expected_shards: BTreeSet<&str> = generated
        .shards
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated.root).expect("write committed table");
        for (name, contents) in &generated.shards {
            std::fs::write(generated_dir().join(name), contents)
                .expect("write path-type include shard");
        }
        // A changed state count can make an older final shard unnecessary.
        // Remove only files in this generator's reserved, exact prefix; the
        // drift branch below rejects these files instead of silently ignoring
        // them when regeneration was not explicitly requested.
        for entry in std::fs::read_dir(generated_dir()).expect("read generated directory") {
            let entry = entry.expect("read generated directory entry");
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("path_types_")
                && name.ends_with(".rs")
                && !expected_shards.contains(name.as_ref())
            {
                std::fs::remove_file(entry.path()).expect("remove stale path-type shard");
            }
        }
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated.root, committed,
        "src/generated/path_types.rs is stale vs the oracle dump; regenerate with LODESTONE_REGEN=1"
    );

    let actual_shards: BTreeSet<String> = std::fs::read_dir(generated_dir())
        .expect("read generated directory")
        .map(|entry| entry.expect("read generated directory entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.starts_with("path_types_") && name.ends_with(".rs"))
        .collect();
    let expected_shards_owned: BTreeSet<String> = expected_shards
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(
        actual_shards, expected_shards_owned,
        "path-type include shard set is missing or stale; regenerate with LODESTONE_REGEN=1"
    );
    for (name, expected) in &generated.shards {
        let actual = std::fs::read_to_string(generated_dir().join(name))
            .unwrap_or_else(|_| panic!("missing path-type include shard {name}"));
        assert_eq!(
            actual, *expected,
            "path-type include shard {name} is stale vs the oracle dump; regenerate with LODESTONE_REGEN=1"
        );
    }

    // --- corpus report ----------------------------------------------------
    let types = parse_dump(&text);
    let dump_states = types.len();
    let distinct: BTreeSet<&str> = types.iter().map(String::as_str).collect();

    // Cross-check every id against the committed table (whole-corpus, not spot).
    let mut mismatches = 0usize;
    for (id, ty) in types.iter().enumerate() {
        let want = screaming_to_camel(ty);
        let got = format!("{:?}", path_types::path_type(validated(id as u32)));
        if got != want {
            mismatches += 1;
        }
    }

    let rodata = dump_states; // one byte per state (fieldless enum)

    println!("=== PATH-TYPE TABLE REPORT ===");
    println!(
        "states (dump / table)    : {dump_states} / {}",
        path_types::STATE_COUNT
    );
    println!(
        "max id + 1 == count      : {dump_states} -> {}",
        dump_states == path_types::STATE_COUNT as usize
    );
    println!(
        "distinct path types      : {} of {} enum variants",
        distinct.len(),
        KNOWN_TYPES.len()
    );
    println!("whole-corpus mismatches  : {mismatches} / {dump_states}");
    println!(
        "rodata (STATE_PATH_TYPE) : {rodata} bytes ({:.1} KiB)",
        rodata as f64 / 1024.0
    );
    println!("==============================");

    assert_eq!(dump_states, path_types::STATE_COUNT as usize);
    assert_eq!(mismatches, 0, "committed table disagrees with the dump");
}
