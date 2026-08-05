//! Issue #109 — the cancelable action wrapper for the core interaction verbs.
//!
//! # What it is
//!
//! [`ActionVetoes`] is the veto point the six verbs every protection,
//! anti-grief and anti-cheat plugin actually cancels have been missing:
//! block-break, block-place, entity-damage, inventory-click, player-move and
//! player-interact. A plugin registers a predicate per verb; the engine asks
//! before it commits, and a `Deny` stops the action *before* the predictor runs
//! and before anything reaches the wire.
//!
//! This is what separates "plugins can read state" from "plugins can be a
//! protection plugin".
//!
//! # Why a synchronous predicate and not an event
//!
//! Issue #109 names the hard constraint itself: *"a plugin system that cancels
//! must not need to re-enter the World to do so"*. Both obvious designs fail it:
//!
//! - **A `Message` a plugin reads and answers.** The commitment happens inside
//!   one system (or one `Sim` method), so a plugin system that read a message
//!   would not run until the *next* tick — after the block is already broken.
//! - **A predicate handed `&World`.** Three of the six verbs commit from plain
//!   `impl Sim` methods that reach the `World` through `self.read`/`self.write`
//!   (see the coverage table below). A predicate given a `&World` there would be
//!   called *inside* a guard, and any `hold_read` in it is `handle.rs`'s rule 1
//!   — the `accb993` hang, with no panic and no log line.
//!
//! So a [`VetoFn`] takes **only the verb's own context** and returns
//! [`Verdict`]. It cannot re-enter the `World` because it is handed no way to.
//! A plugin needing world state to decide keeps it in an `Arc` its own system
//! refreshes each tick — which is a real constraint, written down in
//! `docs/cancelable-actions.md` rather than discovered.
//!
//! # Reading the decision, and the "and no more" problem
//!
//! Vetoes are consulted through [`ActionVetoes::allows`], which takes
//! `&self` — so a call site holding `&World` (a system with `Res<ActionVetoes>`)
//! and a call site holding a guard (`Sim::read`) can both use it unchanged.
//!
//! # Coverage today, stated exactly
//!
//! `CLAUDE.md`'s island rule: a mechanism nothing calls is a defect report, not
//! a status update. So here is what actually calls it, verb by verb, and what
//! does not:
//!
//! | verb | commitment point | wired? |
//! |---|---|---|
//! | block break | `lodestone_shell::interact::drive_mining` (a system) | **yes** |
//! | block place | `lodestone_shell::interact::drive_placement` (a system) | **yes** |
//! | entity damage | `lodestone_shell::sim::actions::Sim::attack_entity` | **yes** |
//! | player move | `lodestone_controller::ecs::send_player_input` (a system) | **yes** |
//! | inventory click | `lodestone_client::handle::ClientHandle::menu_click` | *see doc* |
//! | player interact | `Sim::use_item_live` / `interact_entity` / `use_item_generic` | *see doc* |
//!
//! `crates/lodestone-ecs/tests/veto_coverage.rs` asserts which verbs have a live
//! call site by scanning for it, so a verb silently losing its wiring fails a
//! test rather than becoming a plugin author's bug report.

use std::sync::atomic::{AtomicU64, Ordering};

use bevy_ecs::prelude::Resource;
use lodestone_model::BlockPos;

/// A veto decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Let it happen.
    Allow,
    /// Stop it. The engine must not commit the action, run its predictor, or
    /// send anything.
    Deny,
}

impl Verdict {
    /// `true` if this is [`Verdict::Allow`].
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }

    /// `Deny` if `deny` is true. Convenience for the common
    /// `if protected { Deny } else { Allow }` predicate body.
    #[must_use]
    pub const fn deny_if(deny: bool) -> Self {
        if deny { Self::Deny } else { Self::Allow }
    }
}

/// Which verb a veto applies to. One flat enum rather than six resources, so a
/// plugin registers through one call and the engine's ask sites are uniform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Verb {
    /// Breaking a block. Bukkit's `BlockBreakEvent`.
    BlockBreak,
    /// Placing a block or using an item on one. Bukkit's `BlockPlaceEvent`.
    BlockPlace,
    /// Attacking an entity. Bukkit's `EntityDamageByEntityEvent`.
    EntityDamage,
    /// Clicking a slot in an open container. Bukkit's `InventoryClickEvent`.
    InventoryClick,
    /// The local player's movement input for this tick. Bukkit's
    /// `PlayerMoveEvent`.
    PlayerMove,
    /// Right-clicking a block, an entity, or the air. Bukkit's
    /// `PlayerInteractEvent`.
    PlayerInteract,
}

