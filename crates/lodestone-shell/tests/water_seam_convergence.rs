//! Issue #389, on the mesher live terrain actually uses.
//!
//! **The report:** distant water is visibly blocky along chunk boundaries — "you
//! can see where the chunks are" — and it corrects itself as the player
//! approaches. The mechanism is that a section meshed before its neighbour column
//! arrived decided every face on that seam against *air*, so both sides emit a
//! full-height translucent side quad and the seam draws at double alpha with no
//! depth conflict to give it away.
//!
//! # Why this gate exists alongside the unit test
//!
//! `mesher.rs`'s own `a_seam_meshed_without_its_neighbour_converges_on_the_
//! neighbour_present_answer` measures the same convergence through
//! `mesh_snapshot`, i.e. `mesh_simple` — and **`mesh_simple` has no fluid path at
//! all** (`docs/fluid-rendering.md`, "there are two meshers"). Under the demo
//! palette every non-air block occludes, water included, so that test proves the
//! *snapshot and culling seam* converges but says nothing about water as water.
//! `CLAUDE.md`'s *world* species is exactly this: a fix once measured
//! byte-identical against `--headless` because the only scene in the tree that
//! path could render structurally could not exercise it.
//!
//! So this gate builds real `BlockModels` from `client.jar`, fills two adjacent
//! columns with the real `minecraft:water` state id, and runs the real
//! `mesh_snapshot_fluids` — the function the live model path calls.
//!
//! # Controls, all executed
//!
//! * **The stale mesh.** The absent-neighbour measurement is taken through
//!   `ColumnSource::Complete`, which is the pre-#389 code verbatim: it had no
//!   other option. So the "before" number is measured, not asserted from theory.
//! * **The fixture.** Water on *both* sides of the seam is asserted against the
//!   world data before anything is meshed. A fixture without it cannot exercise
//!   this bug, and every assertion here would pass with the fix reverted.
//! * **A seamless fixture.** The same three measurements with an all-air (but
//!   *present*) neighbour: the count must not drop. That is the world species made
//!   to fire.
//!
//! `#[ignore]`d and fail-closed: a missing jar is an environment failure, never a
//! silent skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test water_seam_convergence -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use lodestone::mesher::{ColumnSource, SectionKey, mesh_snapshot_fluids, snapshot_section_in};
use lodestone_assets::{ResourceManager, ResourceSource, ZipSource};
use lodestone_model::BlockStateRegistry;
use lodestone_render::{BlockModels, SkyDefault, blocks_json_registry};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

/// Two sections: content in the lower one, so the upper is an elided all-air
/// section and `si == -1` is genuinely below the world. Neither is a "not loaded
/// yet" slot, which is the point — the *vertical* boundary must never defer.
const SECTIONS: usize = 2;

/// Walk up for a pack root holding both files the models need, mirroring
/// `crate::resources::asset_root` (private).
fn pack_root() -> PathBuf {
    let cwd = std::env::current_dir().expect("cwd");
    for base in cwd.ancestors() {
        let cache = base.join(".cache/mc");
        let Ok(entries) = std::fs::read_dir(&cache) else {
            continue;
        };
        let mut roots: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.join("client.jar").is_file() && p.join("generated/reports/blocks.json").is_file()
            })
            .collect();
        roots.sort();
        if let Some(best) = roots.pop() {
            return best;
        }
    }
    panic!(
        "no vanilla pack found under any ancestor's .cache/mc/<version>/ (needs client.jar + \
         generated/reports/blocks.json). This gate fails rather than skips: a skip reads as a pass."
    );
}

fn load_models(root: &std::path::Path) -> BlockModels {
    let bytes = std::fs::read(root.join("client.jar")).expect("read client.jar");
    let zip = ZipSource::from_bytes(bytes).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>]);
    let registry =
        blocks_json_registry(&root.join("generated/reports/blocks.json")).expect("blocks.json");
    BlockModels::build(&manager, &registry).expect("bake block models")
}

