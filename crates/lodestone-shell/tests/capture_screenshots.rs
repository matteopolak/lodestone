//! Capture the README's in-game screenshots by driving the **real** client
//! against the flat creative 26.2 oracle and writing PNGs to `docs/images/`.
//!
//! # What it is
//!
//! A live gate in the shape of `live_sign_text_pixels.rs` that ends at a file
//! instead of at an assertion. It joins the oracle with [`Sim`] — the same type
//! `WindowApp` drives — installs the render sources `app/redraw.rs` and
//! `app/session.rs` install, builds each scene over RCON, renders one frame
//! through [`RenderState::render`] and encodes it with
//! [`lodestone::screenshot::encode_png`], the same encoder the `key.screenshot`
//! keybind uses.
//!
//! Nothing here is staged: every pixel comes from this client rendering a real
//! session against a real vanilla server.
//!
//! # How it works
//!
//! Scenes are **data**, not code: one `scripts/screenshot-scenes/<name>.txt`
//! per image. A line starting with `@` is a directive, `#` is a comment, and
//! anything else is an RCON command run verbatim before the shot. That split is
//! deliberate — a scene edit must not cost a seven-minute recompile of this
//! crate, and the camera belongs beside the build that it is aimed at.
//!
//! ```text
//! @size 768 432          # framebuffer, and therefore the PNG
//! @camera 0.5 -58.0 2.0  # eye position, world coordinates
//! @look 0.5 -57.6 12.0   # aim at a point (mutually exclusive with @yawpitch)
//! @yawpitch 180 8        # or aim explicitly, in the render camera's convention
//! @fov 70                # vertical FOV, degrees (default 70)
//! @wait 1500             # milliseconds to keep pumping the sim after the build
//! @hud                   # composite the HUD over the world (off by default)
//! @hand                  # draw the first-person hand (off by default)
//! ```
//!
//! # How to change it
//!
//! Add or edit a file under `scripts/screenshot-scenes/`; nothing in this file
//! needs to know about it. `LODESTONE_SCENES=name1,name2` restricts a run to
//! those stems, which is how you iterate on one image without paying for the
//! whole set.
//!
//! Gotchas, each of which cost a run:
//!
//! * **A freshly uploaded section is mid-fade and draws nothing** until the
//!   animation clock passes it — `FADE_COMPLETE_TICK` in
//!   `live_sign_text_pixels.rs` records the same trap. The pump loop here runs
//!   long enough that `Sim::tick_count` is well past it.
//! * **Every scene shares one world**, so a scene must build what it needs and
//!   must not assume the plot is empty. Each file starts with its own `fill`.
//! * **The camera is free**, but only sections the server streamed to the
//!   player are meshed, so keep a scene within a few chunks of the spawn
//!   column — everything here is inside a 48-block box around it.
//!
//! # Configuration
//!
//! `LODESTONE_SCENES` (optional filter). The oracle's ports and password are
//! the constants below, matching `scripts/live-oracles/creative.sh`.
//!
//! # Dependencies
//!
//! The flat creative 26.2 oracle (`scripts/live-oracles/creative.sh`), a wgpu
//! adapter, the vanilla assets under `.cache/mc/26.2`, and `--features live`.
//!
//! ```text
//! just screenshots
//! ```
#![cfg(feature = "live")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::gpu::RenderState;
use lodestone::hud::{HotbarSlot, HudFrame, HudRenderer};
use lodestone::sim::Sim;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};
use lodestone_testsupport::{RconClient, unique_username};

const HOST: &str = "127.0.0.1";
/// The flat creative 26.2 oracle: game on `:25570`, RCON on `:25571`.
const PORT: u16 = 25570;
const RCON_ADDR: &str = "127.0.0.1:25571";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;

/// Chunks the sim is told to keep. Every scene sits inside this radius of the
/// spawn column, so the camera never looks at an unmeshed section.
const RENDER_DISTANCE: u32 = 8;

/// The world spawn this harness pins before joining, so a scene file can name
/// absolute coordinates instead of offsets from wherever the last run left the
/// spawn point.
const SPAWN: [i32; 3] = [0, -60, 0];

