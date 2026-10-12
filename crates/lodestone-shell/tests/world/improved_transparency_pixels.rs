//! Improved transparency: red stained glass behind a water wall composites the
//! same whichever order the draws happen, which the sorted blended pipeline
//! cannot do.
//!
//! # The scene
//!
//! The camera looks along +z across a stone floor at a backdrop. A wall of water
//! stands 8 blocks ahead and, behind it, a wall of red stained glass 16 blocks
//! ahead; both cover the same block of pixels, which therefore see water in front
//! of glass in front of the backdrop. The water wall shows two surfaces along a
//! ray (its near face and the far face's inward copy), the glass wall one (its
//! back face is culled). The sorted pipeline draws all water before all
//! translucent blocks, so it paints the glass **over** the water (glass is
//! behind it, but water writes no depth), and it draws the two water faces in
//! mesh order, not depth order. The reference rule is depth order.
//!
//! # The expectation
//!
//! Every surface the scene draws is rendered **alone** on the sorted pipeline,
//! over two backdrops (stone and white concrete). A surface blends as
//! `f(S) = P + T*S`, so its frame over the stone backdrop `S` gives `f(S)` and
//! the two backdrops give its transmittance `T` per pixel (a least-squares fit
//! over the three channels). Any stack of surfaces then composites, in any
//! order, as the fold `out = f_i(S) + T_i * (out_behind - S)` from the back. That
//! needs no model of the sprite's texel pattern, the tint, the lighting or the
//! fog: they are all inside the measured `f(S)`.
//!
//! Water's `T` is not measured per pixel but predicted: the built atlas holds
//! one alpha for every texel of the water sprite, and `T = 1 - alpha`. The
//! measured median is then checked against that prediction, which validates
//! the instrument on a surface of known alpha. The atlas is the bundled
//! resource pack's, whose water alpha is 171/255, not the 180/255 of the
//! unmodified client archive.
//!
//! # Three predictions, one per claim
//!
//! * **Draw order** (`fold` in the sorted pipeline's order) must match the
//!   sorted frames. That validates the fold: it reproduces a blend sequence
//!   whose answer is known.
//! * **Depth order** (`fold` front to back) is what the order-independent
//!   frames must match, to a per-pixel tolerance built from three sources, each
//!   computed from the representation's own arithmetic and none from the run:
//!
//!   1. *Depth binning.* The coefficients hold the absorbance averaged over
//!      eight depth bins, and a fragment reads the series back by interpolating
//!      the average of its own bin and the one before it. A surface partway
//!      into a bin therefore sees only part of the absorbance in front of it
//!      from the surfaces sharing the bin. [`oit_representation`] evaluates
//!      that read-back exactly, and the tolerance carries the distance between
//!      its composite and the depth-order composite.
//!   2. *Half floats.* Each coefficient is a running sum in `Rgba16Float`; one
//!      add rounds to at most 2^-11 of the sum, which bounds the absorbance
//!      read back and so each surface's weight.
//!   3. *Quantisation.* The reference frames and the output are 8 bit.
//!
//!   [`oit_representation`] also gives a prediction of what the GPU computes
//!   without the half-float and quantisation terms, which the frame must match
//!   far more tightly.
//! * **Both orders differ**: the sorted two-layer frame must disagree with the
//!   depth-order prediction on nearly every checked pixel, or the scene cannot
//!   tell the orders apart.
//!
//! One layer alone (water; glass) is held to the same standard on both paths,
//! and the whole frame outside what the layer touches must be untouched.
//!
//! `#[ignore]`d and fail-closed: no adapter, no `FLOAT32_BLENDABLE` and no
//! vanilla `client.jar` are failures, never skips.
//!
//! ```text
//! cargo test -p lodestone-shell --test world improved_transparency -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, RenderStats, SKY_COLOR, ThirdPersonBodyState};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_fluids, mesh_snapshot_models_layers, snapshot_section,
    snapshot_visibility,
};
use lodestone::resources::BlockResources;
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, entity_anim::AnimInput, fog::FogSettings,
};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

const W: u32 = 640;
const H: u32 = 480;
const RD_CHUNKS: i32 = 4;
const MIN_Y: i32 = 0;
const FLOOR_TOP: i32 = 64;
const SECTION_COUNT: usize = 8;
const EYE: glam::Vec3 = glam::Vec3::new(0.5, FLOOR_TOP as f32 + 1.62, 0.5);
const FADE_COMPLETE_TICK: u64 = 200;

type Wall = ([i32; 3], [i32; 3]);
/// The water wall: blocks `x -6..=-1`, `y 65..=68`, one block thick at `z = 8`.
const WATER: Wall = ([-6, 65, 8], [-1, 68, 8]);
/// The glass wall behind it, wider and taller so it covers everything the water
/// does: blocks `x -10..=-1`, `y 64..=69` at `z = 16`.
const GLASS: Wall = ([-10, 64, 16], [-1, 69, 16]);
/// The backdrop at `z = 30`.
const BACKDROP: Wall = ([-14, 64, 30], [14, 76, 30]);

/// The sorted pipeline's translucent surfaces, by the world `z` of the plane
/// the mesher puts their quads on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Plane {
    WaterNear,
    WaterFar,
    GlassFront,
    GlassBack,
}

impl Plane {
    fn z(self) -> f32 {
        match self {
            Plane::WaterNear => 8.001,
            Plane::WaterFar => 8.999,
            Plane::GlassFront => 16.0,
            Plane::GlassBack => 17.0,
        }
    }
    fn is_water(self) -> bool {
        matches!(self, Plane::WaterNear | Plane::WaterFar)
    }
    /// View-space depth of the plane for the camera, which looks straight along +z.
    fn depth(self) -> f64 {
        f64::from(self.z() - EYE.z)
    }
}

/// How close to a plane's `z` a quad's vertices must sit, in blocks.
const PLANE_EPSILON: f32 = 0.0005;

/// Depth bins in the coefficient series (also its coefficient count).
const DEPTH_BINS: usize = 8;
/// Largest relative rounding error of one `Rgba16Float` store (10 stored
/// significand bits, so half a unit in the last place of 2^-10).
const HALF_EPSILON: f64 = 1.0 / 2048.0;

/// The sorted two-layer frame must miss the depth-order prediction by more than
/// this many levels on nearly every checked pixel. It is far above every
/// tolerance below, so it separates the orders rather than the noise.
const CONTROL_MIN_DEVIATION: f32 = 16.0;
/// Margin kept inside each wall's edges, in blocks, so a silhouette pixel is
/// never classified.
const EDGE_MARGIN: f32 = 0.15;
/// Squared linear distance between the two backdrops a pixel needs for its
/// transmittance to be measurable: where the ray leaves the backdrop wall the
/// two scenes show the same sky or floor and the fit has nothing to fit.
const MIN_BACKDROP_CONTRAST: f64 = 0.04;
/// Pixels beyond the bounding region a layer changes that must still be
/// untouched by the order-independent composite.
const UNTOUCHED_DILATION: i32 = 2;

