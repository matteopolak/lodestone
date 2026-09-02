//! Hermetic gates for [`super::RenderState`]: the fog/clear-colour day-night
//! track, vanilla's fog span, the humanoid-armour slot mapping, and the
//! third-person body's resolve through the real model corpus.
//!
//! Nothing here needs a wgpu adapter — the pixel gates that do live in
//! [`super::pixel_gates`]. Two tests are `#[ignore]`d anyway because they need
//! the vanilla pack under `.cache/mc/<ver>`, which is a *pack* gate, not a GPU
//! one.
use glam::Vec3;
use lodestone_render::fog::FogUniform;

use crate::entities::EntityDraw;

use super::*;

/// **Gate A (F1).** The terrain/entity fog colour must carry the
/// `FOG_COLOR` day/night track, not just the sky disc. Before this fix
/// `fog_with_clock` returned `self.fog`'s flat day colour unchanged at every
/// hour — a full-brightness `#87B5EB` terrain fog at midnight against a
/// near-black sky disc.
///
/// Expected bytes are hand-derived from vanilla, not from this crate's own
/// formula: `NIGHT_FOG_COLOR_MULTIPLIER_START` is vanilla's own float-to-color
/// construction of `(1.0, 0.05, 0.05, 0.09)` and `..._END` the same for `(1.0, 0.09, 0.09, 0.09)`
///, where vanilla's own float-to-8-bit-channel conversion **floors**
/// (`Mth.floor(value * 255.0F)`) — `0.05*255=12.75`
/// floors to `12`, `0.09*255=22.95` floors to `22`, giving multiplier
/// keyframes `(12,12,22)` at tick 13670 and `(22,22,22)` at tick 22330, not
/// the `(13,13,22)` an earlier draft of this investigation misread from a
/// rounded guess. At tick 18000 (exactly the segment midpoint, `alpha =
/// 4330/8660 = 0.5`) `Mth.lerpInt`'s floor gives `(17,17,22)`.
/// Vanilla's own ARGB channel-multiply is truncating integer division
/// (`red(lhs) * red(rhs) / 255`), so against our day
/// base `SKY_COLOR` (`#87B5EB` = `(135,181,235)`):
///
/// | tick | multiplier | predicted (exact integer) |
/// |---|---|---|
/// | 6000 (noon) | `(255,255,255)` (flat white region) | `(135,181,235)` unchanged |
/// | 18000 (midnight) | `(17,17,22)` | `(9,12,20)` |
/// | 13670 (dusk, on-keyframe) | `(12,12,22)` | `(6,8,20)` |
///
/// This client's `multiply_gamma` round-trips through the continuous
/// piecewise sRGB transfer function rather than vanilla's raw truncating
/// byte division, so a **2-byte** tolerance is used — the same allowance
/// this repo's other gamma-space gates use for that reason.
#[test]
fn fog_with_clock_carries_the_night_track_gate_a() {
    let fog = FogSettings::for_render_distance(SKY_COLOR, 8);
    let to_bytes = |c: [f32; 3]| {
        c.map(|v| (lodestone_render::fog::linear_to_srgb_f32(v) * 255.0).round() as i32)
    };
    let byte_of = |u: &FogUniform| {
        to_bytes([u.color_start[0], u.color_start[1], u.color_start[2]])
    };

    let assert_close = |label: &str, got: [i32; 3], want: [i32; 3]| {
        for i in 0..3 {
            assert!(
                (got[i] - want[i]).abs() <= 2,
                "{label}: channel {i} got {} want {} (±2), full got {got:?} want {want:?}",
                got[i],
                want[i]
            );
        }
    };

    // Noon: the track's flat white region, so our own base colour must
    // survive untouched. This is the discriminating row that a fix which
    // darkens fog at *every* tick (not just night) would fail.
    let overworld_ambient = lodestone_render::light::OVERWORLD_AMBIENT_LIGHT;
    let noon =
        RenderState::fog_uniform_for(&fog, 6000, 1.0, overworld_ambient, [0.0, 0.0, 0.0], 0.0);
    assert_close("noon", byte_of(&noon), [135, 181, 235]);

    // Midnight: the "too extreme" complaint's root cause.
    let midnight =
        RenderState::fog_uniform_for(&fog, 18000, 0.24, overworld_ambient, [0.0, 0.0, 0.0], 0.0);
    assert_close("midnight", byte_of(&midnight), [9, 12, 20]);

    // Dusk, exactly on the first night keyframe.
    let dusk =
        RenderState::fog_uniform_for(&fog, 13670, 0.5, overworld_ambient, [0.0, 0.0, 0.0], 0.0);
    assert_close("dusk", byte_of(&dusk), [6, 8, 20]);

    // The sky-darken lane is untouched by this change and must still ride
    // through in end_enabled[2] — proves the fix is additive, not a
    // replacement of the existing clock plumbing.
    assert_eq!(midnight.end_enabled[2], 0.24);

    // Negative control, executed and observed to fail: the pre-fix
    // behaviour (`FogUniform::new` with no clock at all) must land on the
    // flat day colour at midnight, i.e. the detector actually fires on the
    // bug this test exists to catch.
    let unclocked = FogUniform::new(&fog, [0.0, 0.0, 0.0]);
    assert_close("control (no clock)", byte_of(&unclocked), [135, 181, 235]);
    let diff = (byte_of(&unclocked)[2] - byte_of(&midnight)[2]).abs();
    assert!(
        diff > 100,
        "control must clearly disagree with the fixed midnight value: {diff}"
    );
}

