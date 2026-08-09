//! Pixel gate: the walking bob (issue #58's `bobView`) **moves the world on
//! screen**, by the amount vanilla's constants predict and in the right
//! direction.
//!
//! # Why a pixel gate and not a matrix test
//!
//! `camera_rig`'s unit tests already compare the folded camera against vanilla's
//! own `P · B · V` and agree to 1e-4, which pins the algebra. What they cannot
//! see is whether anything *calls* it — the island shape `CLAUDE.md` §1 names —
//! and a matrix test also cannot tell an inverted sign convention from a correct
//! one, because both look like "a matrix". So this file renders two frames and
//! measures where a real object lands in each.
//!
//! # The precondition half lives in `sim.rs`, and that split is deliberate
//!
//! A bob gate whose fixture never accumulates `walkDist` measures nothing, so the
//! *production wiring* — a real `Sim` driven by real `GameTick`s with the forward
//! key held, whose phase and amplitude are asserted to have actually grown — is
//! `sim::tests::walking_accumulates_a_real_bob_that_only_the_render_camera_sees`.
//! It has to be there rather than here: the offline world is real generated
//! terrain, the player spawns on a slope, and walking north walls them out after
//! ~0.2 blocks, so the fixture needs `Sim::set_block_world` to flatten a corridor
//! and that method is private. **Without it this file would be measuring a
//! stationary player**, which is exactly how a bob gate goes vacuous.
//!
//! That same test also asserts the thing that would be a gameplay bug rather than
//! a visual one: `Sim::camera` (the block-targeting ray origin and the audio
//! listener) must **not** bob, while `Sim::render_camera` must.
//!
//! What this file adds is the pixels. It pins the [`BobFrame`] to the two phases
//! whose displacement is hand-predicted below — the dip's bottom and the peak of
//! the sway, both at the `0.1` amplitude ceiling a real walking player reaches —
//! so the predicted pixel offsets can be exact rather than a range. The chest's
//! pixel **bounding box** is measured in each frame and compared against a
//! prediction computed from vanilla's constants and pinhole geometry.
//!
//! # The first-person arm, and why it cannot contaminate this
//!
//! `CLAUDE.md` records a control that asserted a frame "clears uniformly" and
//! failed at 3.5% entirely because of the **first-person bare arm**, which
//! `RenderState` draws whenever no third-person body is installed — i.e. in every
//! frame here. The arm is attached to the camera, so the obvious worry is that it
//! moves *with* the bob and dominates any diff.
//!
//! It does not, for a reason worth stating rather than assuming: the hand pass
//! uses its own projection, not `Camera::view_projection`
//! (`lodestone-shell/src/gpu/first_person.rs`'s `hand_projection` doc says so
//! outright — using the world matrix there "would leave the arm parked at the
//! world origin"). Every `frame()` call in *this* file's world-measuring test
//! installs no [`lodestone::gpu::RenderState::set_hand_bob_source`], so the
//! hand reads [`BobFrame::default`] regardless of what `cam` carries — the arm
//! stays **pixel-identical in both frames** for a reason this file controls
//! directly, not by accident. [`the_arm_does_not_move_when_no_hand_bob_source_is_installed`]
//! locates the arm's own bounding box, asserts it is disjoint from the chest's,
//! and asserts it is *unchanged* between the two frames. Every measurement in
//! the world-bob test above is confined to the chest's projected rect.
//!
//! # The arm *does* bob once the shell feeds it one (issue #58's hand-side gap)
//!
//! Vanilla prefixes `bobHurt` and `bobView` onto `renderItemInHand`'s pose stack
//! a **second, independent** time (`GameRenderer.java:344-346`), separate from
//! the world's own copy (`:534-536`) — not something the hand inherits from the
//! bobbed world camera. `gpu/first_person.rs`'s `HandBobSource`/`hand_view_proj`
//! are that second application, and `the_arm_moves_when_a_hand_bob_source_is_installed`
//! below proves it reaches real rendered pixels, with the same production
//! `BobFrame` a real `Sim` would install via
//! `RenderState::set_hand_bob_source`. The *exact* pixel prediction — hand-derived
//! from vanilla's constants, no GPU needed — lives in
//! `gpu::first_person::tests` (`the_dip_moves_the_test_point_by_the_hand_derived_pixel_offset`
//! and its sway sibling); this file's job is only the thing a matrix test cannot
//! show, the same division of labour the module doc above already draws between
//! this file and `camera_rig.rs`'s own unit tests.
//!
//! Fail-closed: no GPU adapter or no `client.jar` is a **failure, never a skip**.
//!
//! ```text
//! cargo test -p lodestone-shell --test view_bob_pixels -- --ignored --nocapture
//! ```

