//! Browser integrated-server transport and worker startup.
//!
//! The browser cannot host the integrated server on the page thread. This
//! module owns the Worker-backed byte transport and the small control-plane
//! state machine that waits for world construction before handing the port to
//! the ordinary client session.

#[cfg(target_arch = "wasm32")]
use std::io;
#[cfg(target_arch = "wasm32")]
use std::pin::Pin;
#[cfg(target_arch = "wasm32")]
use std::task::{Context, Poll};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;
#[cfg(target_arch = "wasm32")]
use web_sys::{ErrorEvent, MessageChannel, MessageEvent, Worker};

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
        _on_error: Closure<dyn FnMut(ErrorEvent)>,
    },
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
            Self::Worker { _worker, port, .. } => {
                let result = Pin::new(port).poll_shutdown(cx);
                if matches!(result, Poll::Ready(_)) {
                    // A MessagePort has no peer-close event. Once the client
                    // endpoint is shut down there is no reliable signal for
                    // the server task to observe, so terminate the sole
                    // authoritative Worker explicitly. `Drop` repeats this
                    // as a guard for callers that discard the transport
                    // without polling shutdown.
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
        if let Self::Worker { _worker, .. } = self {
            // `Worker` has no Rust-side join handle, and dropping the JS
            // wrapper does not guarantee that its event loop stops. Terminate
            // the one authoritative server worker when the client endpoint is
            // dropped; this prevents a quit/rejoin cycle from leaving an old
            // world ticking behind the page. Startup failures already
            // terminate explicitly in `launch_browser_worker`.
            _worker.terminate();
        }
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
/// The transferred port contains protocol bytes only. The ordinary Worker
/// channel carries this launch object and its ready/error response, which gives
/// us a precise point at which startup failure must be reported: no worker
/// world exists before `ready`, so constructing a page-owned replacement would
/// violate the browser singleplayer scheduling boundary.
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

#[cfg(target_arch = "wasm32")]
pub(super) async fn launch_browser_worker(
    protocol: i32,
    seed: i64,
    preset: crate::menu::create_world::WorldTypePreset,
) -> Result<BrowserIntegratedTransport, String> {
    let worker = Worker::new("lodestone-server-worker.js")
        .map_err(|e| e.as_string().unwrap_or_else(|| "cannot create server worker".to_string()))?;
    let channel = MessageChannel::new()
        .map_err(|e| e.as_string().unwrap_or_else(|| "cannot create worker channel".to_string()))?;
    let page_port = channel.port1();
    let worker_port = channel.port2();
    // Build this before waiting for `ready` so the worker error callback can
    // wake a client read after startup. A MessagePort itself has no close event.
    let transport = lodestone_net::MessagePortTransport::new(page_port);
    let port_shutdown = transport.shutdown_handle();
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
    let transfer = js_sys::Array::new();
    transfer.push(&worker_port);
    worker
        .post_message_with_transfer(&launch, &transfer)
        .map_err(|e| {
            e.as_string()
                .unwrap_or_else(|| "cannot transfer server port to worker".to_string())
        })?;
    let startup = ready_rx
        .await
        .map_err(|_| "server worker stopped during startup".to_string())?;
    worker.set_onmessage(None);
    drop(on_message);
    if let Err(error) = startup {
        worker.set_onerror(None);
        drop(on_error);
        worker.terminate();
        return Err(error);
    }
    Ok(BrowserIntegratedTransport::Worker {
        _worker: worker,
        port: transport,
        _on_error: on_error,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        BrowserWorkerErrorAction, BrowserWorkerStartupAction, BrowserWorkerStartupState,
        browser_worker_error_action,
    };

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
