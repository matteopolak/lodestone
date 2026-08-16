//! Beacon pyramid detection, primary/secondary effect selection and periodic
//! effect application (issue #616's `SET_BEACON` remainder).
//!
//! ## What it is
//!
//! The pure derivations `BeaconBlockEntity` bundles with its own tick/menu —
//! split out here so each is independently testable against a bare
//! [`ChunkSource`], with no player, connection or menu state involved.
//! `crate::server` and `crate::block_entities` own the stateful half: the
//! `BlockEntity::Beacon` variant, the menu, the payment-slot economy, the
//! `SET_BEACON` consumer, and the periodic call site that turns
//! [`beacon_effects`]'s output into a real [`crate::mob_effects::ActiveEffects::apply`]
//! plus the `UPDATE_MOB_EFFECT` packet a client actually needs to show the
//! buff.
//!
//! ## How it works
//!
//! Four independent derivations, each a direct port of one `BeaconBlockEntity`
//! method:
//!
//! - [`beacon_levels`] — `updateBase`: the pyramid tier (0–4) beneath a
//!   position, by checking each concentric square layer is entirely
//!   `minecraft:{iron,gold,emerald,diamond,netherite}_block`.
//! - [`beam_unobstructed`] — an approximation of `!beamSections.isEmpty()`.
//!   Vanilla tracks the beam as coloured segments (for the render); only
//!   *emptiness* gates effect application, so this checks "every block from
//!   directly above the beacon to the scan height is beam-transparent"
//!   rather than modelling segments or colour at all — `BeaconBlockEntity
//!   .tickBeam`'s own gate: `state.getLightDampening() >= 15 &&
//!   !state.is(Blocks.BEDROCK)` blocks; anything else (including every
//!   `BeaconBeamBlock` — glass/stained-glass/pane/beacon, which all carry
//!   `0` dampening anyway) passes. [`is_beam_transparent`] reads the real
//!   per-block-state [`lodestone_data::light_props::dampening`] census
//!   (the same table `lodestone-world`'s light engine uses) rather than
//!   membership in a hand-picked block family, so a low-opacity block
//!   outside that family — a carpet, a candle, a flower — now agrees with
//!   vanilla instead of being treated as blocking.
//! - [`required_levels_for`] / [`validate_beacon_effects`] — `getRequiredLevelsFor`
//!   / `validateEffects`, clause for clause (see that function's own doc for
//!   each one named).
//! - [`beacon_effects`] — `applyEffects`'s arithmetic: range, duration and
//!   amplifier for the primary and (level-4-only) secondary application.
//!   Returns *what* to apply; finding which players are in range and calling
//!   [`crate::mob_effects::ActiveEffects::apply`] is the caller's job, the
//!   same split [`beacon_levels`] keeps from the block-entity state that
//!   consumes it.
//!
//! ## How to change it
//!
//! [`BASE_BLOCKS`] and [`BEACON_EFFECT_TIERS`] are the two vanilla censuses
//! this module hardcodes (`minecraft:beacon_base_blocks` and
//! `BeaconBlockEntity.BEACON_EFFECTS` — see `docs/beacon.md` for both
//! citations). Neither is bundled as a data asset the way
//! `minecraft:beacon_payment_items` is (`crate::crafting::EMBEDDED_ITEM_TAGS`,
//! reused directly by [`is_beacon_payment_item`] below rather than a second
//! copy) — a block tag with no corresponding bundled JSON in this crate's
//! `assets/tags/block/`, so it is transcribed by hand and must be
//! re-transcribed if 26.2's block tag ever changes.
//!
//! ## Configuration
//!
//! None — every rule here is a vanilla constant, not a server setting.
//!
//! ## Dependencies
//!
//! [`crate::chunk::ChunkSource`] for the two block-reading derivations;
//! [`crate::crafting::EMBEDDED_ITEM_TAGS`] for the payment-item check.

use crate::chunk::ChunkSource;

/// `minecraft:beacon_base_blocks` — the five blocks one pyramid layer may be
/// built from. See this module's own doc for why this is transcribed rather
/// than read from a bundled tag asset.
pub const BASE_BLOCKS: [&str; 5] = [
    "minecraft:iron_block",
    "minecraft:gold_block",
    "minecraft:emerald_block",
    "minecraft:diamond_block",
    "minecraft:netherite_block",
];

