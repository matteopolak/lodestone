//! The world border: centre, size (including a smooth resize in flight),
//! warning distance and warning delay, folded from the six
//! `ClientEvent::WorldBorder*` variants.
//!
//! # What it is
//!
//! A pure fold over the world-border packet family. Before this module all six
//! variants decoded correctly, were covered by
//! `crates/versions/26.2/tests/world_border.rs`, and then reached **nothing** —
//! `lodestone_model::event::route` returned `Route::NOWHERE` for every one of
//! them. They were the largest single cluster in `docs/event-routing.md`'s island
//! list, and `docs/plans/world-state.md` §B2 is the consumer side.
//!
//! # How it works
//!
//! Vanilla splits the size into two `BorderExtent` implementations and this
//! mirrors that split exactly, because the two behave differently at *read*
//! time, not just at write time:
//!
//! * [`BorderExtent::Static`] — one size, forever, until the next packet.
//! * [`BorderExtent::Moving`] — a wall-clock interpolation from `from` to `to`
//!   over `duration_ms`.
//!
//! The interesting property, and the one worth stating because it is easy to get
//! backwards: **vanilla does not clamp the centre when it is set. It clamps the
//! edges when they are read** (the plan records this
//! at `docs/plans/world-state.md`). So `apply` stores whatever the server
//! sent, and [`WorldBorder::min_x`] and its three siblings do the clamping. A
//! fold that clamped on write would disagree with vanilla for any centre beyond
//! ±[`MAX_CENTER_COORDINATE`], and would do so invisibly.
//!
//! # The clock, and why `apply` does not take one
//!
//! A resize is defined by wall time, but `apply(&ClientEvent) -> bool` is the
//! shape every other aggregate in this crate uses (see
//! [`crate::tablist::TabList::apply`]) and threading a clock through it would fork
//! that convention for one caller. Instead the fold records the resize with
//! `started_secs: None` and [`WorldBorder::stamp`] fills it in; the *system*
//! (`lodestone_ecs::session::apply_world_border`) owns the clock and calls
//! `stamp` immediately after a handled event.
//!
//! An unstamped resize reads as "not yet started", i.e. [`WorldBorder::size_at`]
//! returns `from`. That is deliberately inert rather than a panic or a guess: a
//! harness with no `FrameClock` gets a border that holds its old size instead of
//! one that has silently teleported to `to`.
//!
//! # How to change it
//!
//! The constants below are transcribed from `.cache/mc/26.2/src/` and each cites
//! its line. Two traps live in them:
//!
//! * [`MAX_SIZE`] is `5.999997E7` in 26.2 and was `5.9999968E7` in 1.21. The two
//!   differ in the seventh significant figure, so a stale value looks right.
//! * [`DEFAULT_WARNING_TIME`] is **300**, not the `15` the field initializer
//!   shows. Vanilla's own field initializer says 15, its own settings default says 300,
//!   and its own initial-settings-apply step always overwrites — and
//!   the level's own world-border accessor always calls it.
//!   The 15 is dead code. Do not "correct" this back.
//!
//! `damage_per_block` (0.2) and the 5.0 safe zone are **not** modelled here, and
//! that is not an omission: vanilla never sends either to the client
//! (vanilla's own player-list registers no listener for them), so a client-side
//! copy would be a guess dressed as state.
//!
//! # Dependencies
//!
//! `lodestone_model::event::ClientEvent` only. No ECS, no renderer — the ECS
//! wrapper is `lodestone_ecs::session::SessionWorldBorder`.

use lodestone_model::event::ClientEvent;

/// The largest diameter a border may take, vanilla's own max-size constant.
///
/// 26.2's value. 1.21 had `5.9999968E7`; see the module docs.
pub const MAX_SIZE: f64 = 5.999_997E7;

/// The magnitude each border edge is clamped to when read,
/// vanilla's own max-center-coordinate constant.
pub const MAX_CENTER_COORDINATE: f64 = 2.999_998_4E7;

/// Vanilla's default warning distance in blocks.
pub const DEFAULT_WARNING_BLOCKS: i32 = 5;

/// Vanilla's *effective* default warning delay in seconds — vanilla's own
/// settings default, not the dead `15` field initializer. See the module docs.
pub const DEFAULT_WARNING_TIME: i32 = 300;

