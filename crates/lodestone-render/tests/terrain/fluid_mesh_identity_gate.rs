//! Byte-identity gate for `mesh_fluids`: **restructuring the fluid mesher must
//! not move a single vertex.**
//!
//! # What it is
//!
//! Twelve fluid scenes built from **real vanilla 26.2 state ids** out of
//! `client.jar`, meshed through the live `mesh_fluids`, and checksummed. The
//! expected checksums live in `tests/support/fluid_mesh_identity.txt`, and they
//! were produced by running this same file against the **pre-refactor**
//! implementation (`4e0ffdf2`, before the padded grid existed). So the expected
//! value originates outside the code under test in the only sense available for
//! a refactor: it is the output of the implementation being replaced.
//!
//! This path was restructured for cost. Fluid rendering also has
//! deliberate deviations from vanilla in it — boundary side faces landed,
//! and a handful of known divergences from vanilla's fluid renderer are still
//! open — so a
//! cost change that *improved* the output would be a defect here, not a bonus.
//! This gate cannot tell "more correct" from "different"; that is the point.
//!
//! # Why checksums rather than the vertex bytes
//!
//! The twelve scenes together mesh to hundreds of kilobytes of `ModelVertex`,
//! which is not a fixture worth committing. Each mesh instead carries its
//! vertex count, index count, and an **FNV-1a/64** of the exact `bytemuck` byte
//! image of both arrays — plus a fourth hash, `gh`, over the vertices with
//! their UVs zeroed. That fourth one is what separates "the fluid mesher
//! changed" from "the block atlas repacked", which this gate does not control
//! and which rewrites every UV in the corpus at once; see [`digest`].
//! FNV-1a is hand-rolled here on purpose:
//! `std::hash::DefaultHasher` is explicitly not stable across Rust releases, so
//! a committed golden keyed on it would rot silently at the next toolchain
//! bump. `fnv1a_matches_the_published_vectors` pins the implementation against
//! the spec's own test vectors.
//!
//! # Controls (both executed)
//!
//! * **Off-by-one padding.** Every scene is meshed a second time through
//!   [`ClampedToSection`], which answers every probe outside `0..16` as air —
//!   the exact failure mode a mis-sized or mis-indexed padded grid produces.
//!   Every scene that has anything at its boundary must come out *different*
//!   from the golden. Without this the gate could not distinguish "the grid is
//!   right" from "the scenes never look past the section edge".
//! * **The checksum notices a single byte.** `fnv1a` over a buffer and over the
//!   same buffer with one bit flipped must differ.
//!
//! # The awkward categories, and why each is here
//!
//! A section of solid stone has zero fluid cells and can prove nothing; an
//! all-water section cannot exercise a neighbour-height edge case. Each scene
//! below carries a `#[doc]` note naming the structure it exists to exercise,
//! and [`Scene::assert_precondition`] asserts that structure is *actually
//! present* — the "world" species of vacuous test is the one that cannot be
//! read off the source.
//!
//! `#[ignore]`d and fail-closed (a missing jar is an environment failure, never
//! a silent skip):
//!
//! ```text
//! cargo test -p lodestone-render --test fluid_mesh_identity_gate -- --ignored --nocapture
//! # to regenerate after a deliberate, reviewed output change:
//! LODESTONE_REGEN=1 cargo test -p lodestone-render --test fluid_mesh_identity_gate -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;

use lodestone_assets::{ResourceManager, ZipSource};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockStateRegistry, Identifier};
use lodestone_render::block_models::{FluidCell, FluidKind, FluidSprites};
use lodestone_render::fluid_grid::FluidNeighborCell;
use lodestone_render::models::{
    FluidMeshes, FluidSectionView, ModelMesh, ModelVertex, mesh_fluids,
};
use lodestone_render::{BlockModels, blocks_json_registry};

#[path = "../gate_harness/mod.rs"]
mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

/// Where the expected checksums live, relative to the crate root.
const GOLDEN: &str = "tests/support/fluid_mesh_identity.txt";

fn state_id(raw: u32) -> StateId {
    StateId::new(raw).expect("state id from the canonical blocks report")
}

// ---------------------------------------------------------------------------
// FNV-1a/64 — a deterministic checksum that survives toolchain bumps
// ---------------------------------------------------------------------------

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

#[test]
fn fnv1a_matches_the_published_vectors() {
    // FNV-1a/64's own test vectors — an expectation from outside this file.
    assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
}

/// Control: the checksum must notice one flipped bit, or every "byte-identical"
/// claim below is vacuous.
#[test]
fn fnv1a_notices_a_single_flipped_bit() {
    let mut buf = vec![0u8; 4096];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    let before = fnv1a(&buf);
    buf[2048] ^= 0x01;
    let after = fnv1a(&buf);
    assert_ne!(
        before, after,
        "FNV-1a returned the same digest for buffers differing in one bit, so \
         every mesh-identity assertion in this file would be vacuous"
    );
}

