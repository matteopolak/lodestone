//! Pixel gate: a held `minecraft:shield` shows the player its **back** in the
//! first-person hand, so every dye renders identically there — and the pattern
//! layer is still submitted, on the face pointing away.
//!
//! # What this surface actually shows, and why that is not the GUI's answer
//!
//! `assets/minecraft/items/shield.json` carries its `"transformation"` (the
//! `scale [1, -1, -1]`, vanilla's `ShieldSpecialRenderer` flip hoisted into
//! data) on the enclosing `minecraft:condition` node; `lodestone_assets
//! ::item_model` used to read that field only on `minecraft:special` nodes, so
//! the flip never applied and every shield rendered back-to-front.
//! `hotbar_special_item_pixels.rs`'s
//! `shields_with_different_base_colours_draw_different_colours_and_a_plain_one_draws_neither`
//! proves that fix on the **GUI icon** surface, where a dyed shield does show
//! its colour.
//!
//! The first-person hand is a different draw site with its own resolver
//! (`RenderState::prepare_special_hand` in `gpu/first_person.rs`, not
//! `push_special_icon` in `hud/item_icon.rs`) — and it is also a different
//! *view of the shield*. `shield.json`'s `firstperson_righthand` display is
//! `rotation [0, 180, 5]` where its `gui` display is `rotation [15, -25, -5]`:
//! the two differ by about 205 degrees of yaw, so they cannot both show the same
//! face of a one-texel-thick plate. The GUI shows the decorated front — it must,
//! or an inventory shield would not show its banner — therefore the
//! first-person hand shows the **back**, the grip side, exactly as vanilla does
//! when you are carrying a shield rather than looking at it in a slot.
//!
//! The plate's own geometry says the same thing: `shield_model`'s `plate` box is
//! `[-6, -11, -2]` extending `[12, 22, 1]`, so its outer face is at `z = -2`,
//! while the `handle` box occupies `z` from `-1` to `+5`. The handle is the side
//! your arm is on, so `+z` is the wielder and `-z` is the decorated face.
//!
//! # So this gate asserts an identity, and the measurement that earns it
//!
//! Three renders — `minecraft:shield` with `base_color` `red`, `light_blue` and
//! `None` — each held in the main hand against a pure-sky backdrop, mirroring
//! `first_person_banner_hand_pixels.rs`'s own camera and margin. All three come
//! out **byte-identical**, and that is the correct result.
//!
//! An earlier version of this file asserted the opposite — that red must
//! dominate blue by ten counts — and was left red on the record as "a real,
//! newly-found defect", with a leading hypothesis about double-sided geometry
//! and the depth test. The hypothesis was half right and the conclusion was
//! wrong. What the depth test rejects is not the pattern layer as a whole; it is
//! the pattern layer *on the face that is behind*, which is precisely the face
//! carrying the pattern. Measured by rebuilding
//! `EntityPipeline::banner_layer_pipeline` at four depth comparisons and
//! re-running these same three renders:
//!
//! | `depth_compare` | red vs plain, differing pixels | red mean channels |
//! |---|---|---|
//! | `Less` | 0 | — |
//! | `Equal` | 0 | — |
//! | `LessEqual` (shipped) | 0 | `[19.7, 16.0, 14.0]` |
//! | `Greater` | 10381 | — |
//! | `Always` | 10381 | `[40.1, 12.0, 12.4]` |
//!
//! So the layer's geometry, its placement, its mask and its tint are all real
//! and correct — under `Greater` the red channel doubles and the pattern
//! appears — and they sit at strictly greater depth than the base draw wrote,
//! i.e. on the far face. `LessEqual` is vanilla's own
//! `DepthStencilState.DEFAULT` (`GREATER_THAN_OR_EQUAL` under this engine's
//! `[0, 1]` depth), so rejecting them is what vanilla does too. **That table is
//! a recorded measurement, not something this gate re-runs**: it needs a
//! pipeline rebuilt at a different depth comparison, which a test cannot ask for
//! through any public seam here.
//!
//! What the gate *does* run is the identity plus three controls that keep it
//! from being vacuous — a byte-identity assertion is otherwise satisfied by a
//! renderer that draws nothing at all.
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
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