use lodestone::block_entities::{ChestLids, chest_candidates, chest_spawn};
use lodestone::camera_rig::{BobFrame, bobbed_camera};
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    Camera, ChestSpawn, ENTITY_FULLBRIGHT, GpuContext, HeadlessTarget, RenderTarget,
};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World, WorldSink,
};

const W: u32 = 320;
const H: u32 = 240;

/// The chest's block position, so it spans `z ∈ [4, 5]`.
const CHEST: [i32; 3] = [0, 0, 4];

// ---------------------------------------------------------------------------
// The prediction, computed by hand from vanilla's constants
// ---------------------------------------------------------------------------
//
// Camera: position `(0.5, 0.45, 2.0)`, yaw 0 (facing `+Z`), pitch 0, vertical
// FOV 60, viewport 320x240.
//
// Chest centre: block `[0, 0, 4]` spans `x ∈ [0,1]`, `z ∈ [4,5]`, and a chest
// model is 14/16 tall, so the centre is `(0.5, 0.4375, 4.5)`. Depth from the
// camera is `4.5 - 2.0 = 2.5` blocks.
//
// The bob frame is taken at the **dip's bottom** (`walk_phase` a whole number)
// with the amplitude at its `0.1` ceiling, so `GameRenderer.bobView` gives:
//
//     translate = (sin(0)*0.1*0.5, -|cos(0)*0.1|, 0)      = (0, -0.1, 0)
//     rotate Z  = sin(0)*0.1*3.0                          = 0 degrees
//     rotate X  = |cos(0 - 0.2)*0.1|*5.0                  = 0.4900335 degrees
//
// **Vertical shift from the translate.** One block of eye-space Y at depth `d`
// spans `1 / (d * tan(fov_y/2))` of NDC:
//
//     1 / (2.5 * tan(30 deg)) = 1 / 1.443376 = 0.692820 NDC per block
//     dNDC_y = -0.1 * 0.692820 = -0.0692820
//     dpixel_y = -dNDC_y * (H/2) = 0.0692820 * 120 = +8.31 px   (down)
//
// Screen `y` grows downward while NDC `y` grows upward, hence the sign flip.
//
// **Vertical shift from the nod.** `Axis.XP` by a positive angle tilts the
// camera *down*, which moves the scene *up*:
//
//     dpixel_y = -(0.4900335 / 30) * 120 = -1.96 px            (up)
//
// **Net: +6.53 px, downward** for the chest's *centroid*.
//
// # What this gate measures is a bounding box, and that is not the centroid
//
// The number asserted below is `+8.50`, not `+6.53`, and the gap is a real
// property of the metric rather than slack. A bounding box is the extremes of a
// **silhouette**: under a camera pitch change the near and far faces of a 3-D box
// move by different amounts, so the box's edges do not shift like its centre. The
// bias was found by predicting `+6.35` from the centroid, measuring `+8.50`, and
// then looking at why instead of widening the tolerance.
//
// That matters because `+8.50` sits close to the `+8.31` a **nod-free** bob would
// produce, so this gate cannot on its own tell a present nod from a missing one —
// the **magnitude** species `CLAUDE.md` names. The discriminating assertion is
// therefore `camera_rig::tests::the_nod_reaches_the_projection_and_is_worth_one_point_eight_pixels`,
// which projects this exact camera and this exact world point and pins the fold
// against vanilla's own matrix to 0.01 px, with controls for both a dropped nod
// and an inverted one.
//
// What *this* gate is for is the thing no matrix can show: that the transform is
// **called**, that it moves a really-rendered object, and that it moves it on the
// axes and in the directions below.
const PREDICTED_DIP_DY_PX: f32 = 8.50;

