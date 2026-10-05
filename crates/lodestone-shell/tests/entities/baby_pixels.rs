//! Pixel gates for the dedicated baby rigs through the real
//! [`RenderState::render`] path: a baby draws at its own proportions (not the adult
//! mesh at half size) and binds its own sheet.
//!
//! Expected heights are arithmetic on the baby rig's authored extents, taken from the
//! client's own model definitions (feet at texel 24): a baby pig stands 10 texels
//! against the adult's 16, a baby zombie about 15.15 against 32.
//!
//! ```text
//! cargo test -p lodestone-shell --test entities baby_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, EntityOverlay};
use lodestone::gpu::RenderState;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

fn draw(model: &str, baby: bool, sheet: Option<&'static str>) -> EntityDraw {
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
        scale: if baby { 0.5 } else { 1.0 },
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
        variant_sheet: sheet,
        overlay_sheet: None,
        eyes_sheet: None,
        layers: Vec::new(),
        experience_orb_value: None,
        tnt_fuse: None,
        cape_sway: (0.0, 0.0, 0.0),
        baby,
        gear: Vec::new(),
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


fn height(empty: &[u8], frame: &[u8]) -> f32 {
    let mask = silhouette(empty, frame);
    assert!(mask.len() > 100, "must draw: {} px", mask.len());
    let (_, y0, _, y1) = bbox(&mask).unwrap();
    (y1 - y0 + 1) as f32
}

/// Adult over baby silhouette height at the same distance, against `expected`
/// (adult texels over baby texels), within 12% for perspective and the head tilt.
fn assert_height_ratio(model: &str, expected: f32) {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let adult = height(&empty, &scene.shoot(&[draw(model, false, None)]));
    let baby = height(&empty, &scene.shoot(&[draw(model, true, None)]));
    let ratio = baby / adult;
    eprintln!("{model}: adult {adult}px, baby {baby}px, ratio {ratio:.3}, expected {expected:.3}");
    assert!(
        (ratio - expected).abs() < expected * 0.12,
        "{model}: baby/adult height {ratio:.3}, expected about {expected:.3}"
    );
    // The half-scale adult mesh the renderer used before is 0.5 for every model;
    // these expectations are distinguishable from it for the pig only, which the
    // pig case below pins.
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_baby_pig_has_baby_proportions_not_half_the_adult() {
    // 10 texels over 16 is 0.625; the old half-scale adult would read 0.5.
    assert_height_ratio("pig", 10.0 / 16.0);
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_baby_zombie_is_a_big_headed_toddler() {
    // Head top at texel 8.85 over feet at 24 is 15.15; the adult is 32.
    assert_height_ratio("zombie", 15.15 / 32.0);
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_baby_binds_its_own_sheet_and_the_adult_sheet_differs() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let own = scene.shoot(&[draw("pig", true, None)]);
    let own_again = scene.shoot(&[draw("pig", true, None)]);
    let baby_sheet = scene.shoot(&[draw("pig", true, Some("entity/pig/pig_temperate_baby"))]);
    let adult_sheet = scene.shoot(&[draw("pig", true, Some("entity/pig/pig_temperate"))]);
    assert_eq!(own, own_again, "control: two identical frames must match exactly");
    assert_eq!(own, baby_sheet, "control: no sheet draws the baby rig's default sheet");
    assert_ne!(own, adult_sheet, "the adult sheet unwraps differently on the baby rig");
    assert!(silhouette(&empty, &own).len() > 100);
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_baby_sheeps_wool_layer_recolours_the_body_and_matches_the_dye() {
    let woolly = |tint: [u8; 3]| EntityDraw {
        layers: vec![EntityOverlay { sheet: "entity/sheep/sheep_wool_baby", tint }],
        ..draw("sheep", true, None)
    };
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let bare = scene.shoot(&[draw("sheep", true, None)]);
    let white = scene.shoot(&[woolly([255; 3])]);
    let white_again = scene.shoot(&[woolly([255; 3])]);
    let red = scene.shoot(&[woolly([176, 46, 38])]);
    assert_eq!(white, white_again, "control: two identical frames must match exactly");
    assert_ne!(bare, red, "a dyed wool layer must change the body");
    let mask = silhouette(&empty, &white);
    assert!(mask.len() > 100, "the sheep must draw: {} px", mask.len());
    let (w, r) = (mean(&white, &mask), mean(&red, &mask));
    eprintln!("baby sheep: white wool {w:?}, red wool {r:?}");
    assert!(r[0] > r[1] + 8.0 && r[0] > r[2] + 8.0, "red-dyed wool leans red: {r:?}");
    assert!((w[0] - w[1]).abs() < 25.0, "undyed wool is neutral: {w:?}");
}
