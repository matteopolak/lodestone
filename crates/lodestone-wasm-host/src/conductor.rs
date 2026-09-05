//! The conductor: the one native `bevy` system that drives every loaded guest, and
//! the [`WasmHostPlugin`] that installs it.
//!
//! # Why a conductor rather than letting guests be systems
//!
//! A guest cannot *be* a system — it has no Rust type identity with the host, so it
//! cannot be registered with `add_systems` or ordered against arbitrary sets. So the
//! host runs **one** system per schedule slot which drives every guest's due
//! `on-task` callbacks followed by `on-tick`, in sequence.
//!
//! That is not a workaround; it is what preserves `docs/plugin-api.md`'s clause 2,
//! *exactly one system owns each machine*. This system is the single writer of
//! `ActionQueue` on behalf of every guest, so no guest can fork a sequence counter
//! or race another guest's writes, **even maliciously** — the worst a guest can do
//! is return a list. Guests order among themselves by load order, which
//! `crate::manifest` sorts by their declared `EventPriority` tier.
//!
//! # Why the conductor runs in `TickSet::Intent`
//!
//! Actions are egress, so `TickSet::Send` is the semantically obvious home and is
//! **not** used. The reason is a real ordering hazard rather than taste:
//! `lodestone_ecs::events::age_game_event_bus` is anchored `.in_set(TickSet::Send)`
//! and is private, so a reader placed in the same set is *unordered* against the
//! thing that ages the message buffer it reads — a coin flip, resolved at schedule
//! build time, that would show up as a plugin missing every other tick's events.
//! `Predict` would run strictly before `Send`, but look intents must be installed before the existing
//! `apply_look_intent` consumer, physics, and the movement sender. The host therefore
//! runs at `TickSet::Intent`; the event bus is still safe there because it is only
//! aged at `Send`.
//!
//! **If `lodestone-ecs` ever exposes a public ordering anchor for the bus ager, this
//! system should move to `TickSet::Send` and order `.before` it.** That is a
//! one-line change here and a patch the ECS owners would have to make; it is noted
//! in `docs/wasm-plugin-host.md` §"Pending on other work".
//!
//! # What a refused action does
//!
//! It is counted and logged, naming the capability that was missing — never
//! silently dropped. A plugin whose actions vanish with no explanation is the most
//! confusing failure a plugin API can produce, and the `refused` counter is what
//! makes "the capability filter is doing something" observable from outside.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{
    Commands, Entity, IntoScheduleConfigs, MessageReader, Query, Res, ResMut, Resource, With,
};
use bevy_ecs::schedule::ApplyDeferred;
use lodestone_command::StringArgument;
use lodestone_ecs::commands::{CommandOutcome, CommandRegistry, PluginCommand, PluginCommandsPlugin};
use lodestone_ecs::events::{GameEvent, GameEventBusPlugin};
use lodestone_ecs::player::{
    ActionQueue, BreakIntent, BreakOutcome, LocalPlayer, LookIntent, MovementIntent, PlaceOutcome,
    SelectSlotIntent,
};
use lodestone_ecs::veto::{ActionVetoPlugin, ActionVetoes, Verdict};
// `TickSet` via the crate root, not `lodestone_ecs::sets::TickSet`: the `sets` module
// itself is private and only its re-exports are public.
use lodestone_ecs::{ChunkWorld, CorePlugin, GameTick, TickSet};

use crate::abi;
use crate::abi::{IntentAction, LoweredAction};
use crate::capability::Capability;
use crate::host::{
    BlockMutationRefusal, BlockMutationStatus, CommandSpec, Event, PluginGrantPolicy, PluginHost,
    ReloadError, ResidentBlockMutation, ResidentBlockMutationOutcome,
};

/// The loaded guests, as an ECS resource.
///
/// # Why the `Mutex`
///
/// A `wasmtime::Store` is `Send` but not `Sync`, and a bevy `Resource` must be both.
/// The alternatives were worse: a non-send resource pins the `World` to one thread,
/// and this `World` is driven from more than one (`NetIngest` runs on the net
/// thread, `GameTick` on the driver), so a non-send resource would panic the first
/// time the wrong thread touched it. The lock is never contended in practice — the
/// only accessor takes `ResMut`, which bevy already guarantees is exclusive — so it
/// costs an uncontended atomic per tick and buys `Sync` honestly.
#[derive(Resource)]
pub struct WasmPlugins {
    host: Arc<Mutex<PluginHost>>,
    /// Actions refused for want of a capability, cumulative. See this module's
    /// header for why this is a counter and not a silent drop.
    refused: u64,
}

