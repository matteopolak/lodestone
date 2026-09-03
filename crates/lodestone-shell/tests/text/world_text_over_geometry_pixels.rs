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
//! The third arm is a level up again, and its blind spot was the **whole**
//! pixel corpus rather than one file: every gate in `tests/` uploads
//! `translucent_blocks: ModelMesh::default()`, so not one of them had ever
//! put a translucent block on screen. A `text_display`'s alpha-blended
//! background panel was writing depth and deleting the stained glass behind
//! it — the owner's own second symptom — with nothing red anywhere. See
//! [`a_text_display_panel_does_not_delete_the_translucent_geometry_behind_it`].
//!
//! The fourth arm is the same lesson a third time, and the axis it found was
//! being held fixed by this very file. A glyph's drop shadow is an offset
//! *within the text's own plane*, so how much depth that offset is worth is
//! set entirely by how oblique the plane is to the view — and every
//! `text_display` fixture here was a `Vertical` billboard under one ~40°
//! look, which is barely oblique. Measured against a build with no
//! shadow/ink separation, that fixture loses **1 ink pixel of 438**; at 80–85°
//! the same build loses **983–2,974 of ~15–18k**. See
//! [`a_glyph_wins_against_its_own_drop_shadow_at_every_distance_and_angle`].
//!
//! Fail-closed: no GPU adapter or no `client.jar` is a failure, never a
//! silent skip. `upload_world` also advances the section fade-in clock, and
//! the sign arm asserts the board's own coverage before claiming anything
//! about text surviving it — a section uploaded before any `update_animation`
//! renders as flat fog colour, which makes every *visibility* claim about it
//! vacuous while leaving its depth intact.
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
            // No skin: this fixture installs a body to suppress the first-person
            // arm, not to assert a sheet. The draw falls back to the model's own
            // texture, exactly as it did before this field existed.
            player_skin: None,
            feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
            body_yaw_deg: 0.0,
            anim: AnimInput::default(),
            scale: 1.0,
            swim_amount: 0.0,
            slim: false,
            equipment: Vec::new(),
            equipment_skin: Vec::new(),
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

    /// Inclusive on both bounds, matching [`Rect::contains`].
    fn area(self) -> usize {
        let w = (self.x1 - self.x0 + 1).max(0) as usize;
        let h = (self.y1 - self.y0 + 1).max(0) as usize;
        w * h
    }
}

/// How many pixels inside `rect` are **not** sky, taking the frame's own
/// top-left pixel as the sky reference rather than a hardcoded constant.
///
/// `SKY_COLOR` is not what a frame's background actually is — the real one is
/// a time-of-day and eye-height resolved fog colour under a sky disc — so a
/// detector written against the constant reports zero for a perfectly good
/// frame. `CLAUDE.md` records that as a measured hazard for pixel gates; the
/// frame's own corner is the cheap way round it, and this fixture's camera
/// keeps that corner on empty sky at every distance it uses.
fn board_coverage(pixels: &[u8], rect: Rect) -> usize {
    let sky = &pixels[..4];
    pixels
        .chunks_exact(4)
        .enumerate()
        .filter(|(i, px)| {
            rect.contains((*i as u32 % W) as i32, (*i as u32 / W) as i32) && differs(px, sky)
        })
        .count()
}

/// The fraction of the sign's own plane rect that must be board rather than
/// sky, as `NUM / DENOM`.
///
/// The rect is the text plane's projected quad, which sits **inside** the
/// board's face, so a correctly drawn board covers essentially all of it; the
/// floor is set well under that because the rect is derived from a projection
/// and carries a pixel or two of slack at its edges. What it has to separate
/// is "covered" from the fade-in's own answer, which is **zero**.
const BOARD_COVERAGE_NUM: usize = 1;
const BOARD_COVERAGE_DENOM: usize = 2;

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
    // **Advance the section fade-in past its own duration.** A freshly built
    // `RenderState` has `section_fade_tick == 0`, so a section uploaded before
    // any `update_animation` gets `build_time == 0.0` and the model shader
    // resolves `section_visibility(0.0, 0.0) == 0.0` — the section then
    // renders as flat fog colour for its whole `SECTION_FADE_DURATION_SECS`.
    // Depth is untouched by that mix (only `rgb` moves), so the depth claims
    // in this file survive it; what does not survive is any claim about the
    // board being *visible*, which is why [`board_coverage`] exists and why
    // this call is here rather than left to a caller to remember.
    state.update_animation(queue, FADED_IN_TICK);
}

