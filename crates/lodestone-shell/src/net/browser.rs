//! Browser integrated-server transport and worker startup.
//!
//! The browser cannot host the integrated server on the page thread. This
//! module owns the Worker-backed byte transport, a separate worldgen progress
//! channel, and the control-plane state machine that waits for world
//! construction before handing the byte port to the ordinary client session.

#[cfg(target_arch = "wasm32")]
use std::io;
#[cfg(target_arch = "wasm32")]
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(target_arch = "wasm32")]
use std::pin::Pin;
#[cfg(target_arch = "wasm32")]
use std::task::{Context, Poll};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;
#[cfg(target_arch = "wasm32")]
use web_sys::{ErrorEvent, MessageChannel, MessageEvent, MessagePort, Worker};

#[cfg(target_arch = "wasm32")]
use crate::horizon::{
    BrowserHorizonClient, apply_tile_response, parse_tile_response,
};

/// Browser-only client endpoint for an integrated session.
///
/// The Worker is retained for the entire connection. A startup failure is
/// reported to the session instead of falling back to an in-page server: the
/// browser's singleplayer world must have one owner, and that owner must stay
/// off the page thread just as it does after `ready`.
#[cfg(target_arch = "wasm32")]
pub(super) enum BrowserIntegratedTransport {
    Worker {
        _worker: Worker,
        port: lodestone_net::MessagePortTransport,
        _progress_port: MessagePort,
        _horizon_port: MessagePort,
        _on_progress: Closure<dyn FnMut(MessageEvent)>,
        _on_horizon: Closure<dyn FnMut(MessageEvent)>,
        _on_error: Closure<dyn FnMut(ErrorEvent)>,
    },
}

#[cfg(target_arch = "wasm32")]
static NEXT_WORKER_EPOCH: AtomicU32 = AtomicU32::new(1);

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BrowserWorkerProgress {
    epoch: u32,
    session: u64,
    admitted: u64,
    completed: u64,
    committed: u64,
    queue: u64,
    bytes: u64,
}

#[cfg(target_arch = "wasm32")]
const MAX_SAFE_PROGRESS: f64 = 9_007_199_254_740_991.0;

#[cfg(target_arch = "wasm32")]
fn worker_progress(value: &JsValue) -> Option<(BrowserWorkerProgress, String)> {
    let get = |key: &str| {
        js_sys::Reflect::get(value, &JsValue::from_str(key))
            .ok()
            .and_then(|number| number.as_f64())
    };
    let integer = |number: f64| {
        number.is_finite()
            && number >= 0.0
            && number.fract() == 0.0
            && number <= MAX_SAFE_PROGRESS
    };
    let epoch = get("epoch")?;
    let session = get("session").unwrap_or(0.0);
    let admitted = get("admitted")?;
    let completed = get("completed")?;
    let committed = get("committed")?;
    let queue = get("queue")?;
    let bytes = get("bytes")?;
    if ![epoch, session, admitted, completed, committed, queue, bytes]
        .into_iter()
        .all(integer)
        || epoch > u32::MAX as f64
    {
        return None;
    }
    let stage = js_sys::Reflect::get(value, &JsValue::from_str("stage"))
        .ok()?
        .as_string()
        .filter(|stage| !stage.is_empty())?;
    Some((
        BrowserWorkerProgress {
            epoch: epoch as u32,
            session: session as u64,
            admitted: admitted as u64,
            completed: completed as u64,
            committed: committed as u64,
            queue: queue as u64,
            bytes: bytes as u64,
        },
        stage,
    ))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn browser_diagnostic(message: std::fmt::Arguments<'_>) {
    if log::max_level() < log::LevelFilter::Debug {
        return;
    }
    let global = js_sys::global();
    let Ok(post) = js_sys::Reflect::get(&global, &JsValue::from_str("postMessage")) else {
        return;
    };
    let Ok(post) = post.dyn_into::<js_sys::Function>() else {
        return;
    };
    let message = message.to_string();
    let event = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&event, &JsValue::from_str("kind"), &JsValue::from_str("diagnostic"));
    let _ = js_sys::Reflect::set(&event, &JsValue::from_str("message"), &JsValue::from_str(&message));
    let _ = post.call1(&global, &event);
}

