//! Pixel gates for the first-person hands: the **off hand** draws its stack,
//! mirrored, on the left of the screen and draws nothing when empty; the
//! main hand's **attack-cooldown lowering** moves the held item down by the
//! predicted amount; and, as a negative control, the main hand's **use poses**
//! (bow, crossbow, shield, eating) do not change with an off-hand stack.
//!
//! Every frame is pure sky plus the hand pass, at 448x256 (a 16:9-ish target:
//! the hand projection's FOV is vertical, so a square target crops the hands
//! out horizontally). Pixels are classified as "hand" when they differ from the
//! sky clear colour by more than a rounding wobble, and every measurement is a
//! bounding box or a column count over those pixels, so a failure prints where
//! the hand actually landed.
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test gpu first_person_hands_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{
    FirstPersonHandsFrame, HandFrame, ItemUseState, MainHandItem, RenderState, SKY_COLOR,
};
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 448;
const H: u32 = 256;

/// A flat extruded sprite whose silhouette is the same from either side, so a
/// mirrored pose must produce a mirrored silhouette.
const ITEM: &str = "minecraft:diamond_pickaxe";

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

fn held(id: &str) -> MainHandItem {
    MainHandItem {
        item: id.parse::<ResourceLocation>().expect("valid item id"),
        foil: false,
        custom_model_data: None,
        dyed_color: None,
        potion_color: None,
        banner_patterns: Vec::new(),
        base_color: None,
        skin: None,
    }
}

/// Inclusive pixel bounding box of the hand pixels, and their count.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Coverage {
    count: usize,
    min_x: u32,
    max_x: u32,
    min_y: u32,
    max_y: u32,
}

fn coverage(pixels: &[u8], keep: impl Fn(u32, u32) -> bool) -> Option<Coverage> {
    let sky = SKY_COLOR.map(|c| (c * 255.0).round() as i32);
    let mut cov: Option<Coverage> = None;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        if !keep(x, y) {
            continue;
        }
        let d = (i32::from(px[0]) - sky[0]).abs()
            + (i32::from(px[1]) - sky[1]).abs()
            + (i32::from(px[2]) - sky[2]).abs();
        if d <= 8 {
            continue;
        }
        let c = cov.get_or_insert(Coverage {
            count: 0,
            min_x: x,
            max_x: x,
            min_y: y,
            max_y: y,
        });
        c.count += 1;
        c.min_x = c.min_x.min(x);
        c.max_x = c.max_x.max(x);
        c.min_y = c.min_y.min(y);
        c.max_y = c.max_y.max(y);
    }
    cov
}

struct Rig {
    ctx: GpuContext,
    target: HeadlessTarget,
    state: RenderState,
}

impl Rig {
    fn new() -> Self {
        let ctx = GpuContext::new_headless_blocking().expect(
            "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU — do NOT treat a skip as a pass",
        );
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let target = HeadlessTarget::new(ctx.device(), W, H, format);
        let resources = BlockResources::load(true);
        let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
            panic!(
                "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS. \
                 Banner: {:?}",
                resources.banner
            )
        });
        let mut state =
            RenderState::new_headless(ctx.device(), ctx.queue(), format, W, H, Some(atlas.as_ref()));
        state.set_entity_light_source(|_| Some(lodestone_render::ENTITY_FULLBRIGHT));
        Self { ctx, target, state }
    }

    fn shoot(&mut self, frame: FirstPersonHandsFrame) -> (Vec<u8>, lodestone::gpu::RenderStats) {
        self.state.set_first_person_hands(frame);
        let frame = self.target.acquire().expect("headless acquire");
        let stats = self
            .state
            .render(self.ctx.device(), self.ctx.queue(), frame.view(), &camera(), None, &[]);
        (self.target.read_texels(self.ctx.device(), self.ctx.queue()), stats)
    }
}

fn frame(main: Option<MainHandItem>, off: Option<MainHandItem>) -> FirstPersonHandsFrame {
    let mut frame = FirstPersonHandsFrame::holding(main);
    frame.off = HandFrame::resting(off);
    frame
}

