//! Pixel gate: a **chest** item in the hotbar must draw its real block-entity
//! geometry.
//!
//! The bug this exists to hold shut was an island of the ordinary kind.
//! `lodestone-assets` classified chest, shulker box, banner, shield and the rest
//! of the ex-`builtin/entity` family as `IconPart::Special`, carried the special
//! `kind`, and had a test asserting that seam — and the GUI's match arm was
//! literally `IconPart::Special { .. } => {}`. Every crate-local test stayed
//! green while a crafted chest was invisible in the inventory, the hotbar and
//! everywhere else. A user found it by playing.
//!
//! So this drives the real [`HudRenderer`] through the same calls `app.rs`
//! makes:
//!
//! ```text
//! ItemAtlas::icon -> IconPart::Special { kind: "minecraft:chest" }
//!   -> special_icon_geometry -> BlockEntityModelSet::get("chest_single")
//!   -> BlockEntityMesh::part_transforms(gui_item_pose(...), &[])
//!   -> EntityPipeline, inside IconRenderer::draw_models' depth-clearing pass
//!   -> pixels
//! ```
//!
//! # What is asserted, and what each assertion can and cannot see
//!
//! Every expected number is **derived at run time from the same expressions the
//! draw uses** — the baked mesh's own AABB through the real `gui_item_pose` over
//! the real `display.gui` the item atlas resolved — rather than restated as a
//! constant. A hand-copied constant is how a gate ends up measuring a rect 20
//! logical pixels away from a row that was drawing perfectly.
//!
//! * **The lit bounding box** must match the projected AABB. This is the pose
//!   check: a wrong scale, a wrong rotation, or the `-0.5` centring dropped all
//!   move or resize this box. Reported as a box, never as a percentage, because a
//!   fraction cannot tell a uniform-but-wrong frame from a localised blob.
//! * **The lit count** must match the analytic silhouette of a box under this
//!   pose — the sum of the three visible faces' screen-space parallelogram areas.
//!   *This is the discriminator between real geometry and a flat sprite.* A
//!   screen-aligned quad fills its own bounding box; a rotated box projects to a
//!   hexagon and cannot. `hexagon_is_distinguishable_from_a_flat_quad` below
//!   asserts that premise **before** the measurement is believed, so the band is
//!   known to exclude the flat-quad prediction rather than merely being assumed
//!   to.
//!
//!   Being explicit about the claim: this gate distinguishes *geometry* from
//!   *flat quad* and from *nothing*. It does **not** claim to distinguish the
//!   chest sheet from some other entity sheet of similar brightness.
//! * **Shading variation** — the mean brightness of the top rows against the
//!   bottom rows must differ. A flat unshaded quad has one population; three
//!   faces at three angles under the entity shader's directional light have more
//!   than one. (Unlike the block-item gate, this is *not* an inside-out check:
//!   `EntityPipeline` is `cull_mode: None`, so winding does not decide visibility
//!   on this path at all and there is no polarity to assert.)
//!
//! # What else paints in a hotbar cell
//!
//! Asked before the controls were believed, because a control's premise can be
//! false in the safe-looking direction. An inventory frame is full of ink: panel
//! art, slot backgrounds, other stacks' sprites, stack-count glyphs, durability
//! bars. This fixture removes all of it — `hotbar: None` suppresses the
//! procedural frame, `count: 1` suppresses the stack digits, `damage: None`
//! suppresses the durability bar, and eight of the nine slots are empty. The
//! chest itself contributes **no** sprite verts either, which
//! `the_base_sprite_fallback_is_vacuous_for_every_special_kind` establishes
//! rather than assumes. So inside cell 0 the only thing that can paint is the
//! block-entity draw under test.
//!
//! Two controls keep that honest:
//!
//! * **an empty cell** (slot 8, same row) must read exactly 0 — the count is
//!   localised, not a full-screen blend leak;
//! * **no `attach_item_models`**, everything else identical, must read exactly 0
//!   — the executed proof that the new pass puts those pixels there. The special
//!   pass is deliberately gated on the same `models_attached()` signal, so this
//!   one call removes it.
//!
//! Fail-closed like its siblings: a missing GPU or a missing `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test hotbar_special_item_pixels -- --ignored --nocapture
//! ```

use glam::{Mat4, Vec3};
use lodestone::config::{AUTO_GUI_SCALE, calculate_gui_scale};
use lodestone::gpu::RenderState;
use lodestone::hud::{DebugStats, HotbarSlot, HudFrame, HudRenderer};
use lodestone::resources::{BlockResources, load_block_entity_textures, load_item_atlas};
use lodestone_assets::{DisplaySlot, IconPart, ResourceLocation};
use lodestone_render::{
    BlockEntityModelSet, BlockModels, CHEST_SINGLE, GpuContext, HeadlessTarget, RenderTarget,
    SKULL_HUMANOID, gui_item_pose,
};

/// Chosen so `calculate_gui_scale(AUTO, W, H) == 1` and the logical-canvas divide
/// `HudGeometry::build_inner` performs is a no-op, exactly as
/// `hotbar_block_item_pixels.rs` does and for the same reason. Asserted in the
/// test body rather than trusted here.
const W: u32 = 480;
const H: u32 = 320;

/// The item under test. A plain chest: `kind` `minecraft:chest`, sheet
/// `entity/chest/normal`, the single-chest layer.
const ITEM: &str = "minecraft:chest";

/// One representative item per special `kind` present in 26.2, for the
/// fallback-is-vacuous check. Ten kinds over 91 item definitions; these are the
/// ones a `kind`-keyed fix has to account for, and the reason a fix keyed on the
/// chest *item* would have read as done while most of the family stayed dark.
const FAMILY: &[&str] = &[
    "minecraft:chest",
    "minecraft:trapped_chest",
    "minecraft:ender_chest",
    "minecraft:black_shulker_box",
    "minecraft:white_banner",
    "minecraft:shield",
    "minecraft:decorated_pot",
    "minecraft:creeper_head",
    "minecraft:player_head",
    "minecraft:conduit",
];