/// Intent updates emitted by guests during the current game tick.
///
/// They are kept out of [`ActionQueue`] because they are not protocol actions:
/// the local-player systems own validation, simulation, and packet production.
/// The vector preserves guest/load order. Look and movement are independently
/// last-wins, so one guest can set both in one output list without one action
/// accidentally erasing the other. Placement is one-shot and also last-wins:
/// one local player has one `PlaceIntent` component, so selecting any other
/// policy would need a second owner or a queue the production lifecycle does not
/// have.
#[derive(Resource, Default, Debug)]
pub struct PendingWasmIntents(Vec<IntentAction>);

/// Bounded inventory-click requests awaiting the shell's live menu predictor.
///
/// This contains only copied, fixed-size values. It never contains a menu,
/// cursor stack, world handle, or prebuilt container packet: the shell owns all
/// of those and drains this after the game-tick world guard is gone.
#[derive(Resource, Default, Debug)]
pub struct PendingWasmMenuClicks(Vec<crate::abi::InventoryClickIntent>);

/// Bounded copied requests awaiting the shell's authoritative singleplayer
/// bridge. This resource never holds an ECS guard, a network handle, or a
/// server borrow: the shell drains it after the game-tick guard ends, and feeds
/// finite outcomes back before a later guest tick.
#[derive(Resource, Default, Debug)]
pub struct PendingWasmWorldMutations {
    requests: Vec<(usize, ResidentBlockMutation)>,
    outcomes: Vec<(usize, ResidentBlockMutationOutcome)>,
}

impl PendingWasmWorldMutations {
    const MAX_PENDING: usize = 64;

    /// Remove this tick's copied requests for transport by the shell.
    #[must_use]
    pub fn take_requests(&mut self) -> Vec<(usize, ResidentBlockMutation)> {
        std::mem::take(&mut self.requests)
    }

    /// Return all terminal answers accumulated by the network task.
    fn take_outcomes(&mut self) -> Vec<(usize, ResidentBlockMutationOutcome)> {
        std::mem::take(&mut self.outcomes)
    }

    /// Add one terminal answer. It is intentionally public so the shell can
    /// return a result without ever borrowing the guest host.
    pub fn push_outcome(&mut self, plugin: usize, outcome: ResidentBlockMutationOutcome) {
        self.outcomes.push((plugin, outcome));
    }

    fn push_request(&mut self, plugin: usize, request: ResidentBlockMutation) -> bool {
        if self.requests.len() == Self::MAX_PENDING {
            self.push_outcome(
                plugin,
                ResidentBlockMutationOutcome {
                    request_id: request.request_id,
                    status: BlockMutationStatus::Refused(BlockMutationRefusal::Unavailable),
                },
            );
            false
        } else {
            self.requests.push((plugin, request));
            true
        }
    }
}

impl PendingWasmMenuClicks {
    const MAX_PENDING: usize = 64;

    /// Remove every request accumulated during this tick.
    #[must_use]
    pub fn take(&mut self) -> Vec<crate::abi::InventoryClickIntent> {
        std::mem::take(&mut self.0)
    }

    fn push(&mut self, click: crate::abi::InventoryClickIntent) -> bool {
        if self.0.len() == Self::MAX_PENDING {
            return false;
        }
        self.0.push(click);
        true
    }
}

/// The command roots currently owned by the WASM conductor.
///
/// The native registry intentionally knows nothing about guest stores. Keeping
/// this narrow ownership list here lets reload remove only handlers installed by
/// this conductor, never a neighbouring native plugin's commands.
#[derive(Resource, Default, Debug)]
struct WasmCommandRoots(Vec<String>);

/// A requested runtime replacement that cannot safely commit.
#[derive(Debug, thiserror::Error)]
pub enum WasmReloadError {
    #[error("the app has no installed WASM host")]
    MissingHost,
    #[error("the app has no command registry for the WASM host")]
    MissingCommandRegistry,
    #[error(transparent)]
    Reload(#[from] ReloadError),
    #[error("the reloaded WASM command `{command}` conflicts with a command outside the WASM host")]
    CommandConflict { command: String },
}

impl std::fmt::Debug for WasmPlugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmPlugins")
            .field("refused", &self.refused)
            .finish_non_exhaustive()
    }
}

