//! The player's active status effects: the HUD's own top-right overlay, and
//! the model layer for the column vanilla's own inventory-effects widget
//! draws beside the
//! player-inventory panel.
//!
//! ## Two surfaces, one state, and they are not the same widget
//!
//! | | HUD overlay | inventory column |
//! |---|---|---|
//! | vanilla | vanilla's own HUD effects-extract routine | vanilla's own inventory-effects widget |
//! | shown when | no screen, or a screen whose `showsActiveEffects()` is false | the player's own inventory only |
//! | filters `show_icon` | **yes** | **no** — `getActiveEffects()` is used whole |
//! | text | none at all | translated name plus vanilla's own effect-duration formatter |
//! | drawn by | `hud.rs`'s geometry builder, from [`hud_icons`] | `container::geometry`'s `draw_effect_column` |
//!
//! The split of *responsibility* is the part worth knowing: everything from
//! [`InventoryEffectRow`] down is the inventory column's **model**, resolved
//! here because that is where the caller's language table can reach it, and
//! drawn inside the container's own geometry pass because that is where the
//! real GUI sprites and the real proportional font already live. Emitting it
//! from this module instead is what produced the widget's four simultaneous
//! defects — a hash-derived colour swatch where the icon belongs, a flat rect
//! where the nine-sliced background belongs, the 5x7 debug font at 2x scale,
//! and the raw registry path where the translated name belongs — because none
//! of the four assets was reachable from here.
//!
//! ## The HUD overlay
//!
//! [`HudEffectIcon`]/[`hud_icons`] are the model for vanilla's own HUD effects-extract routine, and
//! it is now a port rather than a stand-in: a `24x24` background sprite and an
//! `18x18` icon per effect, two rows (beneficial above, harmful below), **no
//! text at all**, and a flashing icon alpha inside the last 200 ticks.
//!
//! It used to draw a hash-tinted chip with a name and a timer, for one reason:
//! the overlay owned an untextured pipeline of its own, so no sprite was
//! reachable from its draw site. The fix is the one the inventory column
//! already took — **draw it where the atlas is**. The geometry is emitted by
//! `hud.rs`'s own builder, through the same GUI-atlas sprite pipeline the
//! hearts and the hotbar frame use, so this module went back to being a pure
//! model and the separate renderer and shader are gone.
//!
//! The icons themselves became reachable when `GuiAtlas` started enumerating
//! **both** directory sources `assets/minecraft/atlases/gui.json` declares
//! rather than only the first; see [`mob_effect_sprite`].
//!
//! A jar-less run draws no overlay at all, deliberately — the same choice the
//! inventory column makes, and for the same reason: a coloured stand-in is
//! indistinguishable from art that failed to load.
//!
//! ## Layering
//!
//! State folding lives in [`lodestone_game::effect`] (`update_mob_effect` /
//! `remove_mob_effect` fold into [`ActiveEffects`], which the sim ticks down);
//! this module only *interprets* that state. The effect *identity* is a
//! canonical [`Identifier`](lodestone_model::Identifier) — never a
//! version-specific numeric id — so both surfaces are version-free like the
//! rest of the shell.

use lodestone_game::effect::{ActiveEffects, StatusEffect};

/// vanilla's own HUD effect-background sprite — the overlay's plate behind a non-ambient
/// effect. **Not** the inventory column's
/// [`EFFECT_BACKGROUND_SPRITE`]: that one is
/// `container/inventory/effect_background`, a nine-sliced widget of a
/// different size. Two widgets, two sprites, and the pack ships both.
pub const HUD_EFFECT_BACKGROUND_SPRITE: &str = "hud/effect_background";
/// vanilla's own HUD ambient-effect-background sprite — the beacon/aura plate.
pub const HUD_EFFECT_BACKGROUND_AMBIENT_SPRITE: &str = "hud/effect_background_ambient";
/// The side length the background sprite is blitted at (`24, 24`).
pub const HUD_EFFECT_BACKGROUND_SIZE: f32 = 24.0;
/// The side length the effect icon is blitted at (`18, 18`).
pub const HUD_EFFECT_ICON_SIZE: f32 = 18.0;
/// The icon's inset inside the background, on both axes (`x + 3, y + 3`).
pub const HUD_EFFECT_ICON_INSET: f32 = 3.0;
/// The per-icon horizontal pitch: `x -= 25 * n`, counted **per row**, so the
/// beneficial and harmful rows each start over at the right edge.
pub const HUD_EFFECT_STRIDE: f32 = 25.0;
/// The beneficial row's top edge (`int y = 1`).
pub const HUD_EFFECT_TOP_Y: f32 = 1.0;
/// How far below the beneficial row the harmful row sits (`y += 26`).
pub const HUD_EFFECT_ROW_DROP: f32 = 26.0;
/// `instance.endsWithin(200)` — the window inside which a non-ambient icon
/// flashes.
const HUD_EFFECT_FLASH_TICKS: i32 = 200;

/// One icon of vanilla's own HUD effects-extract routine' top-right overlay, resolved but not yet
/// positioned — the layout is two counters and a subtraction, and it belongs
/// where the canvas width is known (`hud.rs`'s geometry builder).
///
/// Carries **no** colour and no text, which is the whole difference from what
/// this used to be: vanilla blits a real background sprite and a real icon,
/// so a tint derived from the id would be a stand-in for art that exists.
#[derive(Debug, Clone, PartialEq)]
pub struct HudEffectIcon {
    /// `mob_effect/<path>` — [`mob_effect_sprite`]'s output.
    pub icon: String,
    /// [`HUD_EFFECT_BACKGROUND_SPRITE`] or, for an ambient effect,
    /// [`HUD_EFFECT_BACKGROUND_AMBIENT_SPRITE`].
    pub background: &'static str,
    /// The **icon** blit's tint alpha (vanilla's own white-with-alpha tint); `1.0` except
    /// while flashing. The background is never tinted — see
    /// [`hud_icon_alpha`].
    pub alpha: f32,
    /// Vanilla's own is-beneficial check: top row when `true`, bottom row otherwise.
    pub beneficial: bool,
}