// ---------------------------------------------------------------------------
// The scenes
// ---------------------------------------------------------------------------

/// The block states a scene draws from, resolved once from the real registry.
#[derive(Clone, Copy)]
struct Palette {
    air: u32,
    water_source: u32,
    /// `water[level=1..=7]`, indexed `0..7`, so `own_height` sweeps `7/9..1/9`.
    water_flowing: [u32; 7],
    lava_source: u32,
    stone: u32,
    grass: u32,
    glass: u32,
    dirt_path: u32,
    waterlogged_slab: u32,
    /// `stone_brick_stairs[facing=north, half=bottom, shape=straight,
    /// waterlogged=true]`. A straight bottom stair covers exactly **one** of its
    /// four sides over the whole square and leaves the other three partially
    /// covered — the case `isFaceOccludedBySelf` is silent about by
    /// construction. See [`Scene::WaterloggedStairBesideAir`].
    waterlogged_stair: u32,
}

/// What structure a scene exists to exercise. Named so a failure says which
/// category moved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scene {
    /// Every cell water, padding included. **Emits nothing** — every face is
    /// culled against the same fluid — which is the point: it is the
    /// fully-submerged interior that dominates an ocean column's *cost* while
    /// contributing no geometry, and the arm a "skip empty output" shortcut
    /// would break.
    WaterOnly,
    /// Water column on a stone floor with water in the side padding: also
    /// zero-output, but with a real occluder below, so the `down` face is culled
    /// by `occludes` rather than by `same`.
    OceanFloor,
    /// Water to `y < 8`, air above, and **nothing solid inside the section** —
    /// the *water-only section* category: its opaque mesh is empty, so it never
    /// reaches `sections_drawn` (`frame.rs:480`), while it still issues a water
    /// draw at `frame.rs:720`. `DESIGN.md` §12.120's 189-of-195 gap. Also the
    /// only scene with real top surfaces and `should_render_backward_up_face`.
    Surface,
    /// Water everywhere with a `6³` air pocket at the section centre: interior
    /// faces in all six directions at once, mid-section rather than at a
    /// boundary, plus the rim geometry `should_render_backward_up_face` exists
    /// for. The richest fixture here, and the one a padding bug cannot fake.
    SubmergedCave,
    /// `water[level]` varying with `x`, air above: sweeps `own_height` across
    /// its whole `1/9..=8/9` range and drives a non-zero flow vector.
    FlowingSlope,
    /// Water pond walled in real `grass_block`: the shoreline case, where a
    /// side face must be culled by a full opaque cube.
    GrassShore,
    /// The same pond walled in `glass`: `overlay_at` is true, so side faces
    /// take the `water_overlay` sprite and lose their back copy — one of the
    /// known divergences from vanilla, and the only scene that exercises the
    /// overlay bits.
    GlassShore,
    /// The same pond walled in `dirt_path`: a partial, height-reduced occluder,
    /// so `partial_occluder_y_range_at` decides the cull. This is the one probe
    /// deliberately left off the padded grid, so it must be exercised.
    PathBank,
    /// Lava in `x < 8`, water in `x >= 8`, stone floor: two fluid kinds in one
    /// section, which is what proves the per-kind sprite memoisation resolves
    /// both and that a lava cell never picks up a water tint.
    LavaAndWater,
    /// Waterlogged slabs interleaved with water: a cell that is both a solid
    /// model and a fluid source.
    ///
    /// **Blind to the self-occlusion test on its own**, which is why the scene
    /// below exists: it surrounds every slab with water on all six sides, so the
    /// neighbour rule (`same`) already culls every face the self test could, and
    /// this scene's digest is byte-identical to [`Surface`](Self::Surface)'s.
    Waterlogged,
    /// A row of **waterlogged stairs standing in air**, with water only inside
    /// their own cells — the input the corpus was missing twice over.
    ///
    /// Two properties no other scene here has:
    ///
    /// * The waterlogged block's faces are against **air**, not water, so
    ///   `same` does not cull them and `isFaceOccludedBySelf` is the rule that
    ///   actually decides. `Waterlogged` above cannot reach that code at all.
    /// * Three of the stair's four sides are **partially** covered — one is the
    ///   bottom half only, two are L-shaped, and just one is flush. Vanilla's
    ///   self test fixes its probe height at `1.0` and asks whether the block's
    ///   own boundary layer covers the *whole* square, so it correctly answers
    ///   "not occluded" for those three and the water's faces are emitted in
    ///   full. Vanilla emits them too; the coplanar overlap is a compositing
    ///   problem, not a culling one, and `fluid_coplanar_depth_gate.rs` is where
    ///   that is measured. This scene's job is to pin the *emission* so a future
    ///   partial-coverage cull rule cannot quietly delete water vanilla draws.
    ///
    /// A waterlogged slab's side would not do: it is fully covered below and
    /// fully open above, so the same output falls out of a whole-square test and
    /// a per-height one alike.
    WaterloggedStairBesideAir,
    /// Solid stone throughout: **zero** fluid cells, so `any_fluid()` is false
    /// and the mesher must emit nothing. The negative end of the range.
    Dry,
    /// One water cell at `(0, 0, 0)` in stone: the fluid bounding box is a
    /// single cell at the low corner, so the shell fill is at its smallest and
    /// a sign error in the padding index shows up immediately.
    PuddleLowCorner,
    /// The same, at `(15, 15, 15)`: the opposite corner, which a `+1`/`-1`
    /// padding mix-up breaks in the other direction.
    PuddleHighCorner,
}