impl WasmPlugins {
    #[must_use]
    pub fn new(host: PluginHost) -> Self {
        Self {
            host: Arc::new(Mutex::new(host)),
            refused: 0,
        }
    }

    /// How many guest actions have been refused for want of a capability since
    /// startup.
    #[must_use]
    pub fn refused_actions(&self) -> u64 {
        self.refused
    }

    /// Run a closure against the host. Useful for tests and for a future in-game
    /// plugin list; not a general escape hatch.
    pub fn with_host<R>(&self, f: impl FnOnce(&mut PluginHost) -> R) -> R {
        let mut guard = self.host.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    fn verdict_broker(&self) -> Arc<Mutex<PluginHost>> {
        Arc::clone(&self.host)
    }

    fn stage_reload(
        &self,
        directory: &Path,
        grants: &PluginGrantPolicy,
    ) -> Result<PluginHost, ReloadError> {
        let guard = self.host.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.stage_directory_reload(directory, grants)
    }

    fn replace_host(&self, replacement: PluginHost) {
        let mut guard = self.host.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = replacement;
    }
}

fn ask_wasm_verdict(
    broker: &Arc<Mutex<PluginHost>>,
    context: &lodestone_ecs::veto::VerbContext,
) -> Verdict {
    let mut host = broker
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match host.verdict_all(context) {
        crate::host::VerdictDispatch::Allow => Verdict::Allow,
        crate::host::VerdictDispatch::Deny | crate::host::VerdictDispatch::Error => Verdict::Deny,
    }
}

/// Installs the wasm plugin tier on an `App`.
///
/// This is *just another native plugin*, registered through the same
/// `add_plugins` seam a consumer uses — it has no privileged position, which is the
/// no-two-APIs principle applied to the loader itself.
///
/// ```no_run
/// # use lodestone_wasm_host::{CapabilitySet, PluginHost, WasmHostPlugin};
/// let host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
/// let mut app = lodestone_app::client_app();
/// app.add_plugins(WasmHostPlugin::new(host));
/// ```
pub struct WasmHostPlugin {
    /// `Plugin::build` takes `&self`, so the host has to be moved out from behind a
    /// shared reference. `Mutex<Option<_>>` + `take()` is bevy's own idiom for a
    /// plugin that owns a non-`Clone` value; a second `build` finds `None` and
    /// leaves the existing resource alone rather than replacing it with an empty
    /// host, which is what would silently unload every plugin.
    host: Mutex<Option<PluginHost>>,
}

impl std::fmt::Debug for WasmHostPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmHostPlugin").finish_non_exhaustive()
    }
}

impl WasmHostPlugin {
    #[must_use]
    pub fn new(host: PluginHost) -> Self {
        Self {
            host: Mutex::new(Some(host)),
        }
    }
}

fn run_wasm_command(
    broker: &Arc<Mutex<PluginHost>>,
    plugin_index: usize,
    invocation: &lodestone_ecs::commands::CommandInvocation<'_>,
) -> CommandOutcome {
    let mut host = broker
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match host.command(
        plugin_index,
        invocation.input.clone(),
        abi::lift_command_context(&invocation.source),
    ) {
        Ok(crate::host::CommandOutcome::Success(result)) => CommandOutcome::Success(result),
        Ok(crate::host::CommandOutcome::Failure(message)) => CommandOutcome::Failure(message),
        Err(error) => CommandOutcome::Failure(format!("WASM command handler failed: {error}")),
    }
}

/// Register the roots the loaded guests declared during `init`.
///
/// The native registry owns parsing, alias rewriting, and permission checks. Each
/// command gets one greedy tail so the guest receives the canonical whole line,
/// but the host deliberately does not pretend that this is the native argument
/// tree API: typed guest argument schemas and suggestions remain a later ABI
/// extension.
fn register_wasm_commands(
    registry: &mut CommandRegistry,
    broker: &Arc<Mutex<PluginHost>>,
) -> Vec<String> {
    let specs = broker
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .command_specs();

    let mut roots = Vec::new();
    for (plugin_index, spec) in specs {
        let command_name = spec.name.clone();
        let mut command = PluginCommand::new(command_name.clone());
        command.description(spec.description);
        for alias in spec.aliases {
            command.alias(alias);
        }
        if let Some(permission) = spec.permission {
            command.permission(permission);
        }

        let root = command.root();
        let root_broker = Arc::clone(broker);
        command.on_execute(root, move |invocation| {
            run_wasm_command(&root_broker, plugin_index, invocation)
        });

        let tail = command.argument(
            root,
            "arguments",
            Arc::new(StringArgument::greedy()),
        );
        let tail_broker = Arc::clone(broker);
        command.on_execute(tail, move |invocation| {
            run_wasm_command(&tail_broker, plugin_index, invocation)
        });

        match registry.register(command) {
            Ok(()) => roots.push(command_name.to_lowercase()),
            Err(error) => {
                tracing::error!(plugin_index, "refused WASM command registration: {error}");
            }
        }
    }
    roots
}