/// Fold the active effects into vanilla's own HUD effects-extract routine' icon list.
///
/// Two differences from [`inventory_rows`], both vanilla's:
///
/// * `showIcon()` **is** honoured here (the inventory column uses
///   the whole active-effects list);
/// * the order is vanilla's own natural ordering reversed,
///   the inventory column's order backwards. That order decides which icon
///   sits furthest right within each row, so it is not cosmetic.
#[must_use]
pub fn hud_icons(fx: &ActiveEffects) -> Vec<HudEffectIcon> {
    let mut sorted: Vec<&StatusEffect> = fx.iter().collect();
    // Vanilla's own natural-order reversal — arguments swapped rather than a
    // `.reverse()` on the result, so a tie stays a tie instead of flipping.
    sorted.sort_by(|a, b| natural_order(b, a));
    sorted
        .into_iter()
        .filter(|e| e.show_icon)
        .map(|e| HudEffectIcon {
            icon: mob_effect_sprite(e.id.path()),
            background: if e.ambient {
                HUD_EFFECT_BACKGROUND_AMBIENT_SPRITE
            } else {
                HUD_EFFECT_BACKGROUND_SPRITE
            },
            alpha: hud_icon_alpha(e),
            beneficial: is_beneficial(e.id.path()),
        })
        .collect()
}

/// The alpha vanilla's own HUD effects-extract routine blits an effect's icon with.
///
/// `1.0` unless the effect is non-ambient and `endsWithin(200)`, in which case
/// it is vanilla's flash:
///
/// ```text
/// usedSeconds = 10 - remaining / 20                      (integer division)
/// alpha = clamp(remaining / 10 / 5 * 0.5, 0, 0.5)
///       + cos(remaining * PI / 5) * clamp(usedSeconds / 10 * 0.25, 0, 0.25)
/// ```
///
/// clamped into `0..=1`. An **ambient** effect never flashes — vanilla only
/// evaluates this inside the non-ambient branch — and an infinite one cannot,
/// because `endsWithin` is `false` for `duration == -1` regardless of the
/// comparison that follows it. Getting either wrong makes a beacon's icons
/// strobe forever.
///
/// The cosine is the **table**, not `f32::cos`; see `lodestone_physics::mth`.
/// Ours takes an `f64`, so the index can round one step differently from
/// vanilla's own single-precision table lookup at the boundary between two of the 65,536
/// entries. That is a sub-frame difference in a decorative fade, recorded
/// rather than papered over.
#[must_use]
pub fn hud_icon_alpha(e: &StatusEffect) -> f32 {
    if e.ambient || e.is_infinite() || e.duration_ticks > HUD_EFFECT_FLASH_TICKS {
        return 1.0;
    }
    let remaining = e.duration_ticks;
    // Integer division, deliberately: `10 - remainingDuration / 20` in Java is
    // an int expression, so this counts whole elapsed seconds of the fade.
    let used_seconds = 10 - remaining / 20;
    let ramp = (remaining as f32 / 10.0 / 5.0 * 0.5).clamp(0.0, 0.5);
    let swing = (used_seconds as f32 / 10.0 * 0.25).clamp(0.0, 0.25);
    let phase = f64::from(remaining as f32 * std::f32::consts::PI / 5.0);
    (ramp + lodestone_physics::mth::cos(phase) * swing).clamp(0.0, 1.0)
}

/// Vanilla's own is-beneficial check — the effect's category equals the
/// beneficial category.
///
/// Transcribed from vanilla's own mob-effects registration class's own per-effect category argument (see
/// `docs/inventory-potion-effects.md`), which is the only place it is stated: it is a
/// constructor argument, so it appears in no generated registry dump. Note
/// **`NEUTRAL` is not beneficial** — `glowing`, `bad_omen`, `trial_omen` and
/// `raid_omen` all draw in the lower row, which is what vanilla's own
/// is-beneficial check comparing the category equal to beneficial (rather
/// than testing it not-equal to harmful) means.
///
/// An id this table does not know is treated as not beneficial, so an effect
/// from a future version lands in the lower row instead of vanishing. The
/// table's completeness against the shipped registry is asserted by
/// `every_registry_effect_has_a_category`, not assumed.
///
/// The durable home for this is `lodestone_data`, beside
/// [`mob_effect_color`](lodestone_data::mob_effects::mob_effect_color) —
/// `lodestone_data::potion` already carries a **potion-scoped** 20-entry
/// subset of the same fact, and `the_potion_table_agrees_about_harmfulness`
/// checks the two against each other rather than letting them drift.
#[must_use]
pub fn is_beneficial(path: &str) -> bool {
    MOB_EFFECT_BENEFICIAL
        .iter()
        .find(|(name, _)| *name == path)
        .is_some_and(|(_, beneficial)| *beneficial)
}

/// `(registry path, isBeneficial())` for every effect in the 26.2 registry.
/// See [`is_beneficial`] for provenance and for why `NEUTRAL` reads `false`.
const MOB_EFFECT_BENEFICIAL: &[(&str, bool)] = &[
    ("speed", true),
    ("slowness", false),
    ("haste", true),
    ("mining_fatigue", false),
    ("strength", true),
    ("instant_health", true),
    ("instant_damage", false),
    ("jump_boost", true),
    ("nausea", false),
    ("regeneration", true),
    ("resistance", true),
    ("fire_resistance", true),
    ("water_breathing", true),
    ("invisibility", true),
    ("blindness", false),
    ("night_vision", true),
    ("hunger", false),
    ("weakness", false),
    ("poison", false),
    ("wither", false),
    ("health_boost", true),
    ("absorption", true),
    ("saturation", true),
    // NEUTRAL
    ("glowing", false),
    ("levitation", false),
    ("luck", true),
    ("unluck", false),
    ("slow_falling", true),
    ("conduit_power", true),
    ("dolphins_grace", true),
    // NEUTRAL
    ("bad_omen", false),
    ("hero_of_the_village", true),
    ("darkness", false),
    // NEUTRAL
    ("trial_omen", false),
    // NEUTRAL
    ("raid_omen", false),
    ("wind_charged", false),
    ("weaving", false),
    ("oozing", false),
    ("infested", false),
    ("breath_of_the_nautilus", true),
];

/// A deterministic, bright RGB tint per effect id. This is a *rendering* choice
/// (distinguish effects at a glance), not registry knowledge, so it is derived
/// from the id rather than a hand-maintained beneficial/harmful table. Distinct
/// ids get distinct tints; the same id is stable across frames and runs (a
/// fixed-key hasher, never `RandomState`).
/// `pub(crate)` since beacon screen (`container::beacon`) reuses
/// this same hash-derived swatch colour for its power buttons — the
/// identical "no real sprite exists, so tint a flat quad" simplification
/// this HUD chip already established.
pub(crate) fn tint_for(path: &str) -> [f32; 3] {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    path.hash(&mut h);
    let v = h.finish();
    // Spread three bytes across a 0.35..=1.0 range so every channel stays bright
    // enough to read over the world, and no effect renders near-black.
    let chan = |shift: u32| 0.35 + ((v >> shift) & 0xff) as f32 / 255.0 * 0.65;
    [chan(0), chan(8), chan(16)]
}