fn suppress_first_person_arm(state: &mut RenderState) {
    state.set_third_person_body_source(|| {
        Some(ThirdPersonBodyState {
            player_skin: None,
            feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
            body_yaw_deg: 0.0,
            anim: AnimInput::default(),
            scale: 1.0,
            swim_amount: 0.0,
            slim: false,
            equipment: Vec::new(),
            equipment_skin: Vec::new(),
        })
    });
}

fn first_state_named(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the block-state table"))
}

struct Ids {
    air: u32,
    stone: u32,
    white: u32,
    water: u32,
    glass: u32,
}

fn build_world(ids: &Ids, backdrop: u32) -> World {
    let mut world = World::new();
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            let column = ChunkColumn::new(
                MIN_Y,
                SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                ids.air,
                0,
            );
            let mut light = ColumnLight::new(SECTION_COUNT);
            for i in 0..light.light_section_count() {
                *light.sky_mut(i) = lodestone_world::LightData::Uniform(15);
                *light.block_mut(i) = lodestone_world::LightData::Uniform(0);
            }
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
            );
        }
    }
    let extent = RD_CHUNKS * 16;
    assert!(
        world.fill_region([-extent, MIN_Y, -extent], [extent + 15, FLOOR_TOP - 1, extent + 15], ids.stone) > 0,
        "fixture: the floor must write blocks"
    );
    assert!(world.fill_region(BACKDROP.0, BACKDROP.1, backdrop) > 0, "fixture: backdrop");
    assert!(world.fill_region(WATER.0, WATER.1, ids.water) > 0, "fixture: water wall");
    assert!(world.fill_region(GLASS.0, GLASS.1, ids.glass) > 0, "fixture: glass wall");
    world
}

/// The production mesher's output for every section of `world`.
fn mesh_world(world: &World, models: &lodestone_render::BlockModels) -> Vec<(SectionKey, SectionGeometry)> {
    let mut out = Vec::new();
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                let Some(snap) = snapshot_section(world, key, Default::default()) else { continue };
                let (mut opaque, translucent_blocks) = mesh_snapshot_models_layers(&snap, models, true, 2);
                let fluids = mesh_snapshot_fluids(&snap, models);
                opaque.merge(&fluids.lava);
                let visibility = snapshot_visibility(&snap, models);
                out.push((
                    key,
                    SectionGeometry::Model { opaque, water: fluids.water, translucent_blocks, visibility },
                ));
            }
        }
    }
    out
}

/// Which quads of one translucent mesh a derived scene keeps.
#[derive(Clone, Copy)]
enum Keep<'a> {
    Nothing,
    Everything,
    /// Only the quads lying on one of these planes.
    Planes(&'a [Plane]),
}

fn on_plane(mesh: &ModelMesh, quad: &[u32], world_z0: f32, plane: Plane) -> bool {
    quad.iter()
        .all(|&i| (mesh.vertices[i as usize].position[2] + world_z0 - plane.z()).abs() <= PLANE_EPSILON)
}

fn filtered(mesh: &ModelMesh, world_z0: f32, keep: Keep) -> ModelMesh {
    match keep {
        Keep::Nothing => ModelMesh::default(),
        Keep::Everything => mesh.clone(),
        Keep::Planes(planes) => ModelMesh {
            vertices: mesh.vertices.clone(),
            indices: mesh
                .indices
                .chunks_exact(6)
                .filter(|quad| planes.iter().any(|&p| on_plane(mesh, quad, world_z0, p)))
                .flatten()
                .copied()
                .collect(),
        },
    }
}

/// A scene with the opaque terrain of `full` and only the chosen translucent
/// quads, so one surface can be rendered alone.
fn select(
    full: &[(SectionKey, SectionGeometry)],
    water: Keep,
    glass: Keep,
) -> Vec<(SectionKey, SectionGeometry)> {
    full.iter()
        .filter_map(|(key, geometry)| {
            let SectionGeometry::Model { opaque, water: w, translucent_blocks, visibility } = geometry else {
                return None;
            };
            let z0 = key.cz as f32 * 16.0;
            Some((
                *key,
                SectionGeometry::Model {
                    opaque: opaque.clone(),
                    water: filtered(w, z0, water),
                    translucent_blocks: filtered(translucent_blocks, z0, glass),
                    visibility: *visibility,
                },
            ))
        })
        .collect()
}

/// Where each water quad sits and when the sorted pipeline draws it.
///
/// The sorted pipeline draws a section's water in mesh index order, not depth
/// order, and the mesher emits a block's faces together, so which of a pixel's
/// two water faces is drawn last depends on the two blocks the ray crosses.
struct WaterDrawOrder {
    /// `(plane, x range, y range, first index)` per quad, in world blocks.
    quads: Vec<(Plane, [f32; 2], [f32; 2], usize)>,
}

impl WaterDrawOrder {
    fn new(full: &[(SectionKey, SectionGeometry)]) -> Self {
        let mut quads = Vec::new();
        let mut sections = 0;
        for (key, geometry) in full {
            let SectionGeometry::Model { water, .. } = geometry else { continue };
            if water.indices.is_empty() {
                continue;
            }
            sections += 1;
            let origin = key.origin();
            for (index, quad) in water.indices.chunks_exact(6).enumerate() {
                let Some(plane) = [Plane::WaterNear, Plane::WaterFar]
                    .into_iter()
                    .find(|&p| on_plane(water, quad, origin[2] as f32, p))
                else {
                    continue;
                };
                let span = |axis: usize| {
                    let values = quad.iter().map(|&i| water.vertices[i as usize].position[axis] + origin[axis] as f32);
                    [
                        values.clone().fold(f32::MAX, f32::min),
                        values.fold(f32::MIN, f32::max),
                    ]
                };
                quads.push((plane, span(0), span(1), index));
            }
        }
        assert_eq!(sections, 1, "fixture: the water wall is expected in one section");
        Self { quads }
    }

    /// The draw position of the face on `plane` that the ray through the pixel
    /// crosses. A face and its inward copy share a block, so the earlier of the
    /// two stands for the pair; they are emitted back to back.
    fn rank(&self, camera: &Camera, x: u32, y: u32, plane: Plane) -> usize {
        let dir = ray_dir(camera, x as f32 + 0.5, y as f32 + 0.5);
        let hit = camera.position + dir * ((plane.z() - camera.position.z) / dir.z);
        self.quads
            .iter()
            .filter(|(p, xs, ys, _)| {
                *p == plane
                    && (xs[0] - 1e-3..=xs[1] + 1e-3).contains(&hit.x)
                    && (ys[0] - 1e-3..=ys[1] + 1e-3).contains(&hit.y)
            })
            .map(|q| q.3)
            .min()
            .unwrap_or_else(|| panic!("fixture: no water quad on {plane:?} under pixel ({x}, {y}), ray hits {hit:?}"))
    }
}

