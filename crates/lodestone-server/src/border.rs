//! Server-authoritative world border.
//!
//! # What this is
//!
//! A faithful port of the real world border: the border's centre, size (with
//! a linear lerp between two sizes), damage per block, safe zone, and warning
//! distance/time. It is
//! the state a real 26.2 server broadcasts on join (`initialize_border`) and
//! mutates with the five `set_border_*` deltas, and the geometry the
//! server-side enforcement reads to damage a
//! player standing past the safe zone.
//!
//! # The two halves: state the client sees vs. state only the server reads
//!
//! * **Broadcast state** — everything the `initialize_border` wire packet
//!   carries: centre, old/new size, lerp time, absolute max size, and the two
//!   warnings, sent as part of the real per-player join sequence. A connection's
//!   join sequence reads it through `ServerProtocol::encode_initialize_border`;
//!   the five `SET_BORDER_*` deltas are its resized successors.
//! * **Enforcement state** — `damage_per_block` and `safe_zone`, which the
//!   client never sees. The connection task reads them every vitals tick and
//!   computes `max(1, floor(-dist * damage_per_block))` for a player past the
//!   safe zone, applied through
//!   [`crate::PlayerVitals::apply_border_damage`].
//!
//! # Interim shape (pre-shape-B) — read this before wiring a resize
//!
//! The world-state plan (`docs/plans/world-state.md` B1) lands this as an
//! interim, deliberately: the world tick loop owns a **plain
//! [`WorldBorder`] of its own** (ticked first, matching the real world tick's
//! order, but a static default today because nothing calls
//! [`WorldBorder::lerp_size_between`] yet), and each connection holds a
//! [`BorderFeed`] it snapshots for its join broadcast and enforcement. Both
//! are the same default state today, so nothing is observably inconsistent;
//! the seam between them is exactly what the ECS migration (shape B) deletes
//! by making the border a world `Resource` the loop ticks and every
//! connection reads. [`BorderFeed::with`] is the resize entry point that
//! wiring (a future `/worldborder`-style command or plugin) will call.
//!
//! # Fidelity notes, against the real implementation
//!
//! * **Lerp matches the real linear-extent update exactly**:
//!   `size = from + (to - from) * progress` where
//!   `progress = (duration - remaining) / duration`, and the final tick snaps
//!   to `to` and becomes a static extent. The plan's gate samples this at
//!   ticks {0, d/4, d/2, d} against the linear formula — see
//!   [`WorldBorder`]'s tests.
//! * **Read-clamping, not write-clamping.** The real min/max-x/z getters
//!   clamp to `±absoluteMaxSize` at *read* time; the real centre setter
//!   stores verbatim (the real implementation
//!   clamps the centre only in its persisted save-data codec, which this
//!   crate has no serialization for).
//! * **`previous_size`.** The real min/max getters interpolate
//!   `previousSize → size` over the partial tick, so the delta-0 read
//!   a whole-tick enforcement uses reflects the *previous* tick's size. This
//!   port mirrors that: the getters below read `previous_size`, and
//!   [`WorldBorder::tick`] advances it exactly as the real linear-extent
//!   update does. The one-tick lag it produces during a lerp is real
//!   behaviour, not a bug.
//! * **The dead `warningTime` field.** The real field initializer is `15`
//!   but the real initial-settings application overwrites it with the
//!   default settings' `300` before any player sees it. This port
//!   ships the `300` as its default and records the discrepancy so nobody
//!   "fixes" it backwards.
//! * **`damage_per_block == 0` disables damage**,
//!   and damage is `max(1, floor(-dist * damage_per_block))` — the `max(1, ..)`
//!   floor means a player one block past the safe zone still takes 1.

use std::sync::{Arc, Mutex};

/// The real default border size — `5.999997E7`, the exact value the real
/// default settings ship.
pub const MAX_SIZE: f64 = 5.999997E7;

/// The real absolute-max-size field's initial value: the read-clamp applied
/// to every min/max getter and the value `initialize_border`'s
/// `absolute_max_size` field carries.
pub const ABSOLUTE_MAX_SIZE: i32 = 29_999_984;

/// The real maximum center coordinate —
/// the range the real save-data codec clamps the centre into at load.
/// Not a live clamp here (nothing persists a
/// border in this crate — see the module doc's read-clamping note), but kept
/// as the documented codec bound a future save path must enforce.
pub const MAX_CENTER_COORDINATE: f64 = 2.9999984E7;

