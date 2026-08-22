//! Pixel gate: a sign's text must be **stably** present as the camera moves,
//! at every distance the gather admits — not merely present in one still
//! frame at reading range.
//!
//! # Why this exists
//!
//! Owner report, from real play: *"signs flicker in and out (completely)
//! when they're far away and I move"*. Three words are load-bearing.
//! *Completely* means a binary include/exclude, not a sampling artefact;
//! *far away* and *as I move* mean the decision varies with the camera. Two
//! mechanisms in the sign path have that shape, and this file measures both
//! so the investigation lands on one rather than on a plausible story.
//!
//! # The blind spot it closes
//!
//! `sign_text_pixels.rs` renders against an empty world (no board in the
//! depth buffer at all). `world_text_over_geometry_pixels.rs` fixed that and
//! put the sign's own board in the depth buffer — but at `2.5` and `12.0`
//! blocks only. `live_sign_text_pixels.rs` shoots one still frame from three
//! blocks away. So the whole sign corpus shares two coincidences: every
//! camera is **close**, and every measurement is a **single** frame. The
//! admitted range is 64 blocks (`block_entities::VIEW_DISTANCE`, which is
//! vanilla's own `BlockEntityRenderer.getViewDistance` default — no sign
//! renderer overrides it), so the corpus was covering under a fifth of it.
//!
//! # What the two arms found
//!
//! [`the_raw_ink_board_standoff_is_exhausted_at_range_so_the_polygon_offset_carries_it`]
//! is arithmetic and needs no GPU. The ink plane sits `0.005` blocks in front
//! of the board face, and through this project's **forward** `[0,1]` depth
//! that is 262 ULPs of `Depth32Float` at 4 blocks and **1 ULP at 48 and
//! beyond**. Two exactly parallel planes one representable step apart is the
//! textbook recipe for the reported symptom, because a parallel pair flips
//! *whole* rather than speckling.
//!
//! [`sign_text_is_stably_present_across_distance_and_camera_motion`] then
//! renders it and **excludes** that hypothesis: the ink holds at a constant
//! 657 px at every one of the seven distances and every sub-block camera
//! displacement. `TEXT_POLYGON_OFFSET`'s `constant: -10` bias is ten ULPs at
//! the fragment's own exponent, and it really does carry the ordering the
//! geometry stopped carrying. So the arithmetic arm is a *pinned finding*
//! rather than a bug report — it records that the offset is load-bearing and
//! that the `0.005`-block standoff must never be read as headroom.
//!
//! The mechanism that *does* fit the report is the sign-text pass's fixed
//! vertex budget; that is measured and gated in `gpu/sign_text.rs`'s own test
//! module, since it is a property of the vertex builder and needs no GPU.
//!
//! # Why the field of view narrows with distance, and why that is not cheating
//!
//! At 70° a one-block sign is ~8 px tall at 60 blocks, so its text is
//! sub-pixel and *no* pixel gate could measure it either way. The field of
//! view is therefore narrowed as the camera retreats, keeping the sign's
//! projected size roughly constant. **The depth question is untouched by
//! this**: `z_ndc = f(n - d) / (d(n - f))` depends only on `near`, `far` and
//! the distance, never on the field of view, so every ULP figure above holds
//! exactly as written. What the narrow lens buys is only the ability to
//! *see* the answer. The constant 657 px across all seven distances is that
//! compensation working, not a frozen render.
//!
//! # Two fixture traps, both measured
//!
//! **The board does not paint unless the section fade clock is advanced.** A
//! section uploaded this frame is mid-fade-in and draws nothing, so a gate
//! that leaves the clock at zero renders its "real board" as sky and every
//! depth claim it makes is vacuous. This file's first run measured exactly
//! that — 0 board pixels at all seven distances — and it is caught here by a
//! precondition rather than reasoned about.
//!
//! **The first-person bare arm paints unconditionally** whenever no
//! third-person body is reported, at a fixed screen rect regardless of camera
//! angle. Suppressed rather than reasoned around.
//!
//! # What this proves, and what it does not
//!
//! `CLAUDE.md`: *a pixel gate proves the draw, and proves nothing past the
//! edge of its own fixture.* This file installs its own [`SignSpawn`], so it
//! proves the **draw** half only — that given correct spans, the ink reaches
//! and holds pixels over a real board at range. The supply half (a real
//! server's bytes reaching `sign_spawns`) is `live_sign_text_wire.rs`'s and
//! `live_sign_text_pixels.rs`'s, and is not re-proved here.
//!
//! Fail-closed: no GPU adapter or no `client.jar` is a failure, never a
//! silent skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test sign_text_distance_stability_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR, ThirdPersonBodyState};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility,
};
use lodestone::resources::BlockResources;
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, SignKind, SignOrientation,
    SignSpawn, entity_anim::AnimInput, fog::FogSettings, sign_text_transform,
};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, SignSide,
    SignTextSpan, World,
};

