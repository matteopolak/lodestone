//! Pixel gate: **container and inventory slots must draw real item icons.**
//!
//! The sibling of `hotbar_block_item_pixels.rs`, and for the same reason. That
//! gate proved the hotbar's nine cells reach pixels; the inventory screen kept
//! drawing a hash-derived colour swatch and a single letter, so a player could
//! see nine items and not the other thirty-seven — including, once crafting
//! landed, the grid and the result slot. `container_screen.rs` stayed green
//! throughout: it measures *coverage inside the widget rect*, which a swatch
//! provides just as well as an icon does. Coverage cannot tell a picture of a
//! diamond from a coloured square.
//!
//! This one drives the real [`ContainerRenderer`] through the same calls the
//! shell makes:
//!
//! ```text
//! Menu::slot_item -> ItemAtlas::icon -> IconPart::{Sprite,Model}
//!   -> ContainerRenderer's icon passes -> pixels
//! ```
//!
//! # What "lit" means here, and why it is a difference
//!
//! Unlike a hotbar cell over a black backdrop, a container slot is *never*
//! blank: the panel and the slot well are chrome this screen always draws. So
//! every measurement below is a **difference against a chrome-only baseline** —
//! the identical menu with the identical wiring and no items in it. A pixel
//! counts as lit when the populated render differs from that baseline, which is
//! exactly "the icon put something here".
//!
//! # The numbers
//!
//! Container cells are 16 px, the same as the hotbar's procedural layout, so a
//! **block** item's isometric silhouette is the same analytic figure:
//!
//! ```text
//! A = 172.5 px^2 in a 16x16 = 256 px cell (bbox 14.14 x 15.73)
//! ```
//!
//! derived in `docs/item-gui-geometry.md` and in the hotbar gate's module docs.
//! The band is tight enough that a half-drawn cube (~86) or a mis-scaled one
//! fails. The winding check (top band brighter than the bottom band, because the
//! visible set is `{Up, East, North}` at `face_shade {1.0, 0.6, 0.8}` and the
//! inside-out set is `{Down, West, South}` at `{0.5, 0.6, 0.8}`) is asserted too,
//! since silhouette area alone cannot see an inside-out cube.
//!
//! A **flat sprite** item is measured separately and loosely: its coverage is
//! whatever its texture's opaque texels happen to be, so the assertion is that
//! it covers most of the cell rather than a specific count.
//!
//! # Controls
//!
//! * **an empty slot** must differ from the baseline by exactly **0** px, so the
//!   counts above are localised to their own slot rather than a screen-wide leak;
//! * **`attach_item_models` never called**, everything else identical: the block
//!   slot must read exactly **0**. That is the executed proof that the new icon
//!   pass is what puts those pixels there. It is a real control on this screen —
//!   without it the block slot previously drew a swatch, which is *not* zero, so
//!   this also pins that the fallback no longer fires once an atlas is attached.
//!
//! Fail-closed like its siblings: a missing GPU or a missing `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test container_item_pixels -- --ignored --nocapture
//! ```

use lodestone::config::{calculate_gui_scale, AUTO_GUI_SCALE};
use lodestone::container::{ContainerFrame, ContainerGeometry, ContainerRenderer, slot_layout};
use lodestone::gpu::RenderState;
use lodestone::resources::{BlockResources, load_item_atlas};
use lodestone_assets::ResourceLocation;
use lodestone_game::{item::ItemStack, menu::Menu};
use lodestone_model::Identifier;
use lodestone_render::{BlockModels, GpuContext, HeadlessTarget, RenderTarget};

/// `ContainerGeometry::build_inner` divides the physical framebuffer down to
/// a **logical** GUI canvas before laying out the panel and its slots (see
/// `ContainerGeometry::widget_rect`'s doc comment), then the render pass
/// stretches that canvas back out to fill the physical target. This gate's
/// pixel-area math (`EXPECTED_LIT`, the top/bottom band rows) is derived for
/// an exact 16x16 physical cell, which only holds if that divide is a no-op.
/// `(480, 320)` is `hud.rs`'s own `hud_vitals_draw_the_real_heart_sprite`
/// gate's fixture, chosen for exactly this reason:
/// `calculate_gui_scale(AUTO, 480, 320) == 1`. `slot_rect` below still
/// performs the physical<->logical conversion explicitly rather than assuming
/// it away, so this file stays correct if `W`/`H` ever change; a scale > 1
/// case is preserved elsewhere in the suite —
/// `container_screen.rs`'s `hit_test_and_drawn_geometry_share_one_panel_origin`
/// runs the equivalent panel-origin math at 1920x1080 (scale 4), but only for
/// CPU geometry, never a GPU readback. `container_item_pixels_scaled.rs` is
/// this file's sibling at GUI scale 2 (`640x480`) for exactly that reason:
/// every one of the three bugs found while wiring GUI scale showed up only
/// at scale > 1, and until that file existed nothing at the pixel level ran
/// this pass through an actual scale-up.
const W: u32 = 480;
const H: u32 = 320;