struct Frame {
    rgba: Vec<u8>,
    stats: RenderStats,
}

#[allow(clippy::too_many_arguments)]
fn render_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    target: &mut HeadlessTarget,
    atlas: &lodestone_render::BlockAtlas,
    geometry: &[(SectionKey, SectionGeometry)],
    camera: &Camera,
    improved: bool,
) -> Frame {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    state.set_fog(FogSettings::for_render_distance(SKY_COLOR, RD_CHUNKS as u32), RD_CHUNKS as u32);
    state.set_improved_transparency(improved);
    for (key, mesh) in geometry {
        state.upload_section(device, queue, *key, mesh);
    }
    state.update_animation(queue, FADE_COMPLETE_TICK);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), camera, None, &[]);
    Frame { rgba: target.read_texels(device, queue), stats }
}

fn ray_dir(camera: &Camera, px: f32, py: f32) -> glam::Vec3 {
    let (sy, cy) = camera.yaw.to_radians().sin_cos();
    let (sp, cp) = camera.pitch.to_radians().sin_cos();
    let right = glam::Vec3::new(-cy, 0.0, -sy);
    let up = glam::Vec3::new(-sy * sp, cp, cy * sp);
    let forward = glam::Vec3::new(-sy * cp, -sp, cy * cp);
    let half_y = (camera.fov_y_degrees.to_radians() * 0.5).tan();
    let half_x = half_y * camera.aspect;
    let ndc_x = 2.0 * px / W as f32 - 1.0;
    let ndc_y = 1.0 - 2.0 * py / H as f32;
    (forward + right * (ndc_x * half_x) + up * (ndc_y * half_y)).normalize()
}

/// A rectangle on a plane of constant `z`, in blocks.
#[derive(Clone, Copy)]
struct Patch {
    z: f32,
    x: (f32, f32),
    y: (f32, f32),
}

impl Patch {
    /// The front face of `wall` (inclusive block coordinates, one thick in `z`),
    /// inside its edges by [`EDGE_MARGIN`].
    fn front_of(wall: Wall) -> Self {
        Self::at(wall, wall.0[2] as f32, 1.0)
    }
    /// The same wall at plane `z`, its top edge `top` blocks into the top row.
    fn at(wall: Wall, z: f32, top: f32) -> Self {
        Self {
            z,
            x: (wall.0[0] as f32 + EDGE_MARGIN, wall.1[0] as f32 + 1.0 - EDGE_MARGIN),
            y: (wall.0[1] as f32 + EDGE_MARGIN, wall.1[1] as f32 + top - EDGE_MARGIN),
        }
    }
    fn crossed_by(self, camera: &Camera, x: u32, y: u32) -> bool {
        let dir = ray_dir(camera, x as f32 + 0.5, y as f32 + 0.5);
        if dir.z <= 0.0 {
            return false;
        }
        let hit = camera.position + dir * ((self.z - camera.position.z) / dir.z);
        (self.x.0..=self.x.1).contains(&hit.x) && (self.y.0..=self.y.1).contains(&hit.y)
    }
}

/// How far from a block boundary, in blocks, a ray must cross a water face for
/// the block it belongs to (and so its draw position) to be unambiguous.
const BLOCK_EDGE_CLEARANCE: f32 = 0.004;

/// Whether the ray through the pixel crosses plane `z` clear of block edges.
fn clear_of_block_edges(camera: &Camera, x: u32, y: u32, z: f32) -> bool {
    let dir = ray_dir(camera, x as f32 + 0.5, y as f32 + 0.5);
    let hit = camera.position + dir * ((z - camera.position.z) / dir.z);
    [hit.x, hit.y].iter().all(|v| {
        let f = v.rem_euclid(1.0);
        f > BLOCK_EDGE_CLEARANCE && f < 1.0 - BLOCK_EDGE_CLEARANCE
    })
}

/// The part of the top row of water a ray can hit on both faces: source water
/// stands 8/9 of a block high and the mesher lowers some corners to 0.74.
const WATER_TOP_ROW_HEIGHT: f32 = 0.74;

struct Box2 {
    min: [u32; 2],
    max: [u32; 2],
    count: usize,
}

impl Box2 {
    fn new() -> Self {
        Self { min: [u32::MAX; 2], max: [0; 2], count: 0 }
    }
    fn add(&mut self, x: u32, y: u32) {
        self.min = [self.min[0].min(x), self.min[1].min(y)];
        self.max = [self.max[0].max(x), self.max[1].max(y)];
        self.count += 1;
    }
}

impl std::fmt::Display for Box2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.count == 0 {
            return write!(f, "no pixels");
        }
        write!(
            f,
            "{} px in x {}..={}, y {}..={}",
            self.count, self.min[0], self.max[0], self.min[1], self.max[1]
        )
    }
}

