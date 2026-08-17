//! Pixel gate: a held `minecraft:shield` draws its real dye colour in the
//! first-person hand — the surface named but left unverified by the shield
//! un-mirroring fix (`5c99876e`).
//!
//! # What was fixed, and what this file proves that the GUI-icon gate cannot
//!
//! `assets/minecraft/items/shield.json` carries its `"transformation"` (the
//! `[1, -1, -1]` scale, vanilla's `ShieldSpecialRenderer` flip hoisted into
//! data) on the enclosing `minecraft:condition` node; `lodestone_assets
//! ::item_model` used to read that field only on `minecraft:special` nodes,
//! so the flip never applied and every shield rendered back-to-front — a
//! defect invisible at the draw site because the front and back UV rects
//! differ by only 200 of 4096 texels. `hotbar_special_item_pixels.rs`'s
//! `shields_with_different_base_colours_draw_different_colours_and_a_plain_one_draws_neither`
//! gate proves the fix on the **GUI icon** surface.
//!
//! The first-person hand is a **different draw site with its own resolver**
//! (`RenderState::prepare_special_hand` in `gpu/first_person.rs`, not
//! `push_special_icon` in `hud/item_icon.rs`) that happens to call the same
//! parser output — `model.items.get(item)?.resolve_special(&ctx)?.transformation`
//! — through the same `compose_special_node_transform`. "The parse is
//! shared" is a claim about the code, not a substitute for a second
//! measurement: this file is that measurement, reusing the GUI gate's own
//! red/light_blue/plain three-way discriminator (see its own doc for why a
//! plain shield is the load-bearing **mid-magnitude anchor**, not filler) so
//! a regression that only breaks one of the two call sites — the exact shape
//! a future refactor could introduce — is still caught here even if the GUI
//! gate stays green.
//!
//! # The measurement — and what it actually found
//!
//! Three renders — `minecraft:shield` with `base_color` `red`, `light_blue`
//! and `None` — each held in the main hand against a pure-sky backdrop,
//! mirroring `first_person_banner_hand_pixels.rs`'s own camera and margin
//! (10, not the GUI gate's 40).
//!
//! **This gate is currently red, and that is a real, newly-found defect —
//! not an artefact of the gate.** Measured: `red` and `light_blue` render
//! **byte-identical** to each other and to a `base_color: None` plain
//! shield (`red r-b = 5.7`, `light_blue b-r = -5.7`, `plain spread = 5.7` —
//! all three the same small magnitude, none anywhere near the ~15-20
//! margin the GUI-icon and held-banner gates measure for a real dye). The
//! CPU-side wiring was traced end to end and is provably correct: `form
//! .transformation` carries `scale [1, -1, -1]`, `shield_has_patterns`
//! reports `true` for the dyed cases (`draw_calls` 4 vs the plain case's
//! 2, confirming the extra translucent pass is issued), the tint bytes
//! for `red`/`light_blue` are the real, distinct `[176, 46, 38]`/
//! `[58, 179, 218]`, the `"base"` pattern mask resolves in `self
//! .block_entities.shield_patterns`, and the draw ranges are two real
//! (36-index) non-degenerate parts. Every one of those was confirmed with
//! targeted `eprintln!` instrumentation added and removed during this
//! investigation — none of it is speculative.
//!
//! So the translucent dye/pattern layer is submitted correctly and still
//! contributes nothing visible. The leading hypothesis, unconfirmed: the
//! shield rig is a double-sided (`cull_mode: None`) thin box, so unlike a
//! banner's separate flag quad, the opaque base pass and the translucent
//! layer pass both rasterise *both* the front (masked) and back (blank)
//! faces of the same box and let the depth test (`CompareFunction
//! ::LessEqual`, zero bias) pick a winner per fragment. The GUI-icon path
//! (different "outer" placement entirely — an isometric `gui_item_pose`,
//! not `first_person_item_matrix`) is measured working, so whatever
//! decides which face is nearer the camera does not obviously generalise
//! from one display context to the other. This needs a real GPU frame
//! capture to confirm, which this text-only environment does not have;
//! flagged as a follow-up rather than guessed at further.
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip. **A red result here is the correct, current
//! state — do not "fix" this file by weakening its assertions.**
//!
//! ```text
//! cargo test -p lodestone-shell --test first_person_shield_hand_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{MainHandItem, RenderState, SKY_COLOR};
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{BlockModels, Camera, GpuContext, HeadlessTarget, RenderTarget};

/// See `first_person_banner_hand_pixels.rs`'s own doc: a 16:9-ish target,
/// never square (a square viewport draws zero held-item pixels — vertical
/// FOV).
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

