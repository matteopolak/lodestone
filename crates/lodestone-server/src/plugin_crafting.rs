//! Plugin-facing crafting-station result hooks: anvil,
//! grindstone, smithing table, loom and stonecutter.
//!
//! # What it is
//!
//! The server-side mirror of Bukkit's `PrepareAnvilEvent`/`PrepareSmithingEvent`/
//! `PrepareItemCraftEvent`: a plugin observes a station's own computed result
//! before it reaches a player's screen and answers with one of three verdicts
//! — allow it unchanged, deny it outright, or replace it with a stack of its
//! own. That is the basis of a custom-repair-cost plugin, a custom trim/dye
//! rule, or an anvil-combine override, none of which exist anywhere in this
//! crate today.
//!
//! # How it works
//!
//! [`CraftingStationHooks`] is a `Clone`-able, `Arc`-backed registry — the
//! same shape [`crate::plugin_channels::PluginChannelRegistry`] already
//! established for wire-level plugin messaging, and the same "cheap clone,
//! one store" shape [`crate::world_state::WorldStateHandle`]'s own siblings
//! (`scoreboard`, `teams`, `nbt_storage`, …) use. It rides `WorldStateHandle`
//! as a new sibling field for the identical reason those do: `WorldStateHandle`
//! is already threaded to `crate::server::dispatch_play_packet`, which is
//! where every one of these packets is handled, so riding here reaches every
//! production call site with no new parameter added to the `serve_connection*`
//! wrappers themselves — only to the handful of leaf functions that actually
//! compute a station's result, each of which already receives a narrow,
//! purpose-built parameter rather than the whole handle (see
//! `crate::server::apply_use_item_on`'s own `difficulty` parameter comment for
//! why: "the only scalar this function needs", not a handle that "would
//! invite a second, unrelated read").
//!
//! [`crate::server::workstation_result`] is the single choke point every one
//! of the five stations' result slot passes through — reached both when the
//! menu is (re)read (drawing/refreshing the result slot: opening the
//! station, placing or removing an ingredient, renaming, picking a loom/
//! stonecutter offer) and when a click actually takes the result (charging
//! XP, clearing the input cells) — so hooking there, rather than each
//! station's own `compute`/`result` function in `crate::anvil`/
//! `crate::smithing`/`crate::loom`/`crate::stonecutting`, covers every one of
//! those paths with one call site and cannot itself become an island: every
//! caller of `workstation_result` already exists in production and none of
//! them changed shape, only gained one more parameter.
//!
//! Verdicts deliberately mirror `docs/packet-wiring.md`'s `EgressFilters`/
//! `ActionVetoes` vocabulary (`Allow`/a refusal/`Replace`) rather than
//! inventing a fourth veto shape for this repo: **the first non-`Allow`
//! verdict wins**, in ascending priority order, so two hooks cannot loop
//! rewriting each other's output, and a hook is asked with a typed
//! [`StationInputs`] — the station and its own input cells, [`plugin-api.md`]'s
//! "observation vocabulary, never wire vocabulary" clause — never a menu
//! index or a raw click. Unlike `ActionVetoes`, a station evaluation has no
//! "human outranks a plugin" question to answer (there is no second, human
//! source of a workstation result to arbitrate against) and no lifecycle
//! beyond "answer this one evaluation", so those two clauses of the intent
//! doctrine do not apply here — the three that do (observation vocabulary,
//! one owner, an always-observable, typed answer) are the ones this module
//! keeps.
//!
//! **Cost is untouched.** Vanilla's own `PrepareAnvilEvent` only ever lets a
//! plugin replace the *result* `ItemStack`, never the anvil's XP-level cost —
//! `AnvilMenu`'s `cost` `DataSlot` is computed once, from the pre-click cells
//! alone, by `crate::server::apply_workstation_clicked`'s own `anvil_cost`
//! binding, entirely separately from [`workstation_result`]. This module
//! follows that: a hook that replaces the result does not, and cannot, change
//! what the take costs.
//!
//! [`plugin-api.md`]: ../../../docs/plugin-api.md
//!
//! # How to change it
//!
//! **When you add a variant to [`crate::container_click::Station`], grep this
//! module and `crate::server::workstation_result` together** — a hook is
//! consulted generically (it does not match on `station`), but a new station
//! whose own compute function is never routed through `workstation_result`
//! would silently never reach a plugin either, the exact shape this repo
//! already knows as an island.
//!
//! # Dependencies
//!
//! `lodestone_model::ItemStack` for the observed cells and the replacement
//! stack; `crate::container_click::Station` for which station this is. No
//! protocol crate — this hook sees only already-resolved game state, never a
//! packet.

use std::fmt;
use std::sync::{Arc, Mutex};

use lodestone_model::ItemStack;

use crate::container_click::Station;

