//! `Mode::Headless` and `Mode::Connect`: the two native CLI diagnostics that
//! need a GPU adapter (or nothing at all) but never a window.
//!
//! Split out of `app.rs`/`app::runners` on purpose, and unconditional (no
//! `window` Cargo feature, no `winit` anywhere in this file) rather than
//! living inside the `app` module: `app` is winit's real, unavoidable
//! consumer (`WindowApp`'s `ApplicationHandler`, `EventLoop`, …) and is gated
//! entirely behind the `window` feature — see `crate`'s own module doc and
//! `docs/runtime-presentation.md`'s "genuinely winit-free headless build"
//! section. Keeping these two modes here means a `--no-default-features`
//! build (no `window`, no `winit` in the dependency graph — checked by `cargo
//! tree -p lodestone-shell --no-default-features -i winit`) still renders a
//! frame offscreen and still streams a live connection; only opening a real
//! window stops being possible.
//!
//! `crate::run` is what dispatches into this module; `app::run` (when the
//! `window` feature compiles that module in at all) calls the same two
//! functions rather than keeping a second copy.

// Both functions below are native-only (`std::fs` for the PPM, a raw TCP
// socket and a blocking sleep for the connect diagnostic — see each
// function's own doc) and so is everything they need; gating the imports the
// same way keeps a wasm32 build from carrying an unused-import warning for
// types nothing on that target calls.
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget, TargetError};

#[cfg(not(target_arch = "wasm32"))]
use crate::config::Config;
#[cfg(not(target_arch = "wasm32"))]
use crate::gpu::RenderState;
#[cfg(not(target_arch = "wasm32"))]
use crate::net::NetClient;
#[cfg(not(target_arch = "wasm32"))]
use crate::platform::Instant;
#[cfg(not(target_arch = "wasm32"))]
use crate::sim::Sim;

/// The ownership gate for the CLI diagnostic modes: the real account roster, or
/// a refusal naming what to do about it.
///
/// `Mode::Window` does not come through here: the windowed build has a UI, so
/// it shows `crate::menu::Screen::Ownership` and lets the player add an
/// account, which is a better answer than exiting. The diagnostic modes have
/// no UI to offer, so the only honest answer is to stop.
///
/// # Errors
/// Returns an error when no locally stored account owns the game.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn require_owned_account() -> anyhow::Result<lodestone_auth::Entitlement> {
    lodestone_auth::Entitlement::from_metadata(&lodestone_auth::AccountsMetadata::load()).ok_or_else(
        || {
            anyhow::anyhow!(
                "no Microsoft account that owns Minecraft is signed in. Run the game \
                 without --headless/--connect and add one from the Accounts screen first."
            )
        },
    )
}

// ---------------------------------------------------------------------------
// Headless: render one frame offscreen, save a PPM, print stats.
// ---------------------------------------------------------------------------