/// How the border's diameter behaves over time.
///
/// Mirrors vanilla's own static/moving border-extent split.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BorderExtent {
    /// A fixed diameter.
    Static {
        /// The diameter, in blocks.
        size: f64,
    },
    /// A resize in flight, interpolated on wall time.
    Moving {
        /// Diameter the resize started from.
        from: f64,
        /// Diameter the resize ends at.
        to: f64,
        /// How long the resize takes, in milliseconds, as the server sent it.
        duration_ms: i64,
        /// Monotonic [`crate::worldborder::WorldBorder::stamp`] reading at which
        /// this resize began, or `None` while it is still unstamped.
        started_secs: Option<f64>,
    },
}

impl BorderExtent {
    /// The diameter this extent reports at monotonic time `now_secs`.
    ///
    /// Vanilla's own moving-extent size getter: `t = elapsed / duration`, then
    /// `t < 1 ? lerp(t, from, to) : to`. A non-positive `duration_ms` therefore
    /// snaps to `to` — in vanilla by way of a division by zero producing an
    /// infinite `t`, here by an explicit guard, which is the same answer without
    /// relying on float infinity semantics.
    #[must_use]
    pub fn size_at(self, now_secs: f64) -> f64 {
        match self {
            Self::Static { size } => size,
            Self::Moving {
                from,
                to,
                duration_ms,
                started_secs,
            } => {
                let Some(started) = started_secs else {
                    // Unstamped: the resize has not begun as far as this fold is
                    // concerned. Hold the old size rather than guess.
                    return from;
                };
                if duration_ms <= 0 {
                    return to;
                }
                #[allow(clippy::cast_precision_loss)]
                let duration_secs = duration_ms as f64 / 1000.0;
                let t = (now_secs - started) / duration_secs;
                if t >= 1.0 {
                    to
                } else if t <= 0.0 {
                    from
                } else {
                    // vanilla's own linear-interpolation helper.
                    from + t * (to - from)
                }
            }
        }
    }

    /// The diameter this extent settles at once any resize completes.
    #[must_use]
    pub fn target_size(self) -> f64 {
        match self {
            Self::Static { size } => size,
            Self::Moving { to, .. } => to,
        }
    }
}

impl Default for BorderExtent {
    fn default() -> Self {
        Self::Static { size: MAX_SIZE }
    }
}

/// The folded world border.
///
/// [`Default`] is vanilla's own default border — full [`MAX_SIZE`] at the origin
/// — so a session that has received no border packet behaves as an unbounded
/// world rather than a zero-size one. [`initialized`](Self::initialized)
/// distinguishes the two honestly for anything that needs to know.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldBorder {
    /// Centre X, exactly as the server sent it — **not** clamped. See the module
    /// docs.
    pub center_x: f64,
    /// Centre Z, exactly as the server sent it — **not** clamped.
    pub center_z: f64,
    /// How the diameter behaves over time.
    pub extent: BorderExtent,
    /// Distance in blocks at which the warning overlay appears.
    pub warning_blocks: i32,
    /// Seconds of lead time at which an incoming shrink starts warning.
    pub warning_time: i32,
    /// The largest diameter this server will ever use, from
    /// [`ClientEvent::WorldBorderInitialized`]. `None` until that packet
    /// arrives — the incremental variants do not carry it.
    pub absolute_max_size: Option<f64>,
    /// Whether [`ClientEvent::WorldBorderInitialized`] has been seen. `false`
    /// means every field is either a default or an incremental update applied to
    /// one.
    pub initialized: bool,
}

impl Default for WorldBorder {
    fn default() -> Self {
        Self {
            center_x: 0.0,
            center_z: 0.0,
            extent: BorderExtent::default(),
            warning_blocks: DEFAULT_WARNING_BLOCKS,
            warning_time: DEFAULT_WARNING_TIME,
            absolute_max_size: None,
            initialized: false,
        }
    }
}

