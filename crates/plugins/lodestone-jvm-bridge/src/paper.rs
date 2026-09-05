//! Paper bootstrap and plugin-descriptor discovery for an operator-supplied set of jars.
//!
//! This module opens jars but never extracts them: descriptor names are exact
//! archive lookups, descriptor reads are bounded, and every selected path stays
//! an operator path. Discovery happens before JVM startup, so invalid metadata
//! fails without loading arbitrary plugin bytecode or touching a world port.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use zip::ZipArchive;

#[cfg(feature = "jvm")]
use jni::objects::{Global, JClass, JObject};
#[cfg(feature = "jvm")]
use jni::Env;
#[cfg(feature = "jvm")]
use crate::runtime::{JvmConfig, JvmRuntime};
#[cfg(feature = "jvm")]
use crate::native_surface;
#[cfg(feature = "jvm")]
use crate::adapter::NativeBlockStateSurface;

const BOOTSTRAP_CLASS: &str = "io.papermc.paper.PaperBootstrap";
const BOOTSTRAP_ENTRY: &str = "io/papermc/paper/PaperBootstrap.class";
const PAPER_MANIFEST_TITLE: &str = "Implementation-Title: Paper";
const PAPER_DESCRIPTOR: &str = "paper-plugin.yml";
const BUKKIT_DESCRIPTOR: &str = "plugin.yml";
const MAX_DESCRIPTOR_BYTES: u64 = 64 * 1024;
const DEFAULT_MAX_PLUGINS: usize = 256;
const CLASS_MAGIC: [u8; 4] = [0xCA, 0xFE, 0xBA, 0xBE];
const END_OF_CENTRAL_DIRECTORY_SIGNATURE: [u8; 4] = [0x50, 0x4B, 0x05, 0x06];
const CENTRAL_DIRECTORY_SIGNATURE: [u8; 4] = [0x50, 0x4B, 0x01, 0x02];
const END_OF_CENTRAL_DIRECTORY_BYTES: u64 = 22;
const MAX_END_OF_CENTRAL_DIRECTORY_SEARCH: u64 = END_OF_CENTRAL_DIRECTORY_BYTES + u16::MAX as u64;

/// Operator paths needed to inspect one Paper server jar and its plugin jars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperBootstrapConfig {
    paper_jar: PathBuf,
    plugins_directory: PathBuf,
    shim_paths: Vec<PathBuf>,
    native_shim: bool,
    max_plugins: usize,
}

impl PaperBootstrapConfig {
    /// Starts a plan for this explicit Paper jar and plugin directory.
    #[must_use]
    pub fn new(paper_jar: impl AsRef<Path>, plugins_directory: impl AsRef<Path>) -> Self {
        Self {
            paper_jar: paper_jar.as_ref().to_owned(),
            plugins_directory: plugins_directory.as_ref().to_owned(),
            shim_paths: Vec::new(),
            native_shim: false,
            max_plugins: DEFAULT_MAX_PLUGINS,
        }
    }

    /// Adds a directory or jar before the Paper jar in bootstrap resolution.
    ///
    /// Shim ordering is deliberate: the isolated loader resolves a matching
    /// class from these paths before the Paper jar, without modifying either.
    #[must_use]
    pub fn with_shim_path(mut self, path: impl AsRef<Path>) -> Self {
        self.shim_paths.push(path.as_ref().to_owned());
        self
    }

    /// Requires the bridge's narrow native surface from the configured shim paths.
    ///
    /// This does not claim a plugin API. It asks every fresh lifecycle loader
    /// to resolve `lodestone.bridge.IsolatedPaperShim`, validate its one static
    /// native declaration, and register it before that loader sees a bootstrap
    /// or plugin entry class.
    #[must_use]
    pub fn with_isolated_native_shim(mut self) -> Self {
        self.native_shim = true;
        self
    }

    /// Limits discovered plugin jars before opening any descriptor.
    #[must_use]
    pub fn with_max_plugins(mut self, max_plugins: usize) -> Self {
        self.max_plugins = max_plugins;
        self
    }

    /// Validates the server jar and returns sorted, metadata-checked plugins.
    pub fn discover(self) -> Result<PaperBootstrapPlan, PaperBootstrapError> {
        if self.max_plugins == 0 {
            return Err(PaperBootstrapError::new("Paper plugin limit must be positive"));
        }
        if self.native_shim && self.shim_paths.is_empty() {
            return Err(PaperBootstrapError::new(
                "an isolated native shim requires at least one shim path",
            ));
        }
        validate_paper_jar(&self.paper_jar)?;
        for shim in &self.shim_paths {
            validate_operator_path(shim, "shim")?;
        }
        let plugin_jars = plugin_jars(&self.plugins_directory, self.max_plugins)?;
        let mut names = BTreeSet::new();
        let mut plugins = Vec::with_capacity(plugin_jars.len());
        for jar in plugin_jars {
            let descriptor = discover_plugin(&jar)?;
            let key = descriptor.name.to_ascii_lowercase();
            if !names.insert(key) {
                return Err(PaperBootstrapError::new(format!(
                    "duplicate plugin name {:?} in {}",
                    descriptor.name,
                    jar.display()
                )));
            }
            plugins.push(descriptor);
        }
        Ok(PaperBootstrapPlan {
            paper_jar: self.paper_jar,
            shim_paths: self.shim_paths,
            native_shim: self.native_shim,
            plugins,
        })
    }
}

/// A validated, deterministic input set for a later Paper lifecycle host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperBootstrapPlan {
    paper_jar: PathBuf,
    shim_paths: Vec<PathBuf>,
    native_shim: bool,
    plugins: Vec<PaperPluginDescriptor>,
}

/// The only lifecycle operations a server-owned Java-plugin host may perform.
///
/// A descriptor first becomes `Loaded`; only a loaded plugin may later be
/// enabled, and only an enabled plugin may later be disabled. The bridge does
/// not expose enable or disable callbacks yet: invoking either would run
/// arbitrary plugin code before the compatible server API exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperPluginLifecycleStep {
    Load,
    Enable,
    Disable,
}

/// The durable phase of one descriptor-driven plugin entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperPluginLifecyclePhase {
    Discovered,
    Loaded,
    Enabled,
    Disabled,
    Failed,
}

impl PaperPluginLifecyclePhase {
    /// Whether this phase permits the next lifecycle operation.
    pub fn accepts(self, step: PaperPluginLifecycleStep) -> bool {
        matches!(
            (self, step),
            (Self::Discovered, PaperPluginLifecycleStep::Load)
                | (Self::Loaded, PaperPluginLifecycleStep::Enable)
                | (Self::Enabled, PaperPluginLifecycleStep::Disable)
        )
    }
}

/// A bounded failure recorded against one lifecycle entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperPluginLifecycleFailure {
    step: PaperPluginLifecycleStep,
    message: String,
}

impl PaperPluginLifecycleFailure {
    fn load(message: impl Into<String>) -> Self {
        Self {
            step: PaperPluginLifecycleStep::Load,
            message: message.into(),
        }
    }

    /// The operation that failed.
    pub fn step(&self) -> PaperPluginLifecycleStep {
        self.step
    }

    /// The bounded host diagnostic for this one entry.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Observable lifecycle state for one validated plugin descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperPluginLifecycleStatus {
    descriptor: PaperPluginDescriptor,
    phase: PaperPluginLifecyclePhase,
    failure: Option<PaperPluginLifecycleFailure>,
}

impl PaperPluginLifecycleStatus {
    fn discovered(descriptor: PaperPluginDescriptor) -> Self {
        Self {
            descriptor,
            phase: PaperPluginLifecyclePhase::Discovered,
            failure: None,
        }
    }

