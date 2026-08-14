//! The outbound action hook: inspect, replace or suppress a
//! [`ClientAction`] after one plugin queued it and before it reaches the wire.
//!
//! # What it is
//!
//! [`EgressFilters`] is a `Resource` holding callbacks the driver runs over
//! [`crate::ActionQueue`] at the moment it is drained — the seam this module
//! provides: "a `Message`/callback fired when `ActionQueue` is drained, before
//! each `ClientAction` reaches `NetClient::send_action`, with a `bool` a plugin
//! can set to suppress that one action". ProtocolLib's outbound side, at the one
//! layer where it is version-free.
//!
//! # Scope: suppression and *version-free* replacement, never encoded bytes
//!
//! Encoded-packet mutation must not be attempted without re-opening the
//! packet-interception-ABI design's version-leak concern, which is still open.
//! Nothing here goes near encoded bytes: the hook
//! sees [`ClientAction`], which is `lodestone_model`'s **version-free**
//! vocabulary. That is the whole reason [`Verdict::Replace`] is safe to offer —
//! a `ClientAction` contains no protocol id, no field order and no wire
//! encoding, so handing one back cannot leak a version into a shared crate. The
//! version-leak concern is about the *other* layer, inside a version crate's
//! `VersionAdapter`, and this hook cannot reach it.
//!
//! So: **replace and suppress, yes; mutate encoded packets, no.** The second
//! stays closed pending that design work.
//!
//! # The gap this hook has, measured rather than assumed
//!
//! [`crate::player::ActionQueue`] is documented as "the one sanctioned egress",
//! and for anything a *plugin* queues it is. It is **not**
//! the only path to the socket. Three of the six interaction verbs the
//! cancelable-action wrapper cares about bypass it entirely and call
//! `NetClient::send_action` directly, to control wire ordering for discrete
//! clicks:
//!
//! | verb | direct-send site |
//! |---|---|
//! | entity attack | `lodestone_shell::sim::actions::Sim::attack_entity` |
//! | right-click / use item | `Sim::interact_entity`, `Sim::use_item_generic`, `Sim::use_item_live` |
//! | container click | `lodestone_client::handle::ClientHandle::menu_click` |
//!
//! A filter therefore **cannot see an attack, a use-item or an inventory
//! click**. That is a real limit of the seam the issue specifies, not an
//! oversight in the implementation, and it is exactly the shape of thing that
//! rots into a false belief — so it is a *test*:
//! `crates/lodestone-ecs/tests/egress_hook_coverage.rs` scans the tree for
//! direct `send_action` sites and fails if the set changes, which is the only
//! way a newly-added bypass gets noticed.
//!
//! # Cost, as a count rather than a timing
//!
//! `CLAUDE.md`: prefer a counter over a duration, and two sequential durations
//! are not protected by being a ratio. So the cost claim here is a **count**,
//! and it is exact rather than approximate:
//!
//! - With no filter registered, [`EgressStats::invocations`] is **0** — no
//!   virtual dispatch, no allocation, no per-action work at all. [`apply`]
//!   returns after one `Vec::is_empty` check.
//! - With filters registered, `invocations` is exactly
//!   `actions × filters` (short-circuiting on the first non-`Allow`).
//!
//! Both are asserted as equalities in this module's tests. Note this hook is
//! *not* on the per-packet-per-player encode path the issue warns about — it
//! runs once per tick over the local client's own handful of queued actions —
//! but the zero-registration case is still made free, because a client nobody
//! wrote a plugin for should pay nothing.

use std::sync::atomic::{AtomicU64, Ordering};

use bevy_ecs::prelude::Resource;
use lodestone_model::ClientAction;

/// What a filter decided about one action.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Let it through unchanged, and let later filters see it.
    Allow,
    /// Drop it. Nothing reaches the wire, and no later filter is consulted.
    Suppress,
    /// Send this instead. No later filter is consulted, so a replacement cannot
    /// be re-replaced into a loop.
    ///
    /// Version-free by construction — see the module doc on why this does not
    /// touch the packet-interception-ABI version-leak concern.
    Replace(Box<ClientAction>),
}

/// One registered filter.
struct Filter {
    priority: i32,
    name: &'static str,
    f: Box<dyn Fn(&ClientAction) -> Verdict + Send + Sync + 'static>,
}

/// Counters for the hook. A count, not a duration — see the module doc.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EgressStats {
    /// How many times any filter callback was actually called. **0 when no
    /// filter is registered**, which is the cost claim.
    pub invocations: u64,
    /// Actions dropped by a [`Verdict::Suppress`].
    pub suppressed: u64,
    /// Actions swapped by a [`Verdict::Replace`].
    pub replaced: u64,
    /// Actions that reached the wire.
    pub passed: u64,
}

