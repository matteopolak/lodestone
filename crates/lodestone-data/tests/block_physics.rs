//! Block **physics facts** that are not geometry, in two halves that are keyed
//! differently on purpose:
//!
//! 1. the per-block-state `legacySolid` / `blocksMotion` census generated into
//!    `src/generated/block_solidity.rs` and read through
//!    [`lodestone_data::block_solidity`] — state-keyed, so it lives behind the
//!    version seam (`VersionAdapter::block_blocks_motion`);
//! 2. the six **name**-keyed movement constants in
//!    [`lodestone_model::block_physics`] — `friction`, `speed_factor`,
//!    `jump_factor`, `bounce_restitution`, `stuck_multiplier` and `climbable`.
//!    Those are *not* version-owned (a block name is stable where a state id is
//!    renumbered every version) so the table lives in the version-free
//!    `lodestone-model`, where a plugin can reach it. This test is what anchors
//!    it to real 26.2 data: it replays every one of the 1,196 blocks in the dump
//!    through `lodestone_model::block_physics` and demands agreement.
//!
//! Modelled on `hardness.rs` and `outline_shapes.rs`: generate-or-assert with
//! `LODESTONE_REGEN=1`, anchored to a committed JVM dump.
//!
//! # Data provenance
//!
//! `tests/support/block_physics_jvm.txt` is an authoritative dump produced by
//! booting the real 26.2 server (`BlockPhysicsOracle.java`) and reading, per
//! block, `Block.getFriction/getSpeedFactor/getJumpFactor/getBounceRestitution`
//! plus `BlockTags.CLIMBABLE`/`SUPPRESSES_BOUNCE` membership; and per block
//! *state*, `BlockStateBase.isSolid()` and `BlockStateBase.blocksMotion()`.
//!
//! None of it is in `blocks.json` (which carries block *properties* only — no
//! friction, no `destroySpeed`, no geometry), and `vendor/minecraft-data` has no
//! 26.x data at all and was measured 92.29% covered and stale for 26.2 on the
//! collision census. So "boot the jar and ask it" is again the only
//! authoritative source.
//!
//! The dump additionally carries two columns that exist purely so this test can
//! make *negative* claims non-vacuously:
//!
//! * `F <block> <forceSolidOn> <forceSolidOff> <dynamicShape>` — the two private
//!   `Properties` flags, read by reflection because neither has a getter. This is
//!   what makes "the shape cannot answer this" a measurement rather than a claim.
//! * `E <block> <class>` — every block whose class overrides
//!   `entityInside` at all. `makeStuckInBlock`'s vector is imperative code, not a
//!   property, so it cannot be dumped; what *can* be dumped is the candidate set,
//!   which turns `lodestone_model`'s three-row `stuck_multiplier` table from an
//!   asserted-complete list into a checked-complete one
//!   ([`stuck_multiplier_is_only_set_by_blocks_that_override_entity_inside`]).
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump from the server (keep the `#` header when copying over the
//!    committed dump). Byte-reproducible: two runs of the command below produced
//!    identical output (md5 `fb3dba4a9dd0c24b430bf37ed72557cf`).
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/protocol/v770/oracle-java && pwd)"
//! docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
//!   CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
//!   cp /oracle/BlockPhysicsOracle.java /work/ && javac -cp "$CP" -d /work /work/BlockPhysicsOracle.java
//!   java -cp "/work:$CP" BlockPhysicsOracle'
//! ```
//!
//!    No `--add-opens` is needed even though the oracle reflects into
//!    `BlockBehaviour.Properties`' private `forceSolid*` fields: the server jar is
//!    on the *classpath*, so its classes are in the unnamed module, where
//!    `setAccessible(true)` is unrestricted. That stops being true the day the
//!    server ships as a named module.
//!
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test block_physics \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```
//!
//! 3. If step 2's *hermetic* siblings now fail, the name-keyed table in
//!    `lodestone-model`'s `adapter.rs` needs the new row — that failure is the
//!    point, and it is why the constants are gated here rather than in
//!    `lodestone-model`'s own tests (which have no JVM dump to check against).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_model::{DEFAULT_BLOCK_PHYSICS, block_physics};
use lodestone_data::{block_solidity, block_states, collision_shapes};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/block_solidity.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/block_physics_jvm.txt");

/// One block's name-keyed constants, exactly as the server reported them.
struct Constants {
    /// Raw `Float.floatToRawIntBits` patterns, so the text round-trip is lossless.
    friction_bits: u32,
    speed_factor_bits: u32,
    jump_factor_bits: u32,
    bounce_restitution_bits: u32,
    climbable: bool,
    suppresses_bounce: bool,
}

/// `Properties.forceSolidOn` / `forceSolidOff` / `Block.hasDynamicShape`.
#[derive(Clone, Copy)]
struct SolidFlags {
    force_on: bool,
    force_off: bool,
    dynamic_shape: bool,
}

struct Dump {
    state_count: usize,
    block_count: usize,
    /// Per-block constants, keyed by `minecraft:*` name.
    constants: BTreeMap<String, Constants>,
    /// Per-block `forceSolid*` / `dynamicShape` flags.
    flags: BTreeMap<String, SolidFlags>,
    /// Blocks whose class overrides `entityInside`, name → declaring class.
    entity_inside: BTreeMap<String, String>,
    /// `(first state id, block name)` per block, ascending.
    blocks: Vec<(usize, String)>,
    /// Per-state `BlockStateBase.isSolid()`.
    legacy_solid: Vec<bool>,
    /// Per-state `BlockStateBase.blocksMotion()`.
    blocks_motion: Vec<bool>,
    /// Per-state `calculateSolid`'s geometry branch alone — the best a consumer
    /// holding only a shape table could do.
    geometry_solid: Vec<bool>,
}

impl Dump {
    /// The block name of every state, expanded from the `B` boundaries.
    fn block_names(&self) -> Vec<&str> {
        let mut names = Vec::with_capacity(self.state_count);
        for (index, (start, name)) in self.blocks.iter().enumerate() {
            let end = self
                .blocks
                .get(index + 1)
                .map_or(self.state_count, |(next, _)| *next);
            assert_eq!(*start, names.len(), "block boundaries are not contiguous");
            for _ in *start..end {
                names.push(name.as_str());
            }
        }
        assert_eq!(
            names.len(),
            self.state_count,
            "block boundaries do not cover all states"
        );
        names
    }
}

fn parse_bits(token: &str) -> u32 {
    u32::from_str_radix(token, 16).expect("float column is a hex u32 bit pattern")
}

fn parse_flag(token: &str) -> bool {
    match token {
        "0" => false,
        "1" => true,
        other => panic!("expected 0 or 1, got {other:?}"),
    }
}

fn parse_dump(text: &str) -> Dump {
    let mut state_count = None;
    let mut block_count = None;
    let mut constants = BTreeMap::new();
    let mut flags = BTreeMap::new();
    let mut entity_inside = BTreeMap::new();
    let mut blocks = Vec::new();
    let mut legacy_solid: Vec<bool> = Vec::new();
    let mut blocks_motion: Vec<bool> = Vec::new();
    let mut geometry_solid: Vec<bool> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        match tok.next().expect("record kind") {
            "C" => {
                assert!(state_count.is_none(), "duplicate C record");
                state_count = Some(
                    tok.next()
                        .expect("state count")
                        .parse::<usize>()
                        .expect("state count is a usize"),
                );
                block_count = Some(
                    tok.next()
                        .expect("block count")
                        .parse::<usize>()
                        .expect("block count is a usize"),
                );
            }
            "K" => {
                let name = tok.next().expect("block name").to_owned();
                let entry = Constants {
                    friction_bits: parse_bits(tok.next().expect("friction")),
                    speed_factor_bits: parse_bits(tok.next().expect("speed factor")),
                    jump_factor_bits: parse_bits(tok.next().expect("jump factor")),
                    bounce_restitution_bits: parse_bits(tok.next().expect("bounce restitution")),
                    climbable: parse_flag(tok.next().expect("climbable")),
                    suppresses_bounce: parse_flag(tok.next().expect("suppresses bounce")),
                };
                assert!(tok.next().is_none(), "trailing tokens on {line:?}");
                assert!(
                    constants.insert(name.clone(), entry).is_none(),
                    "duplicate K record for {name}"
                );
            }
            "F" => {
                let name = tok.next().expect("block name").to_owned();
                let entry = SolidFlags {
                    force_on: parse_flag(tok.next().expect("forceSolidOn")),
                    force_off: parse_flag(tok.next().expect("forceSolidOff")),
                    dynamic_shape: parse_flag(tok.next().expect("dynamicShape")),
                };
                assert!(tok.next().is_none(), "trailing tokens on {line:?}");
                assert!(
                    flags.insert(name.clone(), entry).is_none(),
                    "duplicate F record for {name}"
                );
            }
            "E" => {
                let name = tok.next().expect("block name").to_owned();
                let class = tok.next().expect("declaring class").to_owned();
                assert!(
                    entity_inside.insert(name.clone(), class).is_none(),
                    "duplicate E record for {name}"
                );
            }
            "B" => {
                let start: usize = tok
                    .next()
                    .expect("block start state id")
                    .parse()
                    .expect("start is a usize");
                let name = tok.next().expect("block name").to_owned();
                blocks.push((start, name));
            }
            "P" => {
                let kind = tok.next().expect("bit family");
                let start: usize = tok
                    .next()
                    .expect("start state id")
                    .parse()
                    .expect("start is a usize");
                let target = match kind {
                    "L" => &mut legacy_solid,
                    "M" => &mut blocks_motion,
                    "G" => &mut geometry_solid,
                    other => panic!("unknown bit family {other:?}"),
                };
                assert_eq!(start, target.len(), "P {kind} records are not in state order");
                for ch in tok.next().expect("bitstring").chars() {
                    target.push(match ch {
                        '0' => false,
                        '1' => true,
                        other => panic!("bitstring holds {other:?}"),
                    });
                }
                assert!(tok.next().is_none(), "trailing tokens on {line:?}");
            }
            other => panic!("unknown record kind {other:?} on {line:?}"),
        }
    }

    let state_count = state_count.expect("dump carries a C record");
    let block_count = block_count.expect("C record carries a block count");
    assert_eq!(legacy_solid.len(), state_count, "L bit count");
    assert_eq!(blocks_motion.len(), state_count, "M bit count");
    assert_eq!(geometry_solid.len(), state_count, "G bit count");
    assert_eq!(constants.len(), block_count, "K record count");
    assert_eq!(flags.len(), block_count, "F record count");
    assert_eq!(blocks.len(), block_count, "B record count");

    Dump {
        state_count,
        block_count,
        constants,
        flags,
        entity_inside,
        blocks,
        legacy_solid,
        blocks_motion,
        geometry_solid,
    }
}

/// Packs a per-state bool list into a bitset, bit `i` of byte `i / 8` at
/// `1 << (i % 8)` — the layout [`block_solidity`] reads.
fn pack(bits: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; bits.len().div_ceil(8)];
    for (index, &set) in bits.iter().enumerate() {
        if set {
            out[index / 8] |= 1u8 << (index % 8);
        }
    }
    out
}

/// Renders the committed `block_solidity.rs` source from the parsed dump.
fn generate(dump: &Dump) -> String {
    let count = dump.state_count;
    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test block_physics -- --ignored`\n\
         // from tests/support/block_physics_jvm.txt (a headless 26.2 server dump of\n\
         // BlockStateBase.isSolid()/blocksMotion(), protocol 776 / Minecraft 26.2).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated per-block-state `legacySolid`/`blocksMotion` bitsets for\n\
         //! protocol 776 (Minecraft 26.2), indexed by global block-state id.\n\
         //! Consumed by [`crate::block_solidity`].\n\n",
    );
    let _ = writeln!(out, "/// Number of block states (ids are `0..STATE_COUNT`).");
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {count};\n");

    emit_bitset(
        &mut out,
        "LEGACY_SOLID",
        "`BlockStateBase.isSolid()` (the cached `legacySolid` flag)",
        &dump.legacy_solid,
    );
    emit_bitset(
        &mut out,
        "BLOCKS_MOTION",
        "`BlockStateBase.blocksMotion()` (`legacySolid` net of the cobweb / \
         bamboo-sapling exclusions)",
        &dump.blocks_motion,
    );

    out
}

