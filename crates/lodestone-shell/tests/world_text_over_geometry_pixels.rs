//! Pixel gate: world-space text must survive the **depth test against the
//! geometry it sits on**, not merely reach a vertex buffer.
//!
//! # Why the existing gates could not see this
//!
//! `sign_text_pixels.rs` installs a [`SignSpawn`] and renders it against an
//! **empty world** — there is no sign board, no terrain, nothing in the depth
//! buffer at all, so the polygon-offset bias the pass exists to apply has
//! nothing to win against and any depth failure is unobservable. Every sign
//! gate in the suite shares that fixture shape, which is the
//! shared-blind-spot species `CLAUDE.md`'s evidence standards describe: no
//! individual gate is badly written, the whole corpus simply never puts the
//! subject in the state the feature exists for.
//!
//! `text_display_pixels.rs` has the mirror-image gap. It asserts "more than
//! 100 non-sky pixels inside the entity's rect" — a threshold the
//! **background panel alone** satisfies, at three blocks' range, so a
//! `text_display` whose glyphs are entirely eaten by the panel in front of
//! them still passes. This file therefore measures the glyph ink *against a
//! reference render of the same text with the panel switched off*, at a
//! realistic viewing distance, with the camera pitched up — the exact
//! condition the report describes.
//!
//! Fail-closed: no GPU adapter or no `client.jar` is a failure, never a
//! silent skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test world_text_over_geometry_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR, ThirdPersonBodyState};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility,
};
use lodestone::resources::BlockResources;
use lodestone_render::display::{BillboardMode, DisplayTransformation};
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, SignKind, SignOrientation,
    SignSpawn, entity_anim::AnimInput, fog::FogSettings, sign_text_transform,
};
use lodestone::display_entities::{DisplayDraw, TEXT_DISPLAY_TYPE_PATH};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, SignSide,
    SignTextSpan, World,
};

const W: u32 = 640;
const H: u32 = 480;

/// Render distance, in chunks — `RenderState::new`'s own default, so the fog
/// and cull the fixture assumes are the ones the renderer applies.
const RD_CHUNKS: u32 = 8;

const MIN_Y: i32 = 0;
const SECTION_COUNT: usize = 8;
/// The sign's own block position: the top of a stone platform at `y = 64`.
const SIGN: [i32; 3] = [0, 65, 0];
const GROUND_Y: i32 = 64;

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

fn load_vanilla() -> (BlockResources, std::sync::Arc<lodestone_render::BlockAtlas>) {
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real \
             client.jar under .cache/mc/26.2 (LODESTONE_ASSETS)",
            resources.banner
        )
    });
    (resources, atlas)
}

/// See `distant_flat_terrain_holes.rs`'s identical helper: `RenderState`
/// draws an unconditional first-person bare arm whenever no third-person
/// body is reported, at a fixed screen rect regardless of camera angle.
fn suppress_first_person_arm(state: &mut RenderState) {
    state.set_third_person_body_source(|| {
        Some(ThirdPersonBodyState {
            feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
            body_yaw_deg: 0.0,
            anim: AnimInput::default(),
            scale: 1.0,
            swim_amount: 0.0,
            slim: false,
            equipment: Vec::new(),
        })
    });
}

fn state_id(state: &str) -> u32 {
    lodestone_data::block_states::state_id(state)
        .unwrap_or_else(|| panic!("{state} is not in the 26.2 block-state table"))
}

/// Manhattan RGB distance above which two renders count as different at a
/// pixel. Same threshold every other block-entity pixel gate here uses.
const DIFFERS: i32 = 60;