/// Native-only. `Mode::Headless` renders one frame and writes a PPM, and both
/// halves are native: `std::fs` returns `Err(Unsupported)` in a browser, and there is
/// no command line to select this mode in the first place.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run_headless(_owned: lodestone_auth::Entitlement, config: Config) -> anyhow::Result<()> {
    let ctx = GpuContext::new_headless_blocking()
        .map_err(|e| anyhow::anyhow!("headless GPU bring-up failed: {e}"))?;
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (1280u32, 720u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    let render_distance = config.render_distance;
    // The offline evidence path, and the one place the demo world still exists:
    // this renders a single frame with no server and *fails* below 5% terrain
    // coverage, so it needs a world that does not come from a connection. The
    // interactive client has none — see `Sim::new`. (`Sim::new` would delegate
    // here anyway on `Mode::Headless`; spelled out so the dependency is visible
    // at the call site rather than hidden in a mode check.)
    let mut sim = Sim::with_demo_world(config);
    let mut render = RenderState::new(device, queue, format, w, h, sim.vanilla_atlas());
    // Delegates to `crate::sim::fog_for_render_distance` so the colour and the
    // fade band have one definition shared with the frame clear (and with the
    // windowed path's own bring-up, in `app::sky_fog`) — a second copy of the
    // sky colour here is how the horizon ends up banding in a colour the sky
    // never is.
    render.set_fog(crate::sim::fog_for_render_distance(render_distance), render_distance);

    // Mesh everything and upload.
    let meshes = sim.drain_all_meshes();
    let mut meshed_quads = 0usize;
    for m in &meshes {
        meshed_quads += m.mesh.quad_count();
        render.upload_section(device, queue, m.key, &m.mesh);
    }

    // Let the player settle onto the ground so the camera sits at a sane height.
    for _ in 0..40 {
        sim.step(1.0 / 20.0);
    }

    let camera = sim.camera(w as f32 / h as f32);
    // Outline the block directly under the settled player, as a visible probe.
    let outline = {
        let p = sim.player().position;
        Some([
            p.x.floor() as i32,
            p.y.floor() as i32 - 1,
            p.z.floor() as i32,
        ])
    };
    let start = Instant::now();
    let frame = target
        .acquire()
        .map_err(|e: TargetError| anyhow::anyhow!("headless acquire failed: {e}"))?;
    let entity_draws = sim.entity_draws();
    let _ = sim.extract_particles(&camera);
    render.prepare_particles(device, queue, &sim.particle_instances(), &camera);
    render.update_animation(queue, sim.tick_count());
    let stats = render.render(device, queue, frame.view(), &camera, outline, &entity_draws);
    let pixels = target.read_texels(device, queue);
    let frame_ms = start.elapsed().as_secs_f64() * 1000.0;

    // Coverage: fraction of pixels that clearly aren't the sky clear colour.
    //
    // This target is *not* an sRGB-format texture, so these bytes are the
    // shader's linear output scaled straight to 0..255 with no gamma encode
    // (unlike the swapchain, which is sRGB and would encode them). That's
    // `SKY_COLOR * 255` rounded, not the on-screen sky colour — read
    // `gpu::SKY_COLOR`'s doc comment before touching this to keep the two in
    // sync.
    let sky = [62i32, 118, 211];
    let mut terrain_px = 0usize;
    for px in pixels.chunks_exact(4) {
        let d = (i32::from(px[0]) - sky[0]).abs()
            + (i32::from(px[1]) - sky[1]).abs()
            + (i32::from(px[2]) - sky[2]).abs();
        if d > 60 {
            terrain_px += 1;
        }
    }
    let coverage = terrain_px as f64 / (w * h) as f64 * 100.0;

    let out = "lodestone-frame.ppm";
    write_ppm(out, w, h, &pixels)?;

    sim.stats.section_count = stats.sections_drawn;
    sim.stats.quads = stats.total_quads;
    sim.stats.vram_bytes = stats.vram_bytes;
    sim.stats.vram_reserved_bytes = stats.vram_reserved_bytes;
    sim.stats.frame_ms = frame_ms as f32;

    println!("=== lodestone headless render ===");
    println!("world chunks      = {}", sim.chunk_count());
    println!("sections meshed   = {}", meshes.len());
    println!("sections drawn    = {}", stats.sections_drawn);
    println!("quads (meshed)    = {meshed_quads}");
    println!("quads (drawn)     = {}", stats.total_quads);
    println!("draw calls        = {}", stats.draw_calls);
    println!(
        "mesh VRAM (bytes) = {} live / {} reserved",
        stats.vram_bytes, stats.vram_reserved_bytes
    );
    println!("terrain coverage  = {coverage:.1}%");
    println!("frame time (ms)   = {frame_ms:.3}");
    println!("saved frame       = {out}");
    println!("{}", sim.stats.one_line());

    if coverage < 5.0 {
        anyhow::bail!("rendered frame shows <5% terrain ({coverage:.1}%) — nothing visible");
    }
    Ok(())
}

/// Write a binary (P6) PPM — no image-crate dependency needed for evidence.
fn write_ppm(path: &str, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut buf = Vec::with_capacity((w * h * 3 + 32) as usize);
    buf.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for px in rgba.chunks_exact(4) {
        buf.extend_from_slice(&px[..3]);
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&buf)
}

// ---------------------------------------------------------------------------
// Connect: stream live events for a bounded time, no GPU.
// ---------------------------------------------------------------------------

/// Native-only. `Mode::Connect` is the event-stream CLI diagnostic: it dials TCP
/// (which a page cannot do) and paces itself with `std::thread::sleep`, which **TRAPS**
/// on wasm32 — measured, executed in a wasm VM: `RuntimeError: unreachable`. Latent
/// rather than reachable today, because nothing in a browser can select this mode, but
/// gated rather than left as a trap one `Config` change away.
#[cfg(all(feature = "multiplayer", not(target_arch = "wasm32")))]
pub(crate) fn run_connect(_owned: lodestone_auth::Entitlement, config: Config) -> anyhow::Result<()> {
    println!(
        "connecting to {}:{} (protocol {}) for {}s…",
        config.host,
        config.port,
        config.protocol,
        config.connect_for.as_secs()
    );
    // `None`: `--connect` is the event-stream diagnostic. It has no `Sim`, no
    // renderer and no `World` of its own, so the client mints one — there is
    // nothing for it to be shared *with*.
    let net = NetClient::connect(
        config.host.clone(),
        config.explicit_port(),
        config.protocol,
        None,
    );
    let deadline = Instant::now() + config.connect_for;
    let mut seen = 0usize;

    while Instant::now() < deadline {
        for update in net.poll() {
            seen += 1;
            println!("[net] {update:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("streamed {seen} update(s); exiting");
    Ok(())
}

#[cfg(all(not(feature = "multiplayer"), not(target_arch = "wasm32")))]
pub(crate) fn run_connect(_owned: lodestone_auth::Entitlement, _config: Config) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "multiplayer is disabled in this build of the game; rebuild with the `multiplayer` Cargo feature"
    ))
}
