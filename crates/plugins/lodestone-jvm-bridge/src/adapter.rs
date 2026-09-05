//! Explicit experimental adapter loading and tick dispatch on a dedicated JVM thread.
//!
//! This is a bootstrap contract for developing the Paper host, not a Java plugin
//! discovery mechanism. The supplied class declares `static void onTick(long)`,
//! `static void onBlockStateChanged(int, int, int, int)`, and `static native
//! int blockStateId(int, int, int)`. Native requests cross the world port; the
//! worker never receives world state. A host must service the port and poll
//! completion, and must not dispatch another callback until idle.

use std::cell::RefCell;
use std::ffi::c_void;
use std::fmt;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JString};
use jni::strings::JNIString;
use jni::sys::{jint, jlong};
use jni::{Env, EnvUnowned, JValue, NativeMethod, jni_sig, jni_str};

use crate::runtime::{JvmConfig, JvmRuntime};
use crate::{CallbackDepthGuard, PortServicer, WorldPort, channel};

/// A block query in the host's primary world, in absolute block coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockStateQuery {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// A host must distinguish an unavailable position from a valid air state.
pub type BlockStateAnswer = Result<u32, String>;

/// A requested replacement of one already-resident primary-world block.
///
/// `state_id` is deliberately still raw at this JNI-facing boundary. The
/// native host validates it against the server's generated state table before
/// mutating terrain; this crate does not name a game-data crate merely to
/// duplicate that validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockStateWrite {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub state_id: u32,
}

/// A failed write must not be reported as a successful no-op.
pub type BlockStateWriteAnswer = Result<(), String>;

type BlockPort = WorldPort<BlockStateQuery, BlockStateAnswer>;
type BlockWritePort = WorldPort<BlockStateWrite, BlockStateWriteAnswer>;
type TickPort = WorldPort<(), ServerTickAnswer>;
type Events = SyncSender<Result<AdapterEvent, AdapterError>>;

/// A host must distinguish an inactive game tick from a valid count.
pub type ServerTickAnswer = Result<u64, String>;

/// Proof that setup is running beside live native server-state producers.
///
/// Only [`AdapterHost`] creates this token, from the worker's request ports.
/// A lifecycle setup may consume it to install a loader-local Java surface,
/// but cannot use it to read world state directly. The matching
/// [`AdapterHost::service_pending`] and
/// [`AdapterHost::service_pending_server_tick`] endpoints remain the only
/// producers.
#[derive(Debug)]
pub struct NativeServerSurface {
    _block_port: BlockPort,
    _block_write_port: BlockWritePort,
    _tick_port: TickPort,
}

impl NativeServerSurface {
    fn from_ports(block_port: BlockPort, block_write_port: BlockWritePort, tick_port: TickPort) -> Self {
        Self {
            _block_port: block_port,
            _block_write_port: block_write_port,
            _tick_port: tick_port,
        }
    }
}

thread_local! {
    static CALLBACK_PORT: RefCell<Option<BlockPort>> = const { RefCell::new(None) };
    static BLOCK_WRITE_PORT: RefCell<Option<BlockWritePort>> = const { RefCell::new(None) };
    static SERVER_TICK_PORT: RefCell<Option<TickPort>> = const { RefCell::new(None) };
}

/// One host-to-JVM callback waiting on the dedicated adapter worker.
///
/// The single command channel and [`State::Running`] permit only one of these
/// at a time. A block-change callback therefore cannot be dispatched while a
/// native read or write callback is waiting for the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdapterCommand {
    Tick(u64),
    BlockStateChanged(BlockStateWrite),
}

/// A completed worker transition. Payload-bearing events preserve the host's
/// original callback values so callers can match completion without guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterEvent {
    Ready,
    TickCompleted(u64),
    BlockStateChangedCompleted(BlockStateWrite),
}

#[derive(Debug)]
enum State {
    Loading(Instant),
    Idle,
    Running { command: AdapterCommand, started: Instant },
    Failed(AdapterError),
}