/// vanilla's own effects-widget icon size — the effect icon's side length, and the
/// size the `mob_effect/<id>` sprite is blitted at.
pub const INV_ICON_SIZE: f32 = 18.0;
/// vanilla's own effects-widget spacing — the icon's inset from the widget's own
/// top-left corner, and the trailing padding in the background's width.
pub const INV_SPACING: f32 = 7.0;
/// vanilla's own effects-widget text x-offset — where the name/duration column starts,
/// relative to the widget's left edge.
pub const INV_TEXT_X_OFFSET: f32 = 32.0;
/// vanilla's own effects-widget sprite square size — the background widget's fixed
/// height, and the compact (icon-only) width used when there is no room for
/// text.
pub const INV_BACKGROUND: f32 = 32.0;
/// vanilla's own effects-widget render-state extract routine's `yStep` when five or fewer
/// effects are showing.
const INV_Y_STEP: f32 = 33.0;
/// vanilla's own effects-widget render-state extract routine's crowded-column span (`132`),
/// divided by `count - 1` once more than five effects are active so the
/// column still fits the panel's height instead of running off the bottom.
const INV_CROWDED_SPAN: f32 = 132.0;

/// vanilla's own effects-widget background sprite — the nine-sliced widget
/// behind a non-ambient effect row.
pub const EFFECT_BACKGROUND_SPRITE: &str = "container/inventory/effect_background";
/// vanilla's own effects-widget ambient background sprite — the same widget for
/// a beacon/conduit (ambient) effect.
pub const EFFECT_BACKGROUND_AMBIENT_SPRITE: &str =
    "container/inventory/effect_background_ambient";

/// vanilla's own get-mob-effect-sprite routine: an effect's icon sprite id is its registry id
/// prefixed with `mob_effect/`. The art is **not** under `gui/sprites/**` —
/// `assets/minecraft/atlases/gui.json` declares a second source directory
/// (`{"type": "directory", "prefix": "mob_effect/", "source": "mob_effect"}`),
/// so the file is `textures/mob_effect/<path>.png`. That extra source is the
/// reason a plain `gui/sprites/**` enumeration finds none of these icons.
#[must_use]
pub fn mob_effect_sprite(path: &str) -> String {
    format!("mob_effect/{path}")
}

/// One row of the player-inventory effect column, resolved exactly as
/// vanilla's own effects-widget extract-effects routine resolves it: a real sprite id, the
/// **translated** display name with its level suffix, and
/// vanilla's own effect-duration formatter's string.
///
/// This is the model layer for the widget. It deliberately carries no colour:
/// vanilla draws a real icon, so a tint derived from the id would be a
/// stand-in for art that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryEffectRow {
    /// `mob_effect/<path>` — [`mob_effect_sprite`]'s output, the sprite id the
    /// draw site blits.
    pub sprite: String,
    /// Vanilla's own effect display-name accessor (the `effect.minecraft.<path>` key run
    /// through the language table) plus, for amplifier `1..=9`, a space and
    /// `enchantment.level.<amplifier + 1>` — vanilla's own roman numerals.
    pub name: String,
    /// Vanilla's own effect duration formatter at real time (1.0, 20 ticks per
    /// second): `mm:ss`, `hh:mm:ss`
    /// past an hour, or the translated `effect.duration.infinite` (`∞`).
    pub duration: String,
    /// vanilla's own effect-instance ambient flag — selects the ambient background sprite.
    pub ambient: bool,
}

/// The game's fixed tick rate, which is what
/// vanilla's own tick-rate-manager accessor reports for an ordinary world and what
/// vanilla's own tick-duration formatter divides by. A server running
/// `/tick rate` would report something else; this client has no
/// `TickRateManager`, so the constant is the honest value rather than a
/// placeholder.
const TICKRATE: f32 = 20.0;

/// Vanilla's own effect display-name accessor — a translatable component
/// keyed on the effect's own description id, which is built as
/// `effect.minecraft.<path>`.
///
/// Falls back to the raw key when the language table has no entry, matching
/// every other translated surface in this crate: losing a translation must
/// never cost the row.
fn effect_display_name(
    namespace: &str,
    path: &str,
    translate: &dyn Fn(&str) -> Option<String>,
) -> String {
    let key = format!("effect.{namespace}.{path}");
    translate(&key).unwrap_or(key)
}

/// vanilla's own effects-widget get-effect-name routine: the display name, plus a space and
/// `enchantment.level.<amplifier + 1>` when the amplifier is `1..=9`.
///
/// The bound is vanilla's: amplifier `0` shows no suffix at all (level I is
/// implicit), and amplifier `10` and above shows none either, because
/// `enchantment.level.11` does not exist as a key. This is **not**
/// `potion.potency.*`, which is what the potion *tooltip* uses.
fn effect_row_name(
    namespace: &str,
    path: &str,
    amplifier: u8,
    translate: &dyn Fn(&str) -> Option<String>,
) -> String {
    let name = effect_display_name(namespace, path, translate);
    if (1..=9).contains(&amplifier) {
        let key = format!("enchantment.level.{}", u32::from(amplifier) + 1);
        let level = translate(&key).unwrap_or(key);
        format!("{name} {level}")
    } else {
        name
    }
}