/// The `(x, y)` pixel origin of hotbar cell `i` and the icon size, mirroring
/// `hud::draw_hotbar_items`' procedural branch (no GUI atlas attached).
fn cell_rect(i: u32) -> [u32; 4] {
    let cx = W as f32 * 0.5;
    let cell = 22.0f32;
    let hx = cx - 9.0 * cell * 0.5;
    // `hud::HOTBAR_MARGIN`, not a restated `6.0`: this gate went red when the
    // hotbar was moved flush with the bottom of the screen to match vanilla's own
    // `guiHeight - 22` blit, and a local copy of the number is why.
    let hy = H as f32 - lodestone::hud::HOTBAR_MARGIN - cell;
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

/// Max colour channel at `(x, y)` — "how lit is this pixel".
fn brightness(pixels: &[u8], x: u32, y: u32) -> u32 {
    let i = ((y * W + x) * 4) as usize;
    u32::from(pixels[i].max(pixels[i + 1]).max(pixels[i + 2]))
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

/// The bounding box of the lit pixels inside `rect`, as `(x0, y0, x1, y1)`
/// inclusive, or `None` when nothing is lit.
///
/// **A box, not a percentage.** Two of this repo's false controls were diagnosed
/// in one step by printing a bounding box instead of a fraction: a fraction
/// cannot say *where*, and "3.5% lit" turned out to be the first-person bare arm
/// rather than the subject under test.
fn lit_bbox(pixels: &[u8], rect: [u32; 4]) -> Option<(u32, u32, u32, u32)> {
    let [rx, ry, rw, rh] = rect;
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            if brightness(pixels, x, y) > 20 {
                bbox = Some(match bbox {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    bbox
}

/// Mean brightness of the lit pixels in rows `rect.y + rows` — `None` when the
/// band is empty.
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

/// Mean `[r, g, b]` (0..255) of the lit (non-backdrop) pixels inside `rect`, or
/// `None` when nothing is lit — the per-channel sibling of [`lit_in`], for a
/// gate that has to tell *which colour* drew, not only *whether* something did.
fn mean_rgb_in(pixels: &[u8], rect: [u32; 4]) -> Option<[f32; 3]> {
    let [rx, ry, rw, rh] = rect;
    let (mut sum, mut n) = ([0u32; 3], 0u32);
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            if brightness(pixels, x, y) > 20 {
                let i = ((y * W + x) * 4) as usize;
                sum[0] += u32::from(pixels[i]);
                sum[1] += u32::from(pixels[i + 1]);
                sum[2] += u32::from(pixels[i + 2]);
                n += 1;
            }
        }
    }
    (n > 0).then(|| [sum[0] as f32 / n as f32, sum[1] as f32 / n as f32, sum[2] as f32 / n as f32])
}

/// The 2-D screen-space cross product, i.e. the signed parallelogram area of two
/// projected edge vectors.
fn cross2(u: Vec3, v: Vec3) -> f32 {
    u.x * v.y - u.y * v.x
}

/// The analytic screen-space silhouette of an axis-aligned box under `pose`.
///
/// A box's projection is a hexagon whose area is the sum of the three visible
/// faces' parallelogram areas — the same derivation
/// `hotbar_block_item_pixels.rs` uses for the unit cube, generalised to the
/// chest's actual extent. `pose` is affine, so an edge of the box maps to a fixed
/// screen vector regardless of which corner it starts from.
fn silhouette_area(pose: Mat4, lo: Vec3, hi: Vec3) -> f32 {
    let d = hi - lo;
    let ex = pose.transform_vector3(Vec3::new(d.x, 0.0, 0.0));
    let ey = pose.transform_vector3(Vec3::new(0.0, d.y, 0.0));
    let ez = pose.transform_vector3(Vec3::new(0.0, 0.0, d.z));
    cross2(ex, ey).abs() + cross2(ey, ez).abs() + cross2(ez, ex).abs()
}

/// The projected axis-aligned bounding box of the eight corners of `lo..hi` under
/// `pose`, as `(x0, y0, x1, y1)` in GUI pixels.
fn projected_bbox(pose: Mat4, lo: Vec3, hi: Vec3) -> (f32, f32, f32, f32) {
    let mut out = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for cx in [lo.x, hi.x] {
        for cy in [lo.y, hi.y] {
            for cz in [lo.z, hi.z] {
                let p = pose.transform_point3(Vec3::new(cx, cy, cz));
                out.0 = out.0.min(p.x);
                out.1 = out.1.min(p.y);
                out.2 = out.2.max(p.x);
                out.3 = out.3.max(p.y);
            }
        }
    }
    out
}

/// The `base` sprite fallback that fix offered as the cheap option **cannot draw
/// anything**, for the whole family and not just for chest.
///
/// This is the measurement that decided the approach, so it is a test and not a
/// comment. Every one of the ten special `base` models in 26.2 — `item/chest`,
/// `item/template_banner`, `item/shield`, `item/template_skull`,
/// `item/decorated_pot`, … — has **no `elements` and no `layer0`**, only a
/// `particle` texture naming a *block* texture (`block/oak_planks` for a chest,
/// `block/soul_sand` for a skull) that is not in the item atlas at all. So
/// `classify_model` yields no `IconPart::Sprite`, and "fall back to the base
/// sprite" would have shipped the same zero pixels under a different match arm.
///
/// Asserted through the real production resolver, not a fixture: the point is
/// what the *jar* contains.
#[test]
#[ignore = "requires the vanilla client.jar"]
fn the_base_sprite_fallback_is_vacuous_for_every_special_kind() {
    let atlas = load_item_atlas().expect(
        "the item atlas must build from client.jar; set LODESTONE_ASSETS to a pack root \
         — do NOT treat a missing jar as a pass",
    );

    let mut checked = 0usize;
    for id in FAMILY {
        let item: ResourceLocation = id.parse().expect("valid item id");
        let icon = atlas
            .icon(&item)
            .unwrap_or_else(|| panic!("{id} must resolve to an icon in the item atlas"));

        let specials = icon
            .parts
            .iter()
            .filter(|p| matches!(p, IconPart::Special { .. }))
            .count();
        let sprites = icon
            .parts
            .iter()
            .filter(|p| matches!(p, IconPart::Sprite { .. }))
            .count();
        eprintln!("{id:34} parts={} special={specials} sprite={sprites}", icon.parts.len());

        assert!(
            specials > 0,
            "{id} must resolve to an IconPart::Special — this gate's whole subject is \
             the special family, and an id that stopped being special would make the \
             rest of this file measure something unrelated"
        );
        assert_eq!(
            sprites, 0,
            "{id} resolved a flat IconPart::Sprite, which would mean #369's cheap \
             'draw the base sprite' route was viable after all and this file's stated \
             reason for taking the geometry route is wrong. Re-derive before trusting \
             either path."
        );
        checked += 1;
    }
    assert_eq!(
        checked,
        FAMILY.len(),
        "every representative must have been checked; a short loop here would make \
         the conclusion narrower than it claims"
    );
}

/// The premise the pixel count's discriminating power rests on: for this pose,
/// the hexagonal silhouette of the chest is **materially smaller** than its own
/// bounding box, so "lit ≈ silhouette" excludes "lit ≈ a flat quad filling the
/// bbox".
///
/// Checked separately, and before the GPU gate is believed, because a control
/// whose premise is false fails in the *safe*-looking direction: if the two
/// predictions happened to coincide, the count assertion below would still pass
/// on a flat sprite and the gate would look rigorous while measuring nothing.
#[test]
#[ignore = "requires the vanilla client.jar"]
fn hexagon_is_distinguishable_from_a_flat_quad() {
    let (pose, lo, hi) = chest_pose_and_extent();
    let area = silhouette_area(pose, lo, hi);
    let (x0, y0, x1, y1) = projected_bbox(pose, lo, hi);
    let bbox_area = (x1 - x0) * (y1 - y0);

    eprintln!("silhouette   = {area:.1} px^2");
    eprintln!("bbox         = {:.2} x {:.2} = {bbox_area:.1} px^2", x1 - x0, y1 - y0);
    eprintln!("ratio        = {:.3}", area / bbox_area);

    assert!(
        area < bbox_area * 0.92,
        "the chest's silhouette ({area:.1}) must be materially smaller than its \
         bounding box ({bbox_area:.1}) for the pixel count to tell geometry from a \
         flat quad. At ratio {:.3} the two predictions are too close and the count \
         assertion in the GPU gate would pass on either.",
        area / bbox_area
    );
    assert!(
        area > 1.0 && bbox_area > 1.0,
        "a degenerate pose ({area:.1} px^2 in a {bbox_area:.1} px^2 box) would make \
         every downstream assertion vacuously satisfiable"
    );
}

/// The same premise check as [`hexagon_is_distinguishable_from_a_flat_quad`],
/// for the player head: the composed pose (`display.gui` plus the node's own
/// `"transformation"`) must still leave the silhouette materially smaller than
/// its bounding box, or [`a_player_head_item_in_the_hotbar_reaches_pixels`]'s
/// area band cannot distinguish real geometry from a flat quad.
#[test]
#[ignore = "requires the vanilla client.jar"]
fn player_head_silhouette_is_distinguishable_from_a_flat_quad() {
    let (pose, lo, hi) = special_pose_and_extent("minecraft:player_head", SKULL_HUMANOID);
    let area = silhouette_area(pose, lo, hi);
    let (x0, y0, x1, y1) = projected_bbox(pose, lo, hi);
    let bbox_area = (x1 - x0) * (y1 - y0);

    eprintln!("silhouette   = {area:.1} px^2");
    eprintln!("bbox         = {:.2} x {:.2} = {bbox_area:.1} px^2", x1 - x0, y1 - y0);
    eprintln!("ratio        = {:.3}", area / bbox_area);

    assert!(
        area < bbox_area * 0.95,
        "the player head's silhouette ({area:.1}) must be materially smaller than its \
         bounding box ({bbox_area:.1}) for the pixel count to tell geometry from a flat \
         quad. At ratio {:.3} the two predictions are too close and the count assertion \
         in the GPU gate would pass on either.",
        area / bbox_area
    );
    assert!(
        area > 1.0 && bbox_area > 1.0,
        "a degenerate pose ({area:.1} px^2 in a {bbox_area:.1} px^2 box) would make every \
         downstream assertion vacuously satisfiable"
    );
}

/// An item's GUI pose and baked extent, resolved through the **production**
/// path: the item atlas's own `display.gui` and the baked
/// [`BlockEntityModelSet`]'s own AABB, not numbers copied out of either.
///
/// Parametrized over `(item, model_name)` so the chest gate and the player-head
/// gate below share one derivation rather than each hand-copying it — the same
/// discipline the module doc asks of every expected number in this file.
fn special_pose_and_extent(item: &str, model_name: &'static str) -> (Mat4, Vec3, Vec3) {
    let atlas = load_item_atlas().expect("the item atlas must build from client.jar");
    let item: ResourceLocation = item.parse().expect("valid item id");
    let icon = atlas.icon(&item).expect("item must resolve to an icon");
    let transform = icon.display.get(DisplaySlot::Gui);
    // The node's own `"transformation"` (only the skull family carries one) —
    // read from the same `IconPart::Special` `push_special_icon` reads, so this
    // prediction and the production draw cannot disagree about which node field
    // fed it.
    let node_transformation = icon.parts.iter().find_map(|p| match p {
        IconPart::Special { transformation, .. } => *transformation,
        _ => None,
    });

    let models = BlockEntityModelSet::load();
    let mesh = models.get(model_name).unwrap_or_else(|| {
        panic!(
            "the baked corpus must contain {model_name}; without it this gate would be \
             measuring the absence of a model rather than the absence of a draw"
        )
    });

    let [rx, ry, rw, rh] = cell_rect(0);
    let rect = [rx as f32, ry as f32, rw as f32, rh as f32];
    let outer = gui_item_pose(rect, &transform);
    (
        lodestone_render::compose_special_node_transform(outer, node_transformation),
        mesh.local_min,
        mesh.local_max,
    )
}

/// The chest's GUI pose and baked extent — [`special_pose_and_extent`] pinned to
/// this file's chest fixture, kept as its own name so the existing chest gate
/// below reads unchanged.
fn chest_pose_and_extent() -> (Mat4, Vec3, Vec3) {
    special_pose_and_extent(ITEM, CHEST_SINGLE)
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_chest_item_in_the_hotbar_reaches_pixels() {
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
    // sRGB, like the live surface: the entity shader's gamma-space shade/tint
    // round-trip is written for an sRGB target, and the chest sheet is uploaded
    // as `Rgba8UnormSrgb` to match.
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    // --- premises, each of which would silently hollow out the measurement ----

    let item: ResourceLocation = ITEM.parse().expect("valid item id");
    let item_atlas = load_item_atlas().expect("the item atlas must build from client.jar");
    let icon = item_atlas
        .icon(&item)
        .expect("chest must resolve to an icon in the item atlas");
    // *The* world-species guard. A colour fix once measured byte-identical
    // because it was verified against the one scene that structurally could not
    // exercise it. Assert that this fixture's input really contains the structure
    // the code under test exists to handle — a `Special` part carrying the chest
    // `kind` — rather than assuming the atlas produced one.
    let kind = icon
        .parts
        .iter()
        .find_map(|p| match p {
            IconPart::Special { kind, .. } => Some(kind.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "{ITEM} must resolve to an IconPart::Special; got {:?}. Without that this \
                 gate renders an item the special pass never sees and proves nothing.",
                icon.parts
            )
        });
    assert_eq!(
        kind, "minecraft:chest",
        "the fix is keyed on `kind`, so the kind is load-bearing input"
    );
    // The sheets must exist on disk, separately from whether the pass loaded
    // them: this separates "the pack has no chest textures" from "the pass never
    // built", which are different bugs with the same pixel count.
    let sheets_on_disk = load_block_entity_textures().len();
    assert!(
        sheets_on_disk > 0,
        "no block-entity sheets decoded from the jar; a chest could not draw for \
         reasons that have nothing to do with the wiring under test"
    );

    let (pose, lo, hi) = chest_pose_and_extent();
    let expected_area = silhouette_area(pose, lo, hi);
    let (px0, py0, px1, py1) = projected_bbox(pose, lo, hi);

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
    // Deliberately the *opposite* of the block-item gate's premise: a chest has
    // no baked block-item geometry, which is exactly why `IconPart::Model` could
    // never have covered it and a second pass had to exist.
    assert!(
        models.item(&item).is_none(),
        "{ITEM} unexpectedly has baked block-item geometry. If that is now true, the \
         chest can reach pixels through the *model* stream and this whole pass may be \
         redundant — re-derive before trusting either."
    );

    let mut target = HeadlessTarget::new(device, W, H, format);
    let render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    // One chest in slot 0, everything else empty. `hotbar: None` suppresses the
    // procedural frame, `count: 1` the stack digits, `damage: None` the
    // durability bar — so nothing but the icon can paint inside a cell.
    let slots: Vec<Option<HotbarSlot>> = std::iter::once(Some(HotbarSlot {
        item: item.clone(),
        count: 1,
        damage: None,
        max_damage: None,
        enchanted: false,
        dyed_color: None,
        potion_color: None,
        banner_patterns: Vec::new(),
        base_color: None,
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

    let mut shoot = |hud: &mut HudRenderer| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        let raw_view = frame.create_view(target.raw_view_format());
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            &raw_view,
            Some(render.depth_view()),
            &hud_frame,
            Some(models),
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
    let sheets_in_pass = lit_hud.special_icon_sheets();

    // Control: identical but for `attach_item_models`, which gates both 3-D
    // passes — so the block-entity geometry has nowhere to draw.
    let mut dark_hud = HudRenderer::new(device, format);
    dark_hud.attach_items(device, queue, format, item_atlas.clone());
    let control = shoot(&mut dark_hud);

    let filled = cell_rect(0);
    let empty = cell_rect(8);
    let subject_filled = lit_in(&subject, filled);
    let subject_empty = lit_in(&subject, empty);
    let control_filled = lit_in(&control, filled);
    let bbox = lit_bbox(&subject, filled);

    eprintln!("=== hotbar special-item (chest) pixel gate ===");
    eprintln!("item                  = {ITEM}  kind = {kind}");
    eprintln!("cell rect (slot 0)    = {filled:?}");
    eprintln!("cell rect (slot 8)    = {empty:?}");
    eprintln!("sheets on disk        = {sheets_on_disk}");
    eprintln!("sheets loaded by pass = {sheets_in_pass}");
    eprintln!("mesh AABB (blocks)    = {lo:?} .. {hi:?}");
    eprintln!(
        "projected bbox        = ({px0:.2}, {py0:.2}) .. ({px1:.2}, {py1:.2})  \
         [{:.2} x {:.2}]",
        px1 - px0,
        py1 - py0
    );
    eprintln!("expected silhouette   = {expected_area:.1} px of 256");
    eprintln!("lit, slot 0 (chest)   = {subject_filled}");
    eprintln!("lit bbox, slot 0      = {bbox:?}");
    eprintln!("lit, slot 8 (empty)   = {subject_empty}");
    eprintln!("lit, slot 0 (no item-model pass attached) = {control_filled}");

    // The pass has to have actually built, or "some pixels drew" could be
    // anything at all.
    assert!(
        sheets_in_pass > 0,
        "the special-icon pass reported {sheets_in_pass} sheets after a frame \
         containing a chest — it never built, so whatever painted in the cell is not \
         it. ({sheets_on_disk} sheets are decodable from the jar, so this is a wiring \
         failure and not a missing pack.)"
    );

    // --- where: the bounding box, derived from the draw's own expressions ------

    let Some((bx0, by0, bx1, by1)) = bbox else {
        panic!(
            "nothing drew in hotbar cell 0 for a chest. Expected ~{expected_area:.0} lit \
             px inside ({px0:.1}, {py0:.1})..({px1:.1}, {py1:.1}). This is the original \
             #369 symptom: IconPart::Special reaching an empty match arm."
        );
    };
    // 1.5 px of slack each way: pixel centres are sampled once with no MSAA, so
    // an edge texel can fall either side of the analytic boundary.
    let tol = 1.5f32;
    for (label, observed, expected) in [
        ("x0", bx0 as f32, px0),
        ("y0", by0 as f32, py0),
        ("x1", bx1 as f32, px1),
        ("y1", by1 as f32, py1),
    ] {
        assert!(
            (observed - expected).abs() <= tol + 1.0,
            "the chest's lit {label} is {observed:.1} but `gui_item_pose` over the baked \
             mesh AABB puts it at {expected:.1}. Lit box ({bx0}, {by0})..({bx1}, {by1}); \
             predicted ({px0:.1}, {py0:.1})..({px1:.1}, {py1:.1}). A whole-box offset \
             means the pose composition differs from the one the prediction used; a \
             resize means the scale or the `-0.5` centring does."
        );
    }

    // --- how much: geometry, not a flat quad ---------------------------------

    let low = (expected_area * 0.80) as usize;
    let high = (expected_area * 1.20) as usize;
    let bbox_area = (px1 - px0) * (py1 - py0);
    assert!(
        (low..=high).contains(&subject_filled),
        "a chest icon must cover ~{expected_area:.0} px of the 256 px cell — the \
         hexagonal silhouette of the baked chest AABB under the vanilla [30,45,0]/0.625 \
         pose — got {subject_filled} in box ({bx0}, {by0})..({bx1}, {by1}). Far below \
         means faces are missing or the draw never ran; near {bbox_area:.0} means \
         something is filling the bounding box, i.e. a flat quad rather than geometry."
    );
    // Stated as its own assertion so the failure message names the specific
    // wrong-implementation it excludes, rather than leaving that to the band.
    assert!(
        (subject_filled as f32) < bbox_area * 0.95,
        "{subject_filled} lit px fills essentially the whole {bbox_area:.0} px bounding \
         box. A rotated box projects to a hexagon and leaves its bbox corners empty; \
         only a screen-aligned quad fills it. This reads as the flat-sprite fallback, \
         not the block-entity geometry."
    );

    // --- shading: more than one population, i.e. lit 3-D faces ---------------

    let top_mean = band_mean(&subject, filled, 1..5).expect("the icon must light its top rows");
    let low_mean = band_mean(&subject, filled, 11..15).expect("the icon must light its bottom rows");
    eprintln!("top-band mean         = {top_mean:.1}");
    eprintln!("bottom-band mean      = {low_mean:.1}");
    eprintln!("ratio                 = {:.2}", top_mean / low_mean);
    assert!(
        (top_mean - low_mean).abs() > 4.0,
        "the top and bottom bands read {top_mean:.1} and {low_mean:.1} — indistinguishable. \
         Three faces at three angles under the entity shader's directional light must \
         produce more than one brightness population; a single flat one suggests an \
         unshaded quad or a single face."
    );

    // --- controls -------------------------------------------------------------

    assert_eq!(
        subject_empty, 0,
        "an empty hotbar cell must stay black; {subject_empty} lit pixels there means \
         the draw is not localised to its slot and the count above is not measuring \
         what it claims"
    );
    assert_eq!(
        control_filled, 0,
        "without attach_item_models the same frame must draw nothing in the cell; \
         {control_filled} lit pixels means something else is painting there and the \
         positive assertions above are not evidence for the new pass"
    );
}

/// **The player-head regression this gate exists to hold shut.** `SpecialIcons::new`
/// (`hud/item_icon.rs`) built its bind-group map from `chest_texture_stems()`
/// alone even though `crate::resources::load_block_entity_textures()` — the map it
/// read from — had already decoded every stem `block_entity_texture_stems()`
/// names, skulls included. `special_item_rig` still resolved a player head to a
/// real rig and sheet, `push_special_icon` still recorded the draw, and
/// `build_special_batches`' `!s.textures.contains_key(draw.texture)` guard then
/// silently dropped it every frame — an island one hop *inside* the special-icon
/// pass that the chest gate above cannot see, because a chest's own sheet was
/// always in the (accidentally chest-only) map.
///
/// Otherwise the same shape as [`a_chest_item_in_the_hotbar_reaches_pixels`]:
/// bounding box against the baked `SKULL_HUMANOID` AABB under the real
/// `display.gui` pose, silhouette area against a flat-quad prediction, shading
/// variation, and the same two negative controls. Reusing
/// [`special_pose_and_extent`] rather than a second hand-derivation is the point —
/// a copied constant is exactly how a gate ends up measuring the wrong cell.
///
/// # The node's own `"transformation"` is now part of the prediction
///
/// `assets/minecraft/items/player_head.json` puts a *second* transformation
/// (`translation: [0.5, 0, 0.5]` plus a 180°-about-X rotation) on the
/// `minecraft:special` model node itself (`SpecialModelWrapper.Unbaked.bake`,
/// `.cache/mc/26.2/client-src/net/minecraft/client/renderer/item/
/// SpecialModelWrapper.java`) — carried today by `ItemModelNode::Special`'s
/// `transformation` field and composed by [`special_pose_and_extent`] via
/// `lodestone_render::compose_special_node_transform`, the same call
/// `push_special_icon` makes. Before that field existed this gate's own doc
/// documented the mispositioning it measured rather than vanilla's placement
/// (the prediction sat mostly clipped outside the 16 px cell); now the
/// unclipped `gui_item_pose ∘ node_transform` prediction lands **entirely**
/// inside the cell — measured `(146.3, 303.2)..(157.7, 315.8)` inside a
/// `(143, 300, 16, 16)` cell — so this gate uses the same tight per-edge
/// check [`a_chest_item_in_the_hotbar_reaches_pixels`] does, no clip-to-cell
/// fallback. See `special_item_rig`'s neighbouring comment in
/// `block_entity.rs` for the full citation trail, including the flip this
/// file **used to** apply here, which a closer read of
/// `SkullSpecialRenderer.submit`/`PlayerHeadSpecialRenderer.submit` showed
/// was wrong (the world-only ground/wall flip never reaches the item path at
/// all) and which this gate's own bounding-box mismatch caught before it
/// shipped.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_player_head_item_in_the_hotbar_reaches_pixels() {
    const HEAD: &str = "minecraft:player_head";

    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "cell_rect assumes W x H divides to itself under the GUI scale"
    );

    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    // --- premises --------------------------------------------------------------

    let item: ResourceLocation = HEAD.parse().expect("valid item id");
    let item_atlas = load_item_atlas().expect("the item atlas must build from client.jar");
    let icon = item_atlas
        .icon(&item)
        .expect("player_head must resolve to an icon in the item atlas");
    let kind = icon
        .parts
        .iter()
        .find_map(|p| match p {
            IconPart::Special { kind, .. } => Some(kind.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "{HEAD} must resolve to an IconPart::Special; got {:?}. Without that this \
                 gate renders an item the special pass never sees and proves nothing.",
                icon.parts
            )
        });
    assert_eq!(
        kind, "minecraft:player_head",
        "vanilla splits player_head into its own `kind` (distinct from the mob \
         `minecraft:head` family) precisely because its renderer resolves a \
         profile texture -- see `special_item_rig`'s own doc"
    );
    let sheets_on_disk = load_block_entity_textures().len();
    assert!(
        sheets_on_disk > 0,
        "no block-entity sheets decoded from the jar; a player head could not draw \
         for reasons that have nothing to do with the wiring under test"
    );

    let (pose, lo, hi) = special_pose_and_extent(HEAD, SKULL_HUMANOID);
    let expected_area = silhouette_area(pose, lo, hi);
    let (px0, py0, px1, py1) = projected_bbox(pose, lo, hi);

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
    assert!(
        models.item(&item).is_none(),
        "{HEAD} unexpectedly has baked block-item geometry; if that is now true this \
         whole special-renderer pass may be redundant for it -- re-derive before trusting \
         either"
    );

    let mut target = HeadlessTarget::new(device, W, H, format);
    let render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let slots: Vec<Option<HotbarSlot>> = std::iter::once(Some(HotbarSlot {
        item: item.clone(),
        count: 1,
        damage: None,
        max_damage: None,
        enchanted: false,
        dyed_color: None,
        potion_color: None,
        banner_patterns: Vec::new(),
        base_color: None,
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

    let mut shoot = |hud: &mut HudRenderer| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        let raw_view = frame.create_view(target.raw_view_format());
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            &raw_view,
            Some(render.depth_view()),
            &hud_frame,
            Some(models),
            calculate_gui_scale(AUTO_GUI_SCALE, W, H),
            W,
            H,
        );
        target.read_texels(device, queue)
    };

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
    let sheets_in_pass = lit_hud.special_icon_sheets();

    let mut dark_hud = HudRenderer::new(device, format);
    dark_hud.attach_items(device, queue, format, item_atlas.clone());
    let control = shoot(&mut dark_hud);

    let filled = cell_rect(0);
    let empty = cell_rect(8);
    let subject_filled = lit_in(&subject, filled);
    let subject_empty = lit_in(&subject, empty);
    let control_filled = lit_in(&control, filled);
    let bbox = lit_bbox(&subject, filled);

    eprintln!("=== hotbar special-item (player head) pixel gate ===");
    eprintln!("item                  = {HEAD}  kind = {kind}");
    eprintln!("sheets on disk        = {sheets_on_disk}");
    eprintln!("sheets loaded by pass = {sheets_in_pass}");
    eprintln!("mesh AABB (blocks)    = {lo:?} .. {hi:?}");
    eprintln!("expected silhouette   = {expected_area:.1} px of 256");
    eprintln!("lit, slot 0 (head)    = {subject_filled}");
    eprintln!("lit bbox, slot 0      = {bbox:?}");
    eprintln!("lit, slot 8 (empty)   = {subject_empty}");
    eprintln!("lit, slot 0 (no item-model pass attached) = {control_filled}");
    eprintln!("predicted (gui_item_pose ∘ node_transform) = ({px0:.1}, {py0:.1})..({px1:.1}, {py1:.1})");

    assert!(
        sheets_in_pass > 0,
        "the special-icon pass reported {sheets_in_pass} sheets after a frame \
         containing a player head — it never built, so whatever painted in the \
         cell is not it. ({sheets_on_disk} sheets are decodable from the jar, so \
         this is a wiring failure and not a missing pack.)"
    );

    let Some((bx0, by0, bx1, by1)) = bbox else {
        panic!(
            "nothing drew in hotbar cell 0 for a player head. Expected ~{expected_area:.0} \
             lit px inside ({px0:.1}, {py0:.1})..({px1:.1}, {py1:.1}). This is the \
             `SpecialIcons::new` chest-only-stems bug: the rig and sheet resolve, and \
             `build_special_batches` drops the draw because no skull sheet ever reached \
             its texture map."
        );
    };
    // Now that the node's own `"transformation"` is carried and composed (see
    // this test's own doc), the unclipped prediction lands **entirely** inside
    // the 16 px cell — measured, not assumed: this fails loudly (a value
    // outside `filled`'s bounds) if a future change reverts to only the
    // display-context pose. So this gate now uses the same tight per-edge
    // check [`a_chest_item_in_the_hotbar_reaches_pixels`] does, no clip-to-cell
    // fallback: a whole-box offset means the pose composition differs from the
    // one the prediction used (right- vs left-multiplying the node transform,
    // e.g.), and a resize means the scale, the `-0.5` centring, or the node's
    // own scale does.
    assert!(
        px0 >= filled[0] as f32 && py0 >= filled[1] as f32,
        "the predicted top-left ({px0:.1}, {py0:.1}) falls outside the cell \
         {filled:?} — the node transform is not landing this item back in its \
         cell the way the fix requires"
    );
    assert!(
        px1 <= (filled[0] + filled[2]) as f32 && py1 <= (filled[1] + filled[3]) as f32,
        "the predicted bottom-right ({px1:.1}, {py1:.1}) falls outside the cell \
         {filled:?} — the node transform is not landing this item back in its \
         cell the way the fix requires"
    );
    let tol = 1.5f32;
    for (label, observed, expected) in [
        ("x0", bx0 as f32, px0),
        ("y0", by0 as f32, py0),
        ("x1", bx1 as f32, px1),
        ("y1", by1 as f32, py1),
    ] {
        assert!(
            (observed - expected).abs() <= tol + 1.0,
            "the player head's lit {label} is {observed:.1} but `gui_item_pose` composed \
             with the node's own transformation puts it at {expected:.1}. Lit box \
             ({bx0}, {by0})..({bx1}, {by1}); predicted ({px0:.1}, {py0:.1})..({px1:.1}, \
             {py1:.1})."
        );
    }

    // --- how much: geometry, not a flat quad ---------------------------------
    //
    // Same shape as the chest gate: predict both the correct hypothesis
    // (silhouette area) and the plausible wrong one (a flat quad filling the
    // whole bbox), and require the measurement to land on the former.
    let low = (expected_area * 0.80) as usize;
    let high = (expected_area * 1.20) as usize;
    let bbox_area = (px1 - px0) * (py1 - py0);
    assert!(
        (low..=high).contains(&subject_filled),
        "a player head icon must cover ~{expected_area:.0} px of the 256 px cell — the \
         projected silhouette of the baked `SKULL_HUMANOID` AABB under the composed pose \
         — got {subject_filled} in box ({bx0}, {by0})..({bx1}, {by1}). Far below means \
         faces are missing or the draw never ran; near {bbox_area:.0} means something is \
         filling the bounding box, i.e. a flat quad rather than geometry."
    );
    assert!(
        (subject_filled as f32) < bbox_area * 0.95,
        "{subject_filled} lit px fills essentially the whole {bbox_area:.0} px bounding \
         box. A rotated box projects to a hexagon and leaves its bbox corners empty; only \
         a screen-aligned quad fills it. This reads as the flat-sprite fallback, not the \
         block-entity geometry."
    );

    assert_eq!(
        subject_empty, 0,
        "an empty hotbar cell must stay black; {subject_empty} lit pixels there means the \
         draw is not localised to its slot"
    );
    assert_eq!(
        control_filled, 0,
        "without attach_item_models the same frame must draw nothing in the cell"
    );
}

/// Pixel gate: a `minecraft:banner` item in the hotbar draws its dye colour.
///
/// `special_item_rig`'s own doc table names `banner` as one of six `kind`s
/// that resolve to `None` ("needs the ordered translucent pattern-mask pass,
/// not one rig"), which is why a banner drew zero pixels in a hotbar slot,
/// an inventory slot and the first-person hand — the owner's own report,
/// verified here rather than re-derived from it. `lodestone_render::
/// banner_item_rig` landed the two-mesh (pole/bar, flag) rig; this gate
/// predates the later landing of the *real* translucent pattern-layer pass
/// (`minecraft:banner_patterns` decoded per stack, drawn through
/// `push_special_icon`'s `SpecialIconDraw::BannerLayer` entries — see
/// `hud/item_icon.rs`), so both banners here carry no pattern layers and draw
/// only the base translucent mask, which is what this test's colour
/// assertions check. See `two_dyed_banners_with_loom_patterns_show_both_colours`
/// below for the pattern half specifically.
///
/// # Two colours, not one, and chosen to disagree hard on every channel
///
/// A single banner proves geometry reaches pixels; it cannot prove the tint
/// is the *item's own* colour rather than a hardcoded one, or worse, the
/// mesh's plain `banner_base` texture leaking through untinted. Vanilla's
/// `textureDiffuseColor` puts `RED` at `(176, 46, 38)` and `LIGHT_BLUE` at
/// `(58, 179, 218)` — R and B swap which one dominates — so "the red banner's
/// lit pixels average redder than the light-blue banner's, and the light-blue
/// banner's average bluer than the red banner's" is a claim a hardcoded tint,
/// a swapped channel, or an untinted grey mesh could not all satisfy at once.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn two_differently_dyed_banners_in_the_hotbar_draw_different_colours() {
    const RED: &str = "minecraft:red_banner";
    const LIGHT_BLUE: &str = "minecraft:light_blue_banner";

    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "cell_rect assumes W x H divides to itself under the GUI scale"
    );

    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    // --- premises ---------------------------------------------------------

    let item_atlas = load_item_atlas().expect("the item atlas must build from client.jar");
    for id in [RED, LIGHT_BLUE] {
        let item: ResourceLocation = id.parse().expect("valid item id");
        let icon = item_atlas
            .icon(&item)
            .unwrap_or_else(|| panic!("{id} must resolve to an icon in the item atlas"));
        let kind = icon.parts.iter().find_map(|p| match p {
            IconPart::Special { kind, .. } => Some(kind.clone()),
            _ => None,
        });
        assert_eq!(
            kind.as_deref(),
            Some("minecraft:banner"),
            "{id} must resolve to an IconPart::Special carrying `minecraft:banner`; \
             got {kind:?}. Without that this gate renders an item the special pass \
             never sees and proves nothing."
        );
    }
    let sheets_on_disk = load_block_entity_textures().len();
    assert!(
        sheets_on_disk > 0,
        "no block-entity sheets decoded from the jar; a banner could not draw for \
         reasons that have nothing to do with the wiring under test"
    );

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

    let mut target = HeadlessTarget::new(device, W, H, format);
    let render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let slot = |id: &str| {
        Some(HotbarSlot {
            item: id.parse().expect("valid item id"),
            count: 1,
            damage: None,
            max_damage: None,
            enchanted: false,
            dyed_color: None,
            potion_color: None,
            banner_patterns: Vec::new(),
            base_color: None,
        })
    };
    let slots: Vec<Option<HotbarSlot>> = vec![
        slot(RED),
        slot(LIGHT_BLUE),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ];

    let stats = DebugStats::default();
    let hud_frame = HudFrame {
        show_debug: false,
        crosshair: false,
        hotbar: None,
        hotbar_items: Some(&slots),
        ..HudFrame::new(&stats)
    };

    let mut shoot = |hud: &mut HudRenderer| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        let raw_view = frame.create_view(target.raw_view_format());
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            &raw_view,
            Some(render.depth_view()),
            &hud_frame,
            Some(models),
            calculate_gui_scale(AUTO_GUI_SCALE, W, H),
            W,
            H,
        );
        target.read_texels(device, queue)
    };

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
    let sheets_in_pass = lit_hud.special_icon_sheets();

    let mut dark_hud = HudRenderer::new(device, format);
    dark_hud.attach_items(device, queue, format, item_atlas.clone());
    let control = shoot(&mut dark_hud);

    let red_cell = cell_rect(0);
    let blue_cell = cell_rect(1);
    let empty_cell = cell_rect(8);
    let red_lit = lit_in(&subject, red_cell);
    let blue_lit = lit_in(&subject, blue_cell);
    let empty_lit = lit_in(&subject, empty_cell);
    let control_red_lit = lit_in(&control, red_cell);
    let red_rgb = mean_rgb_in(&subject, red_cell);
    let blue_rgb = mean_rgb_in(&subject, blue_cell);

    eprintln!("=== hotbar special-item (banner) pixel gate ===");
    eprintln!("sheets on disk        = {sheets_on_disk}");
    eprintln!("sheets loaded by pass = {sheets_in_pass}");
    eprintln!("lit, slot 0 ({RED})  = {red_lit}, mean rgb = {red_rgb:?}");
    eprintln!("lit, slot 1 ({LIGHT_BLUE}) = {blue_lit}, mean rgb = {blue_rgb:?}");
    eprintln!("lit, slot 8 (empty)   = {empty_lit}");
    eprintln!("lit, slot 0 (no item-model pass attached) = {control_red_lit}");

    assert!(
        sheets_in_pass > 0,
        "the special-icon pass reported {sheets_in_pass} sheets after a frame \
         containing a banner — it never built, so whatever painted in the cell is \
         not it. ({sheets_on_disk} sheets are decodable from the jar, so this is a \
         wiring failure and not a missing pack.)"
    );

    // --- that: pixels reach both cells, and only while the pass is attached ---

    assert!(
        red_lit > 0,
        "nothing drew in hotbar cell 0 for {RED}. This is the owner's own report — \
         a banner in an item slot drew zero pixels — and `banner_item_rig` \
         resolving `None` (a datapack-shaped item path, or `special_item_rig`'s \
         old `_ => None` still winning) is exactly how it would still fail."
    );
    assert!(blue_lit > 0, "nothing drew in hotbar cell 1 for {LIGHT_BLUE}");
    assert_eq!(
        empty_lit, 0,
        "an empty hotbar cell must stay black; {empty_lit} lit pixels there means \
         the draw is not localised to its own slot"
    );
    assert_eq!(
        control_red_lit, 0,
        "without attach_item_models the same frame must draw nothing in the cell"
    );

    // --- which colour: the two banners must disagree on the *right* channels --

    let [rr, rg, rb] = red_rgb.expect("slot 0 has lit pixels, checked above");
    let [br, bg, bb] = blue_rgb.expect("slot 1 has lit pixels, checked above");
    eprintln!("red banner   r-b = {:.1}", rr - rb);
    eprintln!("light-blue   b-r = {:.1}", bb - br);
    let _ = (rg, bg);

    // `RED`'s textureDiffuseColor is (176, 46, 38): red channel must dominate
    // blue by a wide, unmissable margin. A hardcoded white tint (untinted mesh
    // leaking through) would read r≈b; a channel-order bug (tinting with `bgr`
    // instead of `rgb`) would flip this sign entirely.
    //
    // **The mechanism changed underneath this margin and then changed back.**
    // The base colour used to come from a full-flag tint-multiply
    // (`banner_item_rig`'s first landing); it now comes from a translucent
    // *base layer* drawn over an untinted flag — but that base mask is itself
    // fully covered (`Sheets.BANNER_PATTERN_BASE` has no transparent texels),
    // so once its own placement bug was fixed (an earlier version of this
    // layer used the raw item placement instead of composing through the
    // flag part's own local transform, landing the mask entirely outside the
    // visible cell) the measured margin came back to the same shape as
    // before: red banner r-b = 54.5, light-blue b-r = 60.2 on a real run,
    // both comfortably above the original 40.0.
    assert!(
        rr - rb > 40.0,
        "the {RED} banner's lit pixels average r={rr:.1}, b={rb:.1} — red must \
         dominate blue by a wide margin for this to be the item's own dye colour \
         and not an untinted grey mesh (r≈b) or a channel-swapped tint (b>r)"
    );
    // `LIGHT_BLUE`'s textureDiffuseColor is (58, 179, 218): blue channel must
    // dominate red by the same margin, in the opposite direction.
    assert!(
        bb - br > 40.0,
        "the {LIGHT_BLUE} banner's lit pixels average b={bb:.1}, r={br:.1} — blue \
         must dominate red by a wide margin for the same reason"
    );
    // The pairwise-distinct check CLAUDE.md's evidence section asks for: the two
    // banners' own red channels must differ, or a single shared tint could
    // satisfy both assertions above by coincidence.
    assert!(
        (rr - br).abs() > 40.0,
        "the two banners' red channels ({rr:.1} vs {br:.1}) are too close — they \
         may be drawing with the same tint rather than each item's own colour"
    );
}

/// Pixel index (relative to `rect`'s origin) and `[r, g, b]` for every pixel
/// inside `rect` that is not the black backdrop — the per-pixel sibling of
/// [`mean_rgb_in`], used below to compare two renders of the same cell
/// directly rather than against a guessed absolute target colour.
///
/// **Absolute target colours (`DyeColor::White`/`Black`'s raw
/// `textureDiffuseColor` bytes) were tried first and were the wrong
/// premise**: the translucent layer's own contribution to a 16×16 cell's mean
/// is real but small (this file's sibling test's own margin dropped from
/// 40.0 to 1.5 for the identical reason — see its doc), so no fixed target
/// byte value was ever going to land inside a useful tolerance. Comparing one
/// render against another sidesteps needing to predict the absolute byte at
/// all.
fn cell_rgb(pixels: &[u8], rect: [u32; 4]) -> std::collections::HashMap<(u32, u32), [u8; 3]> {
    let [rx, ry, rw, rh] = rect;
    let mut out = std::collections::HashMap::new();
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            if brightness(pixels, x, y) > 20 {
                let i = ((y * W + x) * 4) as usize;
                out.insert((x - rx, y - ry), [pixels[i], pixels[i + 1], pixels[i + 2]]);
            }
        }
    }
    out
}

