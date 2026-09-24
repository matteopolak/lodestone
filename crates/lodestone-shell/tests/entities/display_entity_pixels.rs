//! Pixel gate: a **`block_display`** and an **`item_display`** reach pixels.
//!
//! # What this proves, and what it does not
//!
//! It proves the *renderer*: given a `DisplayDraw` installed through
//! `RenderState::set_display_draws` — which is exactly the call
//! `app::redraw` makes every frame — the block-model and item-model producers
//! in `gpu/moving_blocks.rs` and `gpu/world_items.rs` put geometry on screen.
//! Until those producers landed, both subtypes resolved all the way to a
//! draw-ready snapshot and then dropped off the edge of the pipeline, and the
//! setter logged a warning saying so.
//!
//! It does **not** prove the producer of that input. This gate builds its own
//! `DisplayDraw`; production derives one from the ECS
//! (`display_entities::extract_display_draws`) off metadata decoded from the
//! wire. That half is gated separately and in two places — `display_entities`'s
//! own extract tests for components → snapshot, and
//! `lodestone_v26_2::packets::metadata`'s `an_item_displays_stack_arrives_at_index_23…`
//! and `index_16_int_is_a_brightness_override_only_for_a_display` for wire →
//! metadata. This repo has shipped three fixes in one day with a passing pixel
//! gate and a still-broken game for exactly the gap this paragraph names, so
//! the split is stated rather than implied.
//!
//! # The arms, and what each separates
//!
//! | arm | what it can see that the others cannot |
//! |---|---|
//! | nothing installed → `block_display` | the block producer exists at all |
//! | nothing installed → `item_display` | the item producer, a different pipeline path from the block one |
//! | `block_display` with no reported state | absence really is "draw nothing", not a stand-in block |
//! | `item_display` with no reported stack | same, on the other producer |
//!
//! The two "no reported payload" arms are the controls that stop the first two
//! from being satisfied by *any* installed draw painting *something*: they
//! install a real display entity at the same position and must come back
//! byte-identical to the empty scene.
//!
//! # Watched to fail
//!
//! Both producers were neutered in turn — the `merge_block_displays` and
//! `merge_item_displays` calls replaced in `prepare_moving_blocks` /
//! `prepare_item_geometry`, then restored from an md5-checked backup — and
//! each arm was observed red **on its own run**, because an `assert!` that
//! fires stops the arms after it from reporting anything:
//!
//! | neutered | this file reported |
//! |---|---|
//! | both | `moving_blocks_drawn` 0 against 1, and the scale gate's `0 px` |
//! | the item producer alone | `a diamond item_display changed only 0 px in (320, 240)..(0, 0)` |
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test display_entity_pixels -- --ignored --nocapture
//! ```

use lodestone::display_entities::{
    BLOCK_DISPLAY_TYPE_PATH, DisplayDraw, ITEM_DISPLAY_TYPE_PATH,
};
use lodestone::gpu::RenderState;
use lodestone::resources::BlockResources;
use lodestone_model::BlockStateRef;
use lodestone_render::display::{BillboardMode, DisplayTransformation};
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// A `block_display`'s position is its model's **corner**, not its centre, so
/// this puts the cube's near-bottom-left corner here and its far-top-right at
/// `(1, 1, 4)` — squarely in front of the camera below.
const SUBJECT_POS: glam::Vec3 = glam::Vec3::new(-0.5, -0.5, 3.0);

/// A solid, fully-opaque, unambiguously-textured block. `stone` rather than a
/// glass or a plant so a single missing face cannot be mistaken for the whole
/// thing failing to draw.
const BLOCK: &str = "minecraft:stone";

/// A flat-sprite item with no `minecraft:special` form, so it can only reach
/// the screen through the ordinary item-model path this file gates.
const ITEM: &str = "minecraft:diamond";

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 0.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

