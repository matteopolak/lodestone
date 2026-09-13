//! Air-supply bubble row layout — the pure half of vanilla's
//! HUD air-bubble extraction and per-bubble-sprite selection functions
//! (26.2 decompile, behavioural
//! reference only).
//!
//! # Why this exists as its own module
//!
//! The three sprites this draws (`hud/air`, `hud/air_empty`,
//! `hud/air_bursting`) are already stitched into [`crate::gui_atlas::GuiAtlas`]
//! for free — it globs the whole `gui/sprites/**` tree, and vanilla ships these
//! under `gui/sprites/hud/air*.png`. What is missing end-to-end is the *value*
//! (`airSupply` is not yet decoded anywhere in the protocol/ECS layers this
//! crate sits above) and the per-slot layout logic, which is this module.
//!
//! Once a HUD has an `(air, max_air, is_underwater)` triple for the local
//! player, wiring the row is: call [`bubble_row`], then for each non-
//! [`BubbleSlot::Hidden`] slot call
//! [`GuiAtlas::geometry`](crate::gui_atlas::GuiAtlas::geometry) with
//! [`BubbleSlot::sprite_id`] and the position from [`bubble_position`].
//!
//! # Refill model (26.2, read from source rather than remembered)
//!
//! `getCurrentAirSupplyBubble` computes `ceil((air + offset) * 10 / max_air)`
//! for the "full" count (`offset = -2`) and the "about to pop" position
//! (`offset = 0`). Because that is a continuous ceiling of a ratio rather than
//! an integer bubble-per-`max_air/10`-ticks snap, the full-bubble count climbs
//! by one bubble at a time as `air` rises smoothly while regenerating out of
//! water — matching the newer "gradual refill" behaviour, not the older
//! "instant full" one. This is a direct port of that formula (in integer
//! arithmetic, see [`current_air_supply_bubble`]'s doc for the equivalence
//! proof), not a re-derivation, so the refill curve should match vanilla
//! exactly.

/// Which sprite (if any) a single bubble slot should draw this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BubbleSlot {
    /// Draw `hud/air` — a full bubble.
    Full,
    /// Draw `hud/air_bursting` — the one popping this tick.
    Popping,
    /// Draw `hud/air_empty`, jittered up by 0 or 1px this frame (vanilla's
    /// `random.nextInt(2)` — see [`bubble_row`]'s `wobble` parameter for why
    /// the actual jitter amount is left to the caller).
    EmptyWobbling,
    /// Draw `hud/air_empty` at its normal position.
    Empty,
    /// Draw nothing for this slot.
    Hidden,
}

impl BubbleSlot {
    /// The [`crate::gui_atlas::GuiAtlas`] sprite id this slot draws, or `None`
    /// for [`BubbleSlot::Hidden`].
    #[must_use]
    pub fn sprite_id(self) -> Option<&'static str> {
        match self {
            BubbleSlot::Full => Some("hud/air"),
            BubbleSlot::Popping => Some("hud/air_bursting"),
            BubbleSlot::EmptyWobbling | BubbleSlot::Empty => Some("hud/air_empty"),
            BubbleSlot::Hidden => None,
        }
    }
}

/// Number of bubble slots in the row (vanilla `Hud.NUM_AIR_BUBBLES`).
pub const BUBBLE_COUNT: usize = 10;
/// Native sprite size in pixels (vanilla `Hud.AIR_BUBBLE_SIZE`).
pub const BUBBLE_SIZE: f32 = 9.0;
/// Horizontal step between bubble slots in pixels (vanilla
/// `Hud.AIR_BUBBLE_SEPERATION`, spelling preserved from the source constant
/// name for grep-ability against it).
pub const BUBBLE_SEPARATION: f32 = 8.0;

/// Whether the bubble row should be drawn at all this frame — vanilla's
/// `if (isUnderWater || currentAirSupplyTicks < maxAirSupplyTicks)` guard. A
/// player at full air who is not submerged draws nothing.
#[must_use]
pub fn bubble_row_visible(air: i32, max_air: i32, is_underwater: bool) -> bool {
    is_underwater || air < max_air
}

/// `Mth.ceil((float) ((current + tick_offset) * 10) / max)`, in exact integer
/// arithmetic.
///
/// # Why integer division reproduces the float ceil exactly
///
/// For the domain this is ever called over (`max_air` a small positive tick
/// count, `current` clamped to `0..=max_air`, `tick_offset` in `{-2, 0}`, so
/// `(current + tick_offset) * 10` is a modest integer that never approaches
/// `f32`'s 24-bit mantissa limit), `ceil(n / d)` for integers `n`, `d > 0` is
/// exactly `n.div_euclid(d) + (n.rem_euclid(d) != 0) as i32`: Rust's
/// `div_euclid`/`rem_euclid` always return a non-negative remainder, so this
/// identity holds for negative `n` too (unlike truncating `/`, which would
/// round a negative quotient the wrong way). `current = 0, tick_offset = -2`
/// is exactly the negative case this matters for — vanilla's own float ceil of
/// a small negative fraction (e.g. `ceil(-20.0 / 300.0) == 0`) is reproduced
/// bit-for-bit as an integer result rather than approximated.
#[must_use]
fn current_air_supply_bubble(current: i32, max: i32, tick_offset: i32) -> i32 {
    if max <= 0 {
        return 0;
    }
    let numerator = (current + tick_offset) * 10;
    numerator.div_euclid(max) + i32::from(numerator.rem_euclid(max) != 0)
}