fn to_linear(byte: f64) -> f64 {
    let c = byte / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_byte(linear: f64) -> f64 {
    let c = linear.clamp(0.0, 1.0);
    let e = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    e * 255.0
}

type Rgb = [f64; 3];

/// The pixel's channels in linear light.
fn linear(frame: &Frame, x: u32, y: u32) -> Rgb {
    let i = ((y * W + x) * 4) as usize;
    std::array::from_fn(|c| to_linear(f64::from(frame.rgba[i + c])))
}

/// The pixel's encoded bytes as floats.
fn encoded(frame: &Frame, x: u32, y: u32) -> Rgb {
    let i = ((y * W + x) * 4) as usize;
    std::array::from_fn(|c| f64::from(frame.rgba[i + c]))
}

/// Half an encoded level, as an error in linear light at `value`: the rounding
/// error of storing `value` in an 8-bit sRGB target.
fn half_level(value: f64) -> f64 {
    let byte = to_byte(value).round().clamp(0.0, 255.0);
    let up = to_linear((byte + 0.5).min(255.0)) - to_linear(byte);
    let down = to_linear(byte) - to_linear((byte - 0.5).max(0.0));
    up.max(down)
}

/// An error of `error` in linear light at `value`, in encoded levels.
fn in_levels(value: f64, error: f64) -> f64 {
    let up = to_byte(value + error) - to_byte(value);
    let down = to_byte(value) - to_byte(value - error);
    up.max(down)
}

fn add3(a: Rgb, b: Rgb) -> Rgb {
    std::array::from_fn(|c| a[c] + b[c])
}

/// One translucent surface, rendered alone on the sorted pipeline, for each
/// pixel of a list.
struct Layer {
    /// `f(S)`: the surface over the stone backdrop, linear.
    over_stone: Vec<Rgb>,
    /// Rounding error of each `over_stone` value, linear.
    rounding: Vec<Rgb>,
    /// Transmittance `T`.
    t: Vec<f64>,
    /// Bound on the error of `t` from 8-bit quantisation (0 when `t` is given).
    t_error: Vec<f64>,
}

/// Fits `f(S') = f(S) + T*(S' - S)` over the three channels. `fixed_t` replaces
/// the fit with a known transmittance.
fn layer(
    px: &[(u32, u32)],
    over_stone: &Frame,
    over_white: &Frame,
    stone: &Frame,
    white: &Frame,
    fixed_t: Option<f64>,
) -> Layer {
    let mut out = Layer {
        over_stone: Vec::with_capacity(px.len()),
        rounding: Vec::with_capacity(px.len()),
        t: Vec::with_capacity(px.len()),
        t_error: Vec::with_capacity(px.len()),
    };
    for &(x, y) in px {
        let (s, sw) = (linear(stone, x, y), linear(white, x, y));
        let (f, fw) = (linear(over_stone, x, y), linear(over_white, x, y));
        let ds: Rgb = std::array::from_fn(|c| sw[c] - s[c]);
        let denom: f64 = ds.iter().map(|d| d * d).sum();
        let (t, t_error) = match fixed_t {
            Some(t) => (t, 0.0),
            None => {
                let numer: f64 = (0..3).map(|c| (fw[c] - f[c]) * ds[c]).sum();
                let noise: f64 =
                    (0..3).map(|c| ds[c].abs() * (half_level(f[c]) + half_level(fw[c]))).sum();
                ((numer / denom).clamp(0.0, 1.0), noise / denom)
            }
        };
        out.over_stone.push(f);
        out.rounding.push(std::array::from_fn(|c| half_level(f[c])));
        out.t.push(t);
        out.t_error.push(t_error);
    }
    out
}

/// What the hardware's linear-to-sRGB encode may exceed the ideal round-to-nearest
/// by, in levels, per blended store: graphics APIs specify the conversion to
/// 0.6 of a unit in the last place, not 0.5.
const SRGB_ENCODE_SLACK_LEVELS: f64 = 0.1;

/// A composite prediction per pixel: the value in linear light and a bound on
/// its error from the reference frames' 8-bit quantisation.
struct Folded {
    value: Vec<Rgb>,
    error: Vec<Rgb>,
    /// Rounding error the sorted pipeline's own per-draw 8-bit stores add (one
    /// store after each draw, each attenuated by the draws painted after it).
    draw_rounding: Vec<Rgb>,
}

/// Composites each pixel's `layers` (last painted first) over the stone
/// backdrop `stone`.
fn fold(stone: &[Rgb], layers_at: &[Vec<&Layer>]) -> Folded {
    let mut folded = Folded { value: Vec::new(), error: Vec::new(), draw_rounding: Vec::new() };
    for (i, s) in stone.iter().enumerate() {
        let layers = &layers_at[i];
        // d = out - S, accumulated from the back; S is the backdrop frame's own
        // pixel, exact.
        let mut d = [0.0; 3];
        let mut error = [0.0; 3];
        let mut rounding = [0.0; 3];
        for l in layers.iter().rev() {
            let t = l.t[i];
            let stage: Rgb = std::array::from_fn(|c| (l.over_stone[i][c] - s[c]) + t * d[c]);
            error = std::array::from_fn(|c| {
                l.rounding[i][c] + t * error[c] + l.t_error[i] * d[c].abs()
            });
            d = stage;
            rounding = std::array::from_fn(|c| t * rounding[c] + half_level(s[c] + d[c]));
        }
        folded.value.push(add3(*s, d));
        folded.error.push(error);
        folded.draw_rounding.push(rounding);
    }
    folded
}

/// Bin position, in the eight-bin series, of each plane for a pixel whose
/// translucent range runs from the nearest to the farthest of `planes`. The
/// series spans normalised depth `0..=1` with the last bin left empty, so a
/// fragment sits at `7 * normalised depth` bins.
fn bin_positions(planes: &[Plane]) -> Vec<f64> {
    let depths: Vec<f64> = planes.iter().map(|p| p.depth()).collect();
    let near = depths.iter().copied().fold(f64::MAX, f64::min);
    let far = depths.iter().copied().fold(f64::MIN, f64::max);
    let range = (far - near).max(0.0001);
    assert!(
        0.75 * range <= 10.0,
        "range {range} is past the depth at which the bins stop spreading evenly, so the \
         linear bin positions below no longer apply"
    );
    depths.iter().map(|d| (DEPTH_BINS - 1) as f64 * (d - near) / range).collect()
}

/// The absorbance in front of surface `i` that the coefficient series reports.
///
/// The series stores, per bin, the mean of the cumulative absorbance
/// `F(z) = sum_m A_m [z >= z_m]` over the bin. The other surfaces add
/// `A_m * clamp(k + 1 - p_m, 0, 1)` to bin `k`'s mean, `p_m` being surface `m`'s
/// position in bins. A fragment reads its own bin's mean and the previous bin's
/// and interpolates by how far into its own bin it sits; the first bin has no
/// previous one.
fn series_read_back(positions: &[f64], absorbance: &[f64], i: usize) -> f64 {
    let bin_mean = |k: usize| -> f64 {
        (0..positions.len())
            .filter(|&m| m != i)
            .map(|m| absorbance[m] * (k as f64 + 1.0 - positions[m]).clamp(0.0, 1.0))
            .sum()
    };
    let p = positions[i];
    let own = (p.floor() as usize).min(DEPTH_BINS - 1);
    let fraction = p - p.floor();
    if own >= 1 {
        (1.0 - fraction) * bin_mean(own - 1) + fraction * bin_mean(own)
    } else {
        fraction * bin_mean(0)
    }
}

/// What the order-independent pipeline computes per pixel, from its own
/// arithmetic, with the error terms it cannot avoid.
struct Representation {
    /// The composite the series gives with exact arithmetic.
    value: Vec<Rgb>,
    /// Error of `value` from the reference frames' quantisation and the
    /// transmittance fit, linear.
    reference_error: Vec<Rgb>,
    /// Bound on the half-float error of the coefficients, linear.
    coefficient_error: Vec<Rgb>,
    /// Bound on the half-float error of the accumulated colour, linear.
    accumulation_error: Vec<Rgb>,
}

/// Evaluates the order-independent composite for `layers` at `positions` over
/// the stone backdrop.
///
/// The accumulated colour is `sum_i t_i P_i`, with `t_i = exp(-read_back_i)` and
/// `P_i = f_i(S) - T_i S` the surface's premultiplied colour; the composite
/// rescales it by `coverage / sum_i a_i t_i` and blends it over the backdrop with
/// that coverage, the coverage being one minus the product of all `T_i`.
fn oit_representation(stone: &[Rgb], layers: &[&Layer], positions: &[f64]) -> Representation {
    let n = layers.len() as f64;
    let mut out = Representation {
        value: Vec::new(),
        reference_error: Vec::new(),
        coefficient_error: Vec::new(),
        accumulation_error: Vec::new(),
    };
    for (px, s) in stone.iter().enumerate() {
        let t: Vec<f64> = layers.iter().map(|l| l.t[px]).collect();
        let absorbance: Vec<f64> = t.iter().map(|t| (-t.max(1e-4).ln()).clamp(0.0, 4.0)).collect();
        let weight: Vec<f64> = (0..layers.len())
            .map(|i| (-series_read_back(positions, &absorbance, i)).exp())
            .collect();
        let alpha: Vec<f64> = t.iter().map(|t| 1.0 - t).collect();
        let premult: Vec<Rgb> = layers
            .iter()
            .zip(&t)
            .map(|(l, t)| std::array::from_fn(|c| l.over_stone[px][c] - t * s[c]))
            .collect();
        let through: f64 = t.iter().product();
        let coverage = 1.0 - if through < 0.02 { 0.0 } else { through };
        let weight_sum: f64 = (0..layers.len()).map(|i| alpha[i] * weight[i]).sum();
        let colour_sum: Rgb = (0..layers.len()).fold([0.0; 3], |acc, i| {
            add3(acc, std::array::from_fn(|c| weight[i] * premult[i][c]))
        });
        let mean: Rgb = std::array::from_fn(|c| colour_sum[c] / weight_sum);
        out.value.push(std::array::from_fn(|c| coverage * mean[c] + (1.0 - coverage) * s[c]));

        // Reference error: the quantisation of each P_i, carried by its share of
        // the rescaled sum, plus the fit error of each T_i, which moves both the
        // surface's colour and its weight by at most a backdrop-wide swing.
        let reference: Rgb = std::array::from_fn(|c| {
            let quantisation: f64 = (0..layers.len())
                .map(|i| weight[i] * layers[i].rounding[px][c])
                .sum::<f64>()
                * coverage
                / weight_sum;
            let fit: f64 = (0..layers.len())
                .map(|i| {
                    let others: f64 = (0..layers.len()).filter(|&j| j != i).map(|j| t[j]).product();
                    let swing = (premult[i][c] / alpha[i].max(1e-6) - s[c]).abs();
                    layers[i].t_error[px] * others * swing
                })
                .sum();
            quantisation + fit
        });
        out.reference_error.push(reference);

        // Coefficient error. A store rounds to at most `HALF_EPSILON` of the
        // sum it produces, once per fragment that adds to that coefficient, and a
        // fragment adds to the average and to the three Haar nodes above its
        // bin. A node of width `2^-power` holds at most `2^(-power/2) * A`
        // of a fragment's absorbance `A`, and reading it back multiplies by its
        // height scale `2^(power/2)`, so each coefficient read moves the
        // read-back absorbance by at most `fragments * HALF_EPSILON * sum of the
        // absorbances adding to it`. A read interpolates two bins, so the larger
        // of the two paths bounds it. That scales surface `i`'s weight by at
        // most `exp(delta_i)`, and a weight moves the weighted mean colour by
        // `share_i * eta_i * (colour_i - mean)`.
        let bins: Vec<usize> = positions.iter().map(|p| (p.floor() as usize).min(DEPTH_BINS - 1)).collect();
        let read_error = |bin: usize| -> f64 {
            let average: f64 = n * absorbance.iter().sum::<f64>();
            let mut nodes: f64 = 0.0;
            for shift in 1..=3 {
                let node = (bin + DEPTH_BINS) >> shift;
                let members: Vec<usize> =
                    (0..layers.len()).filter(|&m| (bins[m] + DEPTH_BINS) >> shift == node).collect();
                nodes += members.len() as f64 * members.iter().map(|&m| absorbance[m]).sum::<f64>();
            }
            HALF_EPSILON * (average + nodes)
        };
        let eta: Vec<f64> = (0..layers.len())
            .map(|i| {
                let own = read_error(bins[i]);
                let before = if bins[i] >= 1 { read_error(bins[i] - 1) } else { 0.0 };
                own.max(before).exp() - 1.0
            })
            .collect();
        let eta_max = eta.iter().copied().fold(0.0, f64::max);
        let delta_total = read_error(DEPTH_BINS - 1);
        let shares: Vec<f64> = (0..layers.len()).map(|i| alpha[i] * weight[i] / weight_sum).collect();
        let colours: Vec<Rgb> = (0..layers.len())
            .map(|i| std::array::from_fn(|c| premult[i][c] / alpha[i].max(1e-6)))
            .collect();
        out.coefficient_error.push(std::array::from_fn(|c| {
            let shift: f64 =
                (0..layers.len()).map(|i| shares[i] * eta[i] * (colours[i][c] - mean[c]).abs()).sum();
            let coverage_swing = through * delta_total.exp_m1() * (mean[c] - s[c]).abs();
            coverage * shift / (1.0 - eta_max) + coverage_swing
        }));

        // Accumulation error: colour and alpha sums each take `n` half-float
        // roundings of a monotone sum, so the rescaled colour carries at most
        // twice that relative error.
        out.accumulation_error
            .push(std::array::from_fn(|c| 2.0 * n * HALF_EPSILON * coverage * mean[c].abs()));
    }
    out
}

/// How a frame compares to a prediction over a pixel list.
struct Comparison {
    failures: Box2,
    /// Largest deviation, in levels.
    worst: f64,
    /// Largest ratio of deviation to tolerance.
    worst_ratio: f64,
    /// Largest tolerance, in levels.
    widest_tolerance: f64,
}

/// Compares `frame` to `expected` (linear) at each pixel against `tolerance`
/// (linear, per channel), which must already include the rounding of the frame
/// being compared.
fn compare(
    frame: &Frame,
    px: &[(u32, u32)],
    expected: &[Rgb],
    tolerance: &[Rgb],
    encodes: usize,
) -> Comparison {
    let slack = SRGB_ENCODE_SLACK_LEVELS * encodes as f64;
    let mut out = Comparison { failures: Box2::new(), worst: 0.0, worst_ratio: 0.0, widest_tolerance: 0.0 };
    for (i, &(x, y)) in px.iter().enumerate() {
        let got = encoded(frame, x, y);
        let mut failed = false;
        for c in 0..3 {
            let deviation = (got[c] - to_byte(expected[i][c])).abs();
            let allowed = in_levels(expected[i][c], tolerance[i][c]) + slack;
            out.worst = out.worst.max(deviation);
            out.widest_tolerance = out.widest_tolerance.max(allowed);
            out.worst_ratio = out.worst_ratio.max(deviation / allowed.max(1e-9));
            failed |= deviation > allowed;
        }
        if failed {
            out.failures.add(x, y);
        }
    }
    out
}

fn total(parts: &[&[Rgb]]) -> Vec<Rgb> {
    (0..parts[0].len())
        .map(|i| parts.iter().fold([0.0; 3], |acc, p| add3(acc, p[i])))
        .collect()
}

fn max_levels(value: &[Rgb], error: &[Rgb]) -> f64 {
    value
        .iter()
        .zip(error)
        .flat_map(|(v, e)| (0..3).map(move |c| in_levels(v[c], e[c])))
        .fold(0.0, f64::max)
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn backdrop_contrast(stone: &Frame, white: &Frame, x: u32, y: u32) -> f64 {
    let (a, b) = (linear(stone, x, y), linear(white, x, y));
    (0..3).map(|c| (b[c] - a[c]).powi(2)).sum()
}

/// Alpha of every texel of a sprite in the built atlas, level 0.
fn sprite_alphas(models: &lodestone_render::BlockModels, name: &str) -> Vec<u8> {
    let location: lodestone_assets::ResourceLocation = name.parse().expect("valid sprite name");
    let atlas = models.atlas();
    let sprite = atlas.sprite(&location).unwrap_or_else(|| panic!("{name} is not in the atlas"));
    let mip = atlas.mip(0).expect("atlas level 0");
    let mut alphas = Vec::new();
    for y in sprite.y..sprite.y + sprite.height {
        for x in sprite.x..sprite.x + sprite.width {
            alphas.push(mip.rgba[((y * mip.width + x) * 4 + 3) as usize]);
        }
    }
    alphas
}

/// Pixels outside `touched` dilated by [`UNTOUCHED_DILATION`] that differ
/// between two frames.
fn differs_outside(a: &Frame, b: &Frame, touched: &Box2) -> Box2 {
    let mut out = Box2::new();
    let (lo, hi) = (
        [touched.min[0] as i32 - UNTOUCHED_DILATION, touched.min[1] as i32 - UNTOUCHED_DILATION],
        [touched.max[0] as i32 + UNTOUCHED_DILATION, touched.max[1] as i32 + UNTOUCHED_DILATION],
    );
    for y in 0..H {
        for x in 0..W {
            let inside = (x as i32) >= lo[0] && (x as i32) <= hi[0] && (y as i32) >= lo[1] && (y as i32) <= hi[1];
            if !inside && encoded(a, x, y) != encoded(b, x, y) {
                out.add(x, y);
            }
        }
    }
    out
}

/// Bounding box of the pixels where two frames differ at all.
fn changed_region(a: &Frame, b: &Frame) -> Box2 {
    let mut out = Box2::new();
    for y in 0..H {
        for x in 0..W {
            if encoded(a, x, y) != encoded(b, x, y) {
                out.add(x, y);
            }
        }
    }
    out
}

#[test]
#[ignore = "requires a GPU adapter with FLOAT32_BLENDABLE and the vanilla client.jar"]
fn glass_behind_water_composites_water_over_glass_whatever_the_draw_order() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    assert!(
        lodestone_render::oit::supported(device),
        "the device lacks FLOAT32_BLENDABLE, so the order-independent path cannot run here"
    );
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real client.jar \
             under .cache/mc/<ver> (LODESTONE_ASSETS)",
            resources.banner
        )
    });
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    // The surfaces' alpha, from the atlas the shader samples.
    let water_alphas = sprite_alphas(models, "minecraft:block/water_still");
    let water_alpha = water_alphas[0];
    assert!(
        water_alphas.iter().all(|&a| a == water_alpha),
        "the water sprite's alpha is not one value, so its transmittance has no single prediction"
    );
    let water_t = 1.0 - f64::from(water_alpha) / 255.0;
    let mut glass_alphas = sprite_alphas(models, "minecraft:block/red_stained_glass");
    glass_alphas.sort_unstable();
    let glass_modal = glass_alphas[glass_alphas.len() / 2];
    let glass_modal_share =
        glass_alphas.iter().filter(|&&a| a == glass_modal).count() as f64 / glass_alphas.len() as f64;
    assert!(
        glass_modal_share > 0.5,
        "no glass alpha covers over half the texels, so the median transmittance has no \
         single prediction"
    );
    let glass_t = 1.0 - f64::from(glass_modal) / 255.0;
    eprintln!(
        "  atlas alphas: water {water_alpha}/255 (T {water_t:.4}), glass modal {glass_modal}/255 \
         ({:.0}% of texels, T {glass_t:.4})",
        glass_modal_share * 100.0
    );

    let ids = Ids {
        air: first_state_named("minecraft:air"),
        stone: first_state_named("minecraft:stone"),
        white: first_state_named("minecraft:white_concrete"),
        water: first_state_named("minecraft:water"),
        glass: first_state_named("minecraft:red_stained_glass"),
    };
    let full_stone = mesh_world(&build_world(&ids, ids.stone), models);
    let full_white = mesh_world(&build_world(&ids, ids.white), models);

    let water_order = WaterDrawOrder::new(&full_stone);

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = Camera {
        position: EYE,
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
    };
    let mut render = |geometry: &[(SectionKey, SectionGeometry)], improved: bool| {
        render_frame(device, queue, format, &mut target, &atlas, geometry, &camera, improved)
    };

    // Every frame, over the stone backdrop and over the white one.
    let nothing = |full: &[(SectionKey, SectionGeometry)]| select(full, Keep::Nothing, Keep::Nothing);
    let only = |full: &[(SectionKey, SectionGeometry)], plane: Plane| {
        let planes = [plane];
        if plane.is_water() {
            select(full, Keep::Planes(&planes), Keep::Nothing)
        } else {
            select(full, Keep::Nothing, Keep::Planes(&planes))
        }
    };
    let water_scene = |full: &[(SectionKey, SectionGeometry)]| select(full, Keep::Everything, Keep::Nothing);
    let glass_scene = |full: &[(SectionKey, SectionGeometry)]| select(full, Keep::Nothing, Keep::Everything);
    let both_scene = |full: &[(SectionKey, SectionGeometry)]| select(full, Keep::Everything, Keep::Everything);

    struct Backdrop {
        none: Frame,
        surface: [Frame; 4],
        sorted_water: Frame,
        sorted_glass: Frame,
        improved_water: Frame,
        improved_glass: Frame,
    }
    let planes = [Plane::WaterNear, Plane::WaterFar, Plane::GlassFront, Plane::GlassBack];
    let mut backdrop = |full: &[(SectionKey, SectionGeometry)]| Backdrop {
        none: render(&nothing(full), false),
        surface: planes.map(|p| render(&only(full, p), false)),
        sorted_water: render(&water_scene(full), false),
        sorted_glass: render(&glass_scene(full), false),
        improved_water: render(&water_scene(full), true),
        improved_glass: render(&glass_scene(full), true),
    };
    let stone = backdrop(&full_stone);
    let white = backdrop(&full_white);
    let sorted_both = render(&both_scene(&full_stone), false);
    let improved_both = render(&both_scene(&full_stone), true);

    for (name, frame, composites) in [
        ("sorted stone", &stone.none, 0),
        ("sorted water", &stone.sorted_water, 0),
        ("sorted glass", &stone.sorted_glass, 0),
        ("sorted both", &sorted_both, 0),
        ("improved water", &stone.improved_water, 1),
        ("improved glass", &stone.improved_glass, 1),
        ("improved both", &improved_both, 1),
    ] {
        assert_eq!(
            frame.stats.oit_composites, composites,
            "{name}: expected {composites} order-independent composite(s), the frame reports {}",
            frame.stats.oit_composites
        );
    }

    // Pixel sets: where each wall's front face is seen, and where both are.
    let covered = |patches: &[Patch]| -> Vec<(u32, u32)> {
        (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|&(x, y)| patches.iter().all(|p| p.crossed_by(&camera, x, y)))
            .filter(|&(x, y)| patches.iter().all(|p| clear_of_block_edges(&camera, x, y, p.z)))
            .filter(|&(x, y)| backdrop_contrast(&stone.none, &white.none, x, y) >= MIN_BACKDROP_CONTRAST)
            .collect()
    };
    // Both water faces, so the pixel sees the whole wall.
    let water_px = covered(&[
        Patch::at(WATER, Plane::WaterNear.z(), WATER_TOP_ROW_HEIGHT),
        Patch::at(WATER, Plane::WaterFar.z(), WATER_TOP_ROW_HEIGHT),
    ]);
    let glass_px = covered(&[Patch::front_of(GLASS)]);
    let overlap: Vec<(u32, u32)> =
        water_px.iter().copied().filter(|p| glass_px.contains(p)).collect();
    assert!(
        overlap.len() > 5_000,
        "only {} px see water in front of glass — the fixture is not showing the overlap this \
         gate measures",
        overlap.len()
    );
    eprintln!("  pixels: water {}, glass {}, overlap {}", water_px.len(), glass_px.len(), overlap.len());

    let stone_linear = |px: &[(u32, u32)]| -> Vec<Rgb> { px.iter().map(|&(x, y)| linear(&stone.none, x, y)).collect() };
    // One surface's layer over a pixel list.
    let surface_layer = |px: &[(u32, u32)], plane: Plane, fixed: Option<f64>| {
        let i = planes.iter().position(|&p| p == plane).expect("plane");
        layer(px, &stone.surface[i], &white.surface[i], &stone.none, &white.none, fixed)
    };

    // Instrument validation: every surface alone, on the sorted pipeline,
    // against the alpha the atlas holds.
    let transmittance_report = |name: &str, layer: &Layer, predicted: f64| {
        let mut t = layer.t.clone();
        let measured = median(&mut t);
        let bound = layer.t_error.iter().copied().fold(0.0, f64::max);
        eprintln!("  {name}: T {measured:.4}, predicted {predicted:.4}, quantisation bound {bound:.4}");
        assert!(
            (measured - predicted).abs() <= bound,
            "{name}: median transmittance {measured:.4} is {:.4} from the {predicted:.4} the atlas \
             predicts, beyond the {bound:.4} 8-bit quantisation allows",
            (measured - predicted).abs()
        );
    };
    transmittance_report("water near face alone", &surface_layer(&water_px, Plane::WaterNear, None), water_t);
    transmittance_report("water far face alone", &surface_layer(&water_px, Plane::WaterFar, None), water_t);
    transmittance_report("glass front face alone", &surface_layer(&glass_px, Plane::GlassFront, None), glass_t);
    transmittance_report("glass back face alone (culled)", &surface_layer(&glass_px, Plane::GlassBack, None), 1.0);

    // One layer alone, on both paths, against the atlas-predicted transmittance.
    let whole_layer = |px: &[(u32, u32)], on_stone: &Frame, on_white: &Frame| {
        layer(px, on_stone, on_white, &stone.none, &white.none, None)
    };
    transmittance_report(
        "water wall, sorted",
        &whole_layer(&water_px, &stone.sorted_water, &white.sorted_water),
        water_t * water_t,
    );
    transmittance_report(
        "water wall, improved",
        &whole_layer(&water_px, &stone.improved_water, &white.improved_water),
        water_t * water_t,
    );
    transmittance_report(
        "glass wall, sorted",
        &whole_layer(&glass_px, &stone.sorted_glass, &white.sorted_glass),
        glass_t,
    );
    transmittance_report(
        "glass wall, improved",
        &whole_layer(&glass_px, &stone.improved_glass, &white.improved_glass),
        glass_t,
    );

    // The water faces' fixed transmittance is the atlas prediction; the glass is
    // fitted per pixel.
    let near = |px: &[(u32, u32)]| surface_layer(px, Plane::WaterNear, Some(water_t));
    let far = |px: &[(u32, u32)]| surface_layer(px, Plane::WaterFar, Some(water_t));
    let front = |px: &[(u32, u32)]| surface_layer(px, Plane::GlassFront, None);

    let assert_clean = |name: &str, comparison: &Comparison, tolerance: &str| {
        eprintln!(
            "  {name}: worst {:.2} levels, widest tolerance {:.2} ({tolerance}), worst/tolerance {:.2}",
            comparison.worst, comparison.widest_tolerance, comparison.worst_ratio
        );
        assert!(
            comparison.failures.count == 0,
            "{name} left {} pixels beyond tolerance: {} (worst deviation {:.2} levels against \
             at most {:.2})",
            comparison.failures.count,
            comparison.failures,
            comparison.worst,
            comparison.widest_tolerance
        );
    };

    // Each case: the layers nearest first in depth order, the sorted pipeline's
    // draw order at each pixel (last painted first), both frames and the pixels
    // they are checked over.
    struct Case<'a> {
        name: &'a str,
        px: &'a [(u32, u32)],
        depth_order: Vec<(Plane, Layer)>,
        draw_order: Vec<Vec<Plane>>,
        sorted: &'a Frame,
        improved: &'a Frame,
    }
    // Water is painted before any glass; within the water, in mesh order.
    let water_draw = |px: &[(u32, u32)], glass_on_top: bool| -> Vec<Vec<Plane>> {
        px.iter()
            .map(|&(x, y)| {
                let near_first = water_order.rank(&camera, x, y, Plane::WaterNear)
                    < water_order.rank(&camera, x, y, Plane::WaterFar);
                let mut order = Vec::new();
                if glass_on_top {
                    order.push(Plane::GlassFront);
                }
                order.extend(if near_first {
                    [Plane::WaterFar, Plane::WaterNear]
                } else {
                    [Plane::WaterNear, Plane::WaterFar]
                });
                order
            })
            .collect()
    };
    let cases = [
        Case {
            name: "water alone",
            px: &water_px,
            depth_order: vec![(Plane::WaterNear, near(&water_px)), (Plane::WaterFar, far(&water_px))],
            draw_order: water_draw(&water_px, false),
            sorted: &stone.sorted_water,
            improved: &stone.improved_water,
        },
        Case {
            name: "glass alone",
            px: &glass_px,
            depth_order: vec![(Plane::GlassFront, front(&glass_px))],
            draw_order: vec![vec![Plane::GlassFront]; glass_px.len()],
            sorted: &stone.sorted_glass,
            improved: &stone.improved_glass,
        },
        Case {
            name: "water over glass",
            px: &overlap,
            depth_order: vec![
                (Plane::WaterNear, near(&overlap)),
                (Plane::WaterFar, far(&overlap)),
                (Plane::GlassFront, front(&overlap)),
            ],
            draw_order: water_draw(&overlap, true),
            sorted: &sorted_both,
            improved: &improved_both,
        },
    ];

    let mut two_layer_control = None;
    let mut glass_tolerance = 0.0;
    for case in &cases {
        let stone_px = stone_linear(case.px);
        let by_plane = |plane: Plane| -> &Layer {
            &case.depth_order.iter().find(|(p, _)| *p == plane).expect("layer in case").1
        };
        let depth_layers: Vec<&Layer> = case.depth_order.iter().map(|(_, l)| l).collect();
        let draw_layers: Vec<Vec<&Layer>> = case
            .draw_order
            .iter()
            .map(|order| order.iter().map(|&p| by_plane(p)).collect())
            .collect();
        let depth_planes: Vec<Plane> = case.depth_order.iter().map(|(p, _)| *p).collect();

        // The sorted pipeline matches the fold in its own draw order.
        let drawn = fold(&stone_px, &draw_layers);
        let sorted_tolerance = total(&[&drawn.error, &drawn.draw_rounding]);
        let sorted_check = compare(case.sorted, case.px, &drawn.value, &sorted_tolerance, draw_layers[0].len());
        assert_clean(&format!("{}: sorted vs draw-order prediction", case.name), &sorted_check, "quantisation");

        // The order-independent frame matches the fold in depth order.
        let true_order = fold(&stone_px, &vec![depth_layers.clone(); case.px.len()]);
        let repr = oit_representation(&stone_px, &depth_layers, &bin_positions(&depth_planes));
        let rounding: Vec<Rgb> =
            true_order.value.iter().map(|v| std::array::from_fn(|c| half_level(v[c]))).collect();
        let binning: Vec<Rgb> = repr
            .value
            .iter()
            .zip(&true_order.value)
            .map(|(r, t)| std::array::from_fn(|c| (r[c] - t[c]).abs()))
            .collect();
        let tolerance = total(&[
            &true_order.error,
            &binning,
            &repr.coefficient_error,
            &repr.accumulation_error,
            &rounding,
        ]);
        let improved_check = compare(case.improved, case.px, &true_order.value, &tolerance, 1);
        eprintln!(
            "  {}: tolerance parts (max levels): binning {:.2}, coefficients {:.2}, accumulation {:.2}, \
             reference quantisation {:.2}, output rounding {:.2}",
            case.name,
            max_levels(&true_order.value, &binning),
            max_levels(&true_order.value, &repr.coefficient_error),
            max_levels(&true_order.value, &repr.accumulation_error),
            max_levels(&true_order.value, &true_order.error),
            max_levels(&true_order.value, &rounding),
        );
        assert_clean(
            &format!("{}: improved vs depth-order prediction", case.name),
            &improved_check,
            "binning + half floats + quantisation",
        );

        // And the series' own prediction, without the binning term, much tighter.
        let tight = total(&[&repr.reference_error, &repr.coefficient_error, &repr.accumulation_error, &rounding]);
        let series_check = compare(case.improved, case.px, &repr.value, &tight, 1);
        assert_clean(
            &format!("{}: improved vs series prediction", case.name),
            &series_check,
            "half floats + quantisation",
        );

        if case.name == "glass alone" {
            glass_tolerance = sorted_check.widest_tolerance + improved_check.widest_tolerance;
        }
        if case.name == "water over glass" {
            // The control: the sorted frame against the depth-order prediction.
            let mut apart = Box2::new();
            let mut worst = 0.0f64;
            let mut sum = 0.0f64;
            for (i, &(x, y)) in case.px.iter().enumerate() {
                let got = encoded(case.sorted, x, y);
                let deviation = (0..3)
                    .map(|c| (got[c] - to_byte(true_order.value[i][c])).abs())
                    .fold(0.0, f64::max);
                worst = worst.max(deviation);
                sum += deviation;
                if deviation > f64::from(CONTROL_MIN_DEVIATION) {
                    apart.add(x, y);
                }
            }
            two_layer_control = Some((apart, worst, sum / case.px.len() as f64, case.px.len()));
        }
    }
    let (apart, worst, mean, n) = two_layer_control.expect("the two-layer case ran");
    eprintln!(
        "  sorted vs depth-order prediction: worst {worst:.1}, mean {mean:.1} levels; beyond \
         {CONTROL_MIN_DEVIATION}: {apart}"
    );
    assert!(
        apart.count * 10 >= n * 9,
        "control failed: only {} of {n} overlap pixels differ from the depth-order prediction by \
         more than {CONTROL_MIN_DEVIATION} levels on the sorted pipeline ({apart}); the scene \
         cannot tell the two orders apart",
        apart.count
    );

    // Single layers agree with the sorted pipeline's frame wherever the layer
    // is not drawn, and the glass, which has no order to disagree about, agrees
    // within the series' own tolerance everywhere it is drawn.
    for (name, sorted, improved) in [
        ("water", &stone.sorted_water, &stone.improved_water),
        ("glass", &stone.sorted_glass, &stone.improved_glass),
    ] {
        let touched = changed_region(sorted, &stone.none);
        let stray = differs_outside(improved, sorted, &touched);
        assert!(
            stray.count == 0,
            "improved {name}-only frame differs from the sorted one outside the layer's own \
             region ({touched}): {stray}"
        );
    }
    // The detector's control: the water region does not hide a difference
    // elsewhere, so the check above would see one.
    let wrong_region = changed_region(&stone.sorted_glass, &stone.none);
    assert!(
        differs_outside(&stone.sorted_water, &stone.none, &wrong_region).count > 0,
        "control failed: comparing the water frame outside the glass region found no difference"
    );
    let glass_case = &cases[1];
    let sorted_vs_improved: f64 = glass_case
        .px
        .iter()
        .map(|&(x, y)| {
            let (a, b) = (encoded(&stone.sorted_glass, x, y), encoded(&stone.improved_glass, x, y));
            (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0, f64::max)
        })
        .fold(0.0, f64::max);
    eprintln!(
        "  glass alone, sorted vs improved: worst {sorted_vs_improved:.0} levels, allowed {glass_tolerance:.2}"
    );
    assert!(
        sorted_vs_improved <= glass_tolerance,
        "a single glass surface has no order to disagree about, yet the sorted and improved \
         frames differ by {sorted_vs_improved} levels, beyond the {glass_tolerance:.2} their two \
         predictions allow"
    );
}