    fn loaded(&mut self) {
        debug_assert!(self.phase.accepts(PaperPluginLifecycleStep::Load));
        self.phase = PaperPluginLifecyclePhase::Loaded;
    }

    fn failed_to_load(&mut self, message: impl Into<String>) {
        debug_assert!(self.phase.accepts(PaperPluginLifecycleStep::Load));
        self.phase = PaperPluginLifecyclePhase::Failed;
        self.failure = Some(PaperPluginLifecycleFailure::load(message));
    }

    /// The validated descriptor that supplies this entry's identity.
    pub fn descriptor(&self) -> &PaperPluginDescriptor {
        &self.descriptor
    }

    /// The entry's completed lifecycle phase.
    pub fn phase(&self) -> PaperPluginLifecyclePhase {
        self.phase
    }

    /// The entry-local failure, if its `Load` attempt was isolated and failed.
    pub fn failure(&self) -> Option<&PaperPluginLifecycleFailure> {
        self.failure.as_ref()
    }
}

/// A server-owned snapshot of the descriptor lifecycle.
///
/// Bootstrap remains a process-wide prerequisite, so its load error is
/// terminal. Plugin entry errors are instead recorded on the affected
/// descriptor and do not prevent later isolated loaders from being checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperPluginLifecycleStatusSet {
    plugins: Vec<PaperPluginLifecycleStatus>,
}

impl PaperPluginLifecycleStatusSet {
    fn discovered(plugins: &[PaperPluginDescriptor]) -> Self {
        Self {
            plugins: plugins.iter().cloned().map(PaperPluginLifecycleStatus::discovered).collect(),
        }
    }

    fn loaded(&mut self, index: usize) {
        self.plugins[index].loaded();
    }

    fn failed_to_load(&mut self, index: usize, message: impl Into<String>) {
        self.plugins[index].failed_to_load(message);
    }

    /// Statuses in deterministic descriptor discovery order.
    pub fn plugins(&self) -> &[PaperPluginLifecycleStatus] {
        &self.plugins
    }
}

/// A concrete Java-facing capability supplied by the hosting server.
///
/// This is deliberately an input, not a claim that a Bukkit `Server` object
/// exists. `NativeBlockStateRead` means the host has installed the one native
/// `blockStateId` declaration in every isolated loader and will service its
/// requests from live server state. It is useful to retain as an exact, real
/// Java-facing capability, but cannot safely construct a plugin: construction
/// also needs loader-owned plugin metadata and a much broader server facade.
#[derive(Debug)]
pub enum PaperServerFacadeInput {
    /// No Java-facing server capability is available to a retained loader.
    Unavailable,
    /// The isolated native shim can read one loaded block-state ID through its
    /// owning adapter worker's request port.
    #[cfg(feature = "jvm")]
    NativeBlockStateRead(NativeBlockStateSurface),
}

impl PaperServerFacadeInput {
    /// Consumes the worker-owned native surface for one retained lifecycle.
    ///
    /// The surface token can originate only from `AdapterHost::start_with_setup`.
    /// Its matching `AdapterHost::service_pending` call is therefore the live
    /// dedicated-server producer, rather than an enum claim a lifecycle host
    /// can manufacture while no native query can be answered.
    #[cfg(feature = "jvm")]
    pub fn native_block_state_read(surface: NativeBlockStateSurface) -> Self {
        Self::NativeBlockStateRead(surface)
    }

    fn state(&self) -> PaperServerFacadeState {
        match self {
            Self::Unavailable => PaperServerFacadeState::Unavailable,
            #[cfg(feature = "jvm")]
            Self::NativeBlockStateRead(_) => PaperServerFacadeState::NativeBlockStateRead,
        }
    }

    fn construction_blocker(&self) -> PaperPluginConstructionBlocker {
        match self {
            Self::Unavailable => PaperPluginConstructionBlocker::ServerFacadeUnavailable,
            #[cfg(feature = "jvm")]
            Self::NativeBlockStateRead(_) => {
                PaperPluginConstructionBlocker::PluginConstructionUnsupported
            }
        }
    }
}

/// The server-owned API prerequisite state for constructing a loaded plugin entry.
///
/// Loading a class proves only that its private loader can resolve it. Before
/// construction, the entry also needs a server facade which can truthfully
/// answer the API calls its constructor may make. A narrow native capability is
/// observable here without being mistaken for that complete facade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperServerFacadeState {
    /// No compatible server facade has been supplied to the retained loader.
    Unavailable,
    /// The private loader has only the native read-only block-state seam.
    NativeBlockStateRead,
}

impl fmt::Display for PaperServerFacadeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("no compatible server facade is installed"),
            Self::NativeBlockStateRead => {
                formatter.write_str("the isolated native block-state read seam is installed")
            }
        }
    }
}

/// Why one descriptor may not yet be constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperPluginConstructionBlocker {
    /// The entry loaded, but its loader has no compatible server facade.
    ServerFacadeUnavailable,
    /// A narrow Java-facing capability exists, but cannot construct a plugin.
    PluginConstructionUnsupported,
    /// The isolated entry load failed, so there is no retained class to construct.
    EntryLoadFailed,
}

impl fmt::Display for PaperPluginConstructionBlocker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServerFacadeUnavailable => {
                formatter.write_str("no compatible server facade is installed")
            }
            Self::PluginConstructionUnsupported => formatter.write_str(
                "the installed server capability cannot supply plugin construction semantics",
            ),
            Self::EntryLoadFailed => formatter.write_str("the isolated plugin entry did not load"),
        }
    }
}

/// Descriptor-backed construction state for one plugin entry.
///
/// The descriptor supplies the future plugin identity and description. It is
/// kept separate from a Java object because calling a constructor would run
/// operator code before the prerequisite state is real.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperPluginConstructionStatus {
    descriptor: PaperPluginDescriptor,
    blocker: PaperPluginConstructionBlocker,
}

impl PaperPluginConstructionStatus {
    /// The validated identity and description for this future construction.
    pub fn descriptor(&self) -> &PaperPluginDescriptor {
        &self.descriptor
    }

    /// The explicit reason no constructor may run yet.
    pub fn blocker(&self) -> PaperPluginConstructionBlocker {
        self.blocker
    }
}

/// The construction precondition snapshot paired with retained lifecycle loaders.
///
/// This records every descriptor, including failed loads, so an operator can
/// distinguish a bad entry jar, no facade, and a deliberately narrow facade
/// input. No variant currently permits construction: adding one requires a
/// real server-owned facade attached to the same retained loader as its entry
/// class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperPluginConstructionReadiness {
    facade: PaperServerFacadeState,
    lifecycle: PaperPluginLifecycleStatusSet,
    plugins: Vec<PaperPluginConstructionStatus>,
}

impl PaperPluginConstructionReadiness {
    fn from_lifecycle(
        status: &PaperPluginLifecycleStatusSet,
        facade_input: &PaperServerFacadeInput,
    ) -> Self {
        Self {
            facade: facade_input.state(),
            lifecycle: status.clone(),
            plugins: status.plugins().iter().map(|plugin| PaperPluginConstructionStatus {
                descriptor: plugin.descriptor().clone(),
                blocker: if plugin.phase() == PaperPluginLifecyclePhase::Loaded {
                    facade_input.construction_blocker()
                } else {
                    PaperPluginConstructionBlocker::EntryLoadFailed
                },
            }).collect(),
        }
    }

    /// The server facade state applied to every retained entry loader.
    pub fn facade(&self) -> PaperServerFacadeState {
        self.facade
    }

    /// The original per-descriptor Load result and diagnostic.
    pub fn lifecycle(&self) -> &PaperPluginLifecycleStatusSet {
        &self.lifecycle
    }