fn differs(a: &[u8], b: &[u8]) -> bool {
    (i32::from(a[0]) - i32::from(b[0])).abs()
        + (i32::from(a[1]) - i32::from(b[1])).abs()
        + (i32::from(a[2]) - i32::from(b[2])).abs()
        > DIFFERS
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Rect {
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    fn padded(self, pad: i32) -> Rect {
        Rect {
            x0: self.x0 - pad,
            y0: self.y0 - pad,
            x1: self.x1 + pad,
            y1: self.y1 + pad,
        }
    }
}

fn changed(subject: &[u8], control: &[u8]) -> (usize, Option<Rect>) {
    let mut count = 0usize;
    let mut rect: Option<Rect> = None;
    for (i, (s, c)) in subject
        .chunks_exact(4)
        .zip(control.chunks_exact(4))
        .enumerate()
    {
        if !differs(s, c) {
            continue;
        }
        count += 1;
        let x = (i as u32 % W) as i32;
        let y = (i as u32 / W) as i32;
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
    (count, rect)
}

fn project(view_proj: glam::Mat4, world: glam::Vec3) -> Option<(f32, f32)> {
    let clip = view_proj * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    Some((
        (ndc_x * 0.5 + 0.5) * W as f32,
        (1.0 - (ndc_y * 0.5 + 0.5)) * H as f32,
    ))
}

// --- The sign arm ----------------------------------------------------------

/// A world with a stone platform and one real `oak_sign` **block** standing
/// on it — the board itself, from the ordinary terrain mesh, which is what
/// the existing sign gates omit.
fn sign_world() -> World {
    let air = state_id("minecraft:air");
    let stone = state_id("minecraft:stone");
    // `rotation=0` so `SignOrientation::Ground { rotation_segment: 0 }`
    // below describes the very block placed here, rather than a different
    // one that happens to share a name.
    let sign = state_id("minecraft:oak_sign[rotation=0,waterlogged=false]");

    let mut world = World::new();
    for cx in -1..=1 {
        for cz in -1..=1 {
            let column = ChunkColumn::new(
                MIN_Y,
                SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                air,
                0,
            );
            let mut light = ColumnLight::new(SECTION_COUNT);
            for i in 0..light.light_section_count() {
                *light.sky_mut(i) = lodestone_world::LightData::Uniform(15);
                *light.block_mut(i) = lodestone_world::LightData::Uniform(0);
            }
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
            );
        }
    }
    let filled = world.fill_region([-16, MIN_Y, -16], [15, GROUND_Y, 15], stone);
    assert!(filled > 0, "fixture: the platform must actually write blocks");
    world.fill_region(SIGN, SIGN, sign);
    world
}

fn upload_world(
    world: &World,
    models: &lodestone_render::BlockModels,
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) {
    let mut uploaded = 0usize;
    for cx in -1..=1 {
        for cz in -1..=1 {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let opaque = mesh_snapshot_models(&snap, models, false);
                let visibility = snapshot_visibility(&snap, models);
                let geometry = SectionGeometry::Model {
                    opaque,
                    water: ModelMesh::default(),
                    translucent_blocks: ModelMesh::default(),
                    visibility,
                };
                state.upload_section(device, queue, key, &geometry);
                uploaded += 1;
            }
        }
    }
    assert!(uploaded > 0, "fixture: some sections must have uploaded");
}

fn sign_spawn() -> SignSpawn {
    let mut front = SignSide::default();
    front.lines[0] = vec![SignTextSpan { text: "LODESTONE".to_owned(), ..Default::default() }];
    front.lines[1] = vec![SignTextSpan { text: "HOSPITAL".to_owned(), ..Default::default() }];
    // Glowing so the ink is the full dye colour rather than the 0.4-scaled
    // dark default — this gate is about whether the ink survives the depth
    // test against the board, not about the dark-scale formula, and black
    // ink on an oak board is a needlessly small signal for that question.
    front.glowing = true;
    front.color = lodestone_world::SignDyeColor::White;
    SignSpawn {
        pos: SIGN,
        kind: SignKind::Plain,
        orientation: SignOrientation::Ground { rotation_segment: 0 },
        front,
        back: SignSide::default(),
        light: lodestone_render::ENTITY_FULLBRIGHT,
    }
}

/// A camera facing the sign's **own** text plane, derived from
/// [`sign_text_transform`]'s matrix rather than from a remembered heading:
/// the plane's local `+Z` axis is the side a reader stands on, so the eye
/// goes `distance` blocks along it and looks back down `-Z`.
fn camera_facing_sign(distance: f32) -> Camera {
    let matrix = sign_text_transform(
        SIGN,
        SignKind::Plain,
        SignOrientation::Ground { rotation_segment: 0 },
        true,
    );
    let origin = matrix.transform_point3(glam::Vec3::ZERO);
    let normal = (matrix.transform_point3(glam::Vec3::Z) - origin).normalize();
    let eye = origin + normal * distance;
    let dir = -normal;
    Camera {
        position: eye,
        // `Camera::basis`: forward is
        // `(-sin(yaw)cos(pitch), -sin(pitch), cos(yaw)cos(pitch))`, inverted
        // here rather than guessed.
        yaw: (-dir.x).atan2(dir.z).to_degrees(),
        pitch: -dir.y.clamp(-1.0, 1.0).asin().to_degrees(),
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS, 0),
    }
}