/// One in-flight linear size lerp, mirroring the real linear-extent
/// implementation.
///
/// `progress` counts **remaining** ticks (the real field is named for that), not
/// elapsed, so `size_at(0)` returns `to` and completes the lerp — the same
/// off-by-one the plan's gate is written against (tick `d`, not `d - 1`, is
/// where the target is reached).
#[derive(Debug, Clone)]
struct BorderLerp {
    /// Size at lerp start.
    from: f64,
    /// Size at lerp end (also the lerp target).
    to: f64,
    /// Total lerp length in ticks.
    duration: i64,
    /// Remaining ticks until the lerp completes.
    progress: i64,
}

impl BorderLerp {
    /// The real moving-extent size calculation:
    /// `progress = (duration - remaining) / duration`, and once that fraction
    /// reaches 1 the result is `to`, not `from + (to - from) * 1.0` — the
    /// branch is the real implementation's own and is what makes the final
    /// tick exact.
    fn size_at(&self, remaining: i64) -> f64 {
        let fraction = (self.duration - remaining) as f64 / self.duration as f64;
        if fraction < 1.0 {
            self.from + (self.to - self.from) * fraction
        } else {
            self.to
        }
    }
}

/// The server-authoritative world border — a plain value type, ticked by the
/// world loop and snapshotted by each connection (see the module doc for the
/// interim shape).
///
/// Defaults are the real default settings'
/// values: centre `(0, 0)`, size [`MAX_SIZE`], damage
/// 0.2/block, safe zone 5.0, warning blocks 5, warning time **300** (the
/// dead-15 correction — see the module doc), static extent (lerp time 0).
#[derive(Debug, Clone)]
pub struct WorldBorder {
    center_x: f64,
    center_z: f64,
    /// Current size (`getSize()`).
    size: f64,
    /// Size one tick ago — the real "previous size" field, the value the
    /// delta-0 min/max getters interpolate from (see the module doc's
    /// fidelity note).
    previous_size: f64,
    /// `None` is a static extent.
    lerp: Option<BorderLerp>,
    damage_per_block: f64,
    safe_zone: f64,
    warning_blocks: i32,
    warning_time: i32,
    absolute_max_size: i32,
}

impl Default for WorldBorder {
    fn default() -> Self {
        Self {
            center_x: 0.0,
            center_z: 0.0,
            size: MAX_SIZE,
            previous_size: MAX_SIZE,
            lerp: None,
            damage_per_block: 0.2,
            safe_zone: 5.0,
            warning_blocks: 5,
            warning_time: 300,
            absolute_max_size: ABSOLUTE_MAX_SIZE,
        }
    }
}

impl WorldBorder {
    /// Advances the border by one server tick — the real per-tick border
    /// update, which replaces the extent with its own updated form: a
    /// static extent is a no-op, and a moving one steps the lerp and switches
    /// to static the tick it completes.
    pub fn tick(&mut self) {
        if let Some(lerp) = self.lerp.take() {
            let remaining = lerp.progress - 1;
            self.previous_size = self.size;
            self.size = lerp.size_at(remaining);
            if remaining > 0 {
                self.lerp = Some(BorderLerp {
                    progress: remaining,
                    ..lerp
                });
            } else {
                // Remaining progress at or below zero: `size_at` has returned
                // `to` and the extent becomes a fresh static extent, whose
                // min/max getters read `to` directly — the box is computed
                // from its own `size`, not from a lingering `previousSize`.
                // Snap `previous_size` to the new static size so the delta-0
                // geometry matches the real implementation instead
                // of reading the pre-final-tick size one tick too long.
                self.previous_size = self.size;
            }
        }
    }

    /// Current centre X.
    #[must_use]
    pub fn center_x(&self) -> f64 {
        self.center_x
    }

    /// Current centre Z.
    #[must_use]
    pub fn center_z(&self) -> f64 {
        self.center_z
    }

    /// Current size (`getSize()`). This is the value a shrink/grow lerp is
    /// *broadcast* against — the plan's gate samples it at {0, d/4, d/2, d}.
    #[must_use]
    pub fn size(&self) -> f64 {
        self.size
    }

    /// Size one tick ago — the value
    /// the min/max getters interpolate from at delta 0 (see module docs).
    #[must_use]
    pub fn previous_size(&self) -> f64 {
        self.previous_size
    }