fn command_claims(specs: &[(usize, CommandSpec)]) -> Result<BTreeSet<String>, WasmReloadError> {
    let mut claims = BTreeSet::new();
    for (_, spec) in specs {
        for claim in std::iter::once(&spec.name).chain(spec.aliases.iter()) {
            let claim = claim.to_lowercase();
            if !claims.insert(claim.clone()) {
                return Err(WasmReloadError::CommandConflict { command: claim });
            }
        }
    }
    Ok(claims)
}

fn retired_command_claims(registry: &CommandRegistry, roots: &[String]) -> BTreeSet<String> {
    roots
        .iter()
        .filter_map(|root| registry.get(root))
        .flat_map(|command| {
            std::iter::once(command.name.to_lowercase())
                .chain(command.aliases.iter().map(|alias| alias.to_lowercase()))
        })
        .collect()
}

/// Replace all guest stores with a revalidated directory snapshot.
///
/// This is an explicit embedding lifecycle operation, not a watcher. It first
/// stages every manifest and module against the supplied grants, then checks
/// that the candidate command roots will not shadow a native command. Only then
/// does it drop the old stores and replace their command registrations. A failed
/// policy parse should be kept outside this function: do not call it with a
/// partial grant set after parsing fails.
///
/// The function also clears guest-owned, not-yet-applied intents. Those values
/// belong to the unloaded stores; replaying them after a successful replacement
/// would let an old guest act after it was removed. Fresh guest stores reset the
/// bounded placement and break-outcome cursors at the same commit point.
pub fn reload_wasm_plugins(
    app: &mut App,
    directory: &Path,
    grants: &PluginGrantPolicy,
) -> Result<(), WasmReloadError> {
    let replacement = {
        let Some(plugins) = app.world().get_resource::<WasmPlugins>() else {
            return Err(WasmReloadError::MissingHost);
        };
        plugins.stage_reload(directory, grants)?
    };
    let specs = replacement.command_specs();
    let candidate_claims = command_claims(&specs)?;
    let current_roots = app
        .world()
        .get_resource::<WasmCommandRoots>()
        .map_or_else(Vec::new, |roots| roots.0.clone());
    {
        let Some(registry) = app.world().get_resource::<CommandRegistry>() else {
            return Err(WasmReloadError::MissingCommandRegistry);
        };
        let retired = retired_command_claims(registry, &current_roots);
        if let Some(command) = candidate_claims
            .iter()
            .find(|command| registry.get(command).is_some() && !retired.contains(*command))
        {
            return Err(WasmReloadError::CommandConflict {
                command: command.clone(),
            });
        }
    }

    let broker = app
        .world()
        .get_resource::<WasmPlugins>()
        .expect("the host was checked above")
        .verdict_broker();
    {
        let mut registry = app.world_mut().resource_mut::<CommandRegistry>();
        for root in &current_roots {
            let removed = registry.unregister(root);
            debug_assert!(
                removed.is_some(),
                "a tracked WASM command root must still be registered"
            );
        }
    }
    app.world().resource::<WasmPlugins>().replace_host(replacement);
    let roots = {
        let mut registry = app.world_mut().resource_mut::<CommandRegistry>();
        register_wasm_commands(&mut registry, &broker)
    };
    app.world_mut().insert_resource(WasmCommandRoots(roots));
    app.world_mut().resource_mut::<PendingWasmIntents>().0.clear();
    app.world_mut().resource_mut::<PendingWasmMenuClicks>().0.clear();
    Ok(())
}

