//! Pixel gate: the first-person **arm** and the first-person **held item**
//! must both dim with the world at night — "walking into a cave
//! leaves your hand lit as if it were noon."
//!
//! # What was actually broken
//!
//! `RenderState::hand_light` already sampled real per-position world light
//! for both branches (`self.entity_light.sample(camera.position)`), and
//! `write_hand_camera` already folded `self.sky_darken.value()` into the
//! **arm**'s `EntityCameraUniform` via `EntityCameraUniform::with_sky_darken`.
//! So the light *sample* and the *sky-darken source* were never the missing
//! piece — both already existed and were already installed (`app.rs` calls
//! `set_sky_darken_source` and `set_entity_light_source` on both connect
//! paths).
//!
//! What was missing was one hop later, and only on the **item** side:
//! `write_hand_camera` wrote the item's `model.hand_cam_buffer` with a bare
//! `FogUniform::disabled()`, which leaves the shared sky-darken lane
//! (`fog.end_enabled[2]`) at its `0.0` sentinel. `model_pipeline.rs`'s
//! `sky_darken()` reads `<= 0.0` as `1.0` — permanent noon — so the arm
//! (entity pipeline, lane correctly set) already dimmed at night while the
//! held item (model pipeline, lane left at the sentinel) stayed lit as if it
//! were noon, right next to it on the same screen. The fix folds the same
//! `self.sky_darken.value()` into the item's buffer via the same lane.
//!
//! # Why both branches need their own gate
//!
//! `FirstPersonHand::{Item, Arm}` is vanilla's own `isEmpty()` fork — never
//! both — and the two draw through **different pipelines with different
//! group-0 layouts** (entity vs. model). A fix that patches one silently
//! leaves the other broken, and a player only sees the mismatch when they
//! compare "my hand" against "my hand holding something", which is exactly
//! how this was reported. So this file gates the arm and the item
//! separately, each against its own determinism control.
//!
//! # The measurement
//!
//! Both passes draw onto a frame that is otherwise pure sky (no terrain, no
//! world entities) at [`W`]x[`H`] = 448x256 — a 16:9-ish target, not a square
//! one: `first-person-held-item.md` measured a square viewport drawing
//! **zero** held-item pixels (`hand_projection`'s FOV is vertical, so the
//! horizontal half-angle grows with aspect), so a square gate here would read
//! as "the item vanished" for a reason that has nothing to do with lighting.
//!
//! `entity_light` is pinned to `sky=15, block=0` — an outdoors, fully
//! sky-lit position with no nearby torch — because a saturated block channel
//! (`ENTITY_FULLBRIGHT`'s default `sky=15, block=15`) would make the shader's
//! `max(brightness(sky) * sky_darken, brightness(block))` return the block half
//! regardless of `sky_darken`, hiding the exact defect this gate exists to catch.
//!
//! For each branch: render at noon (`sky_darken = 1.0`) and at midnight
//! (`sky_darken = 0.24`, vanilla's own floor), and assert the mean channel
//! value over the hand's own non-sky pixels drops by a real margin — plus a
//! determinism control (the *same* `sky_darken` rendered twice) that must be
//! pixel-identical, so "noon differs from midnight" cannot be satisfied by a
//! non-deterministic renderer instead of the lighting fix.
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test first_person_hand_light_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{BlockModels, Camera, GpuContext, HeadlessTarget, RenderTarget};

/// See the module doc: a 16:9-ish target, never square.
const W: u32 = 448;
const H: u32 = 256;

/// Sky-lit, no block light — the one light byte that actually exercises
/// `sky_darken`. See the module doc for why `ENTITY_FULLBRIGHT` (block=15)
/// would hide the bug.
const SKY_LIT_NO_BLOCK: u8 = 0xF0;

/// Vanilla's own night floor (`LightTexture`'s curve at midnight).
const MIDNIGHT: f32 = 0.24;
const NOON: f32 = 1.0;

/// The held item under test: real 3-D geometry (not a flat sprite), the same
/// reference item `thrown_and_held_item_pixels.rs` uses for the equivalent
/// gate in `lodestone-render`.
const ITEM: &str = "minecraft:diamond_pickaxe";

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

/// Mean per-channel byte value over every pixel that differs from the sky
/// clear colour by more than a rounding wobble — i.e. the hand's own pixels,
/// however the arm/item silhouette happens to be shaped.
fn hand_mean_brightness(pixels: &[u8], sky: [u8; 3]) -> (f64, usize) {
    let mut sum = 0f64;
    let mut n = 0usize;
    for px in pixels.chunks_exact(4) {
        let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
            + (i32::from(px[1]) - i32::from(sky[1])).abs()
            + (i32::from(px[2]) - i32::from(sky[2])).abs();
        if d <= 8 {
            continue;
        }
        sum += f64::from(px[0]) + f64::from(px[1]) + f64::from(px[2]);
        n += 1;
    }
    if n == 0 {
        (f64::NAN, 0)
    } else {
        (sum / (n as f64 * 3.0), n)
    }
}

