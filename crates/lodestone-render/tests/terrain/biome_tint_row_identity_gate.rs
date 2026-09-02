//! Byte-identity gate for the **sliding** biome blend: replacing
//! [`resolve_blended_tint`] with [`BlendedTintCursor`] must not move a single
//! colour byte.
//!
//! # What it is
//!
//! Vanilla's biome tint is a radius-2 box — 25 `Colormaps::resolve` samples per
//! tinted quad — and after issue #542's three commits it was still ~63% of
//! `mesh_fluids`'s per-cell cost (`DESIGN.md` §12.124). Adjacent cells share 20
//! of their 25 columns, so a sliding per-channel sum costs 5 samples per step
//! instead of 25. `lodestone_assets::tint::BlendRowCursor` does the arithmetic
//! (and `lodestone-assets/tests/tint.rs` proves it bit-identical to
//! `blend_box`); this gate covers the part above it — that the *tint* wrapper
//! keys its window on everything `Colormaps::resolve` actually reads, and that
//! the meshers' real call order does not defeat it.
//!
//! **Bit-exactness is the whole point.** Vanilla is not colour-managed: tint and
//! shade multiply in gamma space, so a blend that reassociates its sum or divides
//! early shifts colours by a byte or two — invisible in a screenshot and wrong.
//! Every assertion here is `assert_eq!` on bytes. None has a tolerance.
//!
//! # The reference arm, and why it is legitimate
//!
//! [`resolve_blended_tint`] itself. This is a refactor, not a parity fix, so the
//! correct expected output *is* the old output; there is no JVM oracle to want.
//! Both arms run in the same process on the same fixture, which is stronger than
//! a committed golden — the reference cannot rot, and it stays exercised.
//!
//! # The three fixtures, and the species each avoids
//!
//! * A **four-way biome junction** (one biome per `(sign x, sign z)` quadrant).
//!   A single-biome fixture is the `world` species: every one of the 25 samples
//!   returns the same entry, so any window arithmetic at all passes. It also
//!   happens to be the worst case for `NamedBiomeTint`'s four-slot memo.
//! * A **256×256 gradient colormap**, decoded through the real
//!   `Colormap::from_image`. A 1×1 stand-in samples one pixel whatever the
//!   climate, which would make `temperature`/`downfall` — half of what a grass
//!   blend reads — silently irrelevant.
//! * The real `mesh_fluids`/`mesh_models` loops, so the `y → z → x` order the
//!   cursor exploits is the real one rather than one this file chose.
//!
//! # Controls (all executed, none merely described)
//!
//! * [`the_sweep_would_catch_a_miskeyed_cursor`] rebuilds the wrapper *wrongly*
//!   out of the same public `BlendRowCursor` — keying on `(x, z)` only and never
//!   invalidating when the kind or `y` changes — and requires the sweep's own
//!   comparison to reject it. Without this, "the cursor agrees" could mean the
//!   fixture never changes kind or `y`.
//! * [`the_mesh_digest_sees_the_blended_tint`] re-meshes with the biome junction
//!   shifted one column and requires the digest to change. Without it, equal
//!   digests could mean the mesh carries no tint at all.
//! * [`the_junction_fixture_is_a_real_four_way_boundary`] asserts the fixture's
//!   own premise: four distinct biomes, and a blend at the junction that equals
//!   none of the four pure colours.