/// Count of pixels whose RGB differs between two frames.
fn differing(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(x, y)| x[..3] != y[..3])
        .count()
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_held_shield_shows_its_back_so_every_dye_renders_identically() {
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

    // --- Control 1: the two renders below are NOT fed the same inputs. ---
    //
    // A byte-identity assertion over three frames is trivially satisfied when
    // the three frames were built from one description, so the divergence has to
    // be shown at the source. `shield_has_patterns` splits on the dye, and
    // `shield_item_rig` hands the two halves *different base sheets* — the whole
    // reason `shield_base_nopattern` exists.
    let dyed_rig = lodestone_render::shield_item_rig(lodestone_render::shield_has_patterns(
        Some("red"),
        0,
    ));
    let plain_rig =
        lodestone_render::shield_item_rig(lodestone_render::shield_has_patterns(None, 0));
    eprintln!("dyed rig  = {dyed_rig:?}");
    eprintln!("plain rig = {plain_rig:?}");
    assert_ne!(
        dyed_rig.1, plain_rig.1,
        "a dyed and a plain shield must resolve to different base sheets, or the \
         byte-identity assertion below is measuring one description rendered twice"
    );

    // --- Control 2: a dyed shield really does produce a named pattern layer. ---
    //
    // `ShieldSpecialRenderer.submit`'s `base` layer, with no stored patterns at
    // all. If this list were empty the identity below would hold for the boring
    // reason that nothing extra was ever submitted.
    for dye in ["red", "light_blue"] {
        let base_dye = lodestone_render::DyeColor::from_name(dye)
            .unwrap_or_else(|| panic!("{dye} is a vanilla dye"));
        let layers = lodestone_render::shield_pattern_layers(base_dye, &[]);
        eprintln!(
            "{dye} pattern layers = {:?}",
            layers
                .iter()
                .map(|l| l.sprite.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            layers.len(),
            1,
            "a dyed shield with no stored patterns must submit exactly the `base` layer"
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
            custom_model_data: None,
            dyed_color: None,
                potion_color: None,
                banner_patterns: Vec::new(),
                base_color: base_color.clone(),
                skin: None,
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
    eprintln!(
        "red shield:        drawn={red_drawn} draw_calls={red_draw_calls} px={red_n} \
         mean_rgb={red_rgb:?}"
    );
    eprintln!(
        "light_blue shield: drawn={blue_drawn} draw_calls={blue_draw_calls} px={blue_n} \
         mean_rgb={blue_rgb:?}"
    );
    eprintln!(
        "plain shield:      drawn={plain_drawn} draw_calls={plain_draw_calls} px={plain_n} \
         mean_rgb={plain_rgb:?}"
    );

    assert!(
        red_drawn && blue_drawn && plain_drawn,
        "the hand pass did not report a first-person item drawn for every shield \
         variant — `prepare_special_hand`'s `minecraft:shield` branch either did not \
         run or `shield_item_rig` resolved nothing"
    );

    // --- Control 3: something is actually on screen. ---
    //
    // This is the control the identity assertion needs most: three empty frames
    // are byte-identical too.
    assert!(
        red_n > 30,
        "a held red shield reached only {red_n} non-sky pixels — near-zero means a \
         degenerate draw, not merely dim, and would make the identity below vacuous"
    );
    assert!(blue_n > 30, "same near-zero failure for the light_blue shield ({blue_n} px)");
    assert!(plain_n > 30, "same near-zero failure for the plain shield ({plain_n} px)");

    // A dyed shield draws two batches (the opaque base + the translucent
    // dye/pattern layer, `shield_has_patterns` true); a plain one draws only
    // the opaque base (`shield_has_patterns` false) — see that function's own
    // doc. Asserting the *difference* rather than an absolute floor is what
    // makes this catch a regression that always issues the translucent pass (or
    // never does), and it is what separates "the layer is submitted and lands
    // behind the base" from "the layer stopped being submitted at all" — the
    // two states this gate's own frames cannot tell apart.
    assert!(
        red_draw_calls > plain_draw_calls,
        "a dyed shield (base+pattern layer) must issue more draw calls than a plain \
         one (base only): red={red_draw_calls}, plain={plain_draw_calls}"
    );
    assert!(
        blue_draw_calls > plain_draw_calls,
        "same for light_blue vs plain: light_blue={blue_draw_calls}, plain={plain_draw_calls}"
    );

    // --- The assertion. ---
    //
    // The face this camera sees is the grip side, and every shield mask is blank
    // over it, so the dye cannot reach a pixel here. See this file's own header
    // for the derivation from `shield.json`'s two display transforms and for the
    // depth-comparison sweep that located the pattern on the far face.
    let red_vs_blue = differing(&red_pixels, &blue_pixels);
    let red_vs_plain = differing(&red_pixels, &plain_pixels);
    eprintln!("red vs light_blue: {red_vs_blue} differing px");
    eprintln!("red vs plain:      {red_vs_plain} differing px");
    assert_eq!(
        red_vs_blue, 0,
        "a red and a light_blue shield differ in {red_vs_blue} pixels in the \
         first-person hand. This surface shows the shield's BACK — `shield.json`'s \
         `firstperson_righthand` is `rotation [0, 180, 5]` against its `gui`'s \
         `[15, -25, -5]`, so the two views show opposite faces of a one-texel plate, \
         and the GUI is the one showing the decorated front. A difference here means \
         the flip is no longer being applied and the shield is back-to-front again, \
         which is the defect the `\"transformation\"` inheritance fix closed. Check \
         `compose_special_node_transform` and the `minecraft:condition` node's \
         `scale [1, -1, -1]`."
    );
    assert_eq!(
        red_vs_plain, 0,
        "a dyed and a plain shield differ in {red_vs_plain} pixels in the \
         first-person hand, for the same reason — and note the two also use \
         *different base sheets* ({:?} against {:?}), whose 200 differing texels sit \
         inside the front-face UV rect. Those being invisible is the same fact as \
         the dye being invisible.",
        dyed_rig.1,
        plain_rig.1
    );
}
