//! Pixel gate: a chest must **draw**, in its own screen rect, through the real
//! [`RenderState::render`] path — the same call `app.rs`'s frame loop makes.
//!
//! # Why this gate is the whole point of the change
//!
//! A 26.2 chest has **no block model**. `assets/minecraft/blockstates/chest.json`
//! points at `block/chest`, and that file is verbatim
//! `{"textures":{"particle":"minecraft:block/oak_planks"}}` — zero elements. So
//! before this work a chest was a *hole in the world*, and no terrain metric
//! could see it: `sections_drawn`, `total_quads` and every existing pixel gate
//! are byte-identical with and without chests rendering.
//!
//! `lodestone-render`'s own `block_entity` unit tests prove the bake, the
//! placement matrix and the lid maths. They are a **closed loop** with respect to
//! this crate: none of them calls `RenderState::prepare_block_entities`, so all 17
//! would stay green with the shell pass deleted. Per `CLAUDE.md`'s dominant defect
//! class this gate drives the real shell path instead, and asserts coverage
//! *inside the subject's screen rect* rather than a frame average.
//!
//! # The metric, and why it is a rect and not a fraction
//!
//! Two measurements over the same pair of frames:
//!
//! 1. **Differential.** Subject minus control, pixel by pixel. Everything else in
//!    the frame — the clear, the unconditional first-person arm — is identical in
//!    both, so every changed pixel is the chest. Its **bounding box** must fall
//!    inside the chest's own projected rect.
//! 2. **Absolute, inside the rect.** The control must paint ~nothing there and
//!    the subject must fill most of it. This is what distinguishes "the chest
//!    drew" from "something changed somewhere".
//!
//! The expected rect is projected from the **real baked vertices** of the real
//! corpus mesh, through the *same* [`Camera::view_projection`] the render call
//! uses and the *same* `part_transforms` the draw uses — never a remembered
//! literal. Per `CLAUDE.md`, failure output prints a bounding box, because a
//! fraction cannot tell a uniform-but-wrong frame from a localised blob.
//!
//! # The negative control's premise is measured, not assumed
//!
//! `CLAUDE.md` records four cases where a control's premise was false before the
//! feature existed — most memorably a "clears uniformly" assertion that failed at
//! 3.5% because of the **first-person bare arm**, which the hand pass draws in
//! every frame where no third-person body did. That arm is drawn here too.
//! `the_first_person_arm_is_somewhere_else` therefore *locates* it and asserts its
//! bounding box is disjoint from the chest's rect, rather than assuming the rect
//! is clean. If the arm ever moves into that rect this gate fails loudly instead
//! of quietly measuring the arm.
//!
//! Fail-closed: no GPU adapter or no `client.jar` is a **failure, never a skip**.
//! A chest with no sheet draws nothing rather than a placeholder box, so a missing
//! pack would otherwise read as a quiet, indistinguishable-from-passing zero —
//! which is exactly the bug this gate exists to catch.
//!
//! ```text
//! cargo test -p lodestone-shell --test chest_block_entity_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    BlockEntityMesh, BlockEntityModelSet, Camera, ChestSpawn, GpuContext, HeadlessTarget,
    RenderTarget,
};

const W: u32 = 320;
const H: u32 = 240;

/// The chest's block position. Directly ahead of the camera on `+Z`.
const CHEST: [i32; 3] = [0, 0, 4];

/// Manhattan RGB distance above which a pixel counts as "not the clear colour".
/// Matches `sheep_wool_pixels.rs`'s threshold; the chest sheet is oak brown
/// against a sky-blue clear, so the real separation is far above it.
const NON_SKY: i32 = 60;

/// Every block-entity sheet the loader asks the jar for. Asserted so a *silently*
/// jar-less run cannot pass this gate by drawing nothing.
///
/// **Derived, not a literal.** This was `22` — chests only — and went stale the
/// moment the skull renderer added its own sheets to the same loader, failing a
/// gate that had nothing to do with skulls. The loader iterates
/// `block_entity_texture_stems()`, so asking that same function is the only way the
/// number cannot drift again; hardcoding it restates a constant the draw already
/// owns, which is the mistake CLAUDE.md warns about.
///
/// `MIN_SHEETS` keeps the original property the literal was there for: a derived
/// count alone would still pass if the stem list *shrank* to nothing, so the floor
/// catches a corpus that lost entries while the equality catches one that failed to
/// decode them.
fn expected_sheets() -> usize {
    lodestone_render::block_entity::block_entity_texture_stems().len()
}

/// The chest-only corpus size, as a floor. Any future renderer only adds sheets, so
/// dropping below this means the stem list itself regressed.
const MIN_SHEETS: usize = 22;

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

    /// Grown by `pad` pixels on every side, clamped to the frame.
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

