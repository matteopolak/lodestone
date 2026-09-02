//! Pixel gate: a brushable block's revealed item must **draw**, in its own
//! projected screen rect, through the real [`RenderState::render`] path —
//! the same call `app.rs`'s frame loop makes.
//!
//! Closes the verification gap `docs/block-entity-renderers.md`'s
//! "A note on verification scope" names: brushable block landed with real
//! CPU-side unit tests and a real wiring chain, but no `#[ignore]`d GPU
//! pixel gate. `CLAUDE.md`'s dominant defect class is exactly what a wiring
//! review cannot see — a merge call site commented out, a counter stuck at
//! zero — so this is the same shape every sibling item-pipeline gate
//! (`vault_item_cluster_pixels.rs`) already uses.
//!
//! # Not a hole-in-the-world case
//!
//! `suspicious_sand`/`suspicious_gravel` are ordinary real block models —
//! the terrain mesher already draws the block; only the single revealed
//! item hovering off the last-brushed face comes from
//! [`RenderState::set_brushable_source`]/`gpu/world_items.rs::merge_brushable_items`.
//!
//! # The rect comes from the real mesh function, not a baked literal
//!
//! [`lodestone_render::entity::brushable_item_mesh`] returns a
//! [`lodestone_render::ModelMesh`] whose vertex positions are **already
//! world-space** (the pose is baked in by the mesh function itself), so the
//! expected rect here projects that mesh's own vertices through the same
//! [`Camera::view_projection`] `gpu/world_items.rs::merge_brushable_items`
//! feeds into the same function — never a remembered literal.
//!
//! # Two known states, not one frame
//!
//! `dust_progress` grows the item's outward offset along the hit direction
//! (`brushable_item_offset`'s `completion_offset` term) — a real per-state
//! change, not an animation (a revealed item has no clock). `dust_progress`
//! `1` vs `3` (`Direction::Up`) must therefore project to genuinely
//! different rects, told apart the same way
//! `skull_block_entity_pixels.rs`'s `wall_and_floor_skulls_project_to_different_rects`
//! tells a wall skull from a floor one: by asserting the *rects differ*,
//! not merely that both draw.
//!
//! ```text
//! cargo test -p lodestone-shell --test brushable_block_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone::resources::BlockResources;
use lodestone_assets::{DisplaySlot, ResourceLocation};
use lodestone_render::{
    BrushableItemSpawn, Camera, GpuContext, HeadlessTarget, RenderTarget,
    entity::brushable_item_mesh, item_render::ItemStateContext,
};

const W: u32 = 320;
const H: u32 = 240;

/// The brushable block's position, three blocks ahead of the camera on `+Z`.
const BLOCK: [i32; 3] = [0, 0, 3];

/// A solid, non-flat item so the revealed item's silhouette is easy to
/// localise — same reasoning `vault_item_cluster_pixels.rs` gives for
/// choosing a diamond.
const ITEM: &str = "minecraft:diamond";

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

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn area(self) -> usize {
        ((self.x1 - self.x0 + 1) as usize) * ((self.y1 - self.y0 + 1) as usize)
    }

    fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    fn padded(self, pad: u32) -> Rect {
        Rect {
            x0: self.x0.saturating_sub(pad),
            y0: self.y0.saturating_sub(pad),
            x1: (self.x1 + pad).min(W - 1),
            y1: (self.y1 + pad).min(H - 1),
        }
    }

    fn intersects(self, other: Rect) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }
}

fn bbox_of(pixels: &[u8], predicate: impl Fn(&[u8]) -> bool) -> Option<(Rect, usize)> {
    let mut rect: Option<Rect> = None;
    let mut count = 0usize;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        if !predicate(px) {
            continue;
        }
        count += 1;
        let x = (i as u32) % W;
        let y = (i as u32) / W;
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
    rect.map(|r| (r, count))
}

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
    rect.map(|r| (r, count))
}

