//! Pixel gate: the icons on a **held** filled map draw their sprite where the
//! map's own rules put it, in either hand.
//!
//! # What is measured
//!
//! Each shot is sky plus the hand pass. The map picture is a solid opaque grid,
//! so a render with no icons gives the picture's screen box. Adding icons and
//! diffing against that shot isolates the icons, and the diff's bounding box is
//! compared with a box predicted from the sprite's own PNG: the opaque texels'
//! corners, put through the reference placement rules written out again here
//! (wire units are half pixels from the map centre; the sprite is 8x8 pixels,
//! its top row at the lower edge, shifted half a pixel left and down; rotation
//! is clockwise sixteenths) and scaled into the picture's screen box. The
//! stances are upright and face the camera, so that scaling is affine.
//!
//! # Per hand
//!
//! The source answers by map id, so the left hand's icons, the right hand's
//! icons and an icon-free third map are told apart by which stack holds which
//! id; a held map showing the lowest-numbered map would fail them.
//!
//! ```text
//! cargo test -p lodestone-shell --test gpu held_map_decoration_pixels -- --ignored --nocapture
//! ```

use std::sync::Arc;

use lodestone::gpu::{FirstPersonHandsFrame, HandFrame, MainHandItem, MapPicture, RenderState, SKY_COLOR};
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_game::maps::MapId;
use lodestone_model::MapDecoration;
use lodestone_model::ids::Identifier;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

#[path = "../support/map_decoration_probe.rs"]
mod probe;
use probe::{Bounds, Target, assert_box, assert_orientation, changed, expected_map_rect, sprite, to_screen};

const TARGET: Target = Target { w: W, h: H };

/// Non-sky pixels in `keep`, the picture's screen box.
fn picture_box(pixels: &[u8], keep: impl Fn(u32, u32) -> bool) -> Option<Bounds> {
    let sky = SKY_COLOR.map(|c| (c * 255.0).round() as u8);
    let reference: Vec<u8> = sky.iter().copied().chain([255]).cycle().take((W * H * 4) as usize).collect();
    changed(TARGET, pixels, &reference, keep)
}

const W: u32 = 448;
const H: u32 = 256;

const LEFT_MAP: i32 = 31;
const RIGHT_MAP: i32 = 32;
const PLAIN_MAP: i32 = 33;

fn camera(pitch: f32) -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

fn held(id: &str, map_id: Option<i32>) -> MainHandItem {
    MainHandItem {
        map_id,
        item: id.parse::<ResourceLocation>().expect("valid item id"),
        foil: false,
        custom_model_data: None,
        dyed_color: None,
        potion_color: None,
        banner_patterns: Vec::new(),
        base_color: None,
        skin: None,
    }
}

fn marker(kind: &str, x: i8, y: i8, rotation: u8) -> MapDecoration {
    MapDecoration {
        kind: kind.parse::<Identifier>().expect("valid decoration type"),
        x,
        y,
        rotation,
        name: None,
    }
}

struct Rig {
    ctx: GpuContext,
    target: HeadlessTarget,
    state: RenderState,
}

impl Rig {
    fn new() -> Self {
        let ctx = GpuContext::new_headless_blocking().expect(
            "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU — do NOT treat a skip as a pass",
        );
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let target = HeadlessTarget::new(ctx.device(), W, H, format);
        let resources = BlockResources::load(true);
        let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
            panic!(
                "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS. Banner: {:?}",
                resources.banner
            )
        });
        let mut state = RenderState::new_headless(ctx.device(), ctx.queue(), format, W, H, Some(atlas.as_ref()));
        state.set_entity_light_source(|_| Some(lodestone_render::ENTITY_FULLBRIGHT));
        Self { ctx, target, state }
    }

    /// Installs a source that serves `maps` (id, icons) as solid grass pictures.
    fn serve(&mut self, maps: Vec<(i32, Vec<MapDecoration>)>) {
        let colors = Arc::new(vec![0b0000_0110u8; 128 * 128]);
        self.state.set_map_source(move |id, _| {
            let id = id?;
            let (_, icons) = maps.iter().find(|(map, _)| *map == id)?;
            Some(
                MapPicture::new(MapId::new(id)?, 0, Arc::clone(&colors))
                    .with_decorations(u64::try_from(icons.len()).ok()?, Arc::new(icons.clone())),
            )
        });
    }

    fn shoot(&mut self, pitch: f32, frame: FirstPersonHandsFrame) -> Vec<u8> {
        self.state.set_first_person_hands(frame);
        let frame = self.target.acquire().expect("headless acquire");
        self.state
            .render(self.ctx.device(), self.ctx.queue(), frame.view(), &camera(pitch), None, &[]);
        self.target.read_texels(self.ctx.device(), self.ctx.queue())
    }
}

fn hands(main: Option<MainHandItem>, off: Option<MainHandItem>) -> FirstPersonHandsFrame {
    let mut frame = FirstPersonHandsFrame::holding(main);
    frame.off = HandFrame::resting(off);
    frame
}