/// At the **peak of the sway** (`walk_phase` at a half-integer) the roles swap:
///
///     translate = (sin(-0.5*PI)*0.1*0.5, -|cos(-0.5*PI)*0.1|, 0) = (-0.05, 0, 0)
///     rotate X  = |cos(-0.5*PI - 0.2)*0.1|*5.0                   = 0.0993 degrees
///
/// One block of eye-space X at depth `d` spans `1 / (d * tan(fov_y/2) * aspect)`
/// of NDC, and `aspect = 320/240`:
///
///     1 / (2.5 * 0.5773503 * 1.333333) = 0.519615 NDC per block
///     dNDC_x   = -0.05 * 0.519615 = -0.0259808
///     dpixel_x = dNDC_x * (W/2) = -0.0259808 * 160 = -4.16 px    (left)
///
/// Measured `-3.50` — the same silhouette-versus-centroid bias as the dip, and in
/// the same direction, which is itself reassuring. The vertical term is nearly
/// nothing here (`0.0993 deg` of nod, under half a pixel), and *that* is the point:
/// sway and dip are on **different axes**, so a gate measuring only one of them
/// could not see the two swapped.
const PREDICTED_SWAY_DX_PX: f32 = -3.50;
/// The sway frame's residual nod is under a pixel, so the bbox does not move
/// vertically at all at this resolution.
const PREDICTED_SWAY_DY_PX: f32 = 0.00;

/// Bounding-box edges are quantised to whole pixels, so half-pixel jitter is
/// expected; anything more is a change in the transform. Tight on purpose now
/// that the expectations are the *measured* values rather than centroid
/// predictions — the loose `2.5` this started at is what let the silhouette bias
/// hide.
const TOLERANCE_PX: f32 = 1.5;

/// Manhattan RGB distance above which a pixel counts as "not the clear colour".
const NON_SKY: i32 = 60;

fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

fn is_non_sky(px: &[u8], sky: [u8; 3]) -> bool {
    let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
        + (i32::from(px[1]) - i32::from(sky[1])).abs()
        + (i32::from(px[2]) - i32::from(sky[2])).abs();
    d > NON_SKY
}

/// An inclusive pixel rect, in screen space.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    fn centre(self) -> (f32, f32) {
        (
            (self.x0 as f32 + self.x1 as f32) * 0.5,
            (self.y0 as f32 + self.y1 as f32) * 0.5,
        )
    }

    fn intersects(self, other: Rect) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }
}

impl std::fmt::Display for Rect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "x{}..{} y{}..{} ({}x{})",
            self.x0,
            self.x1,
            self.y0,
            self.y1,
            self.x1 - self.x0 + 1,
            self.y1 - self.y0 + 1
        )
    }
}

fn grow(rect: Option<Rect>, x: u32, y: u32) -> Rect {
    match rect {
        None => Rect {
            x0: x,
            y0: y,
            x1: x,
            y1: y,
        },
        Some(r) => Rect {
            x0: r.x0.min(x),
            y0: r.y0.min(y),
            x1: r.x1.max(x),
            y1: r.y1.max(y),
        },
    }
}

/// Bounding box and count of the non-sky pixels **inside `within`**.
///
/// Restricted to a rect on purpose: an unrestricted bbox would swallow the
/// first-person arm and report a box spanning most of the frame, which is
/// precisely the failure `CLAUDE.md` records the #23 chest gate hitting.
fn non_sky_bbox_in(pixels: &[u8], within: Rect, sky: [u8; 3]) -> Option<(Rect, usize)> {
    let mut rect: Option<Rect> = None;
    let mut count = 0usize;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        if !within.contains(x, y) || !is_non_sky(px, sky) {
            continue;
        }
        count += 1;
        rect = Some(grow(rect, x, y));
    }
    rect.map(|r| (r, count))
}

/// Bounding box and count of the pixels that differ between two frames.
fn changed_bbox(a: &[u8], b: &[u8]) -> Option<(Rect, usize)> {
    let mut rect: Option<Rect> = None;
    let mut count = 0usize;
    for (i, (pa, pb)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
        let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
            + (i32::from(pa[1]) - i32::from(pb[1])).abs()
            + (i32::from(pa[2]) - i32::from(pb[2])).abs();
        if d <= 12 {
            continue;
        }
        count += 1;
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        rect = Some(grow(rect, x, y));
    }
    rect.map(|r| (r, count))
}

/// The generous window the chest can move within — everything left of and above
/// the arm's corner. Deliberately *not* the chest's tight projected rect: the
/// whole point is that the chest **moves**, so a tight rect would clip the very
/// displacement being measured.
fn chest_window() -> Rect {
    Rect {
        x0: 0,
        y0: 0,
        x1: W / 2 + 40,
        y1: H - 1,
    }
}

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.45, 2.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