/// A full opaque cube whose faces are all one sprite, so the silhouette is
/// exactly the analytic figure with no cutout texels to argue about.
const BLOCK_ITEM: &str = "minecraft:stone";
/// A flat `item/generated` icon, to exercise the other stream.
const SPRITE_ITEM: &str = "minecraft:diamond";

/// Player-menu slot indices under test. 36 is the first hotbar slot on the
/// inventory screen, 37 the second, 38 the third — real `menu_index` values, not
/// offsets: `slot_layout` carries the index through, so nothing here has to know
/// where the hotbar starts.
const BLOCK_SLOT: usize = 36;
const SPRITE_SLOT: usize = 37;
const EMPTY_SLOT: usize = 38;

/// The analytic silhouette area of the vanilla block pose in a 16 px cell.
const EXPECTED_LIT: f32 = 172.5;

fn id(path: &str) -> Identifier {
    path.parse().expect("valid item id")
}

/// The `(x, y, 16, 16)` screen rect of a menu slot: the widget's own origin plus
/// the slot's local offset, both straight from the module under test.
///
/// `widget_rect` and every slot offset in `layout` are in the **logical** GUI
/// canvas `ContainerGeometry::build_inner` lays its fixed pixel constants into
/// (see `ContainerGeometry::widget_rect`'s doc comment) — not the physical
/// framebuffer this test reads back. The render pass stretches that logical
/// canvas to fill the physical target, so the logical rect is scaled *up* by
/// the effective GUI scale here before being handed back as a physical pixel
/// rect — the mirror of what `hit_test` does to an incoming physical cursor
/// position. At `W`x`H` this multiplication is a verified no-op (see the
/// module-level comment), but it is written generally rather than assumed
/// away, so this function stays correct even if that ever changes.
fn slot_rect(menu: &Menu, frame: &ContainerFrame<'_>, menu_index: usize) -> [u32; 4] {
    let widget = ContainerGeometry::build(frame, W, H)
        .widget_rect
        .expect("a populated frame has a widget rect");
    let layout = slot_layout(menu);
    let slot = layout
        .slots
        .iter()
        .find(|s| s.menu_index == menu_index)
        .expect("menu index must be laid out");
    let scale = calculate_gui_scale(AUTO_GUI_SCALE, W, H).max(1) as f32;
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

/// Mean brightness over `rows` of `rect`, counting only pixels the icon changed.
/// Comparing the horizontal face (top of the hexagon) against the vertical ones
/// (bottom) is what tells `Up` from `Down`, and therefore a correctly wound cube
/// from an inside-out one.
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
fn container_slots_draw_real_item_icons() {
    // `EXPECTED_LIT` and the top/bottom band rows below are derived for an
    // exact 16x16 *physical* cell, which only holds while `slot_rect`'s
    // logical->physical multiplication is a no-op. Assert the precondition
    // rather than silently trusting it — a future change to `W`/`H` that
    // broke this would otherwise sample a scaled cell against unscaled area
    // expectations and either false-fail or false-pass by accident.
    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "the pixel-area math below assumes W x H divides to itself under the \
         GUI scale; if this fails, EXPECTED_LIT and the band row ranges must \
         be re-derived for the scaled cell size"
    );

    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // sRGB, like the live surface: the model shader's tint/shade round-trip is
    // written for an sRGB target.
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

    // Two menus with identical layout: one populated, one empty. The empty one
    // is the chrome baseline every count below is measured against.
    let empty_menu = Menu::player();
    let mut menu = Menu::player();
    menu.set_slot_item(BLOCK_SLOT, Some(ItemStack::new(id(BLOCK_ITEM), 1)));
    menu.set_slot_item(SPRITE_SLOT, Some(ItemStack::new(id(SPRITE_ITEM), 1)));

    let frame = ContainerFrame::new(Some(&menu), "Inventory");
    let base_frame = ContainerFrame::new(Some(&empty_menu), "Inventory");

    let block_rect = slot_rect(&menu, &frame, BLOCK_SLOT);
    let sprite_rect = slot_rect(&menu, &frame, SPRITE_SLOT);
    let empty_rect = slot_rect(&menu, &frame, EMPTY_SLOT);

    let mut target = HeadlessTarget::new(device, W, H, format);
    // The world renderer is here for its *resources*, not its terrain: the icon
    // pass borrows its block atlas, tint palette, animation slots and depth
    // buffer. Nothing is uploaded twice.
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

    // Control: identical in every respect except that the item-model pass was
    // never attached, so a block item's geometry has nowhere to draw.
    let mut dark = ContainerRenderer::new(device, format);
    dark.attach_items(device, queue, format, item_atlas.clone());
    let control = shoot(&mut dark, &frame);

    let block_lit = diff_in(&subject, &chrome, block_rect);
    let sprite_lit = diff_in(&subject, &chrome, sprite_rect);
    let empty_lit = diff_in(&subject, &chrome, empty_rect);
    let control_block = diff_in(&control, &chrome, block_rect);
    let control_sprite = diff_in(&control, &chrome, sprite_rect);
    // Rows 1..5 of the 16 px cell are entirely the horizontal face (the top of
    // the isometric hexagon); rows 11..15 are entirely side faces.
    let top_mean = band_mean(&subject, &chrome, block_rect, 1..5)
        .expect("the block icon must light its top rows");
    let side_mean = band_mean(&subject, &chrome, block_rect, 11..15)
        .expect("the block icon must light its bottom rows");

    eprintln!("=== container item-icon pixel gate ===");
    eprintln!("block slot  {BLOCK_SLOT} rect = {block_rect:?} ({BLOCK_ITEM})");
    eprintln!("sprite slot {SPRITE_SLOT} rect = {sprite_rect:?} ({SPRITE_ITEM})");
    eprintln!("empty slot  {EMPTY_SLOT} rect = {empty_rect:?}");
    eprintln!("expected block silhouette = {EXPECTED_LIT:.1} px of 256");
    eprintln!("lit, block slot           = {block_lit}");
    eprintln!("lit, sprite slot          = {sprite_lit}");
    eprintln!("lit, empty slot           = {empty_lit}");
    eprintln!("lit, block slot (no item-model pass attached) = {control_block}");
    eprintln!("lit, sprite slot (control, atlas still attached) = {control_sprite}");
    eprintln!("top-band mean    = {top_mean:.1} (Up face, face_shade 1.0)");
    eprintln!("bottom-band mean = {side_mean:.1} (side faces, 0.6/0.8)");
    eprintln!("ratio            = {:.2}", top_mean / side_mean);

    let low = (EXPECTED_LIT * 0.85) as usize;
    let high = (EXPECTED_LIT * 1.15) as usize;
    assert!(
        (low..=high).contains(&block_lit),
        "a block item's container icon must cover ~{EXPECTED_LIT:.0} of the 256 px \
         cell (the 14.14x15.73 silhouette of the vanilla [30,225,0]/0.625 pose); got \
         {block_lit}. Far below means faces are missing or the pass never drew; far \
         above means the pose, the ortho, or the slot rect is wrong."
    );

    assert!(
        top_mean > side_mean * 1.15,
        "the top of the icon must be the full-shade Up face, not the half-shade \
         Down face: top={top_mean:.1} side={side_mean:.1}. A ratio at or below 1 \
         means the winding flipped and you are seeing the inside of the cube"
    );

    assert!(
        sprite_lit > 100,
        "a flat-sprite item must cover most of its 256 px cell; got {sprite_lit}. \
         This is the other icon stream, and it shares no code with the block path \
         beyond the sink it writes into"
    );

    assert_eq!(
        empty_lit, 0,
        "an empty container slot must be pixel-identical to the chrome baseline; \
         {empty_lit} changed pixels there means a draw is not localised to its slot \
         and the counts above are not measuring what they claim"
    );

    // The executed negative control: with the icon pass detached, the block slot
    // must be indistinguishable from an empty one. This also pins that the
    // atlas-less colour-swatch fallback does *not* fire once an atlas is
    // attached — a swatch would show up here as ~100 changed pixels.
    assert_eq!(
        control_block, 0,
        "without attach_item_models the same frame must draw nothing in the block \
         slot; {control_block} changed pixels means something else is painting there \
         and the positive assertion is not evidence for the icon pass"
    );

    // ...while the flat stream, which that control leaves attached, still draws.
    // Without this the control above would also pass if `attach_items` were
    // silently broken, and the whole gate would be measuring nothing.
    assert!(
        control_sprite > 100,
        "the control keeps the item atlas attached, so its sprite slot must still \
         draw ({control_sprite} px); if it does not, the control is dark for the \
         wrong reason"
    );
}

