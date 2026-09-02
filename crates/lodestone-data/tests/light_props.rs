//! Per-block-state light dampening/emission table: hermetic checks over the
//! committed table, plus an `#[ignore]`d drift guard that regenerates it from
//! the committed source data and asserts byte-for-byte equality (modelled on
//! `hardness.rs`). The generator lives here so the checked-in table can never
//! silently drift.
//!
//! # Data provenance, and why it is not a JVM dump
//!
//! Every other per-block-state table in this crate is dumped from the real 26.2
//! jar. This one is not, and the reason is structural rather than laziness:
//! vanilla's own "get light dampening" and "get light emission" accessors are **not** exposed on
//! its own block-properties builder. Light emission is a per-state function
//! stored on a private field (read once
//! into a private field by the block-state base class's own
//! constructor), and dampening is a protected
//! method with per-block overrides (overridden by
//! the leaves and tinted-glass block classes). Reading either needs the running jar,
//! and this table is generated from two *committed* sources instead:
//!
//! 1. **`tests/support/light_props_mcdata.txt`** — a committed extract of
//!    `vendor/minecraft-data/data/pc/1.21.11/blocks.json`'s `filterLight` /
//!    `emitLight`, which are that project's transcription of exactly these two
//!    vanilla quantities for the block's **default** state. Committed rather than
//!    read from `vendor/` because `vendor/` is **gitignored** — see [`MCDATA`].
//! 2. **The decompiled 26.2 tree** (`.cache/mc/26.2/src`), read for the
//!    per-state corrections and for the 30 blocks 1.21.11 does not have.
//!
//! Source 1 was cross-checked against source 2's own formula
//! (vanilla's own "get light dampening" accessor = `isSolidRender() ? 15 : propagatesSkylightDown() ? 0 : 1`)
//! on the cases that formula separates:
//! `stone` → 15 (full occlusion shape), `stone_slab` → 0 (not a full occlusion
//! shape, shape not full so skylight propagates), `water` → 1 (fluid state
//! non-empty, so skylight does *not* propagate), `tinted_glass` → 15 (the
//! tinted-glass block's own override), `oak_leaves` → 1, `ice` → 1. Two independent
//! sources agreeing on the cases that discriminate is the evidence here; a
//! single source restating itself would not be.
//!
//! # The three per-state corrections, and their direction
//!
//! `blocks.json` is keyed **per block**, and vanilla's values are **per state**.
//! Three of those divergences are derivable from the committed
//! [`block_states::properties`] alone, and all three are applied by [`generate`]:
//!
//! | property | correction | why |
//! |---|---|---|
//! | `type=double` | dampening `15` | a double slab *is* a full cube, so `isSolidRender()` holds where the block's default (`bottom`) fails it |
//! | `waterlogged=true` | dampening `max(1, ·)` | vanilla's own "propagates skylight down" default requires `fluidState.isEmpty()`, so a waterlogged non-solid costs 1, not 0 |
//! | `lit=false` | emission `0` | an unlit furnace/campfire/redstone torch emits nothing; `blocks.json` records whichever the block's *default* state is |
//!
//! **Every correction, and every residual gap, moves the table toward darker or
//! more-occluding — never brighter.** That is deliberate and load-bearing:
//! `crates/protocol/v770/tests/live_terrain_light.rs` proves the engine against a
//! real vanilla server by asserting we never produce light the server does not,
//! precisely because a props shortfall cannot fake that direction. A table that
//! could over-light would destroy that argument. The known residual gaps are all
//! on the dark side:
//!
//! * `lit=true` families whose *default* state is unlit (`furnace`,
//!   `redstone_lamp`, `redstone_ore`, `copper_bulb`, …) read as emission `0`.
//! * `cave_vines[berries=true]` (14) and `glow_lichen` (7) read as `0` —
//!   `blocks.json` records both as `emitLight=0`, a known upstream gap.
//! * `minecraft:light[level=N]` reads as 15 for every level.
//!
//! None of those blocks is placed by `lodestone-worldgen`'s overworld generator
//! (shape + aquifer + surface + carvers + ores + vegetation), so the integrated
//! server's own terrain is unaffected; they matter only for player-placed blocks.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-extract source 1 from the (gitignored) vendored data, keeping the `#`
//!    header, and commit the result:
//!
//! ```text
//! python3 - <<'PY' > crates/lodestone-data/tests/support/light_props_mcdata.txt
//! import json
//! bd = json.load(open('vendor/minecraft-data/data/pc/1.21.11/blocks.json'))
//! rows = sorted((b['name'], int(b.get('filterLight', 0)), int(b.get('emitLight', 0)))
//!               for b in bd)
//! print("# Per-block light dampening/emission, extracted from")
//! print("# vendor/minecraft-data/data/pc/1.21.11/blocks.json (fields filterLight/emitLight).")
//! print("# Columns: block-name filterLight emitLight. Sorted by name.")
//! print("# Committed as the external anchor for crates/lodestone-data/src/generated/light_props.rs")
//! print("# because vendor/ is not repo state. Refresh command is in tests/light_props.rs's module docs.")
//! print(f"# rows: {len(rows)}")
//! for n, f, e in rows:
//!     print(n, f, e)
//! PY
//! ```
//!
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test light_props \
//!     committed_table_matches_source -- --ignored --nocapture
//! ```
//!
//! If the bump adds blocks the extract does not have,
//! [`unmapped_block_set_is_exactly_the_known_26_2_additions`] fails **naming
//! them** — deliberately, so a new block can never be silently defaulted.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::block_states;
use lodestone_data::light_props;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/light_props.rs")
}