fn emit_bitset(out: &mut String, name: &str, description: &str, bits: &[bool]) {
    let packed = pack(bits);
    let set = bits.iter().filter(|&&b| b).count();
    let _ = writeln!(
        out,
        "/// Per-state {description}, packed one bit per state:\n\
         /// bit `id % 8` of byte `id / 8`. {set} of {} states are set.",
        bits.len()
    );
    let _ = writeln!(out, "pub static {name}: [u8; {}] = [", packed.len());
    for chunk in packed.chunks(16) {
        out.push_str("    ");
        for byte in chunk {
            let _ = write!(out, "0x{byte:02x}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n\n");
}

// ---------------------------------------------------------------------------
// Half 1: the state-keyed solidity census
// ---------------------------------------------------------------------------

/// The strongest check on the generated table: every bit the shipped accessors
/// return equals the bit the real server produced, for all 32,366 states, for
/// both families. Non-vacuous by construction.
#[test]
fn committed_table_matches_the_committed_dump() {
    let dump = parse_dump(DUMP);
    assert_eq!(
        dump.state_count,
        block_solidity::STATE_COUNT as usize,
        "dump/table state count mismatch"
    );
    for state in 0..dump.state_count {
        let id = state as u32;
        assert_eq!(
            block_solidity::legacy_solid(id),
            Some(dump.legacy_solid[state]),
            "legacySolid for state {id} ({:?})",
            block_states::block_name(id)
        );
        assert_eq!(
            block_solidity::blocks_motion(id),
            Some(dump.blocks_motion[state]),
            "blocksMotion for state {id} ({:?})",
            block_states::block_name(id)
        );
    }
    assert_eq!(dump.state_count, 32_366, "expected 32,366 block states");
}

/// Reconciles the dump's block boundaries against `block_states::block_name`,
/// which is generated from `blocks.json` — a second, independently produced
/// Mojang artifact. This is what makes indexing the bitsets by block-state id
/// safe.
#[test]
fn dump_block_boundaries_match_the_block_state_table() {
    let dump = parse_dump(DUMP);
    assert_eq!(dump.block_count, 1_196, "expected 1,196 blocks");
    for (state, name) in dump.block_names().iter().enumerate() {
        assert_eq!(
            block_states::block_name(state as u32),
            Some(*name),
            "blocks.json and the JVM dump disagree at state {state}"
        );
    }
}

#[test]
fn count_matches_block_state_table() {
    assert_eq!(
        block_solidity::STATE_COUNT,
        block_states::STATE_COUNT,
        "the solidity table must cover exactly the block-state id space"
    );
}

#[test]
fn ids_are_contiguous_and_out_of_range_is_none() {
    let count = block_solidity::STATE_COUNT;
    for id in 0..count {
        assert!(block_solidity::legacy_solid(id).is_some(), "id {id} legacySolid");
        assert!(block_solidity::blocks_motion(id).is_some(), "id {id} blocksMotion");
    }
    assert_eq!(block_solidity::legacy_solid(count), None);
    assert_eq!(block_solidity::legacy_solid(u32::MAX), None);
    assert_eq!(block_solidity::blocks_motion(count), None);
    assert_eq!(block_solidity::blocks_motion(u32::MAX), None);
}

/// `blocksMotion` is `legacySolid` minus exactly two blocks in 26.2. Pinned as a
/// completeness statement about vanilla's hard-coded exclusion list — if a third
/// block joins it, this fails rather than the shell quietly disagreeing with the
/// server about one cell.
#[test]
fn blocks_motion_differs_from_legacy_solid_on_exactly_cobweb_and_bamboo_sapling() {
    let mut differ = BTreeSet::new();
    for id in 0..block_solidity::STATE_COUNT {
        if block_solidity::legacy_solid(id) != block_solidity::blocks_motion(id) {
            differ.insert(block_states::block_name(id).expect("named"));
            assert_eq!(
                block_solidity::blocks_motion(id),
                Some(false),
                "the exclusion list can only turn blocksMotion *off* (state {id})"
            );
        }
    }
    assert_eq!(
        differ.into_iter().collect::<Vec<_>>(),
        vec!["minecraft:bamboo_sapling", "minecraft:cobweb"],
        "vanilla's blocksMotion exclusion list changed"
    );
}

/// Vanilla's own `calculateSolid` geometry branch, replayed by the oracle with the
/// two `forceSolid*` early-returns deleted, versus the truth. **This is the
/// measurement that justifies the census existing**: it is computed in the JVM
/// against the same `Cache.collisionShape` this repo's collision census dumps, so
/// it is not our arithmetic being graded against our own data.
#[test]
fn the_geometry_branch_alone_is_wrong_for_two_thousand_states() {
    let dump = parse_dump(DUMP);
    let names = dump.block_names();
    let mut wrong_states = 0usize;
    let mut wrong_blocks = BTreeSet::new();
    let mut false_negatives = 0usize; // vanilla blocks motion, geometry says no
    let mut false_positives = 0usize;
    for state in 0..dump.state_count {
        if dump.legacy_solid[state] != dump.geometry_solid[state] {
            wrong_states += 1;
            wrong_blocks.insert(names[state]);
            if dump.legacy_solid[state] {
                false_negatives += 1;
            } else {
                false_positives += 1;
            }
        }
    }
    assert_eq!(wrong_states, 2_742, "geometry-derived solidity error count changed");
    assert_eq!(wrong_blocks.len(), 222, "affected block count changed");
    assert_eq!(false_negatives, 2_645, "false-negative count changed");
    assert_eq!(false_positives, 97, "false-positive count changed");

    // Every disagreement is explained by one of the three non-geometry branches,
    // and each branch is populated — so none of the counts above is an artifact
    // of a single mechanism.
    let force_on = dump.flags.values().filter(|f| f.force_on).count();
    let force_off = dump.flags.values().filter(|f| f.force_off).count();
    let dynamic = dump.flags.values().filter(|f| f.dynamic_shape).count();
    assert_eq!(force_on, 237, "forceSolidOn block count changed");
    assert_eq!(force_off, 8, "forceSolidOff block count changed");
    assert_eq!(dynamic, 23, "dynamicShape block count changed");
    for name in &wrong_blocks {
        let flags = dump.flags[*name];
        assert!(
            flags.force_on || flags.force_off || flags.dynamic_shape,
            "{name} disagrees with the geometry branch but sets none of \
             forceSolidOn/forceSolidOff/dynamicShape — calculateSolid has grown a \
             fourth branch"
        );
    }
}

/// The **control** for the whole census, and for this change not being a no-op:
/// the derivation the shell shipped before this table existed — `shape_is_solid`
/// over the committed *collision* census, plus the three hard-coded names — is
/// asserted here to give the **wrong** answer on a stated number of states, and
/// the named blocks are asserted individually in both directions.
///
/// Written against `collision_shapes` directly (not through `lodestone-shell`,
/// which this crate cannot depend on) but reproducing `shape_is_solid`'s body
/// line for line, so the number is the one the shell actually had.
#[test]
fn the_shipped_shape_derivation_gets_a_measured_set_of_blocks_wrong() {
    /// Verbatim reproduction of `lodestone_shell::collision::shape_is_solid`.
    fn shape_is_solid(shape: &[lodestone_model::BlockAabb]) -> bool {
        let mut it = shape.iter();
        let Some(first) = it.next() else { return false };
        let (mut min, mut max) = (first.min, first.max);
        for b in it {
            for a in 0..3 {
                min[a] = min[a].min(b.min[a]);
                max[a] = max[a].max(b.max[a]);
            }
        }
        let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        let mean = f64::from(size[0] + size[1] + size[2]) / 3.0;
        mean >= 0.7291666666666666 || f64::from(size[1]) >= 1.0
    }

    let mut wrong_states = 0usize;
    let mut wrong_blocks = BTreeSet::new();
    let mut missed_blocking = 0usize;
    let mut invented_blocking = BTreeSet::new();
    for id in 0..block_solidity::STATE_COUNT {
        let name = block_states::block_name(id).expect("named");
        let old = match name {
            "minecraft:cobweb" | "minecraft:bamboo_sapling" | "minecraft:ladder" => false,
            _ => shape_is_solid(collision_shapes::collision_boxes(id).expect("resolves")),
        };
        let truth = block_solidity::blocks_motion(id).expect("resolves");
        if old != truth {
            wrong_states += 1;
            wrong_blocks.insert(name);
            if truth {
                missed_blocking += 1;
            } else {
                invented_blocking.insert(name);
            }
        }
    }
    assert_eq!(
        wrong_states, 2_618,
        "the pre-census derivation's error count changed — if this moved, the \
         numbers in docs/block-physics-constants.md moved with it"
    );
    assert_eq!(wrong_blocks.len(), 202, "affected block count changed");
    assert_eq!(missed_blocking, 2_497, "false-negative state count changed");
    // The over-blocking half is small enough to name in full.
    assert_eq!(
        invented_blocking.into_iter().collect::<Vec<_>>(),
        vec![
            "minecraft:azalea",
            "minecraft:big_dripleaf",
            "minecraft:chorus_flower",
            "minecraft:chorus_plant",
            "minecraft:end_rod",
            "minecraft:flowering_azalea",
            "minecraft:scaffolding",
            "minecraft:snow",
        ],
        "the set of blocks the old derivation over-blocked changed"
    );

    // Named, both directions, so the control cannot pass by counting alone.
    for name in [
        "minecraft:oak_sign",
        "minecraft:stone_pressure_plate",
        "minecraft:iron_chain",
        "minecraft:white_banner",
        "minecraft:lantern",
        "minecraft:turtle_egg",
        "minecraft:conduit",
        "minecraft:dead_tube_coral",
    ] {
        let id = first_id_named(name);
        assert!(
            block_solidity::blocks_motion(id).expect("resolves"),
            "{name} must block motion"
        );
        assert!(
            !shape_is_solid(collision_shapes::collision_boxes(id).expect("resolves")),
            "{name}'s collision shape must be too thin for the geometry branch — \
             this is the false negative the census fixes"
        );
    }
    for name in [
        "minecraft:azalea",
        "minecraft:flowering_azalea",
        "minecraft:big_dripleaf",
        "minecraft:chorus_plant",
        "minecraft:chorus_flower",
        // `scaffolding` is the *third* branch, not a `forceSolidOff`: it is a
        // `dynamicShape()` block, so vanilla never builds its shape cache and
        // `calculateSolid` returns false before it ever looks at geometry — while
        // the collision census does report its standing shape.
        "minecraft:scaffolding",
    ] {
        let id = first_id_named(name);
        assert!(
            !block_solidity::blocks_motion(id).expect("resolves"),
            "{name} must not block motion"
        );
        assert!(
            shape_is_solid(collision_shapes::collision_boxes(id).expect("resolves")),
            "{name}'s collision shape must look solid to the geometry branch — \
             this is the false positive the census fixes"
        );
    }

    // A ladder is the reason the threshold constant is what it is: its mean
    // extent is *exactly* `(1 + 1 + 3/16) / 3`, so it lands on the `>=` and the
    // geometry branch calls it solid. `forceSolidOff()` on `Blocks.LADDER` is
    // what makes vanilla disagree, and the old code hard-coded that one case.
    let ladder = first_id_named("minecraft:ladder");
    let boxes = collision_shapes::collision_boxes(ladder).expect("resolves");
    assert!(shape_is_solid(boxes), "a ladder lands on the threshold");
    assert!(
        !block_solidity::blocks_motion(ladder).expect("resolves"),
        "vanilla's ladder does not block motion"
    );
    let b = boxes[0];
    let mean = f64::from((b.max[0] - b.min[0]) + (b.max[1] - b.min[1]) + (b.max[2] - b.min[2])) / 3.0;
    assert_eq!(mean, 0.7291666666666666, "the threshold constant *is* a ladder");
}

/// Two blocks a player meets constantly, pinned by hand against the decompiled
/// source so the table is not merely self-consistent.
#[test]
fn hand_checked_solidity_rows() {
    // `Blocks.SLIME_BLOCK` registers with no forceSolid* call and a
    // full cube, so geometry and truth agree: solid.
    assert_eq!(
        block_solidity::blocks_motion(first_id_named("minecraft:slime_block")),
        Some(true)
    );
    // `WebBlock`'s `noCollission()` empties its collision shape, its
    // `forceSolidOn()` makes `legacySolid` true anyway, and `blocksMotion`'s own
    // `block != COBWEB` clause turns it back off. All three layers, one block.
    let cobweb = first_id_named("minecraft:cobweb");
    assert_eq!(block_solidity::legacy_solid(cobweb), Some(true));
    assert_eq!(block_solidity::blocks_motion(cobweb), Some(false));
    assert!(collision_shapes::collision_boxes(cobweb).expect("resolves").is_empty());
    // Air blocks a nothing.
    assert_eq!(
        block_solidity::blocks_motion(first_id_named("minecraft:air")),
        Some(false)
    );
}

// ---------------------------------------------------------------------------
// Half 2: the name-keyed constants in `lodestone-model`
// ---------------------------------------------------------------------------

/// The anchor for `lodestone_model::block_physics`. Replays all 1,196 blocks the
/// real server registered and demands the version-free table agree on every one
/// of `friction`, `speed_factor`, `jump_factor`, `bounce_restitution` and
/// `climbable`, bit-exactly.
///
/// This is deliberately exhaustive rather than a spot-check of the ~30 rows that
/// are not defaults: a table that answers `0.6` for ice passes any test that only
/// looks at the rows the table itself mentions. The remaining 1,166 blocks are
/// where a wrong *default* would hide.
#[test]
fn name_keyed_constants_match_the_committed_dump_for_every_block() {
    let dump = parse_dump(DUMP);
    let mut checked = 0usize;
    for (name, want) in &dump.constants {
        let got = block_physics(name);
        assert_eq!(
            got.friction.to_bits(),
            want.friction_bits,
            "friction for {name}: table {:?} vs server f32::from_bits(0x{:08x})",
            got.friction,
            want.friction_bits
        );
        assert_eq!(
            got.speed_factor.to_bits(),
            want.speed_factor_bits,
            "speed_factor for {name}: table {:?}",
            got.speed_factor
        );
        assert_eq!(
            got.jump_factor.to_bits(),
            want.jump_factor_bits,
            "jump_factor for {name}: table {:?}",
            got.jump_factor
        );
        // `bounce_restitution` is contracted as already net of
        // `BlockTags.SUPPRESSES_BOUNCE`, so a suppressed block must read 0.0
        // whatever `getBounceRestitution` says.
        let expected_bounce = if want.suppresses_bounce {
            0.0f32
        } else {
            f32::from_bits(want.bounce_restitution_bits)
        };
        assert_eq!(
            got.bounce_restitution.to_bits(),
            expected_bounce.to_bits(),
            "bounce_restitution for {name}: table {:?} vs server {expected_bounce:?} \
             (suppressed = {})",
            got.bounce_restitution,
            want.suppresses_bounce
        );
        assert_eq!(got.climbable, want.climbable, "climbable for {name}");
        checked += 1;
    }
    assert_eq!(checked, 1_196, "expected 1,196 blocks checked");
}

/// The 26.2 blocks that set *any* non-default movement constant, enumerated from
/// the dump. Pinned so a data bump that adds a slippery or bouncy block fails
/// here — the `lodestone-model` table would otherwise silently answer the
/// default for it, and nothing else would notice.
#[test]
fn only_twenty_three_blocks_set_a_non_default_movement_constant() {
    let dump = parse_dump(DUMP);
    let default_friction = DEFAULT_BLOCK_PHYSICS.friction.to_bits();
    let default_one = 1.0f32.to_bits();
    let non_default: Vec<&str> = dump
        .constants
        .iter()
        .filter(|(_, c)| {
            c.friction_bits != default_friction
                || c.speed_factor_bits != default_one
                || c.jump_factor_bits != default_one
                || c.bounce_restitution_bits != 0
        })
        .map(|(name, _)| name.as_str())
        .collect();
    // 16 beds + ice/packed_ice/frosted_ice/blue_ice + slime + soul_sand + honey.
    assert_eq!(
        non_default.len(),
        23,
        "the set of blocks with a non-default movement constant changed: {non_default:?}"
    );
    assert_eq!(
        non_default.iter().filter(|n| n.ends_with("_bed")).count(),
        16,
        "all 16 dyed beds must carry bounceRestitution"
    );
    for name in [
        "minecraft:ice",
        "minecraft:packed_ice",
        "minecraft:frosted_ice",
        "minecraft:blue_ice",
        "minecraft:slime_block",
        "minecraft:soul_sand",
        "minecraft:honey_block",
    ] {
        assert!(non_default.contains(&name), "{name} must be non-default");
    }
}

/// `BlockTags.CLIMBABLE`, from the dump rather than from the tag JSON we read by
/// hand. Nine members in 26.2.
#[test]
fn climbable_is_exactly_the_nine_tagged_blocks() {
    let dump = parse_dump(DUMP);
    let climbable: Vec<&str> = dump
        .constants
        .iter()
        .filter(|(_, c)| c.climbable)
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        climbable,
        vec![
            "minecraft:cave_vines",
            "minecraft:cave_vines_plant",
            "minecraft:ladder",
            "minecraft:scaffolding",
            "minecraft:twisting_vines",
            "minecraft:twisting_vines_plant",
            "minecraft:vine",
            "minecraft:weeping_vines",
            "minecraft:weeping_vines_plant",
        ],
        "BlockTags.CLIMBABLE membership changed"
    );
}

/// `BlockTags.SUPPRESSES_BOUNCE`'s only member in 26.2 is `honey_block`, which
/// sets no restitution — so folding the tag in changes nothing today, and
/// `bounce_restitution`'s "already net of the tag" contract is currently
/// unexercised. Pinned because a future bouncy suppressor would otherwise break
/// it **silently**, which is exactly the hazard `docs/collision-shapes.md`
/// flagged and could not gate.
#[test]
fn the_bounce_suppression_tag_is_currently_a_no_op_and_that_is_load_bearing() {
    let dump = parse_dump(DUMP);
    let suppressors: Vec<&str> = dump
        .constants
        .iter()
        .filter(|(_, c)| c.suppresses_bounce)
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        suppressors,
        vec!["minecraft:honey_block"],
        "BlockTags.SUPPRESSES_BOUNCE membership changed"
    );
    for name in &suppressors {
        assert_eq!(
            dump.constants[*name].bounce_restitution_bits, 0,
            "{name} now suppresses bounce *and* sets a restitution, so \
             lodestone_model::block_physics must subtract the tag explicitly"
        );
    }
}

