//! End-to-end gate for the reported water bug: **a vertically falling column of
//! water must not have a gap in it.**
//!
//! The report, on live terrain: "when water flows straight down, there's a gap in
//! it (like a triangle) that's missing the texture on each block of water."
//!
//! # The rule that was missing, and where
//!
//! Vanilla `FluidRenderer.tesselate` does not average corner heights
//! unconditionally. It first computes the fluid's own rendered height and
//! short-circuits:
//!
//! ```text
//! float heightSelf = this.getHeight(level, type, pos, blockState, fluidState);
//! if (heightSelf >= 1.0F) {
//!    heightNorthEast = heightNorthWest = heightSouthEast = heightSouthWest = 1.0F;
//! } else {
//!    ... calculateAverageHeight per corner ...
//! }
//! ```
//!
//! `heightSelf` reaches `1.0` only via `FlowingFluid.getHeight`'s `hasSameAbove`,
//! since `WaterFluid.Source.getAmount` is `8` and so even a source's own height is
//! `8/9`. Every cell of a falling column has water above it, so vanilla draws the
//! whole column at full height and it is seamless.
//!
//! `mesh_fluids` had `FlowingFluid.getHeight` right — `neighbor_height_in` really
//! does return `1.0` for a cell with the same fluid above — and then averaged
//! anyway, because the branch above it had no counterpart in this codebase at all.
//! Against open air that average is `10 / 12 = 0.8333`: the full self cell at
//! weight 10 and each air edge at weight 1. Every block in the column rendered a
//! sixth short.
//!
//! # Why it was a *triangle*
//!
//! The shortfall is not uniform. `add_weighted_height` drops a solid neighbour
//! (`-1.0`) from the average entirely, so a corner facing a wall divided by 11
//! (`0.909`) while a corner facing air divided by 12 (`0.8333`). Two different
//! heights on one quad is a sloped surface, and triangulating a sloped quad is
//! what reads as a wedge.
//!
//! # Why nothing caught it
//!
//! `crates/lodestone-assets/tests/fluid.rs` unit-tests `render_height` (the
//! `hasSameAbove` short-circuit) and `corner_height` (the average) and both were
//! correct. The rule that composes them was unrepresented, so there was no symbol
//! to point a test at — which is why the fix introduces
//! `lodestone_assets::fluid::corner_heights` rather than inlining a conditional.
//!
//! And the scene corpus could not see it either.
//! `crates/lodestone-shell/tests/water_seam_convergence.rs` fills two whole
//! columns with water, and `fluid_mesh_identity_gate.rs`'s `surface`/`water_only`
//! scenes are full slabs — in all of them every horizontal neighbour is *also* a
//! full column, so `corner_height`'s own `edge_a >= 1.0` arm returned `1.0` and the
//! old code was right. The flaw was in the input data, not in any assertion:
//! `CLAUDE.md`'s **world** species, the one that cannot be found by reading the
//! test. This file is the missing input — an isolated column with air beside it.
//!
//! # Hermetic on purpose
//!
//! Unlike `fluid_shoreline_gate.rs` this needs no `client.jar`. That gate's
//! load-bearing fact was what `BlockModels` reports about a real `grass_block`, so
//! a synthetic view would have been a closed loop. Here the load-bearing fact is
//! pure geometry — corner heights as a function of fluid amount and what is
//! directly above — and the outside expectation is vanilla's `tesselate` record
//! quoted above. The sprites are placeholders because no assertion reads a UV.

use lodestone_assets::fluid::{FluidState, SpriteUv};
use lodestone_render::ModelMesh;
use lodestone_render::block_models::{FluidCell, FluidKind, FluidSprites};
use lodestone_render::models::{FluidSectionView, mesh_fluids};

/// Two isolated water columns in open air, one continuing past the top of the
/// section and one ending inside it.
///
/// * **`FALLING_XZ`** — water at every `y` the padded grid can reach, so every
///   in-section cell has water above and takes vanilla's short-circuit.
/// * **`CAPPED_XZ`** — water only up to [`CAPPED_TOP`], so its top cell has *air*
///   above and must still average down. This is the control, and its premise is
///   true by construction rather than by assertion: the cell genuinely has nothing
///   above it.
///
/// Both columns are one cell wide with air on all four sides, which is the input
/// the existing corpus lacks.
struct TwoColumns;

const FALLING_XZ: (i32, i32) = (8, 8);
const CAPPED_XZ: (i32, i32) = (3, 3);
/// The highest `y` the capped column occupies. Well inside the section so the cell
/// above it is a normal in-grid air cell.
const CAPPED_TOP: i32 = 9;

impl TwoColumns {
    fn is_water(x: i32, y: i32, z: i32) -> bool {
        if (x, z) == FALLING_XZ {
            return true;
        }
        (x, z) == CAPPED_XZ && y <= CAPPED_TOP
    }
}

impl FluidSectionView for TwoColumns {
    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        Self::is_water(x, y, z).then(|| FluidCell {
            kind: FluidKind::Water,
            // Vanilla's falling water is `getFlowing(8, true)`: amount 8, falling.
            // The amount is deliberately not 9 — nothing in the game has 9, which
            // is why `heightSelf` can only reach 1.0 through `hasSameAbove`.
            state: FluidState::new(8, true),
        })
    }

    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        // Open air everywhere that is not water: no solid neighbour anywhere, so
        // every `neighbor_height` beside a column is `0.0` (averaged in) rather
        // than `-1.0` (excluded). That is the configuration that yields 10/12.
        false
    }

    fn fluid_sprites(&self, _kind: FluidKind) -> FluidSprites {
        let uv = SpriteUv {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
            anim: 0,
        };
        FluidSprites {
            still: uv,
            flow: uv,
            overlay: None,
        }
    }
}