use lodestone_assets::tint::{
    Colormap, Colormaps, Rgb, TintKind, biome_effects, blend_box,
};
use lodestone_assets::fluid::{FluidState, SpriteUv};
use lodestone_assets::{BakedQuad, Direction, Image};
use lodestone_model::BlockPos;
use lodestone_render::biome_tint::{
    BLEND_RADIUS, BlendedTintCursor, NamedBiomeTint, resolve_blended_tint, rgb_to_bytes,
};
use lodestone_render::{
    FluidCell, FluidKind, FluidSectionView, FluidSprites, ModelMesh, ModelSectionView,
    biome_tint_kind_for_slot, biome_tint_slot, mesh_fluids, mesh_models,
};

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// One biome per `(sign x, sign z)` quadrant. Four distinct `BiomeEffects` with
/// four distinct climates *and* a mix of override/no-override and
/// modifier/no-modifier, so all four `BiomeTint` questions differ across the
/// junction rather than only `water_color`.
const QUADRANTS: [&str; 4] = [
    "minecraft:swamp",         // foliage + dry-foliage override, Swamp modifier
    "minecraft:plains",        // no override at all: a pure colormap sample
    "minecraft:badlands",      // grass + foliage override
    "minecraft:cherry_grove",  // grass + foliage override, a distinct water colour
];

/// The biome at a position, with the junction at `(0, 0)` shifted by `shift` in
/// `x` — the control's one degree of freedom.
fn quadrant_biome(pos: BlockPos, shift: i32) -> Option<&'static str> {
    let i = usize::from(pos.x - shift >= 0) | (usize::from(pos.z >= 0) << 1);
    Some(QUADRANTS[i])
}

/// A 256×256 gradient colormap decoded through the real
/// [`Colormap::from_image`], so a blend's `temperature`/`downfall` inputs
/// actually change the sampled pixel. A uniform stand-in would make half of what
/// a grass blend reads inert without any assertion noticing.
fn gradient_colormap(seed: u32) -> Colormap {
    let mut rgba = vec![0u8; 256 * 256 * 4];
    for y in 0..256usize {
        for x in 0..256usize {
            let i = (y * 256 + x) * 4;
            rgba[i] = (x as u32 ^ seed) as u8;
            rgba[i + 1] = (y as u32).wrapping_mul(3).wrapping_add(seed) as u8;
            rgba[i + 2] = ((x + y) as u32).wrapping_mul(7) as u8;
            rgba[i + 3] = 255;
        }
    }
    let img = Image {
        width: 256,
        height: 256,
        rgba,
    };
    Colormap::from_image(&img, 0x00AA_BBCC).expect("256x256 gradient colormap")
}

fn gradient_colormaps() -> Colormaps {
    Colormaps {
        grass: gradient_colormap(0x11),
        foliage: gradient_colormap(0x55),
        dry_foliage: gradient_colormap(0x99),
    }
}

/// The four kinds a blend applies to. Interleaved deliberately in the sweep: the
/// cursor keys its window on the kind, and a fixture that finishes one kind
/// before starting the next would never test that.
const KINDS: [TintKind; 4] = [
    TintKind::Grass,
    TintKind::Foliage,
    TintKind::DryFoliage,
    TintKind::Water,
];

// ---------------------------------------------------------------------------
// The premise of the fixture itself
// ---------------------------------------------------------------------------

#[test]
fn the_junction_fixture_is_a_real_four_way_boundary() {
    let colormaps = gradient_colormaps();
    // Four distinct biomes, checked against `biome_effects` directly rather than
    // through anything under test.
    let names: std::collections::BTreeSet<&str> = QUADRANTS.iter().copied().collect();
    assert_eq!(names.len(), 4, "the four quadrants must name four biomes");
    for name in QUADRANTS {
        assert!(
            biome_effects(name).is_some(),
            "{name} is not in the vanilla table, so this quadrant renders the plains fallback \
             and the junction has fewer than four distinct sides"
        );
    }
    // For every kind, a blend centred on the junction must differ from all four
    // pure-biome blends. If it equalled one, that quadrant's colour would be
    // dominating and the "boundary" would be cosmetic.
    for kind in KINDS {
        let junction = plain(kind, &colormaps, 0, 64, 0, 0).expect("a blended kind");
        let pures: Vec<Rgb> = (0..4)
            .map(|i| {
                let one = NamedBiomeTint::new(move |_pos: BlockPos| Some(QUADRANTS[i]));
                resolve_blended_tint(kind, &colormaps, &one, BLEND_RADIUS, 0, 64, 0)
                    .expect("a blended kind")
            })
            .collect();
        assert_eq!(pures.len(), 4);
        for (i, pure) in pures.iter().enumerate() {
            assert_ne!(
                junction, *pure,
                "{kind:?}: the blend at the junction ({junction:#08X}) equals pure {} \
                 ({pure:#08X}), so this fixture is not exercising a boundary for that kind",
                QUADRANTS[i]
            );
        }
        // ...and at least two of the four pure colours must themselves differ, or
        // "four biomes" is four names for one colour.
        let distinct: std::collections::BTreeSet<Rgb> = pures.iter().copied().collect();
        assert!(
            distinct.len() >= 3,
            "{kind:?}: the four quadrant biomes produce only {} distinct colours \
             ({pures:#08X?}). The gradient colormap or the biome choice is too flat for a \
             blend error to show up",
            distinct.len()
        );
    }
}