/// A game tick comfortably past `lodestone_render::SECTION_FADE_DURATION_SECS`
/// (`0.75` s at 20 ticks/s, so 15 ticks) — 40 is over twice that, chosen with
/// margin rather than at the boundary because a value that only just clears it
/// would make this fixture sensitive to a change in the fade duration.
const FADED_IN_TICK: u64 = 40;

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
        // **The board has to be on screen before "the ink survives the board"
        // means anything.** A sibling gate measured zero board pixels at all
        // seven of its distances because its sections were mid-fade-in and
        // drew as flat fog colour; `upload_world` now advances that clock, and
        // this is the assertion that would fail loudly if it stopped doing so
        // — or if the platform were moved, or the camera aimed past it.
        let coverage = board_coverage(&control_px, rect);
        assert!(
            coverage * BOARD_COVERAGE_DENOM >= rect.area() * BOARD_COVERAGE_NUM,
            "at {distance} blocks only {coverage} of {} pixels in the sign's own \
             plane rect {rect:?} differ from sky in the *control* frame — the \
             board is not being drawn there, so this gate's whole claim (that \
             the ink beats the board it sits on) has nothing to beat",
            rect.area(),
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

/// One point of the panel-versus-ink sweep.
///
/// `z` is how far down `+Z` the hologram sits from the eye's own column and
/// `scale` its `Display.DATA_SCALE_ID`. The two move **together** on
/// purpose: the eye drop is a fixed fraction of `z` (see [`hologram_camera`])
/// so every case subtends the same angle and contributes the same ~438 px of
/// ink, which is what makes the ink counts comparable across the row.
///
/// # Why the sweep exists at all, and why one point was not enough
///
/// The separation between the panel and the ink is a fixed **world**
/// distance — `-0.01` in local glyph space through the `0.025` text scale, so
/// `0.00025 · scale` blocks. Under the **forward** `[0,1]` `Depth32Float` this
/// renderer used to project into, one ULP grew as the **square** of the viewing
/// distance (`≈6.1e-7 · d²` blocks; the entity-shadow work measured the same
/// curve at 2.44e-06 / 3.84e-05 / 1.55e-04 blocks at 2 / 8 / 16 blocks), so the
/// headroom was `410 · scale / d²` and holding the *angular* size fixed made it
/// fall as `1 / d`:
///
/// | `z` | `scale` | eye distance | geometric headroom, forward `[0,1]` |
/// |---|---|---|---|
/// | 6 | 1 | 7.8 | 6.7 ULP |
/// | 12 | 2 | 15.6 | 3.4 ULP |
/// | 24 | 4 | 31.2 | 1.7 ULP |
/// | 48 | 8 | 62.5 | 0.84 ULP |
/// | 72 | 12 | 93.7 | 0.56 ULP |
///
/// `Camera::projection_matrix` is reversed-Z now, under which the same
/// separation is worth two to three orders of magnitude more at every row
/// (`docs/coplanar-overlay-depth.md`). The sweep is kept in full anyway: it was
/// built because *one* point could not distinguish a mechanism that scales with
/// the entity from one that scales with the camera, and that reasoning is
/// independent of how much precision there is. The glyph pipeline's polygon
/// offset is also kept, for the reason `gpu/display_text.rs` gives — vanilla's
/// own geometric shadow offset is one-sided and a `text_display` is visible from
/// both sides, which is geometry rather than precision.
///
/// The single point this gate used to have was the `z = 24, scale = 4` row.
/// Its own comment claimed scaling the hologram up "does not paper over the
/// depth separation this gate is about: that separation scales with it" —
/// **that reasoning is wrong**, and it is the reason one point was not
/// enough. The separation does scale with the entity, but the ULP it is
/// measured against scales with the *camera distance*, which the entity scale
/// does not touch. A default-scale (`1.0`) hologram — what a `/summon` with
/// no `transformation` tag gets, and what most server holograms are — has
/// four times less headroom at the same screen size than the one row that
/// was being tested.
#[derive(Debug, Clone, Copy)]
struct HologramCase {
    z: f32,
    scale: f32,
}

const HOLOGRAM_SWEEP: [HologramCase; 5] = [
    HologramCase { z: 6.0, scale: 1.0 },
    HologramCase { z: 12.0, scale: 2.0 },
    HologramCase { z: 24.0, scale: 4.0 },
    HologramCase { z: 48.0, scale: 8.0 },
    HologramCase { z: 72.0, scale: 12.0 },
];

/// The hologram from the report: two lines of real text, vanilla's own
/// translucent-black background, `Vertical` billboard (yaw-only — vanilla's
/// own behaviour for that mode, and what makes the panel oblique when the
/// camera is pitched, which is precisely when the report says it breaks).
fn text_display_draw(case: HologramCase, background: i32) -> DisplayDraw {
    DisplayDraw {
        id: 1,
        type_path: TEXT_DISPLAY_TYPE_PATH,
        position: glam::Vec3::new(0.5, 68.0, case.z),
        entity_yaw: 0.0,
        entity_pitch: 0.0,
        billboard: BillboardMode::Vertical,
        transform: DisplayTransformation {
            scale: glam::Vec3::splat(case.scale),
            ..DisplayTransformation::default()
        },
        text: Some(lodestone_model::text::ResolvedText::literal(
            "Main Reveille Hospital\nA Government-run hospital",
        )),
        text_line_width: 200,
        text_background_color: background,
        text_opacity: -1,
        text_style_flags: 0,
        block_state: None,
        item: None,
        item_display_context: 0,
        brightness_override: None,
    }
}

/// The eye drop as a fraction of the hologram's `z`, chosen so the original
/// single-point fixture (`z = 24`, eye 20 blocks below) is reproduced exactly
/// — `20 / 24`, a ~40° upward look. Holding it as a *ratio* is what keeps
/// every row of [`HOLOGRAM_SWEEP`] at the same viewing angle, so the sweep
/// varies distance and scale and nothing else.
const EYE_DROP_RATIO: f32 = 20.0 / 24.0;

/// Eye below the hologram and `z` blocks back, so the line of sight is
/// pitched **up** — the report's own worst case, and the geometry that makes
/// a yaw-only billboard's plane oblique to the view.
fn hologram_camera(case: HologramCase) -> Camera {
    let eye = glam::Vec3::new(0.5, 68.0 - EYE_DROP_RATIO * case.z, 0.0);
    let target = glam::Vec3::new(0.5, 68.0, case.z);
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
/// own pixels — and it does so across [`HOLOGRAM_SWEEP`], because the one
/// point it used to measure sat at four times the depth headroom a
/// default-scale hologram has.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn text_display_glyphs_survive_their_own_background_panel() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let mut worst: Option<(HologramCase, usize, usize)> = None;
    for case in HOLOGRAM_SWEEP {
        let camera = hologram_camera(case);
        let mut shoot = |background: i32| -> Vec<u8> {
            let mut state = RenderState::new(device, queue, format, W, H, None);
            suppress_first_person_arm(&mut state);
            state.set_display_draws(vec![text_display_draw(case, background)]);
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
        println!(
            "z={:>4} scale={:>4}: glyph ink {subject_ink} px with the panel, \
             {reference_ink} px without",
            case.z, case.scale
        );
        assert!(
            reference_ink > 200,
            "at z={} scale={} the panel-less reference drew only {reference_ink} \
             ink px — the fixture itself is broken there, so the comparison \
             below would be vacuous",
            case.z, case.scale
        );
        if worst.is_none_or(|(_, s, r)| subject_ink * r <= s * reference_ink) {
            worst = Some((case, subject_ink, reference_ink));
        }
    }

    // **A magnitude assertion, with both hypotheses computed rather than a
    // direction.** The panel sits `0.00025 · scale` blocks behind the ink
    // (vanilla's `-0.01` in local glyph space through the `0.025` text
    // scale), which is between 6.7 and 0.56 `f32` ULP across the sweep. With
    // the glyphs on an unbiased pipeline the middle row measured **389** of
    // **438** px — the wrong hypothesis, watched failing — and with them on
    // `RenderPipelines.TEXT_POLYGON_OFFSET`'s bias it measures **438**, an
    // exact match. 99% therefore lands on one hypothesis and not the other,
    // rather than merely asserting "not much was lost". The verdict is taken
    // on the **worst** row so a single bad distance cannot be averaged away.
    let (case, subject_ink, reference_ink) = worst.expect("the sweep is non-empty");
    assert!(
        subject_ink * 100 >= reference_ink * 99,
        "adding the background panel ate the glyphs at z={} scale={}: \
         {subject_ink} ink px with the panel against {reference_ink} without. \
         The panel is only 0.00025·scale blocks behind the ink, so the two are \
         close together in world space, and the ink loses wherever the two \
         quads' interpolated depth diverges. Vanilla submits the glyphs through \
         `RenderPipelines.TEXT_POLYGON_OFFSET` and so does this pass. Note the \
         numbers in this file's header were taken under a forward [0,1] \
         projection; this renderer is reversed-Z now, so a failure here is more \
         likely to be the pipeline than the precision",
        case.z, case.scale
    );
}

// --- The drop-shadow arm ---------------------------------------------------

/// `Display.TextDisplay.FLAG_SHADOW`. `gpu/display_text.rs`'s own copy is
/// private to that module and exporting it purely for a test would be worse
/// than restating it; the value is pinned by this arm's own visibility
/// control, which requires the flagged render to differ from the unflagged
/// one — a wrong bit here draws no shadow and fails that assertion by name.
const FLAG_SHADOW: u8 = 1;

/// One point of the shadow-versus-ink sweep: a `Fixed`-billboard hologram at
/// `z` blocks, `Display.DATA_SCALE_ID` `scale`, facing `yaw`.
///
/// `yaw = 180` is face-on **from the front**; `|yaw - 180|` is the obliquity,
/// and a `yaw` outside `(95, 265)` is the display's **back**, where every
/// depth ordering it carries inverts. Both are real viewing positions — a
/// server hologram is walked around — and the sweep carries both.
#[derive(Debug, Clone, Copy)]
struct ShadowCase {
    z: f32,
    scale: f32,
    yaw: f32,
}

impl ShadowCase {
    /// Front or back, by the same `(95, 265)` window the doc above states.
    /// Printed on every row so a failure says which side it was on, since the
    /// two have different causes and different magnitudes.
    fn side(self) -> &'static str {
        if (95.0..265.0).contains(&self.yaw) { "front" } else { "back " }
    }
}

/// Obliquity **and** distance, because the two axes fail for different
/// reasons and neither alone is enough.
///
/// Distance, for the reason [`HOLOGRAM_SWEEP`]'s own doc gives at length: what
/// one ULP of depth is worth in world units changes with the viewing distance
/// under either convention, so a mechanism that holds at one range says nothing
/// about another. `scale` moves with `z` so every row subtends the same angle and
/// contributes comparable ink.
///
/// Obliquity, because **[`HOLOGRAM_SWEEP`] structurally cannot see this
/// defect**. That sweep is a `Vertical` billboard under a ~40° upward look,
/// and measured against a build with no shadow/ink separation at all it lost
/// **1 ink pixel of 438** — a real signal and three orders of magnitude too
/// small to gate on. A shadow is an offset *within the text's own plane*, so
/// how much depth that offset is worth is set by how oblique the plane is to
/// the view, and 40° is barely oblique. At 80–85° the same build loses
/// 983–2,974 px of ~15–18k, which is what the rows below are for. The axis
/// the existing sweep holds fixed is the axis this defect lives on, which is
/// the shared-blind-spot species `CLAUDE.md` describes for a whole fixture
/// corpus.
const SHADOW_SWEEP: [ShadowCase; 6] = [
    // Front, face-on: the plane carries constant depth, so this row must be
    // clean under *any* hypothesis. It is the row that would catch a fix that
    // broke the ordinary case while chasing the oblique one.
    ShadowCase { z: 3.0, scale: 2.0, yaw: 180.0 },
    // Front, 80° oblique, near and far at the same angular size.
    ShadowCase { z: 3.0, scale: 2.0, yaw: 100.0 },
    ShadowCase { z: 12.0, scale: 8.0, yaw: 100.0 },
    // Back, 80° and 85° oblique. 85° is the worst row measured anywhere: it
    // is where a constant-only depth bias still lost 1,204 px, which is why
    // `GLYPH_POLYGON_OFFSET` doubles the slope term too.
    ShadowCase { z: 3.0, scale: 2.0, yaw: 80.0 },
    ShadowCase { z: 3.0, scale: 2.0, yaw: 85.0 },
    ShadowCase { z: 24.0, scale: 16.0, yaw: 85.0 },
];

/// The hologram from the report, at one row of [`SHADOW_SWEEP`], with
/// vanilla's own translucent-black panel so the shadow is contesting depth
/// with the panel behind it at the same time.
fn shadow_case_draw(case: ShadowCase, style_flags: u8) -> DisplayDraw {
    DisplayDraw {
        id: 1,
        type_path: TEXT_DISPLAY_TYPE_PATH,
        position: glam::Vec3::new(0.5, 68.0, case.z),
        entity_yaw: case.yaw,
        entity_pitch: 0.0,
        // `Fixed`, not `Vertical`: a yaw-only billboard turns to face the
        // camera, so the entity's own yaw cannot be used to set the obliquity
        // this sweep varies. `Fixed` is also what a `/summon` with an explicit
        // `Rotation` gets, and what a hologram nailed to a wall is.
        billboard: BillboardMode::Fixed,
        transform: DisplayTransformation {
            scale: glam::Vec3::splat(case.scale),
            ..DisplayTransformation::default()
        },
        text: Some(lodestone_model::text::ResolvedText::literal(
            "Main Reveille Hospital\nA Government-run hospital",
        )),
        text_line_width: 200,
        text_background_color: 0x4000_0000_u32 as i32,
        text_opacity: -1,
        text_style_flags: style_flags,
        block_state: None,
        item: None,
        item_display_context: 0,
        brightness_override: None,
    }
}

/// Level, on the hologram's own axis: this sweep varies obliquity through the
/// **entity's** yaw, so the camera holds still and contributes none of it.
fn shadow_camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 68.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS, 0),
    }
}