#[cfg(target_arch = "wasm32")]
fn forward_phase_timings(value: &JsValue, epoch: u32) {
    let received_epoch = js_sys::Reflect::get(value, &JsValue::from_str("epoch"))
        .ok()
        .and_then(|value| value.as_f64());
    if received_epoch != Some(f64::from(epoch)) {
        return;
    }
    let Ok(phases) = js_sys::Reflect::get(value, &JsValue::from_str("phases"))
        .and_then(|value| value.dyn_into::<js_sys::Array>())
    else {
        return;
    };
    for row in phases.iter() {
        let phase = js_sys::Reflect::get(&row, &JsValue::from_str("phase"))
            .ok()
            .and_then(|value| value.as_string());
        let number = |key| js_sys::Reflect::get(&row, &JsValue::from_str(key))
            .ok()
            .and_then(|value| value.as_f64())
            .filter(|value| value.is_finite() && *value >= 0.0);
        let (Some(phase), Some(calls), Some(items), Some(elapsed), Some(maximum)) =
            (phase, number("calls"), number("items"), number("elapsedMs"), number("maximumMs"))
        else {
            continue;
        };
        browser_diagnostic(format_args!(
            "worldgen timing: phase={phase} calls={calls:.0} items={items:.0} sum_ms={elapsed:.3} max_ms={maximum:.3}",
        ));
    }
}