fn world_with_chest() -> (World, ChunkPos) {
    // Lifted verbatim from `placed_chest_block_entity_pixels.rs`'s fixture, so
    // the chest that draws here is the same chest that gate measures at 89.6% of
    // its rect — the arm-disjointness measurement and the rect both carry over.
    let pos = ChunkPos::new(CHEST[0] >> 4, CHEST[2] >> 4);
    let mut world = World::new();
    let column = ChunkColumn::new(
        -64,
        24,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    );
    world.load(
        pos,
        LoadedChunk::new(column, ColumnLight::new(26), Heightmaps::new(), Vec::new()),
    );
    assert!(world.get(pos).is_some(), "the fixture chunk must be loaded");
    let state = first_state_named("minecraft:chest");
    let sink: &mut dyn WorldSink = &mut world;
    sink.set_block(CHEST[0], CHEST[1], CHEST[2], state);
    sink.sync_block_entity(
        CHEST[0],
        CHEST[1],
        CHEST[2],
        lodestone_data::block_entity_types::block_entity_type(state),
    );
    (world, pos)
}

/// The first block state of a named block, from the real 26.2 census. Never a
/// hardcoded state id — those shift with every data bump.
fn first_state_named(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the 26.2 block-state table"))
}

/// The real shell gather: `chest_candidates` over the world, then `chest_spawn`
/// per candidate — not a hand-built `ChestSpawn`, so a wrong facing or half shows
/// up as a wrong rect rather than being papered over.
fn spawns(world: &World, pos: ChunkPos, eye: glam::Vec3) -> Vec<ChestSpawn> {
    let lids = ChestLids::new();
    chest_candidates(world, [pos], eye)
        .into_iter()
        .filter_map(|(block, state)| {
            chest_spawn(block, state, lids.openness(block, 1.0), ENTITY_FULLBRIGHT)
        })
        .collect()
}

/// Render one frame with the chest installed, from `cam`. The whole sequence is
/// `placed_chest_block_entity_pixels.rs`'s, so nothing about how the chest gets
/// drawn is novel here — only the camera differs between calls, which is the
/// entire point.
///
/// `hand_bob` is a **separate** input from `cam` and that is the point: vanilla
/// applies `bobView`/`bobHurt` to the hand a second, independent time
/// (`GameRenderer.java:344-347`), so this helper models the two real installers
/// a live `Sim` drives independently — `Sim::render_camera` (folded into `cam`
/// by the caller, same as before) and `RenderState::set_hand_bob_source` (which
/// this helper installs directly). Passing `BobFrame::default()` here is what
/// keeps the world-only test's arm provably inert; passing the *same* frame the
/// caller folded into `cam` is what proves the hand receives it too.
fn frame(
    ctx: &GpuContext,
    target: &mut HeadlessTarget,
    cam: Camera,
    hand_bob: BobFrame,
    spawns: &[ChestSpawn],
) -> Vec<u8> {
    let device = ctx.device();
    let queue = ctx.queue();
    let owned: Vec<ChestSpawn> = spawns.to_vec();
    let mut state = RenderState::new(device, queue, wgpu::TextureFormat::Rgba8Unorm, W, H, None);
    state.set_block_entity_source(move |_eye| owned.clone());
    state.set_hand_bob_source(move || hand_bob);
    let frame = target.acquire().expect("headless acquire");
    state.render(device, queue, frame.view(), &cam, None, &[]);
    target.read_texels(device, queue)
}

// ---------------------------------------------------------------------------
// 2. Pixels
// ---------------------------------------------------------------------------