/// The `minecraft:water` source state (`level=0`) and `minecraft:air`, out of the
/// real report. A `level=0` source is what a lake is made of; a flowing level
/// would tilt the surface and put a second variable in the measurement.
fn water_and_air(root: &std::path::Path) -> (u32, u32) {
    let reg = blocks_json_registry(&root.join("generated/reports/blocks.json")).expect("blocks.json");
    let mut water = None;
    let mut air = None;
    for id in 0..reg.state_count() {
        let Some(state) = reg.resolve(id) else {
            continue;
        };
        let name = state.block.to_string();
        if name == "minecraft:air" && air.is_none() {
            air = Some(id);
        }
        if name == "minecraft:water"
            && water.is_none()
            && state.properties.get("level").map(String::as_str) == Some("0")
        {
            water = Some(id);
        }
    }
    (
        water.expect("minecraft:water[level=0] in blocks.json"),
        air.expect("minecraft:air in blocks.json"),
    )
}

/// One fixture column: `water_over(x, z)` decides whether the whole 16-cell
/// height of section 0 is water.
fn column(air: u32, water: u32, water_over: &dyn Fn(usize, usize) -> bool) -> LoadedChunk {
    let mut col = ChunkColumn::new(
        0,
        SECTIONS,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        air,
        0,
    );
    for x in 0..16usize {
        for z in 0..16usize {
            if !water_over(x, z) {
                continue;
            }
            for y in 0..16i32 {
                col.set_block(x, y, z, water);
            }
        }
    }
    LoadedChunk::new(col, ColumnLight::new(SECTIONS), Heightmaps::new(), Vec::new())
}

/// Which column plays the east neighbour at `(1, 0)`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum East {
    /// Not in the store — the chunk still in flight.
    Absent,
    /// Water for `z >= 8`, air for `z < 8`. The split makes the converged answer
    /// a number rather than zero, so "emit nothing" cannot pass.
    HalfWater,
    /// Present but empty — the seamless control.
    AllAir,
}

/// A 3×3 of all-water columns around `(0, 0)` with `(1, 0)` replaced per `east`,
/// so the east neighbour is the **only** variable. With fewer columns present,
/// every measurement would be `Deferred` and the variable would not be under
/// test.
fn seam_world(air: u32, water: u32, east: East) -> World {
    let mut world = World::new();
    for dx in -1..=1i32 {
        for dz in -1..=1i32 {
            let chunk = if (dx, dz) == (1, 0) {
                match east {
                    East::Absent => continue,
                    East::HalfWater => column(air, water, &|_x, z| z >= 8),
                    East::AllAir => column(air, water, &|_x, _z| false),
                }
            } else {
                column(air, water, &|_x, _z| true)
            };
            world.load(ChunkPos::new(dx, dz), chunk);
        }
    }
    world
}

fn subject() -> SectionKey {
    SectionKey {
        cx: 0,
        cz: 0,
        si: 0,
        min_y: 0,
    }
}