/// The camera bot's name, and the one whose eye every frame is rendered from.
/// Fixed rather than [`unique_username`] because it is the name the tab list
/// screenshot shows; see this file's `join_companions` for the tradeoff.
const CAMERA_NAME: &str = "Lodestone";

/// Extra clients joined only so the tab list has more than one row. Same
/// fixed-name tradeoff as [`CAMERA_NAME`].
const COMPANIONS: [&str; 4] = ["Ferris", "Basalt", "Cinder", "Quartz"];

/// The framebuffer, unless a scene overrides it with `@size`.
const DEFAULT_SIZE: (u32, u32) = (768, 432);

fn main_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// One scene: the parsed directives plus the RCON commands that build it.
#[derive(Debug)]
struct Scene {
    name: String,
    commands: Vec<String>,
    size: (u32, u32),
    eye: glam::Vec3,
    /// Resolved to yaw/pitch at parse time, whichever directive supplied it.
    yaw: f32,
    pitch: f32,
    fov: f32,
    settle: Duration,
    hud: bool,
    hand: bool,
}

/// Yaw/pitch (degrees) that aim the camera from `eye` at `target`, inverting
/// the render camera's convention `forward = (-sin y·cos p, -sin p, cos y·cos p)`.
/// Copied from `live_entity_render.rs`, which derives it the same way.
fn look_at(eye: glam::Vec3, target: glam::Vec3) -> (f32, f32) {
    let d = (target - eye).normalize();
    ((-d.x).atan2(d.z).to_degrees(), (-d.y).asin().to_degrees())
}

fn parse_scene(path: &Path) -> Scene {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading scene {}: {e}", path.display()));
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned();

    let mut scene = Scene {
        name,
        commands: Vec::new(),
        size: DEFAULT_SIZE,
        eye: glam::Vec3::new(0.5, -58.0, 0.5),
        yaw: 0.0,
        pitch: 0.0,
        fov: 70.0,
        settle: Duration::from_millis(1500),
        hud: false,
        hand: false,
    };
    // `@look` may appear before or after `@camera`, so the aim point is held
    // aside and resolved once the eye is final.
    let mut look: Option<glam::Vec3> = None;

    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix('@') else {
            scene.commands.push(line.to_owned());
            continue;
        };
        let mut parts = rest.split_whitespace();
        let directive = parts.next().unwrap_or_default();
        let nums: Vec<f32> = parts.filter_map(|p| p.parse().ok()).collect();
        let where_ = format!("{}:{}", path.display(), n + 1);
        match directive {
            "size" => {
                assert!(nums.len() == 2, "@size wants two numbers ({where_})");
                scene.size = (nums[0] as u32, nums[1] as u32);
            }
            "camera" => {
                assert!(nums.len() == 3, "@camera wants three numbers ({where_})");
                scene.eye = glam::Vec3::new(nums[0], nums[1], nums[2]);
            }
            "look" => {
                assert!(nums.len() == 3, "@look wants three numbers ({where_})");
                look = Some(glam::Vec3::new(nums[0], nums[1], nums[2]));
            }
            "yawpitch" => {
                assert!(nums.len() == 2, "@yawpitch wants two numbers ({where_})");
                (scene.yaw, scene.pitch) = (nums[0], nums[1]);
                look = None;
            }
            "fov" => {
                assert!(nums.len() == 1, "@fov wants one number ({where_})");
                scene.fov = nums[0];
            }
            "wait" => {
                assert!(nums.len() == 1, "@wait wants one number ({where_})");
                scene.settle = Duration::from_millis(nums[0] as u64);
            }
            "hud" => scene.hud = true,
            "hand" => scene.hand = true,
            other => panic!("unknown directive @{other} ({where_})"),
        }
    }
    if let Some(target) = look {
        (scene.yaw, scene.pitch) = look_at(scene.eye, target);
    }
    scene
}

fn scenes() -> Vec<Scene> {
    let dir = main_dir().join("scripts/screenshot-scenes");
    let filter: Option<Vec<String>> = std::env::var("LODESTONE_SCENES").ok().map(|v| {
        v.split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    });
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    paths.sort();
    let mut out: Vec<Scene> = paths.iter().map(|p| parse_scene(p)).collect();
    if let Some(names) = &filter {
        out.retain(|s| names.iter().any(|n| n == &s.name));
        assert!(
            !out.is_empty(),
            "LODESTONE_SCENES={names:?} matched no scene under {}",
            dir.display()
        );
    }
    assert!(!out.is_empty(), "no scenes under {}", dir.display());
    out
}