    /// Construction state in deterministic descriptor discovery order.
    pub fn plugins(&self) -> &[PaperPluginConstructionStatus] {
        &self.plugins
    }
}

/// Loader state retained after non-initializing lifecycle entry loading.
///
/// Each global reference owns one fresh isolated loader. Keeping those loaders
/// and their non-initialized entry classes alive preserves the definitions
/// selected from private classpaths and the native registration installed on a
/// shim definition. It retains no plugin object: construction and enablement
/// remain a later, server-lifecycle-owned decision.
#[cfg(feature = "jvm")]
pub struct PaperLifecycleLoad {
    bootstrap_loader: Global<JObject<'static>>,
    plugins: Vec<PaperLoadedPlugin>,
    status: PaperPluginLifecycleStatusSet,
    native_shim: bool,
}

/// Retained lifecycle classes plus the construction prerequisites that govern them.
///
/// Owning the lifecycle load here keeps the non-initialized class and its fresh
/// loader alive for the same worker lifetime as the construction snapshot.
    /// The snapshot currently blocks every constructor explicitly, whether its
    /// loader lacks a facade or retains only the narrow native read surface.
#[cfg(feature = "jvm")]
pub struct PaperPluginConstructionPlan {
    lifecycle: PaperLifecycleLoad,
    readiness: PaperPluginConstructionReadiness,
    _facade_input: PaperServerFacadeInput,
}

/// One successfully loaded plugin entry, kept with its identity and loader.
///
/// The global references are intentionally private. Later lifecycle work must
/// add a server-owned API state before it can construct from the retained entry
/// class or invoke plugin code.
#[cfg(feature = "jvm")]
pub struct PaperLoadedPlugin {
    descriptor: PaperPluginDescriptor,
    loader: Global<JObject<'static>>,
    entry_class: Global<JClass<'static>>,
}

#[cfg(feature = "jvm")]
impl fmt::Debug for PaperLoadedPlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PaperLoadedPlugin")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "jvm")]
impl fmt::Debug for PaperLifecycleLoad {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PaperLifecycleLoad")
            .field("loader_count", &self.loader_count())
            .field("loaded_plugins", &self.plugins.len())
            .field("status", &self.status)
            .finish()
    }
}

#[cfg(feature = "jvm")]
impl fmt::Debug for PaperPluginConstructionPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PaperPluginConstructionPlan")
            .field("loader_count", &self.lifecycle.loader_count())
            .field("readiness", &self.readiness)
            .finish()
    }
}

#[cfg(feature = "jvm")]
impl PaperLifecycleLoad {
    /// Number of isolated loaders retained by this successful lifecycle load.
    pub fn loader_count(&self) -> usize {
        1 + self.plugins.len()
    }

    /// Confirms that the bootstrap loader remains owned with this lifecycle.
    pub fn retains_bootstrap_loader(&self) -> bool {
        let _ = &self.bootstrap_loader;
        true
    }

    /// Per-descriptor load outcomes, including isolated failures.
    pub fn status(&self) -> &PaperPluginLifecycleStatusSet {
        &self.status
    }

    /// Successfully loaded entries, each retained beside its private loader.
    pub fn loaded_plugins(&self) -> &[PaperLoadedPlugin] {
        &self.plugins
    }

    /// Retains this lifecycle load with a server-owned Java-facing input.
    ///
    /// This consumes the raw lifecycle owner so later code cannot separate a
    /// construction description from the loader and class it describes. The
    /// native read input is accepted only when that same lifecycle installed
    /// its declaration in every private loader. It does not invoke any Java
    /// constructor or callback.
    pub fn into_construction_plan(
        self,
        facade_input: PaperServerFacadeInput,
    ) -> Result<PaperPluginConstructionPlan, PaperBootstrapError> {
        validate_construction_facade(self.native_shim, &facade_input)?;
        let readiness = PaperPluginConstructionReadiness::from_lifecycle(
            &self.status,
            &facade_input,
        );
        Ok(PaperPluginConstructionPlan {
            lifecycle: self,
            readiness,
            _facade_input: facade_input,
        })
    }
}

#[cfg(feature = "jvm")]
impl PaperPluginConstructionPlan {
    /// The observable prerequisite state for these retained entry classes.
    pub fn readiness(&self) -> &PaperPluginConstructionReadiness {
        &self.readiness
    }

    /// Number of loaders kept alive beside the blocked construction entries.
    pub fn loader_count(&self) -> usize {
        self.lifecycle.loader_count()
    }
}

#[cfg(feature = "jvm")]
impl PaperLoadedPlugin {
    /// The descriptor identity associated with this loader and entry class.
    pub fn descriptor(&self) -> &PaperPluginDescriptor {
        &self.descriptor
    }

    /// Confirms that this descriptor remains associated with its loader and class.
    pub fn retains_entry_association(&self) -> bool {
        let _ = (&self.loader, &self.entry_class);
        true
    }
}

impl PaperBootstrapPlan {
    /// The user-supplied Paper server jar selected for this plan.
    pub fn paper_jar(&self) -> &Path {
        &self.paper_jar
    }

    /// Validated plugins in stable jar-path order.
    pub fn plugins(&self) -> &[PaperPluginDescriptor] {
        &self.plugins
    }

    /// Whether lifecycle loaders must install the bridge's isolated native shim.
    pub fn requires_isolated_native_shim(&self) -> bool {
        self.native_shim
    }

    /// Starts the JVM without placing operator jars on its system classpath.
    ///
    /// [`Self::load_lifecycle_entries_in_runtime`] supplies the ordered
    /// operator paths to isolated loaders instead. Keeping the system loader
    /// empty prevents an accidental system-loader lookup from defeating
    /// shim-first resolution.
    #[cfg(feature = "jvm")]
    pub fn start_runtime(&self) -> Result<JvmRuntime, PaperBootstrapError> {
        JvmRuntime::start(&JvmConfig::new()).map_err(|error| {
            PaperBootstrapError::new(format!("could not start Paper JVM: {error}"))
        })
    }

    /// Requests every validated entry class through its own isolated loader.
    ///
    /// The callback is invoked first for the server bootstrap and then once
    /// per plugin in discovery order. Each plugin request contains shims, the
    /// server jar, and only that plugin's jar, in that order. The callback must
    /// use a fresh isolated loader for every request; this prevents a plugin
    /// from making its implementation classes visible to another plugin.
    ///
    /// The seam is JVM-independent so hosts can prove their ordering and error
    /// policy without starting a JVM. A successful callback means only that a
    /// class was loaded without initialization. Bootstrap failure remains
    /// terminal, but an individual plugin failure is retained in the returned
    /// status set and later plugins still receive their isolated `Load` check.
    /// It does not construct a plugin, invoke an entry point, initialize Paper,
    /// or establish API compatibility.
    pub fn load_lifecycle_entries<E>(
        &self,
        mut load_class: impl FnMut(&[PathBuf], &str) -> Result<(), E>,
    ) -> Result<PaperPluginLifecycleStatusSet, PaperBootstrapError>
    where
        E: fmt::Display,
    {
        let bootstrap_paths = self.loader_paths(None);
        load_class(&bootstrap_paths, BOOTSTRAP_CLASS).map_err(|error| {
            PaperBootstrapError::new(format!(
                "could not load Paper bootstrap class {BOOTSTRAP_CLASS}: {error}"
            ))
        })?;
        let mut status = PaperPluginLifecycleStatusSet::discovered(&self.plugins);
        for (index, plugin) in self.plugins.iter().enumerate() {
            let plugin_paths = self.loader_paths(Some(plugin.jar()));
            match load_class(&plugin_paths, plugin.main_class()) {
                Ok(()) => status.loaded(index),
                Err(error) => status.failed_to_load(index, format!(
                    "could not load plugin {:?} entry class {}: {error}",
                    plugin.name(),
                    plugin.main_class(),
                )),
            }
        }
        Ok(status)
    }