/// The **special-renderer** icon stream in a container slot, which the gate
/// above cannot reach: `container_slots_draw_real_item_icons` measures a block
/// item (`IconPart::Model`) and a flat sprite (`IconPart::Sprite`), and a
/// player head is neither — it is `IconPart::Special`, a third stream with its
/// own pipeline, its own sheets and its own upload path
/// (`IconRenderer::prepare_special`).
///
/// # Why this exists beside `hotbar_special_item_pixels.rs`
///
/// That file already pixel-gates a player head, and it is green. It drives
/// `HudRenderer`. This one drives `ContainerRenderer` through
/// `render_with_icons`, so the input is a real `Menu` slot holding a real
/// `ItemStack` — the same hop production takes (`Menu::slot_item` ->
/// `container::builder::icon_record` -> `draw_item_icon` ->
/// `IconPart::Special`). The two screens share `item_icon.rs` but not the
/// geometry that feeds it, and `ContainerGeometry` carries the extra
/// `slot_special_count` stratum split the hotbar has no equivalent of, so a
/// head can reach pixels in the hotbar and not in an inventory slot.
///
/// # The numbers
///
/// The head's silhouette is measured, not asserted at a round figure: the
/// vanilla `player_head.json` pose composes `display.gui` with the special
/// node's own transformation, and `hotbar_special_item_pixels.rs`'s
/// `player_head_silhouette_is_distinguishable_from_a_flat_quad` reports it at
/// ~110 px^2 of the 256 px cell. The band here is deliberately wide (a third
/// to three-quarters of the cell): this gate exists to tell "drew" from "drew
/// nothing", and the exact pose is pinned by that other file against the jar.
///
/// # Controls
///
/// * **an empty slot** must differ from the chrome baseline by exactly 0 px, so
///   the count is localised rather than a screen-wide leak;
/// * **`attach_item_models` never called** must read exactly 0 in the head's
///   slot. That is a real control for this stream and not a borrowed one:
///   `IconRenderer::prepare_special` returns early unless `self.models.is_some()`,
///   so detaching the 3-D pass is what makes the special pass dark;
/// * ...and the flat sprite slot in that same control frame must still draw, so
///   the control is not dark for the wrong reason.
///
/// Fail-closed like its sibling: a missing GPU or `client.jar` is a failure,
/// never a skip.
///
/// ```text
/// cargo test -p lodestone-shell --test container_item_pixels -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_player_head_in_a_container_slot_reaches_pixels() {
    /// The `IconPart::Special` subject. `minecraft:player_head` is its own
    /// `kind` in vanilla (distinct from the `minecraft:head` mob family)
    /// because its renderer resolves a profile texture; with no profile it
    /// draws the default skull sheet, which is all this gate needs.
    const HEAD_ITEM: &str = "minecraft:player_head";
    /// Second hotbar cell on the inventory screen — a real `menu_index`.
    const HEAD_SLOT: usize = 37;
    /// Third, left empty as the localisation control.
    const BLANK_SLOT: usize = 38;
    /// First, holding a flat sprite so the detached control has something that
    /// must still draw.
    const FLAT_SLOT: usize = 36;

    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "the cell-area band below is derived for a 16x16 physical cell"
    );

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

    // The precondition this gate would otherwise silently measure the absence
    // of: the head must reach the *special* stream, not a sprite fallback.
    let head_loc: ResourceLocation = HEAD_ITEM.parse().expect("valid item id");
    let icon = item_atlas
        .icon(&head_loc)
        .expect("player_head must resolve to an icon in the item atlas");
    let kind = icon
        .parts
        .iter()
        .find_map(|part| match part {
            lodestone_assets::IconPart::Special { kind, .. } => Some(kind.clone()),
            _ => None,
        })
        .expect("player_head's icon must carry an IconPart::Special");
    assert_eq!(
        kind, "minecraft:player_head",
        "the head must resolve through its own special-renderer kind"
    );

    let empty_menu = Menu::player();
    let mut menu = Menu::player();
    menu.set_slot_item(FLAT_SLOT, Some(ItemStack::new(id(SPRITE_ITEM), 1)));
    menu.set_slot_item(HEAD_SLOT, Some(ItemStack::new(id(HEAD_ITEM), 1)));

    let frame = ContainerFrame::new(Some(&menu), "Inventory");
    let base_frame = ContainerFrame::new(Some(&empty_menu), "Inventory");

    let head_rect = slot_rect(&menu, &frame, HEAD_SLOT);
    let flat_rect = slot_rect(&menu, &frame, FLAT_SLOT);
    let blank_rect = slot_rect(&menu, &frame, BLANK_SLOT);

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

    eprintln!("=== container special-item (player head) pixel gate ===");
    eprintln!("head slot  {HEAD_SLOT} rect = {head_rect:?} ({HEAD_ITEM})");
    eprintln!("lit, head slot            = {head_lit} of 256");
    eprintln!("lit, flat sprite slot     = {flat_lit}");
    eprintln!("lit, empty slot           = {blank_lit}");
    eprintln!("lit, head slot (no item-model pass attached) = {control_head}");
    eprintln!("lit, flat slot (control, atlas still attached) = {control_flat}");

    assert!(
        head_lit > 0,
        "nothing drew in the container slot holding a player head. The head resolved \
         to a real IconPart::Special above, so this is a break below that: either \
         push_special_icon recorded no draw, prepare_special dropped it, or the \
         batch never reached the pass"
    );
    assert!(
        (85..=190).contains(&head_lit),
        "a player head's container icon must cover roughly a third to three-quarters \
         of its 256 px cell (~110 px^2 for the vanilla composed pose — see \
         hotbar_special_item_pixels.rs); got {head_lit}. Far below means parts of the \
         rig are missing, far above means the pose or the slot rect is wrong"
    );
    assert!(
        flat_lit > 100,
        "the flat sprite slot must still draw ({flat_lit} px); if it does not, this \
         frame is wrong for a reason that has nothing to do with the head"
    );
    assert_eq!(
        blank_lit, 0,
        "an empty container slot must be pixel-identical to the chrome baseline; \
         {blank_lit} changed pixels there means the head's draw is not localised to \
         its own slot and the count above is not measuring what it claims"
    );
    assert_eq!(
        control_head, 0,
        "with the 3-D item-model pass detached the special stream is gated off, so \
         the head's slot must be indistinguishable from an empty one; {control_head} \
         changed pixels means something else paints there and the positive assertion \
         is not evidence for the special pass"
    );
    assert!(
        control_flat > 100,
        "the control keeps the item atlas attached, so its sprite slot must still \
         draw ({control_flat} px); if it does not, the control is dark for the wrong \
         reason"
    );
}

