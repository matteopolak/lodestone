//! Pixel gate: a held `minecraft:banner` draws its dye colour in the
//! first-person hand, not nothing.
//!
//! # What was broken
//!
//! `lodestone_render::special_item_rig`'s own doc table names `banner` as one
//! of six `kind`s that resolve to `None` — "needs the ordered translucent
//! pattern-mask pass, not one rig" — which is why `RenderState::
//! prepare_special_hand` returned `None` for a held banner and the player's
//! own report was exactly this: a banner does not render in the hand. See
//! `hotbar_special_item_pixels.rs`'s sibling gate for the item-slot half of
//! the same report; this file is the held-item half, because
//! [`lodestone::gpu::first_person`]'s own module doc records that the two
//! surfaces are resolved through **different pipelines with different group-0
//! layouts** and a fix to one does not imply the other is fixed.
//!
//! `lodestone_render::banner_item_rig` landed the geometry, and the first
//! version of this file closed only the narrower half of the gap: a
//! tint-multiply over the plain flag mesh, base colour only, no loom
//! patterns. That is now the real thing — `minecraft:banner_patterns` decodes
//! for an item stack (`crates/protocol/v770`), `rig.flag` draws **untinted**,
//! and `RenderState::prepare_special_hand` issues a second, translucent
//! pattern-layer pass over the same flag geometry the world's block-entity
//! banner renderer uses (`banner_layer_pipeline`) — see
//! [`a_held_banner_draws_its_own_loom_pattern_not_just_its_base_colour`]
//! below for the pattern half specifically; this test (the first one in the
//! file) still covers the base-colour half.
//!
//! # The measurement
//!
//! Two renders, `minecraft:red_banner` and `minecraft:light_blue_banner`,
//! each held in the main hand against a pure-sky backdrop at [`W`]x[`H`] =
//! 448x256 (16:9-ish, never square — a square viewport is documented
//! elsewhere in this crate to draw **zero** held-item pixels because
//! `hand_projection`'s FOV is vertical). For each: the hand pass must report
//! it drew (`RenderStats::first_person_item_drawn`), a real run of non-sky
//! pixels must exist, and — the discriminator that tells "the item's own
//! colour" from "a hardcoded tint" or "the plain grey mesh leaking through
//! untinted" — the two banners' mean channel values must disagree in the
//! direction their own `textureDiffuseColor` predicts (red R > B, light-blue
//! B > R, by a wide margin, and the two banners' own red channels far apart).
//! Neither banner carries a pattern layer, so both draw the base translucent
//! mask only — proving the base colour reaches pixels is this test's whole
//! job; the pattern gate below is a separate, later addition.
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test first_person_banner_hand_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{MainHandItem, RenderState, SKY_COLOR};
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{BlockModels, Camera, GpuContext, HeadlessTarget, RenderTarget};

/// See the module doc: a 16:9-ish target, never square.
const W: u32 = 448;
const H: u32 = 256;

/// Sky-lit, no block light — full bright is fine here; this gate measures
/// colour, not the day/night sweep `first_person_hand_light_pixels.rs` does.
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