/// The fraction of the reference's own ink the drop shadow must repaint for
/// the ink assertion to be about anything, as `NUM / DENOM`.
///
/// Measured across [`SHADOW_SWEEP`]: the shadow repaints between **83%** and
/// **96%** of the ink area (12,221–16,389 px against 14,707–17,984 of ink),
/// because at these on-screen sizes one font pixel is several screen pixels
/// and the shadow's fringe is comparable to the glyph itself. A quarter is the
/// floor — far below every measured row, and far above the "the flag did
/// nothing" value of zero.
const SHADOW_VISIBLE_NUM: usize = 1;
const SHADOW_VISIBLE_DENOM: usize = 4;

/// **The owner's second report on this pass**: *"the shadow text is
/// z-fighting with the real text in places where both are on the same 'pixel'
/// for holograms"*.
///
/// # What is measured
///
/// A glyph's drop shadow is the same rect offset by one font pixel on both
/// axes, so it overlaps most of the glyph's own area. Every shadow in a block
/// is submitted before every glyph, so under correct rendering the ink wins
/// everywhere and its pixel count is untouched by the flag. Any ink the flag
/// costs is ink that lost the depth test to its own shadow.
///
/// Ink is drawn white and the shadow is that white at `SHADOW_BRIGHTNESS`
/// (`0.25`), so [`ink_pixels`] counts the glyph and never the shadow, and
/// stolen fragments show up as ink that went missing rather than as a colour
/// this gate has to characterise.
///
/// # The controls
///
/// Two, checked at **every** row, because the ink assertion is satisfied
/// trivially by a shadow that was never drawn:
///
/// - the unflagged reference must draw real ink, and
/// - the flagged render must repaint at least [`SHADOW_VISIBLE_NUM`] /
///   [`SHADOW_VISIBLE_DENOM`] of it. With the ink assertion holding, every
///   changed pixel is one the shadow *added* rather than one it stole, so the
///   two together say "the shadow is there and it lost".
///
/// Both hypotheses were computed rather than a direction asserted, and the
/// wrong one was **watched failing** through this exact fixture. Collapsing
/// the shadow back onto the ink's pipeline — the state the drop shadow
/// shipped in — fails **five of the six rows**:
///
/// | row | lost ink px |
/// |---|---|
/// | `yaw 180` front, face-on | **0** |
/// | `yaw 100` front, `z = 3` | 1,232 of 15,592 |
/// | `yaw 100` front, `z = 12` | 983 of 15,594 |
/// | `yaw 80` back, `z = 3` | 1,142 of 15,645 |
/// | `yaw 85` back, `z = 3` | 2,974 of 17,984 |
/// | `yaw 85` back, `z = 24` | 2,905 of 17,984 |
///
/// The face-on row staying at 0 under the neuter is the sweep working as
/// designed rather than a gap: a plane perpendicular to the view carries
/// constant depth, so there is nothing there for an in-plane offset to
/// change. It is in the sweep to catch a *fix* that broke the ordinary case.
///
/// See `gpu/display_text.rs`'s module doc for the full four-hypothesis table,
/// including the one where vanilla's own geometric offset makes things
/// *worse* than having none.
///
/// Failures are collected rather than asserted inside the loop, so a run
/// reports every bad row instead of proving one and leaving the rest as
/// argument.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_glyph_wins_against_its_own_drop_shadow_at_every_distance_and_angle() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = shadow_camera();

    let mut lost: Vec<String> = Vec::new();
    let mut invisible: Vec<String> = Vec::new();

    for case in SHADOW_SWEEP {
        let mut shoot = |style_flags: u8| -> Vec<u8> {
            let mut state = RenderState::new(device, queue, format, W, H, None);
            suppress_first_person_arm(&mut state);
            state.set_display_draws(vec![shadow_case_draw(case, style_flags)]);
            let frame = target.acquire().expect("headless acquire");
            let _ = state.render(device, queue, frame.view(), &camera, None, &[]);
            target.read_texels(device, queue)
        };

        let with_shadow = shoot(FLAG_SHADOW);
        let no_shadow = shoot(0);

        let reference_ink = ink_pixels(&no_shadow);
        let subject_ink = ink_pixels(&with_shadow);
        let (shadow_px, shadow_rect) = changed(&with_shadow, &no_shadow);
        println!(
            "z={:>5} scale={:>5} yaw={:>5} {}: glyph ink {subject_ink} px with \
             its shadow, {reference_ink} px without; the shadow repainted \
             {shadow_px} px (bbox {shadow_rect:?})",
            case.z,
            case.scale,
            case.yaw,
            case.side(),
        );

        assert!(
            reference_ink > 200,
            "at z={} scale={} yaw={} the shadow-less reference drew only \
             {reference_ink} ink px — the fixture itself is broken there, so \
             every comparison below would be vacuous",
            case.z, case.scale, case.yaw
        );
        if shadow_px * SHADOW_VISIBLE_DENOM < reference_ink * SHADOW_VISIBLE_NUM {
            invisible.push(format!(
                "z={} scale={} yaw={} {}: the shadow repainted only {shadow_px} \
                 px against {reference_ink} px of ink",
                case.z,
                case.scale,
                case.yaw,
                case.side(),
            ));
        }
        if subject_ink < reference_ink {
            lost.push(format!(
                "z={} scale={} yaw={} {}: {subject_ink} ink px with the shadow \
                 against {reference_ink} without, {} lost (bbox of the \
                 difference {shadow_rect:?})",
                case.z,
                case.scale,
                case.yaw,
                case.side(),
                reference_ink - subject_ink,
            ));
        }
    }

    assert!(
        invisible.is_empty(),
        "the drop shadow barely painted at all, so the ink assertion below \
         cannot distinguish 'the glyph won' from 'there was nothing to win \
         against': {}",
        invisible.join("; ")
    );
    assert!(
        lost.is_empty(),
        "a glyph's own drop shadow ate the glyph: {}. The shadow is the same \
         rect one font pixel away in the text's own plane, so for a billboard \
         oblique to the view the two quads interpolate different window z at a \
         shared pixel and float rounding decides the winner per fragment — \
         which is why the symptom is speckle rather than the text vanishing. \
         `gpu/display_text.rs` separates them with a polygon offset, measured \
         from the camera and denominated in ULPs of the primitive's own depth \
         plus a multiple of its own depth gradient, rather than with the \
         world-space local-glyph offset vanilla can afford under reversed-Z \
         and which inverts the moment the hologram is viewed from behind",
        lost.join("; ")
    );
}