/// The highest vertex `y` of any **side** (vertical) quad whose cell is the column
/// at `(x, z)` and whose base sits at block `y`.
///
/// Side quads are the observable: a falling column emits no top face at all (the
/// cell above is the same fluid, so vanilla's `renderUp` is false), so the corner
/// heights show up only as the top edge of the four vertical quads. That top edge
/// is exactly where the reported gap was.
fn side_top_at(mesh: &ModelMesh, x: i32, y: i32, z: i32) -> Option<f32> {
    let (fx, fy, fz) = (x as f32, y as f32, z as f32);
    let mut best: Option<f32> = None;
    for quad in mesh.vertices.chunks(4) {
        let (lo, hi) = quad
            .iter()
            .map(|v| v.position[1])
            .fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(v), h.max(v)));
        // Vertical quad (not a level top or bottom face) belonging to this cell.
        if hi - lo < 1e-4 {
            continue;
        }
        let in_cell = quad.iter().all(|v| {
            (v.position[0] - fx) > -0.01
                && (v.position[0] - fx) < 1.01
                && (v.position[2] - fz) > -0.01
                && (v.position[2] - fz) < 1.01
        }) && lo >= fy - 0.01
            && lo < fy + 0.5;
        if in_cell {
            best = Some(best.map_or(hi, |b: f32| b.max(hi)));
        }
    }
    best
}

/// `bake_fluid` carries vanilla's `offs = 0.001F` anti-z-fight inset, so a
/// full-height side quad's top edge lands within a thousandth of the block top
/// rather than exactly on it. Everything asserted below is bracketed against this
/// rather than against `0.0`, and the shortfall being discriminated is `1/6` —
/// more than a hundred times larger, so the two cannot be confused.
const INSET: f32 = 0.01;

/// The two hypotheses, in block units, at the falling column's cells.
const RIGHT: f32 = 1.0;
/// `corner_height(1.0, 0.0, 0.0, 0.0)` — the full self cell at weight 10 against
/// two air edges at weight 1 each.
const WRONG: f32 = 10.0 / 12.0;

#[test]
fn a_falling_water_column_has_no_gap_between_its_blocks() {
    let mesh = mesh_fluids(&TwoColumns).water;
    assert!(
        mesh.quad_count() > 0,
        "the scene produced no fluid geometry at all — nothing below asserts anything"
    );

    // Every cell of the falling column, not just one: the reported symptom was a
    // gap on *each* block, so a fix that only settled the topmost cell has to fail
    // here.
    let (x, z) = FALLING_XZ;
    let mut checked = 0;
    for y in 0..16 {
        let Some(top) = side_top_at(&mesh, x, y, z) else {
            panic!("no side quad found for the falling column cell at y={y}");
        };
        let height = top - y as f32;
        assert!(
            (height - RIGHT).abs() < INSET,
            "falling column cell y={y}: side face reaches {height} above its block \
             base, expected {RIGHT} (vanilla's heightSelf >= 1.0 short-circuit). \
             {WRONG} is the averaging hypothesis this gate exists to refute, and \
             the two differ by {}.",
            RIGHT - WRONG
        );
        checked += 1;
    }
    assert_eq!(checked, 16, "every cell of the column was measured");
}

/// The control, executed, and it must land on the *averaged* value.
///
/// A column that ends inside the section has air above its top cell, so
/// `heightSelf` is `8/9` and vanilla really does average it down. If this cell also
/// came out at `1.0` the fix would be "fluids are always full", which would flatten
/// every shoreline in the game — and no assertion in the test above would notice,
/// because a falling column is full either way.
///
/// Predicted from arithmetic: self `8/9` weighs 10 (it is `>= 0.8`), each of the
/// two air edges weighs 1, so the corner is `(8/9 * 10) / 12 = 0.7407`.
#[test]
fn a_column_that_ends_below_the_top_still_averages_its_last_cell_down() {
    let mesh = mesh_fluids(&TwoColumns).water;
    let (x, z) = CAPPED_XZ;

    let top = side_top_at(&mesh, x, CAPPED_TOP, z)
        .expect("the capped column's top cell must emit side quads");
    let height = top - CAPPED_TOP as f32;
    let expected = (8.0 / 9.0) * 10.0 / 12.0;
    assert!(
        (height - expected).abs() < INSET,
        "the capped column's top cell measured {height}, expected {expected}: with \
         air above it there is no short-circuit and the average must still apply"
    );
    assert!(
        height < RIGHT - 0.1,
        "and it must be visibly short of full — this is the assertion that would \
         fail if the fix had flattened every fluid cell instead of only those with \
         the same fluid above"
    );

    // The cell *below* the cap has water above it, so it takes the short-circuit
    // even though it belongs to the same column. That contrast inside one column is
    // the tightest form of the rule: the difference is what is above the cell, not
    // which column it is in.
    let below = side_top_at(&mesh, x, CAPPED_TOP - 1, z)
        .expect("the cell below the cap must emit side quads");
    let below_height = below - (CAPPED_TOP - 1) as f32;
    assert!(
        (below_height - RIGHT).abs() < INSET,
        "one cell lower measured {below_height}, expected {RIGHT}: it has water \
         directly above it"
    );
}