impl WorldBorder {
    /// Fold one event, returning whether it belonged to this aggregate.
    ///
    /// The `bool` is the convention every aggregate in this crate uses; the
    /// session system uses it to decide whether to [`stamp`](Self::stamp).
    #[allow(clippy::cast_lossless)]
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::WorldBorderCenterChanged { x, z } => {
                self.center_x = *x;
                self.center_z = *z;
                true
            }
            ClientEvent::WorldBorderSizeChanged { size } => {
                self.extent = BorderExtent::Static { size: *size };
                true
            }
            ClientEvent::WorldBorderSizeLerping {
                old_size,
                new_size,
                lerp_time_ms,
            } => {
                self.extent = Self::extent_for_lerp(*old_size, *new_size, *lerp_time_ms);
                true
            }
            ClientEvent::WorldBorderWarningDelayChanged { warning_time } => {
                self.warning_time = *warning_time;
                true
            }
            ClientEvent::WorldBorderWarningDistanceChanged { warning_blocks } => {
                self.warning_blocks = *warning_blocks;
                true
            }
            ClientEvent::WorldBorderInitialized {
                x,
                z,
                old_size,
                new_size,
                lerp_time_ms,
                absolute_max_size,
                warning_blocks,
                warning_time,
            } => {
                self.center_x = *x;
                self.center_z = *z;
                self.extent = Self::extent_for_lerp(*old_size, *new_size, *lerp_time_ms);
                self.absolute_max_size = Some(f64::from(*absolute_max_size));
                self.warning_blocks = *warning_blocks;
                self.warning_time = *warning_time;
                self.initialized = true;
                true
            }
            _ => false,
        }
    }

    /// Vanilla's own lerp-size-between step: equal endpoints
    /// collapse to a static extent rather than a degenerate resize.
    fn extent_for_lerp(old_size: f64, new_size: f64, lerp_time_ms: i64) -> BorderExtent {
        if (old_size - new_size).abs() < f64::EPSILON {
            BorderExtent::Static { size: new_size }
        } else {
            BorderExtent::Moving {
                from: old_size,
                to: new_size,
                duration_ms: lerp_time_ms,
                started_secs: None,
            }
        }
    }

    /// Give an unstamped resize its start time, from the driver's monotonic
    /// clock.
    ///
    /// Idempotent: a resize that already has a start time keeps it, so calling
    /// this on every folded event cannot restart an interpolation that is
    /// already running.
    pub fn stamp(&mut self, now_secs: f64) {
        if let BorderExtent::Moving { started_secs, .. } = &mut self.extent {
            if started_secs.is_none() {
                *started_secs = Some(now_secs);
            }
        }
    }

    /// The border's diameter at monotonic time `now_secs`.
    #[must_use]
    pub fn size_at(&self, now_secs: f64) -> f64 {
        self.extent.size_at(now_secs)
    }

    /// The diameter the border settles at once any resize completes.
    #[must_use]
    pub fn target_size(&self) -> f64 {
        self.extent.target_size()
    }

    /// Whether a resize is currently in flight.
    #[must_use]
    pub fn is_resizing(&self) -> bool {
        matches!(self.extent, BorderExtent::Moving { .. })
    }

    /// West edge at `now_secs`, clamped to ±[`MAX_CENTER_COORDINATE`].
    ///
    /// The clamp is at read time, matching vanilla's own edge accessors. See the
    /// module docs for why that matters.
    #[must_use]
    pub fn min_x(&self, now_secs: f64) -> f64 {
        (self.center_x - self.size_at(now_secs) / 2.0)
            .clamp(-MAX_CENTER_COORDINATE, MAX_CENTER_COORDINATE)
    }

    /// East edge at `now_secs`, clamped to ±[`MAX_CENTER_COORDINATE`].
    #[must_use]
    pub fn max_x(&self, now_secs: f64) -> f64 {
        (self.center_x + self.size_at(now_secs) / 2.0)
            .clamp(-MAX_CENTER_COORDINATE, MAX_CENTER_COORDINATE)
    }

    /// North edge at `now_secs`, clamped to ±[`MAX_CENTER_COORDINATE`].
    #[must_use]
    pub fn min_z(&self, now_secs: f64) -> f64 {
        (self.center_z - self.size_at(now_secs) / 2.0)
            .clamp(-MAX_CENTER_COORDINATE, MAX_CENTER_COORDINATE)
    }

    /// South edge at `now_secs`, clamped to ±[`MAX_CENTER_COORDINATE`].
    #[must_use]
    pub fn max_z(&self, now_secs: f64) -> f64 {
        (self.center_z + self.size_at(now_secs) / 2.0)
            .clamp(-MAX_CENTER_COORDINATE, MAX_CENTER_COORDINATE)
    }

    /// Shortest distance from `(x, z)` to the nearest border edge at
    /// `now_secs`; negative when outside.
    ///
    /// Vanilla's own distance-to-border step, which is what the
    /// warning overlay's opacity and the server's damage tick both read.
    #[must_use]
    pub fn distance_to_border(&self, x: f64, z: f64, now_secs: f64) -> f64 {
        let west = x - self.min_x(now_secs);
        let east = self.max_x(now_secs) - x;
        let north = z - self.min_z(now_secs);
        let south = self.max_z(now_secs) - z;
        west.min(east).min(north).min(south)
    }

    /// Whether `(x, z)` is inside the border at `now_secs`.
    #[must_use]
    pub fn is_within(&self, x: f64, z: f64, now_secs: f64) -> bool {
        self.distance_to_border(x, z, now_secs) > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The expected values here come from the 26.2 decompile's own literals,
    /// transcribed in `docs/plans/world-state.md` — outside this code,
    /// which is what `CLAUDE.md`'s evidence standard requires. A
    /// `decode(encode(x))` pair over our own defaults would prove nothing.
    #[test]
    fn defaults_match_vanillas_effective_settings() {
        let b = WorldBorder::default();
        assert_eq!(b.warning_blocks, 5, "vanilla's own default-warning-blocks constant");
        assert_eq!(
            b.warning_time, 300,
            "vanilla's own effective settings default — not the dead 15 field initializer"
        );
        assert!(!b.initialized);
        assert_eq!(b.absolute_max_size, None);
        // The 26.2 value, distinguished from 1.21's 5.9999968E7. Asserting the
        // difference is the point: the two are equal to six significant figures.
        assert!(
            (MAX_SIZE - 5.999_997E7).abs() < f64::EPSILON,
            "26.2 MAX_SIZE"
        );
        assert!(
            (MAX_SIZE - 5.999_996_8E7).abs() > 1.0,
            "must not be 1.21's value: {MAX_SIZE}"
        );
    }

    #[test]
    fn center_is_stored_unclamped_but_edges_clamp_on_read() {
        let mut b = WorldBorder::default();
        // Well beyond MAX_CENTER_COORDINATE.
        let far = 9.0e7;
        assert!(b.apply(&ClientEvent::WorldBorderCenterChanged { x: far, z: -far }));
        // Stored verbatim -- vanilla does not clamp on set.
        assert!((b.center_x - far).abs() < f64::EPSILON);
        assert!((b.center_z + far).abs() < f64::EPSILON);
        // ...but every edge is clamped when read.
        for edge in [b.min_x(0.0), b.max_x(0.0), b.min_z(0.0), b.max_z(0.0)] {
            assert!(
                edge.abs() <= MAX_CENTER_COORDINATE,
                "edge {edge} escaped the read-time clamp"
            );
        }
    }

    /// The lerp is predicted, not merely asserted to move in the right
    /// direction — `CLAUDE.md`'s *magnitude* species of vacuous test. At the
    /// halfway point a correct implementation reads 150.0 and the plausible
    /// wrong one (interpolating on ticks rather than milliseconds, i.e.
    /// `duration/20`) reads the completed 200.0. The assertion separates them.
    #[test]
    fn lerp_lands_on_the_predicted_midpoint_not_merely_between_the_endpoints() {
        let mut b = WorldBorder::default();
        assert!(b.apply(&ClientEvent::WorldBorderSizeLerping {
            old_size: 100.0,
            new_size: 200.0,
            lerp_time_ms: 4_000,
        }));
        assert!(b.is_resizing());
        // Unstamped: holds `from`.
        assert!((b.size_at(1_000.0) - 100.0).abs() < f64::EPSILON);

        b.stamp(10.0);
        assert!((b.size_at(10.0) - 100.0).abs() < 1e-9, "t=0");
        let mid = b.size_at(12.0);
        assert!(
            (mid - 150.0).abs() < 1e-9,
            "t=0.5 must read the predicted 150.0, got {mid}"
        );
        assert!(
            (mid - 200.0).abs() > 40.0,
            "and must not have already completed (the ms/tick confusion), got {mid}"
        );
        assert!((b.size_at(14.0) - 200.0).abs() < 1e-9, "t=1");
        assert!((b.size_at(9_999.0) - 200.0).abs() < 1e-9, "past the end, clamped");
    }

    #[test]
    fn stamp_is_idempotent_so_a_later_event_cannot_restart_a_running_resize() {
        let mut b = WorldBorder::default();
        b.apply(&ClientEvent::WorldBorderSizeLerping {
            old_size: 0.0,
            new_size: 100.0,
            lerp_time_ms: 1_000,
        });
        b.stamp(5.0);
        b.stamp(500.0);
        // Had the second stamp won, this would read 0.0 rather than the
        // completed 100.0.
        assert!((b.size_at(6.0) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn equal_endpoints_collapse_to_a_static_extent() {
        let mut b = WorldBorder::default();
        b.apply(&ClientEvent::WorldBorderSizeLerping {
            old_size: 64.0,
            new_size: 64.0,
            lerp_time_ms: 5_000,
        });
        assert!(!b.is_resizing(), "vanilla lerpSizeBetween collapses this");
        assert!((b.size_at(0.0) - 64.0).abs() < f64::EPSILON);
    }

    #[test]
    fn zero_duration_snaps_to_the_target() {
        let mut b = WorldBorder::default();
        b.apply(&ClientEvent::WorldBorderSizeLerping {
            old_size: 10.0,
            new_size: 20.0,
            lerp_time_ms: 0,
        });
        b.stamp(1.0);
        assert!((b.size_at(1.0) - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn initialized_sets_every_field_at_once() {
        let mut b = WorldBorder::default();
        assert!(b.apply(&ClientEvent::WorldBorderInitialized {
            x: 8.0,
            z: -16.0,
            old_size: 60.0,
            new_size: 60.0,
            lerp_time_ms: 0,
            absolute_max_size: 29_999_984,
            warning_blocks: 7,
            warning_time: 21,
        }));
        assert!(b.initialized);
        assert!((b.center_x - 8.0).abs() < f64::EPSILON);
        assert!((b.center_z + 16.0).abs() < f64::EPSILON);
        assert!((b.target_size() - 60.0).abs() < f64::EPSILON);
        assert_eq!(b.warning_blocks, 7);
        assert_eq!(b.warning_time, 21);
        assert_eq!(b.absolute_max_size, Some(29_999_984.0));
    }

    /// Distance is predicted from the geometry, and the inside/outside sign is
    /// asserted at both ends so a sign flip cannot pass.
    #[test]
    fn distance_to_border_is_signed_and_measured_to_the_nearest_edge() {
        let mut b = WorldBorder::default();
        b.apply(&ClientEvent::WorldBorderInitialized {
            x: 0.0,
            z: 0.0,
            old_size: 100.0,
            new_size: 100.0,
            lerp_time_ms: 0,
            absolute_max_size: 29_999_984,
            warning_blocks: 5,
            warning_time: 300,
        });
        // Half-size 50. At the centre every edge is 50 away.
        assert!((b.distance_to_border(0.0, 0.0, 0.0) - 50.0).abs() < 1e-9);
        // 10 east of centre: nearest edge is the east one, 40 away.
        assert!((b.distance_to_border(10.0, 0.0, 0.0) - 40.0).abs() < 1e-9);
        // On the edge: zero, and therefore not "within".
        assert!((b.distance_to_border(50.0, 0.0, 0.0)).abs() < 1e-9);
        assert!(!b.is_within(50.0, 0.0, 0.0));
        // Outside: negative.
        assert!(b.distance_to_border(60.0, 0.0, 0.0) < 0.0);
        assert!(!b.is_within(60.0, 0.0, 0.0));
        assert!(b.is_within(49.0, 0.0, 0.0));
    }

    /// The negative control for `apply`'s `_ => false` arm: an unrelated event
    /// must be rejected *and* leave every field untouched. Without this, an
    /// over-broad match would silently claim events another fold owns.
    #[test]
    fn unrelated_events_are_rejected_and_change_nothing() {
        let mut b = WorldBorder::default();
        let before = b;
        assert!(!b.apply(&ClientEvent::KeepAlive { id: 3 }));
        // A near-miss from the same subsystem family: `SimulationDistanceChanged`
        // is also a world-scalar packet and also an island, so an over-broad
        // match is a live risk rather than a theoretical one.
        assert!(!b.apply(&ClientEvent::SimulationDistanceChanged { distance: 8 }));
        assert_eq!(before, b);
    }
}
