//! The embedding itself: a `wasmtime` engine, one `Store` per loaded plugin, and
//! the capability-gated `Linker` that decides what each guest can reach.
//!
//! # What a guest can reach, and why the list is short
//!
//! Nothing, except the interfaces [`PluginHost::load_file`] adds to its `Linker`.
//! There is no ambient authority to remove: this crate depends on `wasmtime` with
//! `default-features = false` and **not** on `wasmtime-wasi`, so there is no
//! `fd_write`, no `path_open`, no clock, no socket and no environment for a guest
//! to find. A `std::fs::File::open` compiled into a `wasm32-unknown-unknown`
//! guest does not fail a permission check — the syscall it would need was never
//! linked, so the module either fails to instantiate (if it references an import)
//! or the call returns an error from a stub inside the guest's own copy of `std`.
//!
//! That is worth stating precisely, because "the sandbox denied it" and "the
//! function does not exist" are different claims and only the second one is true
//! here. It is the stronger of the two.
//!
//! # Preemption: fuel, not epochs, and why
//!
//! `docs/plans/runtime-plugin-loading.md` names epoch interruption as the
//! preemption mechanism. Epochs need a *watchdog* — something must call
//! `Engine::increment_epoch` on a timer — and a host that configures epoch
//! deadlines without one has a deadline that can never trip: an island, in this
//! repo's sense, and one whose test would pass because the well-behaved guest it
//! was pointed at never looped. Fuel needs nothing but a budget, so it is what
//! this crate enforces now: [`PluginHost::with_fuel`] sets a per-call budget, a
//! guest that exhausts it traps with *"all fuel consumed by WebAssembly"*, and
//! [`LoadedPlugin::failure`] records it as permanently failed so the conductor
//! stops calling it. `tests/preemption.rs` gates it against a guest that really
//! does spin forever, with the well-behaved guest under the same budget as the
//! control.
//!
//! Epochs remain the better long-term answer (they cost nothing on the fast path,
//! where fuel costs a counter decrement per block), but they need a watchdog
//! thread calling `Engine::increment_epoch` on a timer to mean anything — a
//! deadline configured without one can never trip. That pairing is not built
//! yet; fuel is what this crate enforces today.

use std::fmt;
use std::path::{Path, PathBuf};

use wasmtime::component::{Component, Linker, TypedFunc};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

use crate::bindings::lodestone::plugin::{filesystem, logging, scheduler, types};
use crate::capability::{Capability, CapabilitySet};

/// The WIT vocabulary, re-exported from the generated bindings so that nothing
/// outside this crate spells a `bindings::lodestone::plugin::…` path.
///
/// [`Event`] is the observation the host delivers and [`Action`] is what comes
/// back; the rest are their payload records. Each mirrors a `lodestone_model` type
/// named in `wit/lodestone-plugin.wit`.
pub use crate::bindings::lodestone::plugin::types::{
    Action, BlockOffset, ChatKind, ChatMessage, Event, Hand, Health, LogLevel, PluginInfo,
    SectionBlocksChanged, SectionPos,
};

/// The world version this host speaks. A guest's `init` must return this, and a
/// manifest must declare it; anything else is a load-time rejection.
///
/// The WIT world is a named, versioned unit, so "a guest built against
/// `lodestone:plugin@0.2.0`" is a thing the host can *detect* rather than
/// discover as a mysterious trap.
pub const ABI_WORLD: &str = "lodestone:plugin@0.2.0";

/// Default per-tick fuel budget. Chosen as "enough for any plugin doing plain
/// data work over a tick's event batch, nowhere near enough to survive a spin
/// loop": the chat responder's real tick uses low thousands of units.
pub const DEFAULT_FUEL_PER_TICK: u64 = 10_000_000;

/// Default per-guest linear-memory ceiling.
pub const DEFAULT_MEMORY_LIMIT: usize = 32 * 1024 * 1024;