fn assert_pixel_identical(a: &[u8], b: &[u8], what: &str) {
    let mut diffs = 0usize;
    for (x, y) in a.iter().zip(b.iter()) {
        if x != y {
            diffs += 1;
        }
    }
    assert_eq!(
        diffs, 0,
        "{what}: two renders with the *same* sky_darken differ by {diffs} bytes — the \
         renderer is not deterministic, so a noon-vs-midnight difference proves nothing"
    );
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_first_person_arm_dims_with_the_world_at_night() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let mut state = RenderState::new(device, queue, format, W, H, None);
    state.set_entity_light_source(|_| Some(SKY_LIT_NO_BLOCK));
    let cam = camera();

    let mut shoot = |darken: f32| -> Vec<u8> {
        state.set_sky_darken_source(move || Some(darken));
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, &[]);
        assert!(
            stats.first_person_arm_drawn,
            "the arm branch did not draw at all (first_person_arm_drawn=false) — a real \
             defect, not a lighting one; the player_wide rig or its texture failed to load"
        );
        target.read_texels(device, queue)
    };

    let noon_1 = shoot(NOON);
    let noon_2 = shoot(NOON);
    assert_pixel_identical(&noon_1, &noon_2, "arm, noon x2 (determinism control)");

    let midnight = shoot(MIDNIGHT);

    let sky = sky_bytes();
    let (noon_mean, noon_px) = hand_mean_brightness(&noon_1, sky);
    let (midnight_mean, midnight_px) = hand_mean_brightness(&midnight, sky);

    eprintln!("=== first-person ARM night gate ===");
    eprintln!("noon     mean channel = {noon_mean:.2} over {noon_px} px");
    eprintln!("midnight mean channel = {midnight_mean:.2} over {midnight_px} px");

    assert!(
        noon_px > 200 && midnight_px > 200,
        "the arm itself should reach a substantial run of pixels at both times \
         (noon={noon_px}, midnight={midnight_px}); near-zero means the whole hand path is \
         broken, not just its lighting"
    );
    // Re-derived from `lightmap.fsh`: at sky 15 / block 0 the light term is
    // exactly `1.0` at noon, and at midnight `get_brightness(1) * SkyFactor` is
    // `0.24`, which `mix(c, notGamma(c), 0.5)` lifts to `0.4532`. (Under the
    // retired `0.2 + 0.8 * l` ramp it was `0.392` — the direction is the same,
    // which is why this gate did not move when the curve landed.)
    //
    // The floor here is deliberately generous (any real, non-trivial gap) rather
    // than tight: the synthetic placeholder texture's own hue and the gamma
    // round-trip both affect the exact numbers, and this gate is about *whether*
    // the arm responds to time of day, not the precise curve. The magnitude of
    // the curve itself is gated at pixels by `entity_night_pixels`'
    // `a_sky_lit_mob_is_darker_at_midnight_than_at_noon`, which predicts 0.4532
    // and rejects both 0.392 and 1.000 — so this loose threshold is not the only
    // thing standing behind the number.
    assert!(
        noon_mean - midnight_mean > 15.0,
        "the arm at midnight ({midnight_mean:.2}) is not meaningfully darker than at noon \
         ({noon_mean:.2}) — the arm is not responding to the world clock at all, which is \
         issue #74's reported symptom"
    );
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_first_person_held_item_dims_with_the_world_at_night() {
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
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS to a \
             pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });
    let item: ResourceLocation = ITEM.parse().expect("valid item id");
    {
        let models: &BlockModels = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        assert!(
            models.item(&item).is_some(),
            "{ITEM} must have baked 3-D geometry; without it this gate would be measuring the \
             absence of an item rather than the absence of lighting"
        );
    }

    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
    state.set_entity_light_source(|_| Some(SKY_LIT_NO_BLOCK));
    // `false` is the glint flag: this gate measures lighting, and a glint
    // pass would add emission that the darken sweep below would read as light.
    state.set_main_hand_source(move || Some((item.clone(), false)));
    let cam = camera();

    let mut shoot = |darken: f32| -> Vec<u8> {
        state.set_sky_darken_source(move || Some(darken));
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, &[]);
        assert!(
            stats.first_person_item_drawn,
            "the item branch did not draw at all (first_person_item_drawn=false) — a real \
             defect, not a lighting one; check the item resolves baked geometry at this \
             viewport aspect (see the module doc on why W/H are not square)"
        );
        target.read_texels(device, queue)
    };

    let noon_1 = shoot(NOON);
    let noon_2 = shoot(NOON);
    assert_pixel_identical(&noon_1, &noon_2, "item, noon x2 (determinism control)");

    let midnight = shoot(MIDNIGHT);

    let sky = sky_bytes();
    let (noon_mean, noon_px) = hand_mean_brightness(&noon_1, sky);
    let (midnight_mean, midnight_px) = hand_mean_brightness(&midnight, sky);

    eprintln!("=== first-person ITEM night gate ===");
    eprintln!("noon     mean channel = {noon_mean:.2} over {noon_px} px");
    eprintln!("midnight mean channel = {midnight_mean:.2} over {midnight_px} px");

    assert!(
        noon_px > 200 && midnight_px > 200,
        "the held item itself should reach a substantial run of pixels at both times \
         (noon={noon_px}, midnight={midnight_px}); near-zero means the whole held-item path \
         is broken, not just its lighting"
    );
    // Direction-and-margin only, for the same reason as the arm branch above: the
    // target here is a plain `Rgba8Unorm`, so the shader's gamma-space shade
    // multiply is *not* proportional to the readback byte and the exact ratio is
    // not predictable from the light term alone. `entity_night_pixels` gates the
    // magnitude on an sRGB target, where it is.
    assert!(
        noon_mean - midnight_mean > 15.0,
        "the held item at midnight ({midnight_mean:.2}) is not meaningfully darker than at \
         noon ({noon_mean:.2}) — this is issue #74 exactly: the item stayed lit as if it were \
         noon while the arm (see the sibling test) correctly dimmed"
    );
}