/// Step the sim one tick and drain its frame outputs the way `app/redraw.rs`
/// does — removals **before** uploads, which is the order that file documents.
fn pump(sim: &mut Sim, render: &mut RenderState, device: &wgpu::Device, queue: &wgpu::Queue) {
    sim.step(1.0 / 20.0);
    for key in sim.drain_removals() {
        render.remove_section(&key);
    }
    for meshed in sim.drain_meshes() {
        render.upload_section(device, queue, meshed.key, &meshed.mesh);
    }
}

fn live_config() -> Config {
    Config {
        mode: Mode::Window,
        host: HOST.into(),
        port: PORT,
        protocol: PROTOCOL,
        connect_in_window: true,
        render_distance: RENDER_DISTANCE,
        ..Config::default()
    }
}

#[test]
#[ignore = "capture harness: requires the flat creative 26.2 oracle on :25570 (+ RCON :25571), the vanilla assets under .cache/mc/26.2, a GPU adapter, and `--features live`"]
fn capture_readme_screenshots() {
    let scenes = scenes();
    let out_dir = main_dir().join("docs/images");
    std::fs::create_dir_all(&out_dir).expect("docs/images");

    let ctx = GpuContext::new_headless_blocking().expect(
        "no wgpu adapter. This harness renders the real client; there is nothing to \
         capture without one.",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // sRGB, unlike the pixel gates' `Rgba8Unorm`: the window's own swapchain is
    // viewed as sRGB (see `SurfaceTarget`'s `view_formats`), so this is the
    // format whose stored bytes are the ones a player sees — and therefore the
    // ones that belong in a PNG. A non-sRGB target would build every pipeline
    // against a linear write and the file would come out dark.
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let mut rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD).unwrap_or_else(|e| {
        panic!(
            "cannot reach RCON at {RCON_ADDR}: {e}. Fix: ./scripts/live-oracles/creative.sh"
        )
    });
    rcon.cmd(&format!(
        "setworldspawn {} {} {}",
        SPAWN[0], SPAWN[1], SPAWN[2]
    ));
    // Keep the whole build area resident whether or not a player is standing in
    // it; the flat oracle unloads columns aggressively.
    rcon.cmd("forceload add -32 -32 32 32");
    rcon.cmd("gamerule doDaylightCycle false");
    rcon.cmd("gamerule doWeatherCycle false");
    rcon.cmd("weather clear");
    // Late morning: a high sun, long-ish shadows, no night desaturation.
    rcon.cmd("time set 2000");

    let mut sim = Sim::new(live_config());
    assert!(
        sim.vanilla_atlas().is_some(),
        "vanilla assets did not load, so this would capture the demo palette rather than \
         the game. Banner: {:?}. Fix: put a vanilla pack at .cache/mc/26.2 or set \
         LODESTONE_ASSETS.",
        sim.asset_banner()
    );
    sim.connect_as(HOST.into(), PORT, PROTOCOL, CAMERA_NAME.to_owned());

    let (mut w, mut h) = scenes[0].size;
    let mut target = HeadlessTarget::new(device, w, h, format);
    let mut render = RenderState::new(device, queue, format, w, h, sim.vanilla_atlas());
    let mut hud = HudRenderer::new(device, format);

    // Join the world before anything else: the sources below capture the
    // session handle, and a scene's blocks only stream to a client that is in.
    let deadline = Instant::now() + Duration::from_secs(60);
    let demo_spawn = sim.player().position;
    let mut placed = false;
    while Instant::now() < deadline {
        pump(&mut sim, &mut render, device, queue);
        if let Some(net) = sim.net()
            && net.world_dimensions().is_some()
            && !net.loaded_chunks().is_empty()
            && sim.player().position != demo_spawn
        {
            placed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        placed,
        "the server never placed the camera client within 60s (still at the demo spawn \
         {demo_spawn:?}). Fix: ./scripts/live-oracles/creative.sh"
    );
    rcon.cmd(&format!("gamemode creative {CAMERA_NAME}"));

    install_render_sources(&mut render, &sim, device, queue, format);
    let companions = join_companions();
    // The companions exist to be tab-list rows, not to stand in shot. Spectator
    // hides their bodies and their name plates from every other client, and a
    // spectator is still a tab-list entry — which is the whole of what they are
    // for. Without this they spawn on top of the camera and a nameplate covers
    // half the frame; measured on the first capture.
    for name in COMPANIONS {
        rcon.cmd(&format!("gamemode spectator {name}"));
        rcon.cmd(&format!("tp {name} 0 -20 0"));
    }
    if let Some(time) = sim
        .net()
        .and_then(|n| n.shared_handle().get().map(|h| h.world_time()))
    {
        println!("world time (game, day) = {time:?}");
    }

    let mut written: Vec<(String, u64)> = Vec::new();
    for scene in &scenes {
        if scene.size != (w, h) {
            (w, h) = scene.size;
            target = HeadlessTarget::new(device, w, h, format);
            render.resize(device, w, h);
        }
        for command in &scene.commands {
            let reply = rcon.cmd(command);
            // `<--[HERE]` is the caret vanilla's `CommandSyntaxException`
            // appends to every parse failure, whatever the message above it
            // says — a single marker beats guessing at the wording of "Unknown
            // block type", "Expected integer" and the rest. A command that
            // parses and then fails (an empty `fill`, say) is not an error
            // here: a scene legitimately clears ground that is already clear.
            assert!(
                !reply.contains("<--[HERE]"),
                "scene {:?} command did not parse:\n  {command}\n  -> {reply}",
                scene.name
            );
        }
        // Let the edits stream back and the freshly meshed sections leave their
        // fade-in. `Sim::step` is what pumps the net thread's update channel.
        let until = Instant::now() + scene.settle;
        while Instant::now() < until {
            pump(&mut sim, &mut render, device, queue);
            std::thread::sleep(Duration::from_millis(10));
        }

        let bytes = shoot(
            scene,
            &mut sim,
            &mut render,
            &mut hud,
            &mut target,
            device,
            queue,
            w,
            h,
        );
        let path = out_dir.join(format!("{}.png", scene.name));
        std::fs::write(&path, &bytes).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
        written.push((scene.name.clone(), bytes.len() as u64));
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }

    drop(companions);
    println!("=== captured {} scene(s) ===", written.len());
    for (name, size) in &written {
        println!("  {name:<28} {size:>8} bytes");
    }
}

/// Render one scene and return its PNG bytes.
///
/// The colour-variance check at the end is the harness's own control: a frame
/// that failed to mesh, failed to light, or landed inside a block reads as a
/// nearly-uniform image, and writing that to `docs/images/` is exactly the
/// silent failure a capture tool must not have.
#[allow(clippy::too_many_arguments)]
fn shoot(
    scene: &Scene,
    sim: &mut Sim,
    render: &mut RenderState,
    hud: &mut HudRenderer,
    target: &mut HeadlessTarget,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let camera = Camera {
        position: scene.eye,
        yaw: scene.yaw,
        pitch: scene.pitch,
        fov_y_degrees: scene.fov,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RENDER_DISTANCE, 0),
    };

    if !scene.hand {
        // `RenderState` draws an unconditional first-person bare arm whenever no
        // third-person body is reported, at a fixed screen rect — the hazard
        // `distant_flat_terrain_holes.rs` records. A disembodied arm in a scenic
        // shot is noise, so scenes opt into it with `@hand`.
        render.set_third_person_body_source(|| {
            Some(lodestone::gpu::ThirdPersonBodyState {
                player_skin: None,
                feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
                body_yaw_deg: 0.0,
                anim: lodestone_render::entity_anim::AnimInput::default(),
                scale: 1.0,
                swim_amount: 0.0,
                slim: false,
                equipment: Vec::new(),
            })
        });
    }

    // Per-frame source installs, in `app/redraw.rs`'s own order — every one of
    // these is a closure over a clock or a snapshot, so a stale install freezes
    // or drops whatever it feeds.
    install_frame_sources(render, sim);
    render.set_fog(sim.fog_settings(), RENDER_DISTANCE);
    render.set_clear_color_tracked(sim.fog_settings().color);
    render.set_sky_mode(sim.sky_mode());
    render.update_animation(queue, sim.tick_count());
    let particles = sim.extract_particles(&camera);
    let _ = particles;
    render.prepare_particles(device, queue, &sim.particle_instances(), &camera);

    let entity_draws = sim.entity_draws();
    let frame = target.acquire().expect("headless acquire");
    let stats = render.render(device, queue, frame.view(), &camera, None, &entity_draws);

    if scene.hud {
        let raw_view = frame.create_view(target.raw_view_format());
        let hotbar = hotbar_records(sim);
        let tab = sim.tab_list_view();
        let sidebar = sim.sidebar();
        let air = sim.air().map(|a| {
            (
                a,
                lodestone_game::player_state::HudState::MAX_AIR,
                sim.player().eye_in_water,
            )
        });
        let hud_frame = HudFrame {
            crosshair: true,
            can_hurt_player: true,
            health: sim.health(),
            food: sim.food(),
            saturation: sim.saturation(),
            armour: sim.armour_value(),
            air,
            xp: sim.xp(),
            hotbar: Some(sim.selected_slot()),
            hotbar_items: Some(hotbar.as_slice()),
            players: Some(&tab),
            sidebar: sidebar.as_ref(),
            attack_cooldown: Some(sim.attack_strength_scale()),
            ..HudFrame::new(&sim.stats)
        };
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            &raw_view,
            Some(render.depth_view()),
            &hud_frame,
            sim.vanilla_atlas().and_then(lodestone_render::BlockAtlas::models),
            0,
            w,
            h,
        );
    }

    let pixels = target.read_texels(device, queue);
    println!(
        "[{}] {w}x{h} eye {:?} yaw {:.1} pitch {:.1} — {} sections, {} quads, {} entities",
        scene.name,
        scene.eye,
        scene.yaw,
        scene.pitch,
        stats.sections_drawn,
        stats.total_quads,
        stats.entities_drawn,
    );

    // The control, and it is deliberately *two* numbers rather than one.
    //
    // `sections_drawn` and `total_quads` above are draw counters, and this
    // repo has measured a harness that submitted geometry and read back
    // nothing while every counter reported health — so the counters cannot
    // stand in for pixels. `distinct` catches a frame that never lit (one flat
    // colour); `off_modal` catches a frame that is *mostly* one thing — a
    // camera inside a block, or a scene that failed to build, both of which
    // leave a legible sky gradient and so clear a distinct-colour floor on
    // their own.
    //
    // The thresholds are set under the measured values, not at a round number:
    // the first scene captured reads 322 distinct / 0.79 off-modal, and a
    // camera buried in deepslate reads 1 / 0.00.
    let distinct = distinct_colours(&pixels);
    let off_modal = off_modal_fraction(&pixels);
    println!(
        "[{}] control: {distinct} distinct colours, {:.2} of pixels off the modal colour",
        scene.name, off_modal
    );
    assert!(
        distinct >= 64 && off_modal >= 0.25,
        "scene {:?} is not a screenshot: {distinct} distinct colours, {off_modal:.2} off-modal. \
         Sections drawn: {}, quads: {}.",
        scene.name,
        stats.sections_drawn,
        stats.total_quads
    );

    lodestone::screenshot::encode_png(&pixels, w, h).expect("png encode")
}

