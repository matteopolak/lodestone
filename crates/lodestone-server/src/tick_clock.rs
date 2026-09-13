//! Timing and accounting for the server's world tick.
//!
//! This module owns the data-only clock boundary used by the live tick loop.
//! It records completed ticks, coarse phase timings, owner-handoff counters,
//! and overload events without knowing how any simulation phase is executed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use super::MILLIS_PER_TICK;

/// Rolling window used for tick and phase percentile samples.
pub const TICK_HISTORY_LEN: usize = 100;

/// A coarse phase of the world tick, used for independent timing samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TickPhase {
    /// Mob, item, border, spawn, and block-entity work.
    MobsAndItems = 0,
    /// Weather and sleep-vote work.
    WeatherAndSleep = 1,
    /// Scheduled ticks and physics work.
    ScheduledAndPhysics = 2,
}

/// Number of phase variants. Kept as a crate-visible constant so the tick
/// module's tests can verify the name table and the enum remain aligned.
pub(crate) const TICK_PHASE_COUNT: usize = 3;

/// Phase labels in discriminant order for diagnostics.
pub(crate) const TICK_PHASE_NAMES: [&str; TICK_PHASE_COUNT] = [
    "mobs_and_items",
    "weather_and_sleep",
    "scheduled_and_physics",
];

/// A phase duration above this threshold contributes to the over-budget count.
pub(crate) const PHASE_SOFT_BUDGET: Duration = Duration::from_millis(MILLIS_PER_TICK / 5);

/// The largest phase duration observed by a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorstPhaseWindow {
    pub phase: TickPhase,
    pub micros: u64,
    pub tick_count: u64,
}

/// Percentile and count summary for one tick phase.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaseStats {
    pub phase: TickPhase,
    pub sample_count: u64,
    pub total_sample_count: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub over_budget_count: u64,
}

/// Cumulative work observed at chunk-owner handoff boundaries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OwnerTickStats {
    pub random_tick_owned_chunks: u64,
    pub thunder_owned_chunks: u64,
    pub scheduled_block_ticks: u64,
    pub scheduled_fluid_ticks: u64,
    pub block_entity_batches: u64,
    pub block_entity_effects: u64,
    pub spawner_batches: u64,
    pub spawner_attempts: u64,
    pub entity_effect_batches: u64,
    pub entity_effects: u64,
}

/// MSPT/TPS, phase, owner-work, and overrun accounting for one tick loop.
///
/// The loop writes through atomics and bounded mutex-protected histories while
/// readers can take snapshots concurrently.
#[derive(Debug)]
pub struct TickClock {
    tick_count: AtomicU64,
    last_mspt_micros: AtomicU64,
    overrun_count: AtomicU64,
    history: Mutex<VecDeque<u64>>,
    phase_history: [Mutex<VecDeque<u64>>; TICK_PHASE_COUNT],
    phase_sample_count: [AtomicU64; TICK_PHASE_COUNT],
    phase_over_budget: [AtomicU64; TICK_PHASE_COUNT],
    owner_stats: [AtomicU64; 10],
    worst_phase: Mutex<Option<WorstPhaseWindow>>,
}

impl Default for TickClock {
    fn default() -> Self {
        Self::new()
    }
}

