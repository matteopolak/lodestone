//! Pixel gate: a **block** item in the hotbar must draw its 3-D icon.
//!
//! This is the island control for the item-GUI-geometry work.
//! `lodestone-render`'s `item_geometry_gate` proves the renderer can bake a
//! block item, pose it into a slot and keep its winding — but it drives the
//! geometry helpers directly, so it is a closed loop: every one of its
//! assertions stays green while the HUD ignores `IconPart::Model` entirely and
//! every hotbar cell renders an empty well. That was in fact the state of the
//! world before this gate existed — and it is far from the first subsystem in
//! this repo that was complete, tested, and reached zero pixels because nothing
//! consumed it. A crate's own suite structurally cannot see that.
//!
//! Only a test that drives the real [`HudRenderer`] can see that. So this one
//! does, through the same calls `app.rs` makes:
//!
//! ```text
//! ItemAtlas::icon -> IconPart::Model -> BlockModels::item
//!   -> gui_item_pose + mesh_item_quads -> HudRenderer's model pass -> pixels
//! ```
//!
//! # What is asserted, and why those numbers
//!
//! The hotbar's procedural (no-GUI-atlas) layout puts a **16 px** icon at
//! `(hx + 3, hy + 3)` with a 22 px pitch, where `hx = cx - 99` and
//! `hy = height - 28`. Under vanilla's `block/block` `display.gui` pose —
//! `rotation [30, 225, 0]`, `scale 0.625` — the unit cube's three visible faces
//! project to a hexagon whose area is the sum of the three faces' screen-space
//! parallelogram areas:
//!
//! ```text
//! A = sum over {(ex,ey), (ey,ez), (ez,ex)} of |u.x*v.y - u.y*v.x|
//!     where [ex ey ez] = S(16, -16, 16) * Rx(30) * Ry(225) * S(0.625)
//!   = 172.5 px^2   (in a 16x16 = 256 px cell; bbox 14.14 x 15.73 px)
//! ```
//!
//! which is the same 14.14 x 15.73 figure `docs/item-gui-geometry.md` derives.
//! Pixel centres are sampled once each with no MSAA, so the lit count must land
//! within a few pixels of that area. The band below is deliberately tight enough
//! that a *half*-drawn cube (three faces missing, ~86) or a mis-scaled one fails
//! it, where "greater than zero" would not.
//!
//! **The count alone cannot see an inside-out cube** — the honest statement of
//! its limits. If the winding flipped, the three *far* faces would survive
//! culling instead, and they project to the same hexagon and therefore the same
//! ~172 px. What distinguishes them is `gui_light: Side` shading: the correct
//! visible set is `{Up, East, North}` with `face_shade` `{1.0, 0.6, 0.8}`, and
//! the inside-out set is `{Down, West, South}` with `{0.5, 0.6, 0.8}`. Under
//! this pose the `Up`/`Down` face is the top of the hexagon and the side faces
//! are the bottom, so the top-band / bottom-band brightness ratio is ~1.4 when
//! correct and ~0.7 when flipped — a separation no silhouette measurement has.
//! That check is asserted below too. (The winding *matrices* are pinned
//! independently by `item_render::tests::winding_matches_the_world_camera` and
//! `item_geometry_gate`; this is the on-screen corroboration.)
//!
//! Two further controls keep the measurement honest:
//!
//! * **an empty cell** (slot 8, same row, 176 px along) must read exactly 0, so
//!   the count is localised rather than a full-screen blend leak;
//! * **no `attach_item_models`**, everything else identical, must read exactly 0
//!   — the executed proof that the new pass is what puts those pixels there and
//!   not some pre-existing draw.
//!
//! Fail-closed like its siblings: a missing GPU or a missing `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test hotbar_block_item_pixels -- --ignored --nocapture
//! ```

use lodestone::config::{calculate_gui_scale, AUTO_GUI_SCALE};
use lodestone::gpu::RenderState;
use lodestone::hud::{DebugStats, HotbarSlot, HudFrame, HudRenderer};
use lodestone::resources::{BlockResources, load_item_atlas};
use lodestone_assets::ResourceLocation;
use lodestone_render::{BlockModels, GpuContext, HeadlessTarget, RenderTarget};

/// `HudGeometry::build_inner` divides the physical framebuffer down to a
/// **logical** GUI canvas via `crate::menu::render::logical_canvas` before
/// laying `draw_hotbar_items`'s fixed pixel constants into it, then the render
/// pass stretches that canvas back out to fill the physical target — so a
/// pixel gate reading back the physical framebuffer must either convert
/// through that scale, or pick a size where the divide is a no-op. `(480,
/// 320)` is `hud.rs`'s own `hud_vitals_draw_the_real_heart_sprite` gate's
/// fixture, chosen for exactly this reason: it sits below vanilla's
/// 320-logical-pixel floor at any scale above 1, so
/// `calculate_gui_scale(AUTO, 480, 320) == 1` and `cell_rect` below needs no
/// separate physical<->logical conversion. Scale-diversity is still exercised
/// elsewhere in the suite —
/// `container_screen.rs`'s `hit_test_and_drawn_geometry_share_one_panel_origin`
/// runs the same panel-origin math at 1920x1080 (scale 4) — so this file
/// dropping to scale 1 does not leave the GUI-scaling code path unexercised.
const W: u32 = 480;
const H: u32 = 320;

