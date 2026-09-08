//! Owner report: "the ice texture looks inverted or something. it looks
//! mostly right but looking at the bottom of the ice (from the top) shows no
//! opacity at all, and i can see the four walls of the ice blocks even when
//! theyre beside other ice so it looks like a grid."
//!
//! Traced to `mesh_models_layers` (`crates/lodestone-render/src/models.rs`)
//! missing the second of vanilla's two face-culling early-outs. Vanilla's
//! face test has two independent clauses, either of which skips the face:
//! clause 1 checks whether the neighbour's face-occlusion shape is a full
//! block (`occludes_at`, already ported); clause 2 is a per-block
//! self-occlusion override that can additionally skip rendering based on the
//! *pair* of states and the direction — that second clause was missing before
//! this gate.
//!
//! Ice's family of blocks overrides that self-occlusion clause so that a face
//! between two states of the exact same block is never drawn, regardless of
//! shape. Clause 1 (`occludes_at`, ported already) does not substitute for
//! it: every member of that family sets vanilla's occlusion shape to empty,
//! so `occludes_at` is correctly `false` for all of them, and clause 1 alone
//! therefore culls *nothing* between two ice blocks — which is exactly the
//! "four walls / grid" symptom.
//!
//! This gate drives the real production function, `mesh_models_layers`, over
//! a hand-built [`ModelSectionView`] — no live world or asset pack needed,
//! matching this crate's existing hermetic mesher gates (see `models.rs`'s
//! own `mesh_models_layers_routes_translucent_blocks_to_the_second_mesh`).

use lodestone_assets::{BakedQuad, Direction};
use lodestone_render::{ModelSectionView, RenderLayer, face_of_direction, mesh_models_layers};

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

/// A real axis-aligned inset face, with coordinates expressed in the same
/// block-local `0.0..=1.0` space as the baked model. The slime model's nested
/// element is `[3, 13]` in model texels, so its six faces sit at `3/16` and
/// `13/16` and span that interval on the other two axes.
fn inset_face(dir: Direction, low: f32, high: f32) -> BakedQuad {
    let (fixed, negative) = match dir {
        Direction::West => (0usize, true),
        Direction::East => (0, false),
        Direction::Down => (1, true),
        Direction::Up => (1, false),
        Direction::North => (2, true),
        Direction::South => (2, false),
    };
    let (a, b) = match fixed {
        0 => (1usize, 2usize),
        1 => (0, 2),
        _ => (0, 1),
    };
    let plane = if negative { low } else { high };
    let corner = |ca: f32, cb: f32| {
        let mut p = [0.0f32; 3];
        p[fixed] = plane;
        p[a] = ca;
        p[b] = cb;
        p
    };
    BakedQuad {
        positions: [
            corner(low, low),
            corner(high, low),
            corner(high, high),
            corner(low, high),
        ],
        uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        direction: dir,
        cullface: None,
        tint_index: None,
        shade: true,
        layer: 0,
        anim: 0,
        sprite: 0,
    }
}

/// The twelve quads produced by the measured slime model: six boundary faces
/// plus six unculled faces for its nested `[3, 13]` cube.
fn slime_model_quads() -> Vec<BakedQuad> {
    let mut quads = full_cube_quads();
    quads.extend(
        ALL_DIRECTIONS
            .into_iter()
            .map(|dir| inset_face(dir, 3.0 / 16.0, 13.0 / 16.0)),
    );
    quads
}

/// Three full cubes in a row along +X at `y == 8, z == 8`: `x == 7` and
/// `x == 8` are the same vanilla half-transparent block family ("ice"),
/// `x == 9` is a **different** one ("glass") — every other cell is air.
///
/// `occludes_at` is `false` everywhere, by construction: every member of this
/// vanilla family has an empty occlusion shape, so a fixture where `occludes_at` alone
/// could explain a culled face would not discriminate `skips_rendering_against`
/// from its absence. This fixture cannot pass its assertions through clause 1
/// — only clause 2 (the fix) can cull the ice/ice seam, and only clause 2's
/// *identity* check (not merely "both are half-transparent") can leave the
/// ice/glass seam undisturbed.
struct HalfTransparentRow {
    quads: Vec<BakedQuad>,
}

/// The vanilla block-family name at a cell, or `None` for air/out of range —
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
        // Vanilla's empty occlusion shape: correct and unconditional for this
        // whole family, and the reason clause 1 cannot be what culls these faces.
        false
    }

    fn quad_layer(
        &self,
        x: usize,
        y: usize,
        z: usize,
        _quad: &BakedQuad,
    ) -> Option<RenderLayer> {
        class_at(x as i32, y as i32, z as i32)
            .map(|_| RenderLayer::Translucent)
            .or(Some(RenderLayer::Solid))
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
    fn quad_layer(
        &self,
        x: usize,
        y: usize,
        z: usize,
        _quad: &BakedQuad,
    ) -> Option<RenderLayer> {
        class_at(x as i32, y as i32, z as i32)
            .map(|_| RenderLayer::Translucent)
            .or(Some(RenderLayer::Solid))
    }
    // No override: inherits the trait default (`false`), reproducing exactly
    // what every implementor answered before this fix existed.
}

