//! Pixel gate: a vault's floating display-item cluster must **draw**, in its
//! own projected screen rect, through the real [`RenderState::render`] path —
//! the same call `app.rs`'s frame loop makes.
//!
//! Not a hole-in-the-world case: a vault's cage/door/base are all real
//! block-model geometry the terrain mesher already draws (`blockstates/
//! vault.json` is a plain `variants` map), so before this landed a vault with
//! a rolled reward looked identical to an empty one — the degradation is a
//! vault that never shows what it holds, not a missing block.
//!
//! # The rect comes from the real placement function, not a baked mesh
//!
//! Like `beacon_beam_pixels.rs`, the expected rect projects
//! [`lodestone_render::entity::vault_display_item_matrix`]'s own output
//! through the real [`Camera::view_projection`] — the exact function
//! `gpu/world_items.rs::merge_vault_items` calls — rather than a remembered
//! literal.
//!
//! # Two known phases, not one frame
//!
//! `CLAUDE.md`'s evidence standard: an animated effect needs two frames at
//! known phases to distinguish an animation from a static pose.
//! [`the_spin_rotates_the_cluster_between_two_known_phases`] renders the same
//! vault at `spin_deg` 0.0 and 90.0 (derived from
//! [`lodestone_render::entity::vault_spin_degrees`]'s own per-tick constant,
//! not a guessed round number) and requires the two frames to differ.
//!
//! # The multi-copy loop is proven by a counter, not just pixels
//!
//! `stats.vault_items_drawn` must equal
//! [`lodestone_render::entity::rendered_amount`]`(count)` exactly — a
//! magnitude prediction, not a "some copies drew" direction check — so a
//! stack of 40 must mesh 4 copies, not 1.
//!
//! ```text
//! cargo test -p lodestone-shell --test vault_item_cluster_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::RenderState;
use lodestone::resources::BlockResources;
use lodestone_assets::{DisplaySlot, ResourceLocation};
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, RenderTarget, VaultSpawn,
    entity::{ground_transform, rendered_amount, vault_display_item_matrix},
    item_render::ItemStateContext,
};

const W: u32 = 320;
const H: u32 = 240;

/// The vault's block position, three blocks ahead of the camera on `+Z`.
const VAULT: [i32; 3] = [0, 0, 3];

/// A solid, non-flat item so the cluster's silhouette is easy to localise —
/// same reasoning `dropped_item_pixels.rs` gives for choosing stone.
const ITEM: &str = "minecraft:diamond";

const NON_SKY: i32 = 60;