impl Plugin for WasmHostPlugin {
    fn build(&self, app: &mut App) {
        // The guests read the event bus, which is off by default because every event
        // then has to take the ECS write lock to reach `Messages<GameEvent>`
        // (`lodestone_ecs::events`'s own module doc). Adding it here rather than
        // asking the consumer to is the right call: a wasm plugin that observes
        // nothing is not a plugin, so the cost is one this tier has genuinely opted
        // into.
        if !app.is_plugin_added::<CorePlugin>() {
            app.add_plugins(CorePlugin);
        }
        if !app.is_plugin_added::<GameEventBusPlugin>() {
            app.add_plugins(GameEventBusPlugin);
        }
        if !app.is_plugin_added::<ActionVetoPlugin>() {
            app.add_plugins(ActionVetoPlugin);
        }
        if !app.is_plugin_added::<PluginCommandsPlugin>() {
            app.add_plugins(PluginCommandsPlugin);
        }
        app.init_resource::<ActionQueue>();

        let Some(host) = self
            .host
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            tracing::warn!("WasmHostPlugin::build ran twice; keeping the first host");
            return;
        };
        let plugins = WasmPlugins::new(host);
        let verdict_broker = plugins.verdict_broker();
        for verb in lodestone_ecs::veto::Verb::ALL {
            let broker = Arc::clone(&verdict_broker);
            app.world_mut()
                .resource_mut::<ActionVetoes>()
                .register(*verb, "wasm-verdicts", 0, move |context| ask_wasm_verdict(&broker, context));
        }
        app.insert_resource(plugins);
        let roots = {
            let mut registry = app.world_mut().resource_mut::<CommandRegistry>();
            register_wasm_commands(&mut registry, &verdict_broker)
        };
        app.insert_resource(WasmCommandRoots(roots));
        app.init_resource::<PendingWasmIntents>();
        app.init_resource::<PendingWasmMenuClicks>();
        app.init_resource::<PendingWasmWorldMutations>();
        app.add_systems(
            GameTick,
            drive_wasm_plugins
                .in_set(TickSet::Intent)
                .before(apply_wasm_intents),
        );
        app.add_systems(
            GameTick,
            (
                apply_wasm_intents,
                apply_wasm_break_intents,
                apply_wasm_place_intents,
                apply_wasm_select_slot_intents,
                ApplyDeferred,
            )
                .chain()
                .in_set(TickSet::Intent)
                .before(lodestone_ecs::player::apply_look_intent),
        );
        app.add_systems(
            GameTick,
            apply_wasm_movement_intents
                .in_set(TickSet::Intent)
                .after(apply_wasm_select_slot_intents)
                .after(lodestone_controller::ecs::compute_movement_intent),
        );
    }
}

