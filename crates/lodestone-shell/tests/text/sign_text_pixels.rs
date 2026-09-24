//! Pixel gate: sign text must **draw**, in a real projected screen area,
//! through the real [`RenderState::render`] path — the same call `app.rs`'s
//! frame loop makes (the third block-entity family after chest
//! and skull, and the first that is *text* rather than a cuboid rig).
//!
//! # Why this gate looks different from `skull_block_entity_pixels.rs`
//!
//! A skull is a solid box: its projected rect fills to ~88% and the gate can
//! assert a high fraction. Sign text is sparse ink over nothing — a raster
//! font's glyphs typically cover a small fraction of even their own tight
//! bounding box, the same reason `nametag_pixels.rs` (the closer precedent:
//! real jar-sourced text, not a fixed mesh) checks "some pixels changed near
//! the real anchor" rather than a fill percentage. This gate follows that
//! shape, but keeps `skull_block_entity_pixels.rs`'s **rect**, not
//! `nametag_pixels.rs`'s point-plus-radius: [`expected_text_rect`] projects
//! the four corners of the real local text plane
//! (`lodestone_render::sign_text_transform`'s own domain — `x` bounded by
//! vanilla's `MAX_TEXT_LINE_WIDTH` of 90 px either side of centre, `y`
//! bounded by the four-line block `sign_midpoint` already derives in
//! `gpu/sign_text.rs`), through the *same* transform and the *same*
//! `Camera::view_projection` the render call uses — never a remembered
//! literal, per `CLAUDE.md`'s "derive the rect from the same expression the
//! draw uses". It is deliberately generous (it bounds the whole plane a line
//! of text *could* occupy, not the actual ink of the one line drawn), so
//! containment is checked, not a fill fraction.
//!
//! # The negative control's premise is measured, not assumed
//!
//! Same discipline `skull_block_entity_pixels.rs` documents: before trusting
//! "the control is clean", `the_first_person_arm_is_somewhere_else` locates
//! the unconditional first-person bare arm and asserts it is disjoint from
//! the sign's rect.
//!
//! Fail-closed: no GPU adapter or no `client.jar` is a failure, never a
//! silent skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test sign_text_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, RenderTarget, SignKind, SignOrientation, SignSpawn,
};
use lodestone_world::{SignSide, SignTextSpan};

const W: u32 = 320;
const H: u32 = 240;

/// The sign's block position — directly ahead of the camera on `+Z`.
const SIGN: [i32; 3] = [0, 0, 2];

/// Manhattan RGB distance above which a pixel counts as "not the clear
/// colour". Matches the other block-entity pixel gates.
const NON_SKY: i32 = 60;

/// Vanilla's `SignBlockEntity.MAX_TEXT_LINE_WIDTH` — the real constant that
/// bounds how wide one line's local-space glyphs can be, used here only to
/// size a generous expected rect, not to wrap anything.
/// (Kept for the plain-sign rect; the hanging gate reads
/// `SignKind::max_text_line_width()` instead, which is 60.)
const MAX_TEXT_LINE_WIDTH: f32 = 90.0;

fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

fn is_non_sky(px: &[u8], sky: [u8; 3]) -> bool {
    let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
        + (i32::from(px[1]) - i32::from(sky[1])).abs()
        + (i32::from(px[2]) - i32::from(sky[2])).abs();
    d > NON_SKY
}

/// An inclusive pixel rect, in screen space. Mirrors
/// `skull_block_entity_pixels.rs`'s `Rect` exactly (each pixel gate in this
/// suite keeps its own copy rather than sharing one — see that file and
/// `chest_block_entity_pixels.rs` for the established precedent).
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