fn sky_bytes() -> [u8; 3] {
    lodestone::gpu::SKY_COLOR.map(|c| (c * 255.0).round() as u8)
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

fn diff_count(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(pa, pb)| {
            let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
                + (i32::from(pa[1]) - i32::from(pb[1])).abs()
                + (i32::from(pa[2]) - i32::from(pb[2])).abs();
            d > 12
        })
        .count()
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

/// Eye level with the vault, a few blocks back on `-Z`, looking straight down
/// `+Z` — the same "yaw 0 faces +Z" convention `beacon_beam_pixels.rs` uses.
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

fn test_spawn(count: u32, spin_deg: f32) -> VaultSpawn {
    VaultSpawn {
        pos: VAULT,
        item: ITEM.parse().expect("valid item id"),
        count,
        spin_deg,
        light: lodestone_render::ENTITY_FULLBRIGHT,
    }
}

/// Projects [`vault_display_item_matrix`]'s own output for the *first* copy
/// (`offset = ZERO`) through `view_proj` — the exact composition
/// `gpu/world_items.rs::merge_vault_items` feeds into
/// [`lodestone_render::entity::vault_display_item_mesh`], never a remembered
/// literal.
fn expected_rect(
    view_proj: glam::Mat4,
    block_pos: glam::Vec3,
    spin_deg: f32,
    ground: &lodestone_assets::DisplayTransform,
    quads: &[lodestone_assets::BakedQuad],
) -> Rect {
    let pose = vault_display_item_matrix(block_pos, spin_deg, glam::Vec3::ZERO, ground);
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    for quad in quads {
        for p in &quad.positions {
            let world = pose.transform_point3(glam::Vec3::from(*p));
            let (sx, sy) = project(view_proj, world);
            if sx.is_finite() && sy.is_finite() {
                let cx = sx.clamp(0.0, (W - 1) as f32);
                let cy = sy.clamp(0.0, (H - 1) as f32);
                min = (min.0.min(cx), min.1.min(cy));
                max = (max.0.max(cx), max.1.max(cy));
            }
        }
    }
    Rect {
        x0: min.0.floor() as u32,
        y0: min.1.floor() as u32,
        x1: max.0.ceil().min((W - 1) as f32) as u32,
        y1: max.1.ceil().min((H - 1) as f32) as u32,
    }
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

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_vault_display_item_draws_in_its_own_screen_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let atlas = load_atlas();
    let item: ResourceLocation = ITEM.parse().expect("valid item id");

    let (ground, quads) = {
        let models = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        let geometry = models
            .item_forms(&item)
            .and_then(|v| v.resolve(&ItemStateContext::new(DisplaySlot::Ground)))
            .unwrap_or_else(|| panic!("{ITEM} must have baked 3-D geometry"));
        (
            ground_transform(&geometry.display, geometry.gui_light),
            geometry.quads.clone(),
        )
    };

    let camera = camera();
    let block_pos = glam::Vec3::new(VAULT[0] as f32, VAULT[1] as f32, VAULT[2] as f32);
    let rect = expected_rect(camera.view_projection(), block_pos, 0.0, &ground, &quads);
    println!("expected rect {rect:?}");
    assert!(
        rect.area() > 100,
        "the cluster projects to only {} px — this gate cannot measure \
         anything that small: {rect:?}",
        rect.area()
    );

    let mut target = HeadlessTarget::new(device, W, H, format);
    let spawn = test_spawn(1, 0.0);

    let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        if install {
            let spawn = spawn.clone();
            state.set_vault_source(move |_eye| vec![spawn.clone()]);
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(true);
    let (control_px, control_stats) = shoot(false);

    assert_eq!(
        subject_stats.vault_items_drawn, 1,
        "a stack of 1 must mesh exactly one copy — `rendered_amount(1) == 1`"
    );
    assert_eq!(
        control_stats.vault_items_drawn, 0,
        "RenderState::new must not default to an installed vault source"
    );

    let sky = sky_bytes();

    let control_in_rect = non_sky_in(&control_px, rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the cluster's own rect \
         {rect:?} — something *else* draws there. Control's non-sky bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, rect, sky);
    let fill = subject_in_rect as f64 / rect.area() as f64;
    assert!(
        fill > 0.05,
        "the cluster fills only {:.1}% of its own projected rect {rect:?} \
         ({subject_in_rect} of {} px). Subject's non-sky bbox: {:?}",
        fill * 100.0,
        rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    let diff = diff_count(&subject_px, &control_px);
    assert!(
        diff > 0,
        "installing a vault source changed no pixel at all — the pass is dead"
    );
}

/// `CLAUDE.md`'s evidence standard: an animated effect needs two frames at
/// known phases, not one, to tell an animation from a static pose. Two spins
/// 90° apart (a quarter turn — enough to move a non-symmetric silhouette
/// visibly, derived from the same constant
/// [`lodestone_render::entity::VAULT_SPIN_DEGREES_PER_TICK`] uses, not a
/// guessed round number) must produce genuinely different frames, and the
/// *same* phase rendered twice must be pixel-identical.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_spin_rotates_the_cluster_between_two_known_phases() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let atlas = load_atlas();
    let camera = camera();
    let mut target = HeadlessTarget::new(device, W, H, format);

    let mut shoot = |spin_deg: f32| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        let spawn = test_spawn(1, spin_deg);
        state.set_vault_source(move |_eye| vec![spawn.clone()]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let at_0 = shoot(0.0);
    let at_90 = shoot(90.0);
    let at_0_again = shoot(0.0);

    let moved = diff_count(&at_0, &at_90);
    let repeat = diff_count(&at_0, &at_0_again);

    println!("px differing 0°->90° = {moved}, same-phase repeat = {repeat}");

    assert_eq!(
        repeat, 0,
        "the same spin phase rendered twice must be pixel-identical; {repeat} \
         differing px means the frame is non-deterministic and the 90° \
         comparison below proves nothing"
    );
    assert!(
        moved > 20,
        "a 90-degree spin must visibly change the cluster's silhouette; only \
         {moved} px differ, which reads as a static pose ignoring `spin_deg`"
    );
}

/// The multi-copy loop (`rendered_amount` + the per-copy jitter/fan), proven
/// by a counter rather than only by pixels — the load-bearing assertion is a
/// magnitude, not a "more than one copy drew" direction check.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_large_stack_meshes_the_predicted_number_of_copies() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let atlas = load_atlas();
    let camera = camera();
    let mut target = HeadlessTarget::new(device, W, H, format);

    for count in [1u32, 16, 40, 64] {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        let spawn = test_spawn(count, 0.0);
        state.set_vault_source(move |_eye| vec![spawn.clone()]);
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        let expected = rendered_amount(count);
        assert_eq!(
            stats.vault_items_drawn, expected as usize,
            "count {count} must mesh exactly rendered_amount({count}) = {expected} \
             copies, got {}",
            stats.vault_items_drawn
        );
    }
}

/// Same discipline every sibling gate in this crate documents: before
/// trusting "the control is clean", locate the unconditional first-person
/// arm and assert it is disjoint from the cluster's rect.
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
    let item: ResourceLocation = ITEM.parse().expect("valid item id");

    let (ground, quads) = {
        let models = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        let geometry = models
            .item_forms(&item)
            .and_then(|v| v.resolve(&ItemStateContext::new(DisplaySlot::Ground)))
            .unwrap_or_else(|| panic!("{ITEM} must have baked 3-D geometry"));
        (
            ground_transform(&geometry.display, geometry.gui_light),
            geometry.quads.clone(),
        )
    };
    let block_pos = glam::Vec3::new(VAULT[0] as f32, VAULT[1] as f32, VAULT[2] as f32);
    let rect = expected_rect(camera.view_projection(), block_pos, 0.0, &ground, &quads);

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
    assert_eq!(stats.vault_items_drawn, 0);

    let sky = sky_bytes();
    let (arm_rect, _arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so a vault-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?}, cluster rect {rect:?}");

    assert!(
        !arm_rect.intersects(rect),
        "the first-person arm ({arm_rect:?}) overlaps the cluster's rect \
         ({rect:?}). The sibling test would then be measuring the arm, which \
         is exactly the false-control failure `CLAUDE.md` records. Move the \
         vault or the camera; do not relax the assertion."
    );
}