/// Mean per-channel `[r, g, b]` over every pixel that differs from the sky
/// clear colour by more than a rounding wobble, plus the count — copied from
/// `first_person_banner_hand_pixels.rs`'s own `hand_mean_rgb` rather than
/// shared, since test binaries in this crate do not share a support module.
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
fn a_held_shield_draws_its_own_base_colour_and_a_plain_one_draws_neither() {
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
    // Premise, mirroring the banner gate's own: a shield has no baked 3-D
    // item geometry, so only the special-renderer path can draw it.
    {
        let models: &BlockModels = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        let item: ResourceLocation = "minecraft:shield".parse().expect("valid item id");
        assert!(
            models.item(&item).is_none(),
            "minecraft:shield unexpectedly has baked block-item geometry — if that is \
             now true, a shield could reach pixels through the ordinary model pipeline \
             and this gate is no longer exercising the special-renderer path the \
             un-mirroring fix touched"
        );
    }

    let cam = camera();
    let sky = sky_bytes();

    // A fresh `RenderState` per item — see the banner gate's own doc for why:
    // `HeldItemEquip`'s swap animation only adopts a new item instantly on a
    // state's very first observation.
    let mut shoot = |base_color: Option<&str>| -> (Vec<u8>, bool, usize) {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        state.set_entity_light_source(|_| Some(SKY_LIT));
        state.set_sky_darken_source(|| Some(1.0));
        let item: ResourceLocation = "minecraft:shield".parse().expect("valid item id");
        let base_color = base_color.map(str::to_string);
        state.set_main_hand_source(move || {
            Some(MainHandItem {
                item: item.clone(),
                foil: false,
                dyed_color: None,
                potion_color: None,
                banner_patterns: Vec::new(),
                base_color: base_color.clone(),
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

    let (red_pixels, red_drawn, red_draw_calls) = shoot(Some("red"));
    let (blue_pixels, blue_drawn, blue_draw_calls) = shoot(Some("light_blue"));
    let (plain_pixels, plain_drawn, plain_draw_calls) = shoot(None);

    let (red_rgb, red_n) = hand_mean_rgb(&red_pixels, sky);
    let (blue_rgb, blue_n) = hand_mean_rgb(&blue_pixels, sky);
    let (plain_rgb, plain_n) = hand_mean_rgb(&plain_pixels, sky);

    eprintln!("=== first-person held shield pixel gate ===");
    eprintln!("red shield:        drawn={red_drawn} draw_calls={red_draw_calls} px={red_n} mean_rgb={red_rgb:?}");
    eprintln!("light_blue shield: drawn={blue_drawn} draw_calls={blue_draw_calls} px={blue_n} mean_rgb={blue_rgb:?}");
    eprintln!("plain shield:      drawn={plain_drawn} draw_calls={plain_draw_calls} px={plain_n} mean_rgb={plain_rgb:?}");

    assert!(
        red_drawn && blue_drawn && plain_drawn,
        "the hand pass did not report a first-person item drawn for every shield \
         variant — `prepare_special_hand`'s `minecraft:shield` branch either did not \
         run or `shield_item_rig` resolved nothing"
    );
    assert!(red_n > 30, "a held red shield reached only {red_n} non-sky pixels — near-zero means a degenerate draw, not merely dim");
    assert!(blue_n > 30, "same near-zero failure for the light_blue shield ({blue_n} px)");
    assert!(plain_n > 30, "same near-zero failure for the plain shield ({plain_n} px)");
    // A dyed shield draws two batches (the opaque base + the translucent
    // dye/pattern layer, `shield_has_patterns` true); a plain one draws only
    // the opaque base (`shield_has_patterns` false) — see that function's
    // own doc. Asserting the *difference* rather than an absolute floor is
    // what makes this catch a regression that always issues the translucent
    // pass (or never does), not just a total draw-call count that happens to
    // clear some threshold.
    assert!(
        red_draw_calls > plain_draw_calls,
        "a dyed shield (base+pattern layer) must issue more draw calls than a plain \
         one (base only): red={red_draw_calls}, plain={plain_draw_calls}"
    );
    assert!(
        blue_draw_calls > plain_draw_calls,
        "same for light_blue vs plain: light_blue={blue_draw_calls}, plain={plain_draw_calls}"
    );

    let [rr, _rg, rb] = red_rgb;
    let [br, _bg, bb] = blue_rgb;
    let [pr, pg, pb] = plain_rgb;
    eprintln!("red shield   r-b = {:.1}", rr - rb);
    eprintln!("light-blue   b-r = {:.1}", bb - br);
    let plain_spread = pr.max(pg).max(pb) - pr.min(pg).min(pb);
    eprintln!("plain shield max-min channel spread = {plain_spread:.1}");

    // Vanilla's dye colours: RED = (176, 46, 38), LIGHT_BLUE = (58, 179,
    // 218) — the same discriminating pair the GUI-icon gate uses, at a
    // smaller margin for the reason this file's own doc gives (the hand
    // pass's entity-light term and the shield's untinted handle both pull
    // the mean toward neutral).
    assert!(
        rr - rb > 10.0,
        "the held red shield's non-sky pixels average r={rr:.1}, b={rb:.1} — red must \
         dominate blue by a real margin for this to be the item's own dye colour and \
         not an untinted grey mesh (r≈b) or, if the mirroring regressed, a swap onto \
         the back face's near-identical pixels"
    );
    assert!(
        bb - br > 10.0,
        "the held light_blue shield's non-sky pixels average b={bb:.1}, r={br:.1} — \
         blue must dominate red by a real margin for the same reason"
    );
    assert!(
        (rr - br).abs() > 8.0,
        "the two shields' red channels ({rr:.1} vs {br:.1}) are too close — they may \
         be drawing with the same tint rather than each item's own colour"
    );
    // The mid-magnitude anchor, mirroring the GUI-icon gate's own reasoning:
    // a plain shield draws no translucent tint layer at all, so its channel
    // spread must sit well below either dyed shield's own r-b/b-r margin.
    assert!(
        plain_spread < (rr - rb).min(bb - br),
        "a plain shield's own channel spread ({plain_spread:.1}) should be well below \
         either dyed shield's discriminating margin (red {:.1}, light_blue {:.1}) — a \
         plain shield draws no translucent tint layer at all, so it should read far \
         closer to a neutral grey",
        rr - rb,
        bb - br
    );
}