/// Pixel gate: a `minecraft:banner` item in the hotbar draws its **loom
/// patterns**, not just its base colour — the half named as still missing by
/// the doc comment on both `lodestone_render::banner_item_rig` and this
/// file's own sibling test above, closed by decoding
/// `minecraft:banner_patterns` for an item stack (`crates/protocol/v770`) and
/// a real translucent pattern-layer pass in `hud/item_icon.rs`'s
/// `SpecialIconDraw::BannerLayer` (mirroring the world block-entity pass's
/// `banner_layer_pipeline`).
///
/// # The discriminating claim: adding a pattern must change the cell
///
/// Two renders of the same slot holding `minecraft:red_banner`, one with
/// **no** pattern layers and one with a `minecraft:lime`-coloured
/// `minecraft:creeper` pattern added. A flat tint — the mechanism this
/// file's sibling test measures, and any regression back toward it — cannot
/// respond to `banner_patterns` at all, so the two cells would be
/// pixel-identical. A real masked layer draw changes exactly the
/// creeper-shaped region: this gate counts pixels that moved by a real
/// margin between the two renders and requires the *moved* pixels' mean to
/// shift toward green (lime, the pattern) while the *unmoved* pixels stay
/// red-dominated (the base, still showing through).
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_dyed_banner_with_a_loom_pattern_shows_both_colours_in_one_cell() {
    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "cell_rect assumes W x H divides to itself under the GUI scale"
    );

    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let item_atlas = load_item_atlas().expect("the item atlas must build from client.jar");
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

    let mut target = HeadlessTarget::new(device, W, H, format);
    let render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let slot = |patterns: Vec<lodestone_model::BannerPatternLayer>| {
        Some(HotbarSlot {
            item: "minecraft:red_banner".parse().expect("valid item id"),
            count: 1,
            damage: None,
            max_damage: None,
            enchanted: false,
            dyed_color: None,
            potion_color: None,
            banner_patterns: patterns,
            base_color: None,
        })
    };
    let plain_slots: Vec<Option<HotbarSlot>> = std::iter::once(slot(Vec::new()))
        .chain(std::iter::repeat_with(|| None).take(8))
        .collect();
    let patterned_slots: Vec<Option<HotbarSlot>> = std::iter::once(slot(vec![
        lodestone_model::BannerPatternLayer {
            pattern_asset_id: "creeper".to_string(),
            color: "lime".to_string(),
        },
    ]))
    .chain(std::iter::repeat_with(|| None).take(8))
    .collect();

    let mut shoot = |slots: &[Option<HotbarSlot>]| -> Vec<u8> {
        let stats = DebugStats::default();
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            hotbar: None,
            hotbar_items: Some(slots),
            ..HudFrame::new(&stats)
        };
        let mut hud = HudRenderer::new(device, format);
        hud.attach_items(device, queue, format, item_atlas.clone());
        hud.attach_item_models(
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
        let frame = target.acquire().expect("headless acquire");
        let raw_view = frame.create_view(target.raw_view_format());
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            &raw_view,
            Some(render.depth_view()),
            &hud_frame,
            Some(models),
            calculate_gui_scale(AUTO_GUI_SCALE, W, H),
            W,
            H,
        );
        target.read_texels(device, queue)
    };

    let plain_pixels = shoot(&plain_slots);
    let patterned_pixels = shoot(&patterned_slots);
    let cell = cell_rect(0);

    let plain = cell_rgb(&plain_pixels, cell);
    let patterned = cell_rgb(&patterned_pixels, cell);

    let mut moved_sum = [0i64; 3];
    let mut moved_n = 0usize;
    let mut unmoved_sum = [0i64; 3];
    let mut unmoved_n = 0usize;
    for (pos, prgb) in &patterned {
        match plain.get(pos) {
            Some(qrgb) => {
                let d = (i32::from(prgb[0]) - i32::from(qrgb[0])).abs()
                    + (i32::from(prgb[1]) - i32::from(qrgb[1])).abs()
                    + (i32::from(prgb[2]) - i32::from(qrgb[2])).abs();
                if d > 24 {
                    for c in 0..3 {
                        moved_sum[c] += i64::from(prgb[c]);
                    }
                    moved_n += 1;
                } else {
                    for c in 0..3 {
                        unmoved_sum[c] += i64::from(prgb[c]);
                    }
                    unmoved_n += 1;
                }
            }
            None => {
                for c in 0..3 {
                    moved_sum[c] += i64::from(prgb[c]);
                }
                moved_n += 1;
            }
        }
    }

    eprintln!("=== hotbar banner pattern-layer pixel gate ===");
    eprintln!("plain red_banner:  lit px={}", plain.len());
    eprintln!("+creeper (lime):   lit px={}, moved px={moved_n}, unmoved px={unmoved_n}", patterned.len());

    assert!(
        moved_n > 3,
        "adding a lime `creeper` pattern layer moved only {moved_n} pixels by more \
         than a rounding wobble relative to the unpatterned render, in a cell with \
         {} lit pixels total — the pattern mask is not reaching pixels",
        patterned.len()
    );
    assert!(
        unmoved_n > 3,
        "adding the pattern moved every lit pixel ({moved_n} of {}) — a masked \
         layer draw should leave the uncovered rest of the flag (and the untinted \
         pole/bar) unchanged, so this looks like a full-cell tint rather than a \
         local mask",
        patterned.len()
    );
    let moved_mean = moved_sum.map(|s| s as f64 / moved_n as f64);
    eprintln!("moved-pixel mean rgb = {moved_mean:?}");
    assert!(
        moved_mean[1] > moved_mean[0],
        "the pixels the creeper pattern moved average rgb={moved_mean:?} — green \
         must exceed red for a lime pattern over a red base, or this is not the \
         pattern's own colour reaching the mask"
    );
}