/// `Hud.getEmptyBubbleDelayDuration`: the empty-bubble edge lags one tick
/// behind the full/popping edge while submerged with any air left, so the
/// last empty bubble doesn't reappear in the same frame the adjacent one pops.
#[must_use]
fn empty_bubble_delay_duration(current: i32, is_underwater: bool) -> i32 {
    i32::from(current != 0 && is_underwater)
}

/// Computes which sprite (if any) each of the 10 bubble slots should draw this
/// frame — a direct port of `Hud.extractAirBubbles`'s per-slot loop.
///
/// `air` is clamped to `0..=max_air` internally, mirroring vanilla's own
/// `Math.clamp(player.getAirSupply(), 0, maxAirSupplyTicks)` (call sites do not
/// need to pre-clamp). `max_air` is floored at `1` to avoid a division by zero
/// on a malformed `minecraft:max_air` attribute; vanilla's own default is `300`.
///
/// `wobble` is the caller's per-frame toggle (vanilla samples
/// `tickCount % 2 == 0`); when a slot would wobble, this returns
/// [`BubbleSlot::EmptyWobbling`] and leaves the actual 0-or-1px jitter to the
/// caller (vanilla additionally rolls `random.nextInt(2)` on top of the tick
/// parity — a second coin flip on top of a coin flip is not load-bearing to
/// the visual, and keeping the RNG out of this function keeps it pure and
/// deterministically testable).
///
/// Slot `0` is bubble `1` in vanilla's `1..=10` loop (rightmost / most recently
/// filled); see [`bubble_position`] for the on-screen ordering.
#[must_use]
pub fn bubble_row(
    air: i32,
    max_air: i32,
    is_underwater: bool,
    wobble: bool,
) -> [BubbleSlot; BUBBLE_COUNT] {
    let mut slots = [BubbleSlot::Hidden; BUBBLE_COUNT];
    let max_air = max_air.max(1);
    let current = air.clamp(0, max_air);
    if !bubble_row_visible(current, max_air, is_underwater) {
        return slots;
    }

    let full = current_air_supply_bubble(current, max_air, -2);
    let popping_position = current_air_supply_bubble(current, max_air, 0);
    let empty_delay = empty_bubble_delay_duration(current, is_underwater);
    let empty = 10 - current_air_supply_bubble(current, max_air, empty_delay);
    let is_popping_bubble = full != popping_position;

    for (i, slot) in slots.iter_mut().enumerate() {
        let bubble = i as i32 + 1; // vanilla's 1..=10
        *slot = if bubble <= full {
            BubbleSlot::Full
        } else if is_popping_bubble && bubble == popping_position && is_underwater {
            BubbleSlot::Popping
        } else if bubble > 10 - empty {
            if empty == 10 && wobble {
                BubbleSlot::EmptyWobbling
            } else {
                BubbleSlot::Empty
            }
        } else {
            BubbleSlot::Hidden
        };
    }
    slots
}

