//! Vanilla's render layer is per **quad**, not per block state — and this gate
//! drives the real production producer to prove it.
//!
//! `SectionCompiler` sends every quad to `quad.materialInfo().layer()`, which
//! `ChunkSectionLayer.byTransparency` derives from the transparency of that
//! quad's own sprite. So `grass_block`'s six fully opaque cube faces draw
//! through `SOLID_TERRAIN` — a pipeline that defines no `ALPHA_CUTOUT` and
//! therefore runs **no alpha test at all** — while its four coplanar
//! `grass_block_side_overlay` decals draw through `CUTOUT_TERRAIN` at `0.5`.
//!
//! This client used to roll a whole block state up to "the most transparent
//! layer across its faces", so those six opaque faces inherited `Cutout` from
//! the decals and were alpha-tested by a discard vanilla never applies to them.
//! Under minification the mip chain can pull a filtered alpha below the
//! threshold at a sprite edge, and the discard then punches a hole through a
//! face vanilla paints opaque.
//!
//! # Why the fixture has to be `grass_block`
//!
//! A gate whose fixture is a single-sprite block state **cannot see this bug**:
//! per-quad and per-block-state agree on every such block by construction. The
//! defect needs a state that mixes an opaque sprite and a non-opaque one in one
//! model, and `grass_block[snowy=false]` is the vanilla one. `stone` and
//! `white_stained_glass` are here as the two uniform controls that bracket it —
//! all-`Solid` and all-`Translucent` — so a failure says which end moved.
//!
//! # Both hypotheses, from outside the code under test
//!
//! `block/grass_block.json` in the 26.2 jar declares two `[0,0,0]..[16,16,16]`
//! elements: six faces textured `#bottom`/`#top`/`#side` and four textured
//! `#overlay`. Decoding the four PNGs those resolve to gives, per texel alpha:
//!
//! | sprite | alpha values | layer |
//! |---|---|---|
//! | `block/dirt` | 255 × 256 | `Solid` |
//! | `block/grass_block_top` | 255 × 256 | `Solid` |
//! | `block/grass_block_side` | 255 × 256 | `Solid` |
//! | `block/grass_block_side_overlay` | 0 × 211, 255 × 45 | `Cutout` |
//!
//! So the per-quad hypothesis predicts **6 quads (24 vertices) with the cutout
//! bypass set and 4 quads (16 vertices) without**; the per-block-state
//! hypothesis predicts **0 and 40**. The two differ at every arm, which is what
//! makes this input discriminating — and the block-state layer is asserted to
//! still be `Cutout` in the same test, so this is a real divergence between two
//! live answers rather than a renaming.
//!
//! `#[ignore]`d and fail-closed: a missing `client.jar` is an environment
//! failure, never a silent skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test per_quad_render_layer -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use lodestone::mesher::{SectionKey, mesh_snapshot_models_layers, snapshot_section};
use lodestone_assets::{ResourceManager, ResourceSource, ZipSource};
use lodestone_data::block_states::StateId;
use lodestone_model::BlockStateRegistry;
use lodestone_render::{
    BlockModels, BlocksJsonRegistry, ModelMesh, RenderLayer, blocks_json_registry,
};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

const SECTIONS: usize = 1;

/// The biome-blend radius `mesh_snapshot_models` uses in production. Named here
/// rather than imported so this file drives the layered entry point with the
/// same value the single-mesh one would.
const BLEND_RADIUS: i32 = 2;

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

fn typed_state(raw: u32) -> StateId {
    StateId::new(raw).expect("state id from blocks.json is in the built-in census")
}

/// The first state of `name` whose properties contain every `(key, value)` in
/// `props`. Explicit because `grass_block`'s `snowy=true` state sorts first and
/// bakes an entirely different model (`grass_block_snow`, six opaque faces and
/// no overlay), which would silently turn the discriminating fixture into a
/// uniform one — the exact coincidence this gate exists to avoid.
fn state_id(reg: &impl BlockStateRegistry, name: &str, props: &[(&str, &str)]) -> u32 {
    for id in 0..reg.state_count() {
        let Some(state) = reg.resolve(id) else {
            continue;
        };
        if state.block.to_string() != name {
            continue;
        }
        if props
            .iter()
            .all(|(k, v)| state.properties.get(*k).map(String::as_str) == Some(*v))
        {
            return id;
        }
    }
    panic!("{name}{props:?} present in blocks.json");
}

/// One air column with `block` placed at `(8, 8, 8)` — surrounded by air on all
/// six sides, so no `cullface` quad is dropped and the emitted quad count is the
/// model's own.
fn column(air: u32, block: u32) -> LoadedChunk {
    let mut col = ChunkColumn::new(
        0,
        SECTIONS,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        air,
        0,
    );
    col.set_block(8, 8, 8, block);
    LoadedChunk::new(col, ColumnLight::new(SECTIONS), Heightmaps::new(), Vec::new())
}

fn world_with(air: u32, block: u32) -> World {
    let mut world = World::new();
    for dx in -1..=1i32 {
        for dz in -1..=1i32 {
            world.load(ChunkPos::new(dx, dz), column(air, block));
        }
    }
    world
}