/// [`RenderState::fog_uniform_for`]'s new `now_secs` parameter must land in
/// `ambient_light`'s `w` lane and nowhere else — a transposition here would
/// silently feed the section fade-in the wrong number while leaving every
/// other fog byte correct, so this checks the lane directly rather than
/// inferring it from a rendered colour. Also checks `rgb` is untouched by the
/// new argument, since the two ride the same `vec4`.
#[test]
fn fog_uniform_for_carries_now_secs_in_the_ambient_light_w_lane() {
    let fog = FogSettings::for_render_distance([0.5, 0.6, 0.7], 8);
    let ambient = [0.1, 0.2, 0.3];
    let u = RenderState::fog_uniform_for(&fog, 6000, 1.0, ambient, [0.0, 0.0, 0.0], 41.0);
    assert_eq!(u.ambient_light, [0.1, 0.2, 0.3, 41.0]);
}

/// [`RenderState::set_clear_color_tracked`]'s pure core must land on the
/// same tracked value [`fog_with_clock_carries_the_night_track_gate_a`]
/// pins for terrain fog — the clear colour and the fog colour derive from
/// one function and one clock, so they cannot drift apart the way
/// `docs/dimension-visuals.md`'s wiring note warns a second copy would.
#[test]
fn clear_color_tracked_matches_the_fog_colour_at_the_same_tick() {
    let to_bytes = |c: [f32; 3]| {
        c.map(|v| (lodestone_render::fog::linear_to_srgb_f32(v) * 255.0).round() as i32)
    };
    for tick in [6000_i64, 18000, 13670] {
        let clear = RenderState::clear_color_tracked_for(tick, SKY_COLOR);
        let fog = RenderState::fog_uniform_for(
            &FogSettings::for_render_distance(SKY_COLOR, 8),
            tick,
            1.0,
            lodestone_render::light::OVERWORLD_AMBIENT_LIGHT,
            [0.0, 0.0, 0.0],
            0.0,
        );
        let fog_rgb = [fog.color_start[0], fog.color_start[1], fog.color_start[2]];
        assert_eq!(
            to_bytes(clear),
            to_bytes(fog_rgb),
            "tick {tick}: clear and fog must track identically"
        );
    }
    // Negative control: the untracked day base must clearly disagree at
    // midnight, proving this actually reads the clock rather than being a
    // no-op wrapper around `set_clear_color`.
    let clear_midnight = RenderState::clear_color_tracked_for(18000, SKY_COLOR);
    assert_ne!(to_bytes(clear_midnight), to_bytes(SKY_COLOR));
}