/// Bounding box of every pixel `predicate` accepts, plus the count — `None` when
/// nothing matched.
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
            None => Rect {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
            },
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

/// Bounding box of the pixels that differ between two frames.
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
            None => Rect {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
            },
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

/// The screen rect of a posed mesh, projected from its **real baked vertices**
/// through the very `part_transforms` the draw uses.
///
/// `parts` selects which parts to include by name; an empty slice means all of
/// them. Every vertex is projected individually rather than the eight corners of
/// an AABB, because a rotated lid's AABB is much larger than the lid, and this
/// gate's whole value is that the rect is tight.
fn posed_screen_rect(
    mesh: &BlockEntityMesh,
    part_transforms: &[glam::Mat4],
    view_proj: glam::Mat4,
    parts: &[&str],
) -> Rect {
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    for (index, range) in mesh.parts.iter().enumerate() {
        if !parts.is_empty() && !parts.contains(&mesh.part_names[index].as_str()) {
            continue;
        }
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

/// Eye slightly above the chest's mid-height, four blocks back on `-Z`, looking
/// straight down `+Z` (yaw `0` faces `+Z` in Minecraft's convention).
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.45, 0.0),
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
fn a_chest_draws_in_its_own_screen_rect_where_no_block_model_could() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    // --- The expected rect, from the real corpus mesh and the real matrices ---
    let models = BlockEntityModelSet::load();
    let spawn = ChestSpawn::at(CHEST);
    let instance = models
        .resolve_chest(&spawn)
        .expect("the single-chest model must be in the corpus");
    let mesh = models.get(instance.model).expect("mesh");
    let chest_rect = posed_screen_rect(
        mesh,
        &instance.part_transforms,
        camera.view_projection(),
        &[],
    );
    println!("chest rect (from real baked vertices): {chest_rect:?}");
    assert!(
        chest_rect.area() > 900,
        "the chest projects to only {} px — this gate cannot measure anything \
         that small, so the camera, not the renderer, is wrong: {chest_rect:?}",
        chest_rect.area()
    );

    // --- Subject: the source installed. Control: no source at all. -----------
    let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            state.set_block_entity_source(move |_eye| vec![ChestSpawn::at(CHEST)]);
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(true);
    let (control_px, control_stats) = shoot(false);

    // --- The sheets really loaded. -------------------------------------------
    // Without this a jar-less run draws nothing and every "the chest is absent"
    // assertion below would still be satisfiable by a broken renderer.
    let expected = expected_sheets();
    assert!(
        expected >= MIN_SHEETS,
        "the block-entity stem list itself regressed: {expected} stems, floor is \
         {MIN_SHEETS} (the chest-only corpus). Renderers only add sheets."
    );
    assert_eq!(
        subject_stats.block_entity_sheets_loaded, expected,
        "expected all {expected} block-entity sheets from client.jar; a short count \
         means the pack is missing or a stem is misspelled, and this gate cannot \
         distinguish that from a broken pass"
    );

    // --- The exact, non-approximate corroboration. ---------------------------
    assert_eq!(
        subject_stats.block_entities_drawn, 1,
        "the source is installed and the chest is in front of the camera"
    );
    assert_eq!(subject_stats.block_entities_culled, 0);
    assert_eq!(
        control_stats.block_entities_drawn, 0,
        "RenderState::new must not default to an installed block-entity source"
    );

    let sky = sky_bytes();

    // --- (2) Absolute, inside the rect. The control's premise, measured. -----
    let control_in_rect = non_sky_in(&control_px, chest_rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the chest's own rect \
         {chest_rect:?} — something *else* draws there, so this gate would be \
         measuring that instead of the chest. Control frame's whole non-sky \
         bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, chest_rect, sky);
    let fill = subject_in_rect as f64 / chest_rect.area() as f64;
    assert!(
        fill > 0.45,
        "the chest fills only {:.1}% of its own projected rect {chest_rect:?} \
         ({subject_in_rect} of {} px). A closed chest is a solid box, so anything \
         this sparse means it drew partially, inside-out, or somewhere else. \
         Subject's non-sky bbox: {:?}",
        fill * 100.0,
        chest_rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    // --- (1) Differential: every changed pixel must *be* the chest. ----------
    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing a chest source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
    let allowed = chest_rect.padded(2);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the chest's projected rect: changed {changed_rect:?}, \
         allowed {allowed:?}. Installing a block-entity source must not repaint \
         anything else in the frame."
    );
    assert!(
        changed_count > chest_rect.area() / 2,
        "only {changed_count} px changed inside a {} px rect",
        chest_rect.area()
    );
}

