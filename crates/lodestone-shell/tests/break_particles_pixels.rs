//! Pixel gate: breaking a block must put debris on the screen.
//!
//! `lodestone-particle` proves the *simulation* (vanilla RNG, velocities, decay)
//! and `particles::tests` proves the *extraction* (sprite rects, the light term).
//! Neither can observe whether a single instance ever reaches a fragment shader,
//! and a crate's own suite is a closed loop — the whole chain
//!
//! ```text
//! Sim::break_block → Particles::destroy_block → tick → extract
//!   → RenderState::prepare_particles → draw → GPU pixels
//! ```
//!
//! can be green end to end while `particles_drawn` counts happily over a frame
//! that looks identical to one with no particles in it.
//!
//! So this asserts on **pixels that changed**, against a paired control. The
//! control is deliberately as close to the subject as it can be: the *same*
//! `Sim`, after the *same* break and remesh, at the *same* camera, differing
//! only in that `prepare_particles` is handed an empty slice. Every other
//! candidate control is weaker — not breaking the block would leave the remesh
//! as an uncontrolled difference, and "the frame changed after a break" is
//! satisfied by the removed block alone.
//!
//! Run it explicitly (it needs a GPU adapter, and per §12.52 it **fails** rather
//! than skips when one is missing — a skip here reads exactly like a pass):
//!
//! ```text
//! cargo test -p lodestone-shell --test break_particles_pixels -- --ignored --nocapture
//! ```

use lodestone::config::{Config, Mode};
use lodestone::gpu::RenderState;
use lodestone::sim::Sim;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 640;
const H: u32 = 480;

/// Build the offline demo world and settle the player onto the ground, so the
/// camera sits at a sane height and the view ray hits real terrain.
fn settled_sim() -> Sim {
    let mut config = Config::default();
    config.mode = Mode::Headless;
    let mut sim = Sim::new(config);
    for _ in 0..40 {
        sim.step(1.0 / 20.0);
    }
    sim
}

/// Aim straight down at the block under the player's feet. Debris spawns inside
/// that block's volume, so a downward view guarantees the particles are on
/// screen rather than behind the camera.
fn look_down(sim: &mut Sim) {
    sim.player_mut(|p| p.pitch = 89.0);
    sim.update_target(W as f32 / H as f32);
}

/// Render one frame through the exact calls the live frame loop makes, and read
/// the texels back.
fn render_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    render: &mut RenderState,
    sim: &mut Sim,
    camera: &Camera,
    with_particles: bool,
) -> Vec<u8> {
    let frame = target.acquire().expect("headless acquire");
    let entity_draws = sim.entity_draws();
    let instances: &[_] = if with_particles {
        &sim.particle_instances()
    } else {
        &[]
    };
    render.prepare_particles(device, queue, instances, camera);
    render.render(device, queue, frame.view(), camera, None, &entity_draws);
    target.read_texels(device, queue)
}

/// Whether two texels differ by more than sensor noise. Both frames come from a
/// deterministic renderer over an identical scene, so any difference at all is a
/// real drawn difference rather than dithering.
fn differs(p: &[u8], q: &[u8]) -> bool {
    let d = (i32::from(p[0]) - i32::from(q[0])).abs()
        + (i32::from(p[1]) - i32::from(q[1])).abs()
        + (i32::from(p[2]) - i32::from(q[2])).abs();
    d > 12
}

/// Changed pixels, split by **location** rather than totalled.
///
/// A single count cannot distinguish "debris around the broken block" from "the
/// blend state leaked and tinted the whole frame" — both give a large number.
/// The debris spawns inside one block directly under the camera, so it must land
/// in the centre and must *not* reach the frame edges. Returns
/// `(centre, border)`, where centre is the middle third in both axes and border
/// is the outer eighth.
fn changed_by_region(a: &[u8], b: &[u8]) -> (usize, usize) {
    let (mut centre, mut border) = (0usize, 0usize);
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if !differs(&a[i..i + 4], &b[i..i + 4]) {
                continue;
            }
            let in_centre = x > W / 3 && x < 2 * W / 3 && y > H / 3 && y < 2 * H / 3;
            let in_border = x < W / 8 || x >= W - W / 8 || y < H / 8 || y >= H - H / 8;
            if in_centre {
                centre += 1;
            }
            if in_border {
                border += 1;
            }
        }
    }
    (centre, border)
}

#[test]
#[ignore = "requires a GPU adapter"]
fn breaking_a_block_puts_debris_on_screen() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in but no adapter is available; do not treat this as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;

    let mut sim = settled_sim();
    look_down(&mut sim);
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut render = RenderState::new(device, queue, format, W, H, sim.vanilla_atlas());
    for m in &sim.drain_all_meshes() {
        render.upload_section(device, m.key, &m.mesh);
    }

    assert!(
        sim.break_block(),
        "the settled player must be looking at a breakable block; a false here \
         is a broken fixture, not a renderer result"
    );
    // Upload the post-break mesh so both frames below see identical terrain.
    for m in &sim.drain_all_meshes() {
        render.upload_section(device, m.key, &m.mesh);
    }
    // Let the debris move off the block centre so it is not hidden inside the
    // neighbouring geometry.
    for _ in 0..2 {
        sim.step(1.0 / 20.0);
    }

    let camera = sim.camera(W as f32 / H as f32);
    let frame = sim.extract_particles(&camera);

    let control_px = render_frame(
        device, queue, &mut target, &mut render, &mut sim, &camera, false,
    );
    let subject_px = render_frame(
        device, queue, &mut target, &mut render, &mut sim, &camera, true,
    );

    let (centre_px, border_px) = changed_by_region(&control_px, &subject_px);

    eprintln!("=== break-particle pixel gate ===");
    eprintln!("particles alive      = {}", frame.alive);
    eprintln!("particles drawn      = {}", frame.drawn);
    eprintln!("particles unresolved = {}", frame.unresolved);
    eprintln!("centre px changed    = {centre_px}");
    eprintln!("border px changed    = {border_px}");

    assert!(
        frame.alive > 0,
        "breaking a block must emit particles; zero alive means the emitter \
         never ran, so the pixel assertion below would be vacuous"
    );
    assert_eq!(
        frame.unresolved, 0,
        "every terrain particle must resolve to a sprite in the demo palette; \
         unresolved particles simulate correctly and draw nothing"
    );
    assert!(
        centre_px > 200,
        "debris reached only {centre_px} centre pixels — the particle draw is \
         not reaching the framebuffer (a frame counter ticks fine over a blank \
         pass)"
    );
    assert_eq!(
        border_px, 0,
        "debris from one block under the camera must not touch the frame edges; \
         a non-zero border count means something is filling the screen rather \
         than drawing 64 small billboards"
    );
}