const SCENES: [Scene; 14] = [
    Scene::WaterOnly,
    Scene::OceanFloor,
    Scene::Surface,
    Scene::SubmergedCave,
    Scene::FlowingSlope,
    Scene::GrassShore,
    Scene::GlassShore,
    Scene::PathBank,
    Scene::LavaAndWater,
    Scene::Waterlogged,
    Scene::WaterloggedStairBesideAir,
    Scene::Dry,
    Scene::PuddleLowCorner,
    Scene::PuddleHighCorner,
];

impl Scene {
    fn name(self) -> &'static str {
        match self {
            Self::WaterOnly => "water_only",
            Self::OceanFloor => "ocean_floor",
            Self::Surface => "surface",
            Self::SubmergedCave => "submerged_cave",
            Self::FlowingSlope => "flowing_slope",
            Self::GrassShore => "grass_shore",
            Self::GlassShore => "glass_shore",
            Self::PathBank => "path_bank",
            Self::LavaAndWater => "lava_and_water",
            Self::Waterlogged => "waterlogged",
            Self::WaterloggedStairBesideAir => "waterlogged_stair_beside_air",
            Self::Dry => "dry",
            Self::PuddleLowCorner => "puddle_low_corner",
            Self::PuddleHighCorner => "puddle_high_corner",
        }
    }

    /// The state id at padded `(x, y, z)`. Coordinates outside `0..16` are the
    /// neighbouring sections, and every scene answers them deliberately —
    /// that is the half a clamped view gets wrong.
    #[allow(clippy::match_same_arms)]
    fn state_at(self, p: Palette, x: i32, y: i32, z: i32) -> u32 {
        let pond = |x: i32, z: i32| (4..12).contains(&x) && (4..12).contains(&z);
        match self {
            Self::WaterOnly => p.water_source,
            Self::OceanFloor => {
                if y < 0 {
                    p.stone
                } else {
                    p.water_source
                }
            }
            Self::Surface => {
                if y < 0 {
                    p.stone
                } else if y < 8 {
                    p.water_source
                } else {
                    p.air
                }
            }
            Self::SubmergedCave => {
                let pocket = (5..11).contains(&x) && (5..11).contains(&y) && (5..11).contains(&z);
                if pocket {
                    p.air
                } else {
                    p.water_source
                }
            }
            Self::FlowingSlope => {
                if y < 0 {
                    p.stone
                } else if y < 4 {
                    // level 0 (source) at x <= 0, then level 1..=7 marching
                    // east, so a single row spans every `own_height`.
                    let level = x.clamp(0, 7);
                    if level == 0 {
                        p.water_source
                    } else {
                        p.water_flowing[(level - 1) as usize]
                    }
                } else {
                    p.air
                }
            }
            Self::GrassShore => {
                if y >= 8 {
                    p.air
                } else if y < 0 || !pond(x, z) {
                    p.grass
                } else {
                    p.water_source
                }
            }
            Self::GlassShore => {
                if y >= 8 {
                    p.air
                } else if y < 0 {
                    p.stone
                } else if pond(x, z) {
                    p.water_source
                } else {
                    p.glass
                }
            }
            Self::PathBank => {
                if y >= 8 {
                    p.air
                } else if y < 0 {
                    p.stone
                } else if pond(x, z) {
                    p.water_source
                } else {
                    p.dirt_path
                }
            }
            Self::LavaAndWater => {
                if y < 0 {
                    p.stone
                } else if y >= 8 {
                    p.air
                } else if x < 8 {
                    p.lava_source
                } else {
                    p.water_source
                }
            }
            Self::Waterlogged => {
                if y < 0 {
                    p.stone
                } else if y >= 8 {
                    p.air
                } else if (x + z) % 3 == 0 {
                    p.waterlogged_slab
                } else {
                    p.water_source
                }
            }
            // Waterlogged stairs standing on a stone bank in **air**, at the
            // north edge of a pond that lies outside the section.
            //
            // Air around each stair is the point: every face of a waterlogged
            // cell then faces air, `same` culls nothing, and the self test is the
            // only rule that can. Spaced two apart in both axes so no stair is
            // another stair's neighbour — an adjacent waterlogged cell is a
            // same-fluid neighbour and `same` would cull the shared faces,
            // putting the scene straight back into `Waterlogged`'s blind spot.
            //
            // The pond at `z < 0` is what makes the scene padding-sensitive: it
            // culls the `z == 0` row's north faces through `same`, so the clamped
            // control (which answers out-of-section probes as air) un-culls them
            // and produces a different mesh. Without it this scene's geometry
            // would not depend on its neighbourhood at all and the off-by-one
            // padding control could not separate.
            Self::WaterloggedStairBesideAir => {
                if y < 0 {
                    p.stone
                } else if y == 0 && z < 0 {
                    p.water_source
                } else if y == 0 && z >= 0 && x.rem_euclid(2) == 0 && z.rem_euclid(2) == 0 {
                    p.waterlogged_stair
                } else {
                    p.air
                }
            }
            Self::Dry => p.stone,
            Self::PuddleLowCorner => {
                if (x, y, z) == (0, 0, 0) {
                    p.water_source
                } else {
                    p.stone
                }
            }
            Self::PuddleHighCorner => {
                if (x, y, z) == (15, 15, 15) {
                    p.water_source
                } else {
                    p.stone
                }
            }
        }
    }
}