/// The four beacon power tiers, index 0 = the tier a level-1 pyramid unlocks
/// — vanilla's `BeaconBlockEntity.BEACON_EFFECTS`. Tier 4 (regeneration) is
/// the level-4-only, secondary-only power [`validate_beacon_effects`]'s own
/// doc names.
pub const BEACON_EFFECT_TIERS: [&[&str]; 4] = [
    &["minecraft:speed", "minecraft:haste"],
    &["minecraft:resistance", "minecraft:jump_boost"],
    &["minecraft:strength"],
    &["minecraft:regeneration"],
];

/// Strips a `[...]` block-state property suffix — the same convention this
/// crate's other per-module private copies use (`fire.rs`, `growth_tick.rs`),
/// duplicated rather than shared so this module has no dependency on their
/// private items.
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

fn is_base_block(state: &str) -> bool {
    BASE_BLOCKS.contains(&base_name(state))
}

/// Blocks a beacon's beam continues through, for [`beam_unobstructed`] —
/// `BeaconBlockEntity.tickBeam`'s own gate, `!(state.getLightDampening() >=
/// 15 && !state.is(Blocks.BEDROCK))`. `state` carries the full block-state
/// string [`ChunkSource::block_state`] returns (bare name or
/// `name[prop=value,...]`), resolved to a global id via
/// [`lodestone_data::block_states::state_id`] — the same resolution
/// `crate::chunk::resolve_palette_state_id` already uses for this exact
/// string shape. An id this crate cannot resolve reads as dampening `0`
/// ([`lodestone_data::light_props::dampening`]'s own "unknown id" default),
/// which is the same fail-open direction [`is_base_block`] already takes for
/// an unrecognised state, not a new failure mode this function invents.
fn is_beam_transparent(state: &str) -> bool {
    if base_name(state) == "minecraft:bedrock" {
        return true;
    }
    let Some(id) = lodestone_data::block_states::state_id(state) else {
        return true;
    };
    lodestone_data::light_props::dampening(id) < 15
}

/// The beacon pyramid tier beneath `(x, y, z)` — vanilla's
/// `BeaconBlockEntity.updateBase`: for `step` `1..=4`, the whole
/// `(2*step+1) × (2*step+1)` square at `y - step` must be [`is_base_block`],
/// and the returned level is the highest `step` for which every layer from
/// `1` to `step` passed (the loop stops at the first failing layer, so a
/// broken layer 2 caps the result at `1` even if layers 3–4 would otherwise
/// qualify).
#[must_use]
pub fn beacon_levels<S: ChunkSource + ?Sized>(source: &S, x: i32, y: i32, z: i32) -> u8 {
    let mut levels = 0u8;
    for step in 1..=4i32 {
        let ly = y - step;
        let mut layer_ok = true;
        'layer: for lx in (x - step)..=(x + step) {
            for lz in (z - step)..=(z + step) {
                if !is_base_block(&source.block_state(lx, ly, lz)) {
                    layer_ok = false;
                    break 'layer;
                }
            }
        }
        if !layer_ok {
            break;
        }
        levels = u8::try_from(step).unwrap_or(4);
    }
    levels
}

/// Whether the beacon's beam is unobstructed — see this module's own doc
/// comment for exactly what this approximates and why. `scan_height` is the
/// number of blocks above `(x, y, z)` to check; the caller's dimension
/// height covers every real column (384 for the overworld/nether/end alike),
/// since [`ChunkSource`] has no height accessor of its own to default to.
#[must_use]
pub fn beam_unobstructed<S: ChunkSource + ?Sized>(source: &S, x: i32, y: i32, z: i32, scan_height: i32) -> bool {
    for dy in 1..=scan_height {
        let state = source.block_state(x, y + dy, z);
        if !is_beam_transparent(&state) {
            return false;
        }
    }
    true
}

/// The pyramid tier `effect` requires — vanilla's `getRequiredLevelsFor`:
/// the 1-based index of the [`BEACON_EFFECT_TIERS`] entry containing it, or
/// `None` for anything not a beacon power at all (vanilla's own
/// `Integer.MAX_VALUE` fallback, restated as `None` here because this
/// module has no sentinel level above `4`).
#[must_use]
pub fn required_levels_for(effect: &str) -> Option<u8> {
    BEACON_EFFECT_TIERS
        .iter()
        .position(|tier| tier.contains(&effect))
        .map(|i| u8::try_from(i + 1).unwrap_or(4))
}

