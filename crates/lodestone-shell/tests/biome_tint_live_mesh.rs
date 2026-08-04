//! The live-mesher end of the biome-tint fix: proves `mesh_snapshot_models` —
//! the exact call `crates/lodestone-shell/src/mesher.rs`'s `MeshScheduler`
//! makes for real terrain — renders **two different grass colours for two
//! different biomes in the same section**, over a real `BlockModels` baked
//! from `client.jar`.
//!
//! `crates/lodestone-render/tests/biome_tint_gate.rs` already proves
//! `mesh_models`/`mesh_fluids` consume `ModelSectionView::biome_tint_at`/
//! `FluidSectionView::water_tint_at` with a hand-built mock view. This test is
//! the layer above it: it proves `SnapshotModelView` (the *real* view,
//! `crates/lodestone-shell/src/mesher.rs`) actually implements those methods
//! against a real `ChunkSection::biome_at_block` and the real vanilla biome
//! table — the seam a mock view cannot exercise, and the one CLAUDE.md's rule
//! 1 calls the "what actually consumes this?" question.
//!
//! # Both hypotheses, and which one this measures
//!
//! `minecraft:swamp`'s grass modifier (`GrassColorModifier::Swamp`) *ignores*
//! the colormap entirely and returns the constant `0x6A7039`
//! (`swamp_modifier_two_tone_by_noise` in `lodestone-assets/tests/tint.rs`
//! already proves that constant against the jar source; `grass_modifier_noise`
//! is unported and defaults to `0.0`, which is the `>= -0.1` branch — see
//! `lodestone-render/src/biome_tint.rs`'s module docs). So the swamp side has
//! an **exact, outside-derived** prediction: `[0x6A, 0x70, 0x39]`. The desert
//! side has no such simple constant (it is a real colormap sample at
//! `temperature=2.0, downfall=0.0`), so it is checked by **inequality**
//! against both the swamp constant and the pre-existing plains-default
//! fallback (`BlockModels::tint_palette()[GRASS_TINT_SLOT]`) — proof that a
//! *different* real colour reached the vertex, not a coincidence of the fixed
//! default reappearing.
//!
//! # Two meshers, and which one this exercises
//!
//! `mesh_snapshot_models` calls `lodestone_render::mesh_models`, never
//! `mesh_simple` — confirmed by reading `crates/lodestone-shell/src/
//! mesher.rs` directly (`mesh_snapshot` is the `mesh_simple` caller, a
//! different function, used for packed/greedy terrain only). Grass is tinted,
//! so it was never a packed-cube candidate in the first place (`lodestone-
//! render/src/models.rs`'s D1 module docs: "the dominant overworld surfaces —
//! grass ... and water — are not [packed cubes]").
//!
//! `#[ignore]`d and fail-closed like `canopy_ao.rs`/`water_seam_convergence.rs`:
//! a missing `client.jar` is an environment failure, never a silent skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test biome_tint_live_mesh -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use lodestone::mesher::{ColumnSource, SectionKey, mesh_snapshot_models, snapshot_section_in};
use lodestone_assets::{ResourceManager, ResourceSource, ZipSource};
use lodestone_model::BlockStateRegistry;
use lodestone_render::{
    BlockModels, BlocksJsonRegistry, GRASS_TINT_SLOT, ModelMesh, SkyDefault, blocks_json_registry,
};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

const SECTIONS: usize = 1;

/// The wire ids this test's biome table gives `minecraft:desert`/`minecraft:swamp`
/// — see `crates/lodestone-shell/src/mesher.rs`'s `FALLBACK_BIOME_NAMES`
/// (alphabetical), independently re-derived here rather than imported, so a
/// silent reordering of that table would break this test's premise loudly
/// (an index mismatch renders the *wrong* biome, not a compile error) instead
/// of invisibly.
const DESERT_ID: u32 = 12;
const SWAMP_ID: u32 = 47;

/// `GrassColorModifier::Swamp`'s constant, independent of the colormap —
/// verified against the jar in `lodestone-assets/tests/tint.rs`'s
/// `swamp_modifier_two_tone_by_noise`.
const SWAMP_GRASS: [u8; 3] = [0x6A, 0x70, 0x39];

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