/// That fix. [`FOG_START_FRACTION`] is the shell's last fraction-shaped
/// fog knob — `sim::fog_for_render_distance` still multiplies by it instead
/// of calling
/// [`FogSettings::for_render_distance`](lodestone_render::fog::FogSettings::for_render_distance).
/// This asserts the two agree exactly wherever anyone plays, and states the
/// divergence outside that range as a number rather than leaving it to be
/// rediscovered.
///
/// `0.9` is not a taste value: it is `1 - span/rd_blocks` wherever vanilla's
/// `clamp(rd_blocks/10, 4, 64)` is unclamped, which is an algebraic
/// identity, not a fit.
#[test]
fn fog_start_fraction_matches_vanillas_span() {
    use lodestone_render::fog::{render_distance_fade_span, FogSettings};

    // Render distances 3..=40 (48..=640 blocks) are the unclamped middle of
    // vanilla's formula, and the fraction is exact across all of them.
    for rd in 3..=40u32 {
        let blocks = rd as f32 * 16.0;
        let exact = FogSettings::for_render_distance(SKY_COLOR, rd);
        let via_fraction = blocks * FOG_START_FRACTION;
        assert!(
            (exact.start - via_fraction).abs() < 1e-3,
            "RD {rd}: span form starts at {:.3}, fraction form at {via_fraction:.3}",
            exact.start
        );
        assert_eq!(exact.end, blocks, "RD {rd}: fog must end at the view distance");
    }

    // Outside it, only the span form has the floor and the cap. These two
    // literals are the whole cost of not having migrated the call site.
    assert!((FogSettings::for_render_distance(SKY_COLOR, 2).start - 28.0).abs() < 1e-3);
    assert!((32.0 * FOG_START_FRACTION - 28.8).abs() < 1e-3);
    assert!((FogSettings::for_render_distance(SKY_COLOR, 48).start - 704.0).abs() < 1e-3);
    assert!((768.0 * FOG_START_FRACTION - 691.2).abs() < 1e-3);

    // The control for the value itself: the fraction this shipped with does
    // *not* satisfy the identity above, and misses by tens of blocks at
    // every render distance — which is the bug, stated as a measurement.
    const SHIPPED_UNTIL_388: f32 = 0.75;
    for rd in [8u32, 16, 32] {
        let blocks = rd as f32 * 16.0;
        let exact = blocks - render_distance_fade_span(blocks);
        let old = blocks * SHIPPED_UNTIL_388;
        assert!(
            exact - old > 19.0,
            "RD {rd}: the old fraction was expected to start the fade far too \
             early; exact {exact:.1} vs old {old:.1}"
        );
    }
}

