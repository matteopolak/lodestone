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

use std::sync::{Arc, Mutex};

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{
    Commands, Entity, IntoScheduleConfigs, MessageReader, Query, ResMut, Resource, With,
};
use lodestone_command::StringArgument;
use lodestone_ecs::commands::{CommandOutcome, CommandRegistry, PluginCommand, PluginCommandsPlugin};
use lodestone_ecs::events::{GameEvent, GameEventBusPlugin};
use lodestone_ecs::player::{ActionQueue, LocalPlayer, LookIntent};
use lodestone_ecs::veto::{ActionVetoPlugin, ActionVetoes, Verdict};
// `TickSet` via the crate root, not `lodestone_ecs::sets::TickSet`: the `sets` module
// itself is private and only its re-exports are public.
use lodestone_ecs::{CorePlugin, GameTick, TickSet};

use crate::abi;
use crate::abi::{IntentAction, LoweredAction};
use crate::host::{Event, PluginHost};

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
/// The vector preserves guest/load order, and the final look request wins just as
/// successive native writes to the same optional component do.
#[derive(Resource, Default, Debug)]
pub struct PendingWasmIntents(Vec<IntentAction>);

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
fn register_wasm_commands(app: &mut App, broker: &Arc<Mutex<PluginHost>>) {
    let specs = broker
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .command_specs();

    for (plugin_index, spec) in specs {
        let mut command = PluginCommand::new(spec.name);
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

        if let Err(error) = app.world_mut().resource_mut::<CommandRegistry>().register(command) {
            tracing::error!(plugin_index, "refused WASM command registration: {error}");
        }
    }
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
        register_wasm_commands(app, &verdict_broker);
        app.insert_resource(plugins);
        app.init_resource::<PendingWasmIntents>();
        app.add_systems(
            GameTick,
            drive_wasm_plugins
                .in_set(TickSet::Intent)
                .before(apply_wasm_intents),
        );
        app.add_systems(
            GameTick,
            apply_wasm_intents
                .in_set(TickSet::Intent)
                .before(lodestone_ecs::player::apply_look_intent),
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
) {
    let batch: Vec<lodestone_model::ClientEvent> = events.read().map(|e| e.0.clone()).collect();

    let mut refused = 0_u64;
    let (lowered, lowered_intents) = plugins.with_host(|host| {
        let fuel = host.fuel_per_tick();
        let mut out = Vec::new();
        let mut intent_out = Vec::new();
        for plugin in host.plugins_mut() {
            let granted = plugin.granted().clone();
            let lifted: Vec<Event> = batch
                .iter()
                .filter_map(|e| abi::lift_event(e, &granted))
                .collect();
            for action in plugin.tick(&lifted, fuel) {
                match abi::lower_action(action, &granted) {
                    Ok(LoweredAction::Client(client_action)) => out.push(client_action),
                    Ok(LoweredAction::Intent(intent)) => intent_out.push(intent),
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
        (out, intent_out)
    });

    plugins.refused = plugins.refused.saturating_add(refused);
    // Appended, not assigned: `ActionQueue` is shared with every native system in
    // the tick, and order is send order on the wire.
    queue.0.extend(lowered);
    intents.0.extend(lowered_intents);
}

/// Apply the guest-owned intent updates before the existing ECS look consumer.
///
/// A `LookIntent` is optional on the local player, so inserting and removing it
/// is the ownership hand-off. `Commands` remains correct here because the explicit
/// ordering against `apply_look_intent` makes Bevy flush this deferred buffer before
/// that reader; the integration gate asserts the resulting rotation reaches the
/// normal outbound movement action in this same `GameTick`.
fn apply_wasm_intents(
    mut pending: ResMut<PendingWasmIntents>,
    players: Query<Entity, With<LocalPlayer>>,
    mut commands: Commands,
) {
    let Some(last) = pending.0.pop() else {
        return;
    };
    pending.0.clear();

    let IntentAction::Look(look) = last;
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
