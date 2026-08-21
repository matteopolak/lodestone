//! Pixel gate: a held `minecraft:player_head` reaches pixels in the
//! first-person hand — a surface with **no existing gate at all**.
//!
//! `hotbar_special_item_pixels.rs`'s `a_player_head_item_in_the_hotbar_reaches_pixels`
//! proves the **GUI icon** surface (`hud/item_icon.rs`'s `push_special_icon`,
//! resolved through `special_item_rig`). The first-person hand is a
//! **different draw site with its own resolver**
//! (`RenderState::prepare_special_hand`'s generic fallback arm in
//! `gpu/first_person.rs`, at the bottom after the banner/shield special
//! cases), which the shield and banner hand gates
//! (`first_person_shield_hand_pixels.rs`, `first_person_banner_hand_pixels.rs`)
//! already prove separately from their own GUI-icon siblings — a skull/head
//! had no equivalent, so a regression isolated to the hand resolver's generic
//! arm (which `minecraft:chest`, `minecraft:shulker_box` and
//! `minecraft:head`/`minecraft:player_head` all share) had nothing to fail.
//!
//! ```text
//! cargo test -p lodestone-shell --test first_person_head_hand_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{MainHandItem, RenderState, SKY_COLOR};
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{BlockModels, Camera, GpuContext, HeadlessTarget, RenderTarget};

/// See the shield/banner hand gates' own docs: a 16:9-ish target, never
/// square (a square viewport draws zero held-item pixels — vertical FOV).
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

fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

/// Count of non-sky pixels — copied from the shield/banner hand gates' own
/// `hand_mean_rgb`, count half only (no colour discrimination needed here:
/// a player head with no profile fetched draws the flat default Steve sheet).
fn hand_non_sky_count(pixels: &[u8], sky: [u8; 3]) -> usize {
    let mut n = 0usize;
    for px in pixels.chunks_exact(4) {
        let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
            + (i32::from(px[1]) - i32::from(sky[1])).abs()
            + (i32::from(px[2]) - i32::from(sky[2])).abs();
        if d > 8 {
            n += 1;
        }
    }
    n
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_held_player_head_reaches_pixels_and_a_held_chest_still_does_too() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS \
             to a pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });
    // Premise, mirroring the shield/banner gates' own: a player head has no
    // baked 3-D item geometry, so only the special-renderer path can draw it.
    {
        let models: &BlockModels = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        let item: ResourceLocation = "minecraft:player_head".parse().expect("valid item id");
        assert!(
            models.item(&item).is_none(),
            "minecraft:player_head unexpectedly has baked block-item geometry — this gate \
             would no longer be exercising the special-renderer path"
        );
    }

    let cam = camera();
    let sky = sky_bytes();

    // A fresh `RenderState` per item, mirroring the shield/banner gates' own
    // reasoning: `HeldItemEquip`'s swap animation only adopts a new item
    // instantly on a state's very first observation.
    let mut shoot = |item_id: &str| -> (Vec<u8>, bool, usize) {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        state.set_entity_light_source(|_| Some(SKY_LIT));
        state.set_sky_darken_source(|| Some(1.0));
        let item: ResourceLocation = item_id.parse().expect("valid item id");
        state.set_main_hand_source(move || {
            Some(MainHandItem {
                item: item.clone(),
                foil: false,
                dyed_color: None,
                potion_color: None,
                banner_patterns: Vec::new(),
                base_color: None,
            })
        });
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, &[]);
        (
            target.read_texels(device, queue),
            stats.first_person_item_drawn,
            stats.draw_calls,
        )
    };

    let (head_pixels, head_drawn, head_draw_calls) = shoot("minecraft:player_head");
    let (chest_pixels, chest_drawn, chest_draw_calls) = shoot("minecraft:chest");

    let head_n = hand_non_sky_count(&head_pixels, sky);
    let chest_n = hand_non_sky_count(&chest_pixels, sky);

    eprintln!("=== first-person held player_head pixel gate ===");
    eprintln!("player_head: drawn={head_drawn} draw_calls={head_draw_calls} px={head_n}");
    eprintln!("chest (control): drawn={chest_drawn} draw_calls={chest_draw_calls} px={chest_n}");

    assert!(
        chest_drawn && chest_n > 30,
        "control failed: a held chest did not reach pixels either (drawn={chest_drawn}, \
         px={chest_n}) — the harness itself, not the head resolver, is broken"
    );
    assert!(
        head_drawn,
        "the hand pass did not report a first-person item drawn for a held player_head — \
         `prepare_special_hand`'s generic fallback arm either did not run or \
         `special_item_rig`/`build_special_hand_draw` resolved nothing for \
         minecraft:player_head"
    );
    assert!(
        head_n > 30,
        "a held player_head reached only {head_n} non-sky pixels (chest control reached \
         {chest_n}) — near-zero means a degenerate draw, not merely dim"
    );
}