/// Pixel gate: shields — the identical island `banner_item_rig`'s doc used to
/// name (`banner_item_rig` reuses `BANNER_BODY`/`BANNER_FLAG`, which had no
/// shield equivalent, and `lodestone_render::banner_pattern::
/// shield_pattern_layers` had no consumer at all) — now draw in the hotbar
/// through a real `"shield"` mesh (`lodestone_assets::block_entity_models::
/// shield_model`, `ShieldModel.createLayer` ported) and
/// `lodestone_render::shield_item_rig`/`shield_has_patterns`.
///
/// # Two `minecraft:base_color`s, not one
///
/// A shield's item id never encodes colour the way `red_banner`/
/// `light_blue_banner` do — every shield is `minecraft:shield`, and the tint
/// is entirely `minecraft:base_color`. So this is the shield analogue of
/// [`two_differently_dyed_banners_in_the_hotbar_draw_different_colours`]:
/// the same two colours (`RED`/`LIGHT_BLUE`), chosen for the same reason —
/// they disagree hard on every channel, so a hardcoded tint, a swapped
/// channel, or an untinted grey mesh could not satisfy the assertions below
/// by accident.
///
/// # The negative control: no `base_color`, no patterns
///
/// `ShieldSpecialRenderer.submit`'s own `hasPatterns` gate means a shield
/// with neither carries **no** translucent layer at all — only the flat
/// `shield_base_nopattern` sheet, untinted. That is the common case (straight
/// off a crafting table) and this gate checks it is not silently treated as
/// "coloured": the plain shield's lit pixels must average much closer to
/// neutral (small `max - min` channel spread) than either dyed shield's,
/// which is the mid-magnitude anchor between "a real translucent tint layer
/// is present" and "only the grey primer sheet is" — a gate that only
/// checked the two dyed shields against each other could pass even if
/// `shield_has_patterns` always returned `true` and every shield paid for a
/// layer draw it does not need.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn shields_with_different_base_colours_draw_different_colours_and_a_plain_one_draws_neither() {
    const SHIELD: &str = "minecraft:shield";

    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "cell_rect assumes W x H divides to itself under the GUI scale"
    );

    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let item_atlas = load_item_atlas().expect("the item atlas must build from client.jar");
    let item: ResourceLocation = SHIELD.parse().expect("valid item id");
    let icon = item_atlas
        .icon(&item)
        .unwrap_or_else(|| panic!("{SHIELD} must resolve to an icon in the item atlas"));
    let kind = icon.parts.iter().find_map(|p| match p {
        IconPart::Special { kind, .. } => Some(kind.clone()),
        _ => None,
    });
    assert_eq!(
        kind.as_deref(),
        Some("minecraft:shield"),
        "{SHIELD} must resolve to an IconPart::Special carrying `minecraft:shield`; \
         got {kind:?}. Without that this gate renders an item the special pass \
         never sees and proves nothing."
    );

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

    let mut target = HeadlessTarget::new(device, W, H, format);
    let render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let slot = |base_color: Option<&str>| {
        Some(HotbarSlot {
            item: SHIELD.parse().expect("valid item id"),
            count: 1,
            damage: None,
            max_damage: None,
            enchanted: false,
            dyed_color: None,
            potion_color: None,
            banner_patterns: Vec::new(),
            base_color: base_color.map(str::to_string),
        })
    };
    let slots: Vec<Option<HotbarSlot>> = vec![
        slot(Some("red")),
        slot(Some("light_blue")),
        slot(None),
        None,
        None,
        None,
        None,
        None,
        None,
    ];

    let stats = DebugStats::default();
    let hud_frame = HudFrame {
        show_debug: false,
        crosshair: false,
        hotbar: None,
        hotbar_items: Some(&slots),
        ..HudFrame::new(&stats)
    };

    let mut shoot = |hud: &mut HudRenderer| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        let raw_view = frame.create_view(target.raw_view_format());
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            &raw_view,
            Some(render.depth_view()),
            &hud_frame,
            Some(models),
            calculate_gui_scale(AUTO_GUI_SCALE, W, H),
            W,
            H,
        );
        target.read_texels(device, queue)
    };

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
    let sheets_in_pass = lit_hud.special_icon_sheets();

    let red_cell = cell_rect(0);
    let blue_cell = cell_rect(1);
    let plain_cell = cell_rect(2);
    let empty_cell = cell_rect(8);
    let red_lit = lit_in(&subject, red_cell);
    let blue_lit = lit_in(&subject, blue_cell);
    let plain_lit = lit_in(&subject, plain_cell);
    let empty_lit = lit_in(&subject, empty_cell);
    let red_rgb = mean_rgb_in(&subject, red_cell);
    let blue_rgb = mean_rgb_in(&subject, blue_cell);
    let plain_rgb = mean_rgb_in(&subject, plain_cell);

    eprintln!("=== hotbar special-item (shield) pixel gate ===");
    eprintln!("sheets loaded by pass = {sheets_in_pass}");
    eprintln!("lit, slot 0 (red shield)        = {red_lit}, mean rgb = {red_rgb:?}");
    eprintln!("lit, slot 1 (light_blue shield) = {blue_lit}, mean rgb = {blue_rgb:?}");
    eprintln!("lit, slot 2 (plain shield)      = {plain_lit}, mean rgb = {plain_rgb:?}");
    eprintln!("lit, slot 8 (empty)             = {empty_lit}");

    assert!(
        sheets_in_pass > 0,
        "the special-icon pass reported {sheets_in_pass} sheets after a frame \
         containing a shield — it never built, so whatever painted in the cell is \
         not it"
    );
    assert!(red_lit > 0, "nothing drew in hotbar cell 0 for a red-based shield");
    assert!(blue_lit > 0, "nothing drew in hotbar cell 1 for a light_blue-based shield");
    assert!(plain_lit > 0, "nothing drew in hotbar cell 2 for a plain shield — the opaque \
         no-pattern sheet alone should still reach pixels even with no translucent layer");
    assert_eq!(
        empty_lit, 0,
        "an empty hotbar cell must stay black; {empty_lit} lit pixels there means \
         the draw is not localised to its own slot"
    );

    let [rr, rg, rb] = red_rgb.expect("slot 0 has lit pixels, checked above");
    let [br, bg, bb] = blue_rgb.expect("slot 1 has lit pixels, checked above");
    let [pr, pg, pb] = plain_rgb.expect("slot 2 has lit pixels, checked above");
    let _ = (rg, bg, pg);
    eprintln!("red shield         r-b = {:.1}", rr - rb);
    eprintln!("light-blue shield  b-r = {:.1}", bb - br);
    eprintln!("plain shield       max-min channel spread = {:.1}", pr.max(pg).max(pb) - pr.min(pg).min(pb));

    // `RED`'s textureDiffuseColor is (176, 46, 38): red channel must dominate
    // blue by a wide margin, exactly as the banner test's identical assertion
    // reasons — see that test's own doc for why the margin is this large
    // (the base mask is fully opaque, so the measured contrast comes back to
    // the full-tint shape once the mask's placement is correct).
    assert!(
        rr - rb > 20.0,
        "the red-based shield's lit pixels average r={rr:.1}, b={rb:.1} — red must \
         dominate blue by a real margin for this to be the stack's own \
         `minecraft:base_color` and not an untinted grey mesh (r≈b) or a \
         channel-swapped tint (b>r)"
    );
    assert!(
        bb - br > 20.0,
        "the light_blue-based shield's lit pixels average b={bb:.1}, r={br:.1} — \
         blue must dominate red by a real margin for the same reason"
    );
    assert!(
        (rr - br).abs() > 20.0,
        "the two shields' red channels ({rr:.1} vs {br:.1}) are too close — they \
         may be drawing with the same tint rather than each stack's own \
         `minecraft:base_color`"
    );

    // The mid-magnitude anchor: a plain shield (no base_color, no patterns)
    // must sit far closer to neutral than either dyed one — the discriminator
    // between "the translucent base-mask layer is genuinely gated on
    // `shield_has_patterns`" and "it always draws, tinted white, and happens
    // to look plausible". A shield with no colour information at all has
    // nothing to be neutral *about* except by construction.
    let red_spread = rr.max(rg).max(rb) - rr.min(rg).min(rb);
    let blue_spread = br.max(bg).max(bb) - br.min(bg).min(bb);
    let plain_spread = pr.max(pg).max(pb) - pr.min(pg).min(pb);
    eprintln!(
        "channel spread: red={red_spread:.1} blue={blue_spread:.1} plain={plain_spread:.1}"
    );
    assert!(
        plain_spread < red_spread.min(blue_spread) * 0.5,
        "a plain shield's own channel spread ({plain_spread:.1}) should be well \
         below either dyed shield's ({red_spread:.1}, {blue_spread:.1}) — a plain \
         shield draws no translucent tint layer at all, so it should read far \
         closer to a neutral grey than a shield carrying a real \
         `minecraft:base_color`"
    );
}