const W: u32 = 960;
const H: u32 = 720;

/// Render distance, in chunks. Large enough that fog is negligible at the
/// far end of the sweep — a fogged board and fogged ink converge on the sky
/// colour together and would confound the subtraction with a second cause.
const RD_CHUNKS: u32 = 16;

const MIN_Y: i32 = 0;
const SECTION_COUNT: usize = 8;
const GROUND_Y: i32 = 64;
/// The sign's block position: standing on the platform's top face.
const SIGN: [i32; 3] = [0, 65, 0];

/// `SignOrientation::Ground { rotation_segment: 0 }` — segment 0 is north, so
/// the **front** text plane faces `+Z` and a reader stands south of it. Named
/// once here because both the world's block state and the spawn must agree:
/// a spawn describing a different rotation than the block placed would put
/// the ink beside its own board rather than on it.
const ROTATION_SEGMENT: u8 = 0;

/// `block/template_sign_rot_0.json`'s upper element runs `z` from `7.33333`
/// to `8.66667` in the model's `0..16` space, so the board's `+Z` face sits
/// at `8.66667/16 = 0.5416669` in block space — `0.0416669` from the block
/// centre the text transform is built around. Transcribed from the jar, not
/// derived from our own mesher, so it is an outside expectation.
const BOARD_FRONT_FACE_LOCAL_Z: f32 = 8.666_67 / 16.0 - 0.5;

/// `sign_text_transform`'s own `TEXT_OFFSET.z` for a plain sign. Duplicated
/// here deliberately: the arithmetic arm must state the separation it
/// predicts from the *source constants*, and reading it back out of the
/// matrix under test would make the prediction agree with itself.
const TEXT_OFFSET_LOCAL_Z: f32 = 0.046_666_667;

/// The distances swept. The far end is just inside
/// `block_entities::VIEW_DISTANCE` (64), so every sample is a range the
/// production gather really does hand to the draw — a gate stopping at 12
/// blocks, as the corpus did, is testing a quarter of the admitted domain.
const DISTANCES: [f32; 7] = [4.0, 8.0, 16.0, 24.0, 32.0, 48.0, 63.0];

/// Sub-block camera displacements applied at each distance — "and I move".
/// Deliberately **not** round fractions of a block: a round step can land
/// every sample on the same side of a rounding boundary and make an unstable
/// frame look stable, which is the coincident-fixture species `CLAUDE.md`
/// records. These are irrational-looking thirds of a texel and up.
const JITTERS: [f32; 6] = [0.0, 0.0137, 0.0411, 0.1073, 0.2531, 0.4909];

/// The separation, in ULPs of `Depth32Float`, at or below which the raw
/// geometry can no longer be said to order the two planes on its own — a
/// tie or one representable step, which `LessEqual` resolves in the ink's
/// favour only by luck. The measurement below asserts the standoff **does**
/// decay to this by [`BIAS_LOAD_BEARING_DISTANCE`]; see that gate's doc for
/// why that is the finding rather than the bug.
const RAW_SEPARATION_EXHAUSTED_ULPS: u32 = 2;

/// The distance by which the raw standoff is exhausted. Well inside
/// `block_entities::VIEW_DISTANCE`, so it is a range real play spends time
/// at, not a corner.
const BIAS_LOAD_BEARING_DISTANCE: f32 = 48.0;