/// The committed extract of `vendor/minecraft-data`'s 1.21.11 `filterLight`/
/// `emitLight`, one `name dampening emission` row per block.
///
/// **`include_str!`, not a path into `vendor/`.** `vendor/minecraft-data` is
/// gitignored — not even a submodule — so a fresh checkout or a throwaway
/// `git worktree` does not have it, and a non-`#[ignore]`d test that read it
/// would fail there while passing on the machine that wrote it. (Caught exactly
/// that way: these two tests were green in the main checkout and red in a
/// detached verification worktree.) `crates/protocol/v770/tests/live_terrain_light.rs`
/// gets away with reading `vendor/` directly only because it is both
/// feature-gated and `#[ignore]`d. Committing the extract is the same external
/// anchor the sibling generators use (`support/*_jvm.txt`).
const MCDATA: &str = include_str!("support/light_props_mcdata.txt");

/// The 30 blocks 26.2 adds that `vendor/minecraft-data 1.21.11` does not carry,
/// with the `(dampening, emission)` read out of their registrations in
/// vanilla's own block-registration source (26.2, de-obfuscated):
///
/// * The nine full cubes — `SULFUR`/`CINNABAR` and their `polished_`,
///   `_bricks`, `chiseled_` and `POTENT_SULFUR` copies — are plain
///   default-properties builders with **no** no-occlusion flag (and its
///   legacy-copy/full-copy sibling builders), so
///   their occlusion shape is a full cube, solid-render holds and dampening is
///   15.
/// * The eighteen `_slab`/`_stairs`/`_wall` variants are register-slab/
///   register-stair/register-wall copies, whose occlusion shape is not a full
///   cube; their shape is not full either, so skylight propagates and dampening
///   is 0. (`type=double` slabs are lifted back to 15 by the per-state
///   correction, which is why this entry is not wrong for them.)
/// * `GOLDEN_DANDELION` is a flower block with a no-collision flag,
///   `POTTED_GOLDEN_DANDELION` a flower-pot block, and
///   `SULFUR_SPIKE` has a no-occlusion flag plus a dynamic-shape flag
///   — none a full cube, so 0.
///
/// **Not one of the 30 calls `.lightLevel(...)`**, so every emission is 0.
const NEW_IN_26_2: &[(&str, u8, u8)] = &[
    ("chiseled_cinnabar", 15, 0),
    ("chiseled_sulfur", 15, 0),
    ("cinnabar", 15, 0),
    ("cinnabar_brick_slab", 0, 0),
    ("cinnabar_brick_stairs", 0, 0),
    ("cinnabar_brick_wall", 0, 0),
    ("cinnabar_bricks", 15, 0),
    ("cinnabar_slab", 0, 0),
    ("cinnabar_stairs", 0, 0),
    ("cinnabar_wall", 0, 0),
    ("golden_dandelion", 0, 0),
    ("polished_cinnabar", 15, 0),
    ("polished_cinnabar_slab", 0, 0),
    ("polished_cinnabar_stairs", 0, 0),
    ("polished_cinnabar_wall", 0, 0),
    ("polished_sulfur", 15, 0),
    ("polished_sulfur_slab", 0, 0),
    ("polished_sulfur_stairs", 0, 0),
    ("polished_sulfur_wall", 0, 0),
    ("potent_sulfur", 15, 0),
    ("potted_golden_dandelion", 0, 0),
    ("sulfur", 15, 0),
    ("sulfur_brick_slab", 0, 0),
    ("sulfur_brick_stairs", 0, 0),
    ("sulfur_brick_wall", 0, 0),
    ("sulfur_bricks", 15, 0),
    ("sulfur_slab", 0, 0),
    ("sulfur_spike", 0, 0),
    ("sulfur_stairs", 0, 0),
    ("sulfur_wall", 0, 0),
];