    fn loader_paths(&self, plugin_jar: Option<&Path>) -> Vec<PathBuf> {
        let mut paths = self.shim_paths.clone();
        paths.push(self.paper_jar.clone());
        if let Some(plugin_jar) = plugin_jar {
            paths.push(plugin_jar.to_owned());
        }
        paths
    }

    /// Loads every lifecycle entry through fresh JVM isolated loaders.
    ///
    /// This is the real JVM consumer of [`Self::load_lifecycle_entries`]. It
    /// deliberately calls `ClassLoader.loadClass`, which does not initialize
    /// the loaded class. If requested, it first validates and registers the
    /// bridge's one native shim member in that *same* fresh loader. It retains
    /// the non-initialized entry class with its loader and descriptor. Plugin
    /// construction, enablement, and event dispatch are later lifecycle work.
    #[cfg(feature = "jvm")]
    pub fn load_lifecycle_entries_in_runtime<'local>(
        &self,
        runtime: &JvmRuntime,
        env: &mut Env<'local>,
    ) -> Result<PaperLifecycleLoad, PaperBootstrapError> {
        let bootstrap_paths = self.loader_paths(None);
        let (bootstrap_loader, _) = self.load_one_entry_in_runtime(runtime, env, &bootstrap_paths, BOOTSTRAP_CLASS)
            .map_err(|error| PaperBootstrapError::lifecycle(
                format!("could not load Paper bootstrap class {BOOTSTRAP_CLASS}"),
                error,
            ))?;
        let mut status = PaperPluginLifecycleStatusSet::discovered(&self.plugins);
        let mut plugins = Vec::with_capacity(self.plugins.len());
        for (index, plugin) in self.plugins.iter().enumerate() {
            let plugin_paths = self.loader_paths(Some(plugin.jar()));
            match self.load_one_entry_in_runtime(runtime, env, &plugin_paths, plugin.main_class()) {
                Ok((loader, entry_class)) => {
                    status.loaded(index);
                    plugins.push(PaperLoadedPlugin {
                        descriptor: plugin.clone(),
                        loader,
                        entry_class,
                    });
                }
                Err(error) => status.failed_to_load(index, format!(
                    "could not load plugin {:?} entry class {}: {error}",
                    plugin.name(),
                    plugin.main_class(),
                )),
            }
        }
        Ok(PaperLifecycleLoad {
            bootstrap_loader,
            plugins,
            status,
            native_shim: self.native_shim,
        })
    }

    #[cfg(feature = "jvm")]
    fn load_one_entry_in_runtime<'local>(
        &self,
        runtime: &JvmRuntime,
        env: &mut Env<'local>,
        paths: &[PathBuf],
        binary_name: &str,
    ) -> Result<(Global<JObject<'static>>, Global<JClass<'static>>), PaperBootstrapError> {
        let config = paths.iter().fold(JvmConfig::new(), |config, path| {
            config.with_classpath(path)
        });
        let native_error = std::cell::RefCell::new(None);
        let loaded = runtime.with_isolated_loader(env, &config, |env, loader| {
            if self.native_shim {
                if let Err(error) = native_surface::install_in_loader(runtime, env, loader) {
                    *native_error.borrow_mut() = Some(error.clone());
                    return Err(crate::runtime::JvmError::new(error.to_string()));
                }
            }
            let entry_class = runtime.load_class_from_loader(env, loader, binary_name)?;
            let loader = env.new_global_ref(loader).map_err(crate::runtime::JvmError::from)?;
            let entry_class = env.new_global_ref(entry_class).map_err(crate::runtime::JvmError::from)?;
            Ok((loader, entry_class))
        });
        if let Some(error) = native_error.into_inner() {
            return Err(PaperBootstrapError::native_surface(error));
        }
        loaded.map_err(|error| PaperBootstrapError::new(error.to_string()))
    }
}

fn validate_construction_facade(
    native_shim: bool,
    facade_input: &PaperServerFacadeInput,
) -> Result<(), PaperBootstrapError> {
    match (native_shim, facade_input) {
        (false, PaperServerFacadeInput::Unavailable) => Ok(()),
        #[cfg(feature = "jvm")]
        (true, PaperServerFacadeInput::NativeBlockStateRead(_)) => Ok(()),
        #[cfg(feature = "jvm")]
        (false, PaperServerFacadeInput::NativeBlockStateRead(_)) => Err(PaperBootstrapError::new(
            "the native block-state facade input requires an isolated native shim in every loader",
        )),
        (true, PaperServerFacadeInput::Unavailable) => Err(PaperBootstrapError::new(
            "an isolated native shim requires the adapter worker's server-owned block-state capability",
        )),
    }
}

/// The metadata needed to choose a future plugin lifecycle policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperPluginDescriptor {
    jar: PathBuf,
    kind: PaperPluginDescriptorKind,
    name: String,
    version: String,
    main_class: String,
    main_class_entry: String,
}

impl PaperPluginDescriptor {
    /// The operator-supplied jar containing this descriptor.
    pub fn jar(&self) -> &Path {
        &self.jar
    }

    /// Which supported descriptor supplied this metadata.
    pub fn kind(&self) -> PaperPluginDescriptorKind {
        self.kind
    }

    /// Plugin identity, validated before lifecycle work begins.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Plugin version text, retained for diagnostics and a later lifecycle.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Dotted Java binary name for the plugin entry class.
    pub fn main_class(&self) -> &str {
        &self.main_class
    }

    /// Archive entry selected as the future isolated-loader entry point.
    ///
    /// Discovery verifies this is one Java class file in the same operator jar;
    /// it does not load, initialize, or invoke that class.
    pub fn main_class_entry(&self) -> &str {
        &self.main_class_entry
    }
}

/// The two descriptor formats accepted by Paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperPluginDescriptorKind {
    /// Paper's own descriptor format.
    Paper,
    /// Bukkit's compatibility descriptor format.
    Bukkit,
}

/// A bounded, actionable jar or descriptor discovery failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperBootstrapError {
    message: String,
    #[cfg(feature = "jvm")]
    native_surface: Option<crate::native_surface::NativeSurfaceError>,
}

impl PaperBootstrapError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            #[cfg(feature = "jvm")]
            native_surface: None,
        }
    }

    #[cfg(feature = "jvm")]
    fn native_surface(error: crate::native_surface::NativeSurfaceError) -> Self {
        Self {
            message: error.to_string(),
            native_surface: Some(error),
        }
    }

    #[cfg(feature = "jvm")]
    fn lifecycle(prefix: String, error: Self) -> Self {
        Self {
            message: format!("{prefix}: {}", error.message),
            native_surface: error.native_surface,
        }
    }

    /// The typed shim-registration cause, if lifecycle setup reached that seam.
    #[cfg(feature = "jvm")]
    pub fn native_surface_error(&self) -> Option<&crate::native_surface::NativeSurfaceError> {
        self.native_surface.as_ref()
    }
}

impl fmt::Display for PaperBootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PaperBootstrapError {}