/// A `DisplayDraw` at vanilla's own accessor defaults, carrying no payload —
/// the state an entity is in immediately after `ADD_ENTITY`, before any
/// `set_entity_data` has arrived.
fn blank_draw(id: i32, type_path: &'static str) -> DisplayDraw {
    DisplayDraw {
        id,
        type_path,
        position: SUBJECT_POS,
        entity_yaw: 0.0,
        entity_pitch: 0.0,
        billboard: BillboardMode::Fixed,
        transform: DisplayTransformation::default(),
        text: None,
        text_line_width: 200,
        text_background_color: 0,
        text_opacity: -1,
        text_style_flags: 0,
        block_state: None,
        item: None,
        item_display_context: 0,
        brightness_override: None,
    }
}

struct Diff {
    count: usize,
    min_x: u32,
    max_x: u32,
    min_y: u32,
    max_y: u32,
}

impl std::fmt::Debug for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} px in ({}, {})..({}, {})",
            self.count, self.min_x, self.min_y, self.max_x, self.max_y
        )
    }
}

/// Differing pixels between two frames, **with a bounding box** — a bare count
/// cannot tell a localised object from a uniform full-frame shift, and this
/// repo has been misled by exactly that.
///
/// The reference is a *rendered* frame rather than a hardcoded sky colour, for
/// the reason `docs/`-recorded hermetic pixel gates give: the real background
/// is a time-of-day-and-eye-height fog resolve under a sky disc, and a gate
/// that diffs against `SKY_COLOR` is blind.
fn diff(subject: &[u8], reference: &[u8]) -> Diff {
    let mut count = 0usize;
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (W, 0u32, H, 0u32);
    for (i, (a, b)) in subject
        .chunks_exact(4)
        .zip(reference.chunks_exact(4))
        .enumerate()
    {
        let d = (i32::from(a[0]) - i32::from(b[0])).abs()
            + (i32::from(a[1]) - i32::from(b[1])).abs()
            + (i32::from(a[2]) - i32::from(b[2])).abs();
        if d <= 8 {
            continue;
        }
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        count += 1;
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    Diff {
        count,
        min_x,
        max_x,
        min_y,
        max_y,
    }
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_block_display_and_an_item_display_reach_pixels() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS \
             to a pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });

    let state_id = lodestone_data::block_states::state_id(BLOCK)
        .unwrap_or_else(|| panic!("{BLOCK} has no state id in the generated block-state table"));

    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();

    // A fresh `RenderState` per shot, mirroring `text_display_pixels.rs`: the
    // setter installs a whole frame's list, so reusing one state between arms
    // would leave the previous arm's draws resident.
    let mut shoot = |draws: Vec<DisplayDraw>| -> (Vec<u8>, usize) {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        state.set_display_draws(draws);
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, &[]);
        (
            target.read_texels(device, queue),
            stats.moving_blocks_drawn,
        )
    };

    let (empty, empty_blocks) = shoot(Vec::new());
    assert_eq!(
        empty_blocks, 0,
        "an empty scene already counted a moving block"
    );

    // Arm 1: a `block_display` showing stone.
    let mut block = blank_draw(1, BLOCK_DISPLAY_TYPE_PATH);
    block.block_state = Some(BlockStateRef::canonical(state_id));
    let (block_px, block_count) = shoot(vec![block]);
    let block_diff = diff(&block_px, &empty);
    assert_eq!(
        block_count, 1,
        "the block-display producer did not reach `merge_moving_block`"
    );
    assert!(
        block_diff.count > 500,
        "a stone block_display filling the middle of the frame changed only \
         {block_diff:?} — it did not reach the screen"
    );

    // Arm 2: an `item_display` holding a diamond, at the same position.
    let mut item = blank_draw(2, ITEM_DISPLAY_TYPE_PATH);
    item.item = Some(lodestone_model::ItemStack::new(
        ITEM.parse().expect("valid item id"),
        1,
    ));
    let (item_px, item_count) = shoot(vec![item]);
    let item_diff = diff(&item_px, &empty);
    assert_eq!(
        item_count, 0,
        "an item_display went through the *block* seam; the two producers are \
         crossed"
    );
    assert!(
        item_diff.count > 100,
        "a diamond item_display changed only {item_diff:?} — it did not reach \
         the screen"
    );

    // The two producers are genuinely different geometry, not one counted
    // twice: a solid cube covers strictly more of the frame than a flat sprite
    // standing in the same place.
    assert!(
        block_diff.count > item_diff.count,
        "a solid stone cube ({block_diff:?}) did not cover more than a flat \
         diamond sprite ({item_diff:?}) at the same position, so the two arms \
         may be drawing the same thing"
    );

    // Control 1: a real `block_display` entity whose state has never been
    // reported must be byte-identical to the empty scene. Without this, arm 1
    // is satisfied by *any* installed draw painting anything at all.
    let (bare_block_px, bare_block_count) = shoot(vec![blank_draw(3, BLOCK_DISPLAY_TYPE_PATH)]);
    let bare_block_diff = diff(&bare_block_px, &empty);
    assert_eq!(bare_block_count, 0);
    assert_eq!(
        bare_block_diff.count, 0,
        "a block_display with no reported state painted {bare_block_diff:?}; \
         absence must draw nothing, not a stand-in"
    );

    // Control 2: the same, on the item producer.
    let (bare_item_px, _) = shoot(vec![blank_draw(4, ITEM_DISPLAY_TYPE_PATH)]);
    let bare_item_diff = diff(&bare_item_px, &empty);
    assert_eq!(
        bare_item_diff.count, 0,
        "an item_display with no reported stack painted {bare_item_diff:?}"
    );
}