/// The committed extract, parsed: `(dampening, emission)` per block *name*, names
/// unprefixed as `blocks.json` writes them. Blank and `#` lines are skipped.
fn mcdata_rows() -> BTreeMap<String, (u8, u8)> {
    let mut out = BTreeMap::new();
    for line in MCDATA.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        let name = tok.next().expect("name column").to_owned();
        let dampening: u8 = tok
            .next()
            .expect("dampening column")
            .parse()
            .expect("dampening is a u8");
        let emission: u8 = tok
            .next()
            .expect("emission column")
            .parse()
            .expect("emission is a u8");
        assert!(
            tok.next().is_none(),
            "unexpected trailing tokens on {line:?}"
        );
        assert!(
            dampening <= 15 && emission <= 15,
            "{name}: light values must be 0..=15, got ({dampening}, {emission})"
        );
        assert!(
            out.insert(name.clone(), (dampening, emission)).is_none(),
            "{name} appears twice in the extract"
        );
    }
    assert_eq!(
        out.len(),
        1166,
        "the committed extract should carry all 1,166 blocks minecraft-data 1.21.11 has"
    );
    out
}

/// `(dampening, emission)` per block *name*, from the extract plus
/// [`NEW_IN_26_2`].
fn props_by_block_name() -> BTreeMap<String, (u8, u8)> {
    let mut out = mcdata_rows();
    for &(name, dampening, emission) in NEW_IN_26_2 {
        // Deliberately `or_insert`: if a future data bump starts carrying one of
        // these, the extract's value wins and `NEW_IN_26_2` becomes dead — which
        // `unmapped_block_set_is_exactly_the_known_26_2_additions` reports.
        out.entry(name.to_owned()).or_insert((dampening, emission));
    }
    out
}

/// Block names in 26.2 (from the committed block-state table) that the extract
/// does not carry.
fn unmapped_block_names() -> BTreeSet<String> {
    let known = mcdata_rows();
    let mut out = BTreeSet::new();
    for id in 0..block_states::STATE_COUNT {
        let full = block_states::block_name(id).expect("state id in range");
        let short = full.strip_prefix("minecraft:").unwrap_or(full);
        if !known.contains_key(short) {
            out.insert(short.to_owned());
        }
    }
    out
}

/// The authoritative per-state pair for `id`: the block's pair, with the three
/// per-state corrections this module's docs justify applied in order.
fn resolved(id: u32, by_name: &BTreeMap<String, (u8, u8)>) -> (u8, u8) {
    let full = block_states::block_name(id).expect("state id in range");
    let short = full.strip_prefix("minecraft:").unwrap_or(full);
    let (mut dampening, mut emission) = *by_name
        .get(short)
        .unwrap_or_else(|| panic!("no light props for block {short} (state {id})"));
    for &(key, value) in block_states::properties(id).unwrap_or(&[]) {
        match (key, value) {
            // A double slab is a full cube: solid-render holds, dampening 15.
            ("type", "double") => dampening = 15,
            // Vanilla's own "propagates skylight down" default requires an empty fluid state.
            ("waterlogged", "true") => dampening = dampening.max(1),
            // `blocks.json` records the *default* state's emission.
            ("lit", "false") => emission = 0,
            _ => {}
        }
    }
    (dampening, emission)
}