/// The bring-up default must be the same curve the shell will immediately
/// override it with, not a differently-shaped placeholder — a mismatch here
/// is a one-frame flash of the wrong fog on every launch.
#[test]
fn bring_up_fog_default_uses_vanillas_span() {
    let d = FogSettings::for_render_distance(SKY_COLOR, DEFAULT_RENDER_DISTANCE_CHUNKS);
    assert_eq!((d.start, d.end), (115.2, 128.0));
    assert_eq!(d.color, SKY_COLOR, "and fades into the colour the frame clears to");
}
/// `Body` and `Saddle` must never reach the humanoid armour path, and the
/// four that must are mapped exactly once each.
///
/// This is the mapping a fold of `"body"` into `Chest` would break, and it
/// has already been shipped wrong once on the census side — wolf armour and
/// horse barding both live in `Body`, so the visible symptom is a player
/// wearing a horse's diamond barding as a chestplate.
#[test]
fn only_the_four_humanoid_slots_map_to_armour() {
    use lodestone_assets::equipment::ArmourSlot;

    assert_eq!(
        humanoid_armour_slot(EquipmentSlot::Head),
        Some(ArmourSlot::Head)
    );
    assert_eq!(
        humanoid_armour_slot(EquipmentSlot::Chest),
        Some(ArmourSlot::Chest)
    );
    assert_eq!(
        humanoid_armour_slot(EquipmentSlot::Legs),
        Some(ArmourSlot::Legs)
    );
    assert_eq!(
        humanoid_armour_slot(EquipmentSlot::Feet),
        Some(ArmourSlot::Feet)
    );
    for slot in [
        EquipmentSlot::Body,
        EquipmentSlot::Saddle,
        EquipmentSlot::MainHand,
        EquipmentSlot::OffHand,
    ] {
        assert_eq!(
            humanoid_armour_slot(slot),
            None,
            "{slot:?} is not HUMANOID_ARMOR"
        );
    }
    // Every slot the model layer knows about is accounted for, so a new
    // vanilla slot fails here rather than being silently ignored.
    let mapped = EquipmentSlot::ALL
        .iter()
        .filter(|s| humanoid_armour_slot(**s).is_some())
        .count();
    assert_eq!(mapped, 4);
}

/// Every humanoid armour sheet 26.2 ships must actually decode out of the
/// real jar at the path [`lodestone_assets::equipment`] computes, at the
/// **64×32** the meshes' UVs assume.
///
/// Ignored without a pack rather than skipped silently: an empty map is the
/// fail-open production behaviour (armour just does not draw), which is
/// exactly the state a path typo would also produce, so the only way to tell
/// them apart is to assert against a real jar.
#[test]
#[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
fn every_humanoid_armour_sheet_decodes_from_the_real_jar() {
    use lodestone_assets::equipment::{ARMOUR_ASSETS, ArmourLayerType};

    let sheets = load_humanoid_armour_textures();
    assert!(
        !sheets.is_empty(),
        "no armour sheets loaded; set LODESTONE_ASSETS to a pack root with client.jar"
    );
    for asset in ARMOUR_ASSETS {
        for layer_type in [ArmourLayerType::Humanoid, ArmourLayerType::HumanoidLeggings] {
            for layer in asset.layers(layer_type) {
                let img = sheets
                    .get(&(layer.texture, layer_type))
                    .unwrap_or_else(|| panic!("{}/{:?} did not load", layer.texture, layer_type));
                assert_eq!(
                    (img.width, img.height),
                    (64, 32),
                    "{}/{:?} is not the 64x32 the armour meshes' UVs assume",
                    layer.texture,
                    layer_type
                );
            }
        }
    }
    // Nine `humanoid` sheets (7 plain materials + leather's two layers,
    // where turtle_scute replaces leather's single-layer slot) and eight
    // `humanoid_leggings` ones (no turtle leggings exist).
    assert_eq!(sheets.len(), 17, "expected 9 humanoid + 8 leggings sheets");
}

