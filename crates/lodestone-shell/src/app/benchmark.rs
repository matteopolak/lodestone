//! Pure state machine for deterministic live client frame benchmarks.
//!
//! Window, network, GPU, and input mutation stay in the caller. This module
//! only turns a connection state plus wall-clock time into a segment and an
//! input intent, which keeps the benchmark choreography unit-testable.

use std::time::Duration;

use crate::config::{BenchmarkConfig, BenchmarkWorkload};
use crate::platform::Instant;

/// One stable phase of a live benchmark session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BenchmarkSegment {
    /// The client has not reached the live connected phase yet. No benchmark
    /// duration is consumed while waiting here.
    WaitingForJoin,
    /// Joined-world settling period, excluded from reported measurements.
    Warmup,
    /// Heavyweight scene mutations have been dispatched; their frame rows are
    /// retained as reachability evidence rather than timing comparisons.
    Mutation,
    /// Fixed-view measurement period.
    Stationary,
    /// Terrain flight or showcase camera orbit measurement period.
    Moving,
    /// Every configured duration has elapsed.
    Complete,
}

/// Input requested for one redraw by [`BenchmarkDriver`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BenchmarkIntent {
    pub segment: BenchmarkSegment,
    pub forward: bool,
    pub sprint: bool,
    pub jump: bool,
    /// Raw horizontal mouse pixels to add during this redraw.
    pub mouse_dx: f32,
    pub complete: bool,
}

impl BenchmarkIntent {
    fn idle(segment: BenchmarkSegment) -> Self {
        Self {
            segment,
            forward: false,
            sprint: false,
            jump: false,
            mouse_dx: 0.0,
            complete: segment == BenchmarkSegment::Complete,
        }
    }
}

/// Deterministic wall-clock choreography for one benchmark run.
#[derive(Debug, Clone)]
pub(crate) struct BenchmarkDriver {
    config: BenchmarkConfig,
    joined_at: Option<Instant>,
    previous_elapsed: Option<Duration>,
}

impl BenchmarkDriver {
    /// Build a driver whose clock will not start until the first connected
    /// update.
    pub(crate) const fn new(config: BenchmarkConfig) -> Self {
        Self {
            config,
            joined_at: None,
            previous_elapsed: None,
        }
    }

    /// Workload configured for this run.
    pub(crate) const fn workload(&self) -> BenchmarkWorkload {
        self.config.workload
    }

    /// Elapsed time since the first connected update, if joining has
    /// happened. Useful for transition logs; segment selection itself stays
    /// inside [`Self::update`].
    pub(crate) fn elapsed(&self, now: Instant) -> Option<Duration> {
        self.joined_at
            .map(|joined| now.saturating_duration_since(joined))
    }

