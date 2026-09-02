//! Pixel gate: container item icons at **GUI scale 2** — the blind spot
//! `container_item_pixels.rs` leaves open.
//!
//! Both GPU readback item-icon gates (`hotbar_block_item_pixels.rs` and
//! `container_item_pixels.rs`) run at `480x320`, where
//! `calculate_gui_scale(AUTO, 480, 320) == 1` and the logical<->physical
//! canvas divide `HudGeometry`/`ContainerGeometry::build_inner` performs is a
//! no-op. `container_screen.rs` exercises scale 4 (`1920x1080`), but only for
//! **CPU geometry** — `hit_test`/`panel_origin` math, no rendering. So no
//! pixel-level gate anywhere in the suite has ever driven the item-model pass
//! through an actual scale-up. That is exactly the blind spot that mattered:
//! every one of the three bugs found while wiring GUI scale (a hardcoded
//! `const S = 2.0` double-applying on native sprites, `IconRenderer::upload`
//! feeding `gui_ortho` a physical size while its geometry was posed
//! logically, and `hit_test` comparing a physical cursor against a logical
//! layout) manifests **only** at scale > 1. A gate that always runs at scale
//! 1 cannot see any of them, no matter how many pixels it counts.
//!
//! This is the sibling of `container_item_pixels.rs`, unchanged in every way
//! except the fixture size and — because a "16 px logical cell" is now a
//! *32 px physical* one — every pixel-area expectation is derived from the
//! scale rather than re-measured by hand. See `EXPECTED_LIT` and
//! `top_rows`/`bottom_rows` below.
//!
//! ```text
//! cargo test -p lodestone-shell --test container_item_pixels_scaled -- --ignored --nocapture
//! ```

use lodestone::config::{calculate_gui_scale, AUTO_GUI_SCALE};
use lodestone::container::{ContainerFrame, ContainerGeometry, ContainerRenderer, slot_layout};
use lodestone::gpu::RenderState;
use lodestone::resources::{BlockResources, load_item_atlas};
use lodestone_assets::ResourceLocation;
use lodestone_game::{item::ItemStack, menu::Menu};
use lodestone_model::Identifier;
use lodestone_render::{BlockModels, GpuContext, HeadlessTarget, RenderTarget};

/// `640x480`: at scale 1 `640/2 = 320 >= 320` and `480/2 = 240 >= 240` so the
/// loop in `calculate_gui_scale` advances to 2; at scale 2 `640/3 = 213 <
/// 320` so it stops there. This is the same fixture `container_screen.rs`
/// already names in its own comments as "640x480 (scale 2)" — chosen there,
/// and here, for landing exactly on 2 rather than sliding on to 3 like a
/// larger framebuffer (e.g. 960x720) would.
const W: u32 = 640;
const H: u32 = 480;

/// A full opaque cube whose faces are all one sprite, so the silhouette is
/// exactly the analytic figure with no cutout texels to argue about.
const BLOCK_ITEM: &str = "minecraft:stone";
/// A flat `item/generated` icon, to exercise the other stream.
const SPRITE_ITEM: &str = "minecraft:diamond";

const BLOCK_SLOT: usize = 36;
const SPRITE_SLOT: usize = 37;
const EMPTY_SLOT: usize = 38;

/// The analytic silhouette area of the vanilla block pose in a 16 **logical**
/// px cell — see `container_item_pixels.rs` and `docs/item-gui-geometry.md`
/// for the derivation. Physical pixel area scales with the *square* of the
/// GUI scale (both width and height of the cell scale by it), so this is
/// **not** re-measured at scale 2 — it is this same figure times `scale^2`,
/// computed from the scale the fixture actually produces rather than
/// hand-tuned to whatever a run of this test happens to emit. If a future
/// change breaks that quadratic relationship (e.g. only one axis scales, or
/// scale is applied twice), this expectation stays fixed while the actual
/// count moves, and the test fails instead of quietly re-centring on the bug.
const EXPECTED_LIT_AT_SCALE_1: f32 = 172.5;