// --- The translucent-geometry arm ------------------------------------------

/// The glass wall's plane, and the hologram in front of it. All three sit on
/// the camera's own `+Z` axis so the panel is unambiguously between the eye
/// and the glass.
const WALL_Z: i32 = 12;
const HOLO_Z: f32 = 2.0;
const EYE_Z: f32 = -8.0;
/// Well above the (empty) ground so nothing but sky is behind the wall.
const HOLO_Y: f32 = 68.0;

/// A world holding **one wall of red stained glass** and nothing else — no
/// floor, no platform, so every non-glass pixel is sky and the glass's own
/// tint is the largest signal available.
///
/// Red rather than blue deliberately: the sky is a mid blue, so a red tint
/// moves two channels hard in opposite directions and cannot be confused
/// with the panel's own darkening.
fn glass_wall_world() -> World {
    let air = state_id("minecraft:air");
    let glass = state_id("minecraft:red_stained_glass");

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
    let filled = world.fill_region([-16, 56, WALL_Z], [15, 80, WALL_Z], glass);
    assert!(
        filled > 0,
        "fixture: the glass wall must actually write blocks"
    );
    world
}

/// [`upload_world`]'s sibling that keeps the **translucent** layer instead of
/// discarding it.
///
/// Every other pixel gate in this crate passes
/// `translucent_blocks: ModelMesh::default()`, which is the shared-blind-spot
/// species `CLAUDE.md` describes at the level of a whole corpus: no gate here
/// had ever put a translucent block on screen, so nothing could see a pass
/// that deletes one. This routes through
/// [`lodestone::mesher::mesh_snapshot_models_layers`] — the same split
/// `mesh_one` uses in production — rather than re-deriving one, so the gate
/// cannot pass against geometry production would never build.
fn upload_world_with_translucent(
    world: &World,
    models: &lodestone_render::BlockModels,
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> usize {
    let mut translucent_quads = 0usize;
    for cx in -1..=1 {
        for cz in -1..=1 {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let (opaque, translucent_blocks) = lodestone::mesher::mesh_snapshot_models_layers(
                    &snap,
                    models,
                    false,
                    lodestone_render::biome_tint::BLEND_RADIUS,
                );
                translucent_quads += translucent_blocks.quad_count();
                let visibility = snapshot_visibility(&snap, models);
                let geometry = SectionGeometry::Model {
                    opaque,
                    water: ModelMesh::default(),
                    translucent_blocks,
                    visibility,
                };
                state.upload_section(device, queue, key, &geometry);
            }
        }
    }
    translucent_quads
}

