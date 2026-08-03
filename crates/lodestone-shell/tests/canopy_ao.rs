//! The **AO occluder predicate**, measured through the production mesher on real
//! baked leaf geometry — Tier 1 item 1's last player-visible divergence.
//!
//! **The report:** the underside of a tree canopy does not darken. In vanilla it
//! is markedly dimmer than open sky.
//!
//! **The cause:** `quad_corner_sample` asked
//! `ModelSectionView::occludes_at` — a *face-culling* predicate (do opaque quads
//! cover all six boundary faces) — where vanilla asks
//! `BlockBehaviour.getShadeBrightness`, i.e.
//! `state.isCollisionShapeFullBlock(..) ? 0.2F : 1.0F` with seven class
//! overrides (`BlockBehaviour.java:315-317`, consumed at
//! `BlockModelLighter.java:45-110`). Leaves are a **full collision cube whose
//! cutout sprite does not occlude for culling**, so every AO corner beside a leaf
//! contributed `1.0` instead of `0.2`.
//!
//! # Why this gate exists alongside `lodestone-render`'s
//!
//! `crates/lodestone-render/tests/model_ao_corner_gate.rs` proves the *pixel*
//! consequence, but it supplies its own `ModelSectionView` — so it proves
//! `quad_corner_sample` reads `ao_occludes_at`, not that anything in the running
//! client ever answers it differently from `occludes_at`. `ao_occludes_at`'s trait
//! default **is** `occludes_at`, which is precisely the island shape `CLAUDE.md`
//! rule 1 names: the whole mechanism is inert unless `SnapshotModelView` overrides
//! it. So this gate drives the real `mesh_snapshot_models` over a real
//! `SectionSnapshot` with real `BlockModels` baked from `client.jar` — the exact
//! call the live chunk mesher makes.
//!
//! It also has to be `mesh_models`, not `mesh_simple`: `--headless`'s demo scene
//! renders through the packed path, which has its own separate AO implementation
//! and cannot exercise this code at all (`docs/model-smooth-lighting.md`, "which
//! mesher this is"). That is `CLAUDE.md`'s *world* species of vacuous test.
//!
//! # What is measured, and both hypotheses
//!
//! A section filled solid with `minecraft:oak_leaves` (a canopy interior, and the
//! only fixture where a leaf's own faces are surrounded by leaves — leaves never
//! cull against leaves, so every interior face is genuinely emitted). The
//! subject is one interior block's **`Down`** quad, located by centroid:
//!
//! | | AO factor | `face_shade(Down)` | vertex `ao` |
//! |---|---|---|---|
//! | correct (`getShadeBrightness`) | `(0.2+0.2+0.2+1.0)/4 = 0.4` | `0.5` | **`0.20`** |
//! | the bug (`occludes_at`) | `(1+1+1+1)/4 = 1.0` | `0.5` | `0.50` |
//!
//! Both numbers are derived from constants that originate outside this codebase
//! (vanilla's `0.2F` shade sample and `CardinalLighting.DEFAULT`'s `down 0.5`), and
//! they are far enough apart that no predicate can satisfy both — the *magnitude*
//! species of vacuous test cannot hide here.
//!
//! # Controls, all executed
//!
//! * **Glass.** `minecraft:glass` is a full collision cube *and* vanilla-exempt
//!   (`TransparentBlock.getShadeBrightness` returns `1.0`), so its interior `Down`
//!   quad must stay at `0.50`. This is the control that fires if the predicate is
//!   ever "simplified" to a `collision_shapes`-derived `isCollisionShapeFullBlock`,
//!   which would darken glass too.
//! * **The fixture's premise.** `BlockModels::occludes` is asserted `false` for
//!   both subjects before anything is meshed. If it were `true` the two predicates
//!   would agree and every assertion below would pass with the fix reverted — and
//!   the interior faces would have been culled anyway.
//! * **The census.** `lodestone_data::shade_brightness` is asserted to disagree
//!   about the two subjects in the first place. An all-zero table satisfies "glass
//!   stays bright" perfectly.
//!
//! `#[ignore]`d and fail-closed: a missing jar is an environment failure, never a
//! silent skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test canopy_ao -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use lodestone::mesher::{ColumnSource, SectionKey, mesh_snapshot_models, snapshot_section_in};
use lodestone_assets::{ResourceManager, ResourceSource, ZipSource};
use lodestone_model::BlockStateRegistry;
use lodestone_render::{BlockModels, BlocksJsonRegistry, ModelMesh, SkyDefault, blocks_json_registry};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

