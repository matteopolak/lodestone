//! Waterlogged blocks z-fighting against their own water, on the mesher live
//! terrain actually uses.
//!
//! **The report:** "waterlogged blocks have z-fighting between the water and the
//! regular block texture", and, more diagnostically, "water and the block are
//! swapping rapidly on the sides that should not show the water at all (eg the
//! back side of a stair)". *Should not show the water at all* is the whole bug:
//! the fix is not an inset, it is that the face is never emitted.
//!
//! **The mechanism.** `FluidRenderer.shouldRenderFace` is
//! `!isNeighborSameFluid(self, neighbourFluid) && !isFaceOccludedBySelf(ownState,
//! dir)`. `mesh_fluids` had only the first conjunct plus the *neighbour*-facing
//! `isFaceOccludedByNeighbor`; nothing anywhere asked about the block sharing the
//! fluid's own cell. A waterlogged stair shares its cell with the water, so the
//! water's face on the stair's solid side landed coplanar with the stair's own
//! face — 1 mm apart after `FluidRenderer`'s `0.001` inset, which is exactly the
//! distance that reads as z-fighting at range.
//!
//! # Why the fixture is a stair and not a full block
//!
//! The discriminating input has to be a waterloggable block whose own geometry
//! reaches the cell boundary on **some** faces and not others:
//!
//! * a waterlogged **full** block would have its water culled by the neighbour
//!   rule anyway, so both hypotheses coincide;
//! * a waterlogged **fence post** touches no side boundary, so both hypotheses
//!   coincide again — and that is this file's *world*-species control;
//! * a **stair** has one full side, one half side and two L-shaped sides, so a
//!   single fixture separates the two hypotheses on four faces at once.
//!
//! The wrong hypothesis is not asserted from theory: a plain `minecraft:water`
//! source in the same cell of the same world is measured as its own arm, and its
//! quad count *is* the no-self-occlusion prediction.
//!
//! # Controls, all executed
//!
//! | control | what it would hide |
//! |---|---|
//! | plain water source | measures the pre-fix count rather than asserting it |
//! | waterlogged fence post | a fixture with no boundary-reaching geometry is blind to this bug |
//! | waterlogged leaves | `noOcclusion()`: a full-cube outline that must **not** self-cull, so the `RenderLayer` gate is load-bearing |
//! | `waterlogged=false` stair | proves the water comes from the waterlogging and not from the fixture |
//! | `facing=south` stair | proves the culled face follows the block's orientation rather than a hardcoded axis |
//! | `half=top` stair | proves `up` is **not** self-tested — vanilla's `renderUp` skips `shouldRenderFace` |
//!
//! Mismatches are collected and reported together rather than asserted inside the
//! loop: an `assert!` per fixture aborts on the first failure, so a neutered arm
//! would prove one case and leave the rest as arguments.
//!
//! `#[ignore]`d and fail-closed: a missing jar is an environment failure, never a
//! silent skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test fluid_self_occlusion -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use lodestone::mesher::{ColumnSource, SectionKey, mesh_snapshot_fluids, snapshot_section_in};
use lodestone_assets::{Direction, ResourceManager, ResourceSource, ZipSource};
use lodestone_model::{BlockAabb, BlockStateRegistry};
use lodestone_render::{
    BlockModels, BlocksJsonRegistry, ModelMesh, RenderLayer, SkyDefault, blocks_json_registry,
};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

/// Two sections, matching `water_seam_convergence`: the upper one is an elided
/// all-air section, so `si == -1` is genuinely below the world.
const SECTIONS: usize = 2;

/// The cell the single subject block occupies, well away from every section
/// boundary so no seam or padding question enters the measurement.
const CELL: (usize, i32, usize) = (8, 8, 8);

// ---------------------------------------------------------------------------
// Fixture plumbing (the `water_seam_convergence` pattern)
// ---------------------------------------------------------------------------

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

fn registry(root: &std::path::Path) -> BlocksJsonRegistry {
    blocks_json_registry(&root.join("generated/reports/blocks.json")).expect("blocks.json")
}

/// The one state id whose block is `block` and whose properties include every
/// pair in `props`. Panics rather than guessing — an ambiguous lookup would make
/// every assertion below about some other state.
fn find_state(reg: &BlocksJsonRegistry, block: &str, props: &[(&str, &str)]) -> u32 {
    let mut hits = Vec::new();
    for id in 0..reg.state_count() {
        let Some(state) = reg.resolve(id) else {
            continue;
        };
        if state.block.to_string() != block {
            continue;
        }
        if props
            .iter()
            .all(|(k, v)| state.properties.get(*k).map(String::as_str) == Some(*v))
        {
            hits.push(id);
        }
    }
    assert_eq!(
        hits.len(),
        1,
        "{block} {props:?} must resolve to exactly one state, matched {}",
        hits.len()
    );
    hits[0]
}