/// A [`FluidSectionView`] over one [`Scene`] and a real [`BlockModels`].
struct SceneView<'a> {
    models: &'a BlockModels,
    palette: Palette,
    scene: Scene,
    /// When true, every probe outside `0..16` answers as air — the control.
    clamp_to_section: bool,
    /// When true, `cell_at` resolves the state **once** and derives all three
    /// answers from it — the shape `SnapshotFluidView` uses in production, and
    /// therefore the shape the grid is actually filled from in the live client.
    /// The default trait composition (three independent probes) is the other
    /// arm. Both must reach the same golden, or the production override is a
    /// silent divergence no other gate here would see (the *world* species:
    /// a gate that only exercises the default `cell_at` proves nothing about
    /// the override the shell installs).
    share_cell_at: bool,
}

impl SceneView<'_> {
    fn state(&self, x: i32, y: i32, z: i32) -> u32 {
        let n = 16;
        if self.clamp_to_section
            && !((0..n).contains(&x) && (0..n).contains(&y) && (0..n).contains(&z))
        {
            return self.palette.air;
        }
        self.scene.state_at(self.palette, x, y, z)
    }
}

impl FluidSectionView for SceneView<'_> {
    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        self.models.fluid(state_id(self.state(x, y, z)))
    }

    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        self.models.occludes(state_id(self.state(x, y, z)))
    }

    fn overlay_at(&self, x: i32, y: i32, z: i32) -> bool {
        self.models.fluid_overlay(state_id(self.state(x, y, z)))
    }

    /// Mirrors the shell mesher's own override, which is the implementation
    /// production resolves to. **The trait default returns
    /// `SelfOcclusion::default()` — every face un-occluded — so leaving it
    /// unoverridden here meant this whole corpus meshed with vanilla's
    /// `shouldRenderFace` self test switched off**, and no digest in the golden
    /// could have noticed it appearing or disappearing. That is the *world*
    /// species of vacuous test: nothing about the source reads wrong, the flaw is
    /// which implementation the transport resolves to.
    ///
    /// The `Solid`-layer gate is the `canOcclude` half of vanilla's
    /// `occlusionShape = canOcclude ? getOcclusionShape(state) : Shapes.empty()`
    /// and is not optional: without it every waterlogged leaves block (full-cube
    /// outline, `noOcclusion()`) would cull its own water away entirely.
    fn self_occlusion_at(&self, x: i32, y: i32, z: i32) -> lodestone_assets::fluid::SelfOcclusion {
        use lodestone_assets::fluid::SelfOcclusion;

        let id = self.state(x, y, z);
        if self.models.layer(state_id(id)) != lodestone_render::RenderLayer::Solid {
            return SelfOcclusion::default();
        }
        let Some(state) = lodestone_data::block_states::StateId::new(id) else {
            return SelfOcclusion::default();
        };
        let boxes = lodestone_data::outline_shapes::outline_boxes(state);
        lodestone_assets::fluid::self_occlusion(boxes)
    }

    fn partial_occluder_y_range_at(&self, x: i32, y: i32, z: i32) -> Option<(f32, f32)> {
        let state = lodestone_data::block_states::StateId::new(self.state(x, y, z))?;
        let boxes = lodestone_data::outline_shapes::outline_boxes(state);
        lodestone_assets::fluid::full_footprint_y_range(boxes)
    }

    fn fluid_sprites(&self, kind: FluidKind) -> FluidSprites {
        self.models.fluid_sprites(kind)
    }

    fn cell_at(&self, x: i32, y: i32, z: i32) -> FluidNeighborCell {
        if !self.share_cell_at {
            // The trait default, written out: three independent probes.
            return FluidNeighborCell {
                fluid: self.fluid_at(x, y, z),
                occludes: self.occludes_at(x, y, z),
                overlay: self.overlay_at(x, y, z),
            };
        }
        // Production's shape: one state resolution, three derivations.
        let id = self.state(x, y, z);
        FluidNeighborCell {
            fluid: self.models.fluid(state_id(id)),
            occludes: self.models.occludes(state_id(id)),
            overlay: self.models.fluid_overlay(state_id(id)),
        }
    }
}