/// Banner masks resolve, and they resolve under **the key the draw site derives**.
///
/// `resolve_banner` hands back `BannerLayerDraw::sprite` as a full
/// `minecraft:entity/banner/<id>` location, while `BannerPatternAtlas` keys on the
/// bare `<id>`. `prepare_block_entities` bridges the two with a `rsplit('/')`, and
/// if that bridge is wrong every layer is silently skipped and a banner draws blank
/// white — an entirely plausible-looking banner. This is the join, checked on a real
/// pattern stack rather than an empty one.
#[test]
#[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
fn banner_masks_resolve_under_the_key_the_draw_site_derives() {
    use lodestone_assets::banner_pattern_atlas::BannerPatternAtlas;
    use lodestone_render::banner_pattern::{DyeColor, StoredPatternLayer};
    use lodestone_render::{BannerSpawn, BlockEntityModelSet};

    let manager = crate::resources::vanilla_manager()
        .expect("no vanilla client.jar under .cache/mc/<version>/; fetch it first");
    let masks = BannerPatternAtlas::load(&manager).expect("load banner pattern masks");
    assert!(!masks.is_empty(), "the mask atlas must not be empty");

    let models = BlockEntityModelSet::load();
    let mut spawn = BannerSpawn::at([4, 65, -9]);
    spawn.base_color = DyeColor::Red;
    spawn.patterns = vec![
        StoredPatternLayer { pattern_asset_id: "creeper".into(), color: DyeColor::Lime },
        StoredPatternLayer { pattern_asset_id: "stripe_downright".into(), color: DyeColor::Black },
    ];
    let resolved = models.resolve_banner(&spawn).expect("the banner rig is in the corpus");
    // Base plus the two stored layers — `banner_pattern_layers` always prepends
    // layer 0, so an empty-looking banner still has one mask to draw.
    assert_eq!(resolved.layers.len(), 3);
    for layer in &resolved.layers {
        let key = layer
            .sprite
            .path()
            .rsplit('/')
            .next()
            .expect("a sprite path always has a last segment");
        assert!(
            masks.get(key).is_some(),
            "{} did not resolve under the bare key {key:?}",
            layer.sprite
        );
    }
    // The base layer carries the block's dye, not white — the whole reason the
    // base colour rides the mask list rather than a per-instance tint.
    let base = &resolved.layers[0];
    assert_eq!(base.color, DyeColor::Red.gamma_rgb());
}

/// The trim-sprite loader against the real jar — the entry point that
/// did not exist while `lodestone_assets::trim` had zero callers.
///
/// Asserts the two things that would silently produce untrimmed armour: that the
/// bake is non-empty, and that the keys it stores are the *same*
/// `trim_sprite_id` outputs `RenderState::trim_sprite_for` derives at draw time. A
/// loader keyed on anything else would look healthy here and miss every lookup.
#[test]
#[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
fn trim_sprites_bake_and_key_the_way_the_draw_site_looks_them_up() {
    use lodestone_assets::equipment::{ArmourLayerType, ArmourSlot, armour_item};
    use lodestone_assets::trim::{trim_material, trim_pattern, trim_sprite_id};

    let sprites = super::entities::load_trim_sprites();
    assert!(
        !sprites.is_empty(),
        "no trim sprites loaded; set LODESTONE_ASSETS to a pack root with client.jar"
    );

    // Gold `sentry` on a diamond chestplate, derived exactly as the draw site
    // derives it — including the wearer's own asset id, which is what makes
    // `suffix_for`'s same-material override reachable.
    let pattern = trim_pattern("sentry").expect("sentry is a 26.2 pattern");
    let material = trim_material("gold").expect("gold is a 26.2 trim material");
    let (_, asset) = armour_item("diamond_chestplate").expect("diamond chestplate is armour");
    let id = trim_sprite_id(pattern, material, ArmourSlot::Chest.layer_type(), asset.id)
        .expect("a well-formed sprite id");
    assert!(sprites.contains_key(&id), "{id} was not baked");

    // The same-material override is a *different* sprite, not the same one: a
    // loader that ignored `suffix_for` would collapse the two and diamond trim
    // would vanish into diamond armour.
    let diamond = trim_material("diamond").expect("diamond is a 26.2 trim material");
    let plain = trim_sprite_id(pattern, diamond, ArmourLayerType::Humanoid, "iron").unwrap();
    let darker = trim_sprite_id(pattern, diamond, ArmourLayerType::Humanoid, "diamond").unwrap();
    assert_ne!(plain, darker);
    assert!(sprites.contains_key(&plain) && sprites.contains_key(&darker));
}