/// Nonblocking host endpoint for one explicitly configured Java adapter.
///
/// Drop disconnects the worker and port without joining an untrusted Java call.
/// Java code cannot be forcibly stopped safely in-process: a timed-out adapter
/// is terminal and may continue executing until the operator exits the process.
/// JNI permits only one JVM startup per process, including after a failed load.
#[derive(Debug)]
pub struct AdapterHost {
    commands: SyncSender<AdapterCommand>,
    events: Receiver<Result<AdapterEvent, AdapterError>>,
    servicer: PortServicer<BlockStateQuery, BlockStateAnswer>,
    block_write_servicer: PortServicer<BlockStateWrite, BlockStateWriteAnswer>,
    server_tick_servicer: PortServicer<(), ServerTickAnswer>,
    deadline: Duration,
    state: State,
}

impl AdapterHost {
    /// Spawns the JVM worker. Loading success or failure arrives through `poll`.
    /// The class name uses dotted Java binary-name spelling, such as `a.B$C`.
    pub fn start(
        config: JvmConfig,
        class: &str,
        deadline: Duration,
    ) -> Result<Self, AdapterError> {
        Self::start_with_setup(config, class, deadline, |_, _, _| Ok(()))
    }

    /// Spawns the JVM worker after running one bounded setup step on it.
    ///
    /// The setup runs after the JVM starts but before the adapter class loads.
    /// It receives no world state and must not invoke arbitrary operator code.
    /// Its [`NativeServerSurface`] token proves that the same worker owns
    /// native request ports with host-side producers; it is the only way a
    /// lifecycle host may claim those narrow capabilities. A successful result
    /// remains on that worker until the adapter stops, so setup can retain
    /// loader-owned JVM state without publishing it to the tick thread.
    pub fn start_with_setup<F, S>(
        config: JvmConfig,
        class: &str,
        deadline: Duration,
        setup: F,
    ) -> Result<Self, AdapterError>
    where
        F: for<'local> FnOnce(&JvmRuntime, &mut Env<'local>, NativeServerSurface)
                -> Result<S, String>
            + Send
            + 'static,
        S: Send + 'static,
    {
        validate_class(class)?;
        if deadline.is_zero() {
            return Err(AdapterError::new("adapter deadline must be positive"));
        }
        let class = class.to_owned();
        Self::spawn(deadline, move |commands, events, port, block_write_port, server_tick_port| {
            let result = run_java(
                config,
                &class,
                commands,
                &events,
                port,
                block_write_port,
                server_tick_port,
                setup,
            );
            if let Err(error) = result {
                let _ = events.send(Err(error));
            }
        })
    }

    fn spawn(
        deadline: Duration,
        run: impl FnOnce(Receiver<AdapterCommand>, Events, BlockPort, BlockWritePort, TickPort)
            + Send
            + 'static,
    ) -> Result<Self, AdapterError> {
        let (commands, receiver) = sync_channel(1);
        let (sender, events) = sync_channel(1);
        let (port, servicer) = channel(deadline);
        let (block_write_port, block_write_servicer) = channel(deadline);
        let (server_tick_port, server_tick_servicer) = channel(deadline);
        std::thread::Builder::new()
            .name("lodestone-java-adapter".to_owned())
            .spawn(move || run(receiver, sender, port, block_write_port, server_tick_port))
            .map_err(|error| AdapterError::new(format!("adapter worker startup: {error}")))?;
        Ok(Self {
            commands,
            events,
            servicer,
            block_write_servicer,
            server_tick_servicer,
            deadline,
            state: State::Loading(Instant::now()),
        })
    }

    /// True after the host polls readiness or the preceding tick's completion.
    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// Queues exactly one tick without blocking. Busy dispatch is an error;
    /// callbacks are never silently dropped or accumulated into a backlog.
    pub fn dispatch_tick(&mut self, tick: u64) -> Result<(), AdapterError> {
        if tick > i64::MAX as u64 {
            return Err(AdapterError::new("adapter tick exceeds Java long range"));
        }
        self.dispatch_command(AdapterCommand::Tick(tick), "tick")
    }

    /// Queues one host-confirmed block-state change for the adapter worker.
    ///
    /// Call this only after the host has applied a successful resident-block
    /// mutation. The callback runs on the dedicated JVM worker, never on the
    /// tick thread, and has no direct route to an ECS handle. A busy worker is
    /// rejected rather than accumulating events or calling Java while a
    /// native world request is unresolved.
    ///
    /// This is a narrow bridge callback, not a Bukkit or Paper event. It calls
    /// the explicit adapter method `onBlockStateChanged(int, int, int, int)`;
    /// no listener registry, cancellation, plugin instance, or Paper event
    /// type exists yet.
    pub fn dispatch_block_state_changed(
        &mut self,
        change: BlockStateWrite,
    ) -> Result<(), AdapterError> {
        if i32::try_from(change.state_id).is_err() {
            return Err(AdapterError::new(
                "block-state callback state id exceeds Java int range",
            ));
        }
        self.dispatch_command(AdapterCommand::BlockStateChanged(change), "block-state callback")
    }

    fn dispatch_command(
        &mut self,
        command: AdapterCommand,
        operation: &str,
    ) -> Result<(), AdapterError> {
        if !self.is_idle() {
            return Err(AdapterError::new("adapter is not idle; poll its pending operation"));
        }
        self.commands.try_send(command)
            .map_err(|error| AdapterError::new(format!("adapter {operation} dispatch: {error}")))?;
        self.state = State::Running { command, started: Instant::now() };
        Ok(())
    }

    /// Answers at most `max` queued block queries on the caller's thread.
    /// The closure should use the public host block API and report unavailable
    /// positions explicitly. It is never invoked on the JVM worker.
    pub fn service_pending(
        &self,
        max: usize,
        answer: impl FnMut(BlockStateQuery) -> BlockStateAnswer,
    ) -> usize {
        if matches!(self.state, State::Failed(_)) {
            return 0;
        }
        self.servicer.service_all_pending(max, answer)
    }

    /// Applies at most `max` native block-state writes on the caller's thread.
    ///
    /// The closure must preserve the host's resident-only mutation contract:
    /// it must reject an absent column rather than generate one on behalf of a
    /// Java callback. It is never invoked on the JVM worker.
    pub fn service_pending_block_writes(
        &self,
        max: usize,
        answer: impl FnMut(BlockStateWrite) -> BlockStateWriteAnswer,
    ) -> usize {
        if matches!(self.state, State::Failed(_)) {
            return 0;
        }
        self.block_write_servicer.service_all_pending(max, answer)
    }

    /// Answers at most `max` queued server-tick reads on the caller's thread.
    ///
    /// The closure must read the host's live tick witness. An inactive server
    /// is an error, not a made-up zero, because zero is a valid tick count for
    /// a future host whose lifecycle has not completed boot yet.
    pub fn service_pending_server_tick(
        &self,
        max: usize,
        mut answer: impl FnMut() -> ServerTickAnswer,
    ) -> usize {
        if matches!(self.state, State::Failed(_)) {
            return 0;
        }
        self.server_tick_servicer.service_all_pending(max, |_| answer())
    }

    /// Polls completion without waiting. Startup/callback deadlines are terminal;
    /// poll regularly even when no world query is pending. Java exceptions name
    /// the failing class/method and preserve the Java exception's description.
    pub fn poll(&mut self) -> Result<Option<AdapterEvent>, AdapterError> {
        if let State::Failed(error) = &self.state {
            return Err(error.clone());
        }
        let started = match self.state {
            State::Loading(started) | State::Running { started, .. } => Some(started),
            _ => None,
        };
        if started.is_some_and(|started| started.elapsed() >= self.deadline) {
            let error = AdapterError::new(format!("adapter operation exceeded {:?}", self.deadline));
            self.state = State::Failed(error.clone());
            return Err(error);
        }
        let result = match self.events.try_recv() {
            Ok(Ok(event)) => match (&self.state, event) {
                (State::Loading(_), AdapterEvent::Ready) => Ok(Some(event)),
                (
                    State::Running { command: AdapterCommand::Tick(tick), .. },
                    AdapterEvent::TickCompleted(done),
                ) if *tick == done => {
                    Ok(Some(event))
                }
                (
                    State::Running {
                        command: AdapterCommand::BlockStateChanged(expected),
                        ..
                    },
                    AdapterEvent::BlockStateChangedCompleted(done),
                ) if *expected == done => {
                    Ok(Some(event))
                }
                _ => Err(AdapterError::new("adapter worker returned an unexpected completion")),
            },
            Ok(Err(error)) => Err(error),
            Err(TryRecvError::Disconnected) => Err(AdapterError::new("adapter worker disconnected")),
            Err(TryRecvError::Empty) => Ok(None),
        };
        match &result {
            Ok(Some(_)) => self.state = State::Idle,
            Err(error) => self.state = State::Failed(error.clone()),
            Ok(None) => {}
        }
        result
    }
}