/// How many *core* wasm instances one plugin may create. See the comment at the
/// `StoreLimitsBuilder` call site for why this is not 1.
const MAX_CORE_INSTANCES_PER_PLUGIN: usize = 32;

#[derive(Debug, Clone, Copy)]
struct ScheduledTask {
    id: types::TaskId,
    due_tick: u64,
    token: u64,
    period_ticks: Option<u32>,
}

/// Everything that can go wrong loading or driving a guest.
///
/// `wasmtime::Error` does not implement `std::error::Error` when wasmtime is
/// built with `default-features = false`, so its message is captured as a
/// `String` rather than chained as a `#[source]`. The capture uses `{:?}`, which
/// for wasmtime's anyhow-shaped error prints the whole causal chain — the part
/// that actually names the missing import.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HostError {
    #[error("reading plugin module `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "`{path}` is not WebAssembly (expected the `\\0asm` preamble, found {found:02x?}) — a \
         plugin module must be a wasm component, or a core module this host can encode into one"
    )]
    NotWasm { path: PathBuf, found: Vec<u8> },
    #[error("encoding `{path}`'s core module into a component: {message}")]
    Encode { path: PathBuf, message: String },
    #[error("compiling plugin `{name}`: {message}")]
    Compile { name: String, message: String },
    /// The capability-denial case with teeth: the guest references an import that
    /// policy did not put in the `Linker`. The message carries wasmtime's own
    /// text, which names the interface.
    #[error(
        "plugin `{name}` could not be instantiated with capabilities [{granted}]: {message}\n\
         note: an unresolved import here means the plugin uses a capability its manifest did not \
         declare, or that host policy does not grant"
    )]
    Instantiate {
        name: String,
        granted: String,
        message: String,
    },
    #[error("plugin `{name}` does not export `{export}`: {message}")]
    MissingExport {
        name: String,
        export: String,
        message: String,
    },
    #[error("plugin `{name}`'s `{export}` trapped: {message}")]
    Trap {
        name: String,
        export: String,
        message: String,
    },
    #[error(
        "plugin `{name}` targets ABI world `{found}`, but this host speaks `{expected}` — rebuild \
         the plugin against the host's `wit/lodestone-plugin.wit`"
    )]
    AbiMismatch {
        name: String,
        found: String,
        expected: String,
    },
    #[error(
        "plugin `{name}` requests capabilities [{missing}] that host policy does not grant \
         (granted: [{granted}])"
    )]
    CapabilityDenied {
        name: String,
        missing: String,
        granted: String,
    },
}

/// Either half of loading a plugin from a manifest can fail, and the two halves have
/// genuinely different causes — a malformed declaration versus a module the runtime
/// refused — so they stay separate types and this joins them rather than one
/// swallowing the other.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Manifest(#[from] crate::manifest::ManifestError),
    #[error(transparent)]
    Host(#[from] HostError),
}

/// Per-guest host state: the `T` in `Store<T>`.
///
/// The three `Vec`s are not debug scaffolding — they are the **recording sink**
/// the capability tests assert against. `crates/lodestone-fuzz` established the
/// shape here: a sink that discards cannot distinguish "the host refused" from
/// "the host wrongly accepted and the guest happened not to try", so the denial
/// gate needs both a refusal *and* a demonstrably-empty record, plus a control in
/// which the same record is non-empty.
pub struct GuestState {
    name: String,
    limits: StoreLimits,
    /// Every `logging.log` call, in order.
    pub(crate) log_lines: Vec<(types::LogLevel, String)>,
    /// Every `filesystem.read-file` call that reached the host, in order —
    /// **whether or not it was allowed to succeed**. This is what makes "zero
    /// filesystem access" an observation rather than an absence.
    pub(crate) fs_reads: Vec<String>,
    /// Reads are additionally confined to this subtree when `fs:read` is granted.
    /// `None` refuses every read, while still recording it.
    fs_root: Option<PathBuf>,
    current_tick: u64,
    next_task_id: types::TaskId,
    scheduled_tasks: Vec<ScheduledTask>,
    running_task: Option<types::TaskId>,
    cancel_running_task: bool,
}