/// Looking straight down puts the two-handed map upright to the camera. Its icon
/// lands where the placement rules say, a red banner turned half a turn so it
/// reads upright, and the map is not the icon-free one with the lowest id.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_two_handed_map_draws_its_banner_where_the_rules_place_it() {
    let mut rig = Rig::new();
    let any = |_: u32, _: u32| true;
    let banner = [marker("minecraft:banner_red", 40, -30, 8)];

    rig.serve(vec![(PLAIN_MAP, vec![]), (LEFT_MAP, banner.to_vec())]);
    let plain = rig.shoot(90.0, hands(Some(held("minecraft:filled_map", Some(PLAIN_MAP))), None));
    let with = rig.shoot(90.0, hands(Some(held("minecraft:filled_map", Some(LEFT_MAP))), None));
    let picture = picture_box(&plain, any).expect("the picture draws");
    assert!(picture.max_x - picture.min_x > 100, "a 0.76 block map fills a good part of the view: {picture:?}");

    let icon = changed(TARGET, &with, &plain, any).expect("the banner must draw over the picture");
    let want = to_screen(picture, expected_map_rect(&sprite("red_banner"), 40, -30, 8));
    eprintln!("picture {picture:?}\nicon {icon:?}\nwant {want:?}");
    assert_box("two-handed banner", icon, want);
    assert_orientation(TARGET, "two-handed banner", "red_banner", (40, -30, 8), picture, &with, &plain);

    // Colour: the sprite is red, the picture is green.
    let (mut r, mut g, mut n) = (0u64, 0u64, 0u64);
    for (i, (a, b)) in with.chunks_exact(4).zip(plain.chunks_exact(4)).enumerate() {
        let _ = i;
        if a != b {
            r += u64::from(a[0]);
            g += u64::from(a[1]);
            n += 1;
        }
    }
    assert!(n > 0 && r > g * 2, "the changed pixels are the red banner: mean r {} g {}", r / n.max(1), g / n.max(1));
}

/// Each hand shows its own map's icons: the off hand's map on the left, the main
/// hand's one-handed map on the right, with a third, icon-free map unused. The
/// right map holds a green banner so its box cannot be mistaken for the left's.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_map_in_each_hand_draws_its_own_icons_on_its_own_side() {
    let mut rig = Rig::new();
    let left_icons = [marker("minecraft:banner_red", -50, 20, 8)];
    let right_icons = [marker("minecraft:banner_green", 30, -40, 8)];
    rig.serve(vec![
        (PLAIN_MAP, vec![]),
        (LEFT_MAP, left_icons.to_vec()),
        (RIGHT_MAP, right_icons.to_vec()),
    ]);
    let left = |x: u32, _: u32| x < W / 2;
    let right = |x: u32, _: u32| x >= W / 2;

    let plain = rig.shoot(
        90.0,
        hands(
            Some(held("minecraft:filled_map", Some(PLAIN_MAP))),
            Some(held("minecraft:filled_map", Some(PLAIN_MAP))),
        ),
    );
    let both = rig.shoot(
        90.0,
        hands(
            Some(held("minecraft:filled_map", Some(RIGHT_MAP))),
            Some(held("minecraft:filled_map", Some(LEFT_MAP))),
        ),
    );
    let left_picture = picture_box(&plain, left).expect("the off-hand picture draws on the left");
    let right_picture = picture_box(&plain, right).expect("the main-hand picture draws on the right");
    assert!(left_picture.max_x < W / 2 && right_picture.min_x >= W / 2);

    let left_icon = changed(TARGET, &both, &plain, left).expect("the off hand's banner draws");
    let right_icon = changed(TARGET, &both, &plain, right).expect("the main hand's banner draws");
    assert_box("off hand", left_icon, to_screen(left_picture, expected_map_rect(&sprite("red_banner"), -50, 20, 8)));
    assert_box("main hand", right_icon, to_screen(right_picture, expected_map_rect(&sprite("green_banner"), 30, -40, 8)));
    assert_orientation(TARGET, "off hand", "red_banner", (-50, 20, 8), left_picture, &both, &plain);
    assert_orientation(TARGET, "main hand", "green_banner", (30, -40, 8), right_picture, &both, &plain);
}

/// A held map shows the player's own marker, unlike an item frame.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_held_map_shows_the_player_marker() {
    let mut rig = Rig::new();
    let any = |_: u32, _: u32| true;
    rig.serve(vec![(PLAIN_MAP, vec![]), (LEFT_MAP, vec![marker("minecraft:player", 0, 0, 12)])]);
    let plain = rig.shoot(90.0, hands(Some(held("minecraft:filled_map", Some(PLAIN_MAP))), None));
    let with = rig.shoot(90.0, hands(Some(held("minecraft:filled_map", Some(LEFT_MAP))), None));
    let picture = picture_box(&plain, any).expect("the picture draws");
    let icon = changed(TARGET, &with, &plain, any).expect("the player marker draws on a held map");
    assert_box("player marker", icon, to_screen(picture, expected_map_rect(&sprite("player"), 0, 0, 12)));
    assert_orientation(TARGET, "player marker", "player", (0, 0, 12), picture, &with, &plain);
}