/// The off hand's stack draws on the **left** half, as the mirror image of the
/// same stack in the main hand; an empty off hand draws nothing there.
///
/// The prediction is symmetry, from the pose rule alone: the off hand is the
/// main hand's chain with the arm sign flipped and the left-hand display slot,
/// so for a flat sprite (identical silhouette from either face) the off-hand
/// bounding box is the main-hand one reflected about the screen's vertical
/// centre line, `x → W - 1 - x`, and unchanged vertically.
#[test]
#[ignore = "requires a GPU adapter"]
fn the_off_hand_draws_its_stack_mirrored_on_the_left() {
    let mut rig = Rig::new();
    let left = |x: u32, _y: u32| x < W / 2;
    let right = |x: u32, _y: u32| x >= W / 2;

    // Main hand only: the item on the right, nothing on the left.
    let (main_only, stats) = rig.shoot(frame(Some(held(ITEM)), None));
    assert!(stats.first_person_item_drawn && !stats.first_person_off_hand_drawn);
    let main_box = coverage(&main_only, right).expect("the main-hand item must draw on the right");
    let main_left = coverage(&main_only, left);
    eprintln!("main only: right {main_box:?}, left {main_left:?}");
    assert!(main_left.is_none(), "nothing may draw on the left with an empty off hand: {main_left:?}");

    // Off hand holding the same stack, main hand empty: the bare arm on the
    // right, and the item on the left.
    let (off_too, stats) = rig.shoot(frame(None, Some(held(ITEM))));
    assert!(stats.first_person_arm_drawn, "the empty main hand draws the bare arm");
    assert!(stats.first_person_off_hand_drawn, "the off hand must report a draw");
    let off_box = coverage(&off_too, left).expect(
        "the off-hand item must draw on the left half — nothing landed there",
    );
    eprintln!("off hand: left {off_box:?}");

    let mirrored = (W - 1 - main_box.max_x, W - 1 - main_box.min_x);
    assert!(
        off_box.min_x.abs_diff(mirrored.0) <= 2
            && off_box.max_x.abs_diff(mirrored.1) <= 2
            && off_box.min_y.abs_diff(main_box.min_y) <= 2
            && off_box.max_y.abs_diff(main_box.max_y) <= 2,
        "the off-hand box {off_box:?} must mirror the main-hand box {main_box:?} \
         (expected x {mirrored:?}, y {}..={})",
        main_box.min_y,
        main_box.max_y
    );
    assert!(
        off_box.count.abs_diff(main_box.count) * 50 <= main_box.count,
        "mirrored silhouettes cover the same area within 2%: off {} vs main {}",
        off_box.count,
        main_box.count
    );

    // Control: an empty off hand next to an empty main hand draws only the
    // arm, on the right.
    let (arm_only, stats) = rig.shoot(frame(None, None));
    assert!(stats.first_person_arm_drawn && !stats.first_person_off_hand_drawn);
    let arm_left = coverage(&arm_only, left);
    assert!(arm_left.is_none(), "an empty off hand must draw nothing: {arm_left:?}");
}

/// The off hand is lowered by its own height, independently of the main hand,
/// and is depth-tested in the same cleared pass: lowering it by `h` moves its
/// top edge down while the main hand's box stays put.
#[test]
#[ignore = "requires a GPU adapter"]
fn the_off_hand_lowers_on_its_own() {
    let mut rig = Rig::new();
    let left = |x: u32, _y: u32| x < W / 2;
    let right = |x: u32, _y: u32| x >= W / 2;

    let (rest, _) = rig.shoot(frame(Some(held(ITEM)), Some(held(ITEM))));
    let mut lowered = frame(Some(held(ITEM)), Some(held(ITEM)));
    lowered.off.inverse_arm_height = 0.2;
    let (low, _) = rig.shoot(lowered);

    let rest_off = coverage(&rest, left).expect("off hand at rest");
    let low_off = coverage(&low, left).expect("off hand lowered");
    let rest_main = coverage(&rest, right).expect("main hand");
    let low_main = coverage(&low, right).expect("main hand");
    eprintln!("off rest {rest_off:?} lowered {low_off:?}; main {rest_main:?} / {low_main:?}");
    assert_eq!(rest_main, low_main, "lowering the off hand must not move the main hand");
    assert!(
        low_off.min_y > rest_off.min_y + 5,
        "a 0.2 lowering (0.12 blocks) must move the off hand's top edge down by several \
         pixels: rest {rest_off:?}, lowered {low_off:?}"
    );
}

/// Heights `k` ticks after an attack with a `1.6` attack speed — a sword:
/// the delay is `20 / 1.6 = 12.5` ticks and the resting height is the cubed
/// scale `((k + 1) / 12.5)³`, approached at most `0.4` a tick from `1.0`.
/// Only the ticks where the item is on screen are listed: tick 1 (`0.6`, the
/// first step down), then the climb back from tick 7. Ticks 2–6 put the
/// item almost wholly below the screen's bottom edge, outside the segment
/// the calibration below measures. The `Sim` witness proves the production tick produces these
/// heights; this gate proves the pixels follow them.
const COOLDOWN_HEIGHTS: [(u32, f32); 5] = [
    (1, 0.6),
    (7, 0.373248),  // 0.72³
    (8, 0.512),     // 0.8³
    (9, 0.681472),  // 0.88³
    (10, 0.884736), // 0.96³
];