/// The hologram that sits in front of the wall: one short line, vanilla's own
/// translucent-black panel, `Center` billboard so it faces the eye squarely
/// and the panel's footprint is a clean rect.
fn wall_hologram(background: i32) -> DisplayDraw {
    DisplayDraw {
        id: 7,
        type_path: TEXT_DISPLAY_TYPE_PATH,
        position: glam::Vec3::new(0.5, HOLO_Y, HOLO_Z),
        entity_yaw: 0.0,
        entity_pitch: 0.0,
        billboard: BillboardMode::Center,
        transform: DisplayTransformation {
            scale: glam::Vec3::splat(3.0),
            ..DisplayTransformation::default()
        },
        text: Some(lodestone_model::text::ResolvedText::literal("HOLOGRAM\nOVER GLASS")),
        text_line_width: 200,
        text_background_color: background,
        text_opacity: -1,
        text_style_flags: 0,
        block_state: None,
        item: None,
        item_display_context: 0,
        brightness_override: None,
    }
}

/// Eye on the wall's own axis, looking straight down `+Z` through the
/// hologram at the glass behind it.
fn wall_camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, HOLO_Y, EYE_Z),
        // `Camera::basis`' forward is
        // `(-sin(yaw)cos(pitch), -sin(pitch), cos(yaw)cos(pitch))`, so yaw 0,
        // pitch 0 looks along `+Z`.
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS, 0),
    }
}