/// `(bypassed vertices, tested vertices, translucent-mesh vertices)` for one
/// block state, meshed through the **production** call — the same
/// `SnapshotModelView` the live mesher builds, not a hand-rolled test view.
fn measure(models: &BlockModels, reg: &BlocksJsonRegistry, block: u32) -> (usize, usize, usize) {
    let air = state_id(reg, "minecraft:air", &[]);
    let world = world_with(air, block);
    let key = SectionKey {
        cx: 0,
        cz: 0,
        si: 0,
        min_y: 0,
    };
    let snapshot = snapshot_section(&world, key).expect("snapshot the subject section");
    let (opaque, translucent) = mesh_snapshot_models_layers(&snapshot, models, true, BLEND_RADIUS);
    let bypassed = count_bypass(&opaque, true);
    let tested = count_bypass(&opaque, false);
    (bypassed, tested, translucent.vertices.len())
}

fn count_bypass(mesh: &ModelMesh, bypassed: bool) -> usize {
    mesh.vertices
        .iter()
        .filter(|v| (v.cutout_bypass != 0) == bypassed)
        .count()
}

/// The whole gate in one test, so every arm reports: an `assert!` inside a loop
/// aborts at the first failure and leaves the rest of the measurement as an
/// argument rather than an observation, so the three subjects are measured
/// first and asserted on afterwards.
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn a_block_states_opaque_faces_are_not_alpha_tested_because_a_sibling_face_is_cutout() {
    let root = pack_root();
    let models = load_models(&root);
    let reg = registry(&root);

    let grass = state_id(&reg, "minecraft:grass_block", &[("snowy", "false")]);
    let stone = state_id(&reg, "minecraft:stone", &[]);
    let glass = state_id(&reg, "minecraft:white_stained_glass", &[]);

    let m_grass = measure(&models, &reg, grass);
    let m_stone = measure(&models, &reg, stone);
    let m_glass = measure(&models, &reg, glass);

    // The per-block-state roll-up is unchanged and still says `Cutout` for
    // grass — so the two answers genuinely disagree on this input, and the
    // per-quad one is not just the old one under a new name. `layer` still
    // feeds occlusion and the packed fast path, which are per-block in vanilla
    // too, so it is *correct* that it did not move.
    assert_eq!(
        models.layer(typed_state(grass)),
        RenderLayer::Cutout,
        "grass_block's block-state layer must still be Cutout — if it is not, this gate's \
         premise (that per-quad and per-block-state disagree here) has evaporated"
    );

    let mut failures: Vec<String> = Vec::new();
    // 6 quads × 4 vertices from the opaque element, 4 × 4 from the overlay one.
    // The wrong (per-block-state) hypothesis gives (0, 40, 0) at this row.
    if m_grass != (24, 16, 0) {
        failures.push(format!(
            "grass_block[snowy=false]: expected (bypassed, tested, translucent) = (24, 16, 0) \
             — six opaque cube faces on the no-alpha-test pass and four \
             grass_block_side_overlay decals alpha-tested. Per-block-state would give \
             (0, 40, 0). Measured {m_grass:?}"
        ));
    }
    // Uniform all-`Solid` control: every face of `cube_all` samples one opaque
    // sprite, so per-quad and per-block-state already agreed here before this
    // change — and this row must therefore be all-bypassed either way.
    if m_stone != (24, 0, 0) {
        failures.push(format!(
            "stone: expected (24, 0, 0) — six faces of one all-opaque sprite. Measured {m_stone:?}"
        ));
    }
    // Uniform all-`Translucent` control: every quad routes to the second mesh,
    // so the opaque mesh is empty at both counters. This is the arm that would
    // catch a per-quad split that accidentally stranded translucent geometry on
    // the opaque pass.
    if m_glass != (0, 0, 24) {
        failures.push(format!(
            "white_stained_glass: expected (0, 0, 24) — every quad on the blended pass. \
             Measured {m_glass:?}"
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The classification itself, read straight off the real stitched atlas: the
/// sprite table `SnapshotModelView::quad_layer` indexes must give six `Solid`
/// and four `Cutout` for grass_block's ten baked quads.
///
/// Separate from the mesher gate above because it fails one layer earlier: if
/// this is red, the mesher gate's numbers say nothing about the mesher.
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn grass_blocks_ten_baked_quads_split_six_solid_four_cutout_by_their_own_sprites() {
    let root = pack_root();
    let models = load_models(&root);
    let reg = registry(&root);
    let grass = state_id(&reg, "minecraft:grass_block", &[("snowy", "false")]);

    let quads = models.quads(typed_state(grass));
    let layers: Vec<Option<RenderLayer>> = quads
        .iter()
        .map(|q| models.sprite_layer(q.sprite))
        .collect();

    let solid = layers.iter().filter(|l| **l == Some(RenderLayer::Solid)).count();
    let cutout = layers.iter().filter(|l| **l == Some(RenderLayer::Cutout)).count();
    let unknown = layers.iter().filter(|l| l.is_none()).count();

    assert_eq!(
        (quads.len(), solid, cutout, unknown),
        (10, 6, 4, 0),
        "grass_block[snowy=false] bakes ten quads — six from the #bottom/#top/#side element \
         (all three sprites all-255 alpha) and four from the #overlay element \
         (grass_block_side_overlay, 211 clear texels of 256). Per-quad layers were {layers:?}"
    );
}