/// `resolve_blended_tint` at `(x, y, z)` with the junction shifted by `shift`.
fn plain(
    kind: TintKind,
    colormaps: &Colormaps,
    x: i32,
    y: i32,
    z: i32,
    shift: i32,
) -> Option<Rgb> {
    let biome = NamedBiomeTint::new(move |pos: BlockPos| quadrant_biome(pos, shift));
    resolve_blended_tint(kind, colormaps, &biome, BLEND_RADIUS, x, y, z)
}

// ---------------------------------------------------------------------------
// The sweep: every position, every kind, interleaved
// ---------------------------------------------------------------------------

/// The `(kind, y, z, x)` visiting order a mesher produces, plus deliberate
/// hostility: `x` runs backwards on odd rows and the kind rotates per cell, so
/// the cursor's key is changed under it as often as it is reused.
fn sweep_positions() -> Vec<(TintKind, i32, i32, i32)> {
    let mut out = Vec::new();
    for (n, y) in [60i32, 61, 62].into_iter().enumerate() {
        for z in -2i32..18 {
            let xs: Vec<i32> = if z % 2 == 0 {
                (-2..18).collect()
            } else {
                (-2..18).rev().collect()
            };
            for (m, x) in xs.into_iter().enumerate() {
                out.push((KINDS[(n + m + z.unsigned_abs() as usize) % 4], y, z, x));
            }
        }
    }
    out
}

#[test]
fn blended_tint_cursor_is_byte_identical_to_resolve_blended_tint() {
    let colormaps = gradient_colormaps();
    let positions = sweep_positions();
    assert!(
        positions.len() > 1000,
        "the sweep visits only {} positions",
        positions.len()
    );
    let mut cursor = BlendedTintCursor::new(BLEND_RADIUS);
    assert_eq!(cursor.radius(), BLEND_RADIUS);
    let biome = NamedBiomeTint::new(|pos: BlockPos| quadrant_biome(pos, 0));
    let mut kind_changes = 0usize;
    let mut previous: Option<(TintKind, i32)> = None;
    for &(kind, y, z, x) in &positions {
        let want = plain(kind, &colormaps, x, y, z, 0).expect("a blended kind");
        let got = cursor
            .resolve(kind, &colormaps, &biome, x, y, z)
            .expect("a blended kind");
        assert_eq!(
            got, want,
            "{kind:?} at ({x}, {y}, {z}): cursor {got:#08X}, resolve_blended_tint {want:#08X}. \
             These must agree to the bit — tint multiplies in gamma space, so a byte of drift \
             is a real colour error that no screenshot shows"
        );
        if previous != Some((kind, y)) {
            kind_changes += 1;
            previous = Some((kind, y));
        }
    }
    assert!(
        kind_changes > positions.len() / 8,
        "only {kind_changes} of {} steps changed the (kind, y) key. The sweep is not \
         exercising the invalidation path, which is the half of this type that
         `BlendRowCursor` cannot get wrong on its own",
        positions.len()
    );
    // The kinds that are not blended must stay `None` on both paths.
    for kind in [
        TintKind::None,
        TintKind::Constant(0x123456),
        TintKind::RedstonePower(7),
    ] {
        assert_eq!(cursor.resolve(kind, &colormaps, &biome, 3, 60, 4), None);
        assert_eq!(
            resolve_blended_tint(kind, &colormaps, &biome, BLEND_RADIUS, 3, 60, 4),
            None
        );
    }
}