/// Far past the section fade-in: a section uploaded this frame is mid-fade
/// and draws nothing until the fade clock passes it. Same constant, same
/// reason, as `live_sign_text_pixels.rs`.
const FADE_COMPLETE_TICK: u64 = 200;

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

fn load_vanilla() -> (BlockResources, std::sync::Arc<lodestone_render::BlockAtlas>) {
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real \
             client.jar under .cache/mc/26.2 (LODESTONE_ASSETS)",
            resources.banner
        )
    });
    (resources, atlas)
}

/// See `world_text_over_geometry_pixels.rs`'s identical helper: `RenderState`
/// draws an unconditional first-person bare arm whenever no third-person body
/// is reported, at a fixed screen rect regardless of camera angle.
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
        })
    });
}

fn state_id(state: &str) -> u32 {
    lodestone_data::block_states::state_id(state)
        .unwrap_or_else(|| panic!("{state} is not in the 26.2 block-state table"))
}

/// Manhattan RGB distance above which two renders count as different at a
/// pixel. Lower than the block-entity gates' 60: at 63 blocks a glyph row is
/// about one pixel wide and lands substantially blended with the board
/// behind it, so a threshold tuned for a solid cuboid would report "no ink"
/// for ink that is plainly there. The control below is what keeps this
/// honest — the same threshold must yield **zero** changed pixels for two
/// renders of the same frame.
const DIFFERS: i32 = 12;