/// The screen rect the sign's text plane can occupy, projected through the
/// same transform and the same `view_projection` the draw uses — the local
/// domain is vanilla's `MAX_TEXT_LINE_WIDTH` either side of centre and the
/// four-line block `gpu/sign_text.rs` centres on `2 * text_line_height`.
fn expected_text_rect(view_proj: glam::Mat4) -> Rect {
    let matrix = sign_text_transform(
        SIGN,
        SignKind::Plain,
        SignOrientation::Ground { rotation_segment: 0 },
        true,
    );
    let half_w = SignKind::Plain.max_text_line_width() / 2.0;
    let half_h = 2.0 * SignKind::Plain.text_line_height();
    let mut rect: Option<Rect> = None;
    for (lx, ly) in [
        (-half_w, -half_h),
        (half_w, -half_h),
        (-half_w, half_h),
        (half_w, half_h),
    ] {
        let world = matrix.transform_point3(glam::Vec3::new(lx, ly, 0.0));
        let (sx, sy) = project(view_proj, world).expect("the text plane must be in front");
        let (x, y) = (sx.round() as i32, sy.round() as i32);
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
    rect.expect("four corners")
}

/// **The gate the existing sign corpus structurally cannot run**: the sign's
/// own board is in the depth buffer, so text that loses the depth tie against
/// it disappears exactly the way it does in real play, while every existing
/// sign gate — rendering against an empty world — stays green.
///
/// It is green as written, and that is the finding it was built to produce.
/// The reported symptom (a server's signs drawing their boards and no text)
/// is **not** in this pass: given real spans, the ink reaches pixels over a
/// real board at both a touching and a reading distance. The defect was one
/// layer up, in `lodestone_world::sign_text`, which parsed each `messages`
/// element as JSON and so produced *no spans at all* for any line carrying a
/// colour or a format flag — see that module's own doc and its
/// `a_styled_line_arrives_as_a_compound_and_still_reaches_spans` gate.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn sign_text_survives_the_depth_test_against_its_own_board() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");
    let world = sign_world();

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    // Two distances: adjacent (where even a nearly-zero depth separation
    // survives float precision) and a realistic reading distance (where it
    // does not). A gate that only measured the near one would be the
    // existing corpus with a board bolted on.
    for distance in [2.5_f32, 12.0] {
        let camera = camera_facing_sign(distance);
        let view_proj = camera.view_projection();
        let rect = expected_text_rect(view_proj);

        let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
            let mut state = RenderState::new(device, queue, format, W, H, Some(&atlas));
            suppress_first_person_arm(&mut state);
            state.set_fog(
                FogSettings::for_render_distance(SKY_COLOR, RD_CHUNKS),
                RD_CHUNKS,
            );
            upload_world(&world, models, &mut state, device, queue);
            if install {
                let spawn = sign_spawn();
                state.set_sign_source(move |_eye| vec![spawn.clone()]);
            }
            let frame = target.acquire().expect("headless acquire");
            let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
            (target.read_texels(device, queue), stats)
        };

        let (subject_px, subject_stats) = shoot(true);
        let (control_px, control_stats) = shoot(false);

        assert!(
            subject_stats.sign_text_vertices > 0,
            "at {distance} blocks the fixture produced no sign-text vertices at \
             all — the font or the text chain is broken, not the depth test"
        );
        assert_eq!(
            control_stats.sign_text_vertices, 0,
            "the control must install no sign source"
        );

        let (count, bbox) = changed(&subject_px, &control_px);
        println!(
            "distance {distance}: {count} px changed, bbox {bbox:?}, expected plane rect {rect:?}, \
             {} sign-text vertices",
            subject_stats.sign_text_vertices
        );
        assert!(
            count > 0,
            "at {distance} blocks the sign uploaded {} text vertices and changed \
             ZERO pixels against the identical frame with no sign source — the \
             ink is being drawn and then losing the depth test against the sign's \
             own board (expected plane rect {rect:?})",
            subject_stats.sign_text_vertices
        );
        let padded = rect.padded(4);
        let outside = subject_px
            .chunks_exact(4)
            .zip(control_px.chunks_exact(4))
            .enumerate()
            .filter(|(i, (s, c))| {
                differs(s, c) && !padded.contains((*i as u32 % W) as i32, (*i as u32 / W) as i32)
            })
            .count();
        assert_eq!(
            outside, 0,
            "at {distance} blocks {outside} of {count} changed pixels fall outside \
             the sign's own text plane {padded:?} — the sign source is moving \
             something other than sign text (changed bbox {bbox:?})"
        );
    }
}

// --- The `text_display` arm ------------------------------------------------

