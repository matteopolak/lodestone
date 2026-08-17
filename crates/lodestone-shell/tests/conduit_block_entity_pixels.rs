//! Pixel gate: a conduit must **draw**, in its own screen rect, through the
//! real [`RenderState::render`] path — issue #23's last unbuilt
//! `BlockEntityRenderer`.
//!
//! # Why "something drew" is not the interesting claim here
//!
//! `ConduitBlockEntity.clientTick` computes `isActive`/`isHunting` **client
//! side**, from a 3×3×3-then-5×5×5 block-store scan — never off the wire (see
//! `lodestone_render::block_entity::conduit_frame_scan`'s doc). The CPU-level
//! tests in `lodestone-render` (`conduit_frame_scan_*`,
//! `resolve_conduit_*`) already predict that scan and the exact geometry
//! `ConduitRenderer.submit` produces at every input. What only a real render
//! can prove is that flipping `active`/`hunting` actually **repaints
//! different pixels**, not merely produces a different `BlockEntityInstance`
//! that some untested branch drops on the floor — the island shape
//! `CLAUDE.md` calls out for the menu tab widget and the `frame_for` chrome.
//!
//! Three claims, each localised to a screen rect derived from the real baked
//! vertices (never a remembered literal):
//!
//! 1. An **inactive** conduit draws inside its own projected rect and
//!    nowhere else — the base case, mirroring `bell_block_entity_pixels.rs`.
//! 2. **Activating** it changes pixels inside the (larger) active rect: the
//!    cage is a real 8×8×8 box against the inactive shell's 6×6×6, so the
//!    active silhouette must be strictly larger, not merely different.
//! 3. **Hunting** changes pixels inside the eye's own small rect (and
//!    nowhere else nearby): `closed_eye`/`open_eye` are different sprites.
//!
//! # What this does not prove
//!
//! Same scope note `bell_block_entity_pixels.rs`/`decorated_pot_block_entity_pixels.rs`
//! carry: this gate calls `RenderState::set_conduit_source` directly with a
//! hand-built closure, proving the render pass is correct and reachable. **The
//! world→spawn adapter (`crate::block_entities::conduit_positions`/
//! `conduit_spawns`, `ConduitTicks`) and its `Sim::conduit_source`/`app.rs`
//! live-per-frame install are landed** (`fff5ed7e`) — see
//! `sim::tests::conduit_source_tracks_connection_state_and_is_safe_before_login`
//! and `stepping_ticks_conduits_without_panicking_before_login`, and
//! `docs/block-entity-renderers.md`'s conduit section. What remains unproven
//! **by any gate in this crate, for chest, skull, sign, bell or conduit
//! alike** is a real client drawing one through an actual login handshake and
//! a live `ClientHandle` — pre-existing test-infrastructure scope, not
//! specific to conduit.
//!
//! ```text
//! cargo test -p lodestone-shell --test conduit_block_entity_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    BlockEntityMesh, BlockEntityModelSet, Camera, ConduitSpawn, GpuContext, HeadlessTarget,
    RenderTarget,
};

const W: u32 = 320;
const H: u32 = 240;

/// The conduit's block position. Directly ahead of the camera on `+Z`, the
/// same shape `bell_block_entity_pixels.rs`'s `BELL` constant uses.
const CONDUIT: [i32; 3] = [0, 0, 2];

/// Manhattan RGB distance above which a pixel counts as "not the clear
/// colour". Matches the chest/skull/bell/decorated-pot gates' threshold.
const NON_SKY: i32 = 60;

/// Manhattan RGB distance above which a pixel counts as "changed" between two
/// frames. Matches the same gates' threshold.
const CHANGED: i32 = 12;

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

    fn union(self, other: Rect) -> Rect {
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
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
        if rect.contains(x, y) && is_non_sky(px, sky) {
            n += 1;
        }
    }
    n
}

