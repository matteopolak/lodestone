//! Pixel gate: a decorated pot's base and sherd sides must **draw**, and —
//! the discriminating claim for this type — a sherd stored on one named side
//! must repaint only that side's own screen area, never a different one.
//!
//! # Why "a pot drew" is not the interesting claim here
//!
//! A decorated pot draws **four independently-textured sides**
//! (`front`/`back`/`left`/`right`, from the stored `sherds`) on one block, and
//! this crate's block-entity batch key is `(model, texture)` — one texture per
//! *instance*, not per face. A gate that only asserted "something non-sky
//! appears where the pot is" would pass under a wrong implementation that
//! drew all four sides with the *same* sherd repeated (or, worse, transposed
//! two sides) — see
//! `lodestone_render::block_entity::BlockEntityModelSet::resolve_decorated_pot`'s
//! doc for the decomposition this actually uses (a base instance plus four
//! single-quad side instances, each its own `(model, texture)` pair, which
//! keeps the existing bind-group and shader interfaces sufficient).
//!
//! `four_distinct_sherds_produce_four_distinct_textures_on_the_right_faces`
//! and the sibling CPU-level tests in `lodestone-render`/`lodestone-assets`
//! already prove the *resolve* step assigns the right texture to the right
//! named model and the right rest pose to the right named side. What only a
//! real render can prove is that changing one **named** side's stored sherd
//! repaints the correct half of the screen and *only* that half — the
//! transposition trap that makes adjacent same-typed fields easy to confuse.
//!
//! # The camera geometry, derived rather than guessed
//!
//! At `facing_yaw_deg = 0.0` (south, [`horizontal_facing_yaw`]'s
//! convention), [`decorated_pot_placement_matrix`]'s rotation term is
//! `180° - 0° = 180°` — **not** the identity chest's own matrix would give at
//! the same yaw, because of the pot's own extra `180°` term (see that
//! function's doc). Working the rotation through by hand
//! (`front`'s rest pivot sits at local `(0.0625, ~, 0.9375)`, i.e. offset
//! `(-0.4375, ~, +0.4375)` from the pot's centre pivot; a `180°` `Y` rotation
//! negates both `x` and `z` of that offset) puts `front` at world
//! `z ≈ 0.0625` — the block's own `-Z` face — and `back` at the mirror
//! `z ≈ 0.9375`, the `+Z` face. A camera on the `-Z` side looking toward `+Z`
//! (the exact convention `bell_block_entity_pixels.rs`'s `camera()` uses)
//! therefore sees `front` as the near, depth-winning face and `back` as the
//! far, depth-losing one — which is exactly what the differential test below
//! measures rather than assumes.
//!
//! ```text
//! cargo test -p lodestone-shell --test decorated_pot_block_entity_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    BlockEntityMesh, BlockEntityModelSet, Camera, DecoratedPotSpawn, GpuContext, HeadlessTarget,
    RenderTarget,
};

const W: u32 = 320;
const H: u32 = 240;

/// The pot's block position. Directly ahead of the camera on `+Z`, the same
/// shape `bell_block_entity_pixels.rs`'s `BELL` constant uses.
const POT: [i32; 3] = [0, 0, 2];

/// Manhattan RGB distance above which a pixel counts as "changed" between two
/// frames. Matches the chest/skull/bell gates' threshold.
const CHANGED: i32 = 12;

/// Manhattan RGB distance above which a pixel counts as "not the clear
/// colour". Matches the chest/skull/bell gates' threshold.
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

/// An inclusive pixel rect, in screen space.
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
        if d <= CHANGED {
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
        if rect.contains_xy(x, y) && is_non_sky(px, sky) {
            n += 1;
        }
    }
    n
}

impl Rect {
    fn contains_xy(self, x: u32, y: u32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
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

/// The screen rect of every part of `mesh`, posed by `part_transforms` and
/// projected through the real `view_proj` — same helper shape
/// `bell_block_entity_pixels.rs`'s `posed_screen_rect` uses.
fn posed_screen_rect(
    mesh: &BlockEntityMesh,
    part_transforms: &[glam::Mat4],
    view_proj: glam::Mat4,
) -> Rect {
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    for (index, range) in mesh.parts.iter().enumerate() {
        let start = range.vertex_start as usize;
        let end = start + range.vertex_count as usize;
        for vertex in &mesh.vertices[start..end] {
            let world = part_transforms[index].transform_point3(glam::Vec3::from(vertex.position));
            let (sx, sy) = project(view_proj, world);
            min = (min.0.min(sx), min.1.min(sy));
            max = (max.0.max(sx), max.1.max(sy));
        }
    }
    assert!(min.0 < max.0 && min.1 < max.1, "no vertices projected");
    Rect {
        x0: min.0.max(0.0).floor() as u32,
        y0: min.1.max(0.0).floor() as u32,
        x1: (max.0.min((W - 1) as f32)).ceil() as u32,
        y1: (max.1.min((H - 1) as f32)).ceil() as u32,
    }
}

/// Union of every instance's own projected rect — the pot's whole silhouette.
fn union_rect(rects: impl IntoIterator<Item = Rect>) -> Rect {
    let mut it = rects.into_iter();
    let mut acc = it.next().expect("at least one rect");
    for r in it {
        acc = Rect {
            x0: acc.x0.min(r.x0),
            y0: acc.y0.min(r.y0),
            x1: acc.x1.max(r.x1),
            y1: acc.y1.max(r.y1),
        };
    }
    acc
}

/// Camera two blocks back on `-Z` from the pot, looking straight down `+Z`
/// (yaw `0` faces `+Z` in Minecraft's convention) at the pot's own
/// mid-height — the same shape `bell_block_entity_pixels.rs`'s `camera()`.
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.5, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
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

/// An undecorated pot must draw its base and all four sides, filling most of
/// its own projected rect — the same "hole in the world otherwise" shape
/// `chest_block_entity_pixels.rs`/`bell_block_entity_pixels.rs` measure.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_decorated_pot_draws_in_its_own_screen_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let view_proj = camera.view_projection();

    let models = BlockEntityModelSet::load();
    let spawn = DecoratedPotSpawn::at(POT);
    let instances = models
        .resolve_decorated_pot(&spawn)
        .expect("the decorated-pot corpus must resolve");
    let pot_rect = union_rect(instances.iter().map(|inst| {
        let mesh = models.get(inst.model).expect("mesh");
        posed_screen_rect(mesh, &inst.part_transforms, view_proj)
    }));
    println!("pot rect (from real baked vertices): {pot_rect:?}");
    assert!(
        pot_rect.area() > 100,
        "the pot projects to only {} px — the camera, not the renderer, is \
         wrong: {pot_rect:?}",
        pot_rect.area()
    );

    let mut shoot = |install: bool| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            state.set_decorated_pot_source(move |_eye| vec![DecoratedPotSpawn::at(POT)]);
        }
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let subject_px = shoot(true);
    let control_px = shoot(false);
    let sky = sky_bytes();

