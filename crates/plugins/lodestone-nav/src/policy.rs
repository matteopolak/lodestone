//! The knobs (`docs/baritone-port.md` §4.11).
//!
//! **Resist growing a hundreds-of-settings surface.** Every field here changes
//! observable behaviour; anything that tempts you beyond it — overlay colours, chat
//! prefixes, log verbosity — belongs to the plugin's UI and must not be able to
//! affect a computed path.
//!
//! # The rule that makes the line matter
//!
//! Anything feeding the cost model is captured **once** into the search request, not
//! read per edge. `NavPolicy` is `Copy` and is copied by value into `Search::new`,
//! immutable there. A mid-search policy change otherwise produces a path optimal for
//! neither the old policy nor the new one, which is worse than a suboptimal but
//! coherent path.

/// Everything that changes what the search considers or how it is executed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavPolicy {
    // --- capability gates: what the search may consider ---
    /// Whether the executor may hold sprint.
    ///
    /// **`false` in M1, deliberately.** Sprint reaches the server only as a
    /// `PlayerCommand::{Start,Stop}Sprinting` edge — the `SetPlayerInput` sprint bit
    /// is stored as `lastClientInput` and does *not* call `setSprinting` — and the
    /// sprint-through-a-descent overshoot rules are M2/M3. A sprinting M1 bot would
    /// move faster and be wrong in a way that looks like a physics bug.
    pub allow_sprint: bool,
    /// The greatest surface-height drop, in blocks, a `Drop` edge may land —
    /// **not** a cell count, a real `to_surface`-relative distance, because
    /// that is the unit vanilla's own damage rule uses.
    ///
    /// Default is vanilla's own `SAFE_FALL_DISTANCE` attribute default (`3.0`,
    /// `Attributes.java:87`), so a *default* policy never takes fall damage at
    /// all — `search::Search::edge_cost` still computes and charges
    /// [`Self::damage_cost`] for whatever this is raised to, per
    /// `docs/baritone-port.md` §4.4's "legality is separate" rule, but the
    /// zero-damage default is deliberate: relaxing it is a policy decision a
    /// caller makes with a concrete number in mind, not a silent default.
    pub max_fall_blocks: f64,

    // --- cost weights ---
    /// Additional cost per 90° of heading change, in ticks.
    ///
    /// The simulated templates *already* charge the real cost of a turn (`EntryRel`
    /// is part of the key), so this is a **preference on top of a measurement**, not
    /// a stand-in for one: it buys smoother-looking routes where two are equally
    /// fast. Raise for smoother paths, lower if the bot refuses reasonable detours.
    pub turn_penalty: f64,
    /// Additional cost of a `StepUp`, in ticks, on top of its simulated duration.
    ///
    /// `docs/baritone-port.md` §4.4: jumps are the least reliable movement class,
    /// so this is a *preference* toward walking around one where a walk-around
    /// exists, not a correction to a measurement. Set to `0` only after
    /// measuring the bot's own jump success rate.
    pub jump_penalty: f64,
    /// Ticks charged per half-heart of a `Drop`'s expected fall damage, from the
    /// real rule (`floor(delta + 1e-6 - SAFE_FALL_DISTANCE)`,
    /// `LivingEntity.java:1856`) — makes fall damage trade against time instead
    /// of being boolean, on top of [`Self::max_fall_blocks`]'s hard legality cap.
    pub damage_cost: f64,

    // --- search tuning ---
    /// `f = g + w·h`. `1.0` is exact and explores terrain that cannot help; the
    /// default `1.25` drives hard toward the goal, which is also what makes the
    /// *partial* good. The returned path costs at most `w ×` optimal.
    pub heuristic_weight: f64,
    /// Hard ceiling on expanded nodes for one search.
    pub search_budget_nodes: u32,
    /// Consecutive expansions at the snapshot edge before the search calls the world
    /// exhausted.
    pub edge_of_world_strikes: u32,
    /// Minimum straight-line blocks a partial plan must cover to be worth
    /// committing. Below it, report failure honestly: a two-block plan produces
    /// visible dithering and no progress, and returning *something* makes the user
    /// believe a route exists while the bot visibly does nothing.
    pub min_progress: f64,
    /// Minimum cost improvement, in raw 1/256 ticks, before a node is re-opened.
    /// Over flat ground, mixtures of edges produce improvements of a single 1/256
    /// tick; propagating those repropagates half the graph for nothing.
    pub min_improvement: u32,
    /// Fraction of a long, goal-missing plan's tail to discard. The far end of a
    /// weighted search is both the least-trustworthy part and the least-known world.
    pub tail_discard: f64,
    /// Plans shorter than this many edges are never truncated.
    pub tail_min_len: usize,
    /// Snapshot radius in chunk columns.
    pub snapshot_radius: i32,

    // --- execution tuning ---
    /// How far the body may stray from the nearest cell of the plan before an
    /// immediate abort, in blocks.
    pub drift_hard: f64,
    /// Softer drift threshold that starts the patience counter, in blocks.
    pub drift_soft: f64,
    /// Consecutive ticks beyond [`Self::drift_soft`] before aborting. Note the
    /// shape: the corridor is *wide* and it is the **patience counter, not the
    /// distance**, that catches most real failures.
    pub drift_patience: u32,
    /// Multiplier on an edge's planning-time cost before it counts as stalled.
    pub stall_multiplier: f64,
    /// Ticks added to the stall budget on top of the multiplier.
    pub stall_grace: u32,
    /// Consecutive stalls before the navigator gives up on the goal rather than
    /// replanning. **This is the loop-breaker**, and it must exist even when you
    /// believe the cost model is perfect.
    pub stall_patience: u32,
    /// Minimum ticks between replans, so a trigger that can fire every tick cannot
    /// make the bot oscillate and execute nothing.
    pub min_replan_interval_ticks: u32,
    /// Once a plan's remaining cost (excluding the currently executing edge —
    /// see [`crate::plan::Plan::remaining_cost_after`]'s own doc comment on why
    /// the exclusion matters) drops below this many ticks, dispatch the next
    /// segment's search now rather than waiting for the plan to run out.
    /// Default **30** (1.5 s), `docs/baritone-port.md` §4.9's own figure — this
    /// is what makes a journey longer than one snapshot possible without a
    /// visible stall at every segment boundary.
    pub replan_lead_ticks: u32,
    /// Whether the executor writes a `LookIntent` as well as the movement axes.
    ///
    /// With it off, the bot walks without turning — which still works, because the
    /// executor solves for `(forward, strafe)` from the world direction — but it
    /// looks wrong and sprint (M3) will not be possible, because the server only
    /// accepts sprinting in the direction you are facing.
    pub steer_yaw: bool,
}

