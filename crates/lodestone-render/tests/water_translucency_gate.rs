//! Vanilla-sourced gates for the owner's two live-singleplayer water reports:
//! "flowing too fast" and "should be more translucent".
//!
//! # What it is
//!
//! Two independent gates against **outside** expectations read straight out of
//! the pinned `client.jar`, not from our own encoder/decoder:
//!
//! * [`built_atlas_carries_vanillas_water_alpha_and_lava_stays_opaque`] decodes
//!   `water_still.png`/`water_flow.png`/`lava_still.png` directly with `png`'s
//!   own indexed-PNG `tRNS` handling and asserts every alpha byte in the
//!   **production-built** atlas (`BlockModels::build`, the exact atlas the
//!   shell uploads) at each fluid sprite's region matches. Water must land on
//!   `180/255`; lava is the negative control and must land on `255` — a gate
//!   that forced everything translucent would fail lava too.
//! * [`animation_cycle_lengths_match_the_jars_mcmeta`] asserts the built
//!   timeline's per-frame hold and full cycle length against the two
//!   `.mcmeta` files' own declared `frametime` (`2` for still, the empty
//!   `{"animation": {}}` object for flow — which must default to `1`, not be
//!   silently mishandled) and the frame counts implied by each sprite's own
//!   strip height (`512/16` and `1024/32`).
//!
//! # Why this and not a smaller unit test
//!
//! Every individual link in this chain (`AnimationMeta::from_value`,
//! `frame_geometry`, `AnimTable::from_atlas`, `Image::decode_png`,
//! `atlas::blit`) already has narrower unit coverage elsewhere, and every one
//! of them read correct on inspection. That is exactly the situation
//! `DESIGN.md` §12 warns is invisible to a per-link review: the break, if any,
//! is a hop nobody walked end to end against a real fixture. This gate walks
//! the *whole* production path — the real jar, through `BlockModels::build`,
//! into the actual atlas bytes and the actual timeline — so it cannot be
//! fooled by a link that is individually correct but wired to the wrong
//! neighbour.
//!
//! `#[ignore]`d and fail-closed (a missing jar is an environment failure, never
//! a silent skip):
//!
//! ```text
//! cargo test -p lodestone-render --test water_translucency_gate -- --ignored --nocapture
//! ```

use lodestone_assets::{Image, ResourceLocation, ResourceManager, ZipSource};
use lodestone_render::{BlockModels, blocks_json_registry};

mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

/// Distinct alpha bytes found across every pixel of every physical frame of
/// `location` in the built atlas.
fn distinct_alphas_in_atlas(
    models: &BlockModels,
    location: &str,
) -> std::collections::BTreeSet<u8> {
    let loc: ResourceLocation = location.parse().expect("valid resource location");
    let atlas = models.atlas();
    let sprite = atlas
        .sprite(&loc)
        .unwrap_or_else(|| panic!("{location} missing from the built atlas"));
    let mut out = std::collections::BTreeSet::new();
    for frame in 0..sprite.frame_count.max(1) {
        let [x, y, w, h] = sprite
            .frame_pixel_rect(frame)
            .unwrap_or_else(|| panic!("{location} frame {frame} out of range"));
        for row in 0..h {
            for col in 0..w {
                let px = (y + row) as usize * atlas.width as usize + (x + col) as usize;
                out.insert(atlas.rgba[px * 4 + 3]);
            }
        }
    }
    out
}