    let control_in_rect = non_sky_in(&control_px, pot_rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the pot's own rect \
         {pot_rect:?} — something else draws there"
    );

    let subject_in_rect = non_sky_in(&subject_px, pot_rect, sky);
    let fill = subject_in_rect as f64 / pot_rect.area() as f64;
    assert!(
        fill > 0.25,
        "the pot fills only {:.1}% of its own projected rect {pot_rect:?} \
         ({subject_in_rect} of {} px)",
        fill * 100.0,
        pot_rect.area()
    );

    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing a decorated-pot source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
}

/// **The discriminating gate.** A sherd stored on `front` must repaint the
/// pot's near (visible) face; the *same* sherd stored on `back` instead must
/// change **nothing** in that frame, because `back` is the far face and loses
/// the depth test to `front`'s own quad. A resolver that swapped `front` and
/// `back` — wiring `DecoratedPotSpawn::front` to
/// `lodestone_render::DECORATED_POT_SIDE_BACK`'s texture, or vice versa —
/// would invert this pair of results exactly, which a single "did the pot
/// change colour somewhere" assertion could not catch.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_sherd_on_the_visible_side_repaints_it_and_the_same_sherd_on_the_hidden_side_does_not() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();

    let blank = || DecoratedPotSpawn::at(POT);
    let with_front = || DecoratedPotSpawn {
        front: Some("angler_pottery_sherd".to_string()),
        ..blank()
    };
    let with_back = || DecoratedPotSpawn {
        back: Some("angler_pottery_sherd".to_string()),
        ..blank()
    };

    let shoot = |spawn: DecoratedPotSpawn| -> Vec<u8> {
        let mut target = HeadlessTarget::new(device, W, H, format);
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_decorated_pot_source(move |_eye| vec![spawn.clone()]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let blank_px = shoot(blank());
    let front_px = shoot(with_front());
    let back_px = shoot(with_back());

    let front_change = changed_bbox(&blank_px, &front_px);
    let back_change = changed_bbox(&blank_px, &back_px);

    println!("front-sherd change: {front_change:?}");
    println!("back-sherd change: {back_change:?}");

    assert!(
        front_change.is_some(),
        "storing a sherd on `front` changed no pixel at all — either `front` \
         is not the visible face at this camera (the module doc's derivation \
         is wrong) or the sherd never reaches the mesh"
    );
    assert!(
        back_change.is_none(),
        "storing the same sherd on `back` changed pixels at {back_change:?} — \
         `back` is supposed to be the far, depth-losing face from this \
         camera. A `front`/`back` transposition in `resolve_decorated_pot` \
         produces exactly this symptom."
    );
}

/// What else already paints here — measured, not assumed. Same control
/// `bell_block_entity_pixels.rs`'s `the_first_person_arm_is_somewhere_else`
/// uses.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_first_person_arm_is_somewhere_else() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    let state = RenderState::new(device, queue, format, W, H, None);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
    let pixels = target.read_texels(device, queue);

    assert!(
        stats.first_person_arm_drawn,
        "this test's premise is that the arm paints unconditionally; if it \
         does not, the sibling gates' control is clean for a different \
         reason than they claim"
    );
    assert_eq!(stats.block_entities_drawn, 0);

    let sky = sky_bytes();
    let (arm_rect, arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so a pot-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?} ({arm_count} px)");

    let models = BlockEntityModelSet::load();
    let view_proj = camera.view_projection();
    let instances = models
        .resolve_decorated_pot(&DecoratedPotSpawn::at(POT))
        .expect("the decorated-pot corpus must resolve");
    let pot_rect = union_rect(instances.iter().map(|inst| {
        let mesh = models.get(inst.model).expect("mesh");
        posed_screen_rect(mesh, &inst.part_transforms, view_proj)
    }));

    assert!(
        !arm_rect.intersects(pot_rect),
        "the first-person arm ({arm_rect:?}) overlaps the pot's rect \
         ({pot_rect:?}). Move the pot or the camera; do not relax the \
         assertion."
    );
}