/// The item under test: a full opaque cube whose faces are all one sprite, so
/// the silhouette area is exactly the analytic figure above with no cutout
/// texels to argue about.
const ITEM: &str = "minecraft:stone";

/// The analytic silhouette area of the vanilla block pose in a 16 px cell (see
/// the module docs). Rasterisation samples pixel centres, so the observed count
/// tracks this closely but is not bound to it exactly.
const EXPECTED_LIT: f32 = 172.5;

/// The `(x, y)` pixel origin of hotbar cell `i` and the icon size, mirroring
/// `hud::draw_hotbar_items`' procedural branch (no GUI atlas attached). Reads
/// `W`/`H` directly as the layout canvas: valid only because `W`x`H` is chosen
/// so `calculate_gui_scale(AUTO, W, H) == 1`, verified in the test body below
/// rather than assumed here.
fn cell_rect(i: u32) -> [u32; 4] {
    let cx = W as f32 * 0.5;
    let cell = 22.0f32;
    let hx = cx - 9.0 * cell * 0.5;
    let hy = H as f32 - 6.0 - cell;
    let x = hx + 3.0 + i as f32 * cell;
    let y = hy + 3.0;
    [x as u32, y as u32, 16, 16]
}

/// Paint `view` a flat colour, so "lit" below means "something drew here".
fn clear_view(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView, rgb: [u8; 3]) {
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
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(rgb[0]) / 255.0,
                        g: f64::from(rgb[1]) / 255.0,
                        b: f64::from(rgb[2]) / 255.0,
                        a: 1.0,
                    }),
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

/// Pixels inside `rect` that are not the (black) backdrop.
fn lit_in(pixels: &[u8], rect: [u32; 4]) -> usize {
    let [rx, ry, rw, rh] = rect;
    let mut lit = 0usize;
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            if brightness(pixels, x, y) > 20 {
                lit += 1;
            }
        }
    }
    lit
}

/// Max colour channel at `(x, y)` — "how lit is this pixel".
fn brightness(pixels: &[u8], x: u32, y: u32) -> u32 {
    let i = ((y * W + x) * 4) as usize;
    u32::from(pixels[i].max(pixels[i + 1]).max(pixels[i + 2]))
}