impl Verb {
    /// Every verb, for iteration in tests and diagnostics. Kept beside the enum
    /// so adding a variant and forgetting this is one place to look, not six.
    pub const ALL: &'static [Self] = &[
        Self::BlockBreak,
        Self::BlockPlace,
        Self::EntityDamage,
        Self::InventoryClick,
        Self::PlayerMove,
        Self::PlayerInteract,
    ];
}

/// What the engine tells a predicate about the action it is about to take.
///
/// Deliberately small and `Copy`: a context is built at every ask site, on the
/// hot path for [`Verb::PlayerMove`] (once per tick), so it must not allocate.
/// Anything a plugin needs beyond this it reads from its own `Arc` — see the
/// module doc on why no `&World` is handed over.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum VerbContext {
    /// [`Verb::BlockBreak`]: the block about to be dug, and its state id.
    BlockBreak {
        /// The block position.
        pos: BlockPos,
        /// The block-state id currently there, as the version adapter reports
        /// it — `None` when the client has no live state for that position.
        state_id: Option<u32>,
    },
    /// [`Verb::BlockPlace`]: the position that would be *changed*.
    BlockPlace {
        /// The block being clicked.
        pos: BlockPos,
    },
    /// [`Verb::EntityDamage`]: who is about to be hit.
    EntityDamage {
        /// The target's protocol entity id.
        target_entity_id: i32,
    },
    /// [`Verb::InventoryClick`]: which slot in which window.
    InventoryClick {
        /// The container's window id (0 is the player's own inventory).
        window_id: i32,
        /// The clicked slot index.
        slot: i32,
        /// The mouse button / key.
        button: i32,
    },
    /// [`Verb::PlayerMove`]: this tick's movement input, reduced to the bits a
    /// protection plugin cares about.
    PlayerMove {
        /// Whether any translation was requested this tick.
        moving: bool,
        /// Whether a jump was requested.
        jumping: bool,
        /// Whether sprint was requested.
        sprinting: bool,
    },
    /// [`Verb::PlayerInteract`]: right-click, on a block, an entity, or air.
    PlayerInteract {
        /// The block clicked, if any.
        pos: Option<BlockPos>,
        /// The entity clicked, if any.
        target_entity_id: Option<i32>,
    },
}

impl VerbContext {
    /// Which [`Verb`] this context belongs to. Kept as a method so an ask site
    /// cannot pass a `BlockBreak` context under the `BlockPlace` verb.
    #[must_use]
    pub const fn verb(&self) -> Verb {
        match self {
            Self::BlockBreak { .. } => Verb::BlockBreak,
            Self::BlockPlace { .. } => Verb::BlockPlace,
            Self::EntityDamage { .. } => Verb::EntityDamage,
            Self::InventoryClick { .. } => Verb::InventoryClick,
            Self::PlayerMove { .. } => Verb::PlayerMove,
            Self::PlayerInteract { .. } => Verb::PlayerInteract,
        }
    }
}

/// A registered veto predicate. Takes only [`VerbContext`] — no `&World`, by
/// design; see the module doc.
type VetoFn = Box<dyn Fn(&VerbContext) -> Verdict + Send + Sync + 'static>;

struct Registered {
    verb: Verb,
    priority: i32,
    name: &'static str,
    f: VetoFn,
}

/// Counters. A count, not a duration, per `CLAUDE.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VetoStats {
    /// How many times any predicate was called. **0 with none registered.**
    pub invocations: u64,
    /// Verbs asked about.
    pub asked: u64,
    /// Asks that came back `Deny`.
    pub denied: u64,
}

/// The veto registry. `init_resource`'d by [`ActionVetoPlugin`].
#[derive(Resource, Default)]
pub struct ActionVetoes {
    vetoes: Vec<Registered>,
    /// Bitset of verbs with at least one registered predicate, so [`Self::allows`]
    /// short-circuits without scanning the list. `PlayerMove` is asked once per
    /// tick and the common case is an empty registry.
    armed: u64,
    invocations: AtomicU64,
    asked: AtomicU64,
    denied: AtomicU64,
}

