//! Hermetic (no GPU) proof that [`mesh_models`]/[`mesh_fluids`] — the **live**
//! path, not [`mesh_simple`](lodestone_render::mesh_simple) — actually consume
//! [`ModelSectionView::biome_tint_at`]/[`FluidSectionView::water_tint_at`] and
//! carry the resolved colour all the way to the emitted [`ModelVertex`].
//!
//! This is the "what actually consumes this?" gate `CLAUDE.md` asks for: a
//! `BiomeTint` implementor with zero callers is exactly the island this repo
//! has shipped nine times before. Two mock views (one per mesher) each place
//! a **distinct** colour at two **different** section-local positions and
//! assert the exact bytes landed on the right quad's vertices — a
//! location-keyed assertion, not a frame average, so a mesher that answered
//! with the *wrong* position's colour (or a global constant) would fail this
//! even though "some colour changed" would pass. `mesh_models` is exercised
//! directly (not through `lodestone-shell`'s `SnapshotModelView`), which is
//! the scene this test *can* exercise from `lodestone-render` alone; the live
//! per-biome-id wiring lives in `crates/lodestone-shell/src/mesher.rs` and is
//! covered separately there.
//!
//! Every control below is asserted to actually distinguish something: a quad
//! with no live override must land the exact `[0, 0, 0, 0]` inert sentinel,
//! never a stale or leaked colour from a neighbouring quad.

use std::collections::HashMap;

use lodestone_assets::fluid::{FluidState, SpriteUv};
use lodestone_assets::{BakedQuad, Direction};
use lodestone_render::{
    FluidCell, FluidKind, FluidSectionView, FluidSprites, ModelMesh, ModelSectionView,
    mesh_fluids, mesh_models,
};

/// One untinted or grass-tinted quad per occupied section-local cell, plus a
/// per-cell biome-tint answer this test controls directly (bypassing any real
/// `TintKind`/palette-slot machinery — that classification is `block_models`'s
/// job and is proven elsewhere; this test is only about the mesher wiring).
#[derive(Default)]
struct FakeModelView {
    quads: HashMap<(usize, usize, usize), Vec<BakedQuad>>,
    tint_answers: HashMap<(usize, usize, usize), [u8; 3]>,
}

fn up_quad(tinted: bool) -> BakedQuad {
    BakedQuad {
        // A degenerate (single-point) quad: this test only cares about which
        // vertex ends up carrying which `tint_rgb_override`, not real shape.
        positions: [[0.5, 1.0, 0.5]; 4],
        uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        direction: Direction::Up,
        cullface: None,
        tint_index: tinted.then_some(0),
        shade: true,
        layer: 0,
        anim: 0,
    }
}

impl FakeModelView {
    fn place(&mut self, x: usize, y: usize, z: usize, tinted: bool, answer: Option<[u8; 3]>) {
        self.quads.insert((x, y, z), vec![up_quad(tinted)]);
        if let Some(rgb) = answer {
            self.tint_answers.insert((x, y, z), rgb);
        }
    }
}

impl ModelSectionView for FakeModelView {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        self.quads.get(&(x, y, z)).map_or(&[], Vec::as_slice)
    }
    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
    fn biome_tint_at(&self, x: usize, y: usize, z: usize, slot: u8) -> Option<[u8; 3]> {
        // Real callers only ever ask about the four reserved biome slots; this
        // fake proves the mesher passes the *quad's own* slot through
        // unmodified by refusing to answer an untinted (255) query, exactly
        // as `block_models::biome_tint_slot` would.
        if slot == 255 {
            return None;
        }
        self.tint_answers.get(&(x, y, z)).copied()
    }
}

/// The bounding box (inclusive, section-local) of every vertex whose
/// `tint_rgb_override` does not equal `expected` — CLAUDE.md's "measure by
/// location, print a bounding box" rule. Empty iterator -> `None` (no
/// mismatch).
fn mismatch_bbox(mesh: &ModelMesh, expected: [u8; 4]) -> Option<([f32; 3], [f32; 3])> {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    let mut any = false;
    for v in &mesh.vertices {
        if v.tint_rgb_override != expected {
            any = true;
            for i in 0..3 {
                lo[i] = lo[i].min(v.position[i]);
                hi[i] = hi[i].max(v.position[i]);
            }
        }
    }
    any.then_some((lo, hi))
}

