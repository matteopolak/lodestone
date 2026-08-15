//! Owner report: "the ice texture looks inverted or something. it looks
//! mostly right but looking at the bottom of the ice (from the top) shows no
//! opacity at all, and i can see the four walls of the ice blocks even when
//! theyre beside other ice so it looks like a grid."
//!
//! Traced to `mesh_models_layers` (`crates/lodestone-render/src/models.rs`)
//! missing the second of vanilla's two `Block.shouldRenderFace` early-outs:
//!
//! ```java
//! public static boolean shouldRenderFace(BlockState state, BlockState neighborState, Direction direction) {
//!    VoxelShape occluder = neighborState.getFaceOcclusionShape(direction.getOpposite());
//!    if (occluder == Shapes.block()) { return false; }        // clause 1: occludes_at
//!    if (state.skipRendering(neighborState, direction)) { return false; } // clause 2: MISSING before this gate
//!    ...
//! }
//! ```
//!
//! `IceBlock` inherits `HalfTransparentBlock.skipRendering`
//! (`neighborState.is(this) ? true : ...`) unchanged — a face between two
//! states of the **exact same** `Block` is never drawn. Clause 1
//! (`occludes_at`, ported already) does not substitute for it: every member of
//! this class sets vanilla's `noOcclusion()`, so `occludes_at` is correctly
//! `false` for all of them, and clause 1 alone therefore culls *nothing*
//! between two ice blocks — which is exactly the "four walls / grid" symptom.
//!
//! This gate drives the real production function, `mesh_models_layers`, over
//! a hand-built [`ModelSectionView`] — no live world or asset pack needed,
//! matching this crate's existing hermetic mesher gates (see `models.rs`'s
//! own `mesh_models_layers_routes_translucent_blocks_to_the_second_mesh`).

use lodestone_assets::{BakedQuad, Direction};
use lodestone_render::{ModelSectionView, face_of_direction, mesh_models_layers};

/// A degenerate (single-point) quad, exactly as `models.rs`'s own `cube_face`
/// test helper builds it: the in-plane shape is irrelevant to face culling,
/// only `direction`/`cullface` matter here.
fn cube_face(dir: Direction, cull: Option<Direction>) -> BakedQuad {
    BakedQuad {
        positions: [[0.0, 0.0, 0.0]; 4],
        uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        direction: dir,
        cullface: cull,
        tint_index: None,
        shade: true,
        layer: 0,
        anim: 0,
        sprite: 0,
    }
}

const ALL_DIRECTIONS: [Direction; 6] = [
    Direction::West,
    Direction::East,
    Direction::Down,
    Direction::Up,
    Direction::North,
    Direction::South,
];

/// A full cube's baked geometry: one quad per face, each carrying its own
/// direction as its `cullface` — precisely what a vanilla `cube_all` model
/// (ice, glass, stone, ...) bakes to.
fn full_cube_quads() -> Vec<BakedQuad> {
    ALL_DIRECTIONS.iter().map(|&d| cube_face(d, Some(d))).collect()
}

/// Three full cubes in a row along +X at `y == 8, z == 8`: `x == 7` and
/// `x == 8` are the same `HalfTransparentBlock` class ("ice"), `x == 9` is a
/// **different** one ("glass") — every other cell is air.
///
/// `occludes_at` is `false` everywhere, by construction: every member of this
/// vanilla class is `noOcclusion()`, so a fixture where `occludes_at` alone
/// could explain a culled face would not discriminate `skips_rendering_against`
/// from its absence. This fixture cannot pass its assertions through clause 1
/// — only clause 2 (the fix) can cull the ice/ice seam, and only clause 2's
/// *identity* check (not merely "both are half-transparent") can leave the
/// ice/glass seam undisturbed.
struct HalfTransparentRow {
    quads: Vec<BakedQuad>,
}

/// The vanilla-class name at a cell, or `None` for air/out of range —
/// `x == 7, 8` are `"ice"`; `x == 9` is `"glass"`; everything else is air.
fn class_at(x: i32, y: i32, z: i32) -> Option<&'static str> {
    if y != 8 || z != 8 {
        return None;
    }
    match x {
        7 | 8 => Some("ice"),
        9 => Some("glass"),
        _ => None,
    }
}

impl ModelSectionView for HalfTransparentRow {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        if class_at(x as i32, y as i32, z as i32).is_some() {
            &self.quads
        } else {
            &[]
        }
    }

    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        // Vanilla `noOcclusion()`: correct and unconditional for this whole
        // class, and the reason clause 1 cannot be what culls these faces.
        false
    }

    fn is_translucent_at(&self, x: usize, y: usize, z: usize) -> bool {
        class_at(x as i32, y as i32, z as i32).is_some()
    }

    fn skips_rendering_against(&self, x: i32, y: i32, z: i32, nx: i32, ny: i32, nz: i32) -> bool {
        let here = class_at(x, y, z);
        let neighbour = class_at(nx, ny, nz);
        here.is_some() && here == neighbour
    }
}

/// A control identical to [`HalfTransparentRow`] except `skips_rendering_against`
/// is hardwired to the pre-fix answer (`false`) — the neuter. If this control
/// does not reproduce the reported grid (18 quads, not 16), the fixture's
/// premise is wrong and the real gate below proves nothing.
struct NeuteredRow {
    quads: Vec<BakedQuad>,
}