fn state_id(reg: &impl BlockStateRegistry, name: &str) -> u32 {
    for id in 0..reg.state_count() {
        let Some(state) = reg.resolve(id) else {
            continue;
        };
        if state.block.to_string() == name {
            return id;
        }
    }
    panic!("{name} present in blocks.json");
}

fn air_id(reg: &impl BlockStateRegistry) -> u32 {
    state_id(reg, "minecraft:air")
}

/// `minecraft:grass_block`'s `snowy=false` state — **not** the first match:
/// `snowy=true`'s state id sorts first in `blocks.json` and its top face uses
/// the untinted `block/snow` texture (vanilla's `grass_block_snow` model), so
/// picking the first match here would silently fixture an untinted block and
/// every assertion in this file would fail for a reason that has nothing to
/// do with biome tint at all.
fn grass_block_id(reg: &impl BlockStateRegistry) -> u32 {
    for id in 0..reg.state_count() {
        let Some(state) = reg.resolve(id) else {
            continue;
        };
        if state.block.to_string() == "minecraft:grass_block"
            && state.properties.get("snowy").map(String::as_str) == Some("false")
        {
            return id;
        }
    }
    panic!("minecraft:grass_block[snowy=false] present in blocks.json");
}

/// One filled-air column, `grass` placed at `(2, 8, 2)` and `(13, 8, 13)`, and
/// its whole 4×4 biome grid split at cell x = 2: cells `x < 2` get
/// `left_biome`, `x >= 2` get `right_biome`. The two grass blocks sit at
/// block x = 2 and x = 13 — biome cells 0 and 3, each 5+ blocks from the
/// x = 8 boundary, well outside `blend_box`'s default radius-2 kernel, so
/// each reads its *pure* biome with nothing blended in from the other side.
fn column(air: u32, grass: u32, left_biome: u32, right_biome: u32) -> LoadedChunk {
    let mut col = ChunkColumn::new(0, SECTIONS, PaletteKind::block_states(), PaletteKind::biomes(), air, left_biome);
    col.set_block(2, 8, 2, grass);
    col.set_block(13, 8, 13, grass);
    for bx in 0..4usize {
        for bz in 0..4usize {
            let biome = if bx < 2 { left_biome } else { right_biome };
            col.set_biome(bx, 8, bz, biome);
        }
    }
    LoadedChunk::new(col, ColumnLight::new(SECTIONS), Heightmaps::new(), Vec::new())
}