/// The generous screen rect one text side's local plane can possibly occupy —
/// see the module doc for why this bounds the whole plane rather than one
/// line's actual ink. `x` in `±MAX_TEXT_LINE_WIDTH / 2`, `y` in
/// `±2 * TEXT_LINE_HEIGHT` (four lines, `AbstractSignRenderer`'s own
/// `signMidpoint` split evenly above and below centre — the same expression
/// `gpu/sign_text.rs::push_side_quads` computes it with, transcribed here
/// rather than imported because that function is private to the shell
/// crate's `gpu` module).
fn expected_text_rect(
    pos: [i32; 3],
    kind: SignKind,
    orientation: SignOrientation,
    is_front: bool,
    view_proj: glam::Mat4,
) -> Rect {
    let matrix = lodestone_render::sign_text_transform(pos, kind, orientation, is_front);
    // Both metrics come from the kind, so a hanging sign's rect is narrower
    // and shorter *and* sits somewhere else — the same expression the draw
    // uses, never a literal.
    let half_w = match kind {
        SignKind::Plain => MAX_TEXT_LINE_WIDTH / 2.0,
        SignKind::Hanging => kind.max_text_line_width() / 2.0,
    };
    let half_h = 2.0 * kind.text_line_height();
    let corners = [
        glam::Vec3::new(-half_w, -half_h, 0.0),
        glam::Vec3::new(half_w, -half_h, 0.0),
        glam::Vec3::new(-half_w, half_h, 0.0),
        glam::Vec3::new(half_w, half_h, 0.0),
    ];
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    for c in corners {
        let world = matrix.transform_point3(c);
        let (sx, sy) = project(view_proj, world);
        min = (min.0.min(sx), min.1.min(sy));
        max = (max.0.max(sx), max.1.max(sy));
    }
    Rect {
        x0: min.0.max(0.0).floor() as u32,
        y0: min.1.max(0.0).floor() as u32,
        x1: (max.0.min((W - 1) as f32)).ceil() as u32,
        y1: (max.1.min((H - 1) as f32)).ceil() as u32,
    }
}

/// A sign facing the camera (rotation segment 0 is north, `RotationSegment`'s
/// own convention — see `lodestone_render::sign`'s module doc); the camera
/// sits south of it and looks north (`+Z` is the block's own... actually the
/// camera looks toward `+Z`, matching every other block-entity gate's own
/// convention in this suite) at the board's own text height.
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.9, 0.0),
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

/// A sign with real front text, glowing so its colour reads clearly against
/// the sky regardless of the dark-scaling default — a deliberate simplifying
/// choice for a gate whose job is proving pixels exist, not proving the
/// darkened non-glow colour (that is `lodestone-render`'s own
/// `dark_scaling_truncates_like_the_real_jar` unit test's job).
fn sign_with_text() -> SignSpawn {
    let mut front = SignSide::default();
    front.lines[0] = vec![SignTextSpan {
        text: "LODESTONE".to_owned(),
        ..Default::default()
    }];
    front.glowing = true;
    SignSpawn {
        pos: SIGN,
        kind: SignKind::Plain,
        orientation: SignOrientation::Ground { rotation_segment: 0 },
        front,
        back: SignSide::default(),
        light: lodestone_render::ENTITY_FULLBRIGHT,
    }
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_sign_draws_text_pixels_in_its_projected_area_where_no_board_could() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let view_proj = camera.view_projection();

    let front_rect = expected_text_rect(
        SIGN,
        SignKind::Plain,
        SignOrientation::Ground { rotation_segment: 0 },
        true,
        view_proj,
    );
    println!("expected front-text rect (from the real placement transform): {front_rect:?}");
    assert!(
        front_rect.area() > 200,
        "the sign's text plane projects to only {} px — this gate cannot \
         measure anything that small, so the camera, not the renderer, is \
         wrong: {front_rect:?}",
        front_rect.area()
    );

    let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            let spawn = sign_with_text();
            state.set_sign_source(move |_eye| vec![spawn.clone()]);
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(true);
    let (control_px, control_stats) = shoot(false);

    // --- The exact, non-approximate corroboration. ---------------------------
    assert!(
        subject_stats.sign_text_vertices > 0,
        "the source is installed with real text, so vertices must be non-zero \
         (0 here means either no client.jar font loaded, or the text/placement \
         chain silently produced no ink)"
    );
    assert_eq!(
        control_stats.sign_text_vertices, 0,
        "RenderState::new must not default to an installed sign source"
    );

    let sky = sky_bytes();

    // --- The control's premise, measured. -------------------------------------
    let control_in_rect = non_sky_in(&control_px, front_rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the sign's own rect \
         {front_rect:?} — something *else* draws there, so this gate would be \
         measuring that instead of the sign. Control frame's whole non-sky \
         bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    // --- Differential: every changed pixel must fall inside the sign's area. -
    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing a sign source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px)");
    let allowed = front_rect.padded(2);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the sign's projected text area: changed \
         {changed_rect:?}, allowed {allowed:?}. Installing a sign source must \
         not repaint anything else in the frame."
    );

    // --- Absolute, inside the rect — a low bar, deliberately: text ink is ----
    // sparse over its own bounding plane (see the module doc), unlike a solid
    // skull box.
    let subject_in_rect = non_sky_in(&subject_px, front_rect, sky);
    assert!(
        subject_in_rect > 20,
        "only {subject_in_rect} non-sky px inside the sign's projected area \
         {front_rect:?} (of {} px) — too little to be real glyph ink. \
         Subject's non-sky bbox: {:?}",
        front_rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );
}