/// Number of distinct RGB triples in a frame, quantised to 5 bits per channel
/// so dithering and light gradients do not inflate the count into meaning
/// nothing.
fn distinct_colours(pixels: &[u8]) -> usize {
    let mut seen = std::collections::HashSet::new();
    for px in pixels.chunks_exact(4) {
        seen.insert((px[0] >> 3, px[1] >> 3, px[2] >> 3));
    }
    seen.len()
}

/// Fraction of pixels that are **not** the frame's single most common colour,
/// at the same 5-bit quantisation.
///
/// This is the half `distinct_colours` cannot see: a camera stuck inside a
/// block still renders the fog gradient over most of the frame and can carry
/// hundreds of distinct colours while showing nothing.
fn off_modal_fraction(pixels: &[u8]) -> f64 {
    let mut counts = std::collections::HashMap::new();
    let mut total = 0usize;
    for px in pixels.chunks_exact(4) {
        *counts
            .entry((px[0] >> 3, px[1] >> 3, px[2] >> 3))
            .or_insert(0usize) += 1;
        total += 1;
    }
    let modal = counts.values().copied().max().unwrap_or(0);
    if total == 0 {
        return 0.0;
    }
    1.0 - (modal as f64 / total as f64)
}

/// The hotbar row, built the way `app/redraw.rs` builds it — minus two fields
/// this side of the crate boundary cannot reach.
///
/// `enchanted` and `skin` come from `hud::item_icon`, which is `pub(crate)`, so
/// an integration test cannot call it. The consequence is narrow and stated
/// rather than hidden: a **glinting** or **custom-head** stack in a captured
/// hotbar would draw without its foil or its face. No scene puts one there.
fn hotbar_records(sim: &Sim) -> Vec<Option<HotbarSlot>> {
    let menu = sim.player_menu();
    (0..9)
        .map(|i| {
            menu.player_native(i).and_then(|st| {
                let item = lodestone_assets::ResourceLocation::parse(&st.item().to_string()).ok()?;
                let damage = st
                    .components()
                    .get_int(lodestone_game::item::DAMAGE_COMPONENT)
                    .and_then(|v| u32::try_from(v).ok());
                let max_damage = st
                    .components()
                    .get_int(lodestone_game::item::MAX_DAMAGE_COMPONENT)
                    .and_then(|v| u32::try_from(v).ok());
                Some(HotbarSlot {
                    item,
                    count: st.count().max(0) as u32,
                    damage,
                    max_damage,
                    enchanted: false,
                    dyed_color: st.dyed_color(),
                    potion_color: st.potion_color(),
                    banner_patterns: st.banner_patterns().to_vec(),
                    base_color: st.base_color().map(str::to_owned),
                    skin: None,
                })
            })
        })
        .collect()
}