    /// Stable CSV/log label for `segment` under this driver's workload.
    pub(crate) const fn label(&self, segment: BenchmarkSegment) -> &'static str {
        match (self.config.workload, segment) {
            (BenchmarkWorkload::Terrain, BenchmarkSegment::WaitingForJoin) => {
                "terrain.waiting_for_join"
            }
            (BenchmarkWorkload::Terrain, BenchmarkSegment::Warmup) => "terrain.warmup",
            (BenchmarkWorkload::Terrain, BenchmarkSegment::Mutation) => "terrain.mutation",
            (BenchmarkWorkload::Terrain, BenchmarkSegment::Stationary) => "terrain.stationary",
            (BenchmarkWorkload::Terrain, BenchmarkSegment::Moving) => "terrain.moving",
            (BenchmarkWorkload::Terrain, BenchmarkSegment::Complete) => "terrain.complete",
            (BenchmarkWorkload::Showcase, BenchmarkSegment::WaitingForJoin) => {
                "showcase.waiting_for_join"
            }
            (BenchmarkWorkload::Showcase, BenchmarkSegment::Warmup) => "showcase.warmup",
            (BenchmarkWorkload::Showcase, BenchmarkSegment::Mutation) => "showcase.mutation",
            (BenchmarkWorkload::Showcase, BenchmarkSegment::Stationary) => "showcase.stationary",
            (BenchmarkWorkload::Showcase, BenchmarkSegment::Moving) => "showcase.moving",
            (BenchmarkWorkload::Showcase, BenchmarkSegment::Complete) => "showcase.complete",
            (BenchmarkWorkload::Megaworld, BenchmarkSegment::WaitingForJoin) => {
                "megaworld.waiting_for_join"
            }
            (BenchmarkWorkload::Megaworld, BenchmarkSegment::Warmup) => "megaworld.warmup",
            (BenchmarkWorkload::Megaworld, BenchmarkSegment::Mutation) => "megaworld.mutation",
            (BenchmarkWorkload::Megaworld, BenchmarkSegment::Stationary) => {
                "megaworld.stationary"
            }
            (BenchmarkWorkload::Megaworld, BenchmarkSegment::Moving) => "megaworld.moving",
            (BenchmarkWorkload::Megaworld, BenchmarkSegment::Complete) => "megaworld.complete",
            (BenchmarkWorkload::Lovelier, BenchmarkSegment::WaitingForJoin) => {
                "lovelier.waiting_for_join"
            }
            (BenchmarkWorkload::Lovelier, BenchmarkSegment::Warmup) => "lovelier.warmup",
            (BenchmarkWorkload::Lovelier, BenchmarkSegment::Mutation) => "lovelier.mutation",
            (BenchmarkWorkload::Lovelier, BenchmarkSegment::Stationary) => {
                "lovelier.stationary"
            }
            (BenchmarkWorkload::Lovelier, BenchmarkSegment::Moving) => "lovelier.moving",
            (BenchmarkWorkload::Lovelier, BenchmarkSegment::Complete) => "lovelier.complete",
            (BenchmarkWorkload::Heavyweight, BenchmarkSegment::WaitingForJoin) => {
                "heavyweight.waiting_for_join"
            }
            (BenchmarkWorkload::Heavyweight, BenchmarkSegment::Warmup) => "heavyweight.warmup",
            (BenchmarkWorkload::Heavyweight, BenchmarkSegment::Mutation) => "heavyweight.mutation",
            (BenchmarkWorkload::Heavyweight, BenchmarkSegment::Stationary) => {
                "heavyweight.stationary"
            }
            (BenchmarkWorkload::Heavyweight, BenchmarkSegment::Moving) => "heavyweight.moving",
            (BenchmarkWorkload::Heavyweight, BenchmarkSegment::Complete) => "heavyweight.complete",
        }
    }

    /// Advance the state machine and return the input for this redraw.
    /// `connected` is only a start gate: once a real join has occurred, a
    /// later disconnect does not reset or silently restart the measurement.
    pub(crate) fn update(&mut self, now: Instant, connected: bool) -> BenchmarkIntent {
        let Some(joined_at) = self.joined_at else {
            if !connected {
                return BenchmarkIntent::idle(BenchmarkSegment::WaitingForJoin);
            }
            self.joined_at = Some(now);
            self.previous_elapsed = Some(Duration::ZERO);
            return self.intent_for(Duration::ZERO, Duration::ZERO);
        };

        let elapsed = now.saturating_duration_since(joined_at);
        let previous_elapsed = self.previous_elapsed.unwrap_or(elapsed).min(elapsed);
        let intent = self.intent_for(elapsed, previous_elapsed);
        self.previous_elapsed = Some(elapsed);
        intent
    }

    fn intent_for(&self, elapsed: Duration, previous_elapsed: Duration) -> BenchmarkIntent {
        let mutation_start = self.config.warmup;
        let stationary_start = mutation_start.saturating_add(self.config.mutation);
        let moving_start = stationary_start.saturating_add(self.config.stationary);
        let complete_at = moving_start.saturating_add(self.config.moving);

        if elapsed < mutation_start {
            let mut intent = BenchmarkIntent::idle(BenchmarkSegment::Warmup);
            if matches!(
                self.config.workload,
                BenchmarkWorkload::Terrain
                    | BenchmarkWorkload::Megaworld
                    | BenchmarkWorkload::Lovelier
            ) {
                // Two press edges, separated by releases, enable creative
                // flight without relying on server commands or menu state.
                // Wait one second: the Python runner sees the warmup marker
                // and applies post-join gamemode/teleport commands first.
                let flight_edge = elapsed.as_millis();
                intent.jump = (1_000..1_075).contains(&flight_edge)
                    || (1_150..1_225).contains(&flight_edge);
            }
            return intent;
        }
        if elapsed < stationary_start {
            return BenchmarkIntent::idle(BenchmarkSegment::Mutation);
        }
        if elapsed < moving_start {
            return BenchmarkIntent::idle(BenchmarkSegment::Stationary);
        }
        if elapsed >= complete_at {
            return BenchmarkIntent::idle(BenchmarkSegment::Complete);
        }

        let mut intent = BenchmarkIntent::idle(BenchmarkSegment::Moving);
        match self.config.workload {
            BenchmarkWorkload::Terrain
            | BenchmarkWorkload::Megaworld
            | BenchmarkWorkload::Lovelier => {
                intent.forward = true;
                intent.sprint = true;
                // Climb clear of the authored spawn before settling into the
                // orbit. A straight horizontal walk eventually becomes a wall
                // benchmark in any real build.
                intent.jump = elapsed.saturating_sub(moving_start) < Duration::from_secs(5);
            }
            BenchmarkWorkload::Showcase | BenchmarkWorkload::Heavyweight => {}
        }
        if self.config.workload == BenchmarkWorkload::Heavyweight
            && self
                .config
                .heavyweight
                .as_ref()
                .is_some_and(|heavy| heavy.camera_plan == "stationary")
        {
            return intent;
        }
        // At the canonical 0.5 sensitivity, 0.15 degrees/raw pixel. Integrate
        // only the portion of this update overlapping the moving segment,
        // making the total exactly one orbit even if redraw cadence changes.
        let overlap_start = previous_elapsed.max(moving_start);
        let overlap = elapsed.saturating_sub(overlap_start);
        let moving_seconds = self.config.moving.as_secs_f32();
        if moving_seconds > 0.0 {
            intent.mouse_dx = (360.0 / 0.15) * overlap.as_secs_f32() / moving_seconds;
        }
        intent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_config() -> BenchmarkConfig {
        BenchmarkConfig {
            workload: BenchmarkWorkload::Terrain,
            debug_overlay: crate::config::BenchmarkDebugOverlay::Closed,
            heavyweight: None,
            warmup: Duration::from_secs(20),
            mutation: Duration::ZERO,
            stationary: Duration::from_secs(30),
            moving: Duration::from_secs(60),
        }
    }

    #[test]
    fn time_does_not_start_before_the_live_join() {
        let t0 = Instant::now();
        let mut driver = BenchmarkDriver::new(fixture_config());
        assert_eq!(
            driver.update(t0, false).segment,
            BenchmarkSegment::WaitingForJoin
        );
        assert_eq!(
            driver.update(t0 + Duration::from_secs(99), false).segment,
            BenchmarkSegment::WaitingForJoin
        );
        assert_eq!(
            driver.update(t0 + Duration::from_secs(100), true).segment,
            BenchmarkSegment::Warmup
        );
    }

    #[test]
    fn terrain_runs_warmup_stationary_moving_then_completes() {
        let t0 = Instant::now();
        let mut driver = BenchmarkDriver::new(fixture_config());
        let _ = driver.update(t0, true);
        assert_eq!(
            driver.update(t0 + Duration::from_secs(20), true).segment,
            BenchmarkSegment::Stationary
        );
        let moving = driver.update(t0 + Duration::from_secs(51), true);
        assert_eq!(moving.segment, BenchmarkSegment::Moving);
        assert!(moving.forward && moving.sprint);
        assert!(driver.update(t0 + Duration::from_secs(110), true).complete);
    }

    #[test]
    fn showcase_orbits_without_translation() {
        let t0 = Instant::now();
        let mut cfg = fixture_config();
        cfg.workload = BenchmarkWorkload::Showcase;
        let mut driver = BenchmarkDriver::new(cfg);
        let _ = driver.update(t0, true);
        let moving = driver.update(t0 + Duration::from_secs(51), true);
        assert!(!moving.forward && !moving.sprint && !moving.jump);
        assert!(moving.mouse_dx > 0.0);
        assert_eq!(driver.label(moving.segment), "showcase.moving");
    }

    #[test]
    fn heavyweight_runs_mutation_before_stationary_and_orbits_without_translation() {
        let t0 = Instant::now();
        let mut cfg = fixture_config();
        cfg.workload = BenchmarkWorkload::Heavyweight;
        cfg.mutation = Duration::from_secs(7);
        let mut driver = BenchmarkDriver::new(cfg);
        let _ = driver.update(t0, true);
        assert_eq!(
            driver.update(t0 + Duration::from_secs(20), true).segment,
            BenchmarkSegment::Mutation
        );
        assert_eq!(
            driver.label(BenchmarkSegment::Mutation),
            "heavyweight.mutation"
        );
        let moving = driver.update(t0 + Duration::from_secs(58), true);
        assert_eq!(moving.segment, BenchmarkSegment::Moving);
        assert!(!moving.forward && !moving.sprint && !moving.jump);
        assert!(moving.mouse_dx > 0.0);
    }

    #[test]
    fn heavyweight_stationary_camera_does_not_add_an_orbit() {
        let t0 = Instant::now();
        let mut cfg = fixture_config();
        cfg.workload = BenchmarkWorkload::Heavyweight;
        cfg.heavyweight = Some(crate::config::HeavyweightConfig {
            scenario: "palette".into(),
            seed: 1,
            scale: 1,
            camera_plan: "stationary".into(),
        });
        let mut driver = BenchmarkDriver::new(cfg);
        let _ = driver.update(t0, true);
        assert_eq!(
            driver.update(t0 + Duration::from_secs(51), true).mouse_dx,
            0.0
        );
    }

    #[test]
    fn terrain_warmup_emits_two_jump_press_edges_for_creative_flight() {
        let t0 = Instant::now();
        let mut driver = BenchmarkDriver::new(fixture_config());
        assert!(!driver.update(t0, true).jump);
        assert!(driver.update(t0 + Duration::from_millis(1_000), true).jump);
        assert!(!driver.update(t0 + Duration::from_millis(1_080), true).jump);
        assert!(driver.update(t0 + Duration::from_millis(1_160), true).jump);
        assert!(!driver.update(t0 + Duration::from_millis(1_240), true).jump);
    }

    #[test]
    fn megaworld_uses_flight_and_stable_segment_labels() {
        let t0 = Instant::now();
        let mut cfg = fixture_config();
        cfg.workload = BenchmarkWorkload::Megaworld;
        let mut driver = BenchmarkDriver::new(cfg);
        assert!(!driver.update(t0, true).jump);
        let moving = driver.update(t0 + Duration::from_secs(51), true);
        assert!(moving.forward && moving.sprint);
        assert_eq!(driver.label(moving.segment), "megaworld.moving");
    }

    #[test]
    fn large_world_movement_is_a_climbing_orbit_not_a_straight_walk() {
        let t0 = Instant::now();
        let mut cfg = fixture_config();
        cfg.workload = BenchmarkWorkload::Megaworld;
        let mut driver = BenchmarkDriver::new(cfg);
        let _ = driver.update(t0, true);

        let climbing = driver.update(t0 + Duration::from_secs(51), true);
        assert!(climbing.forward && climbing.sprint && climbing.jump);
        assert!(climbing.mouse_dx > 0.0);

        let orbiting = driver.update(t0 + Duration::from_secs(56), true);
        assert!(orbiting.forward && orbiting.sprint);
        assert!(!orbiting.jump);
        assert!(orbiting.mouse_dx > 0.0);
    }

    #[test]
    fn lovelier_uses_the_large_world_choreography_and_its_own_labels() {
        let t0 = Instant::now();
        let mut cfg = fixture_config();
        cfg.workload = BenchmarkWorkload::Lovelier;
        let mut driver = BenchmarkDriver::new(cfg);
        let _ = driver.update(t0, true);

        let moving = driver.update(t0 + Duration::from_secs(51), true);
        assert!(moving.forward && moving.mouse_dx > 0.0);
        assert_eq!(driver.label(moving.segment), "lovelier.moving");
    }
}