/// What one crafting-station hook observes for a single evaluation — the
/// observation vocabulary for this seam (`docs/plugin-api.md`'s intent
/// doctrine, clause 1): real facts a plugin author would recognise, never an
/// internal menu-slot index.
#[derive(Debug, Clone)]
pub struct StationInputs {
    /// Which station this evaluation is for.
    pub station: Station,
    /// The station's own input cells, in the same order
    /// [`crate::server::workstation_result`] reads them — `[input, addition]`
    /// for the anvil/grindstone, `[template, base, addition]` for the
    /// smithing table, `[banner, dye, pattern_item]` for the loom, `[input]`
    /// for the stonecutter. A cell is `None` when that slot is empty, never
    /// omitted, so a hook can tell "empty" from "not this station's shape".
    pub cells: Vec<Option<ItemStack>>,
    /// The station's own computed result, `None` when the inputs do
    /// not currently combine into anything. This is what makes
    /// [`StationVerdict::Replace`] able to *tweak* a real result (append a
    /// lore line, force a custom name) rather than forcing every replacing
    /// hook to reimplement the station's own recipe — the same shape
    /// Bukkit's own `PrepareAnvilEvent.getResult()` gives a plugin before it
    /// calls `setResult`.
    pub computed: Option<ItemStack>,
}

/// One hook's answer for one [`StationInputs`] evaluation.
#[derive(Debug, Clone)]
pub enum StationVerdict {
    /// Leave the station's own computed result (or lack of one) unchanged.
    Allow,
    /// Refuse to produce a result at all, regardless of what the station
    /// itself computed — `PrepareAnvilEvent.setResult(null)`'s shape.
    Deny,
    /// Replace the result with a plugin-supplied stack.
    Replace(ItemStack),
}

/// A plugin's registered interest in crafting-station results.
///
/// `&self`, not `&mut self`: a hook may be consulted from any connection
/// processing a click, so the implementor owns its own synchronisation — the
/// same contract [`crate::plugin_channels::PluginChannelHandler`] gives
/// wire-level plugin messaging.
///
/// Must not panic: this runs inline on the connection resolving the click or
/// redrawing the menu, and a panic here takes that player's connection with
/// it.
pub trait CraftingStationHook: Send + Sync {
    /// Answers one evaluation of `inputs`.
    fn on_prepare(&self, inputs: &StationInputs) -> StationVerdict;
}

/// One registered hook, at the priority it was registered with.
struct Registration {
    priority: i32,
    hook: Arc<dyn CraftingStationHook>,
}

/// The shared registry of crafting-station hooks.
///
/// Clone it freely: every clone is the same registry, exactly
/// [`crate::plugin_channels::PluginChannelRegistry`]'s own contract. The
/// [`Default`] is inert — no registered hooks, so [`evaluate`](Self::evaluate)
/// returns `computed` unchanged for every caller that never installs one,
/// which is what lets every existing `workstation_result` caller pass one
/// with no behaviour change.
#[derive(Clone, Default)]
pub struct CraftingStationHooks(Arc<Mutex<Vec<Registration>>>);

impl fmt::Debug for CraftingStationHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hooks = self.0.lock().expect("crafting-station hook registry poisoned");
        f.debug_struct("CraftingStationHooks").field("registered", &hooks.len()).finish()
    }
}

impl CraftingStationHooks {
    /// A fresh, empty registry — the inert [`Default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `hook` at `priority` (ascending — a lower priority is asked
    /// first, mirroring `docs/packet-wiring.md`'s `EgressFilters`/
    /// `ActionVetoes` convention). Registration order breaks a tie between
    /// two hooks registered at the same priority.
    pub fn register(&self, priority: i32, hook: Arc<dyn CraftingStationHook>) {
        let mut hooks = self.0.lock().expect("crafting-station hook registry poisoned");
        hooks.push(Registration { priority, hook });
        hooks.sort_by_key(|registration| registration.priority);
    }