/// Hermetic (no GPU): the whole armour resolution chain a live frame runs,
/// from the `EntityDraw` the extract system produces through to the
/// `(index range, wearer part)` pairs `prepare_armour` uploads.
///
/// This is the *island* check for armour minus the pixels: it asserts that a
/// zombie wearing a full diamond set produces attach points on a wearer
/// resolved through the real corpus, and that each one indexes a real
/// `part_transforms` entry with a positive determinant. What it cannot see —
/// that `prepare_armour` is actually called and its batches drawn — is
/// covered by `render_inner` calling it unconditionally next to
/// `prepare_entities`.
#[test]
fn a_fully_armoured_zombie_resolves_layers_on_real_wearer_parts() {
    use lodestone_assets::ResourceLocation as Rl;
    use lodestone_assets::equipment::ArmourSlot;
    use lodestone_render::entity::{armour_layer_tint, armour_layers};

    let models = EntityModelSet::load();
    let armour = ArmourModelSet::load();
    let draw = EntityDraw {
        hurt: false,
        id: 7,
        type_path: std::sync::Arc::from("zombie"),
        item: None,
        item_model: None,
        item_skin: None,
        equipment: vec![
            (
                EquipmentSlot::Head,
                Rl::parse("minecraft:diamond_helmet").unwrap(),
            ),
            (
                EquipmentSlot::Chest,
                Rl::parse("minecraft:leather_chestplate").unwrap(),
            ),
            (
                EquipmentSlot::Legs,
                Rl::parse("minecraft:iron_leggings").unwrap(),
            ),
            (
                EquipmentSlot::Feet,
                Rl::parse("minecraft:golden_boots").unwrap(),
            ),
            // Must be ignored: animal armour, not humanoid.
            (
                EquipmentSlot::Body,
                Rl::parse("minecraft:diamond_horse_armor").unwrap(),
            ),
        ],
        // No dye reported for any slot in this fixture — this test is
        // about armour *resolution* (real wearer parts), not tint, and
        // an absent dye is `armour_layer_tint_with_dye`'s own "undyed"
        // case (`docs/armour-rendering.md`).
        equipment_dye: Vec::new(),
        equipment_skin: Vec::new(),
            equipment_trim: Vec::new(),
        feet: Vec3::new(4.0, 70.0, -2.0),
        yaw: 41.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput {
            head_yaw_deg: 5.0,
            head_pitch_deg: -3.0,
            limb_swing: 2.0,
            limb_swing_amount: 0.8,
            attack_anim: 0.0,
            age_ticks: 11.0,
            aggressive: false,
            ..AnimInput::REST
        },
        wool: None,
        block_state: None,
        item_frame_rotation: 0,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        name_tag: None,
        item_use: None,
        // Right-handed: this test is about armour resolution, not handedness.
        main_arm_left: false,
        // Not a creeper: only a creeper ever swells.
        creeper_swelling: 0.0,
        // A zombie, not a player — this build's swim rotation only reads this
        // for `type_path == "player"`.
        swim_amount: 0.0,
        death_time: 0.0,
        // No flame overlay from this construction site.
        on_fire: false,
        // Not invisible and not an armour stand.
        invisible: false,
        armor_stand: None,
        // Not a player, so no skin can apply.
        player_skin: None,
        variant_sheet: None,
        // Not an experience orb either.
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
    };

    let instance = models
        .resolve(&draw.type_path, draw.feet, draw.yaw, draw.scale, &draw.anim)
        .expect("zombie resolves");
    let wearer = models.get(instance.model).expect("zombie mesh");

    let mut layer_count = 0;
    let mut attach_count = 0;
    for slot in ArmourSlot::ALL {
        let (_, id) = draw
            .equipment
            .iter()
            .find(|(s, _)| humanoid_armour_slot(*s) == Some(slot))
            .unwrap_or_else(|| panic!("{slot:?} equipped"));
        let layers = armour_layers(slot, id.path());
        assert!(!layers.is_empty(), "{slot:?} ({id}) resolved no layers");
        // Leather is the two-layer case; everything else is one.
        assert_eq!(
            layers.len(),
            if id.path().starts_with("leather") { 2 } else { 1 },
            "{slot:?} ({id}) layer count"
        );
        let mesh = armour.get(slot).expect("slot mesh");
        for (range, wearer_index) in mesh.attach(&wearer.skeleton) {
            let m = instance
                .part_transforms
                .get(wearer_index)
                .expect("wearer part index is in range");
            assert!(range.index_count > 0);
            assert!(
                m.determinant() > 0.0,
                "{slot:?} armour rides a negative-determinant wearer matrix"
            );
            attach_count += 1;
        }
        layer_count += layers.len();
    }
    // 1 diamond helmet layer + 2 leather + 1 iron + 1 golden.
    assert_eq!(layer_count, 5);
    // head+hat, body+arms, body+legs, legs.
    assert_eq!(attach_count, 2 + 3 + 3 + 2);

    // `Body` contributed nothing: the horse armour must not have been read
    // as a chestplate.
    assert!(
        armour_layers(ArmourSlot::Chest, "diamond_horse_armor").is_empty(),
        "animal armour must not resolve as humanoid armour"
    );
    // And the leather tint is vanilla's undyed brown, in gamma bytes.
    let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
    assert_eq!(
        armour_layer_tint(&leather[0]),
        lodestone_assets::equipment::UNDYED_LEATHER_RGB
    );
}