/// `makeStuckInBlock`'s vector is imperative code inside `Block.entityInside`, so
/// it cannot be dumped as data. What *can* be checked mechanically is
/// completeness: every block `lodestone-model` gives a `stuck_multiplier` must
/// override `entityInside`, and the override census is 61 blocks — so the
/// three-row table is checked against a real candidate set rather than asserted
/// to be exhaustive.
#[test]
fn stuck_multiplier_is_only_set_by_blocks_that_override_entity_inside() {
    let dump = parse_dump(DUMP);
    assert_eq!(
        dump.entity_inside.len(),
        61,
        "the entityInside override census changed"
    );
    let with_stuck: Vec<&str> = dump
        .constants
        .keys()
        .filter(|name| block_physics(name).stuck_multiplier.is_some())
        .map(String::as_str)
        .collect();
    assert_eq!(
        with_stuck,
        vec![
            "minecraft:cobweb",
            "minecraft:powder_snow",
            "minecraft:sweet_berry_bush",
        ],
        "the stuck-multiplier table changed"
    );
    for name in &with_stuck {
        assert!(
            dump.entity_inside.contains_key(*name),
            "{name} carries a stuck multiplier but does not override entityInside"
        );
    }
    // The three declaring classes, pinned: if `WebBlock` stops overriding
    // `entityInside`, the row above is stale and this says so.
    assert_eq!(
        dump.entity_inside["minecraft:cobweb"],
        "net.minecraft.world.level.block.WebBlock"
    );
    assert_eq!(
        dump.entity_inside["minecraft:powder_snow"],
        "net.minecraft.world.level.block.PowderSnowBlock"
    );
    assert_eq!(
        dump.entity_inside["minecraft:sweet_berry_bush"],
        "net.minecraft.world.level.block.SweetBerryBushBlock"
    );
}