fn non_sky_in(pixels: &[u8], rect: Rect, sky: [u8; 3]) -> usize {
    let mut n = 0;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        if rect.contains(x, y) && is_non_sky(px, sky) {
            n += 1;
        }
    }
    n
}

fn project(view_proj: glam::Mat4, world: glam::Vec3) -> (f32, f32) {
    let clip = view_proj * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    (
        (ndc_x * 0.5 + 0.5) * W as f32,
        (1.0 - (ndc_y * 0.5 + 0.5)) * H as f32,
    )
}

/// Eye near the block's own top, three blocks back on `-Z`, looking straight
/// down `+Z` — the same "yaw 0 faces +Z" convention `vault_item_cluster_pixels.rs`
/// uses.
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.9, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
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

fn load_atlas() -> std::sync::Arc<lodestone_render::BlockAtlas> {
    let resources = BlockResources::load(true);
    resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS \
             to a pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    })
}

fn test_spawn(dust_progress: u8) -> BrushableItemSpawn {
    BrushableItemSpawn {
        pos: BLOCK,
        hit_direction: lodestone_assets::Direction::Up,
        dust_progress,
        item: ITEM.parse().expect("valid item id"),
        light: lodestone_render::ENTITY_FULLBRIGHT,
    }
}

/// Projects [`brushable_item_mesh`]'s own world-space vertices through
/// `view_proj` — the exact function `gpu/world_items.rs::merge_brushable_items`
/// calls, never a remembered literal.
fn expected_rect(
    view_proj: glam::Mat4,
    fixed: &lodestone_assets::DisplayTransform,
    quads: &[lodestone_assets::BakedQuad],
    gui_light: lodestone_assets::GuiLight,
    dust_progress: u8,
) -> Rect {
    let mesh = brushable_item_mesh(
        quads,
        gui_light,
        fixed,
        BLOCK,
        lodestone_assets::Direction::Up,
        dust_progress,
        lodestone_render::ENTITY_FULLBRIGHT,
    );
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    for v in &mesh.vertices {
        let (sx, sy) = project(view_proj, glam::Vec3::from(v.position));
        if sx.is_finite() && sy.is_finite() {
            let cx = sx.clamp(0.0, (W - 1) as f32);
            let cy = sy.clamp(0.0, (H - 1) as f32);
            min = (min.0.min(cx), min.1.min(cy));
            max = (max.0.max(cx), max.1.max(cy));
        }
    }
    Rect {
        x0: min.0.floor() as u32,
        y0: min.1.floor() as u32,
        x1: max.0.ceil().min((W - 1) as f32) as u32,
        y1: max.1.ceil().min((H - 1) as f32) as u32,
    }
}