/// A bounded, actionable adapter failure; never a default block-state result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterError(String);

impl AdapterError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AdapterError {}

fn validate_class(class: &str) -> Result<(), AdapterError> {
    let valid = class.split('.').all(|segment| {
        let mut chars = segment.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    });
    if valid {
        Ok(())
    } else {
        Err(AdapterError::new(format!("invalid adapter class {class:?}")))
    }
}

fn run_java<S>(
    config: JvmConfig,
    class_name: &str,
    commands: Receiver<AdapterCommand>,
    events: &Events,
    port: BlockPort,
    block_write_port: BlockWritePort,
    server_tick_port: TickPort,
    setup: impl for<'local> FnOnce(&JvmRuntime, &mut Env<'local>, NativeServerSurface)
        -> Result<S, String>,
) -> Result<(), AdapterError> {
    // Operator paths are supplied only to isolated loaders below. Putting
    // either the adapter or a Paper jar on the system loader would let its
    // parent-first lookup defeat shim-first resolution.
    let runtime = JvmRuntime::start(&JvmConfig::new())
        .map_err(|error| AdapterError::new(format!("adapter {class_name}: {error}")))?;
    runtime.with_attached_thread(|env| {
        let result = (|| {
            CALLBACK_PORT.with(|slot| *slot.borrow_mut() = Some(port.clone()));
            BLOCK_WRITE_PORT.with(|slot| *slot.borrow_mut() = Some(block_write_port.clone()));
            SERVER_TICK_PORT.with(|slot| *slot.borrow_mut() = Some(server_tick_port.clone()));
            let surface = NativeServerSurface::from_ports(
                port.clone(),
                block_write_port.clone(),
                server_tick_port.clone(),
            );
            let setup_state = setup(&runtime, env, surface).map_err(|error| {
                AdapterError::new(format!("adapter {class_name} setup: {error}"))
            })?;
            let class = runtime.load_isolated_class(env, &config, class_name)
                .map_err(|error| java_error(env, class_name, error))?;
            register_block_query(env, &class, "blockStateId", "(III)I")
                .map_err(|error| java_error(env, &format!("{class_name}.blockStateId(III)I"), error))?;
            env.get_static_method_id(&class, jni_str!("onTick"), jni_sig!("(J)V"))
                .map_err(|error| java_error(env, &format!("{class_name}.onTick(J)V"), error))?;
            env.get_static_method_id(&class, jni_str!("onBlockStateChanged"), jni_sig!("(IIII)V"))
                .map_err(|error| java_error(
                    env,
                    &format!("{class_name}.onBlockStateChanged(IIII)V"),
                    error,
                ))?;
            if events.send(Ok(AdapterEvent::Ready)).is_err() {
                return Ok(());
            }
            while let Ok(command) = commands.recv() {
                let completion = match command {
                    AdapterCommand::Tick(tick) => {
                        env.with_local_frame(16, |env| {
                            env.call_static_method(&class, jni_str!("onTick"), jni_sig!("(J)V"),
                                &[JValue::Long(tick as i64)])
                                .map(|_| ())
                                .map_err(|error| java_error(
                                    env,
                                    &format!("{class_name}.onTick(J)V"),
                                    error,
                                ))
                        })?;
                        AdapterEvent::TickCompleted(tick)
                    }
                    AdapterCommand::BlockStateChanged(change) => {
                        let state_id = i32::try_from(change.state_id).map_err(|_| {
                            AdapterError::new("block-state callback state id exceeds Java int range")
                        })?;
                        env.with_local_frame(16, |env| {
                            env.call_static_method(
                                &class,
                                jni_str!("onBlockStateChanged"),
                                jni_sig!("(IIII)V"),
                                &[
                                    JValue::Int(change.x),
                                    JValue::Int(change.y),
                                    JValue::Int(change.z),
                                    JValue::Int(state_id),
                                ],
                            )
                            .map(|_| ())
                            .map_err(|error| java_error(
                                env,
                                &format!("{class_name}.onBlockStateChanged(IIII)V"),
                                error,
                            ))
                        })?;
                        AdapterEvent::BlockStateChangedCompleted(change)
                    }
                };
                if events.send(Ok(completion)).is_err() {
                    break;
                }
            }
            drop(setup_state);
            Ok(())
        })();
        CALLBACK_PORT.with(|slot| *slot.borrow_mut() = None);
        BLOCK_WRITE_PORT.with(|slot| *slot.borrow_mut() = None);
        SERVER_TICK_PORT.with(|slot| *slot.borrow_mut() = None);
        Ok(result)
    }).map_err(|error| AdapterError::new(error.to_string()))?
}