/// Control for the sweep. A wrapper keyed on `(x, z)` alone — exactly what you
/// get by forgetting that `Colormaps::resolve` also reads the kind and the `y` —
/// must be **rejected** by the same comparison the sweep makes. Built from the
/// same public `BlendRowCursor`, so this is the real failure mode and not a
/// straw man.
#[test]
fn the_sweep_would_catch_a_miskeyed_cursor() {
    let colormaps = gradient_colormaps();
    let biome = NamedBiomeTint::new(|pos: BlockPos| quadrant_biome(pos, 0));
    let mut miskeyed = lodestone_assets::tint::BlendRowCursor::new(BLEND_RADIUS);
    let mut divergences = 0usize;
    let mut compared = 0usize;
    for &(kind, y, z, x) in &sweep_positions() {
        // No `invalidate` on a kind or `y` change: the bug.
        let got = miskeyed.blend(x, z, |sx, sz| {
            colormaps
                .resolve(kind, &biome, BlockPos::new(sx, y, sz))
                .unwrap_or(0)
        });
        let want = plain(kind, &colormaps, x, y, z, 0).expect("a blended kind");
        compared += 1;
        if got != want {
            divergences += 1;
        }
    }
    assert!(
        divergences > compared / 20,
        "a cursor that never invalidates on a (kind, y) change diverged from \
         resolve_blended_tint in only {divergences} of {compared} positions. The sweep's \
         comparison is therefore not sensitive to mis-keying, and \
         blended_tint_cursor_is_byte_identical_to_resolve_blended_tint proves less than it \
         appears to"
    );
    // The other half of the control: the row cursor is *not* simply always wrong.
    // Fed a single kind and a single `y`, it must agree everywhere — otherwise the
    // divergences above would be evidence of nothing in particular.
    let mut honest = lodestone_assets::tint::BlendRowCursor::new(BLEND_RADIUS);
    for x in -2..18 {
        let got = honest.blend(x, 5, |sx, sz| {
            colormaps
                .resolve(TintKind::Grass, &biome, BlockPos::new(sx, 60, sz))
                .unwrap_or(0)
        });
        let want = blend_box(x, 5, BLEND_RADIUS, |sx, sz| {
            colormaps
                .resolve(TintKind::Grass, &biome, BlockPos::new(sx, 60, sz))
                .unwrap_or(0)
        });
        assert_eq!(got, want, "one kind, one y: the row cursor must agree at x={x}");
    }
}

// ---------------------------------------------------------------------------
// Through the real mesher loops
// ---------------------------------------------------------------------------

/// FNV-1a/64, hand-rolled for the same reason `fluid_mesh_identity_gate.rs`
/// does it: `DefaultHasher` is documented as unstable across Rust releases.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn digest(mesh: &ModelMesh) -> String {
    let vb: &[u8] = bytemuck::cast_slice(&mesh.vertices);
    let ib: &[u8] = bytemuck::cast_slice(&mesh.indices);
    format!(
        "v={} i={} vh={:016x} ih={:016x}",
        mesh.vertices.len(),
        mesh.indices.len(),
        fnv1a(vb),
        fnv1a(ib)
    )
}

#[test]
fn fnv1a_notices_a_single_flipped_bit() {
    let a = [1u8, 2, 3, 4, 5, 6, 7, 8];
    let mut b = a;
    b[5] ^= 0x01;
    assert_ne!(fnv1a(&a), fnv1a(&b), "the digest must see one bit");
    assert_eq!(
        fnv1a(b"a"),
        0xaf63dc4c8601ec8c,
        "FNV-1a/64 of \"a\" is a published test vector; a mismatch means this is not FNV-1a \
         and the digests below are not comparable with any other gate's"
    );
}

