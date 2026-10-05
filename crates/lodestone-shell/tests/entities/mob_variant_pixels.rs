//! Pixel gates for the three entity-variant channels the 04-entities scene
//! exercises: a fox's coat, an axolotl's colour and a wolf's dyed collar, each
//! driven through the real [`RenderState::render`] path.
//!
//! # Metrics and controls
//!
//! Every claim compares the *same* model drawn twice with exactly one input
//! changed.
//!
//! * Fox: the red sheet is orange and the snow sheet near white, so the mean
//!   silhouette colour must lose its red-over-blue lead. Control: red vs red
//!   moves nothing.
//! * Axolotl: gold has `r > b` and blue has `b > r` over the silhouette mean.
//! * Wolf collar: with a blue-tinted collar overlay, pixels that turn much bluer
//!   than the bare wolf must exist and sit inside the wolf's silhouette; with a
//!   red-tinted overlay no pixel turns bluer (the tint, not the layer, makes the
//!   blue); with no overlay nothing changes between two identical frames.
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test entities mob_variant_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, EntityOverlay};
use lodestone::gpu::RenderState;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

fn draw(
    model: &str,
    sheet: Option<&'static str>,
    overlay: Option<EntityOverlay>,
) -> EntityDraw {
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
        overlay_sheet: overlay,
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
fn a_snow_fox_draws_white_where_a_red_fox_draws_orange() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let red = scene.shoot(&[draw("fox", Some("entity/fox/fox"), None)]);
    let red_again = scene.shoot(&[draw("fox", Some("entity/fox/fox"), None)]);
    let snow = scene.shoot(&[draw("fox", Some("entity/fox/fox_snow"), None)]);

    let mask = silhouette(&empty, &red);
    assert!(mask.len() > 200, "the fox must draw: {} px", mask.len());
    assert_eq!(red, red_again, "control: two identical frames must match exactly");

    let r = mean(&red, &mask);
    let s = mean(&snow, &mask);
    eprintln!("fox bbox {:?}: red mean {r:?}, snow mean {s:?}", bbox(&mask));
    // The scene is dimly lit, so compare channel ratios rather than levels:
    // orange is several times redder than blue, a near-white coat is not.
    let ratio = |m: [f32; 3]| (m[0] + 1.0) / (m[2] + 1.0);
    assert!(ratio(r) > 3.0, "the red fox should be orange: {r:?}");
    assert!(ratio(s) < 2.0, "the snow fox should be near-white: {s:?}");
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn an_axolotl_colour_changes_its_hue() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let gold = scene.shoot(&[draw("axolotl", Some("entity/axolotl/axolotl_gold"), None)]);
    let gold_again = scene.shoot(&[draw("axolotl", Some("entity/axolotl/axolotl_gold"), None)]);
    let blue = scene.shoot(&[draw("axolotl", Some("entity/axolotl/axolotl_blue"), None)]);

    let mask = silhouette(&empty, &gold);
    assert!(mask.len() > 100, "the axolotl must draw: {} px", mask.len());
    assert_eq!(gold, gold_again, "control: two identical frames must match exactly");
    let g = mean(&gold, &mask);
    let b = mean(&blue, &mask);
    eprintln!("axolotl bbox {:?}: gold mean {g:?}, blue mean {b:?}", bbox(&mask));
    assert!(g[0] > g[2] + 30.0, "gold should lean red/yellow: {g:?}");
    assert!(b[2] > b[0] + 10.0, "blue should lean blue: {b:?}");
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_wolfs_collar_is_drawn_in_its_dye_colour() {
    let mut scene = Scene::new();
    let sheet = Some("entity/wolf/wolf_tame");
    let collar = |tint: [u8; 3]| {
        Some(EntityOverlay {
            sheet: "entity/wolf/wolf_collar",
            tint,
        })
    };
    let empty = scene.shoot(&[]);
    let bare = scene.shoot(&[draw("wolf", sheet, None)]);
    let bare_again = scene.shoot(&[draw("wolf", sheet, None)]);
    let blue = scene.shoot(&[draw("wolf", sheet, collar([0x3C, 0x44, 0xAA]))]);
    let red = scene.shoot(&[draw("wolf", sheet, collar([0xB0, 0x2E, 0x26]))]);

    let mask = silhouette(&empty, &bare);
    assert!(mask.len() > 300, "the wolf must draw: {} px", mask.len());
    assert_eq!(bare, bare_again, "control: two identical frames must match exactly");

    let bluer = |frame: &[u8]| -> Vec<usize> {
        bare.chunks_exact(4)
            .zip(frame.chunks_exact(4))
            .enumerate()
            .filter(|(_, (b, a))| {
                (i32::from(a[2]) - i32::from(a[0])) - (i32::from(b[2]) - i32::from(b[0])) > 10
            })
            .map(|(i, _)| i)
            .collect()
    };
    let blue_px = bluer(&blue);
    let red_px = bluer(&red);
    let silhouette_box = bbox(&mask).expect("wolf drew");
    eprintln!(
        "wolf bbox {silhouette_box:?}: blue-collar bluer px {} bbox {:?}; red-collar bluer px {}",
        blue_px.len(),
        bbox(&blue_px),
        red_px.len()
    );
    assert!(
        blue_px.len() > 15,
        "a blue collar must turn a patch of the wolf blue; only {} px did",
        blue_px.len()
    );
    assert_eq!(
        red_px.len(),
        0,
        "control: a red collar must not turn anything blue, so the blue is the dye's doing"
    );
    let (x0, y0, x1, y1) = bbox(&blue_px).expect("count > 0");
    let (sx0, sy0, sx1, sy1) = silhouette_box;
    assert!(
        x0 >= sx0 && y0 >= sy0 && x1 <= sx1 && y1 <= sy1,
        "collar box ({x0},{y0})-({x1},{y1}) must lie inside the wolf ({sx0},{sy0})-({sx1},{sy1})"
    );
}