/// An unknown name must fall back to vanilla's defaults, not panic and not to
/// some other block's row — the shell reaches this whenever `block_name` cannot
/// resolve a state.
#[test]
fn unknown_names_get_vanilla_defaults() {
    for name in ["", "minecraft:", "minecraft:not_a_block", "ice", "_bed", "bed"] {
        let got = block_physics(name);
        assert_eq!(got.friction, 0.6, "{name} friction");
        assert_eq!(got.speed_factor, 1.0, "{name} speed factor");
        assert_eq!(got.jump_factor, 1.0, "{name} jump factor");
        assert_eq!(got.bounce_restitution, 0.0, "{name} bounce");
        assert_eq!(got.stuck_multiplier, None, "{name} stuck");
        assert!(!got.climbable, "{name} climbable");
    }
    // ...and the suffix match on `_bed` must not swallow a hypothetical
    // `minecraft:seabed`-style name from another namespace.
    assert_eq!(block_physics("modded:copper_bed").bounce_restitution, 0.0);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Finds the first state id whose block name matches `name`, via the committed
/// block-state table — robust to id shifts across data bumps.
fn first_id_named(name: &str) -> u32 {
    (0..block_states::STATE_COUNT)
        .find(|&id| block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} present in the block-state table"))
}

// ---------------------------------------------------------------------------
// Drift guard
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates/verifies the committed table; run explicitly"]
fn committed_table_matches_dump() {
    let dump = parse_dump(DUMP);
    let generated = generate(&dump);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated, committed,
        "src/generated/block_solidity.rs is stale vs the JVM dump; regenerate with \
         LODESTONE_REGEN=1"
    );
}