impl fmt::Debug for GuestState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestState")
            .field("name", &self.name)
            .field("log_lines", &self.log_lines.len())
            .field("fs_reads", &self.fs_reads)
            .field("fs_root", &self.fs_root)
            .field("current_tick", &self.current_tick)
            .field("scheduled_tasks", &self.scheduled_tasks.len())
            .finish_non_exhaustive()
    }
}

impl types::Host for GuestState {}

impl logging::Host for GuestState {
    fn log(&mut self, level: types::LogLevel, message: String) {
        let name = &self.name;
        match level {
            types::LogLevel::Trace => tracing::trace!(plugin = %name, "{message}"),
            types::LogLevel::Debug => tracing::debug!(plugin = %name, "{message}"),
            types::LogLevel::Info => tracing::info!(plugin = %name, "{message}"),
            types::LogLevel::Warn => tracing::warn!(plugin = %name, "{message}"),
            types::LogLevel::Error => tracing::error!(plugin = %name, "{message}"),
        }
        self.log_lines.push((level, message));
    }
}

impl filesystem::Host for GuestState {
    /// Record first, decide second.
    ///
    /// The recording is unconditional on purpose: a read that is *refused* here
    /// must still be visible to the host operator and to a test, because "a
    /// plugin tried to read `/etc/passwd` and was stopped" is exactly the event
    /// worth knowing about. A guard that returned early would make the refusal
    /// indistinguishable from the plugin never having tried.
    fn read_file(&mut self, path: String) -> Result<Vec<u8>, String> {
        self.fs_reads.push(path.clone());
        let Some(root) = self.fs_root.as_ref() else {
            return Err("no filesystem root is configured for this plugin".to_owned());
        };
        // Second layer, behind the capability: even a granted plugin is confined
        // to a subtree. `canonicalize` on the *candidate* is what defeats `..`;
        // comparing the unresolved path would not.
        let candidate = root.join(path.trim_start_matches('/'));
        let resolved = candidate
            .canonicalize()
            .map_err(|e| format!("{}: {e}", candidate.display()))?;
        let root_resolved = root
            .canonicalize()
            .map_err(|e| format!("{}: {e}", root.display()))?;
        if !resolved.starts_with(&root_resolved) {
            return Err(format!(
                "{} is outside this plugin's filesystem root",
                resolved.display()
            ));
        }
        std::fs::read(&resolved).map_err(|e| format!("{}: {e}", resolved.display()))
    }
}

impl GuestState {
    fn schedule(
        &mut self,
        delay_ticks: u32,
        period_ticks: Option<u32>,
        token: u64,
    ) -> types::TaskId {
        let id = self.next_task_id;
        self.next_task_id = self.next_task_id.wrapping_add(1);
        let delay = u64::from(delay_ticks.max(1));
        self.scheduled_tasks.push(ScheduledTask {
            id,
            due_tick: self.current_tick.saturating_add(delay),
            token,
            period_ticks,
        });
        id
    }

    fn take_next_due_task(&mut self) -> Option<ScheduledTask> {
        let index = self
            .scheduled_tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| task.due_tick <= self.current_tick)
            .min_by_key(|(_, task)| task.id)
            .map(|(index, _)| index)?;
        Some(self.scheduled_tasks.swap_remove(index))
    }
}

impl scheduler::Host for GuestState {
    fn schedule_once(&mut self, delay_ticks: u32, token: u64) -> types::TaskId {
        self.schedule(delay_ticks, None, token)
    }

    fn schedule_repeating(
        &mut self,
        delay_ticks: u32,
        period_ticks: u32,
        token: u64,
    ) -> types::TaskId {
        self.schedule(delay_ticks, Some(period_ticks.max(1)), token)
    }