/// The block-entity and display sources `app/redraw.rs` re-installs every
/// frame. Kept in that file's order so the two can be diffed by eye — a source
/// missing here is a hole in the world, not a missing decoration, for every one
/// of the block types whose 26.2 model is empty (chests, shulkers, pots,
/// conduits, banners).
fn install_frame_sources(render: &mut RenderState, sim: &Sim) {
    if let Some(f) = sim.block_entity_source() {
        render.set_block_entity_source(f);
    }
    if let Some(f) = sim.skull_source() {
        render.set_skull_source(f);
    }
    if let Some(f) = sim.copper_golem_statue_source() {
        render.set_copper_golem_statue_source(f);
    }
    if let Some(f) = sim.sign_source() {
        render.set_sign_source(f);
    }
    if let Some(f) = sim.beacon_source() {
        render.set_beacon_source(f);
    }
    render.set_display_draws(sim.display_draws());
    if let Some(f) = sim.end_portal_source() {
        render.set_end_portal_source(f);
    }
    if let Some(f) = sim.end_gateway_source() {
        render.set_end_gateway_source(f);
    }
    render.set_end_portal_game_time(sim.game_time_for_shaders());
    if let Some(f) = sim.end_gateway_beam_source() {
        render.set_end_gateway_beam_source(f);
    }
    if let Some(f) = sim.bell_source() {
        render.set_bell_source(f);
    }
    if let Some(f) = sim.shulker_source() {
        render.set_shulker_source(f);
    }
    if let Some(f) = sim.decorated_pot_source() {
        render.set_decorated_pot_source(f);
    }
    if let Some(f) = sim.conduit_source() {
        render.set_conduit_source(f);
    }
    if let Some(f) = sim.banner_source() {
        render.set_banner_source(f);
    }
    if let Some(f) = sim.lectern_source() {
        render.set_lectern_source(f);
    }
    if let Some(f) = sim.campfire_source() {
        render.set_campfire_source(f);
    }
    if let Some(f) = sim.brushable_source() {
        render.set_brushable_source(f);
    }
    if let Some(f) = sim.shelf_source() {
        render.set_shelf_source(f);
    }
    if let Some(f) = sim.vault_source() {
        render.set_vault_source(f);
    }
    if let Some(f) = sim.enchanting_table_source() {
        render.set_enchanting_table_source(f);
    }
    if let Some(f) = sim.moving_piston_source() {
        render.set_moving_piston_source(f);
    }
    if let Some(f) = sim.spawner_source() {
        render.set_spawner_source(f);
    }
    if let Some(f) = sim.map_source() {
        render.set_map_source(f);
    }
}