/// Water quads lying on the section's **east** boundary plane (`x ≈ 16`) — the
/// faces the column at `(1, 0)` decides — as a count plus the `(z, y)` bounding
/// box they occupy.
///
/// `mesh_fluids` insets side faces by `0.001` off the block boundary
/// (`FluidRenderer`'s z-fight offset, see `docs/fluid-rendering.md`), so the plane
/// test is a tolerance and not an equality. A count on its own cannot tell a
/// uniformly-wrong seam from a localised one — `CLAUDE.md`'s "measure by
/// location, never by frame average".
fn east_boundary(mesh: &lodestone_render::ModelMesh) -> (usize, String) {
    let mut count = 0usize;
    let (mut z0, mut z1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for quad in mesh.vertices.chunks_exact(4) {
        if !quad.iter().all(|v| (v.position[0] - 16.0).abs() < 0.01) {
            continue;
        }
        count += 1;
        for v in quad {
            z0 = z0.min(v.position[2]);
            z1 = z1.max(v.position[2]);
            y0 = y0.min(v.position[1]);
            y1 = y1.max(v.position[1]);
        }
    }
    let box_ = if count == 0 {
        "none".to_string()
    } else {
        format!("z {z0:.2}..{z1:.2}, y {y0:.2}..{y1:.2}")
    };
    (count, box_)
}

/// `(outcome label, east-boundary water quad count, bounding box)`.
fn measure(
    world: &World,
    models: &BlockModels,
    columns: ColumnSource,
) -> (&'static str, usize, String) {
    use lodestone::mesher::SnapshotOutcome;

    let outcome = snapshot_section_in(world, subject(), Some(SECTIONS), SkyDefault::Full, columns);
    let label = match &outcome {
        SnapshotOutcome::Ready(_) => "Ready",
        SnapshotOutcome::Empty => "Empty",
        SnapshotOutcome::Deferred(_) => "Deferred",
    };
    let snap = outcome
        .any()
        .expect("the subject section is solid water — it must snapshot");
    let fluids = mesh_snapshot_fluids(&snap, models);
    let (count, box_) = east_boundary(&fluids.water);
    (label, count, box_)
}

/// **Anti-vacuity: the fixture really has water on both sides of the seam.** The
/// *world* species of vacuous test lives in the input data and is invisible in
/// the test source, so the input data is asserted directly.
#[test]
#[ignore = "needs client.jar + blocks.json under .cache/mc/<version>/"]
fn the_fixture_has_real_water_on_both_sides_of_the_seam() {
    let root = pack_root();
    let (water, air) = water_and_air(&root);
    assert_ne!(water, air, "water and air must be different state ids");

    let world = seam_world(air, water, East::HalfWater);
    let subj = world
        .section(ChunkPos::new(0, 0), 0)
        .expect("subject section present");
    let east = world
        .section(ChunkPos::new(1, 0), 0)
        .expect("east neighbour present");

    let mut shared = 0usize;
    let mut against_air = 0usize;
    for y in 0..16usize {
        for z in 0..16usize {
            assert_eq!(
                subj.get_block(15, y, z),
                water,
                "the subject must be water across its whole east face"
            );
            if east.get_block(0, y, z) == water {
                shared += 1;
            } else {
                against_air += 1;
            }
        }
    }
    assert_eq!(shared, 128, "half the seam is water against water");
    assert_eq!(against_air, 128, "the other half is water against air");

    // And the models really classify that id as water — a state id the fixture
    // holds but `BlockModels` does not call a fluid would mesh to nothing at all.
    let models = load_models(&root);
    let cell = models
        .fluid(water)
        .expect("BlockModels must classify minecraft:water[level=0] as a fluid");
    assert_eq!(
        cell.kind,
        lodestone_render::FluidKind::Water,
        "and as water, not lava"
    );
}

/// **The convergence gate, on the real fluid path.** Both halves; the second is
/// the load-bearing one.
#[test]
#[ignore = "needs client.jar + blocks.json under .cache/mc/<version>/"]
fn a_water_seam_converges_on_the_neighbour_present_answer() {
    let root = pack_root();
    let models = load_models(&root);
    let (water, air) = water_and_air(&root);

    // The stale mesh, taken through `ColumnSource::Complete` — the pre-#389 code
    // verbatim, which had no other option.
    let (stale_label, stale, stale_box) = measure(
        &seam_world(air, water, East::Absent),
        &models,
        ColumnSource::Complete,
    );
    // The same section re-meshed after the column landed (what
    // `mark_neighbours_dirty` → `heal_dirty_columns` re-drives).
    let (healed_label, healed, healed_box) = measure(
        &seam_world(air, water, East::HalfWater),
        &models,
        ColumnSource::Streaming,
    );
    // And meshed once, with the neighbour there all along.
    let (fresh_label, fresh, fresh_box) = measure(
        &seam_world(air, water, East::HalfWater),
        &models,
        ColumnSource::Complete,
    );

    println!("stale   {stale_label:>8}  {stale:>4} quads  {stale_box}");
    println!("healed  {healed_label:>8}  {healed:>4} quads  {healed_box}");
    println!("fresh   {fresh_label:>8}  {fresh:>4} quads  {fresh_box}");

    assert_eq!(
        stale_label, "Ready",
        "the pre-fix policy must mesh the incomplete neighbourhood, or this is not the \
         stale case"
    );
    assert_eq!(healed_label, "Ready");
    assert_eq!(fresh_label, "Ready");

    assert!(
        stale > 0,
        "control: the stale mesh must actually emit the seam wall this issue is about — \
         0 quads would mean the fixture has no seam ({stale_box})"
    );
    assert!(
        healed < stale,
        "half 1: the water boundary quad count must DROP once the neighbour arrives — \
         stale {stale} ({stale_box}) vs healed {healed} ({healed_box})"
    );
    assert_eq!(
        healed, fresh,
        "half 2 (load-bearing): re-meshing after the neighbour arrives must land on \
         exactly the from-the-start answer, not merely change — healed {healed} \
         ({healed_box}) vs fresh {fresh} ({fresh_box})"
    );
    // Two per emitted side face: `bake_fluid` adds `FluidRenderer.addFace`'s
    // reversed-winding back face to every non-overlay side quad
    // (`docs/fluid-rendering.md`, "Closed — back faces"). Half the 16×16 seam
    // survives, so 128 faces → 256 quads.
    assert_eq!(
        (stale, healed),
        (512, 256),
        "the fixture's arithmetic: 16×16 seam faces, each double-sided, half of them \
         culled by the neighbour's water — stale {stale_box}, healed {healed_box}"
    );
    // The neighbour holds water at `z >= 8`, so *those* faces are the culled half
    // and the survivors are `z < 8`. The top edge is `15.86`, not `16.00`, because
    // a fluid side face's height is the surface's corner heights — `8/9` for a
    // source with air above, averaged down further at the seam by the air-side
    // neighbour, then pulled `0.001` off the boundary by `FluidRenderer`'s
    // z-fight inset. Asserting the box and not just the count is what makes "256
    // quads survived" mean "the right 256"; the first version of the sibling unit
    // test had the halves the wrong way round and only the printed box said so.
    assert_eq!(
        healed_box, "z 0.00..8.00, y 0.00..15.86",
        "the surviving faces must be exactly the half with no water across the seam — a \
         matching count with the wrong faces surviving is a different bug"
    );
}

/// **Control: the deferral is what keeps the stale mesh off the screen.** The
/// same absent neighbour that meshed `Ready` above must mesh `Deferred` under
/// `Streaming`, and `ready()` must refuse it. Without this the fix could be
/// reverted and the gate above would still pass — it measures convergence, not
/// prevention.
#[test]
#[ignore = "needs client.jar + blocks.json under .cache/mc/<version>/"]
fn control_an_absent_neighbour_defers_rather_than_meshing() {
    let root = pack_root();
    let (water, air) = water_and_air(&root);
    let models = load_models(&root);
    let (label, count, box_) = measure(
        &seam_world(air, water, East::Absent),
        &models,
        ColumnSource::Streaming,
    );
    println!("streaming/absent  {label}  {count} quads  {box_}");
    assert_eq!(
        label, "Deferred",
        "a live world with a missing neighbour column must defer the build"
    );
    let refused = snapshot_section_in(
        &seam_world(air, water, East::Absent),
        subject(),
        Some(SECTIONS),
        SkyDefault::Full,
        ColumnSource::Streaming,
    )
    .ready();
    assert!(
        refused.is_none(),
        "`ready()` must refuse a deferred snapshot — that refusal is the fix"
    );
}

/// **Control: a fixture with no water across the seam cannot see this bug.** The
/// east neighbour is present but empty, so its arrival culls nothing and the
/// count does not drop. Had the real fixture been built this way, every assertion
/// above would pass with the fix reverted — the *world* species, made to fire.
#[test]
#[ignore = "needs client.jar + blocks.json under .cache/mc/<version>/"]
fn control_a_seamless_fixture_shows_no_convergence() {
    let root = pack_root();
    let (water, air) = water_and_air(&root);
    let models = load_models(&root);

    let (_, stale, stale_box) = measure(
        &seam_world(air, water, East::Absent),
        &models,
        ColumnSource::Complete,
    );
    let (_, healed, healed_box) = measure(
        &seam_world(air, water, East::AllAir),
        &models,
        ColumnSource::Streaming,
    );
    println!("control stale  {stale} {stale_box}");
    println!("control healed {healed} {healed_box}");
    assert_eq!(
        stale, healed,
        "control: an all-air neighbour culls nothing, so arrival changes nothing — a gate \
         built on this fixture would be blind to #389 ({stale_box} vs {healed_box})"
    );
}

/// The state-id lookup, kept honest: a report whose `minecraft:water` block has
/// no `level` property, or none at all, must fail loudly rather than silently
/// pick some other state.
#[test]
#[ignore = "needs blocks.json under .cache/mc/<version>/"]
fn water_state_lookup_is_unambiguous() {
    let root = pack_root();
    let reg =
        blocks_json_registry(&root.join("generated/reports/blocks.json")).expect("blocks.json");
    let mut levels: BTreeMap<String, usize> = BTreeMap::new();
    for id in 0..reg.state_count() {
        let Some(state) = reg.resolve(id) else {
            continue;
        };
        if state.block.to_string() == "minecraft:water" {
            let level = state
                .properties
                .get("level")
                .cloned()
                .unwrap_or_else(|| "<none>".to_string());
            *levels.entry(level).or_default() += 1;
        }
    }
    assert_eq!(
        levels.get("0").copied(),
        Some(1),
        "exactly one minecraft:water[level=0] source state expected, saw {levels:?}"
    );
}