/// The outbound hook. `init_resource`'d by [`EgressFilterPlugin`], and consulted
/// by the driver's `ActionQueue` drain
/// (`lodestone_shell::sim::step::Sim::drain_action_queue`).
///
/// # Why a callback list rather than a `Message`
///
/// A `Message` was the first shape considered, and it cannot work here: the
/// drain happens *after* `GameTick` has finished, in the driver, so a plugin
/// system that read a `Message<AboutToSend>` would not run again until the next
/// tick — by which time the action has already gone. Suppression has to be
/// synchronous with the drain, and a callback is the only synchronous shape.
///
/// A plugin registers once, at `App`-build time, and the callback receives only
/// `&ClientAction` — **no `&World`**. That is deliberate and is the same
/// argument as [`crate::async_task`]'s: the drain runs while the driver holds
/// the `World` guard, so a callback handed a `World` would be one
/// `hold_read` away from the reentrant deadlock `handle.rs`'s rule 1 exists to
/// stop. A filter that needs world state captures an `Arc` its own system keeps
/// up to date, and [`crate::veto`]'s cancelable-action wrapper covers the cases
/// that genuinely need to consult the world.
#[derive(Resource, Default)]
pub struct EgressFilters {
    filters: Vec<Filter>,
    invocations: AtomicU64,
    suppressed: AtomicU64,
    replaced: AtomicU64,
    passed: AtomicU64,
}

impl std::fmt::Debug for EgressFilters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressFilters")
            .field("filters", &self.filters.iter().map(|f| f.name).collect::<Vec<_>>())
            .field("stats", &self.stats())
            .finish()
    }
}

impl EgressFilters {
    /// Register `f` under `name`, at `priority` (lower runs first).
    ///
    /// `name` is `&'static str` and shows up in `Debug` and in this type's own
    /// diagnostics — a filter that silently eats a plugin's actions is
    /// otherwise very hard to attribute, and "which filter dropped it" is the
    /// first question anyone will ask.
    ///
    /// Ties are broken by registration order (the sort is stable).
    pub fn register(
        &mut self,
        name: &'static str,
        priority: i32,
        f: impl Fn(&ClientAction) -> Verdict + Send + Sync + 'static,
    ) {
        self.filters.push(Filter {
            priority,
            name,
            f: Box::new(f),
        });
        self.filters.sort_by_key(|filter| filter.priority);
    }

    /// How many filters are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.filters.len()
    }

    /// Whether no filter is registered — the free path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// The registered filters' names, lowest priority first. For diagnostics.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.filters.iter().map(|f| f.name).collect()
    }

    /// Counters. See [`EgressStats`].
    #[must_use]
    pub fn stats(&self) -> EgressStats {
        EgressStats {
            invocations: self.invocations.load(Ordering::Relaxed),
            suppressed: self.suppressed.load(Ordering::Relaxed),
            replaced: self.replaced.load(Ordering::Relaxed),
            passed: self.passed.load(Ordering::Relaxed),
        }
    }

    /// Zero the counters, so a caller can measure one interval.
    pub fn reset_stats(&self) {
        self.invocations.store(0, Ordering::Relaxed);
        self.suppressed.store(0, Ordering::Relaxed);
        self.replaced.store(0, Ordering::Relaxed);
        self.passed.store(0, Ordering::Relaxed);
    }

    /// Run every filter over `actions`, in place, dropping suppressed ones and
    /// swapping replaced ones.
    ///
    /// **Returns immediately when no filter is registered**, having touched
    /// nothing and invoked nothing — the cost claim in the module doc. The
    /// `passed` counter is deliberately *not* bumped on that path: it counts
    /// what the hook let through, and a hook nobody installed did not let
    /// anything through, it simply was not there. Counting them would make
    /// `invocations == 0 && passed > 0` a reachable and confusing state.
    pub fn apply(&self, actions: &mut Vec<ClientAction>) {
        if self.filters.is_empty() {
            return;
        }
        let mut kept: Vec<ClientAction> = Vec::with_capacity(actions.len());
        for action in actions.drain(..) {
            let mut verdict = Verdict::Allow;
            for filter in &self.filters {
                self.invocations.fetch_add(1, Ordering::Relaxed);
                verdict = (filter.f)(&action);
                if verdict != Verdict::Allow {
                    // First non-Allow wins: a Suppress cannot be un-suppressed
                    // by a later filter, and a Replace cannot be re-replaced.
                    break;
                }
            }
            match verdict {
                Verdict::Allow => {
                    self.passed.fetch_add(1, Ordering::Relaxed);
                    kept.push(action);
                }
                Verdict::Suppress => {
                    self.suppressed.fetch_add(1, Ordering::Relaxed);
                }
                Verdict::Replace(replacement) => {
                    self.replaced.fetch_add(1, Ordering::Relaxed);
                    self.passed.fetch_add(1, Ordering::Relaxed);
                    kept.push(*replacement);
                }
            }
        }
        *actions = kept;
    }
}