    /// The lerp's target size: `to` while moving,
    /// the current size once static. This is what `initialize_border`'s
    /// `new_size` field carries.
    #[must_use]
    pub fn lerp_target(&self) -> f64 {
        self.lerp.as_ref().map_or(self.size, |lerp| lerp.to)
    }

    /// Remaining lerp ticks (0 for a static extent) —
    /// the `initialize_border` `lerp_time` field.
    #[must_use]
    pub fn lerp_time(&self) -> i64 {
        self.lerp.as_ref().map_or(0, |lerp| lerp.progress)
    }

    /// Read-clamped min X: the real min-x getter is
    /// `center_x - previous_size / 2`, clamped to `±absolute_max_size`.
    #[must_use]
    pub fn get_min_x(&self) -> f64 {
        (self.center_x - self.previous_size / 2.0)
            .clamp(-(self.absolute_max_size as f64), self.absolute_max_size as f64)
    }

    /// Read-clamped max X (see [`get_min_x`](Self::get_min_x)).
    #[must_use]
    pub fn get_max_x(&self) -> f64 {
        (self.center_x + self.previous_size / 2.0)
            .clamp(-(self.absolute_max_size as f64), self.absolute_max_size as f64)
    }

    /// Read-clamped min Z (see [`get_min_x`](Self::get_min_x)).
    #[must_use]
    pub fn get_min_z(&self) -> f64 {
        (self.center_z - self.previous_size / 2.0)
            .clamp(-(self.absolute_max_size as f64), self.absolute_max_size as f64)
    }

    /// Read-clamped max Z (see [`get_min_x`](Self::get_min_x)).
    #[must_use]
    pub fn get_max_z(&self) -> f64 {
        (self.center_z + self.previous_size / 2.0)
            .clamp(-(self.absolute_max_size as f64), self.absolute_max_size as f64)
    }

    /// The `initialize_border` `absolute_max_size` field, the real
    /// absolute-max-size query's value.
    #[must_use]
    pub fn absolute_max_size(&self) -> i32 {
        self.absolute_max_size
    }

    /// The real damage-per-block query — server-side only, never on the wire.
    #[must_use]
    pub fn damage_per_block(&self) -> f64 {
        self.damage_per_block
    }

    /// The real safe-zone query — server-side only, never on the wire.
    #[must_use]
    pub fn safe_zone(&self) -> f64 {
        self.safe_zone
    }

    /// The real warning-blocks query — the `set_border_warning_distance`
    /// packet's value.
    #[must_use]
    pub fn warning_blocks(&self) -> i32 {
        self.warning_blocks
    }

    /// The real warning-time query — the `set_border_warning_delay` packet's
    /// value.
    #[must_use]
    pub fn warning_time(&self) -> i32 {
        self.warning_time
    }

    /// The real within-bounds check with no margin: the
    /// half-open `[min, max)` rectangle, margin 0.
    #[must_use]
    pub fn is_within_bounds(&self, x: f64, z: f64) -> bool {
        self.is_within_bounds_margin(x, z, 0.0)
    }

    /// The real within-bounds check with a margin.
    #[must_use]
    pub fn is_within_bounds_margin(&self, x: f64, z: f64, margin: f64) -> bool {
        x >= self.get_min_x() - margin
            && x < self.get_max_x() + margin
            && z >= self.get_min_z() - margin
            && z < self.get_max_z() + margin
    }

    /// The real distance-to-border query:
    /// the minimum of the four edge distances `z - minZ`, `maxZ - z`,
    /// `x - minX`, `maxX - x`. Positive inside, **negative** past an edge —
    /// the sign the enforcement's `dist + safe_zone < 0` check leans on.
    #[must_use]
    pub fn get_distance_to_border(&self, x: f64, z: f64) -> f64 {
        let from_north = z - self.get_min_z();
        let from_south = self.get_max_z() - z;
        let from_west = x - self.get_min_x();
        let from_east = self.get_max_x() - x;
        from_west.min(from_east).min(from_north).min(from_south)
    }

