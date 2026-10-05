//! Pixel gates for the appearance-selected base sheets: a shulker's dye, a
//! rabbit's coat and a cat's breed, each driven through the real
//! [`RenderState::render`] path.
//!
//! Every claim draws the *same* model with exactly one input changed (the sheet
//! the extraction resolves) and compares the silhouette's mean colour, with
//! the identical-frame control and a missing-sheet control that must equal the
//! model's own default sheet.
//!
//! ```text
//! cargo test -p lodestone-shell --test entities appearance_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, EntityOverlay};
use lodestone::gpu::RenderState;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

fn draw(model: &str, sheet: Option<&'static str>) -> EntityDraw {
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
        variant_sheet: sheet,
        overlay_sheet: None,
        eyes_sheet: None,
        layers: Vec::new(),
        experience_orb_value: None,
        tnt_fuse: None,
        cape_sway: (0.0, 0.0, 0.0),
        baby: false,
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

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_shulkers_dye_changes_its_hue() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let blue = scene.shoot(&[draw("shulker", Some("entity/shulker/shulker_blue"))]);
    let blue_again = scene.shoot(&[draw("shulker", Some("entity/shulker/shulker_blue"))]);
    let red = scene.shoot(&[draw("shulker", Some("entity/shulker/shulker_red"))]);
    let mask = silhouette(&empty, &blue);
    assert!(mask.len() > 300, "the shulker must draw: {} px", mask.len());
    assert_eq!(blue, blue_again, "control: two identical frames must match exactly");
    let (b, r) = (mean(&blue, &mask), mean(&red, &mask));
    eprintln!("shulker bbox {:?}: blue {b:?}, red {r:?}", bbox(&mask));
    assert!(b[2] > b[0] + 10.0, "blue shulker leans blue: {b:?}");
    assert!(r[0] > r[2] + 10.0, "red shulker leans red: {r:?}");
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_rabbits_coat_changes_its_brightness() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let white = scene.shoot(&[draw("rabbit", Some("entity/rabbit/rabbit_white"))]);
    let black = scene.shoot(&[draw("rabbit", Some("entity/rabbit/rabbit_black"))]);
    let default = scene.shoot(&[draw("rabbit", None)]);
    let brown = scene.shoot(&[draw("rabbit", Some("entity/rabbit/rabbit_brown"))]);
    let mask = silhouette(&empty, &white);
    assert!(mask.len() > 100, "the rabbit must draw: {} px", mask.len());
    assert_eq!(default, brown, "control: no sheet draws the default brown coat");
    let luma = |m: [f32; 3]| m[0] + m[1] + m[2];
    let (w, k) = (luma(mean(&white, &mask)), luma(mean(&black, &mask)));
    eprintln!("rabbit bbox {:?}: white {w}, black {k}", bbox(&mask));
    assert!(w > k * 1.5, "white coat must be brighter than black: {w} vs {k}");
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_cats_breed_changes_its_brightness() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let white = scene.shoot(&[draw("cat", Some("entity/cat/cat_white"))]);
    let black = scene.shoot(&[draw("cat", Some("entity/cat/cat_all_black"))]);
    let mask = silhouette(&empty, &white);
    assert!(mask.len() > 100, "the cat must draw: {} px", mask.len());
    let luma = |m: [f32; 3]| m[0] + m[1] + m[2];
    let (w, k) = (luma(mean(&white, &mask)), luma(mean(&black, &mask)));
    eprintln!("cat bbox {:?}: white {w}, black {k}", bbox(&mask));
    assert!(w > k * 1.5, "white cat must be brighter than all-black: {w} vs {k}");
}

fn clothed(sheets: &[&'static str]) -> EntityDraw {
    EntityDraw {
        layers: sheets.iter().map(|&sheet| EntityOverlay { sheet, tint: [255; 3] }).collect(),
        ..draw("villager", None)
    }
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_villagers_profession_layers_recolour_its_clothes() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let bare = scene.shoot(&[clothed(&[])]);
    let bare_again = scene.shoot(&[clothed(&[])]);
    let farmer = scene.shoot(&[clothed(&["entity/villager/profession/farmer"])]);
    let missing = scene.shoot(&[clothed(&["entity/villager/profession/no_such"])]);
    let body = silhouette(&empty, &bare);
    assert!(body.len() > 500, "the villager must draw: {} px", body.len());
    assert_eq!(bare, bare_again, "control: two identical frames must match exactly");
    assert_eq!(bare, missing, "control: a layer with no art draws nothing");
    let changed: Vec<usize> = bare
        .chunks_exact(4)
        .zip(farmer.chunks_exact(4))
        .enumerate()
        .filter(|(_, (a, b))| (0..3).map(|c| i32::from(a[c]).abs_diff(i32::from(b[c]))).sum::<u32>() > 12)
        .map(|(i, _)| i)
        .collect();
    eprintln!("villager body {} px, farmer layer changed {} px, bbox {:?}", body.len(), changed.len(), bbox(&changed));
    assert!(changed.len() > body.len() / 10, "the profession clothes cover a real part of the body");
    // The robe is the inflated jacket part, transparent on the bare sheet, so
    // the clothed silhouette must grow past the bare one.
    let clothed_mask = silhouette(&empty, &farmer);
    assert!(
        clothed_mask.len() > body.len() + 200,
        "the robe adds silhouette: {} vs {}",
        clothed_mask.len(),
        body.len()
    );
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn an_angry_wolfs_sheet_differs_from_the_wild_one_in_a_small_region() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let wild = scene.shoot(&[draw("wolf", Some("entity/wolf/wolf"))]);
    let wild_again = scene.shoot(&[draw("wolf", Some("entity/wolf/wolf"))]);
    let angry = scene.shoot(&[draw("wolf", Some("entity/wolf/wolf_angry"))]);
    let mask = silhouette(&empty, &wild);
    assert!(mask.len() > 200, "the wolf must draw: {} px", mask.len());
    assert_eq!(wild, wild_again, "control: two identical frames must match exactly");
    let changed: Vec<usize> = wild
        .chunks_exact(4)
        .zip(angry.chunks_exact(4))
        .enumerate()
        .filter(|(_, (a, b))| (0..3).map(|c| i32::from(a[c]).abs_diff(i32::from(b[c]))).sum::<u32>() > 12)
        .map(|(i, _)| i)
        .collect();
    eprintln!("wolf body {} px, angry changed {} px, bbox {:?}", mask.len(), changed.len(), bbox(&changed));
    assert!(!changed.is_empty(), "the angry sheet must differ from the wild one");
    assert!(changed.len() * 3 < mask.len(), "angry only repaints the face: {} of {}", changed.len(), mask.len());
}