/// The once-per-session installs `app/session.rs`'s
/// `install_session_render_sources` performs: the sky pass and its textures,
/// the per-dimension ambient floor, the time-of-day clock and the entity light
/// sampler.
///
/// Without the light sampler every mob and every block entity renders at a
/// constant brightness, which looks *plausible* in a screenshot and is wrong —
/// exactly the failure this harness must not ship, so it is installed here even
/// though nothing would go red without it.
fn install_render_sources(
    render: &mut RenderState,
    sim: &Sim,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
) {
    let Some(net) = sim.net() else {
        panic!("no session attached; the join loop above should have made this impossible")
    };
    let handle = net.shared_handle();
    let sky_policy = net.shared_sky_default();

    let darken = handle.clone();
    render.set_sky_darken_source(move || {
        darken
            .get()
            .map(|h| lodestone_render::entity::sky_darken_for_time_of_day(h.world_time().1))
    });
    let ambient = handle.clone();
    render.set_ambient_light_source(move || {
        let dim = ambient.get()?.player().dimension_type?;
        Some(match dim.ambient_light_color {
            Some(packed) => lodestone_render::light::rgb24_to_channels(packed),
            None => lodestone_render::light::OVERWORLD_AMBIENT_LIGHT,
        })
    });
    let light = handle.clone();
    render.set_entity_light_source(move |feet| {
        lodestone::net::entity_light_at(
            &light,
            feet.x.floor() as i32,
            feet.y.floor() as i32,
            feet.z.floor() as i32,
            sky_policy.get(),
        )
    });
    // The raw day-time tick, not `sky_darken`'s derived factor — the sky pass
    // needs the tick itself to place the sun, the moon and the cloud scroll.
    // `app/session.rs` wraps this in `ContinuousTimeOfDay` so the clouds do not
    // step once a second between `SET_TIME` packets; a still frame cannot see
    // that, so the raw value is used here.
    let clock = handle;
    render.set_time_of_day_source(move || clock.get().map(|h| h.world_time().1));

    if !render.has_sky()
        && let Some(sky) = lodestone::resources::load_sky(device, queue, format)
    {
        render.install_sky(sky);
    }
    assert!(
        render.has_sky(),
        "the sky pass did not install, so every capture would have a flat void above the \
         horizon instead of a sky. `resources::load_sky` needs the vanilla pack stack \
         (.cache/mc/26.2 or LODESTONE_ASSETS)."
    );
    if !render.has_screen_effects()
        && let Some(fx) = lodestone::resources::load_screen_effects(device, queue, format)
    {
        render.install_screen_effects(fx);
    }
}