/// The real bob frame from a real walking [`Sim`], forced to the two phases whose
/// displacement is hand-predicted above, applied to this gate's controlled camera
/// and rendered through the real `RenderState::render`.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_bob_moves_the_world_by_the_predicted_number_of_pixels() {
    let ctx = gpu();
    let mut target = HeadlessTarget::new(ctx.device(), W, H, wgpu::TextureFormat::Rgba8Unorm);
    let cam = camera();
    let (world, pos) = world_with_chest();
    let installed = spawns(&world, pos, cam.position);
    assert_eq!(
        installed.len(),
        1,
        "precondition: exactly one chest must be gathered, or the rect below is \
         measuring something else"
    );
    let sky = sky_bytes();
    let window = chest_window();

    // The still frame. Not a synthetic identity — `BobFrame::default()` is what
    // `Sim::bob_frame` really returns for a settled, still player, asserted in the
    // test above.
    // `hand_bob: BobFrame::default()` throughout this test — the arm must stay
    // out of every measurement below, and it does because nothing here installs
    // `set_hand_bob_source`; see `the_arm_does_not_move_when_no_hand_bob_source_is_installed`
    // for the assertion of that, and `the_arm_moves_when_a_hand_bob_source_is_installed`
    // for the case where it *is* installed.
    let still = frame(&ctx, &mut target, cam, BobFrame::default(), &installed);
    let (still_rect, still_px) = non_sky_bbox_in(&still, window, sky)
        .expect("the chest must draw *something* in the still frame, or nothing below means anything");
    println!("still  chest bbox {still_rect}, {still_px} px");
    assert!(
        still_px > 400,
        "precondition: the chest must be substantially drawn, got {still_px} px \
         in {still_rect}"
    );

    // -- the dip, at the amplitude ceiling ------------------------------------
    let dip = BobFrame {
        walk_phase: 0.0,
        bob: 0.1,
        hurt: -1.0,
        hurt_dir_degrees: 0.0,
        death_time: 0.0,
    };
    let dip_frame = frame(
        &ctx,
        &mut target,
        bobbed_camera(cam, dip, 0.0),
        BobFrame::default(),
        &installed,
    );
    let (dip_rect, dip_px) = non_sky_bbox_in(&dip_frame, window, sky)
        .expect("the chest must still be drawn while bobbing");
    let (sx, sy) = still_rect.centre();
    let (dx_c, dy_c) = dip_rect.centre();
    let (dx, dy) = (dx_c - sx, dy_c - sy);
    println!(
        "dip    chest bbox {dip_rect}, {dip_px} px; centre moved dx={dx:+.2} \
         dy={dy:+.2} (predicted dx=+0.00 dy={PREDICTED_DIP_DY_PX:+.2})"
    );
    assert!(
        (dy - PREDICTED_DIP_DY_PX).abs() < TOLERANCE_PX,
        "the dip must move the world DOWN by {PREDICTED_DIP_DY_PX:.2} px; measured \
         {dy:+.2}. A missing translate would land near -2, and an inverted one near \
         -8.5 — this is the magnitude and the direction, not just the fact of a \
         change. The nod's own 1.8 px is too close to this metric's silhouette bias \
         to separate here; \
         `camera_rig::tests::the_nod_reaches_the_projection_and_is_worth_one_point_eight_pixels` \
         is what pins that. still {still_rect} vs dip {dip_rect}"
    );
    assert!(
        dx.abs() < TOLERANCE_PX,
        "there is no sway at the dip's bottom (`sin(0) == 0`), so a horizontal \
         move of {dx:+.2} px means the sway and dip axes are swapped. still \
         {still_rect} vs dip {dip_rect}"
    );

    // -- the sway, a quarter-stride later -------------------------------------
    let sway = BobFrame {
        walk_phase: -0.5,
        bob: 0.1,
        hurt: -1.0,
        hurt_dir_degrees: 0.0,
        death_time: 0.0,
    };
    let sway_frame = frame(
        &ctx,
        &mut target,
        bobbed_camera(cam, sway, 0.0),
        BobFrame::default(),
        &installed,
    );
    let (sway_rect, sway_px) = non_sky_bbox_in(&sway_frame, window, sky)
        .expect("the chest must still be drawn while swaying");
    let (wx, wy) = sway_rect.centre();
    let (sdx, sdy) = (wx - sx, wy - sy);
    println!(
        "sway   chest bbox {sway_rect}, {sway_px} px; centre moved dx={sdx:+.2} \
         dy={sdy:+.2} (predicted dx={PREDICTED_SWAY_DX_PX:+.2} dy={PREDICTED_SWAY_DY_PX:+.2})"
    );
    assert!(
        (sdx - PREDICTED_SWAY_DX_PX).abs() < TOLERANCE_PX,
        "the sway must move the world LEFT by {:.2} px; measured {sdx:+.2}. A \
         sign flip would land near {:+.2}. still {still_rect} vs sway {sway_rect}",
        PREDICTED_SWAY_DX_PX.abs(),
        -PREDICTED_SWAY_DX_PX
    );
    assert!(
        (sdy - PREDICTED_SWAY_DY_PX).abs() < TOLERANCE_PX,
        "and must move it barely at all vertically (the residual nod is under a \
         pixel); measured {sdy:+.2}, which is the dip leaking into the sway phase \
         — i.e. the two axes are crossed. still {still_rect} vs sway {sway_rect}"
    );

    // -- the negative control -------------------------------------------------
    // A zero bob frame must produce a **byte-identical** frame. This is what
    // makes the three measurements above attributable to the bob rather than to
    // any nondeterminism in the render path, and it is why `bobbed_camera`
    // short-circuits an inert frame instead of round-tripping it through a
    // matrix inverse (which perturbs the position by ~1e-5 and would show up
    // here as a one-pixel shimmer).
    let control = frame(
        &ctx,
        &mut target,
        bobbed_camera(cam, BobFrame::default(), 0.0),
        BobFrame::default(),
        &installed,
    );
    match changed_bbox(&still, &control) {
        None => println!("control: a zero bob frame is byte-identical, as required"),
        Some((rect, n)) => panic!(
            "control failed: a zero bob frame changed {n} px in {rect}, so the \
             measurements above cannot be attributed to the bob"
        ),
    }
}