/// Mean per-channel `[r, g, b]` over every pixel that differs from the sky
/// clear colour by more than a rounding wobble, plus the count — the
/// per-channel sibling of `first_person_hand_light_pixels.rs`'s
/// `hand_mean_brightness`, needed here because *which* colour drew is the
/// question, not merely *whether* something did.
fn hand_mean_rgb(pixels: &[u8], sky: [u8; 3]) -> ([f64; 3], usize) {
    let mut sum = [0f64; 3];
    let mut n = 0usize;
    for px in pixels.chunks_exact(4) {
        let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
            + (i32::from(px[1]) - i32::from(sky[1])).abs()
            + (i32::from(px[2]) - i32::from(sky[2])).abs();
        if d <= 8 {
            continue;
        }
        sum[0] += f64::from(px[0]);
        sum[1] += f64::from(px[1]);
        sum[2] += f64::from(px[2]);
        n += 1;
    }
    if n == 0 {
        ([f64::NAN; 3], 0)
    } else {
        ([sum[0] / n as f64, sum[1] / n as f64, sum[2] / n as f64], n)
    }
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_held_banner_draws_its_own_dye_colour_not_nothing() {
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
    // Premise: a banner has no baked 3-D item geometry, so the ordinary model
    // pipeline (`FirstPersonHand::Item`) could never draw it — only the
    // special-renderer path (`FirstPersonHand::Special`) can, which is exactly
    // the path `special_item_rig`'s `_ => None` used to shut for this `kind`.
    {
        let models: &BlockModels = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        let item: ResourceLocation = "minecraft:red_banner".parse().expect("valid item id");
        assert!(
            models.item(&item).is_none(),
            "minecraft:red_banner unexpectedly has baked block-item geometry — if that \
             is now true, a banner could reach pixels through the ordinary model \
             pipeline and this gate is no longer exercising the special-renderer path \
             `banner_item_rig` fixes"
        );
    }

    let cam = camera();
    let sky = sky_bytes();

    // A **fresh** `RenderState` per item, not one reused across both: `HeldItemEquip`
    // is vanilla's own `mainHandItem`/`mainHandHeight` swap animation, seeded at
    // rest on its *first* observation (see `HeldItemEquip::advance`'s doc) but
    // taking real wall-clock time to swap on every observation after that — a
    // second `shoot` on the same `state` would still be mid-dip toward the new
    // item on the very next frame and could read back the *previous* item's
    // pixels almost unchanged. A fresh state's first frame adopts its item
    // instantly, which is what "does this item draw its own colour" needs.
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

    let (red_pixels, red_drawn, red_draw_calls): (Vec<u8>, bool, usize) =
        shoot("minecraft:red_banner");
    let (blue_pixels, blue_drawn, blue_draw_calls): (Vec<u8>, bool, usize) =
        shoot("minecraft:light_blue_banner");

    let (red_rgb, red_n) = hand_mean_rgb(&red_pixels, sky);
    let (blue_rgb, blue_n) = hand_mean_rgb(&blue_pixels, sky);

    eprintln!("=== first-person held banner pixel gate ===");
    eprintln!(
        "red_banner:        drawn={red_drawn} draw_calls={red_draw_calls} \
         px={red_n} mean_rgb={red_rgb:?}"
    );
    eprintln!(
        "light_blue_banner: drawn={blue_drawn} draw_calls={blue_draw_calls} \
         px={blue_n} mean_rgb={blue_rgb:?}"
    );

    assert!(
        red_drawn,
        "the hand pass did not report a first-person item drawn for a held \
         red_banner — this is the owner's own report (a banner does not render \
         in the hand), reproduced: `prepare_special_hand` returned `None`, \
         either because `banner_item_rig` did too or the `kind` never reached it"
    );
    assert!(blue_drawn, "same failure for light_blue_banner");
    assert!(
        red_n > 30,
        "a held red banner reached only {red_n} non-sky pixels — near-zero means \
         the rig resolved but the draw is degenerate (an empty part list, a \
         zero-size mesh), not merely dim"
    );
    assert!(blue_n > 30, "same near-zero failure for light_blue_banner ({blue_n} px)");
    assert!(
        red_draw_calls >= 2 && blue_draw_calls >= 2,
        "a banner rig is two meshes (pole/bar, flag) — red draw_calls={red_draw_calls}, \
         light_blue draw_calls={blue_draw_calls}; fewer than 2 means only one half of \
         the rig reached the pass"
    );

    let [rr, _rg, rb] = red_rgb;
    let [br, _bg, bb] = blue_rgb;
    eprintln!("red banner   r-b = {:.1}", rr - rb);
    eprintln!("light-blue   b-r = {:.1}", bb - br);

    // Vanilla's textureDiffuseColor: RED = (176, 46, 38), LIGHT_BLUE =
    // (58, 179, 218). Same discriminating pair and the same reasoning as the
    // hotbar gate's sibling assertion — a hardcoded/untinted mesh would read
    // r≈b, and a channel-swapped tint would flip the sign.
    //
    // The margin is smaller than the flat GUI-icon gate's (10, not 40): the
    // hand pass shades each vertex by the entity light term and the pole/bar
    // draws untinted right next to the flag, both of which pull the *whole*
    // hand region's mean toward neutral. Measured on a real run: red banner
    // r-b = 17.7, light-blue b-r = 20.6 — comfortably above 10 either way,
    // and re-derived from that run rather than guessed.
    assert!(
        rr - rb > 10.0,
        "the held red_banner's non-sky pixels average r={rr:.1}, b={rb:.1} — red \
         must dominate blue by a real margin for this to be the item's own dye \
         colour and not an untinted grey mesh (r≈b) or a channel-swapped tint (b>r)"
    );
    assert!(
        bb - br > 10.0,
        "the held light_blue_banner's non-sky pixels average b={bb:.1}, r={br:.1} \
         — blue must dominate red by a real margin for the same reason"
    );
    assert!(
        (rr - br).abs() > 8.0,
        "the two banners' red channels ({rr:.1} vs {br:.1}) are too close — they \
         may be drawing with the same tint rather than each item's own colour"
    );
}

/// Per-pixel `[r, g, b]` for every pixel in `pixels` that differs from `sky`
/// by more than a rounding wobble — the per-pixel sibling of [`hand_mean_rgb`],
/// used below to compare two renders directly rather than against a guessed
/// absolute target colour. **Absolute target colours were tried first and
/// were the wrong premise**: the hand pass's own entity-light term renders
/// this scene far dimmer than vanilla's raw `textureDiffuseColor` bytes (the
/// sibling test above's own red/light_blue means top out under 30/255, not
/// near 176 or 218) — a fixed `(249, 255, 254)` "white" target was simply
/// never going to match, no matter how correct the draw. Comparing one
/// render against another, at whatever brightness this scene actually
/// renders at, has no such premise to get wrong.
fn non_sky_rgb(pixels: &[u8], sky: [u8; 3]) -> Vec<(u32, [u8; 3])> {
    pixels
        .chunks_exact(4)
        .enumerate()
        .filter_map(|(i, px)| {
            let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
                + (i32::from(px[1]) - i32::from(sky[1])).abs()
                + (i32::from(px[2]) - i32::from(sky[2])).abs();
            (d > 8).then_some((i as u32, [px[0], px[1], px[2]]))
        })
        .collect()
}

/// Pixel gate: a held banner draws its **loom patterns**, not just its base
/// colour — the half `banner_item_rig`'s doc names as still missing after the
/// tint-multiply landing, closed by decoding `minecraft:banner_patterns` for
/// an item stack (`crates/protocol/v770`) and a real translucent
/// pattern-layer pass over the flag (`RenderState::prepare_special_hand`,
/// mirroring the world block-entity pass's `banner_layer_pipeline`).
///
/// # The discriminating claim: adding a pattern must change the image
///
/// Two renders of the **same** `minecraft:red_banner`, one with **no**
/// pattern layers and one with a `minecraft:lime`-coloured `minecraft:creeper`
/// pattern added. A flat tint — the old mechanism, or any regression back
/// toward it — cannot respond to `banner_patterns` at all, so the two images
/// would be pixel-identical. A real masked layer draw changes exactly the
/// creeper-shaped region: this gate counts pixels that moved by a real
/// margin between the two renders (not a rounding wobble) and requires the
/// **surviving mean** (pixels that did *not* move — the base still showing
/// through) to stay red-dominated while the **moved** pixels' mean shifts
/// measurably toward green, since lime is the green-dominant colour that
/// discriminates hardest against red here.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_held_banner_draws_its_own_loom_pattern_not_just_its_base_colour() {
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

    let cam = camera();
    let sky = sky_bytes();

    let mut shoot = |patterns: Vec<lodestone_model::BannerPatternLayer>| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        state.set_entity_light_source(|_| Some(SKY_LIT));
        state.set_sky_darken_source(|| Some(1.0));
        let item: ResourceLocation = "minecraft:red_banner".parse().expect("valid item id");
        state.set_main_hand_source(move || {
            Some(MainHandItem {
                item: item.clone(),
                foil: false,
                dyed_color: None,
                potion_color: None,
                banner_patterns: patterns.clone(),
                base_color: None,
            })
        });
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, &[]);
        assert!(
            stats.first_person_item_drawn,
            "the hand pass did not report a first-person item drawn for a held \
             red_banner"
        );
        target.read_texels(device, queue)
    };

    let plain_pixels = shoot(Vec::new());
    let patterned_pixels = shoot(vec![lodestone_model::BannerPatternLayer {
        pattern_asset_id: "creeper".to_string(),
        color: "lime".to_string(),
    }]);

    let plain = non_sky_rgb(&plain_pixels, sky);
    let patterned = non_sky_rgb(&patterned_pixels, sky);

    // Index the plain render by pixel index for the moved/unmoved split below.
    let plain_by_index: std::collections::HashMap<u32, [u8; 3]> = plain.into_iter().collect();

    let mut moved_sum = [0i64; 3];
    let mut moved_n = 0usize;
    let mut unmoved_sum = [0i64; 3];
    let mut unmoved_n = 0usize;
    for (i, prgb) in &patterned {
        match plain_by_index.get(i) {
            Some(qrgb) => {
                let d = (i32::from(prgb[0]) - i32::from(qrgb[0])).abs()
                    + (i32::from(prgb[1]) - i32::from(qrgb[1])).abs()
                    + (i32::from(prgb[2]) - i32::from(qrgb[2])).abs();
                if d > 24 {
                    for c in 0..3 {
                        moved_sum[c] += i64::from(prgb[c]);
                    }
                    moved_n += 1;
                } else {
                    for c in 0..3 {
                        unmoved_sum[c] += i64::from(prgb[c]);
                    }
                    unmoved_n += 1;
                }
            }
            // A non-sky pixel in the patterned render with no counterpart in
            // the plain one (the silhouette grew) counts as moved too.
            None => {
                for c in 0..3 {
                    moved_sum[c] += i64::from(prgb[c]);
                }
                moved_n += 1;
            }
        }
    }

    eprintln!("=== first-person held banner pattern-layer pixel gate ===");
    eprintln!("plain red_banner:    non-sky px={}", plain_by_index.len());
    eprintln!("+creeper (lime):     non-sky px={}, moved px={moved_n}, unmoved px={unmoved_n}", patterned.len());
    if moved_n > 0 {
        let m = moved_sum.map(|s| s as f64 / moved_n as f64);
        eprintln!("moved-pixel mean rgb   = {m:?}");
    }
    if unmoved_n > 0 {
        let u = unmoved_sum.map(|s| s as f64 / unmoved_n as f64);
        eprintln!("unmoved-pixel mean rgb = {u:?}");
    }

    // --- the discriminating claim: the mask moved a real number of pixels -
    assert!(
        moved_n > 20,
        "adding a lime `creeper` pattern layer moved only {moved_n} pixels by \
         more than a rounding wobble relative to the unpatterned render — the \
         pattern mask is not reaching pixels, which is the exact gap \
         `banner_item_rig`'s own doc named: colour without pattern"
    );
    assert!(
        unmoved_n > 20,
        "adding the pattern moved every non-sky pixel ({moved_n} of {}) — a masked \
         layer draw should leave the uncovered rest of the flag (and the whole \
         untinted pole/bar) unchanged, so this looks like a full-flag tint \
         rather than a local mask",
        patterned.len()
    );
    // --- which colour moved: lime is green-dominant, unlike red_banner's own
    // red-dominant base, so the *moved* pixels' green channel must exceed
    // their own red channel — the discriminator a flat "everything got a bit
    // darker" shift could not satisfy, since that would move every channel
    // down together rather than flipping their order.
    let moved_mean = moved_sum.map(|s| s as f64 / moved_n as f64);
    assert!(
        moved_mean[1] > moved_mean[0],
        "the pixels the creeper pattern moved average rgb={moved_mean:?} — green \
         must exceed red for a lime pattern over a red base, or this is not the \
         pattern's own colour reaching the mask"
    );
}