fn id(path: &str) -> Identifier {
    path.parse().expect("valid item id")
}

/// The `(x, y, w, h)` **physical** screen rect of a menu slot at the given
/// integer `scale`, mirroring `container_item_pixels.rs`'s `slot_rect`
/// exactly (this file's whole point is to run that same conversion somewhere
/// it is not a no-op).
fn slot_rect(menu: &Menu, frame: &ContainerFrame<'_>, menu_index: usize, scale: f32) -> [u32; 4] {
    let widget = ContainerGeometry::build(frame, W, H)
        .widget_rect
        .expect("a populated frame has a widget rect");
    let layout = slot_layout(menu);
    let slot = layout
        .slots
        .iter()
        .find(|s| s.menu_index == menu_index)
        .expect("menu index must be laid out");
    [
        ((widget.x + slot.x) * scale) as u32,
        ((widget.y + slot.y) * scale) as u32,
        (slot.w * scale) as u32,
        (slot.h * scale) as u32,
    ]
}

fn clear_view(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gate-clear"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("gate-clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    queue.submit(std::iter::once(encoder.finish()));
}

/// Pixels inside `rect` where `shot` differs from the chrome-only `base`.
fn diff_in(shot: &[u8], base: &[u8], rect: [u32; 4]) -> usize {
    let [rx, ry, rw, rh] = rect;
    let mut n = 0usize;
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            let i = ((y * W + x) * 4) as usize;
            let d = (i32::from(shot[i]) - i32::from(base[i])).abs()
                + (i32::from(shot[i + 1]) - i32::from(base[i + 1])).abs()
                + (i32::from(shot[i + 2]) - i32::from(base[i + 2])).abs();
            if d > 12 {
                n += 1;
            }
        }
    }
    n
}

/// Max colour channel at `(x, y)` — "how lit is this pixel".
fn brightness(pixels: &[u8], x: u32, y: u32) -> u32 {
    let i = ((y * W + x) * 4) as usize;
    u32::from(pixels[i].max(pixels[i + 1]).max(pixels[i + 2]))
}