/// `TickSet::Intent`: lift this tick's events, drive every guest, then lower what
/// comes back onto [`ActionQueue`] or the local-player intent seam.
///
/// The per-guest lift is not hoisted out of the loop on purpose: the set of events a
/// guest sees depends on *its own* capabilities, so two guests with different
/// `observe:` grants must get different lists. Hoisting it would be a capability
/// leak dressed up as an optimisation. When guest counts make that cost real, the
/// fix is to cache per distinct capability set, not to lift once.
pub fn drive_wasm_plugins(
    mut plugins: ResMut<WasmPlugins>,
    mut events: MessageReader<GameEvent>,
    mut queue: ResMut<ActionQueue>,
    mut intents: ResMut<PendingWasmIntents>,
    mut menu_clicks: ResMut<PendingWasmMenuClicks>,
    mut world_mutations: ResMut<PendingWasmWorldMutations>,
    chunk_world: Option<Res<ChunkWorld>>,
    players: Query<(Entity, &BreakOutcome, &PlaceOutcome), With<LocalPlayer>>,
) {
    let batch: Vec<lodestone_model::ClientEvent> = events.read().map(|e| e.0.clone()).collect();
    let place_outcome = players
        .iter()
        .next()
        .and_then(|(player, _, outcome)| abi::lift_place_outcome(outcome).map(|outcome| (player, outcome)));
    let break_outcome = players
        .iter()
        .next()
        .map(|(player, outcome, _)| (player, abi::lift_break_outcome(outcome)));

    let mutation_outcomes = world_mutations.take_outcomes();
    let mut refused = 0_u64;
    let (lowered, lowered_intents, lowered_menu_clicks, lowered_world_mutations) = plugins.with_host(|host| {
        // `ChunkWorld` is a cloneable Arc handle, not an ECS guard. Guests can
        // only reach it through the bounded `world-snapshot.read-blocks` import, which
        // copies values and drops the chunk lock before returning to guest code.
        host.set_chunk_world(chunk_world.as_deref().cloned());
        let fuel = host.fuel_per_tick();
        let mut out = Vec::new();
        let mut intent_out = Vec::new();
        let mut menu_click_out = Vec::new();
        let mut world_mutation_out = Vec::new();
        for (plugin_index, plugin) in host.plugins_mut().iter_mut().enumerate() {
            let granted = plugin.granted().clone();
            let lifted: Vec<Event> = batch
                .iter()
                .filter_map(|e| abi::lift_event(e, &granted))
                .collect();
            let mut lifted = lifted;
            // Entity lifecycles carry a host-owned generation ledger, so they
            // cannot be lifted by the stateless generic event mapper above.
            // The guest receives copied packet vocabulary only; the ledger never
            // contains an ECS entity or survives a reload of this guest store.
            for event in &batch {
                lifted.extend(plugin.lift_entity_events(event));
            }
            if granted.contains(Capability::ObservePlace)
                && let Some((player, outcome)) = &place_outcome
                && plugin.observe_place_outcome(*player, outcome)
            {
                lifted.push(Event::PlaceOutcome(outcome.clone()));
            }
            if granted.contains(Capability::ObserveBreak)
                && let Some((player, outcome)) = &break_outcome
                && plugin.observe_break_outcome(*player, outcome)
            {
                lifted.push(Event::BreakOutcome(outcome.clone()));
            }
            for (outcome_plugin, outcome) in &mutation_outcomes {
                if granted.contains(Capability::WriteWorld) && *outcome_plugin == plugin_index {
                    lifted.push(Event::ResidentBlockMutationOutcome(outcome.clone()));
                }
            }
            for action in plugin.tick(&lifted, fuel) {
                match abi::lower_action(action, &granted) {
                    Ok(LoweredAction::Client(client_action)) => out.push(client_action),
                    Ok(LoweredAction::Intent(IntentAction::InventoryClick(click))) => {
                        menu_click_out.push(click)
                    }
                    Ok(LoweredAction::Intent(intent)) => intent_out.push(intent),
                    Ok(LoweredAction::ResidentBlockMutation(request)) => {
                        world_mutation_out.push((plugin_index, request));
                    }
                    Err(missing) => {
                        refused += 1;
                        tracing::warn!(
                            plugin = %plugin.name(),
                            "refused an action: it requires the `{missing}` capability, which this \
                             plugin was not granted"
                        );
                    }
                }
            }
        }
        (out, intent_out, menu_click_out, world_mutation_out)
    });

    // Appended, not assigned: `ActionQueue` is shared with every native system in
    // the tick, and order is send order on the wire.
    queue.0.extend(lowered);
    intents.0.extend(lowered_intents);
    for click in lowered_menu_clicks {
        if !menu_clicks.push(click) {
            refused += 1;
            tracing::warn!("refused a WASM inventory click: the bounded shell handoff is full");
        }
    }
    for (plugin, request) in lowered_world_mutations {
        if !world_mutations.push_request(plugin, request) {
            refused += 1;
            tracing::warn!("refused a WASM resident-block mutation: the bounded shell handoff is full");
        }
    }
    plugins.refused = plugins.refused.saturating_add(refused);
}

/// Apply guest-owned look updates before the existing ECS look consumer.
///
/// A `LookIntent` is optional on the local player, so inserting and removing it
/// is the ownership hand-off. `Commands` remains correct here because the explicit
/// ordering against `apply_look_intent` makes Bevy flush this deferred buffer before
/// that reader; the integration gate asserts the resulting rotation reaches the
/// normal outbound movement action in this same `GameTick`.
fn apply_wasm_intents(
    pending: bevy_ecs::prelude::Res<PendingWasmIntents>,
    players: Query<Entity, With<LocalPlayer>>,
    mut commands: Commands,
) {
    let last = pending.0.iter().rev().find_map(|intent| match intent {
        IntentAction::Look(look) => Some(*look),
        IntentAction::Movement(_)
        | IntentAction::Break(_)
        | IntentAction::Place(_)
        | IntentAction::SelectSlot(_)
        | IntentAction::InventoryClick(_) => None,
    });
    let Some(look) = last else {
        return;
    };
    for entity in &players {
        match look {
            Some(look) => {
                commands.entity(entity).insert(look);
            }
            None => {
                commands.entity(entity).remove::<LookIntent>();
            }
        };
    }
}