/// The attack-cooldown dip moves the held item down the screen by the
/// predicted amount.
///
/// The prediction comes from the projection alone. The lowering is a pure
/// camera-space vertical translation `-0.6·(1 - h)`, which leaves every
/// vertex's depth unchanged, so each vertex's projected `y` moves linearly in
/// the lowering, and the item's visible top edge — the highest of those
/// vertices and of the right screen edge's cut through the item — is
/// piecewise linear. At rest the top is the tip cut by the screen edge, which
/// leaves the linear segment within the first `0.05`; from `0.1` on it is one
/// segment (measured: the `0.1`/`0.3` line predicts `0.4`, `0.5` and `0.6` to
/// within half a pixel). Two calibration frames at lowerings `0.1` and `0.3`,
/// neither of them a cooldown height, fix that line; every listed cooldown
/// tick must then land on it, and the horizontal extent must not move.
///
/// The ticks discriminate: an uncubed curve (`h = scale`) would put tick 7 at
/// lowering `0.36` instead of `0.627`, about 39 px higher; no dip at all
/// leaves every tick at rest.
#[test]
#[ignore = "requires a GPU adapter"]
fn the_cooldown_dip_lowers_the_held_item_by_the_predicted_amount() {
    let mut rig = Rig::new();
    let right = |x: u32, _y: u32| x >= W / 2;
    let main_at = |rig: &mut Rig, lowering: f32| {
        let mut frame = frame(Some(held(ITEM)), None);
        frame.main.inverse_arm_height = lowering;
        let (pixels, stats) = rig.shoot(frame);
        assert!(stats.first_person_item_drawn);
        coverage(&pixels, right)
    };

    let rest = main_at(&mut rig, 0.0).expect("the held item must draw at rest");
    let low = main_at(&mut rig, 0.1).expect("the held item must draw lowered by 0.1");
    let lower = main_at(&mut rig, 0.3).expect("the held item must draw lowered by 0.3");
    let slope = (lower.min_y as f32 - low.min_y as f32) / 0.2;
    let top_at = |lowering: f32| low.min_y as f32 + slope * (lowering - 0.1);
    eprintln!("rest {rest:?}; 0.1 {low:?}; 0.3 {lower:?}; {slope} px per unit");
    assert!(slope > 50.0, "precondition: lowering must move the item visibly: {slope}");

    for (tick, height) in COOLDOWN_HEIGHTS {
        let lowering = 1.0 - height;
        let got = main_at(&mut rig, lowering).unwrap_or_else(|| {
            panic!("tick {tick} after the attack (height {height}): the held item vanished")
        });
        let want_top = top_at(lowering);
        eprintln!("tick {tick}: lowering {lowering}, predicted top {want_top}, got {got:?}");
        assert!(
            (got.min_y as f32 - want_top).abs() <= 2.0,
            "tick {tick} after the attack (height {height}): the item's top edge must sit at \
             y {want_top} ({} + {slope}·({lowering} - 0.1)), got {got:?}",
            low.min_y
        );
        assert!(
            got.min_x.abs_diff(rest.min_x) <= 1,
            "the dip is vertical only: rest {rest:?}, tick {tick} {got:?}"
        );
    }
}

/// Negative control for the use poses: what the main hand draws while
/// drawing a bow, loading a crossbow, blocking with a shield or eating is
/// independent of the off hand. Each pose's right half is byte-identical with
/// the off hand empty and with it holding a stack; with a stack, the off hand
/// lands on the left half — except while a bow is drawn or a crossbow loads,
/// when the frame hides it (`drawn: false`, as `Sim`'s hand selection sets it) and the left half
/// stays sky.
#[test]
#[ignore = "requires a GPU adapter"]
fn the_main_hand_use_poses_ignore_the_off_hand() {
    let mut rig = Rig::new();
    // Pixels that differ between two frames, counted per half.
    let differing = |a: &[u8], b: &[u8], keep: &dyn Fn(u32) -> bool| {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .enumerate()
            .filter(|(i, (p, q))| keep((*i as u32) % W) && p != q)
            .count()
    };
    let poses: [(&str, ItemUseState, bool); 5] = [
        ("minecraft:bow", ItemUseState { using: true, ticks: 12, eat: None }, true),
        ("minecraft:crossbow", ItemUseState { using: true, ticks: 10, eat: None }, true),
        ("minecraft:shield", ItemUseState { using: true, ticks: 10, eat: None }, false),
        ("minecraft:bread", ItemUseState { using: true, ticks: 20, eat: Some((11.5, 32)) }, false),
        ("minecraft:iron_sword", ItemUseState::default(), false),
    ];
    for (item, use_state, hides_off) in poses {
        rig.state.set_item_use_source(move || use_state);
        let (alone, _) = rig.shoot(frame(Some(held(item)), None));
        let mut with_off = frame(Some(held(item)), Some(held("minecraft:totem_of_undying")));
        with_off.off.drawn = !hides_off;
        let (beside, stats) = rig.shoot(with_off);
        assert!(stats.first_person_item_drawn, "{item}: the main hand must draw");
        let right_moved = differing(&alone, &beside, &|x| x >= W / 2);
        assert_eq!(right_moved, 0, "{item}: the main-hand pose moved with the off hand present");
        // The main item can spill into the left half (a raised shield, food at
        // the mouth), so the off hand is detected as a change there, not as
        // any coverage at all.
        let left_changed = differing(&alone, &beside, &|x| x < W / 2);
        eprintln!("{item}: {left_changed} left-half pixels changed by the off-hand stack");
        assert_eq!(stats.first_person_off_hand_drawn, !hides_off, "{item}: off-hand draw flag");
        assert_eq!(
            left_changed > 0,
            !hides_off,
            "{item}: the off-hand stack must change the left half exactly when it is shown"
        );
    }
}