/// The sky reference must stay a plausible blue in the readback's own space;
/// a value that drifted to the *displayed* colour would blow the "is this
/// pixel sky?" test open, which is exactly how the two gates below broke.
#[test]
fn sky_reference_tracks_the_clear_colour() {
    assert_eq!(sky_clear_bytes(), [62, 118, 211]);
}

/// The local player's own skin must survive [`ThirdPersonBodyState::into_draw`]
/// — on **both** channels the world entity pass consults, and it consulted
/// neither for us before this.
///
/// # What this gate covers, and what it deliberately does not
///
/// It covers the *plumbing*: `into_draw` hardcoded `player_skin: None` and
/// `variant_sheet: None` for as long as it existed, so a resolution arriving
/// on `ThirdPersonBodyState` would have been discarded one line before the
/// draw. That is a claim about this function and this gate settles it.
///
/// It does **not** cover the producer. This fixture installs its own
/// `ThirdPersonBodyState`, so it proves nothing about whether
/// `Sim::third_person_body_state` fills that field in a real session — which
/// is the other half of the same bug and needs a live `NetClient` (the local
/// uuid and the tab list both come through it) that no hermetic harness here
/// builds. Stated rather than implied, because a passing gate over an
/// installed input reads as broader evidence than it is.
///
/// The two channels are asserted separately because they fail differently and
/// a fetched sheet masks the identity one: `player_skin.url` selects a bind
/// group in `player_skins` once a fetch lands, while `variant_sheet` is the
/// built-in identity that draws until (or unless) it does. Setting only the
/// first leaves us on the pack's plain Steve for every offline-mode server.
#[test]
fn the_local_bodys_own_skin_reaches_both_draw_channels() {
    let skin = crate::remote_skins::RemoteSkin {
        url: "https://textures.minecraft.net/texture/feedface".to_owned(),
        model: lodestone_assets::PlayerModelType::Slim,
        cape: None,
        // A non-legacy identity on purpose: `steve`/`alex` are what the plain
        // model sheets already look like, so either would pass under the very
        // collapse this field exists to undo.
        default_sheet: "entity/player/slim/ari",
    };
    let state = ThirdPersonBodyState {
        player_skin: Some(skin.clone()),
        feet: Vec3::ZERO,
        body_yaw_deg: 0.0,
        anim: AnimInput::REST,
        scale: 1.0,
        swim_amount: 0.0,
        slim: skin.model.is_slim(),
        equipment: Vec::new(),
        equipment_skin: Vec::new(),
    };
    let draw = state.into_draw();
    assert_eq!(
        draw.player_skin.as_ref().map(|s| s.url.as_str()),
        Some(skin.url.as_str()),
        "our own fetched sheet must reach EntityDraw::player_skin, or the batch \
         key carries no skin and the draw binds the model's own texture"
    );
    assert_eq!(
        draw.variant_sheet,
        Some("entity/player/slim/ari"),
        "our own built-in identity must reach EntityDraw::variant_sheet, or we \
         draw the pack's plain rig sheet whenever the fetch has not landed"
    );
    // And the rig still tracks the sheet, which is the pairing that makes a
    // skin look right rather than a texel out at the shoulder.
    assert_eq!(draw.type_path.as_ref(), "player_slim");
}