    fn cancel(&mut self, id: types::TaskId) {
        self.scheduled_tasks.retain(|task| task.id != id);
        if self.running_task == Some(id) {
            self.cancel_running_task = true;
        }
    }
}

/// One instantiated guest: its own `Store` (so its linear memory, and therefore
/// its state, persists across ticks — the host owns dispatch but a guest may
/// keep its own state between calls rather than being purely stateless) and
/// typed handles to its three exports.
pub struct LoadedPlugin {
    name: String,
    info: PluginInfo,
    granted: CapabilitySet,
    store: Store<GuestState>,
    on_tick: TypedFunc<(Vec<Event>,), (Vec<Action>,)>,
    on_task: TypedFunc<(types::TaskId, u64), (Vec<Action>,)>,
    failure: Option<String>,
}

impl fmt::Debug for LoadedPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedPlugin")
            .field("name", &self.name)
            .field("info", &self.info)
            .field("granted", &self.granted)
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}

impl LoadedPlugin {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn info(&self) -> &PluginInfo {
        &self.info
    }

    #[must_use]
    pub fn granted(&self) -> &CapabilitySet {
        &self.granted
    }

    /// `Some` once this guest has trapped, exhausted its fuel, or otherwise
    /// broken its side of the contract. A failed guest is never called again —
    /// the isolation `docs/plans/runtime-plugin-loading.md` names as the wasm
    /// tier's strongest advantage over the native one, where a panicking plugin
    /// takes the process with it.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Everything the guest logged, for tests and for a future in-game plugin
    /// console.
    #[must_use]
    pub fn log_lines(&self) -> &[(types::LogLevel, String)] {
        &self.store.data().log_lines
    }

    /// Every `read-file` the guest attempted. See [`GuestState::fs_reads`].
    #[must_use]
    pub fn attempted_file_reads(&self) -> &[String] {
        &self.store.data().fs_reads
    }

    /// Drive one tick: hand the guest this tick's events, take back its actions.
    ///
    /// Returns an empty slice for a guest that has already failed, so a caller
    /// needs no separate liveness check.
    pub fn tick(&mut self, events: &[Event], fuel: u64) -> Vec<Action> {
        if self.failure.is_some() {
            return Vec::new();
        }
        if let Err(e) = self.store.set_fuel(fuel) {
            self.failure = Some(format!("setting fuel: {e:?}"));
            return Vec::new();
        }
        let next_tick = self.store.data().current_tick.saturating_add(1);
        self.store.data_mut().current_tick = next_tick;
        let mut actions = Vec::new();
        while let Some(task) = self.store.data_mut().take_next_due_task() {
            {
                let state = self.store.data_mut();
                state.running_task = Some(task.id);
                state.cancel_running_task = false;
            }
            match self.on_task.call(&mut self.store, (task.id, task.token)) {
                Ok((task_actions,)) => actions.extend(task_actions),
                Err(e) => {
                    let message = format!("{e:?}");
                    tracing::error!(plugin = %self.name, "plugin task failed and will not be called again: {message}");
                    self.failure = Some(message);
                    return Vec::new();
                }
            }
            let state = self.store.data_mut();
            state.running_task = None;
            if !state.cancel_running_task
                && let Some(period_ticks) = task.period_ticks
            {
                state.scheduled_tasks.push(ScheduledTask {
                    due_tick: state.current_tick.saturating_add(u64::from(period_ticks)),
                    ..task
                });
            }
            state.cancel_running_task = false;
        }
        // No `post_return` call: wasmtime 47 deprecated it as a no-op — the runtime
        // now runs the component's own `post-return` itself. Calling it emits a
        // deprecation warning and does nothing, so a version bump that reinstates
        // the requirement would show up as a *trap*, not a warning. Worth knowing
        // if guests start failing after a wasmtime upgrade.
        match self.on_tick.call(&mut self.store, (events.to_vec(),)) {
            Ok((tick_actions,)) => {
                actions.extend(tick_actions);
                actions
            }
            Err(e) => {
                let message = format!("{e:?}");
                tracing::error!(plugin = %self.name, "plugin failed and will not be called again: {message}");
                self.failure = Some(message);
                Vec::new()
            }
        }
    }
}