    /// The damage a player standing at `(x, z)` takes this tick, exactly
    /// the real per-tick border-damage check:
    ///
    /// 1. `dist = distance_to_border + safe_zone`.
    /// 2. If `dist` is negative: read the configured damage per block, and
    ///    if it is positive, deal `max(1, floor(-dist * damage_per_block))`
    ///    out-of-border damage.
    ///
    /// Returns `Some(max(1, floor(-dist * damage_per_block)))` when the
    /// player is more than `safe_zone` blocks past an edge, `None` otherwise.
    ///
    /// The connection task has only a position, not the real player's
    /// bounding box, so the outer "is the whole box within bounds" gate that
    /// wraps this check is skipped — it is implied here: a
    /// position with `dist < 0` is necessarily past the safe zone and
    /// therefore outside the border proper, so the gate can never change the
    /// outcome for a single point.
    #[must_use]
    pub fn damage_for_position(&self, x: f64, z: f64) -> Option<f64> {
        let dist = self.get_distance_to_border(x, z) + self.safe_zone;
        if dist < 0.0 && self.damage_per_block > 0.0 {
            Some(((-dist) * self.damage_per_block).floor().max(1.0))
        } else {
            None
        }
    }

    /// The real centre setter: stores the new
    /// centre verbatim — the real implementation clamps the centre only in
    /// its persisted save-data codec, never on write (see the module doc's
    /// read-clamping note). Geometry stays bounded regardless, because the
    /// min/max getters clamp to `±absolute_max_size` on read.
    pub fn set_center(&mut self, x: f64, z: f64) {
        self.center_x = x;
        self.center_z = z;
    }

    /// The real size setter: snaps to a static
    /// extent of `size`, replacing any in-flight lerp.
    pub fn set_size(&mut self, size: f64) {
        self.lerp = None;
        self.size = size;
        self.previous_size = size;
    }

    /// The real lerp-size-between call: lerps `size` linearly from `from` to
    /// `to` over `ticks` ticks, one step per [`tick`](Self::tick). Equal ends,
    /// or a non-positive duration, land on a static extent — the real
    /// implementation's own `from == to` special case plus the safe
    /// resolution of its degenerate zero-duration lerp (a moving extent with
    /// `duration == 0` would divide by zero in the size calculation).
    ///
    /// `game_time` is accepted for signature parity with the real call but
    /// unused here: the real implementation stores it only to compute the
    /// lerp speed, which this model does not expose.
    pub fn lerp_size_between(&mut self, from: f64, to: f64, ticks: i64, game_time: i64) {
        let _ = game_time;
        if from == to || ticks <= 0 {
            self.set_size(to);
            return;
        }
        self.lerp = Some(BorderLerp {
            from,
            to,
            duration: ticks,
            progress: ticks,
        });
        self.size = from;
        self.previous_size = from;
    }

    /// The real damage-per-block setter.
    pub fn set_damage_per_block(&mut self, damage_per_block: f64) {
        self.damage_per_block = damage_per_block;
    }

    /// The real safe-zone setter.
    pub fn set_safe_zone(&mut self, safe_zone: f64) {
        self.safe_zone = safe_zone;
    }

    /// The real warning-blocks setter.
    pub fn set_warning_blocks(&mut self, warning_blocks: i32) {
        self.warning_blocks = warning_blocks;
    }

    /// The real warning-time setter.
    pub fn set_warning_time(&mut self, warning_time: i32) {
        self.warning_time = warning_time;
    }

}

/// A shared handle to a [`WorldBorder`] — the single memory both halves of the
/// border land on once the ECS migration (shape B) makes it a world `Resource`.
///
/// The same `Arc<Mutex<_>>` + `Clone` idiom [`crate::tick::BlockTickFeed`] and
/// [`crate::tick::ExplosionFeed`] establish, but wrapping **state** rather than
/// an event queue: the world loop (shape B) mutates it; a connection snapshots
/// it with [`get`](Self::get) for the join `initialize_border` broadcast and
/// reads it every vitals tick to compute border damage.
///
/// `crate::tick::run_tick_loop_with_weather` ticks **this shared handle**, and every production
/// `serve_connection*` entry point that can reach a live one threads it
/// through (`IntegratedServer::open_in_memory_with_mobs*`; `bind`'s
/// open-to-LAN path still passes a fresh, unshared default — a stated,
/// separate gap, same shape as that path's own unwired sleep vote), and
/// `crate::commands::worldborder` is the resize entry point
/// [`with`](Self::with) exists for. RCON, a command block and this crate's
/// own test doubles still build a `CommandWorld` with no border to reach
/// (`border: None`), in which case `/worldborder` refuses by name rather
/// than mutating a feed nothing reads — see that module's own doc.
#[derive(Debug, Clone, Default)]
pub struct BorderFeed(Arc<Mutex<WorldBorder>>);