/// A **live resource-pack generation bump** must not blank a container slot's
/// special-renderer icon.
///
/// # Why this gate exists
///
/// `WindowApp::redraw`'s reload block replaces the model atlas view, the tint
/// palette and the animation buffer with new GPU objects and re-bakes every
/// sprite's UVs, then re-attaches the surfaces that borrow them. A pass left
/// un-reattached does not error — wgpu resources are `Arc`-backed and a bind
/// group holds a strong reference — it goes on sampling the dropped atlas, and
/// wherever a new UV lands on padding it draws nothing at all. That was a real
/// shipped bug for the flat-sprite and 3-D block-item streams.
///
/// No hermetic gate could see it, because every gate in this suite builds its
/// renderer once and never reloads. This one does reload: it replays the exact
/// sequence `redraw.rs` performs on a generation bump — `reload_block_atlas`,
/// then `attach_items`, then `attach_item_models` — around a frame holding all
/// three icon kinds, and requires the frame after to be **byte-identical** to
/// the frame before.
///
/// # What it can and cannot see
///
/// The bump reloads the *same* pack, deliberately: `GpuAtlas::from_atlas` builds
/// new GPU objects either way, so the objects are genuinely swapped while the
/// sprite packing is held constant. That isolates the reload wiring from a
/// repack — any pixel that moves is the wiring. The cost of holding the pack
/// constant is that this gate cannot see a *stale-but-identical* sheet, so it is
/// evidence about blanking and displacement, not about which pack's texels won.
///
/// The detector is demonstrably able to report a blank: the sibling gate's
/// detached-pass control reads exactly 0 in this same slot rect while the
/// attached one reads ~120.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_resource_pack_reload_does_not_blank_a_container_special_icon() {
    const HEAD_ITEM: &str = "minecraft:player_head";
    const HEAD_SLOT: usize = 37;
    const FLAT_SLOT: usize = 36;
    const CUBE_SLOT: usize = 38;

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

    let empty_menu = Menu::player();
    let mut menu = Menu::player();
    menu.set_slot_item(FLAT_SLOT, Some(ItemStack::new(id(SPRITE_ITEM), 1)));
    menu.set_slot_item(HEAD_SLOT, Some(ItemStack::new(id(HEAD_ITEM), 1)));
    menu.set_slot_item(CUBE_SLOT, Some(ItemStack::new(id(BLOCK_ITEM), 1)));

    let frame = ContainerFrame::new(Some(&menu), "Inventory");
    let base_frame = ContainerFrame::new(Some(&empty_menu), "Inventory");

    let head_rect = slot_rect(&menu, &frame, HEAD_SLOT);
    let flat_rect = slot_rect(&menu, &frame, FLAT_SLOT);
    let cube_rect = slot_rect(&menu, &frame, CUBE_SLOT);

    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let mut renderer = ContainerRenderer::new(device, format);

    // The bring-up wiring `app::lifecycle` performs, in its order.
    let attach = |r: &mut ContainerRenderer, render: &RenderState| {
        r.attach_items(device, queue, format, item_atlas.clone());
        r.attach_item_models(
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
    };
    attach(&mut renderer, &render);

    // `shoot` cannot borrow `render` — the reload below needs it mutably — so
    // the depth view is resolved per call instead.
    macro_rules! shoot {
        ($r:expr, $f:expr, $render:expr) => {{
            let acquired = target.acquire().expect("headless acquire");
            clear_view(device, queue, acquired.view());
            $r.render_with_icons(
                device,
                queue,
                acquired.view(),
                Some($render.depth_view()),
                $f,
                Some(models),
                W,
                H,
            );
            target.read_texels(device, queue)
        }};
    }

    let chrome = shoot!(renderer, &base_frame, render);
    // This frame is what latches the special pass's one lazy build
    // (`IconRenderer::prepare_special` builds `SpecialIcons` on first use), so
    // the bump below happens to an already-built pass — the live case.
    let before = shoot!(renderer, &frame, render);

    let sheets_before = renderer.special_icon_sheets();

    // ---- the generation bump, exactly as `WindowApp::redraw` performs it ----
    render.reload_block_atlas(device, queue, atlas.as_ref());
    attach(&mut renderer, &render);
    renderer.reload_special_icons();
    let sheets_after_bump = renderer.special_icon_sheets();

    let after = shoot!(renderer, &frame, render);
    let sheets_after_frame = renderer.special_icon_sheets();

    // The executed control for the sheet-count arm, and the pre-fix path
    // verbatim: an identical renderer taking an identical bump with
    // `reload_special_icons` omitted. Its special pass is never dropped, so its
    // sheets are still the ones decoded before the bump — a perfectly valid map
    // belonging to the *previous* pack. That is the defect this gate exists for,
    // exhibited rather than argued, and it is what makes the `== 0` assertion
    // above discriminating instead of decorative.
    let mut unreloaded = ContainerRenderer::new(device, format);
    attach(&mut unreloaded, &render);
    let _ = shoot!(unreloaded, &frame, render);
    let stale_before = unreloaded.special_icon_sheets();
    render.reload_block_atlas(device, queue, atlas.as_ref());
    attach(&mut unreloaded, &render);
    let stale_after = unreloaded.special_icon_sheets();

    let head_before = diff_in(&before, &chrome, head_rect);
    let head_after = diff_in(&after, &chrome, head_rect);
    let flat_before = diff_in(&before, &chrome, flat_rect);
    let flat_after = diff_in(&after, &chrome, flat_rect);
    let cube_before = diff_in(&before, &chrome, cube_rect);
    let cube_after = diff_in(&after, &chrome, cube_rect);
    let moved = before
        .iter()
        .zip(&after)
        .filter(|(a, b)| a != b)
        .count();

    eprintln!("=== container icons across a resource-pack generation bump ===");
    eprintln!("head slot  {HEAD_SLOT}  before = {head_before}  after = {head_after}");
    eprintln!("flat slot  {FLAT_SLOT}  before = {flat_before}  after = {flat_after}");
    eprintln!("cube slot  {CUBE_SLOT}  before = {cube_before}  after = {cube_after}");
    eprintln!("whole-frame bytes changed by the bump = {moved}");
    eprintln!("special sheets: before = {sheets_before}, immediately after the bump \
               = {sheets_after_bump}, after the next frame = {sheets_after_frame}");
    eprintln!("control (no reload_special_icons): {stale_before} -> {stale_after}");

    assert!(
        sheets_before > 0,
        "the special pass must have built itself and decoded sheets on the frame \
         before the bump, or every sheet assertion below is vacuous"
    );
    assert_eq!(
        sheets_after_bump, 0,
        "a generation bump must DROP the special pass, so the next frame rebuilds \
         its block-entity sheets against the current pack stack. It still holds \
         {sheets_after_bump} sheets, which are the previous pack's — this pass owns \
         its textures rather than borrowing them, so a re-attach cannot fix it and \
         nothing else ever clears its one-shot `special_tried` latch"
    );
    assert_eq!(
        sheets_after_frame, sheets_before,
        "...and the very next frame carrying a special icon must rebuild it. \
         {sheets_after_frame} against {sheets_before} before the bump means the drop \
         was not paired with a rebuild, which would leave every special icon blank"
    );
    assert_eq!(
        (stale_before, stale_after),
        (sheets_before, sheets_before),
        "the control must exhibit the defect: with `reload_special_icons` omitted, \
         the bump leaves the pass holding its pre-bump sheets ({stale_before} -> \
         {stale_after}). If this ever reads 0 the two arms have stopped differing \
         and the assertion above is no longer evidence of anything"
    );

    assert!(
        head_before > 0,
        "the pre-bump frame must draw the head, or this gate is measuring a blank \
         against a blank; got {head_before}"
    );
    assert!(
        head_after > 0,
        "a resource-pack generation bump blanked the container slot holding a player \
         head: {head_before} px before, {head_after} after. The special stream is not \
         being re-attached by the reload path"
    );
    assert_eq!(
        head_after, head_before,
        "the head's icon must be unchanged across a bump that reloads the same pack"
    );
    assert_eq!(
        (flat_after, cube_after),
        (flat_before, cube_before),
        "the flat-sprite and 3-D block streams must be unchanged across the bump too \
         — if one of these moved, the reload wiring regressed for a stream that was \
         explicitly fixed"
    );
    assert_eq!(
        moved, 0,
        "a bump that reloads the *same* pack must be a whole-frame no-op; {moved} \
         bytes changed, which means some pass is drawing from different objects \
         than it was before"
    );
}