/// The host: one engine, one policy, N loaded guests.
pub struct PluginHost {
    engine: Engine,
    policy: CapabilitySet,
    fuel_per_tick: u64,
    memory_limit: usize,
    fs_root: Option<PathBuf>,
    plugins: Vec<LoadedPlugin>,
}

impl fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginHost")
            .field("policy", &self.policy)
            .field("fuel_per_tick", &self.fuel_per_tick)
            .field("memory_limit", &self.memory_limit)
            .field("fs_root", &self.fs_root)
            .field("plugins", &self.plugins)
            .finish()
    }
}

impl PluginHost {
    /// A host granting `policy`.
    ///
    /// Use [`CapabilitySet::default_policy`] unless you mean something else; it
    /// is the "denied unless granted" default `docs/plugin-api.md` promises.
    pub fn new(policy: CapabilitySet) -> Result<Self, HostError> {
        let mut config = Config::new();
        // Fuel is the preemption mechanism — see this module's header for why not
        // epochs (yet).
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|e| HostError::Compile {
            name: "<engine>".to_owned(),
            message: format!("{e:?}"),
        })?;
        Ok(Self {
            engine,
            policy,
            fuel_per_tick: DEFAULT_FUEL_PER_TICK,
            memory_limit: DEFAULT_MEMORY_LIMIT,
            fs_root: None,
            plugins: Vec::new(),
        })
    }

    #[must_use]
    pub fn with_fuel(mut self, fuel_per_tick: u64) -> Self {
        self.fuel_per_tick = fuel_per_tick;
        self
    }

    #[must_use]
    pub fn with_memory_limit(mut self, bytes: usize) -> Self {
        self.memory_limit = bytes;
        self
    }

    /// Confine granted filesystem reads to `root`. Without this, a plugin holding
    /// `fs:read` still reads nothing — the capability is necessary and not
    /// sufficient.
    #[must_use]
    pub fn with_filesystem_root(mut self, root: PathBuf) -> Self {
        self.fs_root = Some(root);
        self
    }

    #[must_use]
    pub fn policy(&self) -> &CapabilitySet {
        &self.policy
    }

    #[must_use]
    pub fn fuel_per_tick(&self) -> u64 {
        self.fuel_per_tick
    }

    #[must_use]
    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    #[must_use]
    pub fn plugins_mut(&mut self) -> &mut [LoadedPlugin] {
        &mut self.plugins
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Load a plugin from a `.wasm` **file on disk**, granting it
    /// `requested ∩ policy` — or refusing outright if `requested` asks for
    /// anything `policy` withholds.
    ///
    /// # The two refusals, and why both exist
    ///
    /// 1. **Declared but not granted.** `requested` names a capability policy
    ///    withholds: [`HostError::CapabilityDenied`], before the module is even
    ///    compiled. This is the polite path — the plugin was honest and the
    ///    operator said no.
    /// 2. **Used but not declared.** The module references an import that is not
    ///    in the `Linker`: [`HostError::Instantiate`], from wasmtime itself. This
    ///    is the path that makes the manifest untrustworthy-but-harmless: **the
    ///    manifest is a declaration, the `Linker` is the enforcement.** A plugin
    ///    that lies about needing no filesystem access does not get filesystem
    ///    access; it gets a load failure.
    ///
    /// # Core modules are accepted
    ///
    /// A plain `cargo build --target wasm32-unknown-unknown` produces a *core
    /// module*, not a component. Requiring `cargo-component` on a plugin author's
    /// PATH to fix that is friction with no security benefit, so this function
    /// sniffs the wasm preamble and runs `wit_component::ComponentEncoder` itself
    /// when it finds one. No WASI adapter is involved or needed: a guest whose
    /// only imports are this world's has nothing for an adapter to satisfy.
    pub fn load_file(
        &mut self,
        name: &str,
        wasm_path: &Path,
        requested: &CapabilitySet,
    ) -> Result<usize, HostError> {
        let missing = requested.missing_from(&self.policy);
        if !missing.is_empty() {
            return Err(HostError::CapabilityDenied {
                name: name.to_owned(),
                missing: missing
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                granted: self.policy.to_string(),
            });
        }
        // `requested ∩ policy`, which after the check above is just `requested`.
        // Written as the intersection anyway so the invariant does not depend on
        // the early return staying where it is.
        let granted: CapabilitySet = requested.iter().filter(|c| self.policy.contains(*c)).collect();

        let bytes = std::fs::read(wasm_path).map_err(|e| HostError::Io {
            path: wasm_path.to_path_buf(),
            source: e,
        })?;
        let component_bytes = to_component(wasm_path, &bytes)?;

        let component =
            Component::new(&self.engine, &component_bytes).map_err(|e| HostError::Compile {
                name: name.to_owned(),
                message: format!("{e:?}"),
            })?;

        let mut linker: Linker<GuestState> = Linker::new(&self.engine);
        // **This block is the capability gate.** Every `if` here is a security
        // boundary; an interface added unconditionally is a capability granted to
        // everything, silently.
        if granted.contains(Capability::Log) {
            logging::add_to_linker::<_, wasmtime::component::HasSelf<_>>(&mut linker, |s| s)
                .map_err(|e| HostError::Compile {
                    name: name.to_owned(),
                    message: format!("linking logging: {e:?}"),
                })?;
        }
        if granted.contains(Capability::FsRead) {
            filesystem::add_to_linker::<_, wasmtime::component::HasSelf<_>>(&mut linker, |s| s)
                .map_err(|e| HostError::Compile {
                    name: name.to_owned(),
                    message: format!("linking filesystem: {e:?}"),
                })?;
        }
        if granted.contains(Capability::ScheduleTasks) {
            scheduler::add_to_linker::<_, wasmtime::component::HasSelf<_>>(&mut linker, |s| s)
                .map_err(|e| HostError::Compile {
                    name: name.to_owned(),
                    message: format!("linking scheduler: {e:?}"),
                })?;
        }

        let state = GuestState {
            name: name.to_owned(),
            limits: StoreLimitsBuilder::new()
                .memory_size(self.memory_limit)
                // **Not 1.** A *component* is not one core instance: wasmtime
                // instantiates the guest's core module plus the adapter shims the
                // component model generates for its imports and exports, so a
                // single plugin lands at two or more. `instances(1)` fails with
                // "resource limit exceeded: instance count too high at 2" — a
                // message that reads like a runaway guest and is actually a host
                // misconfiguration. This bound exists to stop a guest
                // instantiating *others*, and the number should stay comfortably
                // above what one plugin needs rather than tight against it.
                .instances(MAX_CORE_INSTANCES_PER_PLUGIN)
                .build(),
            log_lines: Vec::new(),
            fs_reads: Vec::new(),
            fs_root: if granted.contains(Capability::FsRead) {
                self.fs_root.clone()
            } else {
                None
            },
            current_tick: 0,
            next_task_id: 0,
            scheduled_tasks: Vec::new(),
            running_task: None,
            cancel_running_task: false,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        store
            .set_fuel(self.fuel_per_tick)
            .map_err(|e| HostError::Compile {
                name: name.to_owned(),
                message: format!("setting initial fuel: {e:?}"),
            })?;

        let instance =
            linker
                .instantiate(&mut store, &component)
                .map_err(|e| HostError::Instantiate {
                    name: name.to_owned(),
                    granted: granted.to_string(),
                    message: format!("{e:?}"),
                })?;

        let init = instance
            .get_typed_func::<(), (PluginInfo,)>(&mut store, "init")
            .map_err(|e| HostError::MissingExport {
                name: name.to_owned(),
                export: "init".to_owned(),
                message: format!("{e:?}"),
            })?;
        let on_tick = instance
            .get_typed_func::<(Vec<Event>,), (Vec<Action>,)>(&mut store, "on-tick")
            .map_err(|e| HostError::MissingExport {
                name: name.to_owned(),
                export: "on-tick".to_owned(),
                message: format!("{e:?}"),
            })?;
        let on_task = instance
            .get_typed_func::<(types::TaskId, u64), (Vec<Action>,)>(&mut store, "on-task")
            .map_err(|e| HostError::MissingExport {
                name: name.to_owned(),
                export: "on-task".to_owned(),
                message: format!("{e:?}"),
            })?;

        let (info,) = init.call(&mut store, ()).map_err(|e| HostError::Trap {
            name: name.to_owned(),
            export: "init".to_owned(),
            message: format!("{e:?}"),
        })?;
        if info.abi != ABI_WORLD {
            return Err(HostError::AbiMismatch {
                name: name.to_owned(),
                found: info.abi,
                expected: ABI_WORLD.to_owned(),
            });
        }

        tracing::info!(
            plugin = %name,
            version = %info.version,
            capabilities = %granted,
            "loaded wasm plugin"
        );
        self.plugins.push(LoadedPlugin {
            name: name.to_owned(),
            info,
            granted,
            store,
            on_tick,
            on_task,
            failure: None,
        });
        Ok(self.plugins.len() - 1)
    }

    /// Load a plugin from its `plugin.toml`, which names both the module and the
    /// capabilities it requests.
    ///
    /// This is the route a real installation takes; [`Self::load_file`] is the one
    /// underneath it, and stays public because a test — or a consumer embedding a
    /// plugin it wrote itself — has no need of a manifest file on disk.
    ///
    /// # Which name wins, and why it is the manifest's
    ///
    /// A module's `init` reports a name too, and the two can disagree. **The manifest
    /// is authoritative** and a disagreement is a warning, not a refusal. That was
    /// not the first design: making it fatal seemed like a cheap way to catch a
    /// `plugin.toml` copied next to the wrong `.wasm`. It also forbids installing the
    /// *same* plugin twice under two names — two configured instances at different
    /// priority tiers, which is an entirely reasonable thing to want and which this
    /// crate's own load-order test needed on the first attempt. Since both strings
    /// are equally attacker-controlled, a fatal check bought no security either; it
    /// only removed a capability. So: the manifest names the plugin (it is what the
    /// operator wrote and what the logs and errors use),
    /// [`LoadedPlugin::info`]`().name` keeps the module's own claim, and the mismatch
    /// is logged so a genuinely mis-copied manifest is still visible.
    pub fn load_manifest(&mut self, manifest_path: &Path) -> Result<usize, LoadError> {
        let manifest = crate::manifest::Manifest::load(manifest_path)?;
        let module = manifest.resolved_module(manifest_path)?;
        let requested = manifest.requested_capabilities()?;
        let index = self.load_file(&manifest.name, &module, &requested)?;
        let reported = &self.plugins[index].info.name;
        if *reported != manifest.name {
            tracing::warn!(
                plugin = %manifest.name,
                module_reports = %reported,
                path = %module.display(),
                "manifest name and module name disagree; using the manifest's. If this was not \
                 deliberate, the plugin.toml may be sitting next to the wrong .wasm"
            );
        }
        Ok(index)
    }

    /// Load every plugin under `dir` — one subdirectory per plugin, each with a
    /// `plugin.toml` — in [`crate::manifest::scan_directory`]'s deterministic
    /// priority-then-name order, and return one result per plugin found.
    ///
    /// One plugin's failure never stops another's: the operator gets every problem at
    /// once, and a single malformed manifest in a `plugins/` directory does not take
    /// the working plugins down with it. A missing directory is simply no plugins,
    /// which is the normal case for a fresh install.
    pub fn load_directory(&mut self, dir: &Path) -> Vec<Result<usize, LoadError>> {
        crate::manifest::scan_directory(dir)
            .into_iter()
            .map(|found| {
                let (path, _manifest) = found?;
                self.load_manifest(&path)
            })
            .collect()
    }

    /// Drive every loaded guest once, in load order, concatenating their actions.
    ///
    /// Load order *is* priority order once a manifest has been through
    /// [`crate::manifest`]'s sort; this function does not re-sort, so that the one
    /// place ordering is decided stays the loader.
    pub fn tick_all(&mut self, events: &[Event]) -> Vec<Action> {
        let fuel = self.fuel_per_tick;
        let mut out = Vec::new();
        for plugin in &mut self.plugins {
            out.extend(plugin.tick(events, fuel));
        }
        out
    }
}

/// Whether `bytes` is already a component, per the wasm preamble.
///
/// A core module's 8-byte preamble is `\0asm` followed by version `1` as a
/// little-endian `u32`; a component's is `\0asm` followed by version `0x0d` and
/// **layer `1`** in the high half — so byte 6 is the discriminant. Checked by
/// hand rather than by adding a `wasmparser` dependency for eight bytes.
fn is_component(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[..4] == *b"\0asm" && bytes[6] == 1
}

fn to_component(path: &Path, bytes: &[u8]) -> Result<Vec<u8>, HostError> {
    if bytes.len() < 8 || bytes[..4] != *b"\0asm" {
        return Err(HostError::NotWasm {
            path: path.to_path_buf(),
            found: bytes.iter().take(8).copied().collect(),
        });
    }
    if is_component(bytes) {
        return Ok(bytes.to_vec());
    }
    wit_component::ComponentEncoder::default()
        .module(bytes)
        .and_then(|e| e.validate(true).encode())
        .map_err(|e| HostError::Encode {
            path: path.to_path_buf(),
            message: format!("{e:?}"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh host has no plugins and grants no filesystem access — the two
    /// facts every other test's control depends on.
    #[test]
    fn a_fresh_host_is_empty_and_grants_no_filesystem_access() {
        let host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
        assert!(host.is_empty());
        assert!(!host.policy().contains(Capability::FsRead));
    }

    /// The preamble sniffer, against both real shapes.
    #[test]
    fn the_preamble_sniffer_separates_modules_from_components() {
        let core = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        let component = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];
        assert!(!is_component(&core));
        assert!(is_component(&component));
        assert!(!is_component(b"\0as"));
    }

    /// Non-wasm bytes are refused by name, rather than reaching wasmtime as a
    /// confusing compile error.
    #[test]
    fn a_non_wasm_file_is_refused_with_its_first_bytes_quoted() {
        let err = to_component(Path::new("/tmp/not.wasm"), b"#!/bin/sh\necho hi\n")
            .expect_err("must refuse");
        let text = err.to_string();
        assert!(text.contains("is not WebAssembly"), "{text}");
    }

    /// Requesting a capability policy withholds is refused *before* any module is
    /// read, and the message names the capability.
    #[test]
    fn requesting_an_ungranted_capability_is_refused_by_name() {
        let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
        let err = host
            .load_file(
                "liar",
                // Deliberately a path that does not exist: the capability check
                // must come first, which is exactly what this asserts.
                Path::new("/nonexistent/plugin.invalid.wasm"),
                &CapabilitySet::from_iter([Capability::FsRead]),
            )
            .expect_err("must refuse");
        let text = err.to_string();
        assert!(text.contains("fs:read"), "{text}");
        assert!(
            matches!(err, HostError::CapabilityDenied { .. }),
            "expected CapabilityDenied, got {err:?}"
        );
    }
}
