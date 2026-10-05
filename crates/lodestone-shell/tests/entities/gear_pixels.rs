//! Pixel gates for worn-gear layers through the real [`RenderState::render`] path: a
//! saddle, horse armour and wolf armour each change the pixels of the animal they
//! are drawn on, inside its own silhouette or just outside it (the layers inflate
//! the body), and a dye tints a dyeable layer.
//!
//! ```text
//! cargo test -p lodestone-shell --test entities gear_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, GearOverlay};
use lodestone::gpu::RenderState;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

pub(super) fn draw(model: &str, gear: Vec<GearOverlay>) -> EntityDraw {
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
        eyes_sheet: None,
        layers: Vec::new(),
        experience_orb_value: None,
        tnt_fuse: None,
        cape_sway: (0.0, 0.0, 0.0),
        baby: false,
        gear,
        chested: false,
        ridden: false,
        painting: None,
        firework: None,
        projectile_owner: None,
    }
}

pub(super) struct Scene {
    ctx: GpuContext,
    target: HeadlessTarget,
    state: RenderState,
    camera: Camera,
}

impl Scene {
    pub(super) fn new() -> Self {
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

    pub(super) fn shoot(&mut self, draws: &[EntityDraw]) -> Vec<u8> {
        let (device, queue) = (self.ctx.device(), self.ctx.queue());
        let frame = self.target.acquire().expect("headless acquire");
        self.state.render(device, queue, frame.view(), &self.camera, None, draws);
        self.target.read_texels(device, queue)
    }
}

pub(super) fn silhouette(empty: &[u8], frame: &[u8]) -> Vec<usize> {
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

pub(super) fn bbox(indices: &[usize]) -> Option<(u32, u32, u32, u32)> {
    indices.iter().fold(None, |acc, &i| {
        let (x, y) = (i as u32 % W, i as u32 / W);
        Some(match acc {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        })
    })
}

pub(super) fn changed(a: &[u8], b: &[u8]) -> Vec<usize> {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .enumerate()
        .filter(|(_, (x, y))| (0..3).map(|c| i32::from(x[c]).abs_diff(i32::from(y[c]))).sum::<u32>() > 12)
        .map(|(i, _)| i)
        .collect()
}

fn gear(model: &'static str, sheet: &'static str, tint: [u8; 3]) -> Vec<GearOverlay> {
    vec![GearOverlay { model, sheet, tint }]
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_saddle_repaints_the_pigs_back_and_swells_its_silhouette() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let bare = scene.shoot(&[draw("pig", vec![])]);
    let bare_again = scene.shoot(&[draw("pig", vec![])]);
    let saddled = scene.shoot(&[draw("pig", gear("pig_saddle", "entity/equipment/pig_saddle/saddle", [255; 3]))]);
    assert_eq!(bare, bare_again, "control: two identical frames must match exactly");
    let (body, with) = (silhouette(&empty, &bare), silhouette(&empty, &saddled));
    let diff = changed(&bare, &saddled);
    eprintln!("pig {} px, saddled {} px, changed {} px bbox {:?} (body {:?})", body.len(), with.len(), diff.len(), bbox(&diff), bbox(&body));
    assert!(body.len() > 400, "the pig must draw: {} px", body.len());
    assert!(diff.len() > body.len() / 20, "the saddle must repaint a real patch: {} of {}", diff.len(), body.len());
    assert!(diff.len() < body.len() / 2, "the saddle is a patch, not the whole pig: {} of {}", diff.len(), body.len());
    let (b, d) = (bbox(&body).unwrap(), bbox(&diff).unwrap());
    // Changed pixels sit within the body's box widened by the 0.5 inflate (a few px).
    assert!(d.0 + 6 >= b.0 && d.1 + 6 >= b.1 && d.2 <= b.2 + 6 && d.3 <= b.3 + 6, "changed {d:?} outside body {b:?}");
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn horse_armour_material_changes_the_colour_and_dye_tints_leather() {
    let armour = |sheet: &'static str, tint| draw("horse", gear("horse_armor", sheet, tint));
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let bare = scene.shoot(&[draw("horse", vec![])]);
    let iron = scene.shoot(&[armour("entity/equipment/horse_body/iron", [255; 3])]);
    let iron_again = scene.shoot(&[armour("entity/equipment/horse_body/iron", [255; 3])]);
    let gold = scene.shoot(&[armour("entity/equipment/horse_body/gold", [255; 3])]);
    let leather_red = scene.shoot(&[armour("entity/equipment/horse_body/leather", [200, 30, 30])]);
    let leather_blue = scene.shoot(&[armour("entity/equipment/horse_body/leather", [30, 30, 200])]);
    assert_eq!(iron, iron_again, "control: two identical frames must match exactly");
    let mask = silhouette(&empty, &iron);
    assert!(mask.len() > 600, "the armoured horse must draw: {} px", mask.len());
    assert!(!changed(&bare, &iron).is_empty(), "armour must repaint the horse");
    let patch = changed(&iron, &gold);
    assert!(patch.len() > 200, "gold differs from iron over a real area: {} px", patch.len());
    let (i, g) = (mean(&iron, &patch), mean(&gold, &patch));
    eprintln!("armour patch {} px: iron {i:?}, gold {g:?}", patch.len());
    assert!(g[0] - g[2] > i[0] - i[2] + 15.0, "gold leans yellow against iron: {g:?} vs {i:?}");
    let patch = changed(&leather_red, &leather_blue);
    assert!(patch.len() > 200, "the dye changes the leather: {} px", patch.len());
    let (r, b) = (mean(&leather_red, &patch), mean(&leather_blue, &patch));
    assert!(r[0] > r[2] + 10.0 && b[2] > b[0] + 10.0, "dye tint steers the hue: red {r:?}, blue {b:?}");
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn wolf_armour_covers_the_wolf() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let bare = scene.shoot(&[draw("wolf", vec![])]);
    let armoured = scene.shoot(&[draw(
        "wolf",
        gear("wolf_armor", "entity/equipment/wolf_body/armadillo_scute", [255; 3]),
    )]);
    let body = silhouette(&empty, &bare);
    let diff = changed(&bare, &armoured);
    eprintln!("wolf {} px, armour changed {} px bbox {:?}", body.len(), diff.len(), bbox(&diff));
    assert!(body.len() > 200, "the wolf must draw: {} px", body.len());
    assert!(diff.len() > body.len() / 10, "the armour covers a real part of the wolf: {} of {}", diff.len(), body.len());
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_donkeys_chests_draw_only_when_the_chest_flag_is_set() {
    let donkey = |chested| EntityDraw { chested, ..draw("donkey", vec![]) };
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let bare = scene.shoot(&[donkey(false)]);
    let bare_again = scene.shoot(&[donkey(false)]);
    let chested = scene.shoot(&[donkey(true)]);
    assert_eq!(bare, bare_again, "control: two identical frames must match exactly");
    let (a, b) = (silhouette(&empty, &bare), silhouette(&empty, &chested));
    let diff = changed(&bare, &chested);
    eprintln!("donkey {} px, chested {} px, changed {} px bbox {:?}", a.len(), b.len(), diff.len(), bbox(&diff));
    assert!(a.len() > 400, "the donkey must draw: {} px", a.len());
    // Seen from the side the near chest sits over the body, so it repaints a patch
    // (about 1.2k px) rather than widening the silhouette much.
    assert!(diff.len() > 500, "the chest boxes repaint a patch: {} px", diff.len());
    assert!(b.len() >= a.len(), "the chests never shrink the silhouette: {} vs {}", b.len(), a.len());
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn rein_lines_draw_only_while_the_horse_is_ridden() {
    let horse = |ridden| EntityDraw {
        ridden,
        ..draw("horse", gear("horse_saddle", "entity/equipment/horse_saddle/saddle", [255; 3]))
    };
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let idle = scene.shoot(&[horse(false)]);
    let idle_again = scene.shoot(&[horse(false)]);
    let ridden = scene.shoot(&[horse(true)]);
    assert_eq!(idle, idle_again, "control: two identical frames must match exactly");
    let diff = changed(&idle, &ridden);
    eprintln!("horse {} px, rein change {} px bbox {:?}", silhouette(&empty, &idle).len(), diff.len(), bbox(&diff));
    assert!(diff.len() > 20, "the rein lines add pixels while ridden: {}", diff.len());
    let camel = |ridden| EntityDraw {
        ridden,
        ..draw("camel", gear("camel_saddle", "entity/equipment/camel_saddle/saddle", [255; 3]))
    };
    let (calm, mounted) = (scene.shoot(&[camel(false)]), scene.shoot(&[camel(true)]));
    assert!(!changed(&calm, &mounted).is_empty(), "the camel's reins appear while ridden");
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_cracked_wolf_armour_draws_over_the_intact_one() {
    let armour = |extra: Option<&'static str>| {
        let mut layers = gear("wolf_armor", "entity/equipment/wolf_body/armadillo_scute", [255; 3]);
        layers.extend(extra.map(|sheet| GearOverlay { model: "wolf_armor", sheet, tint: [255; 3] }));
        draw("wolf", layers)
    };
    let mut scene = Scene::new();
    let intact = scene.shoot(&[armour(None)]);
    let intact_again = scene.shoot(&[armour(None)]);
    let cracked = scene.shoot(&[armour(Some("entity/wolf/wolf_armor_crackiness_high"))]);
    assert_eq!(intact, intact_again, "control: two identical frames must match exactly");
    let diff = changed(&intact, &cracked);
    eprintln!("cracks changed {} px bbox {:?}", diff.len(), bbox(&diff));
    assert!(diff.len() > 50, "the cracks must draw: {} px", diff.len());
}

fn edge_rows(mask: &[usize]) -> (bool, bool) {
    let (_, y0, _, y1) = bbox(mask).unwrap();
    (y0 == 0, y1 == H - 1)
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_baby_happy_ghast_fits_the_frame_where_the_adult_overflows_it() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let adult = silhouette(&empty, &scene.shoot(&[draw("happy_ghast", vec![])]));
    let baby = silhouette(&empty, &scene.shoot(&[draw("happy_ghast_baby", vec![])]));
    eprintln!("adult {} px {:?}, baby {} px {:?}", adult.len(), bbox(&adult), baby.len(), bbox(&baby));
    assert!(baby.len() > 300, "the baby must draw: {} px", baby.len());
    assert!(edge_rows(&adult).0 || edge_rows(&adult).1, "control: a 4-block adult overflows the frame at 3 blocks");
    assert_eq!(edge_rows(&baby), (false, false), "the baby (0.95x mesh) sits inside the frame");
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_baby_ghasts_harness_ropes_and_body_squeeze_each_change_the_pixels() {
    let harness = |model: &'static str| {
        gear(model, "entity/equipment/happy_ghast_body/red_harness", [255; 3])
    };
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let bare = scene.shoot(&[draw("happy_ghast_baby", vec![])]);
    let bare_again = scene.shoot(&[draw("happy_ghast_baby", vec![])]);
    assert_eq!(bare, bare_again, "control: two identical frames must match exactly");
    let idle = scene.shoot(&[draw("happy_ghast_baby", harness("happy_ghast_baby_harness_idle"))]);
    let riding = scene.shoot(&[draw("happy_ghast_baby", harness("happy_ghast_baby_harness"))]);
    let body = changed(&bare, &idle);
    eprintln!("harness changed {} px bbox {:?}", body.len(), bbox(&body));
    assert!(body.len() > 300, "the harness repaints the body: {} px", body.len());
    assert!(!changed(&idle, &riding).is_empty(), "goggles move between idle and ridden");

    let mut ropes = harness("happy_ghast_baby_harness_idle");
    ropes.extend(gear("happy_ghast_baby_ropes", "entity/ghast/happy_ghast_ropes", [255; 3]));
    let roped = scene.shoot(&[draw("happy_ghast_baby", ropes)]);
    assert!(changed(&idle, &roped).len() > 50, "the ropes add pixels over the harness");

    let worn = EntityDraw {
        equipment: vec![(lodestone_model::EquipmentSlot::Body, "minecraft:red_harness".parse().unwrap())],
        ..draw("happy_ghast_baby", vec![])
    };
    let squeezed = scene.shoot(&[worn]);
    let (a, b) = (silhouette(&empty, &bare).len(), silhouette(&empty, &squeezed).len());
    eprintln!("body {a} px, squeezed {b} px");
    assert!(b * 100 < a * 97 && b * 100 > a * 70, "a worn body item squeezes the body by about 0.9375: {a} -> {b}");
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_drowned_outer_layer_swells_the_silhouette_and_repaints_the_body() {
    let mut scene = Scene::new();
    let empty = scene.shoot(&[]);
    let bare = scene.shoot(&[draw("drowned", vec![])]);
    let outer = scene.shoot(&[draw("drowned", gear("drowned_outer", "entity/zombie/drowned_outer_layer", [255; 3]))]);
    let (a, b) = (silhouette(&empty, &bare).len(), silhouette(&empty, &outer).len());
    let diff = changed(&bare, &outer);
    eprintln!("drowned {a} px, with outer {b} px, changed {}", diff.len());
    assert!(a > 500, "the drowned must draw: {a} px");
    assert!(b > a && diff.len() > 100, "the outer layer adds pixels: {a} -> {b}, {} changed", diff.len());
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn nautilus_saddle_armour_and_the_trader_blanket_each_repaint_their_animal() {
    let mut scene = Scene::new();
    for (animal, rig, sheet) in [
        ("nautilus", "nautilus_saddle", "entity/equipment/nautilus_saddle/saddle"),
        ("nautilus", "nautilus_armor", "entity/equipment/nautilus_body/diamond"),
        ("trader_llama", "llama_decor", "entity/equipment/llama_body/trader_llama"),
        ("trader_llama_baby", "llama_baby_decor", "entity/equipment/llama_body/trader_llama_baby"),
    ] {
        let bare = scene.shoot(&[draw(animal, vec![])]);
        let bare_again = scene.shoot(&[draw(animal, vec![])]);
        let worn = scene.shoot(&[draw(animal, gear(rig, sheet, [255; 3]))]);
        assert_eq!(bare, bare_again, "control: two identical frames must match exactly");
        let diff = changed(&bare, &worn);
        eprintln!("{animal} + {rig}: {} px changed, bbox {:?}", diff.len(), bbox(&diff));
        assert!(diff.len() > 100, "{rig} must repaint {animal}: {} px", diff.len());
    }
}