fn validate_paper_jar(path: &Path) -> Result<(), PaperBootstrapError> {
    validate_operator_path(path, "Paper server jar")?;
    let mut archive = open_jar(path)?;
    require_exact_entry(&mut archive, BOOTSTRAP_ENTRY, path)?;
    let manifest = read_exact_entry(&mut archive, "META-INF/MANIFEST.MF", path)?;
    let manifest = std::str::from_utf8(&manifest).map_err(|error| {
        PaperBootstrapError::new(format!("Paper manifest {} is not UTF-8: {error}", path.display()))
    })?;
    if !manifest.lines().any(|line| line.trim_end() == PAPER_MANIFEST_TITLE) {
        return Err(PaperBootstrapError::new(format!(
            "Paper server jar {} lacks {PAPER_MANIFEST_TITLE:?}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_operator_path(path: &Path, kind: &str) -> Result<(), PaperBootstrapError> {
    let metadata = fs::metadata(path).map_err(|error| {
        PaperBootstrapError::new(format!("{kind} {} is unavailable: {error}", path.display()))
    })?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(PaperBootstrapError::new(format!(
            "{kind} {} is neither a file nor a directory",
            path.display()
        )));
    }
    Ok(())
}

fn plugin_jars(directory: &Path, max_plugins: usize) -> Result<Vec<PathBuf>, PaperBootstrapError> {
    let metadata = fs::metadata(directory).map_err(|error| {
        PaperBootstrapError::new(format!("plugin directory {} is unavailable: {error}", directory.display()))
    })?;
    if !metadata.is_dir() {
        return Err(PaperBootstrapError::new(format!(
            "plugin directory {} is not a directory",
            directory.display()
        )));
    }
    let mut jars = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| {
        PaperBootstrapError::new(format!("could not read plugin directory {}: {error}", directory.display()))
    })? {
        let entry = entry.map_err(|error| {
            PaperBootstrapError::new(format!("could not read plugin directory entry: {error}"))
        })?;
        let path = entry.path();
        if entry.file_type().map_err(|error| {
            PaperBootstrapError::new(format!("could not inspect plugin path {}: {error}", path.display()))
        })?.is_file() && path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("jar")) {
            jars.push(path);
        }
    }
    jars.sort();
    if jars.len() > max_plugins {
        return Err(PaperBootstrapError::new(format!(
            "plugin directory {} has {} jars, exceeding the limit of {max_plugins}",
            directory.display(),
            jars.len()
        )));
    }
    Ok(jars)
}

fn discover_plugin(jar: &Path) -> Result<PaperPluginDescriptor, PaperBootstrapError> {
    let mut archive = open_jar(jar)?;
    let paper_entries = entry_count(&mut archive, PAPER_DESCRIPTOR, jar)?;
    let bukkit_entries = entry_count(&mut archive, BUKKIT_DESCRIPTOR, jar)?;
    if paper_entries > 1 || bukkit_entries > 1 {
        return Err(PaperBootstrapError::new(format!(
            "plugin jar {} has duplicate descriptor entries",
            jar.display()
        )));
    }
    let (name, kind) = match (paper_entries, bukkit_entries) {
        (1, 0) => (PAPER_DESCRIPTOR, PaperPluginDescriptorKind::Paper),
        (0, 1) => (BUKKIT_DESCRIPTOR, PaperPluginDescriptorKind::Bukkit),
        (0, 0) => {
            return Err(PaperBootstrapError::new(format!(
                "plugin jar {} has neither {PAPER_DESCRIPTOR} nor {BUKKIT_DESCRIPTOR}",
                jar.display()
            )));
        }
        (1, 1) => {
            return Err(PaperBootstrapError::new(format!(
                "plugin jar {} has both {PAPER_DESCRIPTOR} and {BUKKIT_DESCRIPTOR}; refusing ambiguous metadata",
                jar.display()
            )));
        }
        _ => unreachable!("descriptor counts above one returned early"),
    };
    let bytes = read_exact_entry(&mut archive, name, jar)?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        PaperBootstrapError::new(format!("descriptor {name} in {} is not UTF-8: {error}", jar.display()))
    })?;
    let fields = scalar_fields(text, name, jar)?;
    let plugin_name = required_field(&fields, "name", name, jar)?;
    let version = required_field(&fields, "version", name, jar)?;
    let main_class = required_field(&fields, "main", name, jar)?;
    validate_plugin_name(&plugin_name, name, jar)?;
    validate_scalar("version", &version, name, jar)?;
    validate_class_name(&main_class, name, jar)?;
    let main_class_entry = class_entry_path(&main_class);
    validate_main_class_entry(&mut archive, &main_class_entry, jar)?;
    Ok(PaperPluginDescriptor {
        jar: jar.to_owned(),
        kind,
        name: plugin_name,
        version,
        main_class,
        main_class_entry,
    })
}

fn open_jar(path: &Path) -> Result<ZipArchive<File>, PaperBootstrapError> {
    let file = File::open(path).map_err(|error| {
        PaperBootstrapError::new(format!("could not open jar {}: {error}", path.display()))
    })?;
    ZipArchive::new(file).map_err(|error| {
        PaperBootstrapError::new(format!("could not read jar {}: {error}", path.display()))
    })
}

fn entry_count(
    _archive: &mut ZipArchive<File>,
    name: &str,
    path: &Path,
) -> Result<usize, PaperBootstrapError> {
    central_directory_entry_count(path, name)
}