    /// How many hooks are currently registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.lock().expect("crafting-station hook registry poisoned").len()
    }

    /// Whether no hook is registered — the common, zero-cost case every
    /// existing station keeps today.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Evaluates every registered hook against `inputs`, in ascending
    /// priority order. The first non-`Allow` verdict wins — a later hook is
    /// never asked once one has denied or replaced, so two hooks cannot loop
    /// rewriting each other's output. Returns `computed` unchanged when every
    /// hook allows, including when none are registered at all.
    ///
    /// The lock is not held across a hook call — the registered list is
    /// cloned out first, mirroring
    /// [`PluginChannelRegistry::dispatch`](crate::plugin_channels::PluginChannelRegistry::dispatch)'s
    /// own reasoning, so a hook that itself calls
    /// [`register`](Self::register) cannot deadlock.
    #[must_use]
    pub fn evaluate(&self, inputs: &StationInputs, computed: Option<ItemStack>) -> Option<ItemStack> {
        let hooks: Vec<Arc<dyn CraftingStationHook>> = {
            let guard = self.0.lock().expect("crafting-station hook registry poisoned");
            guard.iter().map(|registration| registration.hook.clone()).collect()
        };
        for hook in &hooks {
            match hook.on_prepare(inputs) {
                StationVerdict::Allow => continue,
                StationVerdict::Deny => return None,
                StationVerdict::Replace(stack) => return Some(stack),
            }
        }
        computed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str) -> ItemStack {
        ItemStack::new(item.parse().expect("valid key"), 1)
    }

    struct AlwaysDeny;
    impl CraftingStationHook for AlwaysDeny {
        fn on_prepare(&self, _inputs: &StationInputs) -> StationVerdict {
            StationVerdict::Deny
        }
    }

    struct AlwaysReplace(ItemStack);
    impl CraftingStationHook for AlwaysReplace {
        fn on_prepare(&self, _inputs: &StationInputs) -> StationVerdict {
            StationVerdict::Replace(self.0.clone())
        }
    }

    struct RecordingAllow(Arc<Mutex<usize>>);
    impl CraftingStationHook for RecordingAllow {
        fn on_prepare(&self, _inputs: &StationInputs) -> StationVerdict {
            *self.0.lock().expect("poisoned") += 1;
            StationVerdict::Allow
        }
    }

    #[test]
    fn an_empty_registry_returns_the_computed_result_unchanged() {
        let hooks = CraftingStationHooks::new();
        let inputs = StationInputs { station: Station::Anvil, cells: vec![None, None], computed: None };
        let computed = Some(stack("minecraft:diamond_sword"));
        assert_eq!(hooks.evaluate(&inputs, computed.clone()), computed);
    }

    #[test]
    fn a_deny_verdict_wins_even_over_a_real_computed_result() {
        let hooks = CraftingStationHooks::new();
        hooks.register(0, Arc::new(AlwaysDeny));
        let inputs = StationInputs { station: Station::Anvil, cells: vec![None, None], computed: None };
        assert_eq!(hooks.evaluate(&inputs, Some(stack("minecraft:diamond_sword"))), None);
    }

    #[test]
    fn a_replace_verdict_substitutes_the_computed_result() {
        let hooks = CraftingStationHooks::new();
        let replacement = stack("minecraft:netherite_sword");
        hooks.register(0, Arc::new(AlwaysReplace(replacement.clone())));
        let inputs = StationInputs { station: Station::Smithing, cells: vec![None, None, None], computed: None };
        assert_eq!(hooks.evaluate(&inputs, None), Some(replacement));
    }

    /// The discriminating case: a lower-priority `Deny` must win over a
    /// higher-priority `Replace` that never even runs, not merely happen to
    /// agree with it — proven by a distinct replacement stack the second
    /// hook would have produced had it been reached.
    #[test]
    fn the_first_non_allow_verdict_in_priority_order_wins_and_short_circuits() {
        let hooks = CraftingStationHooks::new();
        let seen = Arc::new(Mutex::new(0usize));
        hooks.register(10, Arc::new(AlwaysReplace(stack("minecraft:netherite_sword"))));
        hooks.register(0, Arc::new(AlwaysDeny));
        let inputs = StationInputs { station: Station::Anvil, cells: vec![None, None], computed: None };
        assert_eq!(hooks.evaluate(&inputs, Some(stack("minecraft:diamond_sword"))), None);
        let _ = seen;
    }

    /// An `Allow` hook is consulted (it really runs) and then falls through
    /// to whatever the *next* hook decides, rather than the loop stopping at
    /// the first hook regardless of its verdict.
    #[test]
    fn an_allow_verdict_falls_through_to_the_next_hook() {
        let hooks = CraftingStationHooks::new();
        let calls = Arc::new(Mutex::new(0usize));
        hooks.register(0, Arc::new(RecordingAllow(calls.clone())));
        hooks.register(1, Arc::new(AlwaysDeny));
        let inputs = StationInputs { station: Station::Loom, cells: vec![None, None, None], computed: None };
        assert_eq!(hooks.evaluate(&inputs, Some(stack("minecraft:white_banner"))), None);
        assert_eq!(*calls.lock().expect("poisoned"), 1, "the allowing hook must have been asked");
    }

    #[test]
    fn registering_replaces_nothing_and_len_tracks_registrations() {
        let hooks = CraftingStationHooks::new();
        assert!(hooks.is_empty());
        hooks.register(0, Arc::new(AlwaysDeny));
        hooks.register(0, Arc::new(AlwaysDeny));
        assert_eq!(hooks.len(), 2, "two registrations at the same priority are both kept");
    }
}