/// A 3×3 of all-air columns with a single `subject` block at [`CELL`] of the
/// centre column. Every neighbour column is present, so nothing defers and the
/// subject's neighbourhood is unambiguously air on all six sides.
fn lone_block_world(air: u32, subject: u32) -> World {
    let mut world = World::new();
    for dx in -1..=1i32 {
        for dz in -1..=1i32 {
            let mut col = ChunkColumn::new(
                0,
                SECTIONS,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                air,
                0,
            );
            if (dx, dz) == (0, 0) {
                col.set_block(CELL.0, CELL.1, CELL.2, subject);
            }
            world.load(
                ChunkPos::new(dx, dz),
                LoadedChunk::new(col, ColumnLight::new(SECTIONS), Heightmaps::new(), Vec::new()),
            );
        }
    }
    world
}

fn subject_key() -> SectionKey {
    SectionKey {
        cx: 0,
        cz: 0,
        si: 0,
        min_y: 0,
    }
}

// ---------------------------------------------------------------------------
// Measurement: which faces exist, by location
// ---------------------------------------------------------------------------

/// A fluid quad count per cell face, keyed by face name.
///
/// Classification is by the quad's own extent rather than by a hardcoded plane
/// constant, so it survives `bake_fluid`'s `0.001` insets and the `8/9`-averaged
/// corner heights without a tolerance per fixture: a side face is the one with a
/// degenerate extent on `x` or `z`, and up/down are told apart by height. A count
/// alone cannot distinguish a uniformly-wrong cell from a localised one —
/// `CLAUDE.md`'s "measure by location, never by frame average" — which is the
/// whole reason this is a per-face map and not a total.
fn face_counts(mesh: &ModelMesh) -> BTreeMap<&'static str, usize> {
    const TOL: f32 = 0.01;
    let (ox, oy, oz) = (CELL.0 as f32, CELL.1 as f32, CELL.2 as f32);
    let mut out: BTreeMap<&'static str, usize> = BTreeMap::new();
    for face in ["up", "down", "north", "south", "east", "west", "other"] {
        out.insert(face, 0);
    }
    for quad in mesh.vertices.chunks_exact(4) {
        let axis_span = |i: usize| {
            let mut lo = f32::MAX;
            let mut hi = f32::MIN;
            for v in quad {
                lo = lo.min(v.position[i]);
                hi = hi.max(v.position[i]);
            }
            (lo, hi)
        };
        let (x0, x1) = axis_span(0);
        let (y0, y1) = axis_span(1);
        let (z0, z1) = axis_span(2);
        let face = if x1 - x0 < TOL {
            if x0 - ox < 0.5 { "west" } else { "east" }
        } else if z1 - z0 < TOL {
            if z0 - oz < 0.5 { "north" } else { "south" }
        } else if y1 - y0 < TOL {
            if y0 - oy < 0.5 { "down" } else { "up" }
        } else {
            "other"
        };
        *out.get_mut(face).expect("face key present") += 1;
    }
    out
}

/// Mesh a world holding a single `subject` block and count its water quads per
/// face. Returns `(per-face counts, total)`.
fn measure(
    models: &BlockModels,
    air: u32,
    subject: u32,
) -> (BTreeMap<&'static str, usize>, usize) {
    let world = lone_block_world(air, subject);
    let snap = snapshot_section_in(
        &world,
        subject_key(),
        Some(SECTIONS),
        SkyDefault::Full,
        ColumnSource::Complete,
    )
    .any()
    .expect("all nine columns are present, so the subject section must snapshot");
    let fluids = mesh_snapshot_fluids(&snap, models);
    let counts = face_counts(&fluids.water);
    let total = fluids.water.vertices.len() / 4;
    (counts, total)
}