/// Apply the final guest mining ownership update before `TickSet::Send` lets
/// the shell drive its mining lifecycle. The shell remains the sole owner of
/// validation, progress, prediction, sequence, and abort egress.
fn apply_wasm_break_intents(
    pending: Res<PendingWasmIntents>,
    players: Query<Entity, With<LocalPlayer>>,
    mut commands: Commands,
) {
    let break_intent = pending.0.iter().rev().find_map(|intent| match intent {
        IntentAction::Break(break_intent) => Some(*break_intent),
        IntentAction::Look(_)
        | IntentAction::Movement(_)
        | IntentAction::Place(_)
        | IntentAction::SelectSlot(_)
        | IntentAction::InventoryClick(_) => None,
    });
    let Some(break_intent) = break_intent else {
        return;
    };
    for entity in &players {
        match break_intent {
            Some(break_intent) => {
                commands.entity(entity).insert(break_intent);
            }
            None => {
                commands.entity(entity).remove::<BreakIntent>();
            }
        }
    }
}

/// Submit the final guest placement request through the already-installed
/// local-player lifecycle. This system is chained to `apply_deferred` inside
/// `TickSet::Intent`, so the insert is visible before `TickSet::Send`, where
/// the shell consumes `PlaceIntent`.
///
/// The host never manufactures a `ClientAction`: the shell validates the copied
/// target against its current world and inventory, owns prediction/sequence
/// state, then emits the normal action or a bounded `PlaceOutcome`.
fn apply_wasm_place_intents(
    pending: Res<PendingWasmIntents>,
    players: Query<Entity, With<LocalPlayer>>,
    mut commands: Commands,
) {
    let place = pending.0.iter().rev().find_map(|intent| match intent {
        IntentAction::Place(place) => Some(*place),
        IntentAction::Look(_)
        | IntentAction::Movement(_)
        | IntentAction::Break(_)
        | IntentAction::SelectSlot(_)
        | IntentAction::InventoryClick(_) => None,
    });
    let Some(place) = place else {
        return;
    };
    for entity in &players {
        commands.entity(entity).insert(place);
    }
}

/// Submit the final guest hotbar-selection request through the shell-owned
/// selection lifecycle. The shell's later `TickSet::Send` consumer performs the
/// range and same-slot gates, writes `SelectedSlot`, and emits its ordered
/// carried-item echo; the host never constructs that packet itself.
fn apply_wasm_select_slot_intents(
    pending: Res<PendingWasmIntents>,
    players: Query<Entity, With<LocalPlayer>>,
    mut commands: Commands,
) {
    let slot = pending.0.iter().rev().find_map(|intent| match intent {
        IntentAction::SelectSlot(slot) => Some(*slot),
        IntentAction::Look(_)
        | IntentAction::Movement(_)
        | IntentAction::Break(_)
        | IntentAction::Place(_)
        | IntentAction::InventoryClick(_) => None,
    });
    let Some(slot) = slot else {
        return;
    };
    for entity in &players {
        commands.entity(entity).insert(SelectSlotIntent(slot));
    }
}

/// Override the normal controller's copied input after it writes and before
/// physics reads it. `using_item` is deliberately retained from that controller
/// output, because a guest has no authority to forge item-use state.
fn apply_wasm_movement_intents(
    mut pending: ResMut<PendingWasmIntents>,
    mut players: Query<&mut MovementIntent, With<LocalPlayer>>,
) {
    let movement = pending.0.iter().rev().find_map(|intent| match intent {
        IntentAction::Movement(movement) => Some(*movement),
        IntentAction::Look(_)
        | IntentAction::Break(_)
        | IntentAction::Place(_)
        | IntentAction::SelectSlot(_)
        | IntentAction::InventoryClick(_) => None,
    });
    pending.0.clear();

    let Some(Some(movement)) = movement else {
        return;
    };
    for mut intent in &mut players {
        intent.0.forward = movement.forward;
        intent.0.strafe = movement.strafe;
        intent.0.jump = movement.jump;
        intent.0.sneak = movement.sneak;
        intent.0.sprint = movement.sprint;
    }
}