fn differs(a: &[u8], b: &[u8]) -> bool {
    (i32::from(a[0]) - i32::from(b[0])).abs()
        + (i32::from(a[1]) - i32::from(b[1])).abs()
        + (i32::from(a[2]) - i32::from(b[2])).abs()
        > DIFFERS
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Rect {
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    fn padded(self, pad: i32) -> Rect {
        Rect {
            x0: self.x0 - pad,
            y0: self.y0 - pad,
            x1: self.x1 + pad,
            y1: self.y1 + pad,
        }
    }
}

/// Count and bounding box of the pixels that differ between two frames. The
/// box is returned so a failure localises rather than aggregating.
fn changed(subject: &[u8], control: &[u8]) -> (usize, Option<Rect>) {
    let mut count = 0usize;
    let mut rect: Option<Rect> = None;
    for (i, (s, c)) in subject
        .chunks_exact(4)
        .zip(control.chunks_exact(4))
        .enumerate()
    {
        if !differs(s, c) {
            continue;
        }
        count += 1;
        let x = (i as u32 % W) as i32;
        let y = (i as u32 / W) as i32;
        rect = Some(match rect {
            None => Rect { x0: x, y0: y, x1: x, y1: y },
            Some(r) => Rect {
                x0: r.x0.min(x),
                y0: r.y0.min(y),
                x1: r.x1.max(x),
                y1: r.y1.max(y),
            },
        });
    }
    (count, rect)
}

fn project(view_proj: glam::Mat4, world: glam::Vec3) -> Option<(f32, f32)> {
    let clip = view_proj * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    Some((
        (clip.x / clip.w * 0.5 + 0.5) * W as f32,
        (1.0 - (clip.y / clip.w * 0.5 + 0.5)) * H as f32,
    ))
}

/// A world with a stone platform and one real `oak_sign` **block** on it, so
/// the board is ordinary terrain geometry in the depth buffer. Same fixture
/// shape as `world_text_over_geometry_pixels.rs`'s `sign_world`.
fn sign_world() -> World {
    let air = state_id("minecraft:air");
    let stone = state_id("minecraft:stone");
    let sign = state_id("minecraft:oak_sign[rotation=0,waterlogged=false]");

    let mut world = World::new();
    for cx in -1..=1 {
        for cz in -1..=1 {
            let column = ChunkColumn::new(
                MIN_Y,
                SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                air,
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
    let filled = world.fill_region([-16, MIN_Y, -16], [15, GROUND_Y, 15], stone);
    assert!(filled > 0, "fixture: the platform must actually write blocks");
    world.fill_region(SIGN, SIGN, sign);
    world
}

fn upload_world(
    world: &World,
    models: &lodestone_render::BlockModels,
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) {
    let mut uploaded = 0usize;
    for cx in -1..=1 {
        for cz in -1..=1 {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let opaque = mesh_snapshot_models(&snap, models, false);
                let visibility = snapshot_visibility(&snap, models);
                state.upload_section(
                    device,
                    queue,
                    key,
                    &SectionGeometry::Model {
                        opaque,
                        water: ModelMesh::default(),
                        translucent_blocks: ModelMesh::default(),
                        visibility,
                    },
                );
                uploaded += 1;
            }
        }
    }
    assert!(uploaded > 0, "fixture: some sections must have uploaded");
}

fn orientation() -> SignOrientation {
    SignOrientation::Ground { rotation_segment: ROTATION_SEGMENT }
}

fn sign_spawn() -> SignSpawn {
    let mut front = SignSide::default();
    front.lines[0] = vec![SignTextSpan { text: "LODESTONE".to_owned(), ..Default::default() }];
    front.lines[1] = vec![SignTextSpan { text: "HOSPITAL".to_owned(), ..Default::default() }];
    front.lines[2] = vec![SignTextSpan { text: "THIS WAY".to_owned(), ..Default::default() }];
    // Glowing, so the ink is the full dye colour rather than the 0.4-scaled
    // dark default: this gate is about whether the ink survives at range,
    // and near-black ink on an oak board is a needlessly small signal.
    front.glowing = true;
    front.color = lodestone_world::SignDyeColor::White;
    SignSpawn {
        pos: SIGN,
        kind: SignKind::Plain,
        orientation: orientation(),
        front,
        back: SignSide::default(),
        light: lodestone_render::ENTITY_FULLBRIGHT,
    }
}

/// Keeps the sign's projected size roughly constant as the camera retreats —
/// see the module doc for why this is a magnification knob and not a
/// weakening of the depth question. Anchored on the 70° the near sample
/// uses, so the near end of the sweep is an ordinary field of view.
fn fov_for(distance: f32) -> f32 {
    const ANCHOR_DISTANCE: f32 = 4.0;
    const ANCHOR_FOV: f32 = 70.0;
    let half = (ANCHOR_FOV.to_radians() / 2.0).tan() * ANCHOR_DISTANCE / distance;
    (2.0 * half.atan()).to_degrees()
}

/// A camera facing the sign's own text plane at `distance`, displaced
/// `jitter` blocks along the plane's local `+X`. The heading is derived from
/// [`sign_text_transform`]'s matrix rather than from a remembered yaw, and
/// re-aimed at the plane origin after the displacement so the sign stays in
/// frame — "the camera moved", not "the camera looked away".
fn camera_facing_sign(distance: f32, jitter: f32) -> Camera {
    let matrix = sign_text_transform(SIGN, SignKind::Plain, orientation(), true);
    let origin = matrix.transform_point3(glam::Vec3::ZERO);
    let normal = (matrix.transform_point3(glam::Vec3::Z) - origin).normalize();
    let right = (matrix.transform_point3(glam::Vec3::X) - origin).normalize();
    let eye = origin + normal * distance + right * jitter;
    let dir = (origin - eye).normalize();
    Camera {
        position: eye,
        // `Camera::basis`: forward is
        // `(-sin(yaw)cos(pitch), -sin(pitch), cos(yaw)cos(pitch))`, inverted
        // here rather than guessed.
        yaw: (-dir.x).atan2(dir.z).to_degrees(),
        pitch: -dir.y.clamp(-1.0, 1.0).asin().to_degrees(),
        fov_y_degrees: fov_for(distance),
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS, 0),
    }
}

/// The screen rect the sign's text plane can occupy, projected through the
/// same transform and the same `view_projection` the draw uses.
fn expected_text_rect(view_proj: glam::Mat4) -> Rect {
    let matrix = sign_text_transform(SIGN, SignKind::Plain, orientation(), true);
    let half_w = SignKind::Plain.max_text_line_width() / 2.0;
    let half_h = 2.0 * SignKind::Plain.text_line_height();
    let mut rect: Option<Rect> = None;
    for (lx, ly) in [
        (-half_w, -half_h),
        (half_w, -half_h),
        (-half_w, half_h),
        (half_w, half_h),
    ] {
        let world = matrix.transform_point3(glam::Vec3::new(lx, ly, 0.0));
        let (sx, sy) = project(view_proj, world).expect("the text plane must be in front");
        let (x, y) = (sx.round() as i32, sy.round() as i32);
        rect = Some(match rect {
            None => Rect { x0: x, y0: y, x1: x, y1: y },
            Some(r) => Rect {
                x0: r.x0.min(x),
                y0: r.y0.min(y),
                x1: r.x1.max(x),
                y1: r.y1.max(y),
            },
        });
    }
    rect.expect("four corners")
}

// --- The arithmetic arm ----------------------------------------------------

/// The depth value the real [`Camera::view_projection`] assigns one world
/// point — `clip.z / clip.w`, which is exactly what the rasteriser
/// interpolates and writes into a `Depth32Float` attachment. Read through
/// the real projection rather than a transcribed formula, so a change of
/// depth convention is caught here rather than assumed away.
fn depth_of(camera: &Camera, point: glam::Vec3) -> f32 {
    let clip = camera.view_projection() * glam::Vec4::new(point.x, point.y, point.z, 1.0);
    clip.z / clip.w
}

/// The two world points whose depths decide whether a sign's ink is visible:
/// the text plane's own origin (the exact point
/// [`sign_text_transform`] places local `(0, 0, 0)` at) and the board face
/// `separation` blocks behind it along the plane's normal.
fn ink_and_board_points(separation: f32) -> (glam::Vec3, glam::Vec3) {
    let matrix = sign_text_transform(SIGN, SignKind::Plain, orientation(), true);
    let origin = matrix.transform_point3(glam::Vec3::ZERO);
    let normal = (matrix.transform_point3(glam::Vec3::Z) - origin).normalize();
    (origin, origin - normal * separation)
}

fn ulps_between(a: f32, b: f32) -> u32 {
    a.to_bits().abs_diff(b.to_bits())
}

/// **The measurement, no GPU required — and the reason the polygon offset in
/// `gpu/sign_text.rs` is not optional.**
///
/// A sign's ink plane and the board face it sits on are `0.005` blocks
/// apart: `sign_text_transform`'s `TEXT_OFFSET.z` of `0.046666667` against
/// `template_sign_rot_0.json`'s board face at `8.66667/16 - 0.5`. Both
/// constants come from the 26.2 jar, so the premise is an outside
/// expectation; the ULP count is IEEE-754 arithmetic on whatever depth the
/// real [`Camera::view_projection`] produces.
///
/// Through this project's **forward** `[0,1]` depth that standoff decays
/// fast — measured here at 262 ULPs at 4 blocks, 65 at 8, 15 at 16, 7 at 24,
/// 5 at 32 and **1 at 48 and beyond**. Vanilla's reversed-Z would keep it
/// comfortable at every one of those ranges; ours spends its float exponent
/// at the near plane instead (`CLAUDE.md`'s rendering constraints).
///
/// **This is a finding, not a failure.** The sibling pixel arm below renders
/// the same sign over the same board at all seven distances and measures the
/// ink holding at a constant 657 px, so the `TEXT_POLYGON_OFFSET` bias
/// (`constant: -10`, i.e. ten ULPs at the fragment's own exponent) really
/// does carry the ordering the geometry stopped carrying. What this gate
/// pins is that **it has to**: anyone who removes the bias, or who reads the
/// `0.005`-block standoff as headroom, is relying on a separation that is
/// one representable step wide over most of the sign's admitted range.
///
/// It is a *magnitude* claim rather than a direction: the wrong hypothesis
/// ("the standoff is comfortable, the pass could do without the offset")
/// predicts a healthy count at every range, and both are computed in the
/// same run.
#[test]
fn the_raw_ink_board_standoff_is_exhausted_at_range_so_the_polygon_offset_carries_it() {
    let separation = TEXT_OFFSET_LOCAL_Z - BOARD_FRONT_FACE_LOCAL_Z;
    assert!(
        (separation - 0.005).abs() < 1e-6,
        "the fixture's own premise moved: the jar's board face and TEXT_OFFSET \
         are now {separation} blocks apart, not 0.005 — re-derive this gate's \
         constants from the 26.2 model and StandingSignRenderer before reading \
         anything below"
    );

    let mut table = Vec::new();
    for distance in DISTANCES {
        let camera = camera_facing_sign(distance, 0.0);
        let (ink_p, board_p) = ink_and_board_points(separation);
        let board = depth_of(&camera, board_p);
        let ink = depth_of(&camera, ink_p);
        let ulps = ulps_between(board, ink);
        println!(
            "distance {distance:>5}: board z {board:.9}, ink z {ink:.9}, \
             separation {ulps} ULP(s) of Depth32Float"
        );
        table.push((distance, ulps));
    }

    // Monotone decay: a separation that grew with distance would mean the
    // depth convention had changed under this file and every sentence above
    // it needs re-reading.
    for pair in table.windows(2) {
        let [(near, near_ulps), (far, far_ulps)] = [pair[0], pair[1]];
        assert!(
            far_ulps <= near_ulps,
            "the ink/board depth separation grew from {near_ulps} ULPs at {near} \
             blocks to {far_ulps} at {far}. That cannot happen under a forward \
             [0,1] projection, so the projection has changed — re-read this \
             file's premise before trusting any other sign gate."
        );
    }

    let (_, at_range) = table
        .iter()
        .copied()
        .find(|(d, _)| (*d - BIAS_LOAD_BEARING_DISTANCE).abs() < f32::EPSILON)
        .expect("DISTANCES must contain BIAS_LOAD_BEARING_DISTANCE");
    assert!(
        at_range <= RAW_SEPARATION_EXHAUSTED_ULPS,
        "the raw ink/board standoff measures {at_range} ULPs at \
         {BIAS_LOAD_BEARING_DISTANCE} blocks, above the {RAW_SEPARATION_EXHAUSTED_ULPS} \
         this file was written against. If the projection moved to reversed-Z, that \
         is an improvement and this gate and its module doc should be rewritten \
         rather than satisfied — the sign pass would then have real geometric \
         headroom for the first time."
    );
}

// --- The pixel arms --------------------------------------------------------

struct Shot {
    pixels: Vec<u8>,
    vertices: u32,
}

/// One frame from an already-built, already-uploaded [`RenderState`].
/// `install` chooses the subject (the sign wired in) or the control (a sign
/// source returning nothing, so the identical scene minus the ink).
///
/// The state is reused across the whole sweep rather than rebuilt per shot —
/// 84 frames each re-uploading 72 sections is minutes of nothing, and reuse
/// is what production does anyway.
fn shoot(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    state: &mut RenderState,
    camera: &Camera,
    install: bool,
) -> Shot {
    if install {
        let spawn = sign_spawn();
        state.set_sign_source(move |_eye| vec![spawn.clone()]);
    } else {
        state.set_sign_source(|_eye| Vec::new());
    }
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), camera, None, &[]);
    Shot {
        pixels: target.read_texels(device, queue),
        vertices: stats.sign_text_vertices,
    }
}