/// Installs [`EgressFilters`].
///
/// Opt-in. The driver's drain tolerates the resource's absence (it does a
/// `get_resource`), so a client with no plugin pays one resource lookup per tick
/// and nothing else.
#[derive(Debug, Default)]
pub struct EgressFilterPlugin;

impl bevy_app::Plugin for EgressFilterPlugin {
    fn build(&self, app: &mut bevy_app::App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.init_resource::<EgressFilters>();
    }
}

#[cfg(test)]
mod tests {
    use lodestone_model::{ClientAction, Hand};

    use super::{EgressFilterPlugin, EgressFilters, Verdict};

    fn swing() -> ClientAction {
        ClientAction::SwingArm { hand: Hand::Main }
    }

    fn other() -> ClientAction {
        ClientAction::ReleaseUseItem
    }

    #[test]
    fn the_plugin_installs_the_resource() {
        let mut app = bevy_app::App::new();
        app.add_plugins(EgressFilterPlugin);
        assert!(app.world().get_resource::<EgressFilters>().is_some());
    }

    /// **The cost claim, as an exact count.** With no filter registered,
    /// `apply` invokes nothing over a realistic batch — no virtual dispatch, no
    /// allocation, no per-action work. An equality, not an inequality.
    #[test]
    fn with_no_filter_registered_nothing_is_invoked_at_all() {
        let filters = EgressFilters::default();
        let mut actions: Vec<ClientAction> = (0..64).map(|_| swing()).collect();
        for _ in 0..100 {
            filters.apply(&mut actions);
        }
        let stats = filters.stats();
        assert_eq!(
            (stats.invocations, stats.suppressed, stats.replaced, stats.passed),
            (0, 0, 0, 0),
            "6400 action-visits must cost exactly zero filter invocations"
        );
        assert_eq!(actions.len(), 64, "and must not touch the queue");
    }

    /// **The control for the cost claim.** One registered filter over the same
    /// batch invokes exactly `actions × filters`. Without this, the zero above
    /// could be an `apply` that never invokes anything under any circumstances.
    #[test]
    fn with_filters_registered_the_invocation_count_is_exactly_actions_times_filters() {
        let mut filters = EgressFilters::default();
        filters.register("a", 0, |_| Verdict::Allow);
        filters.register("b", 0, |_| Verdict::Allow);
        filters.register("c", 0, |_| Verdict::Allow);

        let mut actions: Vec<ClientAction> = (0..10).map(|_| swing()).collect();
        filters.apply(&mut actions);

        let stats = filters.stats();
        assert_eq!(
            stats.invocations, 30,
            "10 actions x 3 filters, all Allow, is exactly 30 invocations"
        );
        assert_eq!(stats.passed, 10);
        assert_eq!(actions.len(), 10);
    }

    /// A `Suppress` short-circuits: later filters are not consulted, so the
    /// count is lower than `actions × filters`. Predicting the exact number is
    /// what distinguishes a real short-circuit from a `continue`.
    #[test]
    fn a_suppress_short_circuits_the_remaining_filters() {
        let mut filters = EgressFilters::default();
        filters.register("first", 0, |_| Verdict::Suppress);
        filters.register("never", 1, |_| panic!("must not be consulted"));

        let mut actions = vec![swing(), swing(), swing()];
        filters.apply(&mut actions);

        assert!(actions.is_empty(), "all three suppressed");
        let stats = filters.stats();
        assert_eq!(
            stats.invocations, 3,
            "3 actions x 1 filter, because the second is never reached"
        );
        assert_eq!(stats.suppressed, 3);
        assert_eq!(stats.passed, 0);
    }

    /// Suppression is selective: only the matched action goes, and the others
    /// survive **in order**. A filter that dropped everything would pass a
    /// "the suppressed one is gone" check.
    #[test]
    fn suppression_drops_only_the_matching_action_and_preserves_order() {
        let mut filters = EgressFilters::default();
        filters.register("no-swings", 0, |a| {
            if matches!(a, ClientAction::SwingArm { .. }) {
                Verdict::Suppress
            } else {
                Verdict::Allow
            }
        });

        let mut actions = vec![other(), swing(), other(), swing(), other()];
        filters.apply(&mut actions);

        assert_eq!(actions, vec![other(), other(), other()]);
        let stats = filters.stats();
        assert_eq!(stats.suppressed, 2);
        assert_eq!(stats.passed, 3);
    }