/// Whether `primary`/`secondary` are a legal selection for a pyramid of
/// `levels` tiers — vanilla's `BeaconBlockEntity.validateEffects`, every
/// clause of its own body:
///
/// 1. A secondary pick needs the full level-4 pyramid, whatever it is.
/// 2. Each pick's own required tier must fit inside `levels` — an effect
///    [`required_levels_for`] cannot place (`None`) always fails this, the
///    same way vanilla's `Integer.MAX_VALUE` always exceeds any real level.
/// 3. The primary can never be the level-4-only power (tier 4,
///    regeneration) — only a *secondary* pick may be.
/// 4. A secondary, if present, must be either the level-4 power or
///    identical to the primary (the same-effect amplifier-boost stack) —
///    never a *different* tier-1..3 power.
#[must_use]
pub fn validate_beacon_effects(primary: Option<&str>, secondary: Option<&str>, levels: u8) -> bool {
    if secondary.is_some() && levels < 4 {
        return false;
    }
    let primary_level = primary.map_or(0, |e| required_levels_for(e).unwrap_or(u8::MAX));
    let secondary_level = secondary.map_or(0, |e| required_levels_for(e).unwrap_or(u8::MAX));
    if primary_level > levels || secondary_level > levels {
        return false;
    }
    if primary_level >= 4 {
        return false;
    }
    secondary_level == 0 || secondary_level >= 4 || primary == secondary
}

/// One player's earned beacon buff — an effect id, amplifier and duration
/// ready for [`crate::mob_effects::ActiveEffects::apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeaconEffect {
    /// The effect's canonical `minecraft:*` key.
    pub effect: String,
    /// `MobEffectInstance`'s amplifier (`0` = level I).
    pub amplifier: u32,
    /// Duration in game ticks.
    pub duration_ticks: i32,
}

/// The horizontal reach (blocks) and the primary/secondary applications a
/// beacon at pyramid tier `levels` grants — vanilla's
/// `BeaconBlockEntity.applyEffects`'s arithmetic, with no player query: the
/// caller finds who is within `range` blocks horizontally (and, per
/// vanilla's own `expandTowards(0, height, 0)`, anywhere from `range` below
/// to the top of the world) and calls
/// [`crate::mob_effects::ActiveEffects::apply`] with each returned
/// [`BeaconEffect`].
///
/// Empty when `primary` is `None` — vanilla's own `primaryPower != null`
/// gate; a beacon with no power selected applies nothing regardless of its
/// pyramid tier.
#[must_use]
pub fn beacon_effects(levels: u8, primary: Option<&str>, secondary: Option<&str>) -> (f64, Vec<BeaconEffect>) {
    let levels_f = f64::from(levels);
    let range = levels_f.mul_add(10.0, 10.0);
    let duration_ticks = (9 + i32::from(levels) * 2) * 20;
    let mut out = Vec::new();
    let Some(primary) = primary else {
        return (range, out);
    };
    let base_amp = u32::from(levels >= 4 && secondary == Some(primary));
    out.push(BeaconEffect {
        effect: primary.to_owned(),
        amplifier: base_amp,
        duration_ticks,
    });
    if levels >= 4
        && let Some(secondary) = secondary
        && secondary != primary
    {
        out.push(BeaconEffect {
            effect: secondary.to_owned(),
            amplifier: 0,
            duration_ticks,
        });
    }
    (range, out)
}

/// `BeaconMenu.encodeEffect`: the `container_set_data` wire form of an
/// optional effect — `0` for `None`, else the `minecraft:mob_effect`
/// registry id plus one (`0` is reserved as the "no effect" sentinel, so
/// every real id shifts up by one).
#[must_use]
pub fn encode_beacon_effect(effect: Option<&str>) -> i32 {
    effect
        .and_then(lodestone_data::mob_effects::mob_effect_id)
        .map_or(0, |id| id + 1)
}

/// `BeaconMenu.decodeEffect`, the inverse of [`encode_beacon_effect`].
#[must_use]
pub fn decode_beacon_effect(value: i32) -> Option<&'static str> {
    if value == 0 {
        None
    } else {
        lodestone_data::mob_effects::mob_effect_name(value - 1)
    }
}