impl std::fmt::Debug for ActionVetoes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionVetoes")
            .field(
                "registered",
                &self
                    .vetoes
                    .iter()
                    .map(|v| (v.verb, v.name))
                    .collect::<Vec<_>>(),
            )
            .field("stats", &self.stats())
            .finish()
    }
}

const fn verb_bit(verb: Verb) -> u64 {
    1u64 << (verb as u32)
}

impl ActionVetoes {
    /// Register `f` as a veto on `verb`, under `name`, at `priority` (lower runs
    /// first; ties keep registration order).
    ///
    /// `name` is `&'static str` so "which plugin cancelled my block break" is
    /// answerable — the first question anyone asks of a protection plugin.
    pub fn register(
        &mut self,
        verb: Verb,
        name: &'static str,
        priority: i32,
        f: impl Fn(&VerbContext) -> Verdict + Send + Sync + 'static,
    ) {
        self.armed |= verb_bit(verb);
        self.vetoes.push(Registered {
            verb,
            priority,
            name,
            f: Box::new(f),
        });
        self.vetoes.sort_by_key(|v| v.priority);
    }

    /// Whether any predicate is registered for `verb`. One bit test — this is
    /// what makes the unregistered case free.
    #[must_use]
    pub const fn is_armed(&self, verb: Verb) -> bool {
        self.armed & verb_bit(verb) != 0
    }

    /// Ask every predicate registered for `ctx`'s verb, in priority order.
    /// Returns [`Verdict::Deny`] on the first denial.
    ///
    /// **Returns `Allow` after one bit test when nothing is registered for that
    /// verb** — no iteration, no dispatch, no counter writes. `asked` is
    /// therefore *not* bumped on that path: it counts asks the registry actually
    /// considered, so `invocations == 0 && asked > 0` cannot happen.
    #[must_use]
    pub fn allows(&self, ctx: &VerbContext) -> Verdict {
        let verb = ctx.verb();
        if !self.is_armed(verb) {
            return Verdict::Allow;
        }
        self.asked.fetch_add(1, Ordering::Relaxed);
        for registered in self.vetoes.iter().filter(|v| v.verb == verb) {
            self.invocations.fetch_add(1, Ordering::Relaxed);
            if (registered.f)(ctx) == Verdict::Deny {
                self.denied.fetch_add(1, Ordering::Relaxed);
                return Verdict::Deny;
            }
        }
        Verdict::Allow
    }

    /// The names registered for `verb`, in the order they will be consulted.
    #[must_use]
    pub fn names(&self, verb: Verb) -> Vec<&'static str> {
        self.vetoes
            .iter()
            .filter(|v| v.verb == verb)
            .map(|v| v.name)
            .collect()
    }

    /// Counters. See [`VetoStats`].
    #[must_use]
    pub fn stats(&self) -> VetoStats {
        VetoStats {
            invocations: self.invocations.load(Ordering::Relaxed),
            asked: self.asked.load(Ordering::Relaxed),
            denied: self.denied.load(Ordering::Relaxed),
        }
    }

    /// Zero the counters, so a caller can measure one interval.
    pub fn reset_stats(&self) {
        self.invocations.store(0, Ordering::Relaxed);
        self.asked.store(0, Ordering::Relaxed);
        self.denied.store(0, Ordering::Relaxed);
    }
}

/// Installs [`ActionVetoes`].
///
/// Opt-in. Every ask site uses `get_resource`, so a client with no plugin never
/// has the resource and every verb is allowed by a `None` check.
#[derive(Debug, Default)]
pub struct ActionVetoPlugin;

impl bevy_app::Plugin for ActionVetoPlugin {
    fn build(&self, app: &mut bevy_app::App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.init_resource::<ActionVetoes>();
    }
}

#[cfg(test)]
mod tests {
    use lodestone_model::BlockPos;

    use super::{ActionVetoPlugin, ActionVetoes, Verb, VerbContext, Verdict};

    fn break_at(x: i32) -> VerbContext {
        VerbContext::BlockBreak {
            pos: BlockPos::new(x, 64, 0),
            state_id: Some(1),
        }
    }