// ---------------------------------------------------------------------------
// Digesting a mesh
// ---------------------------------------------------------------------------

/// Vertex count, index count, FNV-1a of both byte images, and a fourth hash
/// over the vertices with their **UVs zeroed**.
///
/// The fourth one exists because this gate does not own every input to the
/// number it pins. A fluid vertex's `uv` is a rect out of the stitched block
/// atlas, and the atlas's packing is decided elsewhere — the stitcher's gutter,
/// its mip depth, which sprites happen to be in it. Any of those legitimately
/// moving repacks the atlas and rewrites **every** UV in the corpus, which
/// lands here as "the fluid mesher's output changed" in all eleven water
/// scenes at once. That happened: adding the atlas gutter (matching vanilla's
/// own stitcher padding, `1 << mipLevel`) turned this gate red for a week, and
/// telling it apart from a real mesher regression took reverting the padding
/// call in `block_models.rs` to see the golden reproduce exactly.
///
/// `gh` removes that ambiguity at a glance. An atlas repack moves `vh` and
/// leaves `v`, `i`, `ih` and `gh` alone; anything the fluid mesher itself did
/// moves `gh` too. Zeroing rather than skipping keeps the field's bytes in the
/// digest's stride, so this stays a hash of the whole vertex minus one
/// well-named part rather than of a re-serialised subset.
fn digest(mesh: &ModelMesh) -> String {
    let vb: &[u8] = bytemuck::cast_slice(&mesh.vertices);
    let ib: &[u8] = bytemuck::cast_slice(&mesh.indices);
    let uvless: Vec<ModelVertex> = mesh
        .vertices
        .iter()
        .map(|v| {
            let mut w = *v;
            w.uv = [0.0, 0.0];
            w
        })
        .collect();
    let gb: &[u8] = bytemuck::cast_slice(&uvless);
    format!(
        "v={} i={} vh={:016x} ih={:016x} gh={:016x}",
        mesh.vertices.len(),
        mesh.indices.len(),
        fnv1a(vb),
        fnv1a(ib),
        fnv1a(gb)
    )
}