/// Whether `item` may be placed in a beacon's payment slot — vanilla's
/// `minecraft:beacon_payment_items` tag, read from the same bundled JSON
/// [`crate::crafting::EMBEDDED_ITEM_TAGS`] already carries for the crafting
/// corpus, not a second hardcoded copy.
#[must_use]
pub fn is_beacon_payment_item(item: &str) -> bool {
    payment_items().iter().any(|entry| entry == item)
}

fn payment_items() -> &'static [String] {
    static CELL: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let Some((_, json)) = crate::crafting::EMBEDDED_ITEM_TAGS
            .iter()
            .find(|(id, _)| *id == "beacon_payment_items")
        else {
            return Vec::new();
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json) else {
            return Vec::new();
        };
        parsed
            .get("values")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::chunk::ChunkColumn;

    const MIN_Y: i32 = -64;
    const HEIGHT: i32 = 384;

    /// A `ChunkSource` that retains its edits — the same `Rig` shape
    /// `fire.rs`'s own test module already uses, duplicated per that
    /// module's convention rather than shared.
    struct Rig {
        columns: Mutex<HashMap<(i32, i32), ChunkColumn>>,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                columns: Mutex::new(HashMap::new()),
            }
        }
    }

    impl ChunkSource for Rig {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            let mut columns = self.columns.lock().expect("rig lock");
            columns
                .entry((cx, cz))
                .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT))
                .clone()
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let mut columns = self.columns.lock().expect("rig lock");
            let column = columns
                .entry((cx, cz))
                .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT));
            column.block_state(x - cx * 16, y, z - cz * 16).to_string()
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let mut columns = self.columns.lock().expect("rig lock");
            let column = columns
                .entry((cx, cz))
                .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT));
            column.set_block(x - cx * 16, y, z - cz * 16, name);
        }
    }

    /// Fills the `layers`-tall pyramid base beneath `(x, y, z)` with
    /// `block`, each layer `step` blocks below `y` and `(2*step+1)` wide —
    /// exactly the shape [`beacon_levels`] checks.
    fn build_pyramid(rig: &Rig, x: i32, y: i32, z: i32, block: &str, layers: i32) {
        for step in 1..=layers {
            let ly = y - step;
            for lx in (x - step)..=(x + step) {
                for lz in (z - step)..=(z + step) {
                    rig.set_block(lx, ly, lz, block);
                }
            }
        }
    }

    #[test]
    fn a_full_four_layer_pyramid_reports_level_four() {
        let rig = Rig::new();
        build_pyramid(&rig, 0, 64, 0, "minecraft:iron_block", 4);
        assert_eq!(beacon_levels(&rig, 0, 64, 0), 4);
    }

    /// **Control** for the case above: a level-2 pyramid whose layer 3 is
    /// missing one corner must report `2`, not `3` and not `0` — the
    /// discriminating case between "counts complete layers" and "counts any
    /// base block present anywhere".
    #[test]
    fn a_broken_third_layer_caps_the_level_at_two() {
        let rig = Rig::new();
        build_pyramid(&rig, 0, 64, 0, "minecraft:iron_block", 2);
        // Layer 3 (y=61): fill it, then break exactly one corner.
        for lx in -3..=3 {
            for lz in -3..=3 {
                rig.set_block(lx, 61, lz, "minecraft:iron_block");
            }
        }
        rig.set_block(3, 61, 3, "minecraft:dirt");
        assert_eq!(beacon_levels(&rig, 0, 64, 0), 2);
    }

    /// Every layer may use a *different* base block — vanilla's tag, not a
    /// single fixed material, per layer.
    #[test]
    fn mixed_base_block_materials_still_count() {
        let rig = Rig::new();
        for lx in -1..=1 {
            for lz in -1..=1 {
                rig.set_block(lx, 63, lz, "minecraft:diamond_block");
            }
        }
        for lx in -2..=2 {
            for lz in -2..=2 {
                rig.set_block(lx, 62, lz, "minecraft:netherite_block");
            }
        }
        assert_eq!(beacon_levels(&rig, 0, 64, 0), 2);
    }

    #[test]
    fn no_base_at_all_reports_level_zero() {
        let rig = Rig::new();
        assert_eq!(beacon_levels(&rig, 0, 64, 0), 0);
    }

    #[test]
    fn an_open_shaft_is_unobstructed() {
        let rig = Rig::new();
        assert!(beam_unobstructed(&rig, 0, 64, 0, 20));
    }

    #[test]
    fn a_glass_shaft_is_unobstructed() {
        let rig = Rig::new();
        for dy in 1..=20 {
            rig.set_block(0, 64 + dy, 0, "minecraft:cyan_stained_glass");
        }
        assert!(beam_unobstructed(&rig, 0, 64, 0, 20));
    }

    /// **Control** for the two cases above: a single solid block anywhere in
    /// the shaft must block it — without this, an implementation that
    /// always returned `true` would also pass both.
    #[test]
    fn one_solid_block_blocks_the_beam() {
        let rig = Rig::new();
        rig.set_block(0, 70, 0, "minecraft:stone");
        assert!(!beam_unobstructed(&rig, 0, 64, 0, 20));
    }

    /// The discriminating case against the old hand-picked-family
    /// implementation: a carpet has real, low
    /// [`lodestone_data::light_props::dampening`] (it is not a full block)
    /// but was never in that family, so the old `is_beam_transparent` would
    /// have refused it — a block a player is entirely likely to floor a
    /// beacon shaft with. Vanilla's own `getLightDampening() >= 15` gate
    /// agrees this does not block.
    #[test]
    fn a_carpet_shaft_is_unobstructed_even_though_it_was_never_in_the_old_family_list() {
        let rig = Rig::new();
        for dy in 1..=20 {
            rig.set_block(0, 64 + dy, 0, "minecraft:white_carpet");
        }
        assert!(beam_unobstructed(&rig, 0, 64, 0, 20));
    }

    /// Vanilla's own carve-out: `state.getLightDampening() >= 15 &&
    /// !state.is(Blocks.BEDROCK)` — bedrock is full dampening but explicitly
    /// exempted, so a beacon shaft that happens to cross a bedrock cell (a
    /// creative-mode build) is still unobstructed.
    #[test]
    fn bedrock_is_exempt_from_its_own_full_dampening() {
        let rig = Rig::new();
        rig.set_block(0, 70, 0, "minecraft:bedrock");
        assert!(beam_unobstructed(&rig, 0, 64, 0, 20));
    }

    #[test]
    fn required_levels_for_each_tier_matches_its_index_plus_one() {
        assert_eq!(required_levels_for("minecraft:speed"), Some(1));
        assert_eq!(required_levels_for("minecraft:haste"), Some(1));
        assert_eq!(required_levels_for("minecraft:resistance"), Some(2));
        assert_eq!(required_levels_for("minecraft:jump_boost"), Some(2));
        assert_eq!(required_levels_for("minecraft:strength"), Some(3));
        assert_eq!(required_levels_for("minecraft:regeneration"), Some(4));
        assert_eq!(required_levels_for("minecraft:speed"), Some(1));
        assert_eq!(required_levels_for("minecraft:not_a_beacon_power"), None);
    }

    /// The discriminating pyramid levels: a level-1 pyramid can select a
    /// tier-1 primary alone, but neither a secondary (any tier) nor a
    /// tier-2/3 primary.
    #[test]
    fn a_level_one_pyramid_permits_only_a_tier_one_primary_with_no_secondary() {
        assert!(validate_beacon_effects(Some("minecraft:speed"), None, 1));
        assert!(!validate_beacon_effects(Some("minecraft:resistance"), None, 1));
        assert!(!validate_beacon_effects(Some("minecraft:speed"), Some("minecraft:speed"), 1));
    }

    /// A level-3 pyramid unlocks tier 1–3 primaries but still no secondary
    /// at all — the level-4 boundary is what unlocks secondaries, not tier 3.
    #[test]
    fn a_level_three_pyramid_permits_a_tier_three_primary_but_never_a_secondary() {
        assert!(validate_beacon_effects(Some("minecraft:strength"), None, 3));
        assert!(!validate_beacon_effects(Some("minecraft:strength"), Some("minecraft:strength"), 3));
    }

    /// The level-4 secondary rule and the regeneration special case: a
    /// *different* tier-1..3 secondary is illegal even at level 4 (only the
    /// same effect as primary, or regeneration, may be the secondary); primary
    /// itself may never be regeneration.
    #[test]
    fn a_level_four_pyramid_permits_only_a_matching_or_regeneration_secondary() {
        assert!(validate_beacon_effects(Some("minecraft:speed"), None, 4));
        assert!(validate_beacon_effects(Some("minecraft:speed"), Some("minecraft:speed"), 4));
        assert!(validate_beacon_effects(Some("minecraft:speed"), Some("minecraft:regeneration"), 4));
        assert!(!validate_beacon_effects(Some("minecraft:speed"), Some("minecraft:strength"), 4));
        assert!(!validate_beacon_effects(Some("minecraft:regeneration"), None, 4));
    }

    /// **Magnitude, not sign**: predict the exact range/duration/amplifier
    /// pair at each level, from the arithmetic itself
    /// (`levels * 10 + 10`, `(9 + levels * 2) * 20`), not a plausible round
    /// number.
    #[test]
    fn beacon_effects_predicts_the_exact_range_and_duration_at_every_level() {
        for (levels, expected_range, expected_duration) in
            [(1u8, 20.0, 220), (2, 30.0, 260), (3, 40.0, 300), (4, 50.0, 340)]
        {
            let (range, effects) = beacon_effects(levels, Some("minecraft:speed"), None);
            assert_eq!(range, expected_range, "range at level {levels}");
            assert_eq!(effects.len(), 1);
            assert_eq!(effects[0].duration_ticks, expected_duration, "duration at level {levels}");
            assert_eq!(effects[0].amplifier, 0);
        }
    }

    /// The amplifier boost: only at level 4, and only when secondary equals
    /// primary — the discriminating pair the doc names.
    #[test]
    fn a_stacked_secondary_boosts_the_primary_amplifier_only_at_level_four() {
        let (_, effects) = beacon_effects(4, Some("minecraft:speed"), Some("minecraft:speed"));
        assert_eq!(effects.len(), 1, "same effect twice collapses to one boosted application");
        assert_eq!(effects[0].amplifier, 1);

        let (_, effects) = beacon_effects(3, Some("minecraft:speed"), Some("minecraft:speed"));
        assert_eq!(effects[0].amplifier, 0, "the amplifier boost needs level 4, not just a match");
    }

    /// A genuine secondary application: distinct effect, level 4, no
    /// amplifier boost on either.
    #[test]
    fn a_distinct_secondary_applies_as_its_own_zero_amplifier_effect() {
        let (_, effects) = beacon_effects(4, Some("minecraft:speed"), Some("minecraft:regeneration"));
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0].effect, "minecraft:speed");
        assert_eq!(effects[0].amplifier, 0);
        assert_eq!(effects[1].effect, "minecraft:regeneration");
        assert_eq!(effects[1].amplifier, 0);
    }

    #[test]
    fn no_primary_selected_applies_nothing() {
        let (_, effects) = beacon_effects(4, None, None);
        assert!(effects.is_empty());
    }

    #[test]
    fn encode_beacon_effect_round_trips_through_decode() {
        assert_eq!(encode_beacon_effect(None), 0);
        assert_eq!(decode_beacon_effect(0), None);
        let speed_encoded = encode_beacon_effect(Some("minecraft:speed"));
        assert_ne!(speed_encoded, 0, "a real effect must not collide with the none sentinel");
        assert_eq!(decode_beacon_effect(speed_encoded), Some("minecraft:speed"));
        // A second, distinct effect must not collide with the first.
        let strength_encoded = encode_beacon_effect(Some("minecraft:strength"));
        assert_ne!(speed_encoded, strength_encoded);
        assert_eq!(decode_beacon_effect(strength_encoded), Some("minecraft:strength"));
    }

    #[test]
    fn beacon_payment_items_reads_the_bundled_tag() {
        assert!(is_beacon_payment_item("minecraft:iron_ingot"));
        assert!(is_beacon_payment_item("minecraft:diamond"));
        assert!(is_beacon_payment_item("minecraft:emerald"));
        assert!(is_beacon_payment_item("minecraft:gold_ingot"));
        assert!(is_beacon_payment_item("minecraft:netherite_ingot"));
        assert!(!is_beacon_payment_item("minecraft:iron_block"));
        assert!(!is_beacon_payment_item("minecraft:dirt"));
    }
}