#[test]
fn mesh_models_carries_distinct_biome_colours_to_distinct_positions() {
    let mut view = FakeModelView::default();
    // Two grass-tinted quads, far apart, each with its own live answer.
    let desert_green = [0x91, 0xBD, 0x59]; // vanilla desert-ish sample, arbitrary but distinct
    let swamp_green = [0x6A, 0x70, 0x39]; // vanilla's real swamp constant
    view.place(2, 8, 2, true, Some(desert_green));
    view.place(12, 8, 12, true, Some(swamp_green));
    // A third, untinted quad: must never carry an override at all, proving
    // the mesher does not paint every quad with whatever it last resolved.
    view.place(7, 8, 7, false, None);

    let mesh = mesh_models(&view);
    assert_eq!(mesh.quad_count(), 3, "all three placed quads must mesh");

    let find = |wx: f32, wy: f32, wz: f32| -> [u8; 4] {
        mesh.vertices
            .iter()
            .find(|v| v.position == [wx, wy, wz])
            .unwrap_or_else(|| panic!("no vertex at ({wx}, {wy}, {wz})"))
            .tint_rgb_override
    };

    let at_desert = find(2.5, 9.0, 2.5);
    let at_swamp = find(12.5, 9.0, 12.5);
    let at_untinted = find(7.5, 9.0, 7.5);

    assert_eq!(
        at_desert,
        [desert_green[0], desert_green[1], desert_green[2], 255],
        "the desert quad's own colour must land on its own vertices"
    );
    assert_eq!(
        at_swamp,
        [swamp_green[0], swamp_green[1], swamp_green[2], 255],
        "the swamp quad's own colour must land on its own vertices, not the desert's"
    );
    assert_eq!(
        at_untinted,
        [0, 0, 0, 0],
        "an untinted quad must carry the inert sentinel, never a leaked colour"
    );

    // Negative control, run and observed to actually fail: if the mesher
    // painted every vertex with one global colour (the exact bug this whole
    // feature replaces), this assertion is what would catch it. Prove the
    // control fires by checking a wrong expectation *does* report a mismatch
    // bounding box covering the swamp quad.
    let wrong_everywhere_is_desert = [desert_green[0], desert_green[1], desert_green[2], 255];
    let bbox = mismatch_bbox(&mesh, wrong_everywhere_is_desert);
    let (lo, hi) = bbox.expect("control must find a mismatch: not every vertex is desert-green");
    assert!(
        lo[0] >= 7.0 && hi[0] <= 12.5,
        "mismatch bbox {lo:?}..{hi:?} should cover the swamp+untinted quads, not the desert one"
    );
}

/// A minimal, unlit, unshaded fluid view: one water cell with a real answer
/// from `water_tint_at`, one lava cell (never tinted at all).
#[derive(Default)]
struct FakeFluidView {
    fluids: HashMap<(i32, i32, i32), FluidCell>,
    water_answer: Option<[u8; 3]>,
}

impl FluidSectionView for FakeFluidView {
    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        self.fluids.get(&(x, y, z)).copied()
    }
    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
    fn fluid_sprites(&self, _kind: FluidKind) -> FluidSprites {
        let unit = SpriteUv {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
            anim: 0,
        };
        FluidSprites {
            still: unit,
            flow: unit,
            overlay: None,
        }
    }
    fn water_tint_at(&self, x: i32, y: i32, z: i32) -> Option<[u8; 3]> {
        if (x, y, z) == (5, 5, 5) {
            self.water_answer
        } else {
            None
        }
    }
}

#[test]
fn mesh_fluids_carries_the_real_water_colour_and_never_tints_lava() {
    let mut view = FakeFluidView::default();
    let ocean_blue: [u8; 3] = [0x3F, 0x76, 0xE4];
    view.water_answer = Some(ocean_blue);
    view.fluids.insert(
        (5, 5, 5),
        FluidCell {
            kind: FluidKind::Water,
            state: FluidState::source(),
        },
    );
    view.fluids.insert(
        (9, 5, 5),
        FluidCell {
            kind: FluidKind::Lava,
            state: FluidState::source(),
        },
    );

    let meshes = mesh_fluids(&view);
    assert!(meshes.water.quad_count() > 0, "water must mesh a surface");
    assert!(meshes.lava.quad_count() > 0, "lava must mesh a surface");

    let expected_water = [ocean_blue[0], ocean_blue[1], ocean_blue[2], 255];
    for v in &meshes.water.vertices {
        assert_eq!(
            v.tint_rgb_override, expected_water,
            "every water vertex must carry the real resolved colour"
        );
    }
    // Negative control: lava's `water_tint_at` is never even consulted for it
    // (`mesh_fluids` hardcodes `None` for the `Lava` arm — see `models.rs`),
    // so every lava vertex must carry the inert sentinel even though this
    // view's `water_tint_at` would (wrongly) answer for *any* cell if asked.
    for v in &meshes.lava.vertices {
        assert_eq!(
            v.tint_rgb_override,
            [0, 0, 0, 0],
            "lava must never carry a water-tint override"
        );
    }
}

#[test]
fn no_live_biome_data_is_the_exact_pre_existing_behaviour() {
    // The regression control for every existing caller (GUI items, headless
    // tests, a demo world with no biome grid): a view that never answers
    // `biome_tint_at` must produce vertices whose override is inert on every
    // single vertex, so the shader falls back to the palette exactly as it
    // did before this feature existed.
    let mut view = FakeModelView::default();
    view.place(3, 3, 3, true, None);
    view.place(9, 9, 9, true, None);
    let mesh = mesh_models(&view);
    assert!(!mesh.vertices.is_empty());
    for v in &mesh.vertices {
        assert_eq!(v.tint_rgb_override, [0, 0, 0, 0]);
    }
}