fn digest_meshes(m: &FluidMeshes) -> String {
    format!("water[{}] lava[{}]", digest(&m.water), digest(&m.lava))
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

fn find_state(reg: &dyn BlockStateRegistry, block: &str, want: &[(&str, &str)]) -> u32 {
    let ident: Identifier = block.parse().expect("a valid identifier");
    let wanted: BTreeMap<&str, &str> = want.iter().copied().collect();
    (0..reg.state_count())
        .find(|&id| {
            let Some(state) = reg.resolve(id) else {
                return false;
            };
            if *state.block != ident {
                return false;
            }
            wanted
                .iter()
                .all(|(k, v)| state.properties.get(*k).map(String::as_str) == Some(*v))
        })
        .unwrap_or_else(|| panic!("no state for {block} with {want:?} in the 26.2 registry"))
}

/// The structural facts each scene must actually contain. A scene that stopped
/// containing its own subject would still hash consistently, and the gate would
/// pass while proving nothing.
fn assert_precondition(scene: Scene, view: &SceneView<'_>, meshes: &FluidMeshes) {
    let mut water = 0usize;
    let mut lava = 0usize;
    let mut occluding = 0usize;
    let mut occluding_in_section = 0usize;
    let mut overlay = 0usize;
    let mut partial = 0usize;
    let mut heights = std::collections::BTreeSet::new();
    for y in -1..=16 {
        for z in -1..=16 {
            for x in -1..=16 {
                let in_section = (0..16).contains(&x)
                    && (0..16).contains(&y)
                    && (0..16).contains(&z);
                if in_section && view.occludes_at(x, y, z) {
                    occluding_in_section += 1;
                }
                match view.fluid_at(x, y, z) {
                    Some(f) if f.kind == FluidKind::Water => {
                        water += 1;
                        heights.insert(f.state.amount);
                    }
                    Some(_) => lava += 1,
                    None => {}
                }
                if view.occludes_at(x, y, z) {
                    occluding += 1;
                }
                if view.overlay_at(x, y, z) {
                    overlay += 1;
                }
                if view.partial_occluder_y_range_at(x, y, z).is_some() {
                    partial += 1;
                }
            }
        }
    }
    let n = scene.name();
    let quads = meshes.water.vertices.len() / 4 + meshes.lava.vertices.len() / 4;
    println!(
        "  {n:<18} water {water:5}  lava {lava:5}  occluding {occluding:5} ({occluding_in_section} \
         in-section)  overlay {overlay:5}  partial {partial:5}  quads {quads:5}  amounts {heights:?}"
    );
    match scene {
        Scene::Dry => {
            assert_eq!(water + lava, 0, "{n} must contain no fluid at all");
            assert!(occluding > 0, "{n} must be genuinely solid");
            assert_eq!(quads, 0, "{n} must emit nothing");
        }
        Scene::WaterOnly => {
            assert_eq!(occluding, 0, "{n} must contain no occluder anywhere");
            assert_eq!(water, 18 * 18 * 18, "{n} must be water in every padded cell");
            assert_eq!(
                quads, 0,
                "{n} is the fully-submerged arm: every face is culled against the same fluid, \
                 so it must emit nothing. Non-zero here means face culling changed."
            );
        }
        Scene::Surface => {
            // The water-only *section* category: no solid inside the section, so
            // the opaque mesh is empty and `sections_drawn` never counts it —
            // while the water mesh is not empty and a water draw is still
            // issued. Both halves asserted, because either alone is satisfiable
            // by the wrong scene.
            assert_eq!(
                occluding_in_section, 0,
                "{n} must contain no solid inside the section, or it is not the water-only \
                 category the render path's `mesh: None` arm covers"
            );
            assert!(
                !meshes.water.vertices.is_empty(),
                "{n} must still emit a water draw despite having no opaque geometry"
            );
        }
        Scene::SubmergedCave => {
            assert!(
                quads >= 6,
                "{n} must emit at least one face per direction of the air pocket; got {quads}"
            );
            assert_eq!(
                occluding, 0,
                "{n} must cull only against the same fluid, never against a solid — that is \
                 what makes its faces mid-section rather than boundary artefacts"
            );
        }
        Scene::LavaAndWater => {
            assert!(lava > 0 && water > 0, "{n} must contain both fluids");
        }
        Scene::FlowingSlope => {
            assert!(
                heights.len() >= 8,
                "{n} must sweep every fluid amount; saw {heights:?}"
            );
        }
        Scene::GlassShore => {
            assert!(overlay > 0, "{n} must have overlay-class neighbours");
        }
        Scene::PathBank => {
            assert!(
                partial > 0,
                "{n} must have partial-footprint occluders, or the one probe left off the \
                 grid is never exercised"
            );
        }
        Scene::PuddleLowCorner | Scene::PuddleHighCorner => {
            assert_eq!(water, 1, "{n} must be exactly one fluid cell");
            assert!(occluding > 0, "{n} must be embedded in solid");
        }
        Scene::WaterloggedStairBesideAir => {
            assert!(water > 0, "{n} must contain water");
            // The subject: a waterlogged cell whose four side neighbours are all
            // air, so `same` culls nothing there and the self test is what
            // decides. `Waterlogged` has none of these.
            let mut stairs_in_air = 0usize;
            for z in 0..16 {
                for x in 0..16 {
                    if view.fluid_at(x, 0, z).is_none() {
                        continue;
                    }
                    let air_sides = [(-1, 0), (1, 0), (0, -1), (0, 1)]
                        .iter()
                        .filter(|(dx, dz)| {
                            view.fluid_at(x + dx, 0, z + dz).is_none()
                                && !view.occludes_at(x + dx, 0, z + dz)
                        })
                        .count();
                    if air_sides == 4 {
                        stairs_in_air += 1;
                    }
                }
            }
            assert!(
                stairs_in_air > 0,
                "{n} must contain at least one waterlogged cell with air on all \
                 four sides, or the self-occlusion test is never the deciding \
                 rule and this scene is `waterlogged` again"
            );
            // And the discriminating geometry. `stairs.json` builds a straight
            // stair from a bottom slab `[0,0,0]..[16,8,16]` and a step
            // `[8,8,0]..[16,16,16]`, so of its four sides **exactly one** — the
            // one the step is flush against — is covered over the whole square,
            // and the other three are not: the opposite side is the bottom half
            // only (Matthew's "front of a stair"), and the two remaining sides are
            // L-shaped. The bottom face is fully covered by the slab.
            //
            // Asserting that 1-of-4 signature rather than naming a compass
            // direction is deliberate twice over: it does not depend on this
            // project's stair rotation convention, and it separates all three
            // hypotheses — a whole-square test gives exactly 1, a detector stuck
            // at false gives 0, and one that treats partial coverage as occluding
            // gives 4.
            let occ = view.self_occlusion_at(0, 0, 0);
            let covered_sides = [occ.north, occ.south, occ.east, occ.west]
                .iter()
                .filter(|c| **c)
                .count();
            println!("  {n:<28} stair self-occlusion {occ:?}");
            assert!(
                occ.down,
                "{n}: the stair's bottom face is fully covered by its own slab, so \
                 the self test must report it occluded; got {occ:?}"
            );
            assert_eq!(
                covered_sides, 1,
                "{n}: a straight bottom stair covers exactly one of its four sides \
                 over the whole square; {covered_sides} came back covered ({occ:?}). \
                 0 means the self test is inert here and this scene proves nothing; \
                 4 means a partial-coverage cull rule crept in and is deleting \
                 water vanilla draws."
            );
        }
        Scene::OceanFloor | Scene::GrassShore | Scene::Waterlogged => {
            assert!(water > 0, "{n} must contain water");
            assert!(occluding > 0, "{n} must contain an occluder");
        }
    }
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn mesh_fluids_is_byte_identical_to_the_pre_refactor_implementation() {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");

    let palette = Palette {
        air: find_state(&registry, "minecraft:air", &[]),
        water_source: find_state(&registry, "minecraft:water", &[("level", "0")]),
        water_flowing: std::array::from_fn(|i| {
            find_state(&registry, "minecraft:water", &[("level", &(i + 1).to_string())])
        }),
        lava_source: find_state(&registry, "minecraft:lava", &[("level", "0")]),
        stone: find_state(&registry, "minecraft:stone", &[]),
        grass: find_state(&registry, "minecraft:grass_block", &[("snowy", "false")]),
        glass: find_state(&registry, "minecraft:glass", &[]),
        dirt_path: find_state(&registry, "minecraft:dirt_path", &[]),
        waterlogged_slab: find_state(
            &registry,
            "minecraft:stone_slab",
            &[("type", "bottom"), ("waterlogged", "true")],
        ),
        waterlogged_stair: find_state(
            &registry,
            "minecraft:stone_brick_stairs",
            &[
                ("facing", "north"),
                ("half", "bottom"),
                ("shape", "straight"),
                ("waterlogged", "true"),
            ],
        ),
    };

    println!("--- scene preconditions (padded 18^3 census) ---");
    let mut lines = String::new();
    let mut controls_that_differed = 0usize;
    let mut control_report = String::new();
    for scene in SCENES {
        let view = SceneView {
            models: &models,
            palette,
            scene,
            clamp_to_section: false,
            share_cell_at: false,
        };
        let meshed = mesh_fluids(&view);
        assert_precondition(scene, &view, &meshed);
        let subject = digest_meshes(&meshed);
        writeln!(lines, "{} {subject}", scene.name()).expect("write to a String");

        // The same scene through production's `cell_at` shape (one state read,
        // three derivations) must give the identical mesh.
        let shared = SceneView {
            models: &models,
            palette,
            scene,
            clamp_to_section: false,
            share_cell_at: true,
        };
        assert_eq!(
            digest_meshes(&mesh_fluids(&shared)),
            subject,
            "scene `{}`: the shared-`cell_at` override (what `SnapshotFluidView` installs) \
             disagrees with the default three-probe composition",
            scene.name()
        );

        // Control: the same scene through a view that answers every
        // out-of-section probe as air. A padded grid that is too small, or
        // indexed with the wrong sign, produces exactly this.
        let clamped = SceneView {
            models: &models,
            palette,
            scene,
            clamp_to_section: true,
            share_cell_at: false,
        };
        let control = digest_meshes(&mesh_fluids(&clamped));
        if control == subject {
            writeln!(control_report, "  {} SAME as subject", scene.name())
                .expect("write to a String");
        } else {
            controls_that_differed += 1;
            writeln!(control_report, "  {} differs (good)", scene.name())
                .expect("write to a String");
        }
    }

    println!("--- off-by-one padding control ---\n{control_report}");
    // Predicted 10 of 12 on the reasoning that `water_only` is uniform and so
    // could not separate. **That prediction was wrong, and the control is what
    // said so**: clamping turns `water_only`'s out-of-section neighbours into
    // air, which un-culls every face on the section's outer shell — a uniform
    // scene is still sensitive to its padding. Only `dry` cannot separate,
    // because it emits nothing either way. So the predicted count is 11 of 12,
    // and it is asserted exactly rather than as "some differed".
    assert_eq!(
        controls_that_differed,
        SCENES.len() - 1,
        "the clamped-padding control must change the mesh for every scene except `dry` \
         (which emits nothing either way), so the byte-identity assertion below is known \
         to be sensitive to the neighbourhood the grid caches. Report:\n{control_report}"
    );

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    let header = "\
# Expected `mesh_fluids` output digests, one line per scene:
#   <scene> water[v=<verts> i=<indices> vh=<fnv1a of vertex bytes> ih=<... index
#           bytes> gh=<... vertex bytes with the UVs zeroed>] lava[...]
#
# GENERATED by `fluid_mesh_identity_gate.rs`. DO NOT EDIT BY HAND.
# A diff here means the fluid mesher's OUTPUT changed. For a cost-only change
# that is a defect, not a bonus -- fluid rendering carries deliberate deviations
# from vanilla, so 'different' and 'more correct' are indistinguishable from
# here. Regenerate only for a reviewed, intended output change:
#   LODESTONE_REGEN=1 cargo test -p lodestone-render --test fluid_mesh_identity_gate \\
#     -- --ignored --nocapture
#
# READ `gh` FIRST when this goes red. A fluid vertex's UV is a rect out of the
# stitched block atlas, whose packing this gate does not control, so an atlas
# repack rewrites every UV in the corpus and reads exactly like a mesher
# regression. If `v`, `i`, `ih` and `gh` all held and only `vh` moved, the fluid
# mesher is untouched and something repacked the atlas -- regenerate, and say so
# below. If `gh` moved, the mesher really did change.
#
# Provenance, newest first. Every regeneration belongs on this list with the
# scenes it moved, because the file's whole value is that a line changing is
# surprising:
#
#  4. The block atlas grew its gutter -- `AtlasBuilder::with_padding(1 <<
#     BLOCK_ATLAS_MIP_LEVELS)`, matching vanilla's own stitcher padding, so a minified
#     Linear tap at a sprite's own edge stops inside that sprite. That repacked
#     every sprite and so every baked UV. ALL ELEVEN water scenes moved their
#     vertex hash and NOTHING else moved: identical vertex counts, identical
#     index hashes, and `dry` (no fluid at all) untouched. Confirmed rather than
#     inferred -- dropping the `with_padding` call in `build_complete_atlas`
#     reproduced this file byte-for-byte across all fourteen scenes, then the
#     file was restored from an md5-checked backup. `gh` was added in the same
#     change so the next atlas repack is a one-glance diagnosis instead, and it
#     has its own executed control: re-running with the padding dropped moves
#     `vh` on all eleven water lines (back to the exact digests this file held
#     before the regeneration) and leaves every `gh` byte-identical.
#
#  3. `waterlogged_stair_beside_air` added, and `SceneView` grew a real
#     `self_occlusion_at`. **NO existing scene moved** -- the only change is one
#     new line. Two blind spots closed at once, both of the *world* species:
#     (a) `SceneView` never overrode `self_occlusion_at`, whose trait default
#     reports every face un-occluded, so this whole corpus had been meshing with
#     vanilla's own-block self-occlusion test switched OFF; and (b) even with it on,
#     the scene named `waterlogged` could not exercise it, because it surrounds
#     its slabs with water on all six sides and the neighbour rule culls
#     everything the self test would -- which is why its digest is byte-identical
#     to `surface`'s, and still is. What was missing was a waterlogged block beside
#     AIR, and one whose faces are only *partially* covered by its own geometry.
#     A straight bottom stair is that input: measured, its self-occlusion is
#     `down: true, north: true` and the other three sides false, so exactly one of
#     four sides is covered over the whole square. Turning the self test on moved
#     nothing precisely because (b) held for every pre-existing scene.
#  2. `corner_heights` -- vanilla's fluid-mesh corner branch, which
#     forces all four corners to 1.0 when the fluid's own rendered height already
#     is (the same fluid directly above). `mesh_fluids` was averaging instead, so a
#     falling column of water rendered 10/12 of a block tall and had a visible gap.
#     ONE scene moved: `path_bank`, vertex hash only -- identical vertex count and
#     identical index hash, so the same quads at corrected heights. Its bank is
#     `dirt_path`, a *partial* occluder, so the rim water's horizontal neighbours
#     read as air-like (0.0) and dragged the average down; the 12 other scenes are
#     byte-identical because their rim neighbours are either full columns (1.0) or
#     full occluders (-1.0, excluded), both of which already yielded 1.0 through
#     `corner_height`'s own arms. That is also why no scene here caught the bug:
#     none of them contains an isolated column with air beside it. See
#     `fluid_falling_column_gate.rs`, which is that missing input.
#  1. The padded `FluidGrid` (sha 4e0ffdf2) -- cost-only, no scene moved. This file
#     was originally generated from the implementation immediately before it.
";
    let want = format!("{header}{lines}");
    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(&path, &want).expect("write the golden file");
        println!("regenerated {}", path.display());
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}); generate it with LODESTONE_REGEN=1",
            path.display()
        )
    });
    if have != want {
        let mut diff = String::new();
        for (h, w) in have.lines().zip(want.lines()) {
            if h != w {
                writeln!(diff, "  committed: {h}\n  measured : {w}").expect("write to a String");
            }
        }
        panic!(
            "`mesh_fluids` output changed against {}:\n{diff}\nRead `gh` before reading \
             anything else: it is the vertex hash with the UVs zeroed, so a line where only `vh` \
             moved is an ATLAS REPACK (the fluid mesher is untouched -- regenerate and record it \
             in the golden's provenance list), and a line where `gh` moved is a real change in \
             `mesh_fluids`.\nfull measured output:\n{want}",
            path.display()
        );
    }
    println!("all {} scenes byte-identical to the golden.", SCENES.len());
}