impl Default for NavPolicy {
    fn default() -> Self {
        Self {
            allow_sprint: false,
            max_fall_blocks: crate::graph::SAFE_FALL_DISTANCE,
            turn_penalty: 1.0,
            jump_penalty: 2.0,
            damage_cost: 10.0,
            heuristic_weight: 1.25,
            search_budget_nodes: 20_000,
            edge_of_world_strikes: 50,
            min_progress: 5.0,
            min_improvement: 2,
            tail_discard: 0.15,
            tail_min_len: 20,
            snapshot_radius: 12,
            drift_hard: 3.0,
            drift_soft: 2.0,
            drift_patience: 200,
            stall_multiplier: 3.0,
            stall_grace: 10,
            stall_patience: 3,
            min_replan_interval_ticks: 10,
            replan_lead_ticks: 30,
            steer_yaw: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Copy-by-value is the mechanism that makes "captured once into the search
    /// request" true rather than aspirational. If `NavPolicy` ever gains a non-`Copy`
    /// field this fails to compile, which is the point.
    #[test]
    fn the_policy_is_copy_so_a_search_cannot_observe_a_mid_flight_change() {
        let mut policy = NavPolicy::default();
        let captured = policy;
        policy.heuristic_weight = 99.0;
        assert!((captured.heuristic_weight - 1.25).abs() < f64::EPSILON);
        assert!((policy.heuristic_weight - 99.0).abs() < f64::EPSILON);
    }

    #[test]
    fn m1_defaults_do_not_sprint() {
        assert!(!NavPolicy::default().allow_sprint);
    }
}