/// Renders the committed `light_props.rs` source.
///
/// De-duplicates `(dampening, emission)` pairs in ascending state-id order, the
/// same deterministic scheme `hardness`/`collision_shapes` use. Both values are
/// `0..=15`, so at most 256 pairs exist and the per-state index fits a `u8` —
/// which is why this table is a third the size of its neighbours.
fn generate(by_name: &BTreeMap<String, (u8, u8)>) -> String {
    let count = block_states::STATE_COUNT as usize;

    let mut entry_index: BTreeMap<(u8, u8), usize> = BTreeMap::new();
    let mut distinct: Vec<(u8, u8)> = Vec::new();
    let mut state_entry: Vec<usize> = Vec::with_capacity(count);
    for id in 0..block_states::STATE_COUNT {
        let key = resolved(id, by_name);
        let idx = *entry_index.entry(key).or_insert_with(|| {
            distinct.push(key);
            distinct.len() - 1
        });
        state_entry.push(idx);
    }
    assert!(
        distinct.len() <= usize::from(u8::MAX) + 1,
        "more than 256 distinct light pairs — the u8 per-state index no longer fits"
    );

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test light_props -- --ignored`\n\
         // from vendor/minecraft-data/data/pc/1.21.11/blocks.json (filterLight/emitLight)\n\
         // plus the 26.2 additions and per-state corrections read out of\n\
         // vanilla's own block-registration and block-behaviour source.\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see tests/light_props.rs).\n",
    );
    out.push_str(
        "//! Generated per-block-state light table for protocol 776 (Minecraft 26.2),\n\
         //! indexed by global block-state id. Consumed by [`crate::light_props`].\n\n",
    );

    let _ = writeln!(out, "/// Number of block states (ids are `0..STATE_COUNT`).");
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {count};\n");

    let _ = writeln!(
        out,
        "/// De-duplicated distinct `(dampening, emission)` pairs ({} of them),\n\
         /// indexed by entry index. Both values are `0..=15`.",
        distinct.len()
    );
    let _ = writeln!(
        out,
        "pub static ENTRIES: [(u8, u8); {}] = [",
        distinct.len()
    );
    for &(dampening, emission) in &distinct {
        let _ = writeln!(out, "    ({dampening}, {emission}),");
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Per-state entry index into [`ENTRIES`], indexed by global block-state id."
    );
    let _ = writeln!(out, "pub static STATE_ENTRY: [u8; {count}] = [");
    for chunk in state_entry.chunks(32) {
        out.push_str("    ");
        for idx in chunk {
            let _ = write!(out, "{idx}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n");

    out
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed table
// ---------------------------------------------------------------------------

fn first_id_named(name: &str) -> u32 {
    (0..block_states::STATE_COUNT)
        .find(|&id| block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not a 26.2 block"))
}

/// The state id of `name` carrying every `(key, value)` in `want`.
fn id_named_with(name: &str, want: &[(&str, &str)]) -> u32 {
    (0..block_states::STATE_COUNT)
        .find(|&id| {
            block_states::block_name(id) == Some(name)
                && want.iter().all(|&(k, v)| {
                    block_states::properties(id)
                        .unwrap_or(&[])
                        .iter()
                        .any(|&(hk, hv)| hk == k && hv == v)
                })
        })
        .unwrap_or_else(|| panic!("{name}{want:?} is not a 26.2 block state"))
}

#[test]
fn committed_table_matches_the_committed_sources() {
    // The strongest check: every one of the 32,366 shipped values equals what
    // the two committed sources say, corrections included. Non-vacuous by
    // construction — it iterates the whole id space and compares exact bytes.
    let by_name = props_by_block_name();
    let mut checked = 0usize;
    for id in 0..block_states::STATE_COUNT {
        let want = resolved(id, &by_name);
        let got = light_props::light_props(id)
            .unwrap_or_else(|| panic!("id {id} missing from the committed table"));
        assert_eq!(
            got,
            want,
            "light props mismatch for {} (id {id})",
            block_states::block_name(id).unwrap_or("?")
        );
        checked += 1;
    }
    assert_eq!(
        checked, 32_366,
        "expected 32,366 block states checked, got {checked}"
    );
}

#[test]
fn count_matches_block_state_table() {
    assert_eq!(
        light_props::STATE_COUNT,
        block_states::STATE_COUNT,
        "light table must cover exactly the block-state id space"
    );
}

#[test]
fn out_of_range_ids_are_none() {
    assert_eq!(light_props::light_props(light_props::STATE_COUNT), None);
    assert_eq!(light_props::light_props(u32::MAX), None);
}

/// The cross-check that anchors source 1 against source 2: the six cases
/// vanilla's own "get light dampening" formula
/// (`isSolidRender() ? 15 : propagatesSkylightDown() ? 0 : 1`) separates.
/// `blocks.json` was written by a different project from a different version, so
/// agreement here is two independent readings of the same vanilla behaviour, not
/// one restating itself.
#[test]
fn dampening_matches_vanillas_own_formula_on_the_discriminating_cases() {
    // A dry bottom slab, named explicitly: `first_id_named` would hand back
    // whichever state sorts first (here `type=top, waterlogged=true`, dampening
    // 1 via the waterlogging correction), so this is the fixture-selection trap
    // rather than a table defect — it cost one red run to find.
    let dry_bottom_slab = id_named_with(
        "minecraft:stone_slab",
        &[("type", "bottom"), ("waterlogged", "false")],
    );
    assert_eq!(
        light_props::light_props(dry_bottom_slab).unwrap().0,
        0,
        "dry bottom slab (id {dry_bottom_slab}): no full occlusion shape, shape not full \
         ⇒ skylight propagates"
    );
    for (name, want, why) in [
        ("minecraft:stone", 15u8, "full occlusion shape ⇒ isSolidRender"),
        (
            "minecraft:water",
            1,
            "fluid state non-empty ⇒ propagatesSkylightDown false ⇒ 1",
        ),
        (
            "minecraft:tinted_glass",
            15,
            "TintedGlassBlock overrides getLightDampening to 15",
        ),
        ("minecraft:oak_leaves", 1, "LeavesBlock override"),
        ("minecraft:ice", 1, "not solid-render, does not propagate"),
        ("minecraft:glass", 0, "not solid-render, propagates ⇒ 0"),
        ("minecraft:air", 0, "nothing to dampen"),
    ] {
        let id = first_id_named(name);
        let (dampening, _) = light_props::light_props(id).expect("resolves");
        assert_eq!(dampening, want, "{name} (id {id}): {why}");
    }
}

/// The emission side, pinned to states our own worldgen and the vanilla oracles
/// actually produce.
#[test]
fn emission_matches_vanilla_for_the_sources_worldgen_places() {
    for (name, want) in [
        ("minecraft:lava", 15u8),
        ("minecraft:glowstone", 15),
        ("minecraft:torch", 14),
        ("minecraft:sea_lantern", 15),
        ("minecraft:magma_block", 3),
        ("minecraft:stone", 0),
        ("minecraft:air", 0),
        ("minecraft:water", 0),
    ] {
        let id = first_id_named(name);
        let (_, emission) = light_props::light_props(id).expect("resolves");
        assert_eq!(emission, want, "{name} (id {id}) emission");
    }
}

/// Correction 1, with the *uncorrected* hypothesis stated: a bottom slab is 0
/// and a double slab is 15. A table that took `blocks.json` at face value would
/// give 0 for both, so this assertion lands on one of two computed hypotheses
/// rather than merely checking a sign.
#[test]
fn double_slabs_occlude_and_bottom_slabs_do_not() {
    let bottom = id_named_with(
        "minecraft:stone_slab",
        &[("type", "bottom"), ("waterlogged", "false")],
    );
    let double = id_named_with(
        "minecraft:stone_slab",
        &[("type", "double"), ("waterlogged", "false")],
    );
    assert_eq!(light_props::light_props(bottom).unwrap().0, 0, "bottom slab");
    assert_eq!(
        light_props::light_props(double).unwrap().0,
        15,
        "double slab is a full cube — the per-block source says 0 for it"
    );
}

/// Correction 2: waterlogging costs a level even where the dry state costs none.
#[test]
fn waterlogging_costs_one_level() {
    let dry = id_named_with(
        "minecraft:oak_slab",
        &[("type", "bottom"), ("waterlogged", "false")],
    );
    let wet = id_named_with(
        "minecraft:oak_slab",
        &[("type", "bottom"), ("waterlogged", "true")],
    );
    assert_eq!(light_props::light_props(dry).unwrap().0, 0, "dry slab");
    assert_eq!(
        light_props::light_props(wet).unwrap().0,
        1,
        "waterlogged slab: fluid state non-empty ⇒ skylight does not propagate"
    );
}

/// Correction 3, and the one that keeps the table on the dark side of vanilla:
/// an unlit redstone torch emits nothing, where the per-block source records the
/// *lit* default's 7 for every state.
#[test]
fn unlit_states_emit_nothing() {
    let lit = id_named_with("minecraft:redstone_torch", &[("lit", "true")]);
    let unlit = id_named_with("minecraft:redstone_torch", &[("lit", "false")]);
    assert_eq!(light_props::light_props(lit).unwrap().1, 7, "lit torch");
    assert_eq!(
        light_props::light_props(unlit).unwrap().1,
        0,
        "unlit torch must not emit — the per-block source says 7 for it"
    );
}

/// The table must never claim *more* light than vanilla, because
/// `live_terrain_light.rs`'s soundness argument is exactly that a props gap can
/// only darken us. Emission 15 is vanilla's maximum and dampening 15 fully
/// occludes, so the checkable form of that invariant is that no entry exceeds
/// the range — plus the two hand-checked over-claim candidates above.
#[test]
fn every_entry_is_in_range_and_every_id_resolves() {
    let mut seen_emitters = 0usize;
    for id in 0..light_props::STATE_COUNT {
        let (dampening, emission) = light_props::light_props(id)
            .unwrap_or_else(|| panic!("id {id} did not resolve"));
        assert!(dampening <= 15, "id {id} dampening {dampening} > 15");
        assert!(emission <= 15, "id {id} emission {emission} > 15");
        if emission > 0 {
            seen_emitters += 1;
        }
    }
    assert!(
        seen_emitters > 100,
        "only {seen_emitters} emitting states — a table of all-zero emission would pass \
         every other test here, so this is the vacuity floor"
    );
}

/// The control on the data gap: the set of 26.2 blocks `vendor/minecraft-data`
/// does not carry must be **exactly** the 30 [`NEW_IN_26_2`] covers. A version
/// bump that adds a block outside that list fails here, naming it, instead of
/// silently taking its default — which is how a new emitter would otherwise ship
/// as opaque and unlit.
#[test]
fn unmapped_block_set_is_exactly_the_known_26_2_additions() {
    let unmapped = unmapped_block_names();
    let known: BTreeSet<String> = NEW_IN_26_2
        .iter()
        .map(|&(name, _, _)| name.to_owned())
        .collect();
    assert_eq!(
        unmapped,
        known,
        "the set of 26.2 blocks absent from vendor/minecraft-data changed.\n\
         missing from NEW_IN_26_2: {:?}\n\
         stale in NEW_IN_26_2 (vendor now carries them): {:?}\n\
         read each one's registration from the decompiled source and add it, per the module docs.",
        unmapped.difference(&known).collect::<Vec<_>>(),
        known.difference(&unmapped).collect::<Vec<_>>(),
    );
    assert_eq!(unmapped.len(), 30, "expected 30 unmapped 26.2 blocks");
}

// ---------------------------------------------------------------------------
// Drift guard
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates/verifies the committed table; run explicitly"]
fn committed_table_matches_source() {
    let by_name = props_by_block_name();
    let generated = generate(&by_name);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated, committed,
        "src/generated/light_props.rs is stale vs the committed sources; \
         regenerate with LODESTONE_REGEN=1"
    );
}