/// Mean brightness over `rows` of `rect`, counting only pixels the icon
/// changed. Comparing the horizontal face (top of the hexagon) against the
/// vertical ones (bottom) is what tells `Up` from `Down`, and therefore a
/// correctly wound cube from an inside-out one.
fn band_mean(
    shot: &[u8],
    base: &[u8],
    rect: [u32; 4],
    rows: std::ops::Range<u32>,
) -> Option<f32> {
    let [rx, ry, rw, _] = rect;
    let (mut sum, mut n) = (0u32, 0u32);
    for dy in rows {
        for x in rx..rx + rw {
            let y = ry + dy;
            let i = ((y * W + x) * 4) as usize;
            let d = (i32::from(shot[i]) - i32::from(base[i])).abs()
                + (i32::from(shot[i + 1]) - i32::from(base[i + 1])).abs()
                + (i32::from(shot[i + 2]) - i32::from(base[i + 2])).abs();
            if d > 12 {
                sum += brightness(shot, x, y);
                n += 1;
            }
        }
    }
    (n > 0).then(|| sum as f32 / n as f32)
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_block_item_in_a_container_slot_reaches_pixels_at_gui_scale_two() {
    // The precondition this whole file exists to exercise. This must FAIL,
    // not skip, if `W`/`H` stop producing scale 2 — a silent skip would
    // quietly delete the only scale > 1 pixel gate in the suite while
    // looking green, which is precisely the failure mode
    // `hotbar_block_item_pixels.rs` and `container_item_pixels.rs` already
    // guard against for scale 1.
    let scale = calculate_gui_scale(AUTO_GUI_SCALE, W, H);
    assert_eq!(
        scale, 2,
        "this fixture is chosen to land on GUI scale 2 exactly; if this \
         fails, W/H no longer do and this file is not testing what its \
         name claims. Do not relax this to a skip."
    );
    let scale = scale as f32;

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
    let models: &BlockModels = atlas
        .models()
        .expect("the vanilla load must attach baked block models");
    let block: ResourceLocation = BLOCK_ITEM.parse().expect("valid item id");
    assert!(
        models.item(&block).is_some(),
        "{BLOCK_ITEM} must have baked 3-D inventory geometry; without it this gate \
         would be measuring the absence of an item rather than the absence of a draw"
    );
    let item_atlas =
        load_item_atlas().expect("the item atlas must build from the same client.jar");
    for want in [BLOCK_ITEM, SPRITE_ITEM] {
        let loc: ResourceLocation = want.parse().expect("valid item id");
        assert!(
            item_atlas.icon(&loc).is_some(),
            "{want} must resolve to an icon; the screen reaches both icon kinds \
             through the atlas's cached IconPart"
        );
    }

    // Two menus with identical layout: one populated, one empty. The empty
    // one is the chrome baseline every count below is measured against.
    let empty_menu = Menu::player();
    let mut menu = Menu::player();
    menu.set_slot_item(BLOCK_SLOT, Some(ItemStack::new(id(BLOCK_ITEM), 1)));
    menu.set_slot_item(SPRITE_SLOT, Some(ItemStack::new(id(SPRITE_ITEM), 1)));

    let frame = ContainerFrame::new(Some(&menu), "Inventory");
    let base_frame = ContainerFrame::new(Some(&empty_menu), "Inventory");

    let block_rect = slot_rect(&menu, &frame, BLOCK_SLOT, scale);
    let sprite_rect = slot_rect(&menu, &frame, SPRITE_SLOT, scale);
    let empty_rect = slot_rect(&menu, &frame, EMPTY_SLOT, scale);

    let mut target = HeadlessTarget::new(device, W, H, format);
    let render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let mut shoot = |r: &mut ContainerRenderer, f: &ContainerFrame<'_>| -> Vec<u8> {
        let acquired = target.acquire().expect("headless acquire");
        clear_view(device, queue, acquired.view());
        r.render_with_icons(
            device,
            queue,
            acquired.view(),
            Some(render.depth_view()),
            f,
            Some(models),
            W,
            H,
        );
        target.read_texels(device, queue)
    };

    // Subject: the full wiring — flat atlas plus the 3-D item-model pass.
    let mut lit = ContainerRenderer::new(device, format);
    lit.attach_items(device, queue, format, item_atlas.clone());
    lit.attach_item_models(
        device,
        format,
        render
            .model_atlas_view()
            .expect("the vanilla path must expose a model atlas"),
        render
            .model_atlas_sampler()
            .expect("the vanilla path must expose a model atlas sampler"),
        render
            .model_palette_buffer()
            .expect("the vanilla path must expose a tint palette"),
        render
            .model_anim_buffer()
            .expect("the vanilla path must expose animation slots"),
    );
    let chrome = shoot(&mut lit, &base_frame);
    let subject = shoot(&mut lit, &frame);

    // Control: identical in every respect except that the item-model pass
    // was never attached, so a block item's geometry has nowhere to draw.
    let mut dark = ContainerRenderer::new(device, format);
    dark.attach_items(device, queue, format, item_atlas.clone());
    let control = shoot(&mut dark, &frame);

    let block_lit = diff_in(&subject, &chrome, block_rect);
    let sprite_lit = diff_in(&subject, &chrome, sprite_rect);
    let empty_lit = diff_in(&subject, &chrome, empty_rect);
    let control_block = diff_in(&control, &chrome, block_rect);
    let control_sprite = diff_in(&control, &chrome, sprite_rect);

    // Rows 1..5 of a 16 *logical* px cell are entirely the horizontal face
    // (the top of the isometric hexagon); rows 11..15 are entirely side
    // faces — see `container_item_pixels.rs`. The pose is a pure affine
    // scale-up at scale 2 (that is the whole property under test), so the
    // same proportions hold; only the row indices move with the scale.
    let top_rows = (1.0 * scale) as u32..(5.0 * scale) as u32;
    let bottom_rows = (11.0 * scale) as u32..(15.0 * scale) as u32;
    let top_mean = band_mean(&subject, &chrome, block_rect, top_rows)
        .expect("the block icon must light its top rows");
    let side_mean = band_mean(&subject, &chrome, block_rect, bottom_rows)
        .expect("the block icon must light its bottom rows");

    // The load-bearing number: derived from the scale, not measured and then
    // pasted back in. Physical area scales with `scale^2` because both the
    // cell's width and height do.
    let expected_lit = EXPECTED_LIT_AT_SCALE_1 * scale * scale;
    let low = (expected_lit * 0.85) as usize;
    let high = (expected_lit * 1.15) as usize;

    eprintln!("=== container item-icon pixel gate (GUI scale 2) ===");
    eprintln!("scale                     = {scale}");
    eprintln!("block slot  {BLOCK_SLOT} rect = {block_rect:?} ({BLOCK_ITEM})");
    eprintln!("sprite slot {SPRITE_SLOT} rect = {sprite_rect:?} ({SPRITE_ITEM})");
    eprintln!("empty slot  {EMPTY_SLOT} rect = {empty_rect:?}");
    eprintln!("expected block silhouette = {expected_lit:.1} px (172.5 * scale^2)");
    eprintln!("lit, block slot           = {block_lit}");
    eprintln!("lit, sprite slot          = {sprite_lit}");
    eprintln!("lit, empty slot           = {empty_lit}");
    eprintln!("lit, block slot (no item-model pass attached) = {control_block}");
    eprintln!("lit, sprite slot (control, atlas still attached) = {control_sprite}");
    eprintln!("top-band mean    = {top_mean:.1} (Up face, face_shade 1.0)");
    eprintln!("bottom-band mean = {side_mean:.1} (side faces, 0.6/0.8)");
    eprintln!("ratio            = {:.2}", top_mean / side_mean);

    assert!(
        (low..=high).contains(&block_lit),
        "a block item's container icon at GUI scale 2 must cover ~{expected_lit:.0} \
         physical px (172.5 px^2 at scale 1, scaled by scale^2 = {scale}^2); got \
         {block_lit}. This is exactly the shape of the two bugs this file exists to \
         catch: a scale applied twice would read ~4x too high, a scale never applied \
         (or applied to the wrong axis) would read ~1x (unscaled) or off-axis."
    );

    assert!(
        top_mean > side_mean * 1.15,
        "the top of the icon must be the full-shade Up face, not the half-shade \
         Down face: top={top_mean:.1} side={side_mean:.1}. A ratio at or below 1 \
         means the winding flipped and you are seeing the inside of the cube"
    );

    // Same derivation as the block silhouette: the flat sprite's threshold at
    // scale 1 was "> 100 of a 256 px cell"; at scale 2 the cell's area is
    // 4x (`scale^2`), so the threshold scales the same way rather than
    // staying pinned to the scale-1 number.
    let sprite_threshold = (100.0 * scale * scale) as usize;
    assert!(
        sprite_lit > sprite_threshold,
        "a flat-sprite item must cover most of its scaled cell; got {sprite_lit}, \
         wanted > {sprite_threshold}"
    );

    assert_eq!(
        empty_lit, 0,
        "an empty container slot must be pixel-identical to the chrome baseline; \
         {empty_lit} changed pixels there means a draw is not localised to its slot \
         and the counts above are not measuring what they claim"
    );

    // The executed negative control: with the icon pass detached, the block
    // slot must be indistinguishable from an empty one — proving the zero
    // above is because nothing drew there, not because `slot_rect`'s scale
    // conversion pointed the sample at the wrong (also-empty) region. The
    // sprite-slot assertion right after it is what rules that out: it uses
    // the exact same scaled `slot_rect` conversion and *does* see paint, so
    // the conversion is not silently sampling dead space.
    assert_eq!(
        control_block, 0,
        "without attach_item_models the same frame must draw nothing in the block \
         slot; {control_block} changed pixels means something else is painting there \
         and the positive assertion is not evidence for the icon pass"
    );

    assert!(
        control_sprite > sprite_threshold,
        "the control keeps the item atlas attached, so its sprite slot must still \
         draw ({control_sprite} px, wanted > {sprite_threshold}) at the same scaled \
         rect the block-slot control just read zero from; if it does not, that zero \
         is the scaled rect landing on dead space, not evidence attach_item_models \
         is what draws the block"
    );
}