/// **The whole `IconPart::Special` family shares one pass, and therefore one
/// fate.** Chest, shulker box, banner, shield and player head all draw in a
/// container slot, and all five go dark together when that pass is absent.
///
/// # The question this answers
///
/// A player head vanishing from inventory slots has two shapes, and they need
/// different fixes: either the whole special stream is dark (its lazy build
/// declined, so every kind is blank) or something is wrong with the head in
/// particular (its rig, its sheet, its pose). Those are distinguishable without
/// asking anyone to look at a screen — render all five and see whether they
/// stand or fall together.
///
/// The negative arm is the discriminating half and it is executed, not argued:
/// with the 3-D pass detached, `IconRenderer::prepare_special` declines, and all
/// five must read **exactly 0**. If a future change ever lets one of them draw
/// through that control, "the special pass is dark" has stopped being a
/// sufficient explanation for a blank slot and this gate says so by failing.
///
/// Note what the positive arm is *not* evidence of: it proves the renderer draws
/// these five given a real `ItemStack`, and says nothing about whether the
/// shipped client's build of that pass succeeds. A gate installs its own input.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn every_special_icon_kind_stands_or_falls_with_the_pass() {
    /// One slot each, on the inventory screen's hotbar row — real `menu_index`
    /// values. Pairwise-distinct items across the five `kind`s the resolver
    /// serves, so no two arms can coincide.
    const SUBJECTS: &[(usize, &str)] = &[
        (36, "minecraft:chest"),
        (37, "minecraft:red_shulker_box"),
        (38, "minecraft:white_banner"),
        (39, "minecraft:shield"),
        (40, "minecraft:player_head"),
    ];
    /// Left empty, as the localisation control.
    const BLANK_SLOT: usize = 43;

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

    let empty_menu = Menu::player();
    let mut menu = Menu::player();
    for (slot, item) in SUBJECTS {
        // The precondition: each subject must really reach the special stream.
        // Without this the gate could pass by measuring five flat sprites.
        let loc: ResourceLocation = item.parse().expect("valid item id");
        let icon = item_atlas
            .icon(&loc)
            .unwrap_or_else(|| panic!("{item} must resolve to an icon in the item atlas"));
        assert!(
            icon.parts
                .iter()
                .any(|p| matches!(p, lodestone_assets::IconPart::Special { .. })),
            "{item} must carry an IconPart::Special — if it has become a flat sprite \
             this gate is no longer measuring the special pass for it"
        );
        menu.set_slot_item(*slot, Some(ItemStack::new(id(item), 1)));
    }

    let frame = ContainerFrame::new(Some(&menu), "Inventory");
    let base_frame = ContainerFrame::new(Some(&empty_menu), "Inventory");

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
        render.model_atlas_view().expect("a model atlas"),
        render.model_atlas_sampler().expect("a model atlas sampler"),
        render.model_palette_buffer().expect("a tint palette"),
        render.model_anim_buffer().expect("animation slots"),
    );
    let chrome = shoot(&mut lit, &base_frame);
    let subject = shoot(&mut lit, &frame);

    // The pass declines when the 3-D stream is not attached, which is the same
    // gate a failed lazy build falls through — so this arm is what a dark
    // special stream looks like on this screen.
    let mut dark = ContainerRenderer::new(device, format);
    dark.attach_items(device, queue, format, item_atlas.clone());
    let control = shoot(&mut dark, &frame);

    let blank_rect = slot_rect(&menu, &frame, BLANK_SLOT);
    let mut drew = Vec::new();
    let mut leaked = Vec::new();
    eprintln!("=== every special icon kind, one pass ===");
    eprintln!("sheets loaded by the pass = {}", lit.special_icon_sheets());
    for (slot, item) in SUBJECTS {
        let rect = slot_rect(&menu, &frame, *slot);
        let lit_px = diff_in(&subject, &chrome, rect);
        let dark_px = diff_in(&control, &chrome, rect);
        eprintln!("  {item:<28} slot {slot}  lit = {lit_px:<4} dark = {dark_px}");
        if lit_px == 0 {
            drew.push(*item);
        }
        if dark_px != 0 {
            leaked.push((*item, dark_px));
        }
    }
    let blank_px = diff_in(&subject, &chrome, blank_rect);
    eprintln!("  (empty slot {BLANK_SLOT})                  lit = {blank_px}");

    // Collected, not asserted inside the loop: an `assert!` per iteration stops
    // at the first failure, which would report one kind and leave the other four
    // as arguments rather than observations — and "which of the five drew" is
    // the entire content of this gate.
    assert!(
        drew.is_empty(),
        "these special-renderer kinds drew nothing in a container slot while their \
         siblings did: {drew:?}. They share one pass, so a subset failing is NOT \
         'the special stream is dark' — it is something specific to those kinds"
    );
    assert!(
        leaked.is_empty(),
        "with the special pass declined, these kinds still painted: {leaked:?}. \
         Every one must read exactly 0, or a blank slot can no longer be \
         attributed to the pass being dark"
    );
    assert_eq!(
        blank_px, 0,
        "an empty slot must match the chrome baseline; {blank_px} changed pixels \
         means the five counts above are not localised to their own cells"
    );
}