/// The screen-space `(x, y)` top-left of bubble slot `index` (`0..10`,
/// matching [`bubble_row`]'s slot ordering).
///
/// Vanilla places bubble `n` (`1..=10`) at
/// `xRight - (n - 1) * AIR_BUBBLE_SEPERATION - AIR_BUBBLE_SIZE`: bubbles fill
/// right-to-left from the row's right anchor, each stepping
/// [`BUBBLE_SEPARATION`] px left of the previous. `x_right`/`y` are that
/// anchor and the row's y-line — screen-layout concerns the HUD owns (vanilla
/// derives `yLineAir` from vehicle-heart row count via `getAirBubbleYLine`).
#[must_use]
pub fn bubble_position(index: usize, x_right: f32, y: f32) -> (f32, f32) {
    let bubble = index as f32 + 1.0;
    (x_right - (bubble - 1.0) * BUBBLE_SEPARATION - BUBBLE_SIZE, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_air_not_underwater_draws_nothing() {
        assert_eq!(bubble_row(300, 300, false, false), [BubbleSlot::Hidden; 10]);
    }

    /// Just submerged at full air: vanilla still shows all 10 full bubbles
    /// (the guard is an OR, not just "air below max").
    #[test]
    fn full_air_underwater_shows_all_full() {
        assert_eq!(bubble_row(300, 300, true, false), [BubbleSlot::Full; 10]);
    }

    /// No air left: every slot is empty, and — because `full == popping ==
    /// 0`, `is_popping_bubble` is false — nothing pops from a state with no
    /// air to lose.
    #[test]
    fn zero_air_underwater_shows_all_empty() {
        assert_eq!(bubble_row(0, 300, true, false), [BubbleSlot::Empty; 10]);
    }

    /// A known mid-drain snapshot: air 151/300 underwater. `full =
    /// ceil(1490/300) = 5`, `popping = ceil(1510/300) = 6`, so bubbles 1..=5
    /// are full and bubble 6 is popping; `empty_delay = 1` (submerged, air
    /// nonzero) so `empty = 10 - ceil(1520/300) = 10 - 6 = 4`, meaning bubbles
    /// 7..=10 (`> 10 - 4 == 6`) are empty.
    #[test]
    fn mid_drain_known_snapshot() {
        let row = bubble_row(151, 300, true, false);
        assert_eq!(
            row,
            [
                BubbleSlot::Full,
                BubbleSlot::Full,
                BubbleSlot::Full,
                BubbleSlot::Full,
                BubbleSlot::Full,
                BubbleSlot::Popping,
                BubbleSlot::Empty,
                BubbleSlot::Empty,
                BubbleSlot::Empty,
                BubbleSlot::Empty,
            ]
        );
    }

    /// Refill climbs one bubble at a time as `air` rises smoothly rather than
    /// snapping straight to full — the "gradual refill" the briefing asked to
    /// establish, verified against the ceiling formula rather than assumed.
    #[test]
    fn refill_is_gradual_not_a_snap_to_full() {
        let full_count = |air: i32| {
            bubble_row(air, 300, true, false)
                .iter()
                .filter(|s| **s == BubbleSlot::Full)
                .count()
        };
        let counts: Vec<usize> = (0..=300).step_by(30).map(full_count).collect();
        // Strictly non-decreasing, and it actually passes through intermediate
        // values rather than jumping straight from 0 to 10.
        assert!(counts.windows(2).all(|w| w[1] >= w[0]));
        assert!(
            counts.iter().any(|&c| c > 0 && c < 10),
            "expected an intermediate partial-full count, got {counts:?}"
        );
    }

    /// Negative tick offsets must ceil correctly (the case
    /// `current_air_supply_bubble`'s doc proves): air just above zero must not
    /// under- or over-count the full bubbles by one due to a sign error.
    #[test]
    fn near_zero_air_does_not_show_a_spurious_full_bubble() {
        let row = bubble_row(1, 300, true, false);
        assert!(
            !row.contains(&BubbleSlot::Full),
            "1 tick of air must not render as a full bubble: {row:?}"
        );
    }

    /// `wobble` only ever produces `EmptyWobbling` when the row is entirely
    /// empty (vanilla's `emptyAirBubbles == 10` guard) — a partially-drained
    /// row must not wobble its empty bubbles.
    #[test]
    fn wobble_only_applies_when_the_whole_row_is_empty() {
        let all_empty = bubble_row(0, 300, true, true);
        assert!(all_empty.contains(&BubbleSlot::EmptyWobbling));

        let partial = bubble_row(151, 300, true, true);
        assert!(!partial.contains(&BubbleSlot::EmptyWobbling));
    }

    #[test]
    fn sprite_ids_match_the_three_vanilla_hud_sprites() {
        assert_eq!(BubbleSlot::Full.sprite_id(), Some("hud/air"));
        assert_eq!(BubbleSlot::Popping.sprite_id(), Some("hud/air_bursting"));
        assert_eq!(BubbleSlot::Empty.sprite_id(), Some("hud/air_empty"));
        assert_eq!(BubbleSlot::EmptyWobbling.sprite_id(), Some("hud/air_empty"));
        assert_eq!(BubbleSlot::Hidden.sprite_id(), None);
    }

    /// Bubbles fill right-to-left: slot 0 sits furthest right, each later slot
    /// steps `BUBBLE_SEPARATION` px to the left of the previous.
    #[test]
    fn positions_step_leftward_from_the_right_anchor() {
        let (x0, y0) = bubble_position(0, 100.0, 40.0);
        let (x1, y1) = bubble_position(1, 100.0, 40.0);
        assert_eq!(x0, 100.0 - BUBBLE_SIZE);
        assert_eq!(y0, 40.0);
        assert_eq!(x1, x0 - BUBBLE_SEPARATION);
        assert_eq!(y1, y0);
    }

    /// `max_air <= 0` must not panic (division-by-zero guard).
    #[test]
    fn zero_max_air_does_not_panic() {
        let _ = bubble_row(0, 0, true, false);
    }
}