/// The **special-renderer** stream at GUI scale 2 — the third icon kind, which
/// the gate above does not reach.
///
/// `container_item_pixels.rs`'s `a_player_head_in_a_container_slot_reaches_pixels`
/// is this test at scale 1. It exists separately for the reason this whole file
/// exists: the special pass poses its instance matrices in **logical** GUI pixel
/// space and `IconRenderer::upload` builds its projection from the size the
/// caller hands back, so a physical-vs-logical mix-up in that pass is invisible
/// at scale 1 (where the divide is a no-op) and moves the icon off its cell at
/// every real GUI scale. Every scale the shipped client actually runs at is > 1.
///
/// The expectation is derived from the scale rather than re-measured: a cell is
/// `16 * scale` px on a side, so a silhouette's area scales with `scale^2`.
/// `HEAD_LOGICAL_LIT` is the scale-1 figure the sibling gate prints.
///
/// Controls are the sibling's: an empty slot at exactly 0, the head's own slot
/// at exactly 0 with the 3-D pass detached (`prepare_special` is gated on
/// `models.is_some()`), and the flat sprite still drawing in that same control
/// frame so it is not dark for the wrong reason.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_player_head_in_a_container_slot_reaches_pixels_at_gui_scale_two() {
    /// The `IconPart::Special` subject.
    const HEAD_ITEM: &str = "minecraft:player_head";
    const HEAD_SLOT: usize = 37;
    const BLANK_SLOT: usize = 38;
    const FLAT_SLOT: usize = 36;
    /// Changed pixels the same head covers in a 16 px **logical** cell, as
    /// printed by `container_item_pixels.rs`'s scale-1 sibling. Physical area
    /// scales with the square of the GUI scale.
    const HEAD_LOGICAL_LIT: f32 = 120.0;

    let scale = calculate_gui_scale(AUTO_GUI_SCALE, W, H);
    assert_eq!(
        scale, 2,
        "this fixture is chosen to land on GUI scale 2 exactly; if this \
         fails, W/H no longer do and this file is not testing what its \
         name claims. Do not relax this to a skip."
    );
    let scale = scale as f32;

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
    let models: &BlockModels = atlas
        .models()
        .expect("the vanilla load must attach baked block models");
    let item_atlas =
        load_item_atlas().expect("the item atlas must build from the same client.jar");

    let head_loc: ResourceLocation = HEAD_ITEM.parse().expect("valid item id");
    let icon = item_atlas
        .icon(&head_loc)
        .expect("player_head must resolve to an icon in the item atlas");
    assert!(
        icon.parts
            .iter()
            .any(|part| matches!(part, lodestone_assets::IconPart::Special { .. })),
        "player_head's icon must carry an IconPart::Special; without it this gate \
         measures the absence of an item rather than the absence of a draw"
    );

    let empty_menu = Menu::player();
    let mut menu = Menu::player();
    menu.set_slot_item(FLAT_SLOT, Some(ItemStack::new(id(SPRITE_ITEM), 1)));
    menu.set_slot_item(HEAD_SLOT, Some(ItemStack::new(id(HEAD_ITEM), 1)));

    let frame = ContainerFrame::new(Some(&menu), "Inventory");
    let base_frame = ContainerFrame::new(Some(&empty_menu), "Inventory");

    let head_rect = slot_rect(&menu, &frame, HEAD_SLOT, scale);
    let flat_rect = slot_rect(&menu, &frame, FLAT_SLOT, scale);
    let blank_rect = slot_rect(&menu, &frame, BLANK_SLOT, scale);

    let mut target = HeadlessTarget::new(device, W, H, format);
    let render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let mut shoot = |r: &mut ContainerRenderer, f: &ContainerFrame<'_>| -> Vec<u8> {
        let acquired = target.acquire().expect("headless acquire");
        clear_view(device, queue, acquired.view());
        r.render_with_icons(
            device,
            queue,
            acquired.view(),
            Some(render.depth_view()),
            f,
            Some(models),
            W,
            H,
        );
        target.read_texels(device, queue)
    };

    let mut lit = ContainerRenderer::new(device, format);
    lit.attach_items(device, queue, format, item_atlas.clone());
    lit.attach_item_models(
        device,
        format,
        render
            .model_atlas_view()
            .expect("the vanilla path must expose a model atlas"),
        render
            .model_atlas_sampler()
            .expect("the vanilla path must expose a model atlas sampler"),
        render
            .model_palette_buffer()
            .expect("the vanilla path must expose a tint palette"),
        render
            .model_anim_buffer()
            .expect("the vanilla path must expose animation slots"),
    );
    let chrome = shoot(&mut lit, &base_frame);
    let subject = shoot(&mut lit, &frame);

    let mut dark = ContainerRenderer::new(device, format);
    dark.attach_items(device, queue, format, item_atlas.clone());
    let control = shoot(&mut dark, &frame);

    let head_lit = diff_in(&subject, &chrome, head_rect);
    let flat_lit = diff_in(&subject, &chrome, flat_rect);
    let blank_lit = diff_in(&subject, &chrome, blank_rect);
    let control_head = diff_in(&control, &chrome, head_rect);
    let control_flat = diff_in(&control, &chrome, flat_rect);

    let expected = HEAD_LOGICAL_LIT * scale * scale;
    eprintln!("=== container special-item (player head) pixel gate, GUI scale 2 ===");
    eprintln!("head slot  {HEAD_SLOT} rect = {head_rect:?} ({HEAD_ITEM})");
    eprintln!("expected head silhouette  = {expected:.0} px of {}", 256.0 * scale * scale);
    eprintln!("lit, head slot            = {head_lit}");
    eprintln!("lit, flat sprite slot     = {flat_lit}");
    eprintln!("lit, empty slot           = {blank_lit}");
    eprintln!("lit, head slot (no item-model pass attached) = {control_head}");
    eprintln!("lit, flat slot (control, atlas still attached) = {control_flat}");

    assert!(
        head_lit > 0,
        "nothing drew in the container slot holding a player head at GUI scale 2. \
         The head resolves to a real IconPart::Special (asserted above) and the same \
         fixture at scale 1 draws ~{HEAD_LOGICAL_LIT:.0} px, so this is a \
         logical-vs-physical mix-up in the special pass, not a missing rig"
    );
    let low = (expected * 0.75) as usize;
    let high = (expected * 1.25) as usize;
    assert!(
        (low..=high).contains(&head_lit),
        "a player head's container icon must cover ~{expected:.0} px at GUI scale 2 \
         (the scale-1 figure times scale^2); got {head_lit}. A figure near the \
         unscaled {HEAD_LOGICAL_LIT:.0} means the pose never learned about the scale"
    );
    assert!(
        flat_lit > 100,
        "the flat sprite slot must still draw ({flat_lit} px); if it does not, this \
         frame is wrong for a reason that has nothing to do with the head"
    );
    assert_eq!(
        blank_lit, 0,
        "an empty container slot must be pixel-identical to the chrome baseline; \
         {blank_lit} changed pixels there means the head's draw is not localised"
    );
    assert_eq!(
        control_head, 0,
        "with the 3-D item-model pass detached the special stream is gated off, so \
         the head's slot must be indistinguishable from an empty one; got \
         {control_head}"
    );
    assert!(
        control_flat > 100,
        "the control keeps the item atlas attached, so its sprite slot must still \
         draw ({control_flat} px); if it does not, the control is dark for the wrong \
         reason"
    );
}