/// `zip` indexes names for lookup, which makes a second identical central
/// entry replace the first in its index. Count the central records directly so
/// the exact-entry contract remains a property of the operator archive rather
/// than of that lookup implementation.
fn central_directory_entry_count(path: &Path, expected_name: &str) -> Result<usize, PaperBootstrapError> {
    let mut file = File::open(path).map_err(|error| {
        PaperBootstrapError::new(format!("could not inspect jar {}: {error}", path.display()))
    })?;
    let length = file.metadata().map_err(|error| {
        PaperBootstrapError::new(format!("could not inspect jar {}: {error}", path.display()))
    })?.len();
    let search_length = length.min(MAX_END_OF_CENTRAL_DIRECTORY_SEARCH);
    let search_start = length - search_length;
    file.seek(SeekFrom::Start(search_start)).map_err(|error| {
        PaperBootstrapError::new(format!("could not inspect jar {}: {error}", path.display()))
    })?;
    let mut tail = vec![0; usize::try_from(search_length).expect("ZIP search length fits usize")];
    file.read_exact(&mut tail).map_err(|error| {
        PaperBootstrapError::new(format!("could not inspect jar {}: {error}", path.display()))
    })?;
    let eocd_offset = tail.windows(END_OF_CENTRAL_DIRECTORY_SIGNATURE.len()).rposition(|window| {
        window == END_OF_CENTRAL_DIRECTORY_SIGNATURE
    }).filter(|offset| {
        let end = offset + END_OF_CENTRAL_DIRECTORY_BYTES as usize;
        end <= tail.len()
            && end + usize::from(read_u16(&tail[*offset + 20..*offset + 22])) == tail.len()
    }).ok_or_else(|| PaperBootstrapError::new(format!(
        "jar {} has no valid ZIP end-of-central-directory record",
        path.display()
    )))?;
    let eocd = &tail[eocd_offset..eocd_offset + END_OF_CENTRAL_DIRECTORY_BYTES as usize];
    let entries = read_u16(&eocd[10..12]);
    let directory_size = read_u32(&eocd[12..16]);
    let directory_offset = read_u32(&eocd[16..20]);
    if entries == u16::MAX || directory_size == u32::MAX || directory_offset == u32::MAX {
        return Err(PaperBootstrapError::new(format!(
            "jar {} uses ZIP64 central-directory metadata, which preflight cannot validate",
            path.display()
        )));
    }
    file.seek(SeekFrom::Start(u64::from(directory_offset))).map_err(|error| {
        PaperBootstrapError::new(format!("could not inspect jar {}: {error}", path.display()))
    })?;
    let expected_name = expected_name.as_bytes();
    let mut count = 0;
    let mut consumed = 0_u64;
    for _ in 0..entries {
        let mut header = [0; 46];
        file.read_exact(&mut header).map_err(|error| {
            PaperBootstrapError::new(format!("could not inspect jar {}: {error}", path.display()))
        })?;
        if header[..4] != CENTRAL_DIRECTORY_SIGNATURE {
            return Err(PaperBootstrapError::new(format!(
                "jar {} has an invalid central-directory entry",
                path.display()
            )));
        }
        let name_length = usize::from(read_u16(&header[28..30]));
        let extra_length = u64::from(read_u16(&header[30..32]));
        let comment_length = u64::from(read_u16(&header[32..34]));
        let mut name = vec![0; name_length];
        file.read_exact(&mut name).map_err(|error| {
            PaperBootstrapError::new(format!("could not inspect jar {}: {error}", path.display()))
        })?;
        if name == expected_name {
            count += 1;
        }
        let remaining = extra_length + comment_length;
        file.seek(SeekFrom::Current(i64::try_from(remaining).expect("ZIP field lengths fit i64")))
            .map_err(|error| PaperBootstrapError::new(format!(
                "could not inspect jar {}: {error}", path.display()
            )))?;
        consumed += 46 + u64::try_from(name_length).expect("ZIP name length fits u64") + remaining;
        if consumed > u64::from(directory_size) {
            return Err(PaperBootstrapError::new(format!(
                "jar {} has a central-directory entry beyond its declared size",
                path.display()
            )));
        }
    }
    if consumed != u64::from(directory_size) {
        return Err(PaperBootstrapError::new(format!(
            "jar {} has a central-directory size that does not match its entries",
            path.display()
        )));
    }
    Ok(count)
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn require_exact_entry(
    archive: &mut ZipArchive<File>,
    name: &str,
    path: &Path,
) -> Result<(), PaperBootstrapError> {
    if entry_count(archive, name, path)? != 1 {
        return Err(PaperBootstrapError::new(format!(
            "jar {} must contain exactly one {name}",
            path.display()
        )));
    }
    Ok(())
}

fn read_exact_entry(
    archive: &mut ZipArchive<File>,
    name: &str,
    path: &Path,
) -> Result<Vec<u8>, PaperBootstrapError> {
    require_exact_entry(archive, name, path)?;
    let entry = archive.by_name(name).map_err(|error| {
        PaperBootstrapError::new(format!("could not read {name} from {}: {error}", path.display()))
    })?;
    if entry.size() > MAX_DESCRIPTOR_BYTES {
        return Err(PaperBootstrapError::new(format!(
            "archive entry {name} in {} exceeds {MAX_DESCRIPTOR_BYTES} bytes",
            path.display()
        )));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.take(MAX_DESCRIPTOR_BYTES + 1).read_to_end(&mut bytes).map_err(|error| {
        PaperBootstrapError::new(format!("could not read {name} from {}: {error}", path.display()))
    })?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(PaperBootstrapError::new(format!(
            "archive entry {name} in {} exceeds {MAX_DESCRIPTOR_BYTES} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

fn scalar_fields(
    text: &str,
    descriptor: &str,
    jar: &Path,
) -> Result<BTreeMap<String, String>, PaperBootstrapError> {
    let mut fields = BTreeMap::new();
    for (line_number, line) in text.lines().enumerate() {
        if line.chars().next().is_some_and(char::is_whitespace)
            || line.trim().is_empty()
            || line.trim_start().starts_with('#')
        {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if !matches!(key, "name" | "version" | "main") {
            continue;
        }
        if value.is_empty() || matches!(value, "|" | ">") {
            return Err(PaperBootstrapError::new(format!(
                "descriptor {descriptor} in {} has no scalar {key} at line {}",
                jar.display(),
                line_number + 1
            )));
        }
        let value = unquote(value);
        if fields.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(PaperBootstrapError::new(format!(
                "descriptor {descriptor} in {} repeats {key}",
                jar.display()
            )));
        }
    }
    Ok(fields)
}

fn unquote(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if matches!(bytes[0], b'\'' | b'"') && bytes[0] == bytes[value.len() - 1] {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn required_field(
    fields: &BTreeMap<String, String>,
    name: &str,
    descriptor: &str,
    jar: &Path,
) -> Result<String, PaperBootstrapError> {
    fields.get(name).cloned().ok_or_else(|| PaperBootstrapError::new(format!(
        "descriptor {descriptor} in {} is missing required {name}",
        jar.display()
    )))
}

fn validate_plugin_name(value: &str, descriptor: &str, jar: &Path) -> Result<(), PaperBootstrapError> {
    validate_scalar("name", value, descriptor, jar)?;
    if !value.chars().all(|character| character.is_ascii_alphanumeric() || matches!(character, ' ' | '_' | '-' | '.')) {
        return Err(PaperBootstrapError::new(format!(
            "descriptor {descriptor} in {} has invalid plugin name {value:?}",
            jar.display()
        )));
    }
    Ok(())
}

fn validate_scalar(
    field: &str,
    value: &str,
    descriptor: &str,
    jar: &Path,
) -> Result<(), PaperBootstrapError> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(PaperBootstrapError::new(format!(
            "descriptor {descriptor} in {} has invalid {field}",
            jar.display()
        )));
    }
    Ok(())
}

fn validate_class_name(value: &str, descriptor: &str, jar: &Path) -> Result<(), PaperBootstrapError> {
    let valid = value.split('.').all(|segment| {
        let mut chars = segment.chars();
        chars.next().is_some_and(|character| character.is_ascii_alphabetic() || matches!(character, '_' | '$'))
            && chars.all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '$'))
    });
    if valid {
        Ok(())
    } else {
        Err(PaperBootstrapError::new(format!(
            "descriptor {descriptor} in {} has invalid main class {value:?}",
            jar.display()
        )))
    }
}

fn class_entry_path(binary_name: &str) -> String {
    format!("{}.class", binary_name.replace('.', "/"))
}

fn validate_main_class_entry(
    archive: &mut ZipArchive<File>,
    entry_name: &str,
    jar: &Path,
) -> Result<(), PaperBootstrapError> {
    require_exact_entry(archive, entry_name, jar)?;
    let mut entry = archive.by_name(entry_name).map_err(|error| {
        PaperBootstrapError::new(format!(
            "could not read plugin entry {entry_name} from {}: {error}",
            jar.display()
        ))
    })?;
    let mut magic = [0; CLASS_MAGIC.len()];
    entry.read_exact(&mut magic).map_err(|error| {
        PaperBootstrapError::new(format!(
            "plugin entry {entry_name} in {} is not a Java class file: {error}",
            jar.display()
        ))
    })?;
    if magic != CLASS_MAGIC {
        return Err(PaperBootstrapError::new(format!(
            "plugin entry {entry_name} in {} is not a Java class file",
            jar.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn discovery_sorts_jars_and_accepts_each_supported_descriptor() {
        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar(
            "z-last.jar",
            BUKKIT_DESCRIPTOR,
            valid_descriptor("Zulu", "z.Main"),
        );
        fixture.plugin_jar(
            "a-first.jar",
            PAPER_DESCRIPTOR,
            valid_descriptor("Alpha", "a.Main"),
        );
        let plan = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .with_shim_path(fixture.root.join("shim"))
            .with_isolated_native_shim()
            .discover()
            .expect("discover plugins");
        assert_eq!(plan.plugins().iter().map(PaperPluginDescriptor::name).collect::<Vec<_>>(), ["Alpha", "Zulu"]);
        assert_eq!(plan.plugins()[0].kind(), PaperPluginDescriptorKind::Paper);
        assert_eq!(plan.plugins()[1].kind(), PaperPluginDescriptorKind::Bukkit);
        assert_eq!(plan.plugins()[0].main_class_entry(), "a/Main.class");
        assert!(plan.requires_isolated_native_shim());
    }

    #[test]
    fn discovery_rejects_duplicate_names_and_ambiguous_descriptors() {
        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar("first.jar", BUKKIT_DESCRIPTOR, valid_descriptor("Same", "a.Main"));
        fixture.plugin_jar("second.jar", PAPER_DESCRIPTOR, valid_descriptor("same", "b.Main"));
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("duplicate names must fail");
        assert!(error.to_string().contains("duplicate plugin name"));

        let fixture = Fixture::new();
        fixture.paper_jar();
        let descriptor = valid_descriptor("One", "a.Main");
        write_jar(
            &fixture.plugins_path().join("ambiguous.jar"),
            [
                (PAPER_DESCRIPTOR, descriptor.as_bytes()),
                (BUKKIT_DESCRIPTOR, descriptor.as_bytes()),
            ],
        );
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("two descriptors must fail");
        assert!(error.to_string().contains("both paper-plugin.yml and plugin.yml"));
    }

    #[test]
    fn discovery_rejects_invalid_or_missing_metadata_before_a_jvm_starts() {
        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar("bad.jar", BUKKIT_DESCRIPTOR, "name: Bad\nversion: one\nmain: not/a/class\n");
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("invalid main must fail");
        assert!(error.to_string().contains("invalid main class"));

        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar("missing.jar", BUKKIT_DESCRIPTOR, "name: Missing\nmain: a.Main\n");
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("missing version must fail");
        assert!(error.to_string().contains("missing required version"));
    }

    #[test]
    fn discovery_requires_one_java_class_at_each_declared_entry_point() {
        let fixture = Fixture::new();
        fixture.paper_jar();
        let descriptor = valid_descriptor("Missing", "a.Main");
        write_jar(
            &fixture.plugins_path().join("missing.jar"),
            [(BUKKIT_DESCRIPTOR, descriptor.as_bytes())],
        );
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("a declared entry point must exist exactly once");
        assert!(error.to_string().contains("a/Main.class"), "{error}");

        let fixture = Fixture::new();
        fixture.paper_jar();
        let descriptor = valid_descriptor("Malformed", "a.Main");
        write_jar(
            &fixture.plugins_path().join("malformed.jar"),
            [
                (BUKKIT_DESCRIPTOR, descriptor.as_bytes()),
                ("a/Main.class", b"not-a-class"),
            ],
        );
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("an arbitrary archive entry is not a Java class");
        assert!(error.to_string().contains("not a Java class file"), "{error}");

        let fixture = Fixture::new();
        fixture.paper_jar();
        let descriptor = valid_descriptor("Duplicate", "a.Main");
        write_duplicate_entry_jar(
            &fixture.plugins_path().join("duplicate.jar"),
            descriptor.as_bytes(),
            "a/Main.class",
        );
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("duplicate entry points are ambiguous");
        assert!(error.to_string().contains("exactly one a/Main.class"), "{error}");
    }

    #[test]
    fn paper_discovery_requires_the_expected_class_and_manifest_marker() {
        let fixture = Fixture::new();
        write_jar(&fixture.paper_path(), [("META-INF/MANIFEST.MF", b"Implementation-Title: Paper\n")]);
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("missing bootstrap class must fail");
        assert!(error.to_string().contains(BOOTSTRAP_ENTRY));
    }

    #[test]
    fn native_shim_request_requires_an_operator_shim_path_before_jvm_startup() {
        let fixture = Fixture::new();
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .with_isolated_native_shim()
            .discover()
            .expect_err("a native shim cannot be resolved without a shim path");
        assert!(error.to_string().contains("requires at least one shim path"), "{error}");
    }

    #[test]
    fn lifecycle_loader_isolates_plugin_failures_and_retains_load_status() {
        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar("z-last.jar", BUKKIT_DESCRIPTOR, valid_descriptor("Zulu", "z.Main"));
        fixture.plugin_jar("a-first.jar", PAPER_DESCRIPTOR, valid_descriptor("Alpha", "a.Main"));
        let shim = fixture.root.join("shim");
        let plan = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .with_shim_path(&shim)
            .discover()
            .expect("discover lifecycle fixture");
        let mut requests = Vec::new();
        let status = plan.load_lifecycle_entries(|paths, class| {
            requests.push((paths.to_vec(), class.to_owned()));
            if class == "z.Main" {
                Err("fixture loader rejected Zulu")
            } else {
                Ok(())
            }
        })
        .expect("one plugin loader failure must be isolated");

        assert_eq!(requests.len(), 3, "every plugin must receive an isolated load attempt");
        assert_eq!(requests[0].1, BOOTSTRAP_CLASS);
        assert_eq!(requests[1].1, "a.Main");
        assert_eq!(requests[2].1, "z.Main");
        assert_eq!(requests[0].0, vec![shim.clone(), fixture.paper_path()]);
        assert_eq!(
            requests[1].0,
            vec![
                shim.clone(),
                fixture.paper_path(),
                fixture.plugins_path().join("a-first.jar"),
            ]
        );
        assert_eq!(
            requests[2].0,
            vec![
                shim,
                fixture.paper_path(),
                fixture.plugins_path().join("z-last.jar"),
            ]
        );
        assert_eq!(status.plugins()[0].phase(), PaperPluginLifecyclePhase::Loaded);
        assert_eq!(status.plugins()[1].phase(), PaperPluginLifecyclePhase::Failed);
        let failure = status.plugins()[1].failure().expect("Zulu must name its own failure");
        assert_eq!(failure.step(), PaperPluginLifecycleStep::Load);
        assert!(failure.message().contains("plugin \"Zulu\" entry class z.Main"), "{failure:?}");
        assert!(failure.message().contains("fixture loader rejected Zulu"), "{failure:?}");
    }

    #[test]
    fn lifecycle_phase_requires_load_then_enable_then_disable() {
        assert!(PaperPluginLifecyclePhase::Discovered.accepts(PaperPluginLifecycleStep::Load));
        assert!(!PaperPluginLifecyclePhase::Discovered.accepts(PaperPluginLifecycleStep::Enable));
        assert!(PaperPluginLifecyclePhase::Loaded.accepts(PaperPluginLifecycleStep::Enable));
        assert!(!PaperPluginLifecyclePhase::Loaded.accepts(PaperPluginLifecycleStep::Disable));
        assert!(PaperPluginLifecyclePhase::Enabled.accepts(PaperPluginLifecycleStep::Disable));
        assert!(!PaperPluginLifecyclePhase::Disabled.accepts(PaperPluginLifecycleStep::Load));
        assert!(!PaperPluginLifecyclePhase::Failed.accepts(PaperPluginLifecycleStep::Enable));
    }

    #[test]
    fn construction_readiness_preserves_descriptions_and_blocks_every_constructor() {
        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar("ready.jar", PAPER_DESCRIPTOR, valid_descriptor("Ready", "a.Main"));
        fixture.plugin_jar("failed.jar", BUKKIT_DESCRIPTOR, valid_descriptor("Failed", "b.Main"));
        let plan = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect("discover construction fixture");
        let status = plan.load_lifecycle_entries(|_, class| {
            if class == "b.Main" { Err("fixture entry failure") } else { Ok(()) }
        })
        .expect("the bootstrap and first entry load");

        let readiness = PaperPluginConstructionReadiness::from_lifecycle(
            &status,
            &PaperServerFacadeInput::Unavailable,
        );
        assert_eq!(readiness.facade(), PaperServerFacadeState::Unavailable);
        assert_eq!(readiness.plugins().len(), 2);
        assert_eq!(readiness.plugins()[0].descriptor().name(), "Failed");
        assert_eq!(readiness.plugins()[0].descriptor().version(), "one");
        assert_eq!(readiness.plugins()[0].descriptor().main_class(), "b.Main");
        assert_eq!(
            readiness.plugins()[0].blocker(),
            PaperPluginConstructionBlocker::EntryLoadFailed,
        );
        assert_eq!(readiness.plugins()[1].descriptor().name(), "Ready");
        assert_eq!(
            readiness.plugins()[1].blocker(),
            PaperPluginConstructionBlocker::ServerFacadeUnavailable,
        );
        assert_eq!(
            PaperPluginConstructionBlocker::ServerFacadeUnavailable.to_string(),
            "no compatible server facade is installed",
        );
    }

    #[test]
    fn isolated_native_shim_requires_a_worker_owned_capability() {
        let missing_input =
            validate_construction_facade(true, &PaperServerFacadeInput::Unavailable)
                .expect_err("a registered native declaration requires its server-owned input");
        assert!(missing_input
            .to_string()
            .contains("adapter worker's server-owned block-state capability"));
    }

    #[test]
    fn lifecycle_loader_does_not_attempt_plugins_after_bootstrap_failure() {
        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar(
            "plugin.jar",
            BUKKIT_DESCRIPTOR,
            valid_descriptor("Alpha", "a.Main"),
        );
        let plan = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect("discover lifecycle fixture");
        let mut classes = Vec::new();
        let error = plan.load_lifecycle_entries(|_, class| {
            classes.push(class.to_owned());
            Err("bootstrap unavailable")
        })
        .expect_err("bootstrap failure must stop before plugin loading");

        assert_eq!(classes, [BOOTSTRAP_CLASS]);
        assert!(error.to_string().contains("Paper bootstrap class"), "{error}");
        assert!(error.to_string().contains("bootstrap unavailable"), "{error}");
    }

    #[test]
    #[ignore = "requires LODESTONE_PAPER_JAR pointing to a locally materialized Paper server jar"]
    fn local_paper_jar_is_discovered_without_extracting_it() {
        let paper_jar = PathBuf::from(std::env::var_os("LODESTONE_PAPER_JAR")
            .expect("LODESTONE_PAPER_JAR is required"));
        let fixture = Fixture::new();
        let plan = PaperBootstrapConfig::new(paper_jar, fixture.plugins_path())
            .discover()
            .expect("discover local Paper jar");
        assert!(plan.plugins().is_empty());
    }

    fn valid_descriptor(name: &str, main: &str) -> String {
        format!("name: {name}\nversion: one\nmain: {main}\n")
    }

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let number = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("lodestone-paper-discovery-{}-{number}", std::process::id()));
            fs::create_dir_all(root.join("plugins")).expect("create fixture plugin directory");
            fs::create_dir(root.join("shim")).expect("create fixture shim directory");
            Self { root }
        }

        fn paper_path(&self) -> PathBuf {
            self.root.join("paper.jar")
        }

        fn plugins_path(&self) -> PathBuf {
            self.root.join("plugins")
        }

        fn paper_jar(&self) {
            write_jar(
                &self.paper_path(),
                [
                    (BOOTSTRAP_ENTRY, b"class bytes are not read during discovery"),
                    ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\nImplementation-Title: Paper\n"),
                ],
            );
        }

        fn plugin_jar(&self, name: &str, descriptor: &str, contents: impl AsRef<str>) {
            let contents = contents.as_ref();
            let main_class = contents.lines().find_map(|line| line.strip_prefix("main: "));
            if let Some(main_class) = main_class {
                let entry = class_entry_path(main_class);
                write_jar(
                    &self.plugins_path().join(name),
                    [(descriptor, contents.as_bytes()), (entry.as_str(), &CLASS_MAGIC)],
                );
            } else {
                write_jar(&self.plugins_path().join(name), [(descriptor, contents.as_bytes())]);
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write_jar<const N: usize>(path: &Path, entries: [(&str, &[u8]); N]) {
        let file = File::create(path).expect("create fixture jar");
        let mut writer = zip::ZipWriter::new(file);
        for (name, contents) in entries {
            writer.start_file(name, zip::write::SimpleFileOptions::default()).expect("write fixture entry");
            writer.write_all(contents).expect("write fixture contents");
        }
        writer.finish().expect("finish fixture jar");
    }

    /// `ZipWriter` prevents duplicate names, so this fixture writes the small
    /// stored-entry archive directly to exercise discovery's hostile-jar path.
    fn write_duplicate_entry_jar(path: &Path, descriptor: &[u8], duplicate_name: &str) {
        let entries = [
            (BUKKIT_DESCRIPTOR, descriptor),
            (duplicate_name, CLASS_MAGIC.as_slice()),
            (duplicate_name, CLASS_MAGIC.as_slice()),
        ];
        let mut file = File::create(path).expect("create duplicate-entry fixture jar");
        let mut central = Vec::new();
        let mut offset = 0_u32;
        for (name, contents) in entries {
            let name = name.as_bytes();
            let size = u32::try_from(contents.len()).expect("fixture entry size");
            let crc = crc32(contents);
            write_u32(&mut file, 0x0403_4B50);
            write_u16(&mut file, 20);
            write_u16(&mut file, 0);
            write_u16(&mut file, 0);
            write_u16(&mut file, 0);
            write_u16(&mut file, 0);
            write_u32(&mut file, crc);
            write_u32(&mut file, size);
            write_u32(&mut file, size);
            write_u16(&mut file, u16::try_from(name.len()).expect("fixture name length"));
            write_u16(&mut file, 0);
            file.write_all(name).expect("write fixture name");
            file.write_all(contents).expect("write fixture contents");
            central.push((name.to_vec(), crc, size, offset));
            offset += 30 + u32::try_from(name.len()).expect("fixture name length") + size;
        }
        let central_offset = offset;
        for (name, crc, size, local_offset) in central {
            write_u32(&mut file, 0x0201_4B50);
            write_u16(&mut file, 20);
            write_u16(&mut file, 20);
            write_u16(&mut file, 0);
            write_u16(&mut file, 0);
            write_u16(&mut file, 0);
            write_u16(&mut file, 0);
            write_u32(&mut file, crc);
            write_u32(&mut file, size);
            write_u32(&mut file, size);
            write_u16(&mut file, u16::try_from(name.len()).expect("fixture name length"));
            write_u16(&mut file, 0);
            write_u16(&mut file, 0);
            write_u16(&mut file, 0);
            write_u16(&mut file, 0);
            write_u32(&mut file, 0);
            write_u32(&mut file, local_offset);
            file.write_all(&name).expect("write central fixture name");
            offset += 46 + u32::try_from(name.len()).expect("fixture name length");
        }
        write_u32(&mut file, 0x0605_4B50);
        write_u16(&mut file, 0);
        write_u16(&mut file, 0);
        write_u16(&mut file, 3);
        write_u16(&mut file, 3);
        write_u32(&mut file, offset - central_offset);
        write_u32(&mut file, central_offset);
        write_u16(&mut file, 0);
    }

    fn write_u16(writer: &mut File, value: u16) {
        writer.write_all(&value.to_le_bytes()).expect("write fixture u16");
    }

    fn write_u32(writer: &mut File, value: u32) {
        writer.write_all(&value.to_le_bytes()).expect("write fixture u32");
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 0 { crc >> 1 } else { (crc >> 1) ^ 0xEDB8_8320 };
            }
        }
        !crc
    }
}