impl From<jni::errors::Error> for AdapterError {
    fn from(error: jni::errors::Error) -> Self {
        Self::new(error.to_string())
    }
}

fn java_error(env: &mut Env<'_>, operation: &str, error: impl fmt::Display) -> AdapterError {
    let description = env.exception_occurred().and_then(|exception| {
        env.exception_clear();
        let description = env.call_method(&exception, jni_str!("toString"),
            jni_sig!("()Ljava/lang/String;"), &[]).ok()?.l().ok()?;
        let description = env.cast_local::<JString>(description).ok()?;
        description.try_to_string(env).ok()
    });
    env.exception_clear();
    AdapterError::new(format!("adapter {operation}: {}", description.unwrap_or_else(|| error.to_string())))
}

#[allow(unsafe_code)]
pub(crate) fn register_block_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the static native accepts exactly three jint arguments and
    // returns jint. Callers first validate the supplied method name and
    // descriptor against that exact declaration. EnvUnowned contains every
    // Rust unwind at the FFI boundary.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(&name, &signature,
            native_block_state_id as *mut c_void);
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_server_tick_query(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the static native accepts no arguments and returns a jlong.
    // Callers validate the supplied declaration before registration. The
    // callback contains panics and errors at the JNI boundary.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(&name, &signature,
            native_server_tick_count as *mut c_void);
        env.register_native_methods(class, &[method])
    }
}