/// Pixel gate: a `minecraft:shield` item in the hotbar draws its **loom
/// patterns**, not just its base colour — the shield analogue of
/// [`a_dyed_banner_with_a_loom_pattern_shows_both_colours_in_one_cell`].
/// Unlike a banner, a shield's pattern layers re-tint the *whole* mesh
/// (`plate` and `handle` together — see `lodestone_render::block_entity::
/// SHIELD`'s doc for why there is no separate flag-shaped sub-part), so this
/// is also the check that `push_special_icon`'s shield branch resolves the
/// right mesh/part range rather than the banner's `"flag"` one.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_based_shield_with_a_loom_pattern_shows_both_colours_in_one_cell() {
    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "cell_rect assumes W x H divides to itself under the GUI scale"
    );

    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let item_atlas = load_item_atlas().expect("the item atlas must build from client.jar");
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

    let mut target = HeadlessTarget::new(device, W, H, format);
    let render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let slot = |patterns: Vec<lodestone_model::BannerPatternLayer>| {
        Some(HotbarSlot {
            item: "minecraft:shield".parse().expect("valid item id"),
            count: 1,
            damage: None,
            max_damage: None,
            enchanted: false,
            dyed_color: None,
            potion_color: None,
            banner_patterns: patterns,
            base_color: Some("red".to_string()),
        })
    };
    let plain_slots: Vec<Option<HotbarSlot>> = std::iter::once(slot(Vec::new()))
        .chain(std::iter::repeat_with(|| None).take(8))
        .collect();
    let patterned_slots: Vec<Option<HotbarSlot>> = std::iter::once(slot(vec![
        lodestone_model::BannerPatternLayer {
            pattern_asset_id: "creeper".to_string(),
            color: "lime".to_string(),
        },
    ]))
    .chain(std::iter::repeat_with(|| None).take(8))
    .collect();

    let mut shoot = |slots: &[Option<HotbarSlot>]| -> Vec<u8> {
        let stats = DebugStats::default();
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            hotbar: None,
            hotbar_items: Some(slots),
            ..HudFrame::new(&stats)
        };
        let mut hud = HudRenderer::new(device, format);
        hud.attach_items(device, queue, format, item_atlas.clone());
        hud.attach_item_models(
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
        let frame = target.acquire().expect("headless acquire");
        let raw_view = frame.create_view(target.raw_view_format());
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            &raw_view,
            Some(render.depth_view()),
            &hud_frame,
            Some(models),
            calculate_gui_scale(AUTO_GUI_SCALE, W, H),
            W,
            H,
        );
        target.read_texels(device, queue)
    };

    let plain_pixels = shoot(&plain_slots);
    let patterned_pixels = shoot(&patterned_slots);
    let cell = cell_rect(0);

    let plain = cell_rgb(&plain_pixels, cell);
    let patterned = cell_rgb(&patterned_pixels, cell);

    let mut moved_sum = [0i64; 3];
    let mut moved_n = 0usize;
    let mut unmoved_sum = [0i64; 3];
    let mut unmoved_n = 0usize;
    for (pos, prgb) in &patterned {
        match plain.get(pos) {
            Some(qrgb) => {
                let d = (i32::from(prgb[0]) - i32::from(qrgb[0])).abs()
                    + (i32::from(prgb[1]) - i32::from(qrgb[1])).abs()
                    + (i32::from(prgb[2]) - i32::from(qrgb[2])).abs();
                if d > 24 {
                    for c in 0..3 {
                        moved_sum[c] += i64::from(prgb[c]);
                    }
                    moved_n += 1;
                } else {
                    for c in 0..3 {
                        unmoved_sum[c] += i64::from(prgb[c]);
                    }
                    unmoved_n += 1;
                }
            }
            None => {
                for c in 0..3 {
                    moved_sum[c] += i64::from(prgb[c]);
                }
                moved_n += 1;
            }
        }
    }

    eprintln!("=== hotbar shield pattern-layer pixel gate ===");
    eprintln!("plain red-based shield: lit px={}", plain.len());
    eprintln!("+creeper (lime):        lit px={}, moved px={moved_n}, unmoved px={unmoved_n}", patterned.len());

    assert!(
        moved_n > 3,
        "adding a lime `creeper` pattern layer moved only {moved_n} pixels by more \
         than a rounding wobble relative to the unpatterned render, in a cell with \
         {} lit pixels total — the pattern mask is not reaching pixels",
        patterned.len()
    );
    assert!(
        unmoved_n > 3,
        "adding the pattern moved every lit pixel ({moved_n} of {}) — a masked \
         layer draw should leave the uncovered rest of the shield unchanged, so \
         this looks like a full-cell tint rather than a local mask",
        patterned.len()
    );
    let moved_mean = moved_sum.map(|s| s as f64 / moved_n as f64);
    let unmoved_mean = unmoved_sum.map(|s| s as f64 / unmoved_n as f64);
    eprintln!("moved-pixel mean rgb   = {moved_mean:?}");
    eprintln!("unmoved-pixel mean rgb = {unmoved_mean:?}");
    assert!(
        moved_mean[1] > moved_mean[0],
        "the pixels the creeper pattern moved average rgb={moved_mean:?} — green \
         must exceed red for a lime pattern over a red base, or this is not the \
         pattern's own colour reaching the mask"
    );
    // The mid-alpha anchor this file's own creeper/red-banner sibling gate does
    // not need (a banner's base mask and its pattern masks paint over disjoint
    // mesh regions relative to the untinted pole/bar) but a shield's does: every
    // layer here re-tints the *same* `plate`+`handle` mesh, base then pattern,
    // both through the identical translucent pipeline — so the *unmoved*
    // pixels (base mask only) must themselves still read red-dominated, or the
    // "moved" split above could be hiding a case where the base layer itself
    // never drew and only the pattern layer's own translucent draw is visible.
    assert!(
        unmoved_mean[0] > unmoved_mean[2],
        "the shield's own unmoved (base-only) pixels average rgb={unmoved_mean:?} \
         — red must exceed blue there too, or the base-colour layer this test's \
         sibling gate already proved is not actually surviving underneath the \
         pattern mask"
    );
}
