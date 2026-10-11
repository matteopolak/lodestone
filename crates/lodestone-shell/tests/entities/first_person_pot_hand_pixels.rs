//! Pixel gate: a held decorated pot draws the sherds on its
//! `minecraft:pot_decorations`, not four plain sides.
//!
//! Two renders of the same `minecraft:decorated_pot` in the main hand: one with
//! no decorations and one with a sherd on every face. A hand that ignores the
//! component draws identical images; a real one repaints the four side panels.
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a failure.
//!
//! ```text
//! cargo test -p lodestone-shell --test entities first_person_pot_hand_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{MainHandItem, RenderState, SKY_COLOR};
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_model::PotDecorations;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 448;
const H: u32 = 256;
const SKY_LIT: u8 = 0xFF;

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

fn sherds() -> PotDecorations {
    let key = |name: &str| Some(name.parse().expect("valid key"));
    PotDecorations {
        back: key("minecraft:miner_pottery_sherd"),
        left: key("minecraft:prize_pottery_sherd"),
        right: key("minecraft:angler_pottery_sherd"),
        front: key("minecraft:archer_pottery_sherd"),
    }
}

/// Pixels that differ between two equally sized renders by more than a rounding
/// wobble.
fn moved_pixels(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(p, q)| (0..3).map(|c| (i32::from(p[c]) - i32::from(q[c])).abs()).sum::<i32>() > 24)
        .count()
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_held_decorated_pot_draws_its_sherds() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().expect("the vanilla pack loads (client.jar present)");
    let cam = camera();
    let sky = SKY_COLOR.map(|c| (c * 255.0).round() as u8);

    let mut shoot = |decorations: Option<PotDecorations>| -> Vec<u8> {
        let mut state = RenderState::new_headless(device, queue, format, W, H, Some(atlas.as_ref()));
        state.set_entity_light_source(|_| Some(SKY_LIT));
        state.set_sky_darken_source(|| Some(1.0));
        let item: ResourceLocation = "minecraft:decorated_pot".parse().expect("valid item id");
        state.set_main_hand_source(move || {
            Some(MainHandItem {
                map_id: None,
                item: item.clone(),
                foil: false,
                custom_model_data: None,
                dyed_color: None,
                potion_color: None,
                banner_patterns: Vec::new(),
                pot_decorations: decorations.clone(),
                base_color: None,
                skin: None,
            })
        });
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, &[]);
        assert!(stats.first_person_item_drawn, "the hand pass drew no held decorated pot");
        target.read_texels(device, queue)
    };

    let blank = shoot(None);
    let blank_again = shoot(None);
    let decorated = shoot(Some(sherds()));
    let drawn = blank.chunks_exact(4).filter(|p| (0..3).any(|c| p[c].abs_diff(sky[c]) > 8)).count();
    let control = moved_pixels(&blank, &blank_again);
    let moved = moved_pixels(&blank, &decorated);
    eprintln!("pot hand: drawn px={drawn}, blank-vs-blank moved={control}, blank-vs-sherds moved={moved}");
    assert!(drawn > 200, "the held pot barely drew ({drawn} px)");
    assert_eq!(control, 0, "two identical renders must not differ");
    assert!(moved > 50, "sherds on every face moved only {moved} px against the blank pot");
}