#[allow(unsafe_code)]
pub(crate) fn register_block_state_write(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method_name: &str,
    descriptor: &str,
) -> jni::errors::Result<()> {
    // SAFETY: the static native accepts four jint arguments and returns jint.
    // The isolated native surface validates this exact declaration before the
    // function pointer is installed. The callback contains every error at the
    // JNI boundary.
    unsafe {
        let name = JNIString::new(method_name);
        let signature = JNIString::new(descriptor);
        let method = NativeMethod::from_raw_parts(&name, &signature,
            native_block_state_write as *mut c_void);
        env.register_native_methods(class, &[method])
    }
}

extern "system" fn native_block_state_id<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    x: jint,
    y: jint,
    z: jint,
) -> jint {
    env.with_env(|_env| -> Result<jint, AdapterError> {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let port = CALLBACK_PORT.with(|slot| slot.borrow().clone())
            .ok_or_else(|| AdapterError::new("blockStateId requires the adapter worker thread"))?;
        let state = port.request(BlockStateQuery { x, y, z })
            .map_err(|error| AdapterError::new(format!("blockStateId: {error}")))?
            .map_err(|error| AdapterError::new(format!("blockStateId({x},{y},{z}): {error}")))?;
        jint::try_from(state).map_err(|_| AdapterError::new("blockStateId exceeds Java int range"))
    }).resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_server_tick_count<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jlong {
    env.with_env(|_env| -> Result<jlong, AdapterError> {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let port = SERVER_TICK_PORT.with(|slot| slot.borrow().clone())
            .ok_or_else(|| AdapterError::new("serverTickCount requires the adapter worker thread"))?;
        let tick = port.request(())
            .map_err(|error| AdapterError::new(format!("serverTickCount: {error}")))?
            .map_err(|error| AdapterError::new(format!("serverTickCount: {error}")))?;
        jlong::try_from(tick)
            .map_err(|_| AdapterError::new("serverTickCount exceeds Java long range"))
    }).resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_block_state_write<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    x: jint,
    y: jint,
    z: jint,
    state_id: jint,
) -> jint {
    env.with_env(|_env| -> Result<jint, AdapterError> {
        let _depth = CallbackDepthGuard::enter()
            .map_err(|error| AdapterError::new(error.to_string()))?;
        let state_id = u32::try_from(state_id)
            .map_err(|_| AdapterError::new("setBlockStateId requires a non-negative state id"))?;
        let port = BLOCK_WRITE_PORT.with(|slot| slot.borrow().clone())
            .ok_or_else(|| AdapterError::new("setBlockStateId requires the adapter worker thread"))?;
        port.request(BlockStateWrite { x, y, z, state_id })
            .map_err(|error| AdapterError::new(format!("setBlockStateId: {error}")))?
            .map_err(|error| AdapterError::new(format!("setBlockStateId({x},{y},{z},{state_id}): {error}")))?;
        jint::try_from(state_id)
            .map_err(|_| AdapterError::new("setBlockStateId exceeds Java int range"))
    }).resolve::<ThrowRuntimeExAndDefault>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn await_event(host: &mut AdapterHost) -> AdapterEvent {
        let limit = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(event) = host.poll().expect("worker event") {
                return event;
            }
            assert!(Instant::now() < limit, "worker did not report an event");
            std::thread::yield_now();
        }
    }

    #[test]
    fn callback_queries_run_on_host_and_ticks_do_not_overlap() {
        let host_thread = std::thread::current().id();
        let mut host = AdapterHost::spawn(Duration::from_secs(2), move |commands, events, port, _, _| {
            assert_ne!(std::thread::current().id(), host_thread);
            events.send(Ok(AdapterEvent::Ready)).unwrap();
            for command in commands {
                let AdapterCommand::Tick(tick) = command else {
                    panic!("fixture expected only a tick callback");
                };
                let result = port.request(BlockStateQuery { x: 11, y: 7, z: -3 }).unwrap();
                assert_eq!(result, Ok(422));
                events.send(Ok(AdapterEvent::TickCompleted(tick))).unwrap();
            }
        }).unwrap();
        assert!(host.dispatch_tick(37).is_err(), "loading is not ready");
        assert_eq!(await_event(&mut host), AdapterEvent::Ready);
        assert!(host.dispatch_tick(u64::MAX).is_err(), "do not truncate Java long");
        host.dispatch_tick(37).unwrap();
        assert!(host.dispatch_tick(38).is_err(), "no second tick while one is pending");
        let limit = Instant::now() + Duration::from_secs(2);
        while host.service_pending(1, |query| {
            assert_eq!(std::thread::current().id(), host_thread);
            assert_eq!(query, BlockStateQuery { x: 11, y: 7, z: -3 });
            Ok((query.x * 31 + query.y * 7 - query.z * 5 + 17) as u32)
        }) == 0 {
            assert!(Instant::now() < limit, "query did not reach the host");
            std::thread::yield_now();
        }
        assert_eq!(await_event(&mut host), AdapterEvent::TickCompleted(37));
        assert!(host.is_idle());
    }

    #[test]
    fn block_change_callbacks_are_bounded_and_preserve_the_applied_value() {
        let host_thread = std::thread::current().id();
        let expected = BlockStateWrite {
            x: -17,
            y: 64,
            z: 33,
            state_id: 1234,
        };
        let mut host = AdapterHost::spawn(Duration::from_secs(2), move |commands, events, _, _, _| {
            assert_ne!(std::thread::current().id(), host_thread);
            events.send(Ok(AdapterEvent::Ready)).unwrap();
            let command = commands.recv().expect("one host callback");
            assert_eq!(command, AdapterCommand::BlockStateChanged(expected));
            events.send(Ok(AdapterEvent::BlockStateChangedCompleted(expected))).unwrap();
        }).unwrap();
        assert_eq!(await_event(&mut host), AdapterEvent::Ready);
        host.dispatch_block_state_changed(expected).unwrap();
        assert!(host.dispatch_tick(37).is_err(), "no callback backlog is permitted");
        assert_eq!(
            await_event(&mut host),
            AdapterEvent::BlockStateChangedCompleted(expected),
        );
        assert!(host.is_idle());
        let oversized = BlockStateWrite { state_id: i32::MAX as u32 + 1, ..expected };
        let error = host
            .dispatch_block_state_changed(oversized)
            .expect_err("the Java callback cannot represent this native state id");
        assert!(error.to_string().contains("Java int range"), "{error}");
    }

    #[test]
    fn deadline_rejects_a_late_completion_and_remains_terminal() {
        let mut host = AdapterHost::spawn(Duration::from_secs(2), |commands, events, _port, _, _| {
            events.send(Ok(AdapterEvent::Ready)).unwrap();
            let _ = commands.recv();
        }).unwrap();
        // An already-delivered event must not rescue an operation whose host
        // deadline elapsed. Backdate the clock state instead of sleeping.
        let limit = Instant::now() + Duration::from_secs(2);
        while host.events.try_recv().is_err() {
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        let (sender, receiver) = sync_channel(1);
        sender.send(Ok(AdapterEvent::Ready)).unwrap();
        host.events = receiver;
        host.state = State::Loading(Instant::now() - Duration::from_secs(3));
        let error = host.poll().expect_err("late readiness must time out");
        assert!(error.to_string().contains("exceeded"));
        assert_eq!(host.poll(), Err(error));
        assert!(host.dispatch_tick(1).is_err());
        assert_eq!(host.service_pending(1, |_| panic!("terminal host must not service")), 0);
    }

    #[test]
    fn worker_errors_preserve_the_named_failure() {
        let mut host = AdapterHost::spawn(Duration::from_secs(2), |commands, events, _port, _, _| {
            events.send(Err(AdapterError::new("example.Adapter.onTick: missing member"))).unwrap();
            let _ = commands.recv();
        }).unwrap();
        let limit = Instant::now() + Duration::from_secs(2);
        loop {
            match host.poll() {
                Err(error) => {
                    assert_eq!(error.to_string(), "example.Adapter.onTick: missing member");
                    break;
                }
                Ok(_) => assert!(Instant::now() < limit),
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn server_tick_reads_run_on_the_host_and_preserve_zero() {
        let host_thread = std::thread::current().id();
        let mut host = AdapterHost::spawn(
            Duration::from_secs(2),
            move |_commands, events, _port, _write_port, tick_port| {
                assert_ne!(std::thread::current().id(), host_thread);
                events.send(Ok(AdapterEvent::Ready)).unwrap();
                assert_eq!(tick_port.request(()).unwrap(), Ok(0));
            },
        )
        .unwrap();
        assert_eq!(await_event(&mut host), AdapterEvent::Ready);
        let limit = Instant::now() + Duration::from_secs(2);
        while host.service_pending_server_tick(1, || Ok(0)) == 0 {
            assert!(Instant::now() < limit, "server tick query did not reach the host");
            std::thread::yield_now();
        }
    }

    #[test]
    fn block_writes_run_on_the_host_and_keep_the_full_request() {
        let host_thread = std::thread::current().id();
        let mut host = AdapterHost::spawn(
            Duration::from_secs(2),
            move |_commands, events, _port, write_port, _tick_port| {
                assert_ne!(std::thread::current().id(), host_thread);
                events.send(Ok(AdapterEvent::Ready)).unwrap();
                assert_eq!(
                    write_port.request(BlockStateWrite {
                        x: -17,
                        y: 64,
                        z: 33,
                        state_id: 1234,
                    }).unwrap(),
                    Ok(())
                );
            },
        ).unwrap();
        assert_eq!(await_event(&mut host), AdapterEvent::Ready);
        let limit = Instant::now() + Duration::from_secs(2);
        while host.service_pending_block_writes(1, |write| {
            assert_eq!(std::thread::current().id(), host_thread);
            assert_eq!(
                write,
                BlockStateWrite { x: -17, y: 64, z: 33, state_id: 1234 }
            );
            Ok(())
        }) == 0 {
            assert!(Instant::now() < limit, "block write did not reach the host");
            std::thread::yield_now();
        }
    }
}