/// How a view answers a tint question: through the cursor (production, after this
/// change), through `resolve_blended_tint` (production, before it), or through
/// the cursor with the biome junction shifted (the digest-sensitivity control).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TintPath {
    Cursor { shift: i32 },
    Plain { shift: i32 },
}

/// A view over one water-and-air section, whose tint answers come from the real
/// `Colormaps` + `NamedBiomeTint` + the four-way biome junction.
///
/// The `Cursor` arm holds its cursor in a `RefCell` and calls it exactly as
/// `crates/lodestone-shell/src/mesher.rs`'s `SnapshotFluidView`/
/// `SnapshotModelView` do — that shape is what makes driving the real
/// `mesh_fluids`/`mesh_models` loops here worth anything.
struct JunctionView {
    colormaps: Colormaps,
    path: TintPath,
    cursor: std::cell::RefCell<BlendedTintCursor>,
    /// The one grass-tinted quad every solid cell emits, for the model arm.
    quad: Vec<BakedQuad>,
}

impl JunctionView {
    fn new(path: TintPath) -> Self {
        Self {
            colormaps: gradient_colormaps(),
            path,
            cursor: std::cell::RefCell::new(BlendedTintCursor::new(BLEND_RADIUS)),
            quad: vec![grass_quad()],
        }
    }

    fn tint(&self, kind: TintKind, x: i32, y: i32, z: i32) -> Option<[u8; 3]> {
        let shift = match self.path {
            TintPath::Cursor { shift } | TintPath::Plain { shift } => shift,
        };
        let biome = NamedBiomeTint::new(move |pos: BlockPos| quadrant_biome(pos, shift));
        let rgb = match self.path {
            TintPath::Cursor { .. } => self
                .cursor
                .borrow_mut()
                .resolve(kind, &self.colormaps, &biome, x, y, z)?,
            TintPath::Plain { .. } => {
                resolve_blended_tint(kind, &self.colormaps, &biome, BLEND_RADIUS, x, y, z)?
            }
        };
        Some(rgb_to_bytes(rgb))
    }
}

/// A single grass-tinted up-facing quad. Degenerate in shape on purpose: this
/// gate is about which colour bytes land on which vertex, not about geometry,
/// which `model_ao_corner_gate.rs` and friends already cover.
///
/// `tint_index` carries the **slot** byte, not a model-JSON tint index:
/// `mesh_models` passes `quad.tint_index as u8` straight to `biome_tint_at`
/// (`models.rs:818`), and production's view turns it back into a [`TintKind`]
/// with `biome_tint_kind_for_slot`. Taken from [`biome_tint_slot`] rather than
/// written as a constant so this cannot drift from the mapping production uses.
fn grass_quad() -> BakedQuad {
    BakedQuad {
        positions: [[0.5, 1.0, 0.5]; 4],
        uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        direction: Direction::Up,
        cullface: None,
        tint_index: Some(i32::from(
            biome_tint_slot(TintKind::Grass).expect("Grass has a biome tint slot"),
        )),
        shade: true,
        layer: 0,
        anim: 0,
        sprite: 0,
    }
}

impl ModelSectionView for JunctionView {
    fn quads_at(&self, _x: usize, y: usize, _z: usize) -> &[BakedQuad] {
        // A single solid layer, so every emitted quad is a *surface* quad with a
        // live tint rather than one culled against its neighbour.
        if y == 4 { &self.quad } else { &[] }
    }

    fn occludes_at(&self, _x: i32, y: i32, _z: i32) -> bool {
        y == 4
    }

    /// The same two lines `crates/lodestone-shell/src/mesher.rs`'s
    /// `SnapshotModelView::biome_tint_at` runs: slot to kind through the real
    /// inverse, then one tint resolution.
    fn biome_tint_at(&self, x: usize, y: usize, z: usize, slot: u8) -> Option<[u8; 3]> {
        let kind = biome_tint_kind_for_slot(slot)?;
        self.tint(kind, x as i32, y as i32, z as i32)
    }
}