/// Decodes a real jar sprite's raw alpha set directly (bypassing the atlas
/// entirely), for comparison against what survives the stitch.
fn distinct_alphas_in_raw_sprite(
    manager: &ResourceManager,
    location: &str,
) -> std::collections::BTreeSet<u8> {
    let loc: ResourceLocation = location.parse().expect("valid resource location");
    let bytes = manager
        .read_asset(&loc, "textures", "png")
        .unwrap_or_else(|| panic!("{location} missing from the jar"));
    let img = Image::decode_png(&bytes).expect("decode real jar sprite");
    let mut out = std::collections::BTreeSet::new();
    for y in 0..img.height {
        for x in 0..img.width {
            out.insert(img.pixel(x, y)[3]);
        }
    }
    out
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn built_atlas_carries_vanillas_water_alpha_and_lava_stays_opaque() {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");

    // Outside expectation: decode straight from the jar, independent of the
    // atlas-building code under test.
    let raw_water_still = distinct_alphas_in_raw_sprite(&manager, "minecraft:block/water_still");
    let raw_water_flow = distinct_alphas_in_raw_sprite(&manager, "minecraft:block/water_flow");
    let raw_lava_still = distinct_alphas_in_raw_sprite(&manager, "minecraft:block/lava_still");

    let mut mismatches = Vec::new();
    let mut check = |name: &str, raw: &std::collections::BTreeSet<u8>, atlas_loc: &str| {
        let built = distinct_alphas_in_atlas(&models, atlas_loc);
        if &built != raw {
            mismatches.push(format!(
                "{name}: raw jar decode gives alpha set {raw:?}, but the built atlas gives \
                 {built:?} at the same sprite's region"
            ));
        }
        built
    };

    let water_still_built = check("water_still", &raw_water_still, "minecraft:block/water_still");
    let water_flow_built = check("water_flow", &raw_water_flow, "minecraft:block/water_flow");
    let lava_still_built = check("lava_still", &raw_lava_still, "minecraft:block/lava_still");

    // Pin the actual numbers too, not just "raw == built" — a bug that
    // corrupted both identically would pass the comparison above and still be
    // wrong.
    if water_still_built != std::collections::BTreeSet::from([180]) {
        mismatches.push(format!(
            "water_still: expected alpha {{180}} (the jar's tRNS value on every \
             palette entry), got {water_still_built:?} in the built atlas"
        ));
    }
    if water_flow_built != std::collections::BTreeSet::from([180]) {
        mismatches.push(format!(
            "water_flow: expected alpha {{180}}, got {water_flow_built:?} in the built atlas"
        ));
    }
    // Negative control: lava has no tRNS chunk in the jar and must round-trip
    // fully opaque. A gate that forced every fluid translucent would still
    // pass the two checks above and fail only here.
    if lava_still_built != std::collections::BTreeSet::from([255]) {
        mismatches.push(format!(
            "lava_still (negative control): expected alpha {{255}} (no tRNS chunk in \
             the jar), got {lava_still_built:?} in the built atlas — a control failing \
             here means the gate cannot tell 'water is right' from 'everything is \
             translucent'"
        ));
    }

    assert!(
        mismatches.is_empty(),
        "water translucency mismatches between the real jar and the production-built atlas:\n{}",
        mismatches.join("\n")
    );
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn animation_cycle_lengths_match_the_jars_mcmeta() {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");
    let atlas = models.atlas();

    let mut mismatches = Vec::new();
    for (location, sprite_width) in [
        ("minecraft:block/water_still", 16u32),
        ("minecraft:block/water_flow", 32u32),
    ] {
        let loc: ResourceLocation = location.parse().expect("valid resource location");
        let sprite = atlas
            .sprite(&loc)
            .unwrap_or_else(|| panic!("{location} missing from the built atlas"));

        // The jar's own mcmeta, parsed independently of the atlas builder, is
        // the outside expectation for `frametime`.
        let meta_bytes = manager
            .read_asset(&loc, "textures", "png.mcmeta")
            .unwrap_or_else(|| panic!("{location} missing mcmeta"));
        let meta_json: serde_json::Value =
            serde_json::from_slice(&meta_bytes).expect("valid mcmeta json");
        let anim = meta_json
            .get("animation")
            .unwrap_or_else(|| panic!("{location}'s mcmeta has no \"animation\" section"));
        // Vanilla's own default when `frametime` is absent (as in
        // `water_flow`'s empty `{"animation": {}}`) is `1` — the exact case an
        // over-eager parser could silently mishandle.
        let expected_frametime = anim
            .get("frametime")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1) as u32;

        assert_eq!(
            sprite.frame_height, sprite_width,
            "{location}: vanilla's animated fluid sprites are square frames stacked \
             vertically, so frame_height must equal the sprite width ({sprite_width})"
        );
        assert!(
            sprite.frame_count >= 2,
            "{location}: expected an animated multi-frame strip, got frame_count \
             {} — this sprite is not exercising anything",
            sprite.frame_count
        );

        let anim_slot = sprite.anim_slot;
        if anim_slot == 0 {
            mismatches.push(format!(
                "{location}: anim_slot is 0 (treated as static) despite frame_count \
                 {} — the sprite never advances",
                sprite.frame_count
            ));
            continue;
        }
        let timeline = &models.sprite_animations()[anim_slot as usize - 1];
        let expected_cycle = u64::from(expected_frametime) * u64::from(sprite.frame_count);

        if timeline.frames.len() as u32 != sprite.frame_count {
            mismatches.push(format!(
                "{location}: timeline has {} frames, expected {} (one per physical \
                 strip frame, since the mcmeta's own \"frames\" list is empty/absent)",
                timeline.frames.len(),
                sprite.frame_count
            ));
        }
        for (i, f) in timeline.frames.iter().enumerate() {
            if f.hold_ticks != expected_frametime {
                mismatches.push(format!(
                    "{location} frame {i}: hold_ticks {} != expected frametime {}",
                    f.hold_ticks, expected_frametime
                ));
            }
        }
        let cycle = timeline.cycle_ticks();
        if cycle != expected_cycle {
            mismatches.push(format!(
                "{location}: cycle_ticks() {cycle} != expected {expected_cycle} \
                 ({expected_frametime} ticks/frame * {} frames)",
                sprite.frame_count
            ));
        }
    }

    // Cross-check the two fluid sprites are not accidentally sharing a cycle
    // length — the discriminating requirement between "flows too fast" being
    // real and being a false read of vanilla's own (correct) 2x speed
    // difference between still and flowing water.
    let still_loc: ResourceLocation = "minecraft:block/water_still".parse().unwrap();
    let flow_loc: ResourceLocation = "minecraft:block/water_flow".parse().unwrap();
    let still_sprite = atlas.sprite(&still_loc).expect("water_still in atlas");
    let flow_sprite = atlas.sprite(&flow_loc).expect("water_flow in atlas");
    if still_sprite.anim_slot != 0 && flow_sprite.anim_slot != 0 {
        let still_cycle =
            models.sprite_animations()[still_sprite.anim_slot as usize - 1].cycle_ticks();
        let flow_cycle =
            models.sprite_animations()[flow_sprite.anim_slot as usize - 1].cycle_ticks();
        if still_cycle != 2 * flow_cycle {
            mismatches.push(format!(
                "water_still's cycle ({still_cycle} ticks) is not exactly 2x water_flow's \
                 ({flow_cycle} ticks) — vanilla's still water is genuinely half the frame \
                 rate of flowing water by design (frametime 2 vs 1 over the same 32 \
                 frames), so a 1:1 or wrong ratio here is a real divergence from vanilla, \
                 not a false read of intentional vanilla behaviour"
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "animation timing mismatches between the real jar and the built timeline:\n{}",
        mismatches.join("\n")
    );
}