fn filled_world(air: u32, grass: u32, left_biome: u32, right_biome: u32) -> World {
    let mut world = World::new();
    for dx in -1..=1i32 {
        for dz in -1..=1i32 {
            world.load(
                ChunkPos::new(dx, dz),
                column(air, grass, left_biome, right_biome),
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

/// Every vertex `tint_rgb_override` at exactly `(x + 0.5, 9.0, z + 0.5)`-ish —
/// i.e. anywhere on the grass block's top face at block `(x, 8, z)` — printed
/// with its bounding box so a mismatch says *where*, not just *how much*.
fn tint_on_top_face(mesh: &ModelMesh, x: f32, z: f32) -> [u8; 4] {
    // Grass block geometry near the block's top edge (y = 9) isn't only the
    // Up face: the side elements' top rim sits at the same height and is
    // genuinely untinted (the base dirt texture, `flag == 0`). Filtering to
    // just the *tinted* vertices in the region isolates the Up face (and any
    // tinted side-overlay geometry, which carries the same colour, so
    // uniformity still holds) without needing a face-direction field this
    // vertex format doesn't carry.
    let all_near: Vec<[u8; 4]> = mesh
        .vertices
        .iter()
        .filter(|v| {
            v.position[1] > 8.99
                && v.position[0] >= x
                && v.position[0] <= x + 1.0
                && v.position[2] >= z
                && v.position[2] <= z + 1.0
        })
        .map(|v| v.tint_rgb_override)
        .collect();
    let matches: Vec<[u8; 4]> = all_near.iter().copied().filter(|c| c[3] != 0).collect();
    assert!(
        !matches.is_empty(),
        "no *tinted* top-face vertex found near block ({x}, 8, {z}) — fixture premise broken. \
         All vertices near that position (tinted and untinted): {all_near:?}"
    );
    let first = matches[0];
    for (i, m) in matches.iter().enumerate() {
        assert_eq!(
            *m, first,
            "every tinted vertex near ({x},8,{z}) should carry the same colour \
             (vertex {i} disagreed) — bounding box of all {} tinted matches: {matches:?}",
            matches.len()
        );
    }
    first
}

#[test]
#[ignore = "needs a real client.jar under .cache/mc/<version>/"]
fn live_mesh_snapshot_models_tints_two_biomes_differently() {
    let root = pack_root();
    let models = load_models(&root);
    let reg = registry(&root);
    let air = air_id(&reg);
    let grass = grass_block_id(&reg);

    let world = filled_world(air, grass, DESERT_ID, SWAMP_ID);
    let outcome = snapshot_section_in(
        &world,
        subject_key(),
        Some(SECTIONS),
        SkyDefault::Full,
        ColumnSource::Complete,
    );
    let snap = outcome.any().expect("filled 3x3 world snapshots as Ready");
    assert_eq!(
        snap.unloaded_neighbours(),
        0,
        "fixture premise: every neighbour column is loaded"
    );

    let live_mesh = mesh_snapshot_models(&snap, &models);
    assert!(live_mesh.quad_count() > 0, "fixture must actually mesh something");

    let desert_top = tint_on_top_face(&live_mesh, 2.0, 2.0);
    let swamp_top = tint_on_top_face(&live_mesh, 13.0, 13.0);
    // Printed unconditionally (not just on failure) so `--nocapture` shows the
    // real measured bytes next to the exact prediction they're checked
    // against below — CLAUDE.md's "write down what was measured".
    println!(
        "measured: desert_top(rgb)={:?} swamp_top(rgb)={:?} (predicted swamp = {SWAMP_GRASS:?})",
        &desert_top[..3],
        &swamp_top[..3],
    );

    // The override flag (.a) must be set on both — this is the whole point:
    // the live view answered with a real colour, not the palette fallback.
    assert_eq!(desert_top[3], 255, "desert grass must carry a live override");
    assert_eq!(swamp_top[3], 255, "swamp grass must carry a live override");

    // Exact, outside-derived prediction for the swamp side.
    assert_eq!(
        [swamp_top[0], swamp_top[1], swamp_top[2]],
        SWAMP_GRASS,
        "swamp grass must be exactly GrassColorModifier::Swamp's constant, \
         independent of the colormap"
    );

    // The desert side must be a genuinely different colour — not swamp's
    // constant, and not the old flat plains-default this whole feature
    // replaces.
    let plains_default = models.tint_palette()[GRASS_TINT_SLOT as usize];
    let plains_default_bytes = [
        (plains_default[0] * 255.0).round() as u8,
        (plains_default[1] * 255.0).round() as u8,
        (plains_default[2] * 255.0).round() as u8,
    ];
    assert_ne!(
        [desert_top[0], desert_top[1], desert_top[2]],
        SWAMP_GRASS,
        "desert must not accidentally equal swamp's colour"
    );
    assert_ne!(
        [desert_top[0], desert_top[1], desert_top[2]],
        plains_default_bytes,
        "desert must render its own real colour, not the pre-existing flat plains default \
         (proves this is genuinely per-biome, not the old single-shade bug still showing \
         through by coincidence)"
    );

    // Negative control, executed and observed to hold: a world with the SAME
    // biome on both sides must NOT differ — ruling out "any two positions
    // always differ" as an explanation (e.g. per-vertex noise, a stale
    // buffer, or reading the wrong cell).
    let uniform_world = filled_world(air, grass, DESERT_ID, DESERT_ID);
    let uniform_outcome = snapshot_section_in(
        &uniform_world,
        subject_key(),
        Some(SECTIONS),
        SkyDefault::Full,
        ColumnSource::Complete,
    );
    let uniform_snap = uniform_outcome.any().expect("uniform world snapshots as Ready");
    let uniform_mesh = mesh_snapshot_models(&uniform_snap, &models);
    let a = tint_on_top_face(&uniform_mesh, 2.0, 2.0);
    let b = tint_on_top_face(&uniform_mesh, 13.0, 13.0);
    assert_eq!(
        a, b,
        "control: two grass blocks in a UNIFORM biome must render identically — \
         a difference here would mean the earlier pass/fail was noise, not biome"
    );
}