/// One 16-block section, so the fixture is a solid cube of one block type with
/// nothing else in it to explain a reading.
const SECTIONS: usize = 1;

/// The interior block whose `Down` quad is the subject. Deep enough that all
/// three of its AO-corner cells are inside the same filled section, so the
/// measurement never depends on how an absent neighbour snapshot answers.
const SUBJECT: [usize; 3] = [8, 8, 8];

/// `face_shade(Down)` — `CardinalLighting.DEFAULT`'s `down` component. Mirrored
/// here as a documented prediction, not imported: the point is that the expected
/// bytes come from vanilla's own table.
const DOWN_SHADE: f32 = 0.5;

/// Vanilla's occluded AO sample; three occluding ring cells plus the always-open
/// front cell average to `0.4`.
const AO_THREE_OCCLUDERS: f32 = (0.2 + 0.2 + 0.2 + 1.0) / 4.0;

/// Walk up for a pack root holding both files the models need, mirroring
/// `crate::resources::asset_root` (private) and `water_seam_convergence`.
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

// `BlockStateRegistry` is a *trait*; the concrete registry is `BlocksJsonRegistry`.
fn registry(root: &std::path::Path) -> BlocksJsonRegistry {
    blocks_json_registry(&root.join("generated/reports/blocks.json")).expect("blocks.json")
}

/// First state id of `name` that is not waterlogged, out of the real report — so
/// no state id is hand-typed and the fixture survives a data bump's renumbering.
fn state_id(reg: &impl BlockStateRegistry, name: &str) -> u32 {
    for id in 0..reg.state_count() {
        let Some(state) = reg.resolve(id) else {
            continue;
        };
        if state.block.to_string() != name {
            continue;
        }
        if state.properties.get("waterlogged").map(String::as_str) == Some("true") {
            continue;
        }
        return id;
    }
    panic!("{name} present in blocks.json");
}

fn air_id(reg: &impl BlockStateRegistry) -> u32 {
    state_id(reg, "minecraft:air")
}

/// A column whose whole section 0 is `fill`.
fn column(air: u32, fill: u32) -> LoadedChunk {
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
            for y in 0..16i32 {
                col.set_block(x, y, z, fill);
            }
        }
    }
    LoadedChunk::new(col, ColumnLight::new(SECTIONS), Heightmaps::new(), Vec::new())
}