/// Vanilla's own tick-duration formatter: `mm:ss`, widening to
/// `hh:mm:ss` only once there is at least one whole hour.
#[must_use]
fn format_tick_duration(ticks: i32, tickrate: f32) -> String {
    let seconds = (ticks as f32 / tickrate).floor().max(0.0) as i64;
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    let hours = minutes / 60;
    let minutes = minutes % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Vanilla's own effect-duration formatter at real time (1.0, tick rate).
fn format_duration(effect: &StatusEffect, translate: &dyn Fn(&str) -> Option<String>) -> String {
    if effect.is_infinite() {
        return translate("effect.duration.infinite")
            .unwrap_or_else(|| "effect.duration.infinite".to_owned());
    }
    format_tick_duration(effect.duration_ticks, TICKRATE)
}

/// Vanilla's own effect-color accessor for `path`, the last tiebreaker in
/// [`natural_order`]. `None` for an effect this build's registry table does
/// not know, which sorts last rather than panicking.
fn effect_color(path: &str) -> Option<u32> {
    let id = lodestone_data::mob_effects::mob_effect_id(&format!("minecraft:{path}"))?;
    lodestone_data::mob_effects::mob_effect_color(id)
}

/// vanilla's own effect-instance comparator — the order
/// vanilla's own natural-order sort puts the column in.
///
/// Two branches, and which one applies depends on the pair: while either
/// duration is at or under the `32147` cutoff, **or** either effect is
/// non-ambient, non-ambient sorts first, then finite before infinite, then
/// shorter duration, then colour. Otherwise (both long and both ambient) only
/// ambience and colour are compared.
fn natural_order(a: &StatusEffect, b: &StatusEffect) -> std::cmp::Ordering {
    /// vanilla's own effect-instance comparator's `updateCutOff`.
    const UPDATE_CUT_OFF: i32 = 32147;
    // `isInfiniteDuration()` is `duration == -1`, so an infinite effect's raw
    // `getDuration()` is `-1` and therefore *is* `<= 32147` — the first branch
    // is what an infinite effect always takes unless both are ambient.
    let short = a.duration_ticks <= UPDATE_CUT_OFF || b.duration_ticks <= UPDATE_CUT_OFF;
    let colour = || effect_color(a.id.path()).cmp(&effect_color(b.id.path()));
    if short && (!a.ambient || !b.ambient) {
        // `compareFalseFirst`: `false` before `true`.
        a.ambient
            .cmp(&b.ambient)
            .then_with(|| a.is_infinite().cmp(&b.is_infinite()))
            .then_with(|| a.duration_ticks.cmp(&b.duration_ticks))
            .then_with(colour)
    } else {
        a.ambient.cmp(&b.ambient).then_with(colour)
    }
}

/// Fold the active effects into the inventory column's rows, in vanilla's own
/// natural-order.
///
/// `translate` is the live language table (`Sim::translator`). Passing
/// `&|_| None` yields raw keys, which is the jar-less degradation every other
/// translated surface in this crate takes — not a design choice about naming.
///
/// Unlike the HUD's own top-right overlay, vanilla's own inventory-effects widget does **not**
/// filter on `showIcon`: `getActiveEffects()` is used whole, so a
/// `show_icon = false` effect still occupies a row here.
#[must_use]
pub fn inventory_rows(
    fx: &ActiveEffects,
    translate: &dyn Fn(&str) -> Option<String>,
) -> Vec<InventoryEffectRow> {
    let mut sorted: Vec<&StatusEffect> = fx.iter().collect();
    sorted.sort_by(|a, b| natural_order(a, b));
    sorted
        .into_iter()
        .map(|e| InventoryEffectRow {
            sprite: mob_effect_sprite(e.id.path()),
            name: effect_row_name(e.id.namespace(), e.id.path(), e.amplifier, translate),
            duration: format_duration(e, translate),
            ambient: e.ambient,
        })
        .collect()
}

/// Where the inventory effect column starts, and how much width it has to
/// work with — vanilla's own effects-widget can-see-effects check/`extractRenderState`
/// (`26.2`): `x0 = leftPos + imageWidth + 2`, `availableWidth = screenWidth -
/// x0`. Real 26.2 source, read directly — **not** the older
/// `EffectRenderingInventoryScreen` shape some descriptions of this feature
/// still name: this version never repositions the container panel itself
/// (`InventoryScreen`'s own `leftPos` comes from the ordinary centred/
/// recipe-book-shifted layout, untouched by whether any effect is active) —
/// it only decides whether there is *already* enough free canvas beside the
/// panel to draw into. A panel-shifting "make room" step does not exist in
/// this version's vanilla, either in the inventory-effects widget or the
/// inventory screen itself, so this port does
/// not add one either.
#[must_use]
pub fn inventory_column_x0(panel_x: f32, panel_width: f32) -> f32 {
    panel_x + panel_width + 2.0
}

/// vanilla's own effects-widget can-see-effects check: `availableWidth >= 32`.
#[must_use]
pub fn inventory_can_see_effects(available_width: f32) -> bool {
    available_width >= INV_BACKGROUND
}

/// vanilla's own effects-widget render-state extract routine's `maxWidth`:
/// `availableWidth >= 120 ? availableWidth - 7 : 32`.
#[must_use]
pub fn inventory_max_width(available_width: f32) -> f32 {
    if available_width >= 120.0 {
        available_width - 7.0
    } else {
        INV_BACKGROUND
    }
}

/// vanilla's own effects-widget render-state extract routine's `yStep`: `33` for five or fewer
/// active effects, `132 / (count - 1)` above that so a crowded column still
/// fits between `topPos` and the bottom of the panel instead of overflowing.
#[must_use]
pub fn inventory_y_step(count: usize) -> f32 {
    if count > 5 {
        INV_CROWDED_SPAN / (count as f32 - 1.0)
    } else {
        INV_Y_STEP
    }
}

/// vanilla's own effects-widget background extract routine's `textureWidth`:
/// `min(maxTextureWidth, max(32 + width(name) + 7, 32 + width(duration) + 7))`.
///
/// `width` is the caller's own font metric, so this stays font-agnostic — the
/// container's real proportional font on a normal run, the fixed-advance debug
/// font on a jar-less one.
#[must_use]
pub fn inventory_texture_width(name_px: f32, duration_px: f32, max_texture_width: f32) -> f32 {
    let name_width = INV_TEXT_X_OFFSET + name_px + INV_SPACING;
    let duration_width = INV_TEXT_X_OFFSET + duration_px + INV_SPACING;
    max_texture_width.min(name_width.max(duration_width))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::effect::StatusEffect;
    use lodestone_model::Identifier;

    fn id(path: &str) -> Identifier {
        Identifier::new("minecraft", path).unwrap()
    }

    fn effect(path: &str, amp: u8, dur: i32) -> StatusEffect {
        StatusEffect::new(id(path), amp, dur)
    }

    #[test]
    fn the_hud_overlay_keeps_only_show_icon_effects_and_picks_the_right_plate() {
        let mut fx = ActiveEffects::new();
        fx.apply(StatusEffect {
            id: id("night_vision"),
            amplifier: 0,
            duration_ticks: -1,
            ambient: true,
            show_particles: true,
            show_icon: true,
        });
        fx.apply(StatusEffect {
            id: id("hidden"),
            amplifier: 0,
            duration_ticks: 100,
            ambient: false,
            show_particles: true,
            show_icon: false,
        });
        let icons = hud_icons(&fx);
        assert_eq!(icons.len(), 1, "the show_icon=false effect draws no icon");
        assert_eq!(icons[0].icon, "mob_effect/night_vision");
        assert_eq!(
            icons[0].background, HUD_EFFECT_BACKGROUND_AMBIENT_SPRITE,
            "an ambient effect takes the ambient plate"
        );
        assert!(
            icons[0].beneficial,
            "night vision is BENEFICIAL, so it belongs in the upper row"
        );
        assert!(
            (icons[0].alpha - 1.0).abs() < f32::EPSILON,
            "an ambient, infinite effect must never flash: got {}",
            icons[0].alpha
        );
        // Same list, seen by the inventory column: it does *not* filter on
        // `show_icon`, so it keeps the effect the overlay drops. Both surfaces
        // reading one state and disagreeing about this is the whole reason
        // they are separate folds.
        assert_eq!(inventory_rows(&fx, &|_| None).len(), 2);
    }

    /// `MobEffectCategory` decides the row, and **`NEUTRAL` is not
    /// beneficial** — `isBeneficial()` is `category == BENEFICIAL`, not
    /// `!= HARMFUL`. Reading it the other way puts four effects in the wrong
    /// row and looks entirely plausible on screen.
    #[test]
    fn neutral_effects_draw_in_the_lower_row() {
        for neutral in ["glowing", "bad_omen", "trial_omen", "raid_omen"] {
            assert!(
                !is_beneficial(neutral),
                "{neutral} is NEUTRAL, which isBeneficial() reports as false"
            );
        }
        for beneficial in ["speed", "regeneration", "breath_of_the_nautilus"] {
            assert!(is_beneficial(beneficial), "{beneficial} is BENEFICIAL");
        }
        for harmful in ["poison", "wither", "infested"] {
            assert!(!is_beneficial(harmful), "{harmful} is HARMFUL");
        }
        assert!(
            !is_beneficial("a_future_effect"),
            "an unknown id must fall to the lower row rather than the upper one"
        );
    }

    /// The category table must cover the whole shipped registry, or an effect
    /// silently defaults into the harmful row and nothing is red.
    ///
    /// The expected set comes from `lodestone_data`'s generated registry
    /// names — Mojang's own `registries.json` — not from this file, so the
    /// two cannot agree by sharing a mistake.
    #[test]
    fn every_registry_effect_has_a_category() {
        let mut missing = Vec::new();
        for id in 0..lodestone_data::mob_effects::MOB_EFFECT_COUNT {
            let Some(name) = lodestone_data::mob_effects::mob_effect_name(id as i32) else {
                continue;
            };
            let path = name.strip_prefix("minecraft:").unwrap_or(name);
            if !MOB_EFFECT_BENEFICIAL.iter().any(|(n, _)| *n == path) {
                missing.push(path.to_string());
            }
        }
        assert!(
            missing.is_empty(),
            "MOB_EFFECT_BENEFICIAL is missing {} of the registry's effects: {missing:?}",
            missing.len()
        );
        // And the reverse, so a renamed effect leaves a dead row behind rather
        // than quietly shadowing nothing.
        let mut unknown = Vec::new();
        for (path, _) in MOB_EFFECT_BENEFICIAL {
            let full = format!("minecraft:{path}");
            if lodestone_data::mob_effects::mob_effect_id(&full).is_none() {
                unknown.push((*path).to_string());
            }
        }
        assert!(
            unknown.is_empty(),
            "MOB_EFFECT_BENEFICIAL names effects the registry does not have: {unknown:?}"
        );
    }

    /// `lodestone_data::potion` carries a **potion-scoped** subset of the same
    /// `MobEffectCategory` fact (`harmful` on each tooltip entry). Two tables
    /// stating one thing is a drift hazard, so they are checked against each
    /// other rather than left to agree by luck — and the expectation for each
    /// is the *other* table, both transcribed from vanilla's own mob-effects registration class.
    #[test]
    fn the_potion_table_agrees_about_harmfulness() {
        let mut checked = 0usize;
        let mut wrong: Vec<String> = Vec::new();
        for potion in 0..lodestone_data::potion::POTION_COUNT {
            let potion = lodestone_data::potion::PotionId::from_registry_id(potion as i32)
                .expect("generated potion id is valid");
            let entries = lodestone_data::potion::potion_effect_entries(potion);
            let raw = lodestone_data::potion::potion_built_in_effects(potion);
            for (entry, (effect_index, _, _)) in entries.iter().zip(raw.iter()) {
                let Some(name) = lodestone_data::mob_effects::mob_effect_name(*effect_index as i32)
                else {
                    continue;
                };
                let path = name.strip_prefix("minecraft:").unwrap_or(name);
                // `harmful` is `category == HARMFUL`; `is_beneficial` is
                // `category == BENEFICIAL`. NEUTRAL makes both false, so the
                // check is one-directional: harmful implies not beneficial.
                checked += 1;
                if entry.harmful && is_beneficial(path) {
                    wrong.push(format!("{path}: potion says HARMFUL, this table says BENEFICIAL"));
                }
            }
        }
        assert!(
            checked > 0,
            "no potion effect was compared — the cross-check measured nothing, which is a \
             failure to run rather than agreement"
        );
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// The flash is vanilla's, and its two gates are the ones worth pinning:
    /// an effect outside the 200-tick window never fades, and an **ambient**
    /// one never fades even inside it (vanilla evaluates the formula only in
    /// the non-ambient branch). Getting the second wrong makes a beacon's
    /// icons strobe permanently.
    #[test]
    fn the_icon_flashes_only_inside_the_last_two_hundred_ticks() {
        let full = effect("speed", 0, 201);
        assert!(
            (hud_icon_alpha(&full) - 1.0).abs() < f32::EPSILON,
            "one tick outside the window is not a flash"
        );
        let boundary = effect("speed", 0, 200);
        assert!(
            hud_icon_alpha(&boundary) < 1.0,
            "`endsWithin(200)` is inclusive, so 200 flashes: got {}",
            hud_icon_alpha(&boundary)
        );

        let mut ambient = effect("speed", 0, 40);
        ambient.ambient = true;
        assert!(
            (hud_icon_alpha(&ambient) - 1.0).abs() < f32::EPSILON,
            "an ambient effect must not flash"
        );

        let infinite = effect("speed", 0, -1);
        assert!(
            (hud_icon_alpha(&infinite) - 1.0).abs() < f32::EPSILON,
            "`endsWithin` is false for an infinite duration whatever the comparison says"
        );

        // The value itself, predicted from the formula rather than from a
        // round number. **37 ticks, and the oddness is the point.** A multiple
        // of ten puts the phase on a whole turn where every cosine agrees and
        // both clamps saturate, so the first draft of this used 60 and the
        // discriminating check below caught it. At 37: ramp = 37/50*0.5 =
        // 0.37 (unsaturated), usedSeconds = 10 - 1 = 9, swing = 9/10*0.25 =
        // 0.225 (unsaturated), and the phase is 7.4π.
        const TICKS: i32 = 37;
        let e = effect("speed", 0, TICKS);
        let arg = TICKS as f32 * std::f32::consts::PI / 5.0;
        let want = (0.37 + lodestone_physics::mth::cos(f64::from(arg)) * 0.225).clamp(0.0, 1.0);
        assert!(
            (hud_icon_alpha(&e) - want).abs() < 1e-6,
            "predicted {want}, got {}",
            hud_icon_alpha(&e)
        );
        // And the wrong hypothesis this could plausibly have been written as —
        // the standard library's cosine instead of vanilla's quantized table —
        // must give a different answer at this input, or the assertion above
        // proves nothing about which one is in use.
        let std_hypothesis = (0.37 + arg.cos() * 0.225).clamp(0.0, 1.0);
        assert_ne!(
            want.to_bits(),
            std_hypothesis.to_bits(),
            "this input does not separate vanilla's own cos lookup table from f32::cos, so pick another"
        );
    }

    /// The overlay's order is vanilla's own natural ordering reversed — the inventory
    /// column's order backwards. It decides which icon sits furthest right,
    /// so a gate that only checks membership cannot see it.
    #[test]
    fn the_overlay_is_in_reverse_natural_order() {
        let mut fx = ActiveEffects::new();
        fx.apply(effect("speed", 0, 300));
        fx.apply(effect("strength", 0, 100));
        fx.apply(effect("haste", 0, 600));

        let column: Vec<String> = inventory_rows(&fx, &|_| None)
            .into_iter()
            .map(|r| r.sprite)
            .collect();
        let overlay: Vec<String> = hud_icons(&fx).into_iter().map(|i| i.icon).collect();

        assert_eq!(column.len(), 3);
        let mut reversed = column.clone();
        reversed.reverse();
        assert_eq!(
            overlay, reversed,
            "the overlay must be the column's order reversed, not the same order"
        );
        assert_ne!(
            overlay, column,
            "this fixture does not separate the two orders — pick durations that do"
        );
    }

    #[test]
    fn distinct_effects_tint_differently_and_stably() {
        let a = tint_for("speed");
        let b = tint_for("poison");
        assert_ne!(a, b, "different effects must be visually distinguishable");
        assert_eq!(a, tint_for("speed"), "tint must be stable for a given id");
        for ch in a {
            assert!((0.35..=1.0).contains(&ch), "channels stay bright: {ch}");
        }
    }

    /// The keys this widget asks the language table for, recorded rather than
    /// assumed. `effect.minecraft.<path>` is vanilla's own effect
    /// display-name accessor's
    /// description id and `enchantment.level.<amplifier + 1>` is
    /// vanilla's own effects-widget get-effect-name routine's numeral — asking for anything else
    /// (or asking for nothing, which is what drew `speed` on screen) is the
    /// reported bug.
    #[test]
    fn the_rows_ask_for_vanillas_own_translation_keys() {
        use std::cell::RefCell;

        let asked: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let translate = |key: &str| -> Option<String> {
            asked.borrow_mut().push(key.to_owned());
            None
        };

        let mut fx = ActiveEffects::new();
        fx.apply(effect("speed", 1, 1800));
        let rows = inventory_rows(&fx, &translate);
        assert_eq!(rows.len(), 1);

        let asked = asked.into_inner();
        assert!(
            asked.contains(&"effect.minecraft.speed".to_owned()),
            "the display name must be looked up under MobEffect's own descriptionId; \
             asked for {asked:?}"
        );
        assert!(
            asked.contains(&"enchantment.level.2".to_owned()),
            "amplifier 1 must resolve its numeral through enchantment.level.2 — not \
             potion.potency.*, which is the potion tooltip's key; asked for {asked:?}"
        );
        // A table that resolves nothing still yields the keys, never a silent
        // blank: losing a translation must not cost the row.
        assert_eq!(rows[0].name, "effect.minecraft.speed enchantment.level.2");
    }

    /// vanilla's own effects-widget get-effect-name routine's amplifier bound, at both edges.
    /// Amplifier `0` is level I and shows no numeral; `9` is the last one with
    /// an `enchantment.level.*` key; `10` has none and so shows none either.
    ///
    /// `0` and `10` are the discriminating inputs: a naive `amplifier >= 1`
    /// port agrees with vanilla at `0` and disagrees at `10`, and a naive
    /// `amplifier + 1` numeral agrees everywhere below `10`.
    #[test]
    fn the_level_numeral_appears_only_for_amplifiers_one_through_nine() {
        let translate = |key: &str| -> Option<String> {
            match key {
                "effect.minecraft.speed" => Some("Speed".to_owned()),
                "enchantment.level.2" => Some("II".to_owned()),
                "enchantment.level.10" => Some("X".to_owned()),
                _ => None,
            }
        };
        let name = |amp: u8| {
            let mut fx = ActiveEffects::new();
            fx.apply(effect("speed", amp, 200));
            inventory_rows(&fx, &translate).remove(0).name
        };
        assert_eq!(name(0), "Speed", "level I carries no numeral");
        assert_eq!(name(1), "Speed II");
        assert_eq!(name(9), "Speed X");
        assert_eq!(
            name(10),
            "Speed",
            "amplifier 10 has no enchantment.level.11 key, so vanilla appends nothing"
        );
    }

    /// vanilla's own tick-duration formatter: `mm:ss`, widening to `hh:mm:ss` only
    /// once a whole hour is present, and floor-dividing rather than rounding.
    ///
    /// The values are re-derived from the tick count rather than reached for
    /// as round numbers — `1799` ticks is 89 seconds, not 90.
    #[test]
    fn the_duration_line_is_format_tick_duration() {
        assert_eq!(format_tick_duration(1800, 20.0), "01:30");
        assert_eq!(
            format_tick_duration(1799, 20.0),
            "01:29",
            "the seconds are floored, so one tick short of 90 s reads 89 s"
        );
        assert_eq!(format_tick_duration(0, 20.0), "00:00");
        // 3600 s exactly: the first duration wide enough to grow an hours field.
        assert_eq!(format_tick_duration(20 * 3600, 20.0), "01:00:00");
        assert_eq!(
            format_tick_duration(20 * 3599, 20.0),
            "59:59",
            "one second under an hour still reads mm:ss"
        );
    }

    /// vanilla's own effect-duration formatter's infinite branch: the translated
    /// `effect.duration.infinite`, never a clock reading.
    #[test]
    fn an_infinite_effect_shows_the_infinity_string() {
        let translate = |key: &str| -> Option<String> {
            (key == "effect.duration.infinite").then(|| "INF".to_owned())
        };
        let mut fx = ActiveEffects::new();
        fx.apply(StatusEffect {
            id: id("night_vision"),
            amplifier: 0,
            duration_ticks: -1,
            ambient: false,
            show_particles: true,
            show_icon: true,
        });
        assert_eq!(inventory_rows(&fx, &translate).remove(0).duration, "INF");
    }

    /// Vanilla's own natural-order sort — its own effect-instance comparator:
    /// non-ambient before ambient, then finite before
    /// infinite, then shorter duration first.
    ///
    /// The insertion order below is deliberately the reverse of the expected
    /// one, so a fold that preserves insertion order (which is what this
    /// widget used to do) fails every arm rather than coincidentally passing.
    #[test]
    fn the_column_is_in_mob_effect_instance_natural_order() {
        let mut fx = ActiveEffects::new();
        fx.apply(StatusEffect {
            id: id("night_vision"),
            amplifier: 0,
            duration_ticks: 400,
            ambient: true,
            show_particles: true,
            show_icon: true,
        });
        fx.apply(StatusEffect {
            id: id("haste"),
            amplifier: 0,
            duration_ticks: -1,
            ambient: false,
            show_particles: true,
            show_icon: true,
        });
        fx.apply(effect("strength", 0, 600));
        fx.apply(effect("speed", 0, 200));

        let sprites: Vec<String> = inventory_rows(&fx, &|_| None)
            .into_iter()
            .map(|r| r.sprite)
            .collect();
        assert_eq!(
            sprites,
            vec![
                mob_effect_sprite("speed"),
                mob_effect_sprite("strength"),
                mob_effect_sprite("haste"),
                mob_effect_sprite("night_vision"),
            ],
            "non-ambient first, then finite before infinite, then shortest first"
        );
    }

    /// vanilla's own inventory-effects widget uses `getActiveEffects()` whole — unlike the HUD's
    /// own overlay, it does **not** filter on `showIcon`. Stated as an
    /// assertion because the two surfaces sit in this same module and share a
    /// state source, so the difference is easy to unify by accident.
    #[test]
    fn the_column_keeps_effects_the_hud_overlay_hides() {
        let mut fx = ActiveEffects::new();
        fx.apply(StatusEffect {
            id: id("speed"),
            amplifier: 0,
            duration_ticks: 200,
            ambient: false,
            show_particles: true,
            show_icon: false,
        });
        assert_eq!(
            inventory_rows(&fx, &|_| None).len(),
            1,
            "the inventory column shows a show_icon=false effect"
        );
        assert!(
            hud_icons(&fx).is_empty(),
            "the HUD overlay does not — Hud.extractEffects gates on instance.showIcon()"
        );
    }

    #[test]
    fn inventory_layout_matches_effects_in_inventory_java() {
        // `canSeeEffects`: exactly `>= 32` is visible, one pixel under is not.
        assert!(inventory_can_see_effects(32.0));
        assert!(!inventory_can_see_effects(31.999));
        // `maxWidth`: the `>= 120` branch subtracts 7; below it, pinned at 32.
        assert_eq!(inventory_max_width(200.0), 193.0);
        assert_eq!(inventory_max_width(120.0), 113.0);
        assert_eq!(inventory_max_width(119.999), INV_BACKGROUND);
        assert_eq!(inventory_max_width(40.0), INV_BACKGROUND);
        // `yStep`: 33 up to and including five effects, `132 / (n - 1)` above.
        assert_eq!(inventory_y_step(1), 33.0);
        assert_eq!(inventory_y_step(5), 33.0);
        assert_eq!(inventory_y_step(6), 132.0 / 5.0);
        assert_eq!(inventory_y_step(7), 132.0 / 6.0);
    }

    /// Headless GPU proof that the overlay reaches pixels **as vanilla's
    /// widget**, through the real `HudRenderer` sprite pipeline and the real
    /// pack's GUI atlas.
    ///
    /// Three things this asserts that a coverage count alone cannot, each
    /// chosen because it is exactly what the previous approximation got wrong:
    ///
    /// * **the ink fits inside a 24x24 plate.** The chip widget this replaced
    ///   was `ICON + PAD + text` wide with two lines of text; a bounding box
    ///   inside one plate rect is the cheapest thing that cannot be true of it.
    /// * **the row split is real.** A beneficial effect paints the upper row
    ///   and *nothing* in the lower one, and a harmful effect the reverse. A
    ///   single shared position counter, or `isBeneficial` read as
    ///   `!= HARMFUL`, passes a coverage check and fails this.
    /// * **an empty effect set paints nothing** — the control, and the reason
    ///   the two arms above are evidence rather than a description.
    ///
    /// Not hermetic on purpose: every input comes from the real pack, because
    /// the sprite ids are the thing under test. A jar-less run has no atlas and
    /// the overlay correctly draws nothing, which is indistinguishable from a
    /// broken draw — so this **fails** rather than skips when the atlas is
    /// absent.
    #[test]
    #[ignore = "requires a GPU adapter and the vanilla client.jar"]
    fn the_overlay_blits_vanillas_own_sprites_in_two_rows() {
        use crate::config::{AUTO_GUI_SCALE, calculate_gui_scale};
        use crate::hud::{DebugStats, HudFrame, HudRenderer};
        use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

        let ctx = GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (640u32, 480u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = HudRenderer::new(device, target.raw_view_format());

        let atlas = crate::resources::load_gui_atlas().expect(
            "this gate needs the vanilla GUI atlas: without it the overlay correctly draws \
             nothing, which is indistinguishable from the bug. Set LODESTONE_ASSETS to a \
             pack root with client.jar.",
        );
        // The icons live in `atlases/gui.json`'s **second** directory source.
        // Asserting they are present separates "the atlas loaded" from "the
        // atlas loaded the half this widget needs" — the state this whole
        // widget was stuck in before that source was implemented.
        for id in [
            HUD_EFFECT_BACKGROUND_SPRITE,
            HUD_EFFECT_BACKGROUND_AMBIENT_SPRITE,
            "mob_effect/speed",
            "mob_effect/poison",
        ] {
            assert!(
                atlas.contains(id),
                "the GUI atlas must carry `{id}`; without it this gate measures an absent \
                 sprite rather than an absent draw"
            );
        }
        hud.attach_gui(device, queue, format, atlas);

        let scale = calculate_gui_scale(AUTO_GUI_SCALE, w, h).max(1) as f32;
        // The two plate rects, in physical pixels, derived from the same
        // constants the draw uses rather than restated.
        let plate = |beneficial: bool| -> (u32, u32, u32, u32) {
            let logical_w = w as f32 / scale;
            let x0 = (logical_w - HUD_EFFECT_STRIDE) * scale;
            let y0 = if beneficial {
                HUD_EFFECT_TOP_Y
            } else {
                HUD_EFFECT_TOP_Y + HUD_EFFECT_ROW_DROP
            } * scale;
            let side = HUD_EFFECT_BACKGROUND_SIZE * scale;
            (x0 as u32, y0 as u32, (x0 + side) as u32, (y0 + side) as u32)
        };
        let upper = plate(true);
        let lower = plate(false);

        let stats = DebugStats::default();
        let shoot = |hud: &mut HudRenderer,
                     target: &mut HeadlessTarget,
                     icons: &[HudEffectIcon]|
         -> Vec<u8> {
            let frame = target.acquire().expect("headless acquire");
            let raw = hud.flat_colour_view(&frame);
            {
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("effects-gate-clear"),
                });
                enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("effects-gate-clear-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: frame.view(),
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                queue.submit(std::iter::once(enc.finish()));
            }
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                effects: Some(icons),
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &raw, &hud_frame, w, h);
            drop(frame);
            target.read_texels(device, queue)
        };

        let lit_in = |px: &[u8], r: (u32, u32, u32, u32)| -> usize {
            let mut n = 0;
            for y in r.1..r.3.min(h) {
                for x in r.0..r.2.min(w) {
                    let i = ((y * w + x) * 4) as usize;
                    if px[i] > 12 || px[i + 1] > 12 || px[i + 2] > 12 {
                        n += 1;
                    }
                }
            }
            n
        };
        // Where the ink actually is, over the whole frame — the assertion that
        // the widget is one small plate rather than a wide chip needs a box,
        // not a count.
        let bbox = |px: &[u8]| -> Option<(u32, u32, u32, u32)> {
            let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
            let mut any = false;
            for y in 0..h {
                for x in 0..w {
                    let i = ((y * w + x) * 4) as usize;
                    if px[i] > 12 || px[i + 1] > 12 || px[i + 2] > 12 {
                        any = true;
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x);
                        y1 = y1.max(y);
                    }
                }
            }
            any.then_some((x0, y0, x1, y1))
        };

        let mut beneficial_fx = ActiveEffects::new();
        beneficial_fx.apply(effect("speed", 0, 1800));
        let mut harmful_fx = ActiveEffects::new();
        harmful_fx.apply(effect("poison", 0, 1800));

        let empty = shoot(&mut hud, &mut target, &[]);
        let good = shoot(&mut hud, &mut target, &hud_icons(&beneficial_fx));
        let bad = shoot(&mut hud, &mut target, &hud_icons(&harmful_fx));

        eprintln!("=== hud status-effect overlay ===");
        eprintln!("gui scale = {scale}; upper plate = {upper:?}; lower plate = {lower:?}");
        eprintln!(
            "empty: upper={} lower={} bbox={:?}",
            lit_in(&empty, upper),
            lit_in(&empty, lower),
            bbox(&empty)
        );
        eprintln!(
            "speed (BENEFICIAL): upper={} lower={} bbox={:?}",
            lit_in(&good, upper),
            lit_in(&good, lower),
            bbox(&good)
        );
        eprintln!(
            "poison (HARMFUL): upper={} lower={} bbox={:?}",
            lit_in(&bad, upper),
            lit_in(&bad, lower),
            bbox(&bad)
        );

        // Control first: with no effects nothing paints anywhere, so the two
        // arms below are measuring this widget and not some other HUD element
        // that happens to live in the corner.
        assert_eq!(
            bbox(&empty),
            None,
            "an empty effect set must paint nothing at all"
        );

        // Both arms collected before asserting, so a run reports every failing
        // row rather than aborting on the first.
        let mut wrong: Vec<String> = Vec::new();
        for (name, px, want, other) in [
            ("speed (BENEFICIAL)", &good, upper, lower),
            ("poison (HARMFUL)", &bad, lower, upper),
        ] {
            let hit = lit_in(px, want);
            let leak = lit_in(px, other);
            // A 24x24 plate at this scale; even a mostly-transparent icon over
            // it covers far more than a fifth of the rect.
            let floor = ((HUD_EFFECT_BACKGROUND_SIZE * scale).powi(2) / 5.0) as usize;
            if hit < floor {
                wrong.push(format!("{name}: only {hit} px in its own row (want > {floor})"));
            }
            if leak != 0 {
                wrong.push(format!("{name}: {leak} px leaked into the other row"));
            }
            // The whole widget is one plate. The chip this replaced was several
            // times wider and two text lines tall, so this is the assertion it
            // could not have passed.
            let Some((x0, y0, x1, y1)) = bbox(px) else {
                wrong.push(format!("{name}: painted nothing"));
                continue;
            };
            if x0 < want.0 || y0 < want.1 || x1 >= want.2 || y1 >= want.3 {
                wrong.push(format!(
                    "{name}: ink at ({x0},{y0})..({x1},{y1}) escapes its own 24x24 plate \
                     {want:?} — that is a chip, not vanilla's widget"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}