/// **The gate the corpus could not run**: the same sign, over its own real
/// board, swept across every range the gather admits and across sub-block
/// camera displacements at each one.
///
/// The assertion is *stability*, not presence. Presence at one distance is
/// what `world_text_over_geometry_pixels.rs` already establishes; what the
/// owner reports is text that is there and then is not, so the failure this
/// has to be able to name is "ink at some camera positions and none at
/// others, at the same distance".
///
/// Failures are collected rather than asserted inside the loops, so the
/// report names **every** range that broke — an `assert!` in the loop proves
/// exactly one and leaves the rest as arguments.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn sign_text_is_stably_present_across_distance_and_camera_motion() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");
    let world = sign_world();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut state = RenderState::new(device, queue, format, W, H, Some(&atlas));
    suppress_first_person_arm(&mut state);
    state.set_fog(
        FogSettings::for_render_distance(SKY_COLOR, RD_CHUNKS),
        RD_CHUNKS,
    );
    upload_world(&world, models, &mut state, device, queue);
    // **Without this the board never paints.** A section uploaded this frame is
    // mid-fade-in and draws nothing until the fade clock passes it, so a gate
    // that leaves the clock at zero renders its "real board" as sky and every
    // depth-test claim it makes is vacuous. `live_sign_text_pixels.rs` names
    // the same constant for the same reason.
    state.update_animation(queue, FADE_COMPLETE_TICK);

    // Collected, then asserted once — see the doc above.
    let mut blank: Vec<(f32, f32, usize)> = Vec::new();
    let mut unstable: Vec<(f32, Vec<usize>)> = Vec::new();
    let mut no_board: Vec<f32> = Vec::new();

    for distance in DISTANCES {
        let mut counts = Vec::new();
        for jitter in JITTERS {
            let camera = camera_facing_sign(distance, jitter);
            let view_proj = camera.view_projection();
            let rect = expected_text_rect(view_proj);

            let subject = shoot(device, queue, &mut target, &mut state, &camera, true);
            let control = shoot(device, queue, &mut target, &mut state, &camera, false);

            assert!(
                subject.vertices > 0,
                "at {distance} blocks / jitter {jitter} the fixture produced no \
                 sign-text vertices at all — the font or the text chain is \
                 broken, not the depth test"
            );
            assert_eq!(
                control.vertices, 0,
                "the control must install no sign source, or the subtraction \
                 below measures something other than the ink"
            );

            // The board must actually be in the depth buffer, or this whole
            // sweep is the empty-world corpus with extra steps. A gate whose
            // subject is absent reports health for a scene that never existed.
            let sky = SKY_COLOR.map(|c| (c * 255.0).round() as u8);
            let board_px = control
                .pixels
                .chunks_exact(4)
                .enumerate()
                .filter(|(i, px)| {
                    rect.contains((*i as u32 % W) as i32, (*i as u32 / W) as i32)
                        && (i32::from(px[0]) - i32::from(sky[0])).abs()
                            + (i32::from(px[1]) - i32::from(sky[1])).abs()
                            + (i32::from(px[2]) - i32::from(sky[2])).abs()
                            > 60
                })
                .count();
            if board_px == 0 && jitter == 0.0 {
                no_board.push(distance);
            }

            let (count, bbox) = changed(&subject.pixels, &control.pixels);
            println!(
                "distance {distance:>5} jitter {jitter:<7}: {count:>6} ink px, \
                 board {board_px:>6} px, bbox {bbox:?}, rect {rect:?}, \
                 {} vertices",
                subject.vertices
            );
            if count == 0 {
                blank.push((distance, jitter, board_px));
            }
            let padded = rect.padded(4);
            let outside = subject
                .pixels
                .chunks_exact(4)
                .zip(control.pixels.chunks_exact(4))
                .enumerate()
                .filter(|(i, (s, c))| {
                    differs(s, c)
                        && !padded.contains((*i as u32 % W) as i32, (*i as u32 / W) as i32)
                })
                .count();
            assert_eq!(
                outside, 0,
                "at {distance} blocks / jitter {jitter}, {outside} of {count} changed \
                 pixels fall outside the sign's own text plane {padded:?} — the sign \
                 source is moving something other than sign text (bbox {bbox:?})"
            );
            counts.push(count);
        }
        // Toggling within one distance is the reported symptom itself: the
        // camera moved a fraction of a block and the ink came and went.
        let min = counts.iter().copied().min().unwrap_or(0);
        let max = counts.iter().copied().max().unwrap_or(0);
        if min == 0 && max > 0 {
            unstable.push((distance, counts));
        }
    }

    assert!(
        no_board.is_empty(),
        "the sign's board painted nothing inside its own projected rect at {no_board:?} \
         blocks, so the depth buffer there is empty and every 'ink survived' reading \
         at those ranges is vacuous — fix the fixture before reading the rest"
    );
    assert!(
        unstable.is_empty(),
        "sign text TOGGLED with sub-block camera motion at {unstable:?} — \
         (distance, ink-pixel count per jitter). This is the reported symptom \
         reproduced: the ink and the board are the same depth to within the \
         hardware's precision at that range, so which one wins depends on \
         rounding, and both planes being parallel the whole text flips at once."
    );
    assert!(
        blank.is_empty(),
        "sign text reached the vertex buffer and changed ZERO pixels at \
         {blank:?} — (distance, jitter, board pixels present). The ink is \
         being drawn and losing the depth test against the sign's own board."
    );
}