impl FluidSectionView for JunctionView {
    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        // Water up to y = 7 inside the section, air above: the `surface` shape
        // #542 recorded as the one that actually emits quads (an all-water
        // section culls every face and hashes the empty buffer).
        let inside = (0..16).contains(&x) && (0..16).contains(&z);
        if inside && (0..8).contains(&y) {
            Some(FluidCell {
                kind: FluidKind::Water,
                state: FluidState::source(),
            })
        } else {
            None
        }
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
        self.tint(TintKind::Water, x, y, z)
    }
}

#[test]
fn the_real_mesher_loops_produce_byte_identical_meshes() {
    let cursor_water = mesh_fluids(&JunctionView::new(TintPath::Cursor { shift: 0 }));
    let plain_water = mesh_fluids(&JunctionView::new(TintPath::Plain { shift: 0 }));
    let cursor_models = mesh_models(&JunctionView::new(TintPath::Cursor { shift: 0 }));
    let plain_models = mesh_models(&JunctionView::new(TintPath::Plain { shift: 0 }));

    // Preconditions, before any comparison: an empty mesh is byte-identical to
    // another empty mesh, which is the vacuity #542 hit with `water_only` and
    // `ocean_floor`.
    assert!(
        plain_water.water.quad_count() > 0,
        "the fluid fixture meshed no water quads, so its digest is the empty buffer"
    );
    assert!(
        plain_models.quad_count() > 0,
        "the model fixture meshed no quads, so its digest is the empty buffer"
    );
    // And the tint must actually have reached the vertices, or both arms are
    // comparing the inert `[0, 0, 0, 0]` sentinel.
    let live: usize = plain_models
        .vertices
        .iter()
        .filter(|v| v.tint_rgb_override[3] == 255)
        .count();
    assert!(
        live > 0,
        "no model vertex carries a live tint override, so this gate would pass with the tint \
         path removed entirely"
    );

    assert_eq!(
        digest(&cursor_water.water),
        digest(&plain_water.water),
        "mesh_fluids water: the cursor changed the byte image"
    );
    assert_eq!(
        digest(&cursor_water.lava),
        digest(&plain_water.lava),
        "mesh_fluids lava"
    );
    assert_eq!(
        digest(&cursor_models),
        digest(&plain_models),
        "mesh_models: the cursor changed the byte image"
    );
}

/// Control for the test above: equal digests only mean something if a *changed*
/// tint would give unequal ones. Shift the biome junction one column and require
/// both digests to move.
#[test]
fn the_mesh_digest_sees_the_blended_tint() {
    let base_water = mesh_fluids(&JunctionView::new(TintPath::Cursor { shift: 0 }));
    let moved_water = mesh_fluids(&JunctionView::new(TintPath::Cursor { shift: 1 }));
    let base_models = mesh_models(&JunctionView::new(TintPath::Cursor { shift: 0 }));
    let moved_models = mesh_models(&JunctionView::new(TintPath::Cursor { shift: 1 }));
    assert_ne!(
        digest(&base_water.water),
        digest(&moved_water.water),
        "moving the biome junction one column left the water byte image unchanged, so the \
         digest does not carry the blended tint and the identity assertions above are vacuous"
    );
    assert_ne!(
        digest(&base_models),
        digest(&moved_models),
        "moving the biome junction one column left the model byte image unchanged, so the \
         digest does not carry the blended tint"
    );
    // Geometry must be *identical* across the shift — only the colour moved. If
    // the counts changed, the control would be proving something about culling.
    assert_eq!(
        base_water.water.vertices.len(),
        moved_water.water.vertices.len(),
        "shifting a biome must not change how much geometry is emitted"
    );
    assert_eq!(
        base_models.indices.len(),
        moved_models.indices.len(),
        "shifting a biome must not change how much geometry is emitted"
    );
}