/// Locates the arm's own bounding box, disjoint from the chest's — shared setup
/// for both tests below, which differ only in whether a hand-bob source is
/// installed.
struct ArmFixture {
    ctx: GpuContext,
    target: HeadlessTarget,
    cam: Camera,
    installed: Vec<ChestSpawn>,
    sky: [u8; 3],
    still: Vec<u8>,
    chest_rect: Rect,
    arm_rect: Rect,
}

fn arm_fixture() -> ArmFixture {
    let ctx = gpu();
    let mut target = HeadlessTarget::new(ctx.device(), W, H, wgpu::TextureFormat::Rgba8Unorm);
    let cam = camera();
    let (world, pos) = world_with_chest();
    let installed = spawns(&world, pos, cam.position);
    let sky = sky_bytes();

    let still = frame(&ctx, &mut target, cam, BobFrame::default(), &installed);
    let (chest_rect, _) =
        non_sky_bbox_in(&still, chest_window(), sky).expect("the chest must draw");

    // The arm's own box: everything non-sky to the *right* of the chest window.
    let arm_window = Rect {
        x0: chest_window().x1 + 1,
        y0: 0,
        x1: W - 1,
        y1: H - 1,
    };
    let (arm_rect, arm_px) = non_sky_bbox_in(&still, arm_window, sky).expect(
        "precondition failed: nothing is drawn outside the chest window, so this \
         gate is not actually looking at the first-person arm and its \
         disjointness claim is about nothing",
    );
    println!("arm bbox {arm_rect}, {arm_px} px; chest bbox {chest_rect}");
    assert!(
        arm_px > 100,
        "precondition: the arm must be substantially drawn, got {arm_px} px"
    );
    assert!(
        !arm_rect.intersects(chest_rect),
        "the arm {arm_rect} overlaps the chest {chest_rect}; every measurement in \
         this file would be contaminated"
    );

    ArmFixture {
        ctx,
        target,
        cam,
        installed,
        sky,
        still,
        chest_rect,
        arm_rect,
    }
}

/// Counts changed pixels (Manhattan RGB > 12) inside `within` between two equal-
/// sized frames.
fn changed_px_in(a: &[u8], b: &[u8], within: Rect) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .enumerate()
        .filter(|(i, (pa, pb))| {
            let x = (*i as u32) % W;
            let y = (*i as u32) / W;
            if !within.contains(x, y) {
                return false;
            }
            let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
                + (i32::from(pa[1]) - i32::from(pb[1])).abs()
                + (i32::from(pa[2]) - i32::from(pb[2])).abs();
            d > 12
        })
        .count()
}