/// The lid animation has to reach **pixels**, not merely a different matrix.
///
/// Both frames install the source and both draw exactly one chest, so the pass
/// runs identically in each and the *only* difference is the openness — the
/// tightest control available. The assertion is directional and localised: a
/// fully open lid stands up and tips toward the camera, so it must paint chest
/// pixels **above** the closed chest's own top edge, where the closed frame has
/// only sky.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn opening_the_lid_paints_above_the_closed_chests_silhouette() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    let models = BlockEntityModelSet::load();
    let closed_spawn = ChestSpawn::at(CHEST);
    let open_spawn = ChestSpawn {
        openness: 1.0,
        ..closed_spawn
    };
    let closed = models.resolve_chest(&closed_spawn).expect("closed");
    let open = models.resolve_chest(&open_spawn).expect("open");
    let mesh = models.get(closed.model).expect("mesh");
    let view_proj = camera.view_projection();

    let closed_rect = posed_screen_rect(mesh, &closed.part_transforms, view_proj, &[]);
    let open_lid_rect = posed_screen_rect(mesh, &open.part_transforms, view_proj, &["lid"]);
    println!("closed rect {closed_rect:?}, open lid rect {open_lid_rect:?}");
    // Screen Y grows downward, so "above" is a *smaller* y. Assert the test's own
    // geometry first: if the open lid does not project above the closed chest at
    // all, the assertion below would be vacuous rather than failing.
    assert!(
        open_lid_rect.y0 + 8 < closed_rect.y0,
        "the open lid projects to y0 {} against the closed chest's {} — this \
         camera cannot see the swing, so the pixel assertion below would measure \
         nothing. Fix the camera, not the renderer.",
        open_lid_rect.y0,
        closed_rect.y0
    );
    // The band that is sky when shut and lid when open.
    let band = Rect {
        x0: closed_rect.x0,
        y0: open_lid_rect.y0,
        x1: closed_rect.x1,
        y1: closed_rect.y0.saturating_sub(2),
    };
    println!("swing band {band:?} ({} px)", band.area());

    let mut shoot = |spawn: ChestSpawn| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_block_entity_source(move |_eye| vec![spawn]);
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (closed_px, closed_stats) = shoot(closed_spawn);
    let (open_px, open_stats) = shoot(open_spawn);

    // Both drew — this is not "one chest vs no chest".
    assert_eq!(closed_stats.block_entities_drawn, 1);
    assert_eq!(open_stats.block_entities_drawn, 1);
    assert_eq!(
        closed_stats.block_entity_sheets_loaded,
        expected_sheets(),
        "the vanilla pack must be present for this gate to mean anything"
    );

    let sky = sky_bytes();
    let closed_band = non_sky_in(&closed_px, band, sky);
    let open_band = non_sky_in(&open_px, band, sky);
    println!("swing band: closed {closed_band} px, open {open_band} px");

    // The control half: shut, that band is sky. If this fires, the band was
    // mis-derived and the whole comparison is meaningless.
    assert!(
        closed_band * 20 < band.area(),
        "the closed chest already paints {closed_band} of {} px in the band above \
         its own top edge — the band is wrong, not the animation",
        band.area()
    );
    // The subject half: open, it is lid.
    assert!(
        open_band > closed_band + band.area() / 5,
        "an open lid painted only {open_band} px in the {} px band above the closed \
         silhouette (closed: {closed_band}). The lid matrices moved (proved by \
         `lodestone-render`'s unit tests) but no pixel followed, which is the \
         island this gate exists to catch. Changed bbox: {:?}",
        band.area(),
        changed_bbox(&open_px, &closed_px)
    );
}

/// What else already paints here — **measured**, not assumed.
///
/// `CLAUDE.md` records a control that asserted a frame "clears uniformly" and
/// failed at 3.5% because of the unconditional first-person bare arm. That arm is
/// drawn in this gate's frames too (nothing installs a third-person body, so the
/// hand pass always runs). This test locates it and asserts it is disjoint from
/// the chest's rect, so the sibling gates' clean-control premise is a measurement
/// rather than a hope.
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
        "this test's premise is that the arm paints unconditionally; if it does \
         not, the sibling gates' controls are clean for a *different* reason than \
         they claim and their rationale needs rewriting"
    );
    assert_eq!(stats.block_entities_drawn, 0);

    let sky = sky_bytes();
    let (arm_rect, arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so a chest-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?} ({arm_count} px)");

    let models = BlockEntityModelSet::load();
    let instance = models
        .resolve_chest(&ChestSpawn::at(CHEST))
        .expect("single chest");
    let mesh = models.get(instance.model).expect("mesh");
    let chest_rect =
        posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection(), &[]);

    assert!(
        !arm_rect.intersects(chest_rect),
        "the first-person arm ({arm_rect:?}) overlaps the chest's rect \
         ({chest_rect:?}). The sibling gates would then be measuring the arm, \
         which is exactly the false-control failure `CLAUDE.md` records. Move the \
         chest or the camera; do not relax the assertion."
    );
}