fn fixed_display_and_quads(
    atlas: &lodestone_render::BlockAtlas,
) -> (lodestone_assets::DisplayTransform, Vec<lodestone_assets::BakedQuad>, lodestone_assets::GuiLight) {
    let item: ResourceLocation = ITEM.parse().expect("valid item id");
    let models = atlas
        .models()
        .expect("the vanilla load must attach baked block models");
    let geometry = models
        .item_forms(&item)
        .and_then(|v| v.resolve(&ItemStateContext::new(DisplaySlot::Fixed)))
        .unwrap_or_else(|| panic!("{ITEM} must have baked 3-D geometry"));
    (
        geometry.display.get(DisplaySlot::Fixed),
        geometry.quads.clone(),
        geometry.gui_light,
    )
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_brushable_items_reveal_draws_in_its_own_screen_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let atlas = load_atlas();
    let camera = camera();

    let (fixed, quads, gui_light) = fixed_display_and_quads(&atlas);
    let rect = expected_rect(camera.view_projection(), &fixed, &quads, gui_light, 2);
    println!("expected rect {rect:?}");
    assert!(
        rect.area() > 50,
        "the revealed item projects to only {} px — this gate cannot measure \
         anything that small: {rect:?}",
        rect.area()
    );

    let mut target = HeadlessTarget::new(device, W, H, format);
    let spawn = test_spawn(2);

    let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        if install {
            let spawn = spawn.clone();
            state.set_brushable_source(move |_eye| vec![spawn.clone()]);
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(true);
    let (control_px, control_stats) = shoot(false);

    assert_eq!(
        subject_stats.brushable_items_drawn, 1,
        "one brushed block with a real item must mesh exactly one revealed item"
    );
    assert_eq!(
        control_stats.brushable_items_drawn, 0,
        "RenderState::new must not default to an installed brushable source"
    );

    let sky = sky_bytes();

    let control_in_rect = non_sky_in(&control_px, rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the item's own rect \
         {rect:?} — something *else* draws there. Control's non-sky bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, rect, sky);
    let fill = subject_in_rect as f64 / rect.area() as f64;
    assert!(
        fill > 0.05,
        "the revealed item fills only {:.1}% of its own projected rect {rect:?} \
         ({subject_in_rect} of {} px). Subject's non-sky bbox: {:?}",
        fill * 100.0,
        rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing a brushable source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
    let allowed = rect.padded(4);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the item's projected rect: changed {changed_rect:?}, \
         allowed {allowed:?}. Installing a brushable source must not repaint anything \
         else in the frame."
    );
}

/// `dust_progress` grows the item's outward lift (`brushable_item_offset`'s
/// `completion_offset` term) — a real per-state change a broken gather could
/// silently ignore (e.g. always reading `dusted` as its default). `1` vs `3`
/// must project to genuinely different rects, and the real rendered frames
/// must differ too.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn dust_progress_changes_the_items_position() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let atlas = load_atlas();
    let camera = camera();
    let view_proj = camera.view_projection();

    let (fixed, quads, gui_light) = fixed_display_and_quads(&atlas);
    let low_rect = expected_rect(view_proj, &fixed, &quads, gui_light, 1);
    let high_rect = expected_rect(view_proj, &fixed, &quads, gui_light, 3);
    println!("dust_progress=1 rect {low_rect:?}, dust_progress=3 rect {high_rect:?}");
    assert_ne!(
        low_rect, high_rect,
        "dust_progress 1 and 3 projected to the *identical* rect — the gather is not \
         reading the block state's `dusted` property into the placement"
    );

    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut shoot = |dust_progress: u8| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        let spawn = test_spawn(dust_progress);
        state.set_brushable_source(move |_eye| vec![spawn.clone()]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };
    let low_px = shoot(1);
    let high_px = shoot(3);
    let (diff_rect, diff_count) = changed_bbox(&low_px, &high_px)
        .expect("dust_progress 1 and 3 produced pixel-identical frames");
    println!("dust_progress 1-vs-3 changed bbox {diff_rect:?} ({diff_count} px)");
}

/// Same discipline every sibling gate in this crate documents: before
/// trusting "the control is clean", locate the unconditional first-person
/// arm and assert it is disjoint from the item's rect.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_first_person_arm_is_somewhere_else() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let atlas = load_atlas();
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    let (fixed, quads, gui_light) = fixed_display_and_quads(&atlas);
    let rect = expected_rect(camera.view_projection(), &fixed, &quads, gui_light, 2);

    let state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
    let pixels = target.read_texels(device, queue);

    assert!(
        stats.first_person_arm_drawn,
        "this test's premise is that the arm paints unconditionally; if it does \
         not, the sibling gate's control is clean for a *different* reason than \
         it claims and its rationale needs rewriting"
    );
    assert_eq!(stats.brushable_items_drawn, 0);

    let sky = sky_bytes();
    let (arm_rect, _arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so an item-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?}, item rect {rect:?}");

    assert!(
        !arm_rect.intersects(rect),
        "the first-person arm ({arm_rect:?}) overlaps the item's rect \
         ({rect:?}). The sibling test would then be measuring the arm, which \
         is exactly the false-control failure `CLAUDE.md` records. Move the \
         block or the camera; do not relax the assertion."
    );
}