fn render(counts: &BTreeMap<&'static str, usize>) -> String {
    counts
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(f, n)| format!("{f}={n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `[up, down, north, south, east, west]`, the order every expectation below is
/// written in.
///
/// **The counts are not symmetric, and predicting the round number is how this
/// gate failed on its first run.** A side face and the top face each carry
/// `FluidRenderer.addFace`'s reversed copy, so they count **2**; the bottom face
/// is the one `tesselate` passes `addBackFace = false` for, so it counts **1**.
/// A fully open water cell is therefore **11** quads, not 12.
fn expect_faces(up: usize, down: usize, north: usize, south: usize, east: usize, west: usize) -> BTreeMap<&'static str, usize> {
    let mut m: BTreeMap<&'static str, usize> = BTreeMap::new();
    m.insert("up", up);
    m.insert("down", down);
    m.insert("north", north);
    m.insert("south", south);
    m.insert("east", east);
    m.insert("west", west);
    m.insert("other", 0);
    m
}

// ---------------------------------------------------------------------------
// The pure predicate, no jar needed
// ---------------------------------------------------------------------------

fn aabb(min: [f32; 3], max: [f32; 3]) -> BlockAabb {
    BlockAabb { min, max }
}

/// `face_fully_covered` on shapes written out by hand from vanilla's own record,
/// including the case a single-box reduction cannot answer.
///
/// Vanilla's straight-stair shape union is `Block.column(16, 0, 8)` (the bottom
/// slab) or'd with a half-cell step; neither box covers a side face on its own,
/// and their union covers exactly one. A predicate that required one box — which
/// is what `full_footprint_y_range` does for the *neighbour* test — answers "not
/// occluded" for all four sides and misses this bug entirely.
#[test]
fn face_coverage_is_exact_for_a_two_box_union() {
    let slab = aabb([0.0, 0.0, 0.0], [1.0, 0.5, 1.0]);
    let step_north = aabb([0.0, 0.5, 0.0], [1.0, 1.0, 0.5]);
    let stair = [slab, step_north];

    // The two boxes stack to a full square on `-Z` and nowhere else horizontal.
    assert!(
        face_fully_covered_or_panic(&stair, Direction::North),
        "the slab covers y 0..0.5 and the step covers y 0.5..1 at z = 0, so the north face \
         is fully covered by the union"
    );
    assert!(!face_fully_covered_or_panic(&stair, Direction::South));
    assert!(!face_fully_covered_or_panic(&stair, Direction::East));
    assert!(!face_fully_covered_or_panic(&stair, Direction::West));
    assert!(face_fully_covered_or_panic(&stair, Direction::Down));
    // Vanilla never asks about `up` for a fluid face, but the geometry answer is
    // still the honest one: the step reaches z = 0.5 only.
    assert!(!face_fully_covered_or_panic(&stair, Direction::Up));

    // Control: the single box alone answers "not covered" on the north face, so
    // the assertion above is really about the union and not about the slab.
    assert!(!face_fully_covered_or_panic(&[slab], Direction::North));
    assert!(face_fully_covered_or_panic(&[slab], Direction::Down));

    // A shape that misses a strip is not covered, however many boxes it has: two
    // quarter-columns leave the middle open.
    let gappy = [
        aabb([0.0, 0.0, 0.0], [0.25, 1.0, 1.0]),
        aabb([0.75, 0.0, 0.0], [1.0, 1.0, 1.0]),
    ];
    assert!(!face_fully_covered_or_panic(&gappy, Direction::North));

    // A box floating off the plane contributes nothing, even though it would
    // cover the square if projected.
    let floating = [aabb([0.0, 0.0, 0.25], [1.0, 1.0, 0.75])];
    assert!(!face_fully_covered_or_panic(&floating, Direction::North));
    assert!(!face_fully_covered_or_panic(&floating, Direction::South));

    // A genuinely full cube covers all six.
    let cube = [aabb([0.0, 0.0, 0.0], [1.0, 1.0, 1.0])];
    for d in [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::East,
        Direction::West,
    ] {
        assert!(face_fully_covered_or_panic(&cube, d), "full cube, {d:?}");
    }
    // And an empty shape covers nothing — the answer plain water relies on.
    assert!(!face_fully_covered_or_panic(&[], Direction::North));
    assert!(lodestone_assets::fluid::self_occlusion(&[]).is_empty());
}

fn face_fully_covered_or_panic(boxes: &[BlockAabb], face: Direction) -> bool {
    lodestone_assets::fluid::face_fully_covered(boxes, face)
}

/// **`up` is deliberately absent from `SelfOcclusion`.** `FluidRenderer.tesselate`
/// computes `renderUp` as bare `!isNeighborSameFluid(self, above)` — the only one
/// of the six faces that never reaches `shouldRenderFace`. A full cube therefore
/// self-occludes five faces, not six, and this pins that asymmetry against a
/// future "tidy-up" adding the sixth.
#[test]
fn self_occlusion_has_no_up_face_because_vanilla_does_not_test_one() {
    let cube = [aabb([0.0, 0.0, 0.0], [1.0, 1.0, 1.0])];
    let occ = lodestone_assets::fluid::self_occlusion(&cube);
    assert_eq!(
        (occ.down, occ.north, occ.south, occ.east, occ.west),
        (true, true, true, true, true),
        "a full cube covers every face `shouldRenderFace` asks about"
    );
    // `face_fully_covered` does answer for `Up`, and it is `true` here — so the
    // absence above is a modelling decision, not the predicate failing.
    assert!(face_fully_covered_or_panic(&cube, Direction::Up));
}

// ---------------------------------------------------------------------------
// The fixture, asserted against the world data before anything is meshed
// ---------------------------------------------------------------------------

/// **Anti-vacuity, the *world* species.** A waterlogged stair has to be *both* a
/// solid model and a water cell in one cell — that co-occupancy is the entire
/// premise, and it is invisible in the source of any test that only counts quads.
#[test]
#[ignore = "needs client.jar + blocks.json under .cache/mc/<version>/"]
fn the_fixture_is_a_stair_and_a_water_source_in_one_cell() {
    let root = pack_root();
    let reg = registry(&root);
    let models = load_models(&root);
    let stair = find_state(
        &reg,
        "minecraft:oak_stairs",
        &[
            ("facing", "north"),
            ("half", "bottom"),
            ("shape", "straight"),
            ("waterlogged", "true"),
        ],
    );

    let cell = models
        .fluid(stair)
        .expect("a waterlogged stair must classify as a fluid cell");
    assert_eq!(cell.kind, lodestone_render::FluidKind::Water);
    assert!(
        !models.quads(stair).is_empty(),
        "and it must also carry solid model geometry — a cell that is only water \
         cannot z-fight against a block in the same cell"
    );
    assert_eq!(
        models.layer(stair),
        RenderLayer::Solid,
        "the `canOcclude` stand-in: oak planks are fully opaque"
    );

    let boxes = lodestone_data::outline_shapes::outline_boxes(stair)
        .expect("oak_stairs is in the outline census");
    println!("oak_stairs[facing=north,half=bottom] outline boxes ({}):", boxes.len());
    for b in boxes {
        println!("  min {:?} max {:?}", b.min, b.max);
    }
    let occ = lodestone_assets::fluid::self_occlusion(boxes);
    println!("  self_occlusion {occ:?}");
    assert_eq!(
        (occ.down, occ.north, occ.south, occ.east, occ.west),
        (true, true, false, false, false),
        "vanilla's `StairBlock.SHAPE_BOTTOM_STRAIGHT` for facing=north is the bottom slab \
         plus a step over z 0..0.5: the north side and the underside are fully covered, the \
         south side is half covered and east/west are L-shaped. If this fails, read the \
         printed boxes — the orientation of `Shapes.rotateHorizontal`'s base shape is the \
         thing being pinned"
    );

    // The `waterlogged=false` sibling shares the geometry and carries no fluid,
    // which is what makes the dry control below meaningful.
    let dry = find_state(
        &reg,
        "minecraft:oak_stairs",
        &[
            ("facing", "north"),
            ("half", "bottom"),
            ("shape", "straight"),
            ("waterlogged", "false"),
        ],
    );
    assert!(models.fluid(dry).is_none());
    assert_eq!(
        lodestone_data::outline_shapes::outline_boxes(dry),
        Some(boxes),
        "the two states must differ only in the fluid, or the dry control is measuring \
         a different shape"
    );
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// **The gate: which fluid faces exist, per face, on the live mesher.**
///
/// The `plain_water` row is the no-self-occlusion arm, *measured*: a water source
/// alone in air has the same corner heights as the waterlogged stair (both are
/// `amount = 8` sources with air above), so its 12 quads are exactly what the
/// stair emitted before this fix, and exactly what it would emit again if the self
/// test were removed.
#[test]
#[ignore = "needs client.jar + blocks.json under .cache/mc/<version>/"]
fn a_waterlogged_block_emits_no_fluid_face_its_own_geometry_already_covers() {
    let root = pack_root();
    let reg = registry(&root);
    let models = load_models(&root);
    let air = find_state(&reg, "minecraft:air", &[]);

    let stair = |facing: &'static str, half: &'static str, waterlogged: &'static str| {
        find_state(
            &reg,
            "minecraft:oak_stairs",
            &[
                ("facing", facing),
                ("half", half),
                ("shape", "straight"),
                ("waterlogged", waterlogged),
            ],
        )
    };

    // `(label, state, expected per-face, why)`.
    let cases: Vec<(&str, u32, BTreeMap<&'static str, usize>, &str)> = vec![
        (
            "plain_water",
            find_state(&reg, "minecraft:water", &[("level", "0")]),
            expect_faces(2, 1, 2, 2, 2, 2),
            "the no-self-occlusion arm: all six faces present. Eleven quads, not twelve: \
             every face gets `addFace`'s reversed copy **except** the bottom, which \
             `tesselate` passes `addBackFace = false` for",
        ),
        (
            "stair_bottom_north",
            stair("north", "bottom", "true"),
            expect_faces(2, 0, 0, 2, 2, 2),
            "the reported bug: the stair's own solid north side and its underside \
             lose their coplanar water faces, the half and L-shaped sides keep theirs",
        ),
        (
            "stair_bottom_south",
            stair("south", "bottom", "true"),
            expect_faces(2, 0, 2, 0, 2, 2),
            "orientation: the culled side follows `facing`, so north and south swap",
        ),
        (
            "stair_top_north",
            stair("north", "top", "true"),
            expect_faces(2, 1, 0, 2, 2, 2),
            "`half=top` inverts the shape: the underside is no longer covered so the \
             down face returns, the north side is still covered, and `up` survives \
             because vanilla's `renderUp` never consults `shouldRenderFace`",
        ),
        (
            "slab_bottom",
            find_state(
                &reg,
                "minecraft:stone_slab",
                &[("type", "bottom"), ("waterlogged", "true")],
            ),
            expect_faces(2, 0, 2, 2, 2, 2),
            "the golden corpus's only waterlogged block: a bottom slab covers its \
             underside and nothing else",
        ),
        (
            "control_fence_post",
            find_state(
                &reg,
                "minecraft:oak_fence",
                &[
                    ("north", "false"),
                    ("south", "false"),
                    ("east", "false"),
                    ("west", "false"),
                    ("waterlogged", "true"),
                ],
            ),
            expect_faces(2, 1, 2, 2, 2, 2),
            "*world*-species control: a post touches no boundary, so both hypotheses \
             agree and a gate built on this fixture would be blind",
        ),
        (
            "control_leaves",
            find_state(
                &reg,
                "minecraft:oak_leaves",
                &[("distance", "7"), ("persistent", "true"), ("waterlogged", "true")],
            ),
            expect_faces(2, 1, 2, 2, 2, 2),
            "`canOcclude` control: a full-cube outline that vanilla marks \
             `noOcclusion()`, so the `RenderLayer` gate must stop all five faces \
             being culled",
        ),
        (
            "control_dry_stair",
            stair("north", "bottom", "false"),
            expect_faces(0, 0, 0, 0, 0, 0),
            "fixture control: the water comes from the waterlogging, not the world",
        ),
    ];

    let mut mismatches: Vec<String> = Vec::new();
    for (label, state, expected, why) in &cases {
        let (counts, total) = measure(&models, air, *state);
        let expected_total: usize = expected.values().sum();
        println!(
            "{label:<22} {total:>3} quads  {:<52} layer={:?}",
            render(&counts),
            models.layer(*state)
        );
        if counts != *expected {
            mismatches.push(format!(
                "{label}: expected [{}] got [{}] ({total} quads vs {expected_total}) — {why}",
                render(expected),
                render(&counts)
            ));
        }
    }

    // The leaves control is only worth anything if the geometry really would have
    // culled it: without this, "leaves keep their faces" could just mean the
    // outline census reports leaves as shapeless.
    let leaves = find_state(
        &reg,
        "minecraft:oak_leaves",
        &[("distance", "7"), ("persistent", "true"), ("waterlogged", "true")],
    );
    let leaf_boxes = lodestone_data::outline_shapes::outline_boxes(leaves)
        .expect("oak_leaves is in the outline census");
    let leaf_geometry_would_cull = lodestone_assets::fluid::self_occlusion(leaf_boxes);
    println!("leaves outline self_occlusion (before the layer gate): {leaf_geometry_would_cull:?}");
    if leaf_geometry_would_cull.is_empty() {
        mismatches.push(
            "control_leaves is vacuous: the leaves outline shape does not cover any face, so \
             the `RenderLayer` gate is not what keeps its water"
                .to_string(),
        );
    }
    if models.layer(leaves) == RenderLayer::Solid {
        mismatches.push(format!(
            "control_leaves is vacuous the other way: leaves classify as {:?}, so the gate \
             that is supposed to spare them does not fire",
            models.layer(leaves)
        ));
    }

    assert!(
        mismatches.is_empty(),
        "{} of {} fluid face-set expectations failed:\n  {}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n  ")
    );
}