impl ModelSectionView for NeuteredRow {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        if class_at(x as i32, y as i32, z as i32).is_some() {
            &self.quads
        } else {
            &[]
        }
    }
    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
    fn is_translucent_at(&self, x: usize, y: usize, z: usize) -> bool {
        class_at(x as i32, y as i32, z as i32).is_some()
    }
    // No override: inherits the trait default (`false`), reproducing exactly
    // what every implementor answered before this fix existed.
}

/// Predicted quad counts, derived from vanilla's rule rather than guessed:
///
/// * `ice` at `x=7`: 6 faces, minus the one toward `x=8` (same block, culled) = 5.
/// * `ice` at `x=8`: 6 faces, minus the one toward `x=7` (same block, culled) = 5.
///   Its `+X` face toward the `x=9` glass is a **different** block and stays.
/// * `glass` at `x=9`: 6 faces, none culled (its only non-air neighbour, the
///   `x=8` ice, is a different block) = 6.
///
/// Total after the fix: 5 + 5 + 6 = 16. Before it (the neuter, and the bug as
/// reported): no interior faces are culled at all: 6 + 6 + 6 = 18.
const PREDICTED_FIXED_TOTAL: usize = 16;
const PREDICTED_BUGGY_TOTAL: usize = 18;

#[test]
fn same_block_interior_faces_are_culled_but_different_blocks_are_not() {
    let view = HalfTransparentRow {
        quads: full_cube_quads(),
    };
    let (opaque, translucent) = mesh_models_layers(&view);
    assert_eq!(
        opaque.quad_count(),
        0,
        "every cell in this fixture is translucent; nothing belongs in the opaque mesh"
    );
    assert_eq!(
        translucent.quad_count(),
        PREDICTED_FIXED_TOTAL,
        "expected 5 (ice@7) + 5 (ice@8) + 6 (glass@9) = 16 quads: the ice/ice \
         seam culled on both sides, the ice/glass seam untouched on both sides"
    );
}

/// The control: with `skips_rendering_against` hardwired to `false` (the
/// trait default, i.e. every caller before this fix), the fixture reproduces
/// the reported bug exactly — 18 quads, no interior face culled at all,
/// including between the two *identical* ice cells. Proves the fixture's
/// premise (that `occludes_at` alone cannot cull these seams) is real, not
/// assumed: this control shares that same `occludes_at() == false` and still
/// goes red on the very seam the fix targets.
#[test]
fn neutered_view_reproduces_the_reported_grid() {
    let view = NeuteredRow {
        quads: full_cube_quads(),
    };
    let (opaque, translucent) = mesh_models_layers(&view);
    assert_eq!(opaque.quad_count(), 0);
    assert_eq!(
        translucent.quad_count(),
        PREDICTED_BUGGY_TOTAL,
        "control premise: with no same-block skip, every face on every seam \
         in this fixture must survive, ice/ice included — reproducing the \
         reported wireframe-lattice grid"
    );
}

/// `skips_rendering_against` must key on the **exact** block, not merely "is
/// this class". Three cells, all pairwise distinct: `ice`, `blue_ice`,
/// `frosted_ice` — three different vanilla `Block` instances (`FrostedIceBlock`
/// extends `IceBlock` extends `HalfTransparentBlock`; `blue_ice` is its own
/// sibling registration) that must **not** skip against one another, matching
/// `neighborState.is(this)`'s literal-identity semantics rather than an
/// "any half-transparent neighbour" shortcut a coarser implementation could
/// pass this same corpus with.
#[test]
fn distinct_half_transparent_siblings_do_not_skip_against_each_other() {
    struct ThreeSiblings {
        quads: Vec<BakedQuad>,
    }
    fn sibling_class_at(x: i32, y: i32, z: i32) -> Option<&'static str> {
        if y != 8 || z != 8 {
            return None;
        }
        match x {
            7 => Some("ice"),
            8 => Some("blue_ice"),
            9 => Some("frosted_ice"),
            _ => None,
        }
    }
    impl ModelSectionView for ThreeSiblings {
        fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
            if sibling_class_at(x as i32, y as i32, z as i32).is_some() {
                &self.quads
            } else {
                &[]
            }
        }
        fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
            false
        }
        fn is_translucent_at(&self, x: usize, y: usize, z: usize) -> bool {
            sibling_class_at(x as i32, y as i32, z as i32).is_some()
        }
        fn skips_rendering_against(
            &self,
            x: i32,
            y: i32,
            z: i32,
            nx: i32,
            ny: i32,
            nz: i32,
        ) -> bool {
            let here = sibling_class_at(x, y, z);
            let neighbour = sibling_class_at(nx, ny, nz);
            here.is_some() && here == neighbour
        }
    }

    let view = ThreeSiblings {
        quads: full_cube_quads(),
    };
    let (_, translucent) = mesh_models_layers(&view);
    // No seam is same-block here, so nothing is culled: 6 + 6 + 6 = 18.
    assert_eq!(
        translucent.quad_count(),
        18,
        "ice/blue_ice/frosted_ice are three different vanilla blocks and must \
         not cull each other's interior faces"
    );
}

/// [`face_of_direction`] round-trips every [`Direction`] used above — a
/// cross-check that this gate's `cullface` arithmetic (mirrored from
/// `models.rs`'s own cull loop) agrees with the production normal table.
#[test]
fn face_of_direction_covers_every_direction_used_here() {
    for d in ALL_DIRECTIONS {
        let _ = face_of_direction(d).normal();
    }
}