#[cfg(target_arch = "wasm32")]
impl tokio::io::AsyncRead for BrowserIntegratedTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Worker { port, .. } => Pin::new(port).poll_read(cx, buf),
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl tokio::io::AsyncWrite for BrowserIntegratedTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Self::Worker { port, .. } => Pin::new(port).poll_write(cx, data),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Worker { port, .. } => Pin::new(port).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Worker {
                _worker,
                port,
                _progress_port,
                _horizon_port,
                ..
            } => {
                let result = Pin::new(port).poll_shutdown(cx);
                if matches!(result, Poll::Ready(_)) {
                    // A MessagePort has no peer-close event. Once the client
                    // endpoint is shut down there is no reliable signal for
                    // the server task to observe, so terminate the sole
                    // authoritative Worker explicitly. `Drop` repeats this
                    // as a guard for callers that discard the transport
                    // without polling shutdown.
                    _progress_port.close();
                    _horizon_port.close();
                    _worker.terminate();
                }
                result
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for BrowserIntegratedTransport {
    fn drop(&mut self) {
        let Self::Worker {
            _worker,
            _progress_port,
            _horizon_port,
            ..
        } = self;
        // `Worker` has no Rust-side join handle, and dropping the JS wrapper
        // does not guarantee that its event loop stops.
        _progress_port.close();
        _horizon_port.close();
        _worker.terminate();
    }
}

#[cfg(target_arch = "wasm32")]
fn world_preset_wire_id(preset: crate::menu::create_world::WorldTypePreset) -> u8 {
    use crate::menu::create_world::WorldTypePreset;
    match preset {
        WorldTypePreset::Normal => 0,
        WorldTypePreset::LargeBiomes => 1,
        WorldTypePreset::Amplified => 2,
        WorldTypePreset::SingleBiomeSurface => 3,
        WorldTypePreset::Flat => 4,
        WorldTypePreset::FlatAllDimensions => 5,
        WorldTypePreset::DebugAllBlockStates => 6,
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn world_preset_from_wire_id(
    id: u8,
) -> Option<crate::menu::create_world::WorldTypePreset> {
    use crate::menu::create_world::WorldTypePreset;
    Some(match id {
        0 => WorldTypePreset::Normal,
        1 => WorldTypePreset::LargeBiomes,
        2 => WorldTypePreset::Amplified,
        3 => WorldTypePreset::SingleBiomeSurface,
        4 => WorldTypePreset::Flat,
        5 => WorldTypePreset::FlatAllDimensions,
        6 => WorldTypePreset::DebugAllBlockStates,
        _ => return None,
    })
}

/// Starts the server worker and waits for its synchronous world construction.
///
/// The transferred protocol port contains bytes only. A second transferred
/// port carries structured worldgen counters, while the ordinary Worker
/// channel carries launch and ready/error responses. No page-side replacement
/// is created when startup fails.
#[derive(Debug, PartialEq, Eq)]
#[cfg(any(target_arch = "wasm32", test))]
pub(super) enum BrowserWorkerStartupAction {
    Progress(String),
    Ready,
    Failed(String),
    Ignored,
}

#[derive(Debug, PartialEq, Eq)]
#[cfg(any(target_arch = "wasm32", test))]
pub(super) enum BrowserWorkerStartupState {
    Waiting,
    Ready,
    Failed,
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BrowserWorkerErrorAction {
    /// Startup did not finish. The caller must report the launch failure; an
    /// in-page replacement would violate the browser scheduling boundary.
    StartupFailure(String),
    /// The worker was already handed to the running client. Only disconnect is
    /// valid now; starting another server would create two mutable worlds.
    SessionFailure(String),
}

#[cfg(any(target_arch = "wasm32", test))]
pub(super) fn browser_worker_error_action(
    startup_pending: bool,
    message: String,
) -> BrowserWorkerErrorAction {
    if startup_pending {
        BrowserWorkerErrorAction::StartupFailure(message)
    } else {
        BrowserWorkerErrorAction::SessionFailure(message)
    }
}

#[cfg(any(target_arch = "wasm32", test))]
impl BrowserWorkerStartupState {
    pub(super) fn new() -> Self {
        Self::Waiting
    }

    /// Classify one control-plane message without letting a progress update
    /// consume the one-shot startup result. The browser worker emits several
    /// progress messages while it loads the module, enters server startup, and
    /// constructs the authoritative world source; only `ready` completes
    /// startup.
    pub(super) fn receive(
        &mut self,
        kind: Option<&str>,
        stage: Option<String>,
        message: Option<String>,
    ) -> BrowserWorkerStartupAction {
        if !matches!(self, Self::Waiting) {
            return BrowserWorkerStartupAction::Ignored;
        }

        match kind {
            Some("progress") => match stage.filter(|stage| !stage.is_empty()) {
                Some(stage) => BrowserWorkerStartupAction::Progress(stage),
                None => {
                    *self = Self::Failed;
                    BrowserWorkerStartupAction::Failed(
                        "server worker sent malformed startup progress".to_string(),
                    )
                }
            },
            Some("ready") => {
                *self = Self::Ready;
                BrowserWorkerStartupAction::Ready
            }
            Some("error") => {
                *self = Self::Failed;
                BrowserWorkerStartupAction::Failed(
                    message
                        .filter(|message| !message.is_empty())
                        .unwrap_or_else(|| "server worker failed during startup".to_string()),
                )
            }
            _ => {
                *self = Self::Failed;
                BrowserWorkerStartupAction::Failed(
                    "server worker sent an invalid startup response".to_string(),
                )
            }
        }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
pub(super) fn browser_worker_view_radius(requested: Option<i32>) -> Result<i32, String> {
    let radius = requested.unwrap_or(8);
    if radius < 0 {
        return Err("invalid browser worker view radius".to_string());
    }
    Ok(radius)
}

#[cfg(target_arch = "wasm32")]
pub(super) async fn launch_browser_worker(
    protocol: i32,
    seed: i64,
    preset: crate::menu::create_world::WorldTypePreset,
    view_radius: i32,
    horizon_surface: super::SharedHorizonSurface,
) -> Result<BrowserIntegratedTransport, String> {
    let view_radius = browser_worker_view_radius(Some(view_radius))?;
    let channel = MessageChannel::new()
        .map_err(|e| e.as_string().unwrap_or_else(|| "cannot create worker channel".to_string()))?;
    let page_port = channel.port1();
    let worker_port = channel.port2();
    let progress_channel = match MessageChannel::new() {
        Ok(channel) => channel,
        Err(error) => {
            page_port.close();
            worker_port.close();
            return Err(
                error
                    .as_string()
                    .unwrap_or_else(|| "cannot create worker progress channel".to_string()),
            );
        }
    };
    let page_progress_port = progress_channel.port1();
    let worker_progress_port = progress_channel.port2();
    let horizon_channel = match MessageChannel::new() {
        Ok(channel) => channel,
        Err(error) => {
            page_port.close();
            worker_port.close();
            page_progress_port.close();
            worker_progress_port.close();
            return Err(
                error
                    .as_string()
                    .unwrap_or_else(|| "cannot create worker horizon channel".to_string()),
            );
        }
    };
    let page_horizon_port = horizon_channel.port1();
    let worker_horizon_port = horizon_channel.port2();
    let worker = match Worker::new("lodestone-server-worker.js") {
        Ok(worker) => worker,
        Err(error) => {
            page_port.close();
            worker_port.close();
            page_progress_port.close();
            worker_progress_port.close();
            page_horizon_port.close();
            worker_horizon_port.close();
            return Err(
                error
                    .as_string()
                    .unwrap_or_else(|| "cannot create server worker".to_string()),
            );
        }
    };
    let epoch = NEXT_WORKER_EPOCH.fetch_add(1, Ordering::Relaxed).max(1);
    let horizon_client = BrowserHorizonClient::new(epoch, page_horizon_port.clone());
    let horizon_cache = horizon_client.cache();
    // Build this before waiting for `ready` so the worker error callback can
    // wake a client read after startup. A MessagePort itself has no close event.
    let mut transport = lodestone_net::MessagePortTransport::new(page_port);
    if log::max_level() >= log::LevelFilter::Debug {
        transport.set_diagnostics_sink(|progress| {
            browser_diagnostic(format_args!(
                "transport: side=client endpoint={} posted={} received={} drained={} send_credit={} receive_credit={} longest_pending_ms={:.3} current_pending_ms={:.3} closed={}",
                progress.endpoint_id,
                progress.posted_bytes,
                progress.received_bytes,
                progress.drained_bytes,
                progress.send_credit,
                progress.receive_credit,
                progress.longest_write_pending.as_secs_f64() * 1000.0,
                progress.current_write_pending.as_secs_f64() * 1000.0,
                progress.closed,
            ));
        });
    }
    let port_shutdown = transport.shutdown_handle();
    let progress_started = crate::platform::Instant::now();
    let on_progress = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let value = event.data();
        let kind = js_sys::Reflect::get(&value, &JsValue::from_str("kind"))
            .ok()
            .and_then(|kind| kind.as_string());
        if kind.as_deref() == Some("worldgen-timing") {
            forward_phase_timings(&value, epoch);
            return;
        }
        if kind.as_deref() == Some("connection-progress") {
            let number = |key| js_sys::Reflect::get(&value, &JsValue::from_str(key))
                .ok()
                .and_then(|value| value.as_f64());
            let boolean = |key| js_sys::Reflect::get(&value, &JsValue::from_str(key))
                .ok()
                .and_then(|value| value.as_bool());
            if number("epoch") == Some(f64::from(epoch)) {
                browser_diagnostic(format_args!(
                    "connection: running={:?} elapsed_ms={:.1} passes={:.0} client_loaded={:?} center={:.0},{:.0} radius={:.0} owed={:.0} delivered={:.0} sends={:.0} remaining={:.0} activity={} activity_ms={:.1} target={:?},{:?} packet={:?}",
                    boolean("running"),
                    number("elapsedMs").unwrap_or(0.0),
                    number("passes").unwrap_or(0.0),
                    boolean("clientLoaded"),
                    number("centerX").unwrap_or(0.0),
                    number("centerZ").unwrap_or(0.0),
                    number("radius").unwrap_or(0.0),
                    number("owedColumns").unwrap_or(0.0),
                    number("deliveredColumns").unwrap_or(0.0),
                    number("chunksSent").unwrap_or(0.0),
                    number("remaining").unwrap_or(0.0),
                    js_sys::Reflect::get(&value, &JsValue::from_str("activity"))
                        .ok().and_then(|value| value.as_string()).unwrap_or_default(),
                    number("activityMs").unwrap_or(0.0),
                    number("targetX"),
                    number("targetZ"),
                    number("packetId"),
                ));
            }
            return;
        }
        if kind.as_deref() == Some("transport-progress") {
            let number = |key| js_sys::Reflect::get(&value, &JsValue::from_str(key))
                .ok().and_then(|value| value.as_f64());
            if number("epoch") == Some(f64::from(epoch)) {
                browser_diagnostic(format_args!(
                    "transport: side=server endpoint={:.0} posted={:.0} received={:.0} drained={:.0} send_credit={:.0} receive_credit={:.0} longest_pending_ms={:.3} current_pending_ms={:.3} closed={:?}",
                    number("endpoint").unwrap_or(0.0),
                    number("postedBytes").unwrap_or(0.0),
                    number("receivedBytes").unwrap_or(0.0),
                    number("drainedBytes").unwrap_or(0.0),
                    number("sendCredit").unwrap_or(0.0),
                    number("receiveCredit").unwrap_or(0.0),
                    number("longestPendingMs").unwrap_or(0.0),
                    number("currentPendingMs").unwrap_or(0.0),
                    js_sys::Reflect::get(&value, &JsValue::from_str("closed"))
                        .ok().and_then(|value| value.as_bool()),
                ));
            }
            return;
        }
        if kind.as_deref() == Some("worker-health") {
            let number = |key| js_sys::Reflect::get(&value, &JsValue::from_str(key))
                .ok()
                .and_then(|value| value.as_f64());
            if number("epoch") == Some(f64::from(epoch)) {
                browser_diagnostic(format_args!(
                    "server health: ticks={:.0} overruns={:.0} mspt_ms={:.2} budget_tps={:.1} observed_tps={:?} callback_gap_ms={:.1} wake_p95_ms={:?} wake_max_ms={:?} deadline_max_ms={:?} catch_up_ticks={:?} recovery_yields={:?} yield_max_ms={:?} shed_ticks={:?}",
                    number("tickCount").unwrap_or(0.0),
                    number("overruns").unwrap_or(0.0),
                    number("msptMs").unwrap_or(0.0),
                    number("tps").unwrap_or(0.0),
                    number("observedTps"),
                    number("callbackGapMs").unwrap_or(0.0),
                    number("tickWakeP95Ms"),
                    number("tickWakeMaxMs"),
                    number("tickDeadlineMaxMs"),
                    number("tickCatchUpCount"),
                    number("tickCooperativeYields"),
                    number("tickYieldMaxMs"),
                    number("tickShedCount"),
                ));
                log::debug!(
                    "browser server worker health: ticks={:?} witness={:?} overruns={:?} mspt_ms={:?} budget_tps={:?} observed_tps={:?} callback_gap_ms={:?}",
                    number("tickCount"),
                    number("tickWitness"),
                    number("overruns"),
                    number("msptMs"),
                    number("tps"),
                    number("observedTps"),
                    number("callbackGapMs"),
                );
            }
            return;
        }
        if kind.as_deref() != Some("worldgen-progress") {
            return;
        }
        match worker_progress(&value) {
            Some((progress, stage)) if progress.epoch == epoch => {
                if progress.session < 4 || progress.session % 16 == 0 {
                    let target_x = js_sys::Reflect::get(&value, &JsValue::from_str("targetX"))
                        .ok()
                        .and_then(|value| value.as_f64());
                    let target_z = js_sys::Reflect::get(&value, &JsValue::from_str("targetZ"))
                        .ok()
                        .and_then(|value| value.as_f64());
                    browser_diagnostic(format_args!(
                        "worldgen: session={} target={:?},{:?} stage={stage} admitted={} completed={} committed={} queue={} bytes={} elapsed_ms={:.1}",
                        progress.session,
                        target_x,
                        target_z,
                        progress.admitted,
                        progress.completed,
                        progress.committed,
                        progress.queue,
                        progress.bytes,
                        progress_started.elapsed().as_secs_f64() * 1000.0,
                    ));
                }
                log::debug!(
                    "browser worldgen progress: stage={stage} admitted={} completed={} committed={} queue={} bytes={} elapsed_ms={:.3}",
                    progress.admitted,
                    progress.completed,
                    progress.committed,
                    progress.queue,
                    progress.bytes,
                    progress_started.elapsed().as_secs_f64() * 1000.0,
                );
            }
            Some(_) => tracing::warn!("ignoring stale browser worldgen progress"),
            None => tracing::warn!("ignoring malformed browser worldgen progress"),
        }
    });
    page_progress_port.set_onmessage(Some(on_progress.as_ref().unchecked_ref()));
    page_progress_port.start();
    let on_horizon = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let Some(response) = parse_tile_response(&event.data()) else {
            tracing::debug!("ignoring malformed browser horizon response");
            return;
        };
        if response.epoch != epoch {
            tracing::debug!("ignoring stale browser horizon response");
            return;
        }
        if !apply_tile_response(&horizon_cache, response) {
            tracing::debug!("ignoring unmatched browser horizon response");
        }
    });
    page_horizon_port.set_onmessage(Some(on_horizon.as_ref().unchecked_ref()));
    page_horizon_port.start();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    let ready_tx = std::rc::Rc::new(std::cell::RefCell::new(Some(ready_tx)));
    let on_message = {
        let ready_tx = std::rc::Rc::clone(&ready_tx);
        let mut startup_state = BrowserWorkerStartupState::new();
        Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let value = event.data();
            let kind = js_sys::Reflect::get(&value, &JsValue::from_str("kind"))
                .ok()
                .and_then(|v| v.as_string());
            let stage = js_sys::Reflect::get(&value, &JsValue::from_str("stage"))
                .ok()
                .and_then(|v| v.as_string());
            let message = js_sys::Reflect::get(&value, &JsValue::from_str("message"))
                .ok()
                .and_then(|v| v.as_string());
            match startup_state.receive(kind.as_deref(), stage, message) {
                BrowserWorkerStartupAction::Progress(stage) => {
                    if stage == "preparing-world" {
                        let executor = js_sys::Reflect::get(&value, &JsValue::from_str("executor"))
                            .ok().and_then(|value| value.as_string());
                        let workers = js_sys::Reflect::get(&value, &JsValue::from_str("workers"))
                            .ok().and_then(|value| value.as_f64());
                        browser_diagnostic(format_args!(
                            "server startup: phase={stage} executor={} workers={:.0}",
                            executor.as_deref().unwrap_or("unknown"), workers.unwrap_or(0.0),
                        ));
                    } else {
                        browser_diagnostic(format_args!("server startup: {stage}"));
                    }
                    tracing::debug!(%stage, "browser server worker startup progress");
                }
                BrowserWorkerStartupAction::Ready => {
                    if let Some(tx) = ready_tx.borrow_mut().take() {
                        let _ = tx.send(Ok(()));
                    }
                }
                BrowserWorkerStartupAction::Failed(error) => {
                    if let Some(tx) = ready_tx.borrow_mut().take() {
                        let _ = tx.send(Err(error));
                    }
                }
                BrowserWorkerStartupAction::Ignored => {}
            }
        })
    };
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    let on_error = {
        let ready_tx = std::rc::Rc::clone(&ready_tx);
        let port_shutdown = port_shutdown.clone();
        Closure::<dyn FnMut(ErrorEvent)>::new(move |event: ErrorEvent| {
            let message = if event.message().is_empty() {
                "server worker stopped".to_string()
            } else {
                event.message()
            };
            match browser_worker_error_action(ready_tx.borrow().is_some(), message) {
                BrowserWorkerErrorAction::StartupFailure(message) => {
                    if let Some(tx) = ready_tx.borrow_mut().take() {
                        let _ = tx.send(Err(message));
                    }
                }
                BrowserWorkerErrorAction::SessionFailure(message) => {
                    port_shutdown.signal(message);
                }
            }
        })
    };
    worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    let launch = js_sys::Object::new();
    js_sys::Reflect::set(&launch, &JsValue::from_str("kind"), &JsValue::from_str("start"))
        .expect("plain launch object accepts kind");
    js_sys::Reflect::set(
        &launch,
        &JsValue::from_str("protocol"),
        &JsValue::from_f64(f64::from(protocol)),
    )
    .expect("plain launch object accepts protocol");
    js_sys::Reflect::set(
        &launch,
        &JsValue::from_str("seed"),
        &JsValue::from_str(&seed.to_string()),
    )
    .expect("plain launch object accepts seed");
    js_sys::Reflect::set(
        &launch,
        &JsValue::from_str("preset"),
        &JsValue::from_f64(f64::from(world_preset_wire_id(preset))),
    )
    .expect("plain launch object accepts preset");
    js_sys::Reflect::set(
        &launch,
        &JsValue::from_str("viewRadius"),
        &JsValue::from_f64(f64::from(view_radius)),
    )
    .expect("plain launch object accepts view radius");
    js_sys::Reflect::set(
        &launch,
        &JsValue::from_str("epoch"),
        &JsValue::from_f64(f64::from(epoch)),
    )
    .expect("plain launch object accepts cancellation epoch");
    js_sys::Reflect::set(
        &launch,
        &JsValue::from_str("logLevel"),
        &JsValue::from_str(match log::max_level() {
            log::LevelFilter::Off => "off",
            log::LevelFilter::Error => "error",
            log::LevelFilter::Warn => "warn",
            log::LevelFilter::Info => "info",
            log::LevelFilter::Debug => "debug",
            log::LevelFilter::Trace => "trace",
        }),
    )
    .expect("plain launch object accepts log level");
    let transfer = js_sys::Array::new();
    transfer.push(&worker_port);
    transfer.push(&worker_progress_port);
    transfer.push(&worker_horizon_port);
    if let Err(error) = worker.post_message_with_transfer(&launch, &transfer) {
        worker.set_onmessage(None);
        worker.set_onerror(None);
        page_progress_port.close();
        page_horizon_port.close();
        worker_progress_port.close();
        worker_horizon_port.close();
        worker.terminate();
        return Err(
            error
                .as_string()
                .unwrap_or_else(|| "cannot transfer server port to worker".to_string()),
        );
    }
    let startup = match ready_rx.await {
        Ok(startup) => startup,
        Err(_) => {
            worker.set_onmessage(None);
            worker.set_onerror(None);
            page_progress_port.close();
            page_horizon_port.close();
            worker_progress_port.close();
            worker_horizon_port.close();
            worker.terminate();
            return Err("server worker stopped during startup".to_string());
        }
    };
    worker.set_onmessage(None);
    drop(on_message);
    if let Err(error) = startup {
        worker.set_onerror(None);
        drop(on_error);
        page_horizon_port.close();
        worker_horizon_port.close();
        worker.terminate();
        return Err(error);
    }
    if matches!(
        preset,
        crate::menu::create_world::WorldTypePreset::Normal
            | crate::menu::create_world::WorldTypePreset::LargeBiomes
            | crate::menu::create_world::WorldTypePreset::Amplified
    ) {
        let _ = horizon_surface.set(horizon_client);
    }
    Ok(BrowserIntegratedTransport::Worker {
        _worker: worker,
        port: transport,
        _progress_port: page_progress_port,
        _horizon_port: page_horizon_port,
        _on_progress: on_progress,
        _on_horizon: on_horizon,
        _on_error: on_error,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        BrowserWorkerErrorAction, BrowserWorkerStartupAction, BrowserWorkerStartupState,
        browser_worker_error_action, browser_worker_view_radius,
    };

    #[test]
    fn worker_view_radius_keeps_legacy_default_and_rejects_negative_values() {
        assert_eq!(browser_worker_view_radius(None), Ok(8));
        assert_eq!(browser_worker_view_radius(Some(0)), Ok(0));
        assert!(browser_worker_view_radius(Some(-1)).is_err());
    }

    #[test]
    #[cfg(feature = "window")]
    fn worker_view_radius_preserves_the_shared_non_round_stream_radius() {
        let requested = i32::try_from(crate::app::integrated_stream_radius(9)).unwrap();
        assert_eq!(browser_worker_view_radius(Some(requested)), Ok(11));
    }

    #[test]
    fn startup_accepts_progress_until_ready() {
        let mut startup = BrowserWorkerStartupState::new();
        for stage in ["loading-module", "starting-server", "preparing-world"] {
            assert_eq!(
                startup.receive(Some("progress"), Some(stage.to_string()), None),
                BrowserWorkerStartupAction::Progress(stage.to_string())
            );
        }
        assert_eq!(
            startup.receive(Some("ready"), None, None),
            BrowserWorkerStartupAction::Ready
        );
        assert_eq!(startup, BrowserWorkerStartupState::Ready);
        assert_eq!(
            startup.receive(Some("progress"), Some("late".to_string()), None),
            BrowserWorkerStartupAction::Ignored,
            "messages after ready must not complete startup a second time"
        );
    }

    #[test]
    fn startup_rejects_malformed_and_failed_messages() {
        let mut malformed = BrowserWorkerStartupState::new();
        assert_eq!(
            malformed.receive(Some("progress"), None, None),
            BrowserWorkerStartupAction::Failed(
                "server worker sent malformed startup progress".to_string()
            )
        );
        assert_eq!(
            malformed.receive(Some("ready"), None, None),
            BrowserWorkerStartupAction::Ignored
        );

        let mut failed = BrowserWorkerStartupState::new();
        assert_eq!(
            failed.receive(Some("error"), None, Some("compute pool stopped".to_string())),
            BrowserWorkerStartupAction::Failed("compute pool stopped".to_string())
        );
        assert_eq!(failed, BrowserWorkerStartupState::Failed);

        let mut unknown = BrowserWorkerStartupState::new();
        assert_eq!(
            unknown.receive(Some("unexpected"), None, None),
            BrowserWorkerStartupAction::Failed(
                "server worker sent an invalid startup response".to_string()
            )
        );
    }

    #[test]
    fn startup_ready_is_still_immediate_without_progress() {
        let mut startup = BrowserWorkerStartupState::new();
        assert_eq!(
            startup.receive(Some("ready"), None, None),
            BrowserWorkerStartupAction::Ready
        );
        assert_eq!(startup, BrowserWorkerStartupState::Ready);
    }

    #[test]
    fn worker_error_before_ready_is_a_launch_failure() {
        assert_eq!(
            browser_worker_error_action(true, "module failed".to_string()),
            BrowserWorkerErrorAction::StartupFailure("module failed".to_string())
        );
    }

    #[test]
    fn worker_error_after_ready_only_disconnects_the_session() {
        let action = browser_worker_error_action(false, "worker crashed".to_string());
        assert_eq!(
            action,
            BrowserWorkerErrorAction::SessionFailure("worker crashed".to_string())
        );
        // This is the detector control: changing the post-ready branch to the
        // pre-ready launch-failure path would produce `StartupFailure` and
        // fail here.
        assert!(!matches!(action, BrowserWorkerErrorAction::StartupFailure(_)));
    }
}