/// The hologram from the report: two lines of real text, vanilla's own
/// translucent-black background, `Vertical` billboard (yaw-only — vanilla's
/// own behaviour for that mode, and what makes the panel oblique when the
/// camera is pitched, which is precisely when the report says it breaks).
fn text_display_draw(background: i32) -> DisplayDraw {
    DisplayDraw {
        id: 1,
        type_path: TEXT_DISPLAY_TYPE_PATH,
        position: glam::Vec3::new(0.5, 68.0, 24.0),
        entity_yaw: 0.0,
        entity_pitch: 0.0,
        billboard: BillboardMode::Vertical,
        // A real hologram is scaled up — the default 0.025 blocks per font
        // pixel puts a glyph under three screen pixels at this range, which
        // is too small a sample to measure anything with. Scale changes the
        // ink's size and the panel's alike, so it does not paper over the
        // depth separation this gate is about: that separation scales with
        // it.
        transform: DisplayTransformation {
            scale: glam::Vec3::splat(4.0),
            ..DisplayTransformation::default()
        },
        text: Some(lodestone_model::text::Text::literal(
            "Main Reveille Hospital\nA Government-run hospital",
        )),
        text_line_width: 200,
        text_background_color: background,
        text_opacity: -1,
        text_style_flags: 0,
        block_state: None,
        item: None,
        item_display_context: 0,
    }
}

/// Eye well below the hologram and 24 blocks away, so the line of sight is
/// pitched **up** — the report's own worst case, and the geometry that makes
/// a yaw-only billboard's plane oblique to the view.
fn hologram_camera() -> Camera {
    let eye = glam::Vec3::new(0.5, 48.0, 0.0);
    let target = glam::Vec3::new(0.5, 68.0, 24.0);
    let dir = (target - eye).normalize();
    Camera {
        position: eye,
        yaw: (-dir.x).atan2(dir.z).to_degrees(),
        pitch: -dir.y.clamp(-1.0, 1.0).asin().to_degrees(),
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS, 0),
    }
}

/// Opaque glyph ink: the text is drawn white at full opacity, the panel is
/// translucent black over sky, and the sky is a mid blue — so "every channel
/// above 200" separates ink from both without needing a reference frame.
fn ink_pixels(pixels: &[u8]) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 200 && px[1] > 200 && px[2] > 200)
        .count()
}

/// **The gate `text_display_pixels.rs` structurally cannot run**: it asserts
/// only that *something* paints in the entity's rect, which the background
/// panel alone satisfies. This one measures the **glyph ink** against the
/// identical text with the panel switched off, so ink lost to the panel in
/// front of it is visible as a number rather than hidden behind the panel's
/// own pixels.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn text_display_glyphs_survive_their_own_background_panel() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = hologram_camera();

    let mut shoot = |background: i32| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        suppress_first_person_arm(&mut state);
        state.set_display_draws(vec![text_display_draw(background)]);
        let frame = target.acquire().expect("headless acquire");
        let _ = state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    // Vanilla's own default background, `0x40000000` — translucent black.
    let with_panel = shoot(0x4000_0000_u32 as i32);
    // `0` is vanilla's own "no panel at all" sentinel
    // (`TextDisplayRenderer.submitInner`'s `if (backgroundColor != 0)`), so
    // this reference draws the *same* glyphs with nothing in front of them.
    let no_panel = shoot(0);

    let reference_ink = ink_pixels(&no_panel);
    let subject_ink = ink_pixels(&with_panel);
    println!("glyph ink: {subject_ink} px with the panel, {reference_ink} px without");

    assert!(
        reference_ink > 200,
        "the panel-less reference drew only {reference_ink} ink px — the fixture \
         itself is broken, so the comparison below would be vacuous"
    );
    // **A magnitude assertion, with both hypotheses computed rather than a
    // direction.** The panel sits 0.00025 blocks behind the ink (vanilla's
    // `-0.01` in local glyph space through the `0.025` text scale), which is
    // a couple of `f32` ULP in this depth buffer at this range. With the
    // glyphs on an unbiased pipeline that measured **389** of **438** px —
    // the wrong hypothesis, watched failing — and with them on
    // `RenderPipelines.TEXT_POLYGON_OFFSET`'s bias it measures **438**, an
    // exact match. 99% therefore lands on one hypothesis and not the other,
    // rather than merely asserting "not much was lost".
    assert!(
        subject_ink * 100 >= reference_ink * 99,
        "adding the background panel ate the glyphs: {subject_ink} ink px with \
         the panel against {reference_ink} without. The panel is only 0.00025 \
         blocks behind the ink, so the two are within a few float ULP of each \
         other in the depth buffer and the ink loses wherever the two quads' \
         interpolated depth diverges — which is everywhere once the plane is \
         oblique to the view. Vanilla submits the glyphs through \
         `RenderPipelines.TEXT_POLYGON_OFFSET` for exactly this reason and \
         `gpu/display_text.rs` must keep doing the same"
    );
}