/// Join the extra clients whose only job is to be rows in the tab list.
///
/// **Fixed names, not [`unique_username`]**, and that is a deliberate exception
/// to the live-gate rule. Offline mode derives the account UUID from the name,
/// so a shared name is a shared player file — the hazard being that a *dead*
/// player is held on the death screen and is sent no chunks. These clients never
/// render anything and the oracle is flat, creative and peaceful, so there is
/// nothing here that can kill one; what a unique name would cost is the whole
/// point of the image, since `E0_1k3j9fa2` is not a screenshot of a tab list.
/// The camera client is put in creative on join for the same reason.
fn join_companions() -> Vec<lodestone::net::NetClient> {
    let clients: Vec<lodestone::net::NetClient> = COMPANIONS
        .iter()
        .map(|name| {
            lodestone::net::NetClient::connect_as(
                HOST.to_owned(),
                PORT,
                PROTOCOL,
                None,
                (*name).to_owned(),
            )
        })
        .collect();
    // Drain each one's update channel until it is in the world, so the camera
    // client's own tab list has actually received them. Bounded — a companion
    // that never arrives costs a thinner tab list, not a failed capture.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let ready = clients
            .iter()
            .filter(|c| {
                let _ = c.poll();
                !c.loaded_chunks().is_empty()
            })
            .count();
        if ready == clients.len() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = unique_username();
    clients
}