/// Hermetic (no GPU, no device): [`ThirdPersonBodyState::into_draw`] must
/// hand back exactly the [`EntityDraw`] shape [`RenderState::render_inner`]
/// folds into a frame's entity list, and that draw must actually resolve
/// through the real model corpus — including the outer-layer overlay
/// parts and a positive-determinant pose for every part — for *both*
/// skin rigs. [`EntityModelSet::load`]/`resolve` are pure CPU (baking
/// happens once at load, not per frame), so this needs no wgpu adapter.
#[test]
fn third_person_body_state_resolves_through_the_real_corpus() {
    let models = EntityModelSet::load();
    for slim in [false, true] {
        let state = ThirdPersonBodyState {
            // No skin: this fixture installs a body to suppress the first-person
            // arm, not to assert a sheet. The draw falls back to the model's own
            // texture, exactly as it did before this field existed.
            player_skin: None,
            feet: Vec3::new(1.0, 2.0, 3.0),
            body_yaw_deg: 123.0,
            anim: AnimInput {
                head_yaw_deg: 10.0,
                head_pitch_deg: -5.0,
                limb_swing: 2.0,
                limb_swing_amount: 1.0,
                attack_anim: 0.0,
                age_ticks: 15.0,
                aggressive: false,
                ..AnimInput::REST
            },
            scale: 1.0,
            // Nonzero and distinct from every other numeric field above, so
            // a transposition or a dropped assignment in `into_draw` cannot
            // hide behind a coincidental zero — see the discriminating
            // assertion below.
            swim_amount: 0.42,
            slim,
            equipment: Vec::new(),
            equipment_skin: Vec::new(),
        };
        let expected_model = if slim { "player_slim" } else { "player_wide" };
        let draw = state.clone().into_draw();
        assert_eq!(draw.id, LOCAL_PLAYER_DRAW_ID);
        assert_eq!(draw.type_path.as_ref(), expected_model);
        assert_eq!(draw.feet, state.feet);
        assert_eq!(draw.yaw, state.body_yaw_deg);
        assert_eq!(draw.scale, state.scale);
        assert_eq!(draw.anim, state.anim);
        // The body-pitch swim ramp must reach the draw the local player's
        // body actually renders from — this is the assertion that would have
        // caught `swim_amount: 0.0` being hardcoded in `into_draw`.
        assert_eq!(
            draw.swim_amount, state.swim_amount,
            "ThirdPersonBodyState::swim_amount did not reach EntityDraw::swim_amount"
        );
        assert!(draw.item.is_none());
        assert!(draw.equipment.is_empty());

        let instance = models
            .resolve(&draw.type_path, draw.feet, draw.yaw, draw.scale, &draw.anim)
            .unwrap_or_else(|| panic!("{expected_model} must resolve through the corpus"));
        assert_eq!(instance.model, expected_model);
        let mesh = models.get(expected_model).expect("mesh");
        for overlay in [
            "hat",
            "jacket",
            "right_sleeve",
            "left_sleeve",
            "right_pants",
            "left_pants",
        ] {
            assert!(
                mesh.skeleton.index_of(overlay).is_some(),
                "{expected_model} is missing its outer-layer part {overlay:?} — an \
                 omitted overlay looks like a missing-skin-layer bug, not a missing \
                 feature"
            );
        }
        for (i, part) in instance.part_transforms.iter().enumerate() {
            assert!(
                part.determinant() > 0.0,
                "{expected_model} part {i}: determinant must be positive, was {}",
                part.determinant()
            );
        }
    }
}