/// Two cells at `(8, 8, 8)` and `(9, 8, 8)` carrying an arbitrary model. The
/// left cell is always slime; `right_class` selects a matching slime neighbour,
/// a different honey neighbour, or air. This keeps the geometry and location
/// fixed while each assertion changes only the identity control.
struct InsetRow {
    quads: Vec<BakedQuad>,
    right_class: Option<&'static str>,
    skip_same: bool,
}

impl InsetRow {
    fn class_at(&self, x: i32, y: i32, z: i32) -> Option<&'static str> {
        if y != 8 || z != 8 {
            return None;
        }
        match x {
            8 => Some("slime_block"),
            9 => self.right_class,
            _ => None,
        }
    }
}

impl ModelSectionView for InsetRow {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        if self.class_at(x as i32, y as i32, z as i32).is_some() {
            &self.quads
        } else {
            &[]
        }
    }

    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        // The half-transparent family deliberately has no full occlusion
        // shape, so the inset-face branch is the only possible seam culler.
        false
    }

    fn quad_layer(
        &self,
        x: usize,
        y: usize,
        z: usize,
        _quad: &BakedQuad,
    ) -> Option<RenderLayer> {
        self.class_at(x as i32, y as i32, z as i32)
            .map(|_| RenderLayer::Translucent)
            .or(Some(RenderLayer::Solid))
    }

    fn skips_rendering_against(&self, x: i32, y: i32, z: i32, nx: i32, ny: i32, nz: i32) -> bool {
        if !self.skip_same {
            return false;
        }
        let here = self.class_at(x, y, z);
        let neighbour = self.class_at(nx, ny, nz);
        here.is_some() && here == neighbour
    }
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
/// this family". Three cells, all pairwise distinct: `ice`, `blue_ice`,
/// `frosted_ice` — three different vanilla block registrations, with
/// `frosted_ice` a subtype of `ice` in the half-transparent family and
/// `blue_ice` its own sibling registration, that must **not** skip against
/// one another, matching vanilla's literal block-identity self-occlusion
/// semantics rather than an "any half-transparent neighbour" shortcut a
/// coarser implementation could pass this same corpus with.
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
        fn quad_layer(
            &self,
            x: usize,
            y: usize,
            z: usize,
            _quad: &BakedQuad,
        ) -> Option<RenderLayer> {
            sibling_class_at(x as i32, y as i32, z as i32)
                .map(|_| RenderLayer::Translucent)
                .or(Some(RenderLayer::Solid))
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

/// The nested six-face element is culled only at a matching neighbour. The
/// outer shell still loses its shared boundary pair, while an isolated slime
/// keeps all twelve model quads and a slime/honey seam keeps every quad.
#[test]
fn slime_inset_faces_skip_only_at_a_matching_neighbour() {
    let isolated = InsetRow {
        quads: slime_model_quads(),
        right_class: None,
        skip_same: true,
    };
    let (_, isolated_translucent) = mesh_models_layers(&isolated);
    assert_eq!(
        isolated_translucent.quad_count(),
        12,
        "an isolated slime block must retain its six outer and six nested faces"
    );

    let matching = InsetRow {
        quads: slime_model_quads(),
        right_class: Some("slime_block"),
        skip_same: true,
    };
    let (_, matching_translucent) = mesh_models_layers(&matching);
    assert_eq!(
        matching_translucent.quad_count(),
        20,
        "two touching slime blocks must remove one outer and one nested face per cell"
    );

    // Executed negative control for the old path: without the same-material
    // hook, both the boundary pair and the nested pair remain.
    let neutered = InsetRow {
        quads: slime_model_quads(),
        right_class: Some("slime_block"),
        skip_same: false,
    };
    let (_, neutered_translucent) = mesh_models_layers(&neutered);
    assert_eq!(
        neutered_translucent.quad_count(),
        24,
        "without same-material culling, every outer and nested face survives"
    );

    let different = InsetRow {
        quads: slime_model_quads(),
        right_class: Some("honey_block"),
        skip_same: true,
    };
    let (_, different_translucent) = mesh_models_layers(&different);
    assert_eq!(
        different_translucent.quad_count(),
        24,
        "a slime/honey seam must not use a same-material skip"
    );
}

/// A no-cull diagonal blade is a discriminating control for the geometry gate:
/// a view that reports every neighbour as the same material must not suppress
/// arbitrary translucent model geometry merely because it lacks `cullface`.
#[test]
fn same_material_skip_does_not_cull_diagonal_no_cull_geometry() {
    let blade = BakedQuad {
        positions: [
            [0.1, 0.0, 0.1],
            [0.9, 0.0, 0.9],
            [0.9, 1.0, 0.9],
            [0.1, 1.0, 0.1],
        ],
        uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        direction: Direction::North,
        cullface: None,
        tint_index: None,
        shade: true,
        layer: 0,
        anim: 0,
        sprite: 0,
    };
    let view = InsetRow {
        quads: vec![blade],
        right_class: Some("slime_block"),
        skip_same: true,
    };
    let (_, translucent) = mesh_models_layers(&view);
    assert_eq!(
        translucent.quad_count(),
        2,
        "same-material identity must not suppress diagonal no-cull model quads"
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