impl TickClock {
    /// Creates an empty clock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tick_count: AtomicU64::new(0),
            last_mspt_micros: AtomicU64::new(0),
            overrun_count: AtomicU64::new(0),
            history: Mutex::new(VecDeque::with_capacity(TICK_HISTORY_LEN)),
            phase_history: std::array::from_fn(|_| Mutex::new(VecDeque::with_capacity(TICK_HISTORY_LEN))),
            phase_sample_count: [const { AtomicU64::new(0) }; TICK_PHASE_COUNT],
            phase_over_budget: [const { AtomicU64::new(0) }; TICK_PHASE_COUNT],
            owner_stats: [const { AtomicU64::new(0) }; 10],
            worst_phase: Mutex::new(None),
        }
    }

    /// Records one completed tick.
    pub(crate) fn record_tick(&self, elapsed: Duration) {
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.tick_count.fetch_add(1, Ordering::Relaxed);
        self.last_mspt_micros.store(micros, Ordering::Relaxed);
        let mut history = self.history.lock().expect("tick history lock poisoned");
        if history.len() == TICK_HISTORY_LEN {
            history.pop_front();
        }
        history.push_back(micros);
    }

    /// Records one rate-limited overload event.
    pub(crate) fn record_overrun(&self) {
        self.overrun_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Records one phase duration.
    pub(crate) fn record_phase(&self, phase: TickPhase, elapsed: Duration) {
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        let idx = phase as usize;
        self.phase_sample_count[idx].fetch_add(1, Ordering::Relaxed);
        {
            let mut history = self.phase_history[idx]
                .lock()
                .expect("tick phase history lock poisoned");
            if history.len() == TICK_HISTORY_LEN {
                history.pop_front();
            }
            history.push_back(micros);
        }
        if elapsed > PHASE_SOFT_BUDGET {
            self.phase_over_budget[idx].fetch_add(1, Ordering::Relaxed);
        }
        let mut worst = self.worst_phase.lock().expect("worst tick phase lock poisoned");
        if worst.is_none_or(|window| micros > window.micros) {
            *worst = Some(WorstPhaseWindow {
                phase,
                micros,
                tick_count: self.tick_count(),
            });
        }
    }

    /// Adds owner-boundary work to the cumulative counters.
    pub(crate) fn record_owner_work(&self, stats: OwnerTickStats) {
        for (counter, value) in self.owner_stats.iter().zip([
            stats.random_tick_owned_chunks,
            stats.thunder_owned_chunks,
            stats.scheduled_block_ticks,
            stats.scheduled_fluid_ticks,
            stats.block_entity_batches,
            stats.block_entity_effects,
            stats.spawner_batches,
            stats.spawner_attempts,
            stats.entity_effect_batches,
            stats.entity_effects,
        ]) {
            counter.fetch_add(value, Ordering::Relaxed);
        }
    }

    /// Returns cumulative owner-boundary work.
    #[must_use]
    pub fn owner_stats(&self) -> OwnerTickStats {
        let read = |index: usize| self.owner_stats[index].load(Ordering::Relaxed);
        OwnerTickStats {
            random_tick_owned_chunks: read(0),
            thunder_owned_chunks: read(1),
            scheduled_block_ticks: read(2),
            scheduled_fluid_ticks: read(3),
            block_entity_batches: read(4),
            block_entity_effects: read(5),
            spawner_batches: read(6),
            spawner_attempts: read(7),
            entity_effect_batches: read(8),
            entity_effects: read(9),
        }
    }

    /// Returns a percentile summary for one phase.
    #[must_use]
    pub fn phase_stats(&self, phase: TickPhase) -> PhaseStats {
        let idx = phase as usize;
        let mut samples: Vec<u64> = {
            let history = self.phase_history[idx]
                .lock()
                .expect("tick phase history lock poisoned");
            history.iter().copied().collect()
        };
        samples.sort_unstable();
        let sample_count = samples.len();
        let percentile = |p: f64| -> f64 {
            if sample_count == 0 {
                return 0.0;
            }
            let rank = ((p * sample_count as f64).ceil() as usize).clamp(1, sample_count) - 1;
            samples[rank] as f64 / 1000.0
        };
        PhaseStats {
            phase,
            sample_count: sample_count as u64,
            total_sample_count: self.phase_sample_count[idx].load(Ordering::Relaxed),
            p50_ms: percentile(0.50),
            p95_ms: percentile(0.95),
            p99_ms: percentile(0.99),
            max_ms: samples.last().copied().unwrap_or(0) as f64 / 1000.0,
            over_budget_count: self.phase_over_budget[idx].load(Ordering::Relaxed),
        }
    }

    /// Returns the largest phase window observed, if any.
    #[must_use]
    pub fn worst_phase_window(&self) -> Option<WorstPhaseWindow> {
        *self.worst_phase.lock().expect("worst tick phase lock poisoned")
    }

    /// Returns the number of completed ticks.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.tick_count.load(Ordering::Relaxed)
    }

    /// Returns the number of recorded overload events.
    #[must_use]
    pub fn overrun_count(&self) -> u64 {
        self.overrun_count.load(Ordering::Relaxed)
    }

    /// Returns a point-in-time snapshot of all clock accounting.
    #[must_use]
    pub fn stats(&self) -> TickStats {
        let history = self.history.lock().expect("tick history lock poisoned");
        let sample_count = history.len() as u64;
        let sum_micros: u64 = history.iter().sum();
        let mspt_avg_ms = if sample_count == 0 {
            0.0
        } else {
            (sum_micros as f64 / sample_count as f64) / 1000.0
        };
        let mspt_ms = self.last_mspt_micros.load(Ordering::Relaxed) as f64 / 1000.0;
        let tps = 1000.0 / mspt_avg_ms.max(MILLIS_PER_TICK as f64);
        TickStats {
            tick_count: self.tick_count(),
            mspt_ms,
            mspt_avg_ms,
            tps,
            overrun_count: self.overrun_count(),
            mobs_and_items: self.phase_stats(TickPhase::MobsAndItems),
            weather_and_sleep: self.phase_stats(TickPhase::WeatherAndSleep),
            scheduled_and_physics: self.phase_stats(TickPhase::ScheduledAndPhysics),
            owner_work: self.owner_stats(),
            worst_phase_window: self.worst_phase_window(),
        }
    }
}

/// Point-in-time snapshot of [`TickClock`] accounting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickStats {
    pub tick_count: u64,
    pub mspt_ms: f64,
    pub mspt_avg_ms: f64,
    pub tps: f64,
    pub overrun_count: u64,
    pub mobs_and_items: PhaseStats,
    pub weather_and_sleep: PhaseStats,
    pub scheduled_and_physics: PhaseStats,
    pub owner_work: OwnerTickStats,
    pub worst_phase_window: Option<WorstPhaseWindow>,
}