    /// Replacement swaps the action rather than dropping it — the length is
    /// unchanged and the contents are not.
    #[test]
    fn a_replace_swaps_the_action_and_keeps_the_slot() {
        let mut filters = EgressFilters::default();
        filters.register("swings-become-releases", 0, |a| {
            if matches!(a, ClientAction::SwingArm { .. }) {
                Verdict::Replace(Box::new(ClientAction::ReleaseUseItem))
            } else {
                Verdict::Allow
            }
        });

        let mut actions = vec![swing(), other()];
        filters.apply(&mut actions);

        assert_eq!(actions, vec![other(), other()]);
        let stats = filters.stats();
        assert_eq!(stats.replaced, 1);
        assert_eq!(stats.passed, 2, "a replacement still reaches the wire");
        assert_eq!(stats.suppressed, 0);
    }

    /// A replacement is not re-offered to later filters, so two filters that
    /// each rewrite the other's output cannot loop.
    #[test]
    fn a_replacement_is_not_itself_re_replaced() {
        let mut filters = EgressFilters::default();
        filters.register("a-to-b", 0, |a| {
            if matches!(a, ClientAction::SwingArm { .. }) {
                Verdict::Replace(Box::new(ClientAction::ReleaseUseItem))
            } else {
                Verdict::Allow
            }
        });
        filters.register("b-to-a", 1, |a| {
            if matches!(a, ClientAction::ReleaseUseItem) {
                Verdict::Replace(Box::new(ClientAction::SwingArm { hand: Hand::Main }))
            } else {
                Verdict::Allow
            }
        });

        let mut actions = vec![swing()];
        filters.apply(&mut actions);
        assert_eq!(
            actions,
            vec![other()],
            "the first filter's replacement is final for this action"
        );
    }

    /// Priority decides order, and the observable consequence is *which*
    /// filter's verdict wins. Registration order is deliberately not the
    /// answer — this registers the high-priority one second.
    #[test]
    fn a_lower_priority_number_runs_first_and_its_verdict_wins() {
        let mut filters = EgressFilters::default();
        filters.register("late", 10, |_| {
            Verdict::Replace(Box::new(ClientAction::ReleaseUseItem))
        });
        filters.register("early", -10, |_| Verdict::Suppress);

        assert_eq!(filters.names(), vec!["early", "late"]);
        let mut actions = vec![swing()];
        filters.apply(&mut actions);
        assert!(
            actions.is_empty(),
            "the -10 filter suppressed it before the +10 one could replace it"
        );
    }

    /// The mirror of the test above: swap only the priorities and the outcome
    /// flips. Two runs differing in one number, which is what makes the
    /// ordering claim a measurement rather than a restatement of the sort call.
    #[test]
    fn swapping_the_priorities_flips_which_verdict_wins() {
        let mut filters = EgressFilters::default();
        filters.register("late", -10, |_| {
            Verdict::Replace(Box::new(ClientAction::ReleaseUseItem))
        });
        filters.register("early", 10, |_| Verdict::Suppress);

        let mut actions = vec![swing()];
        filters.apply(&mut actions);
        assert_eq!(
            actions,
            vec![other()],
            "with the priorities swapped the replacement wins instead"
        );
    }

    /// Ties keep registration order, so two plugins at the default priority get
    /// a defined (if arbitrary) order rather than a sort-dependent one.
    #[test]
    fn equal_priorities_keep_registration_order() {
        let mut filters = EgressFilters::default();
        for name in ["one", "two", "three"] {
            filters.register(name, 0, |_| Verdict::Allow);
        }
        assert_eq!(filters.names(), vec!["one", "two", "three"]);
    }

    /// `reset_stats` really zeroes, so a caller can measure one interval rather
    /// than the process's whole history — the accumulation question
    /// `CLAUDE.md` asks of any counter-based gate.
    #[test]
    fn stats_can_be_reset_to_measure_one_interval() {
        let mut filters = EgressFilters::default();
        filters.register("a", 0, |_| Verdict::Allow);
        let mut actions = vec![swing()];
        filters.apply(&mut actions);
        assert_eq!(filters.stats().invocations, 1);
        filters.reset_stats();
        assert_eq!(filters.stats(), super::EgressStats::default());
    }
}