/// A display's synced `Transformation` is really applied: doubling the scale
/// makes a `block_display` cover strictly more of the frame.
///
/// # Why this arm exists beside the one above
///
/// The gate above passes for a producer that ignores
/// [`DisplayDraw::placement`] entirely and draws at the bare entity position —
/// which is the shape of the bug this repo has shipped before, an inherited
/// transformation read on one node and dropped everywhere else. The scale is
/// the cheapest field to observe in pixels, and the prediction is directional
/// *and* bounded: a `2×` cube seen head-on subtends about four times the area,
/// so "more" is asserted with a floor well above noise rather than as a bare
/// inequality.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_block_displays_transformation_scale_changes_what_is_drawn() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let resources = BlockResources::load(true);
    let atlas = resources
        .vanilla_atlas
        .clone()
        .expect("the vanilla pack must load for this gate");
    let state_id = lodestone_data::block_states::state_id(BLOCK).expect("stone has a state id");

    let mut target = HeadlessTarget::new(device, W, H, format);
    let cam = camera();

    let mut shoot = |draws: Vec<DisplayDraw>| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        state.set_display_draws(draws);
        let frame = target.acquire().expect("headless acquire");
        let _ = state.render(device, queue, frame.view(), &cam, None, &[]);
        target.read_texels(device, queue)
    };

    let empty = shoot(Vec::new());

    let mut unit = blank_draw(1, BLOCK_DISPLAY_TYPE_PATH);
    unit.block_state = Some(BlockStateRef::canonical(state_id));
    let mut doubled = unit.clone();
    doubled.transform.scale = glam::Vec3::splat(2.0);

    let unit_diff = diff(&shoot(vec![unit]), &empty);
    let doubled_diff = diff(&shoot(vec![doubled]), &empty);

    assert!(
        unit_diff.count > 500,
        "the unit-scale arm drew almost nothing ({unit_diff:?}), so the \
         comparison below would be vacuous"
    );
    assert!(
        doubled_diff.count > unit_diff.count * 2,
        "doubling the transformation scale took the covered area from \
         {unit_diff:?} to {doubled_diff:?} — under 2x, so the synced \
         Transformation is not reaching the pose"
    );
}