/// Mean brightness of the lit pixels in rows `ry + band` of `rect`, or `None`
/// when the band is empty. Used to compare the horizontal face (top of the
/// hexagon) against the vertical ones (bottom), which is what tells `Up` from
/// `Down` and therefore a correctly wound cube from an inside-out one.
fn band_mean(pixels: &[u8], rect: [u32; 4], rows: std::ops::Range<u32>) -> Option<f32> {
    let [rx, ry, rw, _] = rect;
    let (mut sum, mut n) = (0u32, 0u32);
    for y in rows {
        for x in rx..rx + rw {
            let b = brightness(pixels, x, ry + y);
            if b > 20 {
                sum += b;
                n += 1;
            }
        }
    }
    (n > 0).then(|| sum as f32 / n as f32)
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_block_item_in_the_hotbar_reaches_pixels() {
    // `cell_rect` hand-derives its rect straight from `W`/`H` with no
    // physical<->logical conversion, which is only correct while the divide
    // `HudGeometry::build_inner` performs is a no-op. Assert the precondition
    // rather than silently trusting it — a future change to `W`/`H` that
    // broke this would otherwise sample the wrong screen region and either
    // false-fail or, worse, false-pass by accident.
    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "cell_rect assumes W x H divides to itself under the GUI scale; if this \
         fails, cell_rect must convert its rect through the scale explicitly"
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

    // The real production load: `client.jar` + `blocks.json` -> BlockAtlas with
    // baked models attached. This is the same call `Sim::new` makes.
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
    let item: ResourceLocation = ITEM.parse().expect("valid item id");
    assert!(
        models.item(&item).is_some(),
        "{ITEM} must have baked 3-D inventory geometry; without it this gate would \
         be measuring the absence of an item rather than the absence of a draw"
    );
    let item_atlas =
        load_item_atlas().expect("the item atlas must build from the same client.jar");
    assert!(
        item_atlas.icon(&item).is_some(),
        "{ITEM} must resolve to an icon; the HUD reaches the model geometry through \
         the atlas's cached IconPart::Model"
    );

    let mut target = HeadlessTarget::new(device, W, H, format);
    // The world renderer is here for its *resources*, not its terrain: the HUD's
    // item pass borrows its block atlas, tint palette, animation slots and depth
    // buffer. Nothing is uploaded twice.
    let render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    // One stone in slot 0, everything else empty. `hotbar: None` suppresses the
    // procedural hotbar frame, and `count: 1` suppresses the stack digits, so the
    // only thing that can paint inside a cell is the icon itself.
    let slots: Vec<Option<HotbarSlot>> = std::iter::once(Some(HotbarSlot {
        item: item.clone(),
        count: 1,
        damage: None,
        max_damage: None,
        enchanted: false,
    }))
    .chain(std::iter::repeat_with(|| None).take(8))
    .collect();

    let stats = DebugStats::default();
    let hud_frame = HudFrame {
        show_debug: false,
        crosshair: false,
        hotbar: None,
        hotbar_items: Some(&slots),
        ..HudFrame::new(&stats)
    };

    // Render one frame with `hud` over a black backdrop and read it back.
    let mut shoot = |hud: &mut HudRenderer| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            Some(render.depth_view()),
            &hud_frame,
            Some(models),
            // Derived, not hardcoded to 1: the test asserts this fixture's scale
            // is 1 so the canvas divide is a no-op, and deriving it here means a
            // future fixture-size change cannot silently desync draw from layout.
            calculate_gui_scale(AUTO_GUI_SCALE, W, H),
            W,
            H,
        );
        target.read_texels(device, queue)
    };

    // Subject: the full wiring, exactly as `app.rs` builds it.
    let mut lit_hud = HudRenderer::new(device, format);
    lit_hud.attach_items(device, queue, format, item_atlas.clone());
    lit_hud.attach_item_models(
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
    let subject = shoot(&mut lit_hud);

    // Control: identical in every respect except that the item-model pass was
    // never attached, so the geometry has nowhere to draw.
    let mut dark_hud = HudRenderer::new(device, format);
    dark_hud.attach_items(device, queue, format, item_atlas.clone());
    let control = shoot(&mut dark_hud);

    let filled = cell_rect(0);
    let empty = cell_rect(8);
    let subject_filled = lit_in(&subject, filled);
    let subject_empty = lit_in(&subject, empty);
    let control_filled = lit_in(&control, filled);
    // Rows 1..5 of the 16 px cell are entirely the horizontal face (the top of
    // the isometric hexagon); rows 11..15 are entirely side faces.
    let top_mean = band_mean(&subject, filled, 1..5).expect("the icon must light its top rows");
    let side_mean =
        band_mean(&subject, filled, 11..15).expect("the icon must light its bottom rows");

    eprintln!("=== hotbar block-item pixel gate ===");
    eprintln!("item                 = {ITEM}");
    eprintln!("cell rect (slot 0)   = {filled:?}");
    eprintln!("cell rect (slot 8)   = {empty:?}");
    eprintln!("expected silhouette  = {EXPECTED_LIT:.1} px of 256");
    eprintln!("lit, slot 0 (block)  = {subject_filled}");
    eprintln!("lit, slot 8 (empty)  = {subject_empty}");
    eprintln!("lit, slot 0 (no item-model pass attached) = {control_filled}");
    eprintln!("top-band mean         = {top_mean:.1} (Up face, face_shade 1.0)");
    eprintln!("bottom-band mean      = {side_mean:.1} (side faces, 0.6/0.8)");
    eprintln!("ratio                 = {:.2}", top_mean / side_mean);

    // The load-bearing assertion: real pixels, in the right place, in the right
    // quantity for a correctly posed isometric cube.
    let low = (EXPECTED_LIT * 0.85) as usize;
    let high = (EXPECTED_LIT * 1.15) as usize;
    assert!(
        (low..=high).contains(&subject_filled),
        "a block item's icon must cover ~{EXPECTED_LIT:.0} of the 256 px cell \
         (the {}x{} silhouette of the vanilla [30,225,0]/0.625 pose); got \
         {subject_filled}. Far below means faces are missing or the pass never \
         drew; far above means the pose or the ortho is wrong.",
        14.14, 15.73
    );

    // Orientation: the horizontal face on top must be the **bright** one. An
    // inside-out cube has the same silhouette and the same pixel count, but its
    // top band is the `Down` face at half shade, so this ratio inverts.
    assert!(
        top_mean > side_mean * 1.15,
        "the top of the icon must be the full-shade Up face, not the half-shade \
         Down face: top={top_mean:.1} side={side_mean:.1}. A ratio at or below 1 \
         means the winding flipped and you are seeing the inside of the cube — \
         which looks like a plausible isometric block in a screenshot"
    );

    // Localisation: an untouched cell on the same row must be untouched.
    assert_eq!(
        subject_empty, 0,
        "an empty hotbar cell must stay black; {subject_empty} lit pixels there \
         means the draw is not localised to its slot and the count above is not \
         measuring what it claims"
    );

    // The executed negative control.
    assert_eq!(
        control_filled, 0,
        "without attach_item_models the same frame must draw nothing in the cell; \
         {control_filled} lit pixels means something else is painting there and the \
         positive assertion is not evidence for the new pass"
    );
}