/// A 3×3 of identical filled columns, so the centre section's horizontal AO ring
/// is never deciding against a missing neighbour.
fn filled_world(air: u32, fill: u32) -> World {
    let mut world = World::new();
    for dx in -1..=1i32 {
        for dz in -1..=1i32 {
            world.load(ChunkPos::new(dx, dz), column(air, fill));
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

/// What one meshed fixture reports.
///
/// **Deliberately not "the `ao` of one located quad".** A block's `Down` quad and
/// the block-below's `Up` quad are *geometrically identical* — same four corner
/// positions, same centroid — so a centroid lookup cannot tell them apart, and
/// distinguishing them by winding would mean asserting a polarity, which
/// `CLAUDE.md` forbids. The population statistics below are exact instead: `ao`
/// is `face_shade x (1 - 0.2k)` for `k` occluding ring cells, so the minimum over
/// the whole mesh is `min(face_shade) x min(1 - 0.2k)` — `0.5 x 0.4 = 0.20` when
/// leaves occlude and `0.5 x 1.0 = 0.50` when they do not.
struct Measured {
    /// Darkest vertex `ao` anywhere in the mesh.
    min_ao: f32,
    /// Every distinct `ao`, rounded to 3 dp and sorted — `{0.5, 0.6, 0.8, 1.0}`
    /// exactly (the four `face_shade` constants) iff AO contributed nothing.
    distinct: Vec<f32>,
    /// Where the darkest vertices are: `(x0, y0, z0, x1, y1, z1)`, section-local.
    /// A fraction cannot tell a uniformly-wrong mesh from a localised one.
    min_bbox: [f32; 6],
    /// How many vertices sit at [`Self::min_ao`].
    min_count: usize,
    quads: usize,
}

fn round3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

fn measure(models: &BlockModels, fill: u32, air: u32) -> Measured {
    let world = filled_world(air, fill);
    // Fixture premise, read off the world rather than the snapshot
    // (`SectionSnapshot::at` is private, deliberately): the subject cell and the
    // three cells its `Down` AO ring samples all hold the subject block. Without
    // this the fixture cannot produce a three-occluder corner at all, and every
    // assertion below would pass with the fix reverted.
    let centre = world
        .section(ChunkPos::new(0, 0), 0)
        .expect("the centre column's section 0 is loaded");
    for cell in [
        [SUBJECT[0], SUBJECT[1], SUBJECT[2]],
        [SUBJECT[0] + 1, SUBJECT[1] - 1, SUBJECT[2]],
        [SUBJECT[0], SUBJECT[1] - 1, SUBJECT[2] + 1],
        [SUBJECT[0] + 1, SUBJECT[1] - 1, SUBJECT[2] + 1],
    ] {
        assert_eq!(
            centre.get_block(cell[0], cell[1], cell[2]),
            fill,
            "fixture premise: {cell:?} holds the subject block"
        );
    }

    let outcome = snapshot_section_in(
        &world,
        subject_key(),
        Some(SECTIONS),
        SkyDefault::Full,
        ColumnSource::Complete,
    );
    let snap = outcome
        .any()
        .expect("a solid 3x3 of filled columns snapshots as Ready");
    assert_eq!(
        snap.unloaded_neighbours(),
        0,
        "fixture premise: every neighbour slot is a reading, not a guess — an unloaded slot \
         reads as air and would silently remove occluders from the AO rings"
    );

    let mesh: ModelMesh = mesh_snapshot_models(&snap, models);
    assert!(
        mesh.quad_count() > 0,
        "a solid section of non-occluding blocks must emit interior faces — if this is 0 the \
         geometry was culled and nothing below is under test"
    );

    let min_ao = mesh
        .vertices
        .iter()
        .map(|v| round3(v.ao))
        .fold(f32::INFINITY, f32::min);
    let mut distinct: Vec<f32> = mesh.vertices.iter().map(|v| round3(v.ao)).collect();
    distinct.sort_by(f32::total_cmp);
    distinct.dedup();

    let mut min_bbox = [f32::MAX, f32::MAX, f32::MAX, f32::MIN, f32::MIN, f32::MIN];
    let mut min_count = 0usize;
    for v in &mesh.vertices {
        if round3(v.ao) > min_ao {
            continue;
        }
        min_count += 1;
        for axis in 0..3 {
            min_bbox[axis] = min_bbox[axis].min(v.position[axis]);
            min_bbox[axis + 3] = min_bbox[axis + 3].max(v.position[axis]);
        }
    }

    Measured {
        min_ao,
        distinct,
        min_bbox,
        min_count,
        quads: mesh.quad_count(),
    }
}

#[test]
#[ignore = "needs client.jar + blocks.json; run explicitly"]
fn a_leaf_canopy_interior_darkens_and_glass_does_not() {
    let root = pack_root();
    let models = load_models(&root);
    let reg = registry(&root);
    let air = air_id(&reg);
    let leaves = state_id(&reg, "minecraft:oak_leaves");
    let glass = state_id(&reg, "minecraft:glass");

    // --- Premises, before any meshing. Each of these being false would make
    // every assertion below pass with the fix reverted.
    assert!(
        !models.occludes(leaves),
        "premise: leaves do not occlude for culling (cutout sprite). If they did, the two \
         predicates would agree and this gate would measure nothing"
    );
    assert!(
        !models.occludes(glass),
        "premise: glass does not occlude for culling either — so the *only* difference between \
         the two scenes is the AO predicate's answer"
    );
    assert_eq!(
        lodestone_data::shade_brightness::occludes_ambient_light(leaves),
        Some(true),
        "premise: vanilla's getShadeBrightness is 0.2 for leaves (full collision cube, no \
         override) — anchored to the real 26.2 server in \
         crates/lodestone-data/tests/shade_brightness.rs"
    );
    assert_eq!(
        lodestone_data::shade_brightness::occludes_ambient_light(glass),
        Some(false),
        "premise: TransparentBlock overrides getShadeBrightness to 1.0, so glass is exempt \
         *despite* being a full collision cube — this is what makes it a control against a \
         collision-shape-only derivation"
    );

    let leaf = measure(&models, leaves, air);
    let control = measure(&models, glass, air);

    let darkened = round3(DOWN_SHADE * AO_THREE_OCCLUDERS);
    let bright = round3(DOWN_SHADE);
    // The four `face_shade` constants and nothing else: what a mesh looks like
    // when every AO factor is 1.0.
    let shade_only: Vec<f32> = vec![0.5, 0.6, 0.8, 1.0];

    println!("=== CANOPY AO PREDICATE GATE (mesh_models, real baked geometry) ===");
    println!(
        "  predicted darkest vertex: {darkened:.3} with vanilla's getShadeBrightness, \
         {bright:.3} with the occludes_at bug"
    );
    println!(
        "  leaves: min ao {:.3} at {} vertices, bbox {:?}, distinct ao {:?}, {} quads",
        leaf.min_ao, leaf.min_count, leaf.min_bbox, leaf.distinct, leaf.quads
    );
    println!(
        "  glass:  min ao {:.3} at {} vertices, bbox {:?}, distinct ao {:?}, {} quads",
        control.min_ao, control.min_count, control.min_bbox, control.distinct, control.quads
    );

    // --- The subject. `ao = face_shade x (1 - 0.2k)`, so the mesh's darkest
    // vertex is exactly `face_shade(Down) x (1 - 0.2*3)` when leaves occlude and
    // exactly `face_shade(Down)` when they do not. Both hypotheses predicted;
    // only one can be measured.
    assert!(
        (leaf.min_ao - darkened).abs() < 1e-3,
        "leaf canopy interior: the darkest vertex must be {darkened:.3} (face_shade(Down) x \
         three occluding AO cells), got {:.3}. A reading of {bright:.3} is the bug — the AO \
         term asked occludes_at, which answers `false` for a cutout leaf, so nothing under a \
         canopy darkened",
        leaf.min_ao
    );
    // Not one stray vertex, and not the whole mesh either: a solid canopy has
    // thousands of interior Down corners with all three ring cells occupied.
    assert!(
        leaf.min_count > 1000,
        "only {} vertices reached the darkest value — a canopy interior should darken in bulk, \
         so this suggests a single corner case rather than the AO term",
        leaf.min_count
    );
    assert!(
        leaf.distinct.len() > shade_only.len(),
        "the leaf mesh carries only {:?} distinct ao values — that is the face-shade set alone, \
         i.e. AO contributed nothing anywhere",
        leaf.distinct
    );

    // --- The control, and the reason it is glass specifically: glass IS a full
    // collision cube, so a predicate built from `collision_shapes` alone (the
    // seven getShadeBrightness overrides dropped) would darken it exactly like
    // the leaves.
    assert!(
        (control.min_ao - bright).abs() < 1e-3,
        "control: glass is vanilla-exempt (TransparentBlock.getShadeBrightness -> 1.0), so its \
         darkest vertex must stay at face_shade(Down) = {bright:.3}, got {:.3} at {} vertices \
         in {:?}. This firing means the predicate stopped honouring the overrides",
        control.min_ao,
        control.min_count,
        control.min_bbox
    );
    assert_eq!(
        control.distinct, shade_only,
        "control: with every AO factor at 1.0 the only distinct ao values can be the four \
         face_shade constants {shade_only:?}; got {:?}, so ambient occlusion fired somewhere \
         vanilla applies none",
        control.distinct
    );
}