    #[test]
    fn the_plugin_installs_the_resource() {
        let mut app = bevy_app::App::new();
        app.add_plugins(ActionVetoPlugin);
        assert!(app.world().get_resource::<ActionVetoes>().is_some());
    }

    /// Every verb has a distinct bit, so arming one cannot arm another. Checked
    /// across all six rather than sampled — a shift computed from a discriminant
    /// is exactly the kind of thing that silently collides.
    #[test]
    fn arming_one_verb_arms_only_that_verb() {
        for verb in Verb::ALL {
            let mut vetoes = ActionVetoes::default();
            vetoes.register(*verb, "probe", 0, |_| Verdict::Deny);
            for other in Verb::ALL {
                assert_eq!(
                    vetoes.is_armed(*other),
                    other == verb,
                    "registering {verb:?} must arm {verb:?} and nothing else, but \
                     is_armed({other:?}) disagreed"
                );
            }
        }
    }

    /// Every context reports the verb it belongs to — the mapping an ask site
    /// relies on to not consult the wrong verb's predicates.
    #[test]
    fn every_context_maps_to_its_own_verb() {
        let contexts = [
            (
                VerbContext::BlockBreak { pos: BlockPos::new(0, 0, 0), state_id: None },
                Verb::BlockBreak,
            ),
            (VerbContext::BlockPlace { pos: BlockPos::new(0, 0, 0) }, Verb::BlockPlace),
            (VerbContext::EntityDamage { target_entity_id: 1 }, Verb::EntityDamage),
            (
                VerbContext::InventoryClick { window_id: 0, slot: 0, button: 0 },
                Verb::InventoryClick,
            ),
            (
                VerbContext::PlayerMove { moving: true, jumping: false, sprinting: false },
                Verb::PlayerMove,
            ),
            (
                VerbContext::PlayerInteract { pos: None, target_entity_id: None },
                Verb::PlayerInteract,
            ),
        ];
        assert_eq!(
            contexts.len(),
            Verb::ALL.len(),
            "every verb needs a context in this table, or a verb is untested"
        );
        for (ctx, verb) in contexts {
            assert_eq!(ctx.verb(), verb);
        }
    }

    /// **The cost claim, as an exact count.** With nothing registered, asking
    /// 10 000 times invokes nothing and records nothing.
    #[test]
    fn with_nothing_registered_asking_costs_exactly_zero() {
        let vetoes = ActionVetoes::default();
        for i in 0..10_000 {
            assert_eq!(vetoes.allows(&break_at(i)), Verdict::Allow);
        }
        assert_eq!(vetoes.stats(), super::VetoStats::default());
    }

    /// **The control for the cost claim.** One registered predicate over the
    /// same loop invokes exactly once per ask — so the zero above is the
    /// short-circuit, not an `allows` that never invokes anything.
    #[test]
    fn with_one_registered_the_invocation_count_is_exactly_the_ask_count() {
        let mut vetoes = ActionVetoes::default();
        vetoes.register(Verb::BlockBreak, "always-allow", 0, |_| Verdict::Allow);
        for i in 0..50 {
            assert_eq!(vetoes.allows(&break_at(i)), Verdict::Allow);
        }
        let stats = vetoes.stats();
        assert_eq!((stats.asked, stats.invocations, stats.denied), (50, 50, 0));
    }

    /// A veto on one verb does not affect another — the property that makes six
    /// verbs safe to share one registry.
    #[test]
    fn a_veto_on_one_verb_does_not_deny_another() {
        let mut vetoes = ActionVetoes::default();
        vetoes.register(Verb::BlockBreak, "no-breaking", 0, |_| Verdict::Deny);

        assert_eq!(vetoes.allows(&break_at(0)), Verdict::Deny);
        assert_eq!(
            vetoes.allows(&VerbContext::BlockPlace { pos: BlockPos::new(0, 0, 0) }),
            Verdict::Allow,
            "placing must be unaffected by a break veto"
        );
        assert_eq!(
            vetoes.allows(&VerbContext::EntityDamage { target_entity_id: 7 }),
            Verdict::Allow
        );
    }