impl BorderFeed {
    /// Snapshots the current border state — the plain [`WorldBorder`] the join
    /// sequence reads (centre/size/warnings for `initialize_border`) and the
    /// per-tick enforcement reads (`damage_for_position`).
    #[must_use]
    pub fn get(&self) -> WorldBorder {
        self.0.lock().expect("border feed lock poisoned").clone()
    }

    /// Mutates the shared border — `crate::tick::run_tick_loop_with_weather`'s
    /// own per-tick `WorldBorder::tick` call and `crate::commands::worldborder`'s
    /// resize entry point (`set_size`, `lerp_size_between`, `set_center`,
    /// `set_damage_per_block`, ...) both go through this.
    pub fn with<R>(&self, f: impl FnOnce(&mut WorldBorder) -> R) -> R {
        let _order = crate::lock_order::acquire(crate::lock_order::LockClass::Border);
        f(&mut *self.0.lock().expect("border feed lock poisoned"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real defaults, exactly as the real default settings record them.
    /// The `warning_time == 300` (not the field
    /// initializer's 15) is the plan's recorded correction — see the module
    /// doc's fidelity note.
    #[test]
    fn default_border_is_static_full_size_with_real_settings() {
        let border = WorldBorder::default();
        assert_eq!(border.center_x(), 0.0);
        assert_eq!(border.center_z(), 0.0);
        assert_eq!(border.size(), MAX_SIZE);
        assert_eq!(border.previous_size(), MAX_SIZE);
        assert_eq!(border.lerp_target(), MAX_SIZE, "static extent targets its own size");
        assert_eq!(border.lerp_time(), 0, "static extent has no lerp");
        assert_eq!(border.damage_per_block(), 0.2);
        assert_eq!(border.safe_zone(), 5.0);
        assert_eq!(border.warning_blocks(), 5);
        assert_eq!(border.warning_time(), 300);
        assert_eq!(border.absolute_max_size(), ABSOLUTE_MAX_SIZE);
    }

    /// A static extent's `tick` is a no-op — the real static extent's
    /// update simply returns itself unchanged.
    #[test]
    fn static_border_tick_changes_nothing() {
        let mut border = WorldBorder::default();
        border.tick();
        assert_eq!(border.size(), MAX_SIZE);
        assert_eq!(border.lerp_time(), 0);
        assert_eq!(border.previous_size(), MAX_SIZE);
    }

    /// **The plan's magnitude gate** (`docs/plans/world-state.md` B1): shrink
    /// via lerp and sample `size` at ticks {0, d/4, d/2, d} against the exact
    /// linear formula `from + (to - from) * (elapsed / d)`. Expected values
    /// are computed here from that formula with outside constants — not from
    /// the code under test — and the sample points are chosen so the
    /// arithmetic is exact in binary (0.25/0.5/1.0 fractions, whole-block
    /// deltas).
    #[test]
    fn lerp_shrink_samples_match_the_exact_linear_formula() {
        let from = 1000.0;
        let to = 100.0;
        let d = 400;
        let mut border = WorldBorder::default();
        border.lerp_size_between(from, to, d, 0);

        // Tick 0: before any `tick`, size is the lerp's start.
        assert_eq!(border.size(), from, "tick 0 is the lerp start");
        assert_eq!(border.lerp_time(), d);

        for _ in 0..d / 4 {
            border.tick();
        }
        let expected_quarter = from + (to - from) * ((d / 4) as f64 / d as f64);
        assert_eq!(border.size(), expected_quarter, "tick d/4");
        assert_eq!(expected_quarter, 775.0, "the formula must land on 775.0");

        for _ in 0..d / 4 {
            border.tick();
        }
        let expected_half = from + (to - from) * ((d / 2) as f64 / d as f64);
        assert_eq!(border.size(), expected_half, "tick d/2");
        assert_eq!(expected_half, 550.0);

        for _ in 0..d / 2 {
            border.tick();
        }
        assert_eq!(border.size(), to, "tick d reaches the target exactly");
        assert_eq!(
            border.lerp_time(),
            0,
            "the completed lerp reports no remaining time"
        );

        // Control: past tick d, the now-static extent keeps the target.
        border.tick();
        assert_eq!(border.size(), to, "a static extent no longer moves");
    }

    /// The lerp is exactly linear, so the same formula sampled at an
    /// arbitrary elapsed tick (not just the plan's quarters) holds — and the
    /// *geometry* lags the broadcast size by one tick during a shrink, because
    /// the real delta-0 min/max getters read the previous-size field (the module doc's
    /// fidelity note). Pinned so a refactor cannot silently switch the getters
    /// to the current `size` and "fix" the lag into a deviation.
    #[test]
    fn geometry_lags_the_broadcast_size_by_one_tick_during_a_lerp() {
        let from = 200.0;
        let to = 100.0;
        let d = 100;
        let mut border = WorldBorder::default();
        border.set_center(0.0, 0.0);
        border.lerp_size_between(from, to, d, 0);
        assert_eq!(border.size(), from);
        assert_eq!(border.get_max_x(), 100.0, "min/max reflect from at start");

        border.tick();
        // After one tick size has moved 1/100 of the way (199.0), but the
        // delta-0 geometry still reflects the pre-tick `from`.
        assert_eq!(border.size(), from + (to - from) * (1.0 / d as f64));
        assert_eq!(border.get_max_x(), 100.0);

        // Once the lerp completes, geometry catches up to `to`.
        for _ in 0..d - 1 {
            border.tick();
        }
        assert_eq!(border.size(), to);
        assert_eq!(border.get_max_x(), 50.0);
    }

    /// **The plan's enforcement gate**: a player `d` blocks past the safe zone
    /// takes exactly `max(1, floor(d * 0.2))` per tick. With a 100-wide
    /// border (edges at ±50) and the default 5-block safe zone, a player at
    /// `z = 65` is 10 past the safe zone and must take exactly 2.0 — not 1.0,
    /// not 2.5, not a sign change.
    #[test]
    fn damage_past_the_safe_zone_is_exactly_floor_d_times_damage_per_block() {
        let mut border = WorldBorder::default();
        border.set_center(0.0, 0.0);
        border.set_size(100.0); // edges at ±50

        // 15 past the edge, 10 past the 5-block safe zone: floor(10 * 0.2) = 2.
        assert_eq!(border.damage_for_position(0.0, 65.0), Some(2.0));
        // Same amount on the other three edges (symmetric).
        assert_eq!(border.damage_for_position(0.0, -65.0), Some(2.0));
        assert_eq!(border.damage_for_position(65.0, 0.0), Some(2.0));
        assert_eq!(border.damage_for_position(-65.0, 0.0), Some(2.0));

        // One block past the safe zone: floor(1 * 0.2) = 0, but the
        // `max(1, ..)` floor means the player still takes exactly 1.
        assert_eq!(border.damage_for_position(0.0, 56.0), Some(1.0));

        // 20 past the safe zone: floor(20 * 0.2) = 4.
        assert_eq!(border.damage_for_position(0.0, 75.0), Some(4.0));
    }

    /// **The zero-detector with its control** (the plan gate's second half):
    /// a player inside the safe zone takes no damage — and the detector
    /// actually works, because moving the same player outside makes the
    /// identical call fire. Without the control the `None` would prove nothing
    /// about the detector.
    #[test]
    fn inside_the_safe_zone_is_zero_damage_and_outside_fires_the_same_call() {
        let mut border = WorldBorder::default();
        border.set_center(0.0, 0.0);
        border.set_size(100.0);

        // 4 blocks inside the edge: distance 4, `4 + 5` safe-zone-buffered
        // distance is positive → no damage.
        assert_eq!(border.damage_for_position(0.0, 46.0), None);
        // Inside but hugging the edge: distance 0.1, still buffered → none.
        assert_eq!(border.damage_for_position(0.0, 49.9), None);

        // **Control**: the same coordinate moves 10 blocks outward and the
        // same call must now return damage — proof the `None` above came from
        // the geometry, not from a detector that never fires.
        assert_eq!(
            border.damage_for_position(0.0, 65.0),
            Some(2.0),
            "the zero-damage call must fail when the player moves outside the safe zone"
        );
    }

    /// `damage_per_block == 0` disables border damage entirely, even deep
    /// past the safe zone — the real per-tick check's own positive-damage
    /// gate.
    #[test]
    fn zero_damage_per_block_disables_damage() {
        let mut border = WorldBorder::default();
        border.set_center(0.0, 0.0);
        border.set_size(100.0);
        border.set_damage_per_block(0.0);
        assert_eq!(border.damage_for_position(0.0, 65.0), None);
    }

    /// Read-clamping: with a centred full-size border, `center - size/2`
    /// would be `-29_999_985.0`, one past the `±absolute_max_size` clamp, so
    /// the getters land exactly on `±29_999_984` — the `initialize_border`
    /// `absolute_max_size` value. An even larger size changes nothing further.
    #[test]
    fn geometry_is_clamped_to_absolute_max_at_read_time() {
        let mut border = WorldBorder::default();
        border.set_center(0.0, 0.0);
        border.set_size(MAX_SIZE);
        assert_eq!(border.get_min_x(), -(ABSOLUTE_MAX_SIZE as f64));
        assert_eq!(border.get_max_x(), ABSOLUTE_MAX_SIZE as f64);
        assert_eq!(border.get_min_z(), -(ABSOLUTE_MAX_SIZE as f64));
        assert_eq!(border.get_max_z(), ABSOLUTE_MAX_SIZE as f64);

        border.set_size(MAX_SIZE * 2.0);
        assert_eq!(border.get_min_x(), -(ABSOLUTE_MAX_SIZE as f64), "still clamped");
        assert_eq!(border.get_max_x(), ABSOLUTE_MAX_SIZE as f64);
    }

    /// `set_size` snaps the current extent to a static one, replacing any
    /// in-flight lerp — the real size setter replaces the whole extent wholesale.
    #[test]
    fn set_size_snaps_an_in_flight_lerp() {
        let mut border = WorldBorder::default();
        border.lerp_size_between(1000.0, 100.0, 400, 0);
        border.tick();
        assert_ne!(border.size(), 500.0);

        border.set_size(500.0);
        assert_eq!(border.size(), 500.0);
        assert_eq!(border.lerp_time(), 0);
        assert_eq!(border.lerp_target(), 500.0);

        border.tick();
        assert_eq!(border.size(), 500.0, "snapped extent stays put");
    }

    /// `lerp_size_between` with equal ends — or a non-positive duration —
    /// lands directly on a static extent, never a moving extent that
    /// would divide by zero (the real implementation's own `from == to`
    /// branch).
    #[test]
    fn equal_ends_or_zero_duration_are_static() {
        let mut border = WorldBorder::default();
        border.lerp_size_between(500.0, 500.0, 400, 0);
        assert_eq!(border.lerp_time(), 0);
        assert_eq!(border.size(), 500.0);

        border.lerp_size_between(1000.0, 200.0, 0, 0);
        assert_eq!(border.lerp_time(), 0);
        assert_eq!(border.size(), 200.0);
    }

    /// `is_within_bounds` uses the same read-clamped geometry as
    /// `get_distance_to_border` — a point just inside the edge is within,
    /// one at the edge is not (the half-open `[min, max)` convention), and the
    /// margin variant widens the rect.
    #[test]
    fn within_bounds_tracks_the_half_open_geometry() {
        let mut border = WorldBorder::default();
        border.set_center(0.0, 0.0);
        border.set_size(100.0);
        assert!(border.is_within_bounds(0.0, 0.0));
        assert!(border.is_within_bounds(49.9, 49.9));
        assert!(!border.is_within_bounds(50.0, 0.0), "max is half-open");
        assert!(!border.is_within_bounds(0.0, 60.0));
        assert!(border.is_within_bounds_margin(0.0, 51.0, 5.0));
        assert!(!border.is_within_bounds_margin(0.0, 60.0, 5.0));
    }

    /// The feed's `get` is a snapshot and `with` mutates the shared border so
    /// a later `get` sees it — the two operations shape B reconnects, so this
    /// pins the mechanism.
    #[test]
    fn border_feed_mutation_is_visible_through_get() {
        let feed = BorderFeed::default();
        assert_eq!(feed.get().size(), MAX_SIZE);

        feed.with(|border| border.lerp_size_between(1000.0, 100.0, 400, 0));
        assert_eq!(feed.get().lerp_time(), 400);

        // `get` is a copy: mutating the snapshot must not touch the feed.
        let mut snapshot = feed.get();
        snapshot.set_size(1.0);
        assert_eq!(feed.get().lerp_time(), 400, "the feed is unchanged");
    }
}