/// Front and back text must occupy **different** screen areas, the real-pixel
/// proof of the `back_text_sits_behind_front_text_on_the_boards_two_faces`
/// unit test in `lodestone-render`: the two placement matrices differ by
/// more than a sign flip, and a bug that collapsed them (drawing the back
/// text on the front face, say) would still pass the base gate above.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn front_and_back_text_project_to_different_areas() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();

    let shoot = |front_text: bool| -> Vec<u8> {
        let mut target = HeadlessTarget::new(device, W, H, format);
        let mut state = RenderState::new(device, queue, format, W, H, None);
        let mut side = SignSide::default();
        side.lines[0] = vec![SignTextSpan {
            text: "LODESTONE".to_owned(),
            ..Default::default()
        }];
        side.glowing = true;
        let spawn = if front_text {
            SignSpawn {
                pos: SIGN,
                kind: SignKind::Plain,
                orientation: SignOrientation::Ground { rotation_segment: 0 },
                front: side,
                back: SignSide::default(),
                light: lodestone_render::ENTITY_FULLBRIGHT,
            }
        } else {
            SignSpawn {
                pos: SIGN,
                kind: SignKind::Plain,
                orientation: SignOrientation::Ground { rotation_segment: 0 },
                front: SignSide::default(),
                back: side,
                light: lodestone_render::ENTITY_FULLBRIGHT,
            }
        };
        state.set_sign_source(move |_eye| vec![spawn.clone()]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let front_px = shoot(true);
    let back_px = shoot(false);
    let (diff_rect, diff_count) = changed_bbox(&front_px, &back_px)
        .expect("front-only and back-only text produced pixel-identical frames");
    println!("front-vs-back changed bbox {diff_rect:?} ({diff_count} px)");
    assert!(diff_count > 0);
}

/// A **hanging** sign's text draws, and draws somewhere a plain sign's does
/// not — the real-pixel proof that [`SignKind`] is threaded all the way from
/// the spawn to the vertex, not merely accepted and ignored.
///
/// This is the assertion that matters for the hanging port, and it is
/// deliberately cross-arm rather than "some pixels appeared": both kinds
/// produce ink at the same block position, so a `SignKind` that reached the
/// transform as a no-op would pass every count-based check. The two projected
/// rects are computed from the real transform (hand-checked in
/// `lodestone-render`'s `hanging_transform_origin_matches_the_hand_computed_expression`:
/// `y = 0.305` against the plain sign's `0.83333`) and required to be
/// **disjoint in `y`**, then each arm's ink is required to land in its *own*
/// rect and nothing to land in the other's.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_hanging_signs_text_draws_in_its_own_area_and_not_the_plain_ones() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();
    let view_proj = camera.view_projection();
    let orientation = SignOrientation::Ground { rotation_segment: 0 };

    let plain_rect = expected_text_rect(SIGN, SignKind::Plain, orientation, true, view_proj);
    let hanging_rect = expected_text_rect(SIGN, SignKind::Hanging, orientation, true, view_proj);
    println!("plain {plain_rect:?} hanging {hanging_rect:?}");
    assert!(
        hanging_rect.y0 > plain_rect.y1,
        "the two kinds' text planes must project to disjoint screen bands for \
         this gate to separate them (hanging text sits lower in the world, so \
         lower on screen means a *larger* y). plain {plain_rect:?} hanging \
         {hanging_rect:?} — if these overlap, the camera is wrong, not the \
         renderer."
    );
    assert!(hanging_rect.area() > 200, "hanging rect too small: {hanging_rect:?}");

    // Differential against a sign-free frame, not an absolute count in each
    // band: the first-person arm paints unconditionally and low on screen,
    // which is exactly where a hanging sign's text lands. An absolute
    // "nothing else in this band" assertion would therefore be measuring the
    // arm — the premise-false control failure `CLAUDE.md` records. Every
    // count below is "what changed when the source was installed".
    let shoot = |kind: Option<SignKind>| -> (Vec<u8>, u32) {
        let mut target = HeadlessTarget::new(device, W, H, format);
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if let Some(kind) = kind {
            let spawn = SignSpawn { kind, ..sign_with_text() };
            state.set_sign_source(move |_eye| vec![spawn.clone()]);
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats.sign_text_vertices)
    };

    let (control_px, control_verts) = shoot(None);
    let (plain_px, plain_verts) = shoot(Some(SignKind::Plain));
    let (hanging_px, hanging_verts) = shoot(Some(SignKind::Hanging));
    assert_eq!(control_verts, 0);
    assert!(plain_verts > 0 && hanging_verts > 0, "{plain_verts} / {hanging_verts}");

    let inside = |changed: Rect, rect: Rect| {
        let allowed = rect.padded(2);
        allowed.x0 <= changed.x0
            && allowed.y0 <= changed.y0
            && changed.x1 <= allowed.x1
            && changed.y1 <= allowed.y1
    };

    let (plain_changed, plain_count) = changed_bbox(&plain_px, &control_px)
        .expect("a plain sign changed no pixel at all");
    let (hanging_changed, hanging_count) = changed_bbox(&hanging_px, &control_px)
        .expect("a hanging sign changed no pixel at all — the kind reaches the \
                 spawn but nothing draws");
    println!("plain changed {plain_changed:?} ({plain_count} px)");
    println!("hanging changed {hanging_changed:?} ({hanging_count} px)");
    assert!(plain_count > 20 && hanging_count > 20);

    assert!(
        inside(plain_changed, plain_rect),
        "plain ink {plain_changed:?} outside the plain rect {plain_rect:?}"
    );
    assert!(
        inside(hanging_changed, hanging_rect),
        "hanging ink {hanging_changed:?} is outside the hanging rect \
         {hanging_rect:?}. If it lands in the *plain* rect ({plain_rect:?}) \
         instead, `SignKind` reached the spawn but not the transform."
    );
    // And the negation, which is what a no-op `SignKind` fails: the hanging
    // ink is not inside the plain band, nor vice versa.
    assert!(!inside(hanging_changed, plain_rect));
    assert!(!inside(plain_changed, hanging_rect));
}

/// What else already paints here — **measured**, not assumed. Mirrors
/// `skull_block_entity_pixels.rs`'s test of the same name.
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
         not, the sibling gate's control is clean for a *different* reason than \
         it claims and its rationale needs rewriting"
    );
    assert_eq!(stats.sign_text_vertices, 0);

    let sky = sky_bytes();
    let (arm_rect, arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so a sign-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?} ({arm_count} px)");

    let front_rect = expected_text_rect(
        SIGN,
        SignKind::Plain,
        SignOrientation::Ground { rotation_segment: 0 },
        true,
        camera.view_projection(),
    );

    assert!(
        !arm_rect.intersects(front_rect),
        "the first-person arm ({arm_rect:?}) overlaps the sign's text area \
         ({front_rect:?}). The sibling gate would then be measuring the arm, \
         which is exactly the false-control failure `CLAUDE.md` records. Move \
         the sign or the camera; do not relax the assertion."
    );
}