    /// A predicate that looks at the context: only the protected region is
    /// denied. A predicate denying everything would pass a "the protected block
    /// was denied" check, so the unprotected case is asserted too.
    #[test]
    fn a_region_veto_denies_inside_and_allows_outside() {
        let mut vetoes = ActionVetoes::default();
        vetoes.register(Verb::BlockBreak, "spawn-protection", 0, |ctx| {
            let VerbContext::BlockBreak { pos, .. } = ctx else {
                return Verdict::Allow;
            };
            Verdict::deny_if(pos.x.abs() <= 16 && pos.z.abs() <= 16)
        });

        assert_eq!(vetoes.allows(&break_at(0)), Verdict::Deny, "inside spawn");
        assert_eq!(vetoes.allows(&break_at(16)), Verdict::Deny, "on the boundary");
        assert_eq!(vetoes.allows(&break_at(17)), Verdict::Allow, "outside spawn");
        let stats = vetoes.stats();
        assert_eq!((stats.asked, stats.denied), (3, 2));
    }

    /// The first `Deny` short-circuits, so a later predicate cannot un-deny —
    /// and is not even consulted. Asserted by a predicate that panics if run.
    #[test]
    fn the_first_denial_short_circuits_and_cannot_be_overridden() {
        let mut vetoes = ActionVetoes::default();
        vetoes.register(Verb::BlockBreak, "deny", 0, |_| Verdict::Deny);
        vetoes.register(Verb::BlockBreak, "never", 1, |_| {
            panic!("must not be consulted after a Deny")
        });

        assert_eq!(vetoes.allows(&break_at(0)), Verdict::Deny);
        assert_eq!(vetoes.stats().invocations, 1);
    }

    /// Priority decides consultation order, and registration order does not.
    /// Registered high-first to make that observable.
    #[test]
    fn lower_priority_runs_first() {
        let mut vetoes = ActionVetoes::default();
        vetoes.register(Verb::BlockBreak, "late", 10, |_| Verdict::Allow);
        vetoes.register(Verb::BlockBreak, "early", -10, |_| Verdict::Allow);
        assert_eq!(vetoes.names(Verb::BlockBreak), vec!["early", "late"]);
    }

    /// All predicates run when none denies, so `Allow` really means "everyone
    /// agreed" rather than "the first one agreed".
    #[test]
    fn every_predicate_is_consulted_when_none_denies() {
        let mut vetoes = ActionVetoes::default();
        for name in ["a", "b", "c"] {
            vetoes.register(Verb::BlockBreak, name, 0, |_| Verdict::Allow);
        }
        assert_eq!(vetoes.allows(&break_at(0)), Verdict::Allow);
        assert_eq!(vetoes.stats().invocations, 3);
    }

    /// `allows` takes `&self`, which is what lets a call site holding a `&World`
    /// (a system's `Res<ActionVetoes>`) and one holding a guard
    /// (`Sim::read(|w| ...)`) share the same entry point. A compile-time fact,
    /// pinned so a later `&mut self` refactor fails here rather than at four
    /// unrelated call sites.
    #[test]
    fn allows_needs_only_a_shared_borrow() {
        let vetoes = ActionVetoes::default();
        let shared: &ActionVetoes = &vetoes;
        assert_eq!(shared.allows(&break_at(0)), Verdict::Allow);
    }

    /// Reached through the `World` the way an ask site does it, rather than only
    /// as a bare struct — the *world*-species check that the resource is usable
    /// through the registry and not just as a value.
    #[test]
    fn a_veto_registered_through_the_app_is_readable_from_the_world() {
        let mut app = bevy_app::App::new();
        app.add_plugins(ActionVetoPlugin);
        app.world_mut()
            .resource_mut::<ActionVetoes>()
            .register(Verb::EntityDamage, "peaceful", 0, |_| Verdict::Deny);

        let vetoes = app.world().resource::<ActionVetoes>();
        assert_eq!(
            vetoes.allows(&VerbContext::EntityDamage { target_entity_id: 3 }),
            Verdict::Deny
        );
        assert_eq!(vetoes.allows(&break_at(0)), Verdict::Allow);
    }

    #[test]
    fn stats_can_be_reset() {
        let mut vetoes = ActionVetoes::default();
        vetoes.register(Verb::PlayerMove, "a", 0, |_| Verdict::Allow);
        let _ = vetoes.allows(&VerbContext::PlayerMove {
            moving: true,
            jumping: false,
            sprinting: false,
        });
        assert_eq!(vetoes.stats().asked, 1);
        vetoes.reset_stats();
        assert_eq!(vetoes.stats(), super::VetoStats::default());
    }
}
