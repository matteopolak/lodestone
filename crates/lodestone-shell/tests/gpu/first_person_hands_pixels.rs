//! Pixel gates for the first-person hands: the **off hand** draws its stack,
//! mirrored, on the left of the screen and draws nothing when empty; the
//! main hand's **attack-cooldown lowering** moves the held item down by the
//! predicted amount.
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

use lodestone::gpu::{FirstPersonHandsFrame, HandFrame, MainHandItem, RenderState, SKY_COLOR};
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