/// Opaque glyph ink — the same "every channel above 200" separator
/// [`ink_pixels`] uses, per pixel.
fn is_ink(px: &[u8]) -> bool {
    px[0] > 200 && px[1] > 200 && px[2] > 200
}

/// **The gate the whole pixel corpus structurally could not run**: a
/// `text_display`'s background panel must *blend over* the translucent
/// geometry behind it, not delete it.
///
/// The owner's second symptom — *"some blocks like glass don't render at all
/// when they're behind the billboard panel"* — is the direct consequence of
/// `RenderPipelines.TEXT_BACKGROUND`'s depth-write flag being ported
/// faithfully into a renderer that has neither of the two things vanilla
/// leans on (reversed-Z, and a separate translucent render target whose
/// depth is copied before the translucent features draw). Translucent
/// terrain draws after `gpu/display_text.rs` in `gpu/frame.rs`, so a
/// depth-writing translucent panel rejects it.
///
/// Four renders, so the claim is about the *glass under the panel* and not
/// about the panel:
///
/// | render | glass | hologram |
/// |---|---|---|
/// | `sky` | no | no |
/// | `holo` | no | yes |
/// | `glass` | yes | no |
/// | `both` | yes | yes |
///
/// `holo` against `sky` **derives** the panel's own footprint rather than
/// restating a rect, and the ink pixels are excluded from it so the
/// assertion is about the translucent panel alone. Inside that footprint,
/// `both` must differ from `holo` — the glass showing through — and the
/// control that the detector can see anything at all is that `glass` differs
/// from `sky` on those very same pixels.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_text_display_panel_does_not_delete_the_translucent_geometry_behind_it() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");
    let world = glass_wall_world();

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = wall_camera();

    let mut shoot = |glass: bool, hologram: bool| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, Some(&atlas));
        suppress_first_person_arm(&mut state);
        state.set_fog(
            FogSettings::for_render_distance(SKY_COLOR, RD_CHUNKS),
            RD_CHUNKS,
        );
        if glass {
            let quads = upload_world_with_translucent(&world, models, &mut state, device, queue);
            assert!(
                quads > 0,
                "fixture: the glass wall produced no translucent quads — the \
                 mesher classified red_stained_glass as something other than \
                 RenderLayer::Translucent, so this gate would be vacuous"
            );
        }
        if hologram {
            // `0x40000000` is vanilla's own accessor default for
            // `Display.TextDisplay`'s background: translucent black.
            state.set_display_draws(vec![wall_hologram(0x4000_0000_u32 as i32)]);
        }
        // **Not optional** — `distant_flat_terrain_holes.rs::render_frame`'s
        // doc carries the whole measurement. A freshly uploaded section is
        // stamped with `build_time = section_fade_tick / 20`, the shader
        // paints `mix(fog_colour, lit_colour, section_visibility(now,
        // build_time))`, and a harness that renders immediately after
        // uploading has `now == build_time == 0`, so **every section is pure
        // fog colour** while `RenderStats` reports it drawn. The first run of
        // this gate hit exactly that: its control measured the glass wall
        // changing **0** of 5,949 panel pixels against a bare sky. 200 ticks
        // is 10 s against a 0.75 s fade — clearing it, not probing its edge.
        state.update_animation(queue, 200);
        let frame = target.acquire().expect("headless acquire");
        let _ = state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let sky = shoot(false, false);
    let holo = shoot(false, true);
    let glass = shoot(true, false);
    let both = shoot(true, true);

    // The panel's own footprint, derived rather than restated: every pixel
    // the hologram changed against a bare sky, minus the opaque ink.
    let panel: Vec<usize> = holo
        .chunks_exact(4)
        .zip(sky.chunks_exact(4))
        .enumerate()
        .filter(|(_, (h, s))| differs(h, s) && !is_ink(h))
        .map(|(i, _)| i)
        .collect();
    assert!(
        panel.len() > 500,
        "the hologram's panel covered only {} px against a bare sky — the \
         fixture is too small to measure anything with",
        panel.len()
    );

    // **The control, run first**: the population of panel-footprint pixels on
    // which the glass is visible *at all* against a bare sky. Everything below
    // is asserted on exactly this set, because a pixel the detector cannot see
    // the glass on with no hologram present says nothing about whether the
    // hologram deleted it.
    let visible: Vec<usize> = panel
        .iter()
        .copied()
        .filter(|&i| differs(&glass[i * 4..i * 4 + 4], &sky[i * 4..i * 4 + 4]))
        .collect();
    let (whole_frame, wall_bbox) = changed(&glass, &sky);
    println!(
        "panel footprint {} px, glass visible on {} of them; \
         glass changes {whole_frame} px of the whole frame, bbox {wall_bbox:?}",
        panel.len(),
        visible.len()
    );
    assert!(
        visible.len() > 500,
        "control: the glass wall is visible on only {} of {} panel-footprint \
         pixels against a bare sky (and on {whole_frame} px of the whole frame, \
         bbox {wall_bbox:?}). The wall is not actually behind the panel, or it \
         is not being drawn at all, so everything below would be vacuous",
        visible.len(),
        panel.len()
    );

    // **Both hypotheses, computed rather than a direction.** If the panel
    // blends over the glass, the glass's own tint survives into `both` on
    // essentially all of `visible` — attenuated by the panel's 0.75 pass-through
    // but nowhere near erased. If the panel *deletes* the glass, `both` is
    // byte-identical to `holo` and the count is exactly **0**: the two
    // hypotheses are separated by the whole range, not by a tuned margin, so
    // the halfway predicate cannot land on the wrong one. Measured before the
    // fix: 0. After: see the printed line.
    let survived = visible
        .iter()
        .filter(|&&i| differs(&both[i * 4..i * 4 + 4], &holo[i * 4..i * 4 + 4]))
        .count();
    println!(
        "glass still visible under the panel on {survived} of {} pixels",
        visible.len()
    );
    assert!(
        survived * 2 > visible.len(),
        "the background panel deleted the translucent geometry behind it: the \
         glass is visible on {} panel-footprint pixels with no hologram present \
         and on only {survived} of them with one. The wrong hypothesis predicts \
         exactly 0 here. The panel is alpha-blended and writes depth, and \
         translucent terrain draws after `gpu/display_text.rs` in \
         `gpu/frame.rs`, so the write rejects it",
        visible.len()
    );
}
