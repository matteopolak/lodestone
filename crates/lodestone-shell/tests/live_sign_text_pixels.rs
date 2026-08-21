//! The join `sign_text_pixels.rs` and `live_sign_text_wire.rs` each leave
//! open: a **real vanilla 26.2 server's** sign bytes, gathered by the real
//! production function, rendered through the real
//! [`RenderState::render`](lodestone::gpu::RenderState::render), asserted as
//! pixels.
//!
//! # Why this gate exists
//!
//! `CLAUDE.md`'s rule, paid for three times in one day: *a pixel gate proves
//! the draw, and proves nothing past the edge of its own fixture*. Every sign
//! pixel gate in this tree builds its own [`SignSpawn`] in-process, so the
//! whole corpus is blind to anything the **supply** does — and sign text has
//! now been reported blank against a real server twice after two separate,
//! individually-correct fixes. `live_sign_text_wire.rs` closed the supply half
//! (wire → `World` → `sign_spawns` → spans) and stops at spans;
//! `sign_text_pixels.rs` closed the draw half and starts at a hand-built
//! spawn. Nothing crossed the seam between them, which is precisely the shape
//! of defect that survives a green corpus.
//!
//! This gate installs the **unmodified** `SignSpawn` values
//! `lodestone::block_entities::sign_spawns` returned for a live server's own
//! signs. The only thing it does to that list is filter it to the one probe
//! sign the camera is aimed at; no field is rewritten.
//!
//! # The control
//!
//! Not "no source installed" — that would only prove `RenderState` has no
//! default sign source, which `sign_text_pixels.rs` already asserts. The
//! control here renders the **same live spawn with its four front lines
//! emptied**, so the only difference between the two frames is the text the
//! server actually sent. Anything that survives that subtraction came from
//! the live sign's own words.
//!
//! ```text
//! scripts/live-oracles/creative.sh
//! cargo test -p lodestone-shell --features live --test live_sign_text_pixels -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::gpu::{RenderState, SKY_COLOR, ThirdPersonBodyState};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility,
};
use lodestone::net::{NetClient, NetUpdate};
use lodestone::resources::BlockResources;
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, SignKind, SignSpawn,
    entity_anim::AnimInput, fog::FogSettings,
};
use lodestone_testsupport::unique_username;
use lodestone_world::{ChunkPos, World};

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25570;
const PROTOCOL: i32 = 776;

const W: u32 = 320;
const H: u32 = 240;

/// The mixed-style probe sign `live_sign_text_wire.rs` reads — one red line,
/// one bold line, one collapsed-string line, one empty. Placed over RCON into
/// the creative oracle's flat world; see that gate for why a *mixed* sign is
/// the discriminating fixture rather than an all-plain one.
const SIGN: [i32; 3] = [3, -59, 3];

/// Vanilla's `SignBlockEntity.MAX_TEXT_LINE_WIDTH`, used only to size a
/// generous expected rect.
const MAX_TEXT_LINE_WIDTH: f32 = 90.0;

/// Manhattan RGB distance above which two pixels count as different. Matches
/// the other block-entity pixel gates.
const DIFFERENT: i32 = 24;

/// Render distance the terrain arm configures. Must match the camera's own
/// `far_for_render_distance`, or the view-distance cull and the far clip
/// disagree about which of the uploaded sections exist.
const RD_CHUNKS: u32 = 8;

/// Far past the chunk fade-in: a section uploaded this frame is mid-fade and
/// draws nothing until the animation clock passes it.
const FADE_COMPLETE_TICK: u64 = 200;

fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
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
}