/// A **custom head skin** — a `minecraft:player_head` whose `minecraft:profile`
/// carries a `textures` property — draws the *default* skull sheet in a
/// container slot, not its own texture and not nothing.
///
/// # What this establishes and why it matters which
///
/// "Custom heads don't render" has two shapes that point at different hops: the
/// slot could be **empty** (the icon never reached the pass) or it could hold a
/// **plain Steve head** (the icon reached the pass and lost its texture on the
/// way). This gate measures which, so nobody has to infer it from a screenshot.
///
/// The answer is the second: `container::builder::icon_record` builds the slot
/// record from the stack and carries every other per-stack component a slot
/// icon needs — `dyed_color`, `potion_color`, `banner_patterns`, `base_color` —
/// and does not carry `minecraft:profile`. `ItemIcon` has no field for it. So
/// `lodestone_render::special_item_rig` resolves the head to
/// `skull_texture_stem(SkullType::Player)`, the default sheet, and a custom head
/// is drawn as a plain one.
///
/// The world and first-person surfaces do not share the gap — a placed head
/// resolves its profile through `BlockEntityTexture::PlayerSkin` — so the same
/// head is correct when placed and plain in an inventory.
///
/// # The pinned equality is the defect, not the contract
///
/// The second assertion below requires the custom head to be **byte-identical**
/// to a plain one. That is a bug pinned deliberately, and it is written to fail
/// the moment someone threads the profile through: when the GUI path learns to
/// resolve a head's own texture, this assertion must be **inverted** to require
/// a difference. Do not relax it to a range — the point is that today the
/// difference is exactly zero.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_custom_head_skin_draws_the_default_sheet_in_a_container_slot() {
    const PLAIN_SLOT: usize = 36;
    const CUSTOM_SLOT: usize = 37;

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
        .expect("the vanilla pack must load; set LODESTONE_ASSETS");
    let models: &BlockModels = atlas.models().expect("baked block models");
    let item_atlas = load_item_atlas().expect("the item atlas must build");

    // A real custom head: a full profile carrying a signed `textures` property,
    // the exact shape `ClientboundContainerSetContent` delivers for a
    // server-placed decorative head. `lodestone-v770`'s own
    // `decodes_a_full_profile_with_signed_textures` pins that this is what
    // arrives off the wire, so the fixture is the wire's shape rather than one
    // invented here.
    let mut custom = ItemStack::new(id("minecraft:player_head"), 1);
    custom.set_profile(Some(lodestone_model::ItemProfile {
        name: Some("Notch".to_owned()),
        id: Some(uuid::Uuid::from_u128(0x0699_a79f_444e_9472_6a5b_efca_90e3_8aaf)),
        properties: vec![lodestone_model::ProfileProperty {
            name: "textures".to_owned(),
            value: "eyJ0ZXh0dXJlcyI6e319".to_owned(),
            signature: Some("sig-bytes".to_owned()),
        }],
    }));
    assert!(
        custom
            .profile()
            .is_some_and(|p| p.properties.iter().any(|q| q.name == "textures")),
        "the fixture must really carry a textures property, or this gate is \
         comparing two plain heads and can prove nothing"
    );

    let empty_menu = Menu::player();
    let mut menu = Menu::player();
    menu.set_slot_item(
        PLAIN_SLOT,
        Some(ItemStack::new(id("minecraft:player_head"), 1)),
    );
    menu.set_slot_item(CUSTOM_SLOT, Some(custom));

    let frame = ContainerFrame::new(Some(&menu), "Inventory");
    let base_frame = ContainerFrame::new(Some(&empty_menu), "Inventory");
    let plain_rect = slot_rect(&menu, &frame, PLAIN_SLOT);
    let custom_rect = slot_rect(&menu, &frame, CUSTOM_SLOT);

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
        render.model_atlas_view().expect("a model atlas"),
        render.model_atlas_sampler().expect("a model atlas sampler"),
        render.model_palette_buffer().expect("a tint palette"),
        render.model_anim_buffer().expect("animation slots"),
    );
    let chrome = shoot(&mut lit, &base_frame);
    let subject = shoot(&mut lit, &frame);

    let plain_lit = diff_in(&subject, &chrome, plain_rect);
    let custom_lit = diff_in(&subject, &chrome, custom_rect);

    // Cell-against-cell, so "the same picture" is checked directly rather than
    // inferred from two equal counts — two different faces can easily cover the
    // same number of pixels, which is exactly the coincidence a count-only
    // comparison would miss.
    let [px, py, pw, ph] = plain_rect;
    let [cx, cy, ..] = custom_rect;
    let mut differing = 0usize;
    for dy in 0..ph {
        for dx in 0..pw {
            let a = (((py + dy) * W + px + dx) * 4) as usize;
            let b = (((cy + dy) * W + cx + dx) * 4) as usize;
            if subject[a..a + 3] != subject[b..b + 3] {
                differing += 1;
            }
        }
    }

    eprintln!("=== a custom head skin in a container slot ===");
    eprintln!("plain  head slot {PLAIN_SLOT}  lit = {plain_lit} of 256");
    eprintln!("custom head slot {CUSTOM_SLOT}  lit = {custom_lit} of 256");
    eprintln!("pixels where the two cells differ = {differing}");

    assert!(
        custom_lit > 0,
        "a custom head drew nothing at all in its slot. That would mean the icon \
         never reached the special pass — a different and worse defect than losing \
         its texture, and it would move the search upstream of the draw"
    );
    assert_eq!(
        differing, 0,
        "a custom head is no longer pixel-identical to a plain one ({differing} px \
         differ). If you have just threaded minecraft:profile through to the GUI \
         icon path, this is the assertion that was pinning the bug: INVERT it to \
         require a difference rather than relaxing it"
    );
}
