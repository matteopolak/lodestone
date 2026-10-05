//! Pixel gate for the glowing-eyes layer: a spider's eyes draw through the
//! real [`RenderState::render`] path as a self-lit, alpha-blended layer.
//!
//! # Metrics and controls
//!
//! The same spider is drawn with and without `eyes_sheet`, and only that input
//! changes.
//!
//! * Pixels that changed must exist, sit inside the spider's silhouette, occupy
//!   a small part of it (the eyes, not a repaint of the body) and be red-led.
//! * Control: an eyes sheet that names no loaded texture draws nothing, so the
//!   frame equals the bare spider's.
//! * Control: two identical frames match exactly.
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test entities eyes_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::RenderState;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

fn draw(model: &str, eyes: Option<&'static str>) -> EntityDraw {
    EntityDraw {
        hurt: false,
        id: 1,
        type_path: std::sync::Arc::from(model),
        named_cosmetics: Default::default(),
        item: None,
        item_model: None,
        item_skin: None,
        main_arm_left: false,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_skin: Vec::new(),
        equipment_trim: Vec::new(),
        feet: glam::Vec3::new(0.0, 0.0, 3.0),
        yaw: 90.0,
        head_yaw: 90.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        wool: None,
        block_state: None,
        item_frame_rotation: 0,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        name_tag: None,
        item_use: None,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
        death_time: 0.0,
        on_fire: false,
        invisible: false,
        armor_stand: None,
        player_skin: None,
        variant_sheet: None,
        overlay_sheet: None,
        eyes_sheet: eyes,
        layers: Vec::new(),
        experience_orb_value: None,
        tnt_fuse: None,
        cape_sway: (0.0, 0.0, 0.0),
        baby: false,
        painting: None,
        firework: None,
        projectile_owner: None,
    }
}

struct Scene {
    ctx: GpuContext,
    target: HeadlessTarget,
    state: RenderState,
    camera: Camera,
}

impl Scene {
    fn new() -> Self {
        let ctx = GpuContext::new_headless_blocking().expect(
            "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU — do NOT treat a skip as a pass",
        );
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let target = HeadlessTarget::new(ctx.device(), W, H, format);
        let state = RenderState::new_headless(ctx.device(), ctx.queue(), format, W, H, None);
        let camera = Camera {
            position: glam::Vec3::new(0.0, 0.6, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 60.0,
            aspect: W as f32 / H as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };
        Self { ctx, target, state, camera }
    }

    fn shoot(&mut self, draws: &[EntityDraw]) -> Vec<u8> {
        let (device, queue) = (self.ctx.device(), self.ctx.queue());
        let frame = self.target.acquire().expect("headless acquire");
        self.state.render(device, queue, frame.view(), &self.camera, None, draws);
        self.target.read_texels(device, queue)
    }
}

fn silhouette(empty: &[u8], frame: &[u8]) -> Vec<usize> {
    empty
        .chunks_exact(4)
        .zip(frame.chunks_exact(4))
        .enumerate()
        .filter(|(_, (e, f))| {
            (0..3).map(|c| i32::from(e[c]) - i32::from(f[c])).map(i32::abs).sum::<i32>() > 40
        })
        .map(|(i, _)| i)
        .collect()
}

/// Mean `[r, g, b]` over the pixels `mask` names.
fn mean(frame: &[u8], mask: &[usize]) -> [f32; 3] {
    let mut sum = [0.0_f32; 3];
    for &i in mask {
        for c in 0..3 {
            sum[c] += f32::from(frame[i * 4 + c]);
        }
    }
    sum.map(|s| s / mask.len().max(1) as f32)
}

fn bbox(indices: &[usize]) -> Option<(u32, u32, u32, u32)> {
    indices.iter().fold(None, |acc, &i| {
        let (x, y) = (i as u32 % W, i as u32 / W);
        Some(match acc {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        })
    })
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_spiders_eyes_glow_red_over_a_small_part_of_its_body() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let bare = scene.shoot(&[draw("spider", None)]);
    let bare_again = scene.shoot(&[draw("spider", None)]);
    let eyed = scene.shoot(&[draw("spider", Some("entity/spider/spider_eyes"))]);
    let missing = scene.shoot(&[draw("spider", Some("entity/spider/no_such_eyes"))]);

    let body = silhouette(&empty, &bare);
    assert!(body.len() > 300, "the spider must draw: {} px", body.len());
    assert_eq!(bare, bare_again, "control: two identical frames must match exactly");
    assert_eq!(bare, missing, "control: an eyes sheet with no art draws nothing");

    let changed: Vec<usize> = bare
        .chunks_exact(4)
        .zip(eyed.chunks_exact(4))
        .enumerate()
        .filter(|(_, (a, b))| (0..3).any(|c| a[c] != b[c]))
        .map(|(i, _)| i)
        .collect();
    eprintln!("spider body {} px, eyes changed {} px, bbox {:?}", body.len(), changed.len(), bbox(&changed));
    assert!(changed.len() >= 4, "the eyes must change pixels: {}", changed.len());
    assert!(
        changed.len() * 10 < body.len(),
        "the eyes are a small part of the body: {} of {}",
        changed.len(),
        body.len()
    );
    let inside = changed.iter().filter(|i| body.binary_search(i).is_ok()).count();
    assert!(inside * 10 >= changed.len() * 9, "the eyes sit on the body: {inside}/{}", changed.len());
    let m = mean(&eyed, &changed);
    eprintln!("eye mean {m:?}");
    assert!(m[0] > m[1] * 2.0 && m[0] > m[2] * 2.0, "spider eyes are red: {m:?}");
}