/// Bounding box and count of the pixels that differ between two frames.
/// Returns a box so a failure localises rather than aggregating — a gate that
/// can only say "something moved" cannot say *where*.
fn changed(a: &[u8], b: &[u8]) -> Option<(Rect, usize)> {
    let mut min = (u32::MAX, u32::MAX);
    let mut max = (0u32, 0u32);
    let mut count = 0usize;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let d: i32 = (0..3)
                .map(|c| (i32::from(a[i + c]) - i32::from(b[i + c])).abs())
                .sum();
            if d > DIFFERENT {
                count += 1;
                min = (min.0.min(x), min.1.min(y));
                max = (max.0.max(x), max.1.max(y));
            }
        }
    }
    (count > 0).then_some((
        Rect {
            x0: min.0,
            y0: min.1,
            x1: max.0,
            y1: max.1,
        },
        count,
    ))
}

fn project(view_proj: glam::Mat4, world: glam::Vec3) -> (f32, f32) {
    let clip = view_proj * world.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        (ndc.x * 0.5 + 0.5) * W as f32,
        (1.0 - (ndc.y * 0.5 + 0.5)) * H as f32,
    )
}

/// The screen rect the sign's own text plane projects to, derived through the
/// **same** `sign_text_transform` and the same `view_projection` the draw
/// uses — never a remembered literal.
fn expected_text_rect(spawn: &SignSpawn, view_proj: glam::Mat4) -> Rect {
    let matrix =
        lodestone_render::sign_text_transform(spawn.pos, spawn.kind, spawn.orientation, true);
    let half_w = match spawn.kind {
        SignKind::Plain => MAX_TEXT_LINE_WIDTH / 2.0,
        SignKind::Hanging => spawn.kind.max_text_line_width() / 2.0,
    };
    let half_h = 2.0 * spawn.kind.text_line_height();
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    for c in [
        glam::Vec3::new(-half_w, -half_h, 0.0),
        glam::Vec3::new(half_w, -half_h, 0.0),
        glam::Vec3::new(-half_w, half_h, 0.0),
        glam::Vec3::new(half_w, half_h, 0.0),
    ] {
        let (sx, sy) = project(view_proj, matrix.transform_point3(c));
        min = (min.0.min(sx), min.1.min(sy));
        max = (max.0.max(sx), max.1.max(sy));
    }
    Rect {
        x0: min.0.max(0.0).floor() as u32,
        y0: min.1.max(0.0).floor() as u32,
        x1: max.0.min((W - 1) as f32).ceil() as u32,
        y1: max.1.min((H - 1) as f32).ceil() as u32,
    }
}

/// Looking at the **front** face of a `rotation=0` standing sign, which is a
/// fact worth deriving rather than guessing: `RotationSegment` 0 is angle 0,
/// so `StandingSignRenderer.textTransformation` applies no Y rotation and
/// `TEXT_OFFSET`'s `+z` puts the *front* text plane on the `+Z` (south) side
/// of the block. `Camera`'s yaw 0 faces `+Z`, so a camera north of the sign
/// looking south sees its **back**.
///
/// That matters more than it sounds: from the wrong side the front text is
/// still drawn, and against an empty world it is still *visible* — through
/// the space the board would occupy. A sky-only gate therefore passes from
/// either side and cannot tell them apart, which is exactly how a fixture
/// ends up measuring text seen through its own board.
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(
            SIGN[0] as f32 + 0.5,
            SIGN[1] as f32 + 0.9,
            SIGN[2] as f32 + 3.0,
        ),
        yaw: 180.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