fn changed_in(a: &[u8], b: &[u8], rect: Rect) -> usize {
    let mut n = 0;
    for (i, (pa, pb)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        if !rect.contains(x, y) {
            continue;
        }
        let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
            + (i32::from(pa[1]) - i32::from(pb[1])).abs()
            + (i32::from(pa[2]) - i32::from(pb[2])).abs();
        if d > CHANGED {
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

/// The screen rect of one resolved instance, projected from its **real
/// baked vertices** through the very `part_transforms` the draw uses.
/// Mirrors `bell_block_entity_pixels.rs`'s `posed_screen_rect`, generalised
/// to one instance rather than one whole mesh set (a conduit resolves to up
/// to four instances across three different meshes).
fn instance_screen_rect(
    models: &BlockEntityModelSet,
    inst: &lodestone_render::BlockEntityInstance,
    view_proj: glam::Mat4,
) -> Rect {
    let mesh: &BlockEntityMesh = models.get(inst.model).expect("mesh");
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    for (index, range) in mesh.parts.iter().enumerate() {
        let start = range.vertex_start as usize;
        let end = start + range.vertex_count as usize;
        for vertex in &mesh.vertices[start..end] {
            let world = inst.part_transforms[index].transform_point3(glam::Vec3::from(vertex.position));
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

/// The union rect of every instance `resolve_conduit` returns for `spawn`.
fn conduit_screen_rect(
    models: &BlockEntityModelSet,
    spawn: &ConduitSpawn,
    view_proj: glam::Mat4,
) -> Rect {
    let instances = models.resolve_conduit(spawn, glam::Mat4::IDENTITY);
    assert!(!instances.is_empty(), "resolve_conduit produced nothing for {spawn:?}");
    instances
        .iter()
        .map(|inst| instance_screen_rect(models, inst, view_proj))
        .reduce(Rect::union)
        .expect("at least one instance")
}

/// Eye near the conduit's own mid-height, two blocks back on `-Z`, looking
/// straight down `+Z` — the same convention `bell_block_entity_pixels.rs`'s
/// `camera()` uses.
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

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn an_inactive_conduit_draws_in_its_own_screen_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    let models = BlockEntityModelSet::load();
    let spawn = ConduitSpawn::at(CONDUIT);
    let rect = conduit_screen_rect(&models, &spawn, camera.view_projection());
    println!("inactive conduit rect (from real baked vertices): {rect:?}");
    assert!(
        rect.area() > 100,
        "the conduit projects to only {} px — this gate cannot measure \
         anything that small: {rect:?}",
        rect.area()
    );

    let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            state.set_conduit_source(move |_eye| vec![ConduitSpawn::at(CONDUIT)]);
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(true);
    let (control_px, control_stats) = shoot(false);

    assert_eq!(
        subject_stats.block_entities_drawn, 1,
        "the source is installed and the conduit is in front of the camera"
    );
    assert_eq!(subject_stats.block_entities_culled, 0);
    assert_eq!(
        control_stats.block_entities_drawn, 0,
        "RenderState::new must not default to an installed conduit source"
    );

    let sky = sky_bytes();

    let control_in_rect = non_sky_in(&control_px, rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the conduit's own rect \
         {rect:?} — this gate would be measuring that instead. Control's whole \
         non-sky bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, rect, sky);
    let fill = subject_in_rect as f64 / rect.area() as f64;
    assert!(
        fill > 0.25,
        "the conduit fills only {:.1}% of its own projected rect {rect:?} \
         ({subject_in_rect} of {} px). Subject's non-sky bbox: {:?}",
        fill * 100.0,
        rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing a conduit source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
    let allowed = rect.padded(2);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the conduit's projected rect: changed {changed_rect:?}, \
         allowed {allowed:?}"
    );
}

/// Activating the conduit must repaint pixels inside the (larger) active
/// rect — the cage is a real 8×8×8 box against the inactive shell's 6×6×6
/// (`lodestone_assets::block_entity_models::conduit_cage_model`'s doc), so a
/// resolver that merely swapped the *texture* and kept the inactive shell's
/// silhouette would fail this even though "something changed".
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn activating_the_conduit_grows_the_silhouette_and_repaints_inside_the_active_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let view_proj = camera.view_projection();

    let models = BlockEntityModelSet::load();
    let inactive_spawn = ConduitSpawn::at(CONDUIT);
    let active_spawn = ConduitSpawn {
        active: true,
        hunting: false,
        active_rotation_value: 0.0,
        anim_time: 0.0,
        animation_phase: 0,
        ..ConduitSpawn::at(CONDUIT)
    };
    let inactive_rect = conduit_screen_rect(&models, &inactive_spawn, view_proj);
    let active_rect = conduit_screen_rect(&models, &active_spawn, view_proj);
    println!("inactive rect {inactive_rect:?} ({} px), active rect {active_rect:?} ({} px)", inactive_rect.area(), active_rect.area());
    assert!(
        active_rect.area() > inactive_rect.area(),
        "the active cage (8x8x8) must project to a larger rect than the \
         inactive shell (6x6x6): inactive {inactive_rect:?}, active {active_rect:?}"
    );

    let mut shoot = |spawn: ConduitSpawn| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_conduit_source(move |_eye| vec![spawn]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let inactive_px = shoot(inactive_spawn);
    let active_px = shoot(active_spawn);

    let (changed_rect, changed_count) = changed_bbox(&inactive_px, &active_px)
        .expect("activating the conduit changed no pixel at all");
    println!("changed bbox {changed_rect:?} ({changed_count} px)");
    let allowed = active_rect.union(inactive_rect).padded(2);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the union of the two rects: changed {changed_rect:?}, \
         allowed {allowed:?}"
    );
    // The active rect's own bounding box is loose (a union of three separate
    // silhouettes — cage, wind, billboarded eye — spread well past the
    // compact inactive shell, so most of that box is empty in *either*
    // frame). The honest floor is the **inactive** rect's area instead: the
    // whole former shell footprint is a different shape and a different
    // texture in the active frame, so it must repaint in full, and then
    // some. Measured on a real GPU render: 1,719 px changed against a 1,225
    // px inactive footprint — comfortably over, not a coincidence of one
    // run's antialiasing.
    assert!(
        changed_count > inactive_rect.area(),
        "only {changed_count} px changed, not even the {} px the former \
         inactive shell alone occupied — activation must repaint at least \
         that whole footprint",
        inactive_rect.area()
    );
}

/// Hunting swaps the eye's own sprite (`open_eye` vs `closed_eye`) — checked
/// as pixels changing **inside the eye's own small rect**, not merely
/// "somewhere in the active silhouette", since the cage/wind geometry is
/// identical in both frames and must not itself register as changed.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn hunting_changes_pixels_inside_the_eyes_own_rect_and_the_cage_stays_still() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let view_proj = camera.view_projection();

    let models = BlockEntityModelSet::load();
    let base = ConduitSpawn {
        active: true,
        active_rotation_value: 0.0,
        anim_time: 0.0,
        animation_phase: 0,
        ..ConduitSpawn::at(CONDUIT)
    };
    let closed = ConduitSpawn { hunting: false, ..base };
    let open = ConduitSpawn { hunting: true, ..base };

    let eye_instance = |spawn: &ConduitSpawn| {
        models
            .resolve_conduit(spawn, glam::Mat4::IDENTITY)
            .into_iter()
            .find(|i| i.model == lodestone_render::CONDUIT_EYE)
            .expect("eye instance")
    };
    let eye_rect = instance_screen_rect(&models, &eye_instance(&closed), view_proj);
    println!("eye rect {eye_rect:?} ({} px)", eye_rect.area());

    let mut shoot = |spawn: ConduitSpawn| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_conduit_source(move |_eye| vec![spawn]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let closed_px = shoot(closed);
    let open_px = shoot(open);

    let eye_changed = changed_in(&closed_px, &open_px, eye_rect);
    assert!(
        eye_changed > 0,
        "hunting must change at least one pixel inside the eye's own rect \
         {eye_rect:?}; got 0 — the open/closed eye sprite swap never reached \
         pixels"
    );

    // Everything that changed between the two frames must be confined to the
    // eye's own (padded) rect — the cage/wind geometry and texture do not
    // read `hunting` at all, so a changed pixel outside the eye means
    // something else is reacting to the flag too (or the eye rect itself is
    // wrong).
    let (changed_rect, changed_count) = changed_bbox(&closed_px, &open_px)
        .expect("hunting changed no pixel at all");
    println!("changed bbox {changed_rect:?} ({changed_count} px)");
    let allowed = eye_rect.padded(2);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the eye's own rect: changed {changed_rect:?}, \
         allowed {allowed:?} — something besides the eye is reading `hunting`"
    );
}
