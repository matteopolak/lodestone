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
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
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
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
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