/// Joins the oracle and returns the live client, still connected — the caller
/// decides whether it wants only the spawn list or the whole world behind it.
fn joined() -> NetClient {
    let net = NetClient::connect_as(HOST.into(), PORT, PROTOCOL, None, unique_username());
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut logged_in = false;
    let mut last_err: Option<String> = None;
    while Instant::now() < deadline {
        for u in net.poll() {
            match u {
                NetUpdate::LoggedIn { .. } => logged_in = true,
                NetUpdate::Error(e) => last_err = Some(e),
                NetUpdate::Disconnected(r) => {
                    last_err = Some(format!("disconnected: {}", r.to_plain_string()));
                }
                _ => {}
            }
        }
        if logged_in && net.loaded_chunks().len() >= 4 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(logged_in, "never logged in: {last_err:?}");
    net
}

/// The spawn list the production gather built from the server's own chunk
/// payload — no field of it is rewritten anywhere in this file.
fn live_spawns(net: &NetClient) -> Vec<SignSpawn> {
    lodestone::block_entities::sign_spawns(&net.shared_handle(), camera().position)
}

/// The one probe sign this file's camera is aimed at, plus a precondition on
/// the supply half: if the server's bytes never became spans, a pixel
/// assertion below would fail for a reason that has nothing to do with the
/// renderer, and this is what tells the two apart.
fn subject_spawn(spawns: &[SignSpawn]) -> SignSpawn {
    let matching: Vec<&SignSpawn> = spawns.iter().filter(|s| s.pos == SIGN).collect();
    assert_eq!(
        matching.len(),
        1,
        "the mixed-style probe sign at {SIGN:?} must reach sign_spawns; got positions {:?}. \
         Place it with the setblock in live_sign_text_wire.rs's doc.",
        spawns.iter().map(|s| s.pos).collect::<Vec<_>>()
    );
    let spawn = matching[0].clone();
    let front_spans: usize = spawn.front.lines.iter().map(Vec::len).sum();
    assert!(
        front_spans >= 3,
        "the live sign reached sign_spawns with only {front_spans} front span(s): {:?}",
        spawn.front.lines
    );
    spawn
}

/// The same spawn with its words removed and every other field untouched —
/// the control both gates below subtract, so what survives is exactly the
/// text the server sent.
fn blanked(spawn: &SignSpawn) -> SignSpawn {
    let mut blanked = spawn.clone();
    blanked.front.lines = Default::default();
    blanked.back.lines = Default::default();
    blanked
}

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

/// `RenderState` draws an unconditional first-person bare arm at a fixed
/// screen rect whenever no third-person body is reported — see
/// `partial_connectivity_hall_holes.rs`'s identical helper. It would land in
/// both arms and cancel out of the A/B, but it also paints inside the frame,
/// so it is suppressed rather than reasoned about.
fn suppress_first_person_arm(state: &mut RenderState) {
    state.set_third_person_body_source(|| {
        Some(ThirdPersonBodyState {
            player_skin: None,
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

/// Meshes and uploads the live world's own sections around the sign, so the
/// sign's **board** is real terrain geometry in the depth buffer rather than
/// absent.
fn upload_live_terrain(
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    world: &World,
    models: &lodestone_render::BlockModels,
) -> usize {
    let centre = ChunkPos::from_block(SIGN[0], SIGN[2]);
    let mut uploaded = 0usize;
    for cx in (centre.x - 2)..=(centre.x + 2) {
        for cz in (centre.z - 2)..=(centre.z + 2) {
            let Some(chunk) = world.get(ChunkPos { x: cx, z: cz }) else {
                continue;
            };
            let min_y = chunk.column.min_y();
            for si in 0..chunk.column.section_count() {
                let key = SectionKey { cx, cz, si, min_y };
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let opaque = mesh_snapshot_models(&snap, models, false);
                state.upload_section(
                    device,
                    queue,
                    key,
                    &SectionGeometry::Model {
                        opaque,
                        water: ModelMesh::default(),
                        translucent_blocks: ModelMesh::default(),
                        visibility: snapshot_visibility(&snap, models),
                    },
                );
                uploaded += 1;
            }
        }
    }
    uploaded
}

#[test]
#[ignore = "requires the creative oracle on 127.0.0.1:25570, a GPU adapter, the vanilla client.jar, and --features live"]
fn a_live_servers_sign_text_reaches_pixels() {
    let net = joined();
    let spawns = live_spawns(&net);
    let subject = subject_spawn(&spawns);

    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let rect = expected_text_rect(&subject, camera.view_projection());
    println!("live spawn {:?}", subject.pos);
    println!("front lines {:?}", subject.front.lines);
    println!("expected text rect (from the real placement transform): {rect:?}");
    assert!(
        rect.area() > 200,
        "the sign's text plane projects to only {} px — the camera, not the \
         renderer, is wrong: {rect:?}",
        rect.area()
    );

    let mut shoot = |list: Vec<SignSpawn>| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        suppress_first_person_arm(&mut state);
        state.set_sign_source(move |_eye| list.clone());
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(vec![subject.clone()]);
    let (control_px, control_stats) = shoot(vec![blanked(&subject)]);

    // Exact corroboration, independent of any pixel threshold.
    assert!(
        subject_stats.sign_text_vertices > 0,
        "a live sign carrying real spans produced zero sign-text vertices — the \
         break is between sign_spawns and the vertex buffer (font load, layout, \
         or push_side_quads), not in the supply"
    );
    assert_eq!(
        control_stats.sign_text_vertices, 0,
        "the blanked control must submit no sign-text geometry, or the \
         subtraction below measures something other than the text"
    );

    println!(
        "sky {:?}, subject vertices {}, control vertices {}",
        sky_bytes(),
        subject_stats.sign_text_vertices,
        control_stats.sign_text_vertices
    );

    let (changed_rect, changed_count) = changed(&subject_px, &control_px).unwrap_or_else(|| {
        panic!(
            "the live sign's own text changed ZERO pixels while submitting {} \
             vertices — geometry reached the buffer and no ink reached the frame",
            subject_stats.sign_text_vertices
        )
    });
    println!("changed {changed_count} px, bbox {changed_rect:?}");
    assert!(
        changed_count > 30,
        "only {changed_count} px differ; three lines of real text must paint more \
         than that. bbox {changed_rect:?}, expected rect {rect:?}"
    );

    let padded = rect.padded(2);
    assert!(
        padded.contains(changed_rect.x0, changed_rect.y0)
            && padded.contains(changed_rect.x1, changed_rect.y1),
        "the changed pixels' bbox {changed_rect:?} escapes the sign's own text \
         plane {padded:?} — something other than this sign's text moved"
    );
}

/// **The gate that matches the owner's actual report**, and the one thing the
/// test above still cannot see.
///
/// "The board draws, the text does not" is a claim about the text losing to
/// the *board*, and a sign's board is not drawn by the sign pass at all — it
/// is ordinary block-model geometry the terrain mesher produces (see
/// `lodestone_render::sign`'s module doc). Every hermetic sign gate renders
/// against an empty world, so **no gate in this tree has ever put a sign's own
/// board in the depth buffer underneath its text**.
///
/// The separation is small and this project's depth is forward `[0, 1]` rather
/// than vanilla's reversed-Z, which spends float exponent where depth needs
/// it: `template_sign_rot_0`'s board spans `z ∈ [7.33333, 8.66667]/16` and
/// `StandingSignRenderer.TEXT_OFFSET` puts the text plane at
/// `0.5 + 0.046666667`, i.e. **0.005 blocks** — 5 mm — in front of the front
/// face. So this arm renders the live sign over the live world's own meshed
/// terrain and requires the text to survive.
///
/// # What it measured, and the failure that was worth having
///
/// It reproduced the owner's exact symptom on its first correct run —
/// 38,528 px of board, 1,236 sign-text vertices submitted, **0 px of text
/// surviving** — with the camera on the sign's *back* side. That is right
/// behaviour seen from the wrong place, not a defect: a `rotation=0` sign's
/// front text is on `+Z`, so from the north it is genuinely behind its own
/// board. It is recorded because it is the whole point of this file: the
/// sky-only arm above **passes from either side**, since with no board there
/// is nothing for the far-side text to hide behind. From the front the same
/// gate measures 221 px over 38,528 px of board.
///
/// So a hermetic sign gate cannot tell "the text draws" from "the text draws
/// where the board would have eaten it", and any future claim that sign text
/// is fine has to come from this arm rather than that one.
#[test]
#[ignore = "requires the creative oracle on 127.0.0.1:25570, a GPU adapter, the vanilla client.jar, and --features live"]
fn a_live_signs_text_survives_its_own_board_in_the_depth_buffer() {
    let net = joined();
    let spawns = live_spawns(&net);
    let subject = subject_spawn(&spawns);

    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load — this gate needs a real client.jar under \
             .cache/mc/26.2 (LODESTONE_ASSETS)"
        )
    });
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let rect = expected_text_rect(&subject, camera.view_projection());

    let handle = net.shared_handle();
    let client = handle.get().expect("client");
    let store = client.chunk_world();

    let mut shoot = |list: Vec<SignSpawn>, terrain: bool| -> (Vec<u8>, usize, u32) {
        let mut state = RenderState::new(device, queue, format, W, H, Some(&atlas));
        suppress_first_person_arm(&mut state);
        // Both arms carry the same fog and the same animation tick: a
        // freshly uploaded section starts mid-fade, so without advancing the
        // clock past the fade the terrain is submitted and draws nothing —
        // which is exactly the vacuous "uploaded > 0 but 0 px" state the
        // premise check below exists to catch.
        state.set_fog(FogSettings::for_render_distance(SKY_COLOR, RD_CHUNKS), RD_CHUNKS);
        let uploaded = if terrain {
            let world = store.read();
            upload_live_terrain(&mut state, device, queue, &world, models)
        } else {
            0
        };
        state.update_animation(queue, FADE_COMPLETE_TICK);
        state.set_sign_source(move |_eye| list.clone());
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (
            target.read_texels(device, queue),
            uploaded,
            stats.sign_text_vertices,
        )
    };

    // Arm A: the live sign over the live board. Arm B: the identical frame
    // with only the words removed.
    let (with_text, uploaded, text_vertices) = shoot(vec![subject.clone()], true);
    let (board_only, uploaded_b, blank_vertices) = shoot(vec![blanked(&subject)], true);
    assert!(
        uploaded > 0 && uploaded == uploaded_b,
        "fixture: both arms must upload the same live sections ({uploaded} vs {uploaded_b})"
    );
    assert!(text_vertices > 0, "the live sign submitted no text geometry");
    assert_eq!(blank_vertices, 0, "the blanked control submitted geometry");

    // The fixture's own premise, measured rather than assumed: the board has
    // to actually be in the frame, or this gate degenerates into the sky-only
    // one above and its green means nothing. This check earned its keep on the
    // first run — a freshly uploaded section starts mid-fade, so 41 sections
    // uploaded and painted **0 px** until the animation clock advanced.
    let (no_terrain_px, _, _) = shoot(vec![blanked(&subject)], false);
    let board_pixels = changed(&board_only, &no_terrain_px).map_or(0, |(_, n)| n);
    println!("uploaded {uploaded} live section(s); board paints {board_pixels} px");
    assert!(
        board_pixels > 200,
        "only {board_pixels} px differ between a meshed live world and an empty \
         one — the sign's board is not in the depth buffer, so this gate is not \
         testing what it claims"
    );

    let (changed_rect, changed_count) = changed(&with_text, &board_only).unwrap_or_else(|| {
        panic!(
            "the live sign's text changed ZERO pixels against its own board while \
             submitting {text_vertices} vertices — this is the owner's exact report \
             reproduced: the board draws and the text loses to it"
        )
    });
    println!("over the board: changed {changed_count} px, bbox {changed_rect:?}");
    assert!(
        changed_count > 30,
        "only {changed_count} px of text survive over the sign's own board (the \
         sky-only arm paints far more), so the text is being eaten by the board's \
         depth. bbox {changed_rect:?}, expected rect {rect:?}"
    );
    let padded = rect.padded(2);
    assert!(
        padded.contains(changed_rect.x0, changed_rect.y0)
            && padded.contains(changed_rect.x1, changed_rect.y1),
        "the surviving pixels' bbox {changed_rect:?} escapes the text plane {padded:?}"
    );
}