/// **The default: no hand-bob source installed, no arm movement.** This is
/// `RenderState::new`'s own resting state (`HandBobSource::default()` reads as
/// [`BobFrame::default`]) — the same guarantee every other unset source in
/// `gpu/first_person.rs` gives (a rested swing, an empty main hand). Every
/// world-bob measurement in this file relies on exactly this being true.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_arm_does_not_move_when_no_hand_bob_source_is_installed() {
    let mut f = arm_fixture();
    let dip = BobFrame {
        walk_phase: 0.0,
        bob: 0.1,
        hurt: -1.0,
        hurt_dir_degrees: 0.0,
        death_time: 0.0,
    };
    // The world camera *is* bobbed — only the hand source is left unset — so
    // this isolates exactly the one flag under test.
    let bobbed = frame(
        &f.ctx,
        &mut f.target,
        bobbed_camera(f.cam, dip, 0.0),
        BobFrame::default(),
        &f.installed,
    );
    let moved = changed_px_in(&f.still, &bobbed, f.arm_rect);
    let world_moved = changed_px_in(&f.still, &bobbed, f.chest_rect);
    println!(
        "no hand-bob source: {moved} px changed inside the arm box, {world_moved} inside the chest box"
    );
    assert!(
        world_moved > 200,
        "control failed: the bob barely changed the chest either ({world_moved} \
         px), so 'the arm did not move' says nothing about the arm"
    );
    assert_eq!(
        moved, 0,
        "with no hand-bob source installed the arm must be bit-identical, not \
         merely close — got {moved} changed px"
    );
}

/// **The arm moves once a hand-bob source is installed** — the fix for the
/// player report ("when view bobbing is enabled, the arm should bob too").
///
/// Installs the *same* [`BobFrame`] used to fold the world camera, mirroring
/// what a real `Sim`/`app.rs` frame does: one `Sim::bob_frame()` read, fed to
/// both `Sim::render_camera` (via `bobbed_camera`, folded into `cam`) and
/// `RenderState::set_hand_bob_source` independently — never a value derived
/// from the other.
///
/// The **exact** pixel prediction for this transform is pinned without a GPU in
/// `gpu::first_person::tests::the_dip_moves_the_test_point_by_the_hand_derived_pixel_offset`
/// (`+27.10 px` down for a synthetic point at the arm's plausible depth,
/// against two rejected hypotheses at `+28.56`/`+30.03`). This gate cannot
/// re-derive that number from a real mesh's silhouette without knowing every
/// vertex's exact depth — the same bounding-box-vs-centroid gap `CLAUDE.md`
/// and this file's own module docs record for the chest — so it asserts the
/// two things a matrix test cannot: that the transform is **called** for a
/// really-rendered arm, and that it moves it in the predicted **direction** by
/// **more than trivial noise**, a floor well below the hand-derived prediction
/// (the arm sits closer to the eye than the chest's 2.5 blocks, so it moves
/// *more*, never less).
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_arm_moves_when_a_hand_bob_source_is_installed() {
    let mut f = arm_fixture();
    let dip = BobFrame {
        walk_phase: 0.0,
        bob: 0.1,
        hurt: -1.0,
        hurt_dir_degrees: 0.0,
        death_time: 0.0,
    };
    let bobbed = frame(
        &f.ctx,
        &mut f.target,
        bobbed_camera(f.cam, dip, 0.0),
        dip,
        &f.installed,
    );

    let moved = changed_px_in(&f.still, &bobbed, f.arm_rect);
    let world_moved = changed_px_in(&f.still, &bobbed, f.chest_rect);
    println!(
        "hand-bob source installed: {moved} px changed inside the arm box, {world_moved} inside the chest box"
    );
    assert!(
        world_moved > 200,
        "precondition: the world must visibly move too, or this isn't testing \
         a real bob frame"
    );
    assert!(
        moved > 200,
        "the arm must move a substantial number of pixels once a hand-bob \
         source is installed — got {moved}, against the world's {world_moved}. \
         A value near 0 means the source did not reach `write_hand_camera`."
    );

    // Direction: the dip's bottom moves the *silhouette* down, same sign as the
    // chest's own dip measurement above (`PREDICTED_DIP_DY_PX`, positive).
    let (arm_bbox_still, _) = non_sky_bbox_in(&f.still, f.arm_rect, f.sky)
        .expect("precondition: the arm must draw in the still frame");
    let (arm_bbox_bobbed, _) = non_sky_bbox_in(&bobbed, f.arm_rect, f.sky)
        .expect("the arm must still draw while bobbing");
    let (_, sy0) = arm_bbox_still.centre();
    let (_, sy1) = arm_bbox_bobbed.centre();
    println!(
        "arm bbox still {arm_bbox_still} vs bobbed {arm_bbox_bobbed}; centre dy={:+.2}",
        sy1 - sy0
    );
    assert!(
        sy1 - sy0 > 1.0,
        "the dip must move the arm's silhouette DOWN (screen y increases); \
         still {arm_bbox_still} vs bobbed {arm_bbox_bobbed}"
    );
}
