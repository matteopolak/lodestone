//! The three run modes: windowed, headless PPM capture, and connect-only.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn run_windowed(config: Config) -> anyhow::Result<()> {
    run_windowed_with_app(Sim::client_app(), config)
}

#[cfg(all(target_arch = "wasm32", feature = "runtime-presentation"))]
pub(super) fn run_offscreen_with_control(
    plugin_app: lodestone_app::App,
    config: Config,
    canvas: web_sys::OffscreenCanvas,
    lifecycle: Rc<Cell<bool>>,
) -> anyhow::Result<BrowserControl> {
    let frame_signal = BrowserFrameSignal::new();
    let task_lifecycle = Rc::clone(&lifecycle);
    let shutdown = Rc::new(Cell::new(false));
    let task_shutdown = Rc::clone(&shutdown);
    let input = Rc::new(RefCell::new(BrowserInputQueue::default()));
    let task_input = Rc::clone(&input);
    let actions = Rc::new(RefCell::new(BrowserActionQueue::default()));
    let task_actions = Rc::clone(&actions);
    let task_signal = frame_signal.clone();
    wasm_bindgen_futures::spawn_local(async move {
        match lodestone_render::window::attach_offscreen_canvas_async(canvas).await {
            Ok((gpu, target)) if !task_lifecycle.get() && !task_shutdown.get() => {
                let mut app = WindowApp::new_with_app_and_offscreen(
                    plugin_app,
                    config,
                    Rc::clone(&task_lifecycle),
                    task_signal,
                    Rc::clone(&task_actions),
                );
                app.finish_bring_up(
                    None,
                    gpu,
                    super::PresentationTarget::Surface(target),
                );
                let mut pending_input = Vec::new();
                while !task_lifecycle.get() && !task_shutdown.get() {
                    task_input.borrow_mut().drain_into(&mut pending_input);
                    for input in pending_input.drain(..) {
                        if app.dispatch_browser_input(input) {
                            task_shutdown.set(true);
                            break;
                        }
                    }
                    if task_shutdown.get() {
                        break;
                    }
                    app.redraw();
                    if app.ui.quit_requested() {
                        task_shutdown.set(true);
                        break;
                    }
                    if browser_delay(16).await.is_err() {
                        task_shutdown.set(true);
                        break;
                    }
                }
                app.shutdown_browser_presentation();
            }
            Ok((gpu, _target)) => {
                gpu.device().destroy();
                task_lifecycle.set(true);
            }
            Err(error) => {
                tracing::error!(target: "gpu", "failed to attach GPU to offscreen canvas: {error}");
                task_lifecycle.set(true);
            }
        }
    });
    Ok(BrowserControl { lifecycle, shutdown, frame_signal, input, actions })
}

#[cfg(all(target_arch = "wasm32", feature = "runtime-presentation"))]
async fn browser_delay(milliseconds: i32) -> Result<(), wasm_bindgen::JsValue> {
    use js_sys::{Function, Promise, Reflect};
    use wasm_bindgen::{JsCast, closure::Closure};
    use wasm_bindgen_futures::JsFuture;

    let promise = Promise::new(&mut |resolve, reject| {
        let global = js_sys::global();
        let timeout = Reflect::get(&global, &wasm_bindgen::JsValue::from_str("setTimeout"))
            .and_then(|value| {
                value
                    .dyn_into::<Function>()
                    .map_err(|_| wasm_bindgen::JsValue::from_str("setTimeout is unavailable"))
            });
        let timeout = match timeout {
            Ok(timeout) => timeout,
            Err(error) => {
                let _ = reject.call1(&wasm_bindgen::JsValue::NULL, &error);
                return;
            }
        };
        let callback = Closure::once_into_js(move || {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        });
        if let Err(error) = timeout.call2(
            &global,
            &callback,
            &wasm_bindgen::JsValue::from_f64(f64::from(milliseconds.max(0))),
        ) {
            let _ = reject.call1(&wasm_bindgen::JsValue::NULL, &error);
        }
    });
    JsFuture::from(promise).await.map(|_| ())
}

/// [`run_windowed`], around a caller-composed [`lodestone_app::App`] instead of
/// [`Sim::client_app`]'s own — the entry point a downstream crate reaches through
/// [`crate::run_with_app`] to register a plugin into the real, on-screen client.
/// Everything past `WindowApp::new_with_app` is identical to `run_windowed`: the
/// composed `App` only changes what `Sim` the constructed `WindowApp` holds, never
/// how the winit loop drives it.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn run_windowed_with_app(
    mut plugin_app: lodestone_app::App,
    config: Config,
) -> anyhow::Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let grants = match config.plugin_grants_path.as_deref() {
            Some(path) => crate::wasm_plugins::load_grants_from_file(path)?,
            None => lodestone_wasm_host::PluginGrantPolicy::default(),
        };
        crate::wasm_plugins::install_from_directory_with_grants(
            &mut plugin_app,
            std::path::Path::new(lodestone_wasm_host::DEFAULT_PLUGIN_DIR),
            &grants,
        )?;
    }

    // `EventLoop::<ShellEvent>::with_user_event().build()` rather than
    // `EventLoop::new()`: the two are identical when `ShellEvent = ()`
    // (`EventLoop::new()`'s own doc says it is an alias of
    // `EventLoop::builder().build()`, and `builder()` is itself
    // `with_user_event()` — see winit's `event_loop.rs`), so this changes
    // nothing here. It is what lets `run_headless_session` (below) hand a
    // `EventLoopProxy<AppEvent>` to a controlling thread while `ShellEvent`
    // is `AppEvent`.
    let event_loop = EventLoop::<ShellEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let app = WindowApp::new_with_app(plugin_app, config);

    let mut app = app;
    event_loop.run_app(&mut app)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Headless session: a real, persistent session with no
// presentation attached at start, controllable at runtime.
// ---------------------------------------------------------------------------

/// Native-only, like `run_connect`/`run_headless`: it needs a real OS event
/// loop and blocking stdin, neither of which a browser page has.
///
/// Unlike those two this is **not** a bounded diagnostic. It starts a session
/// exactly as `--window` does — real login, real ticking, real persistence —
/// except with no window, no GPU and no presentation-only ECS systems
/// attached (`WindowApp::new_headless_session`), and it stays running until
/// told to attach, detach, or quit. The event loop still owns this thread
/// (`run_app`'s platform requirement — macOS in particular runs the loop only
/// on the main thread), so runtime control comes from a second thread reading
/// stdin and forwarding commands through an `EventLoopProxy` — the sanctioned
/// way to reach a running `ApplicationHandler` from outside it (winit hands
/// every callback, `user_event` included, a live `&ActiveEventLoop`).
///
/// This is deliberately a plain, demonstrable control surface rather than a
/// public Rust API: a library caller wanting the same thing calls
/// `EventLoop::create_proxy()` and `EventLoopProxy::send_event` directly —
/// `WindowApp::user_event` (`app::lifecycle`) is the reusable mechanism, this
/// stdin loop is just one way to drive it for the shipped binary.
///
/// Recognised commands, one per line on stdin:
/// * `attach` — create a window with input inert (the resolved open
///   question: attach defaults to *not* driving the session).
/// * `attach input` — create a window with input live immediately.
/// * `arm` / `disarm` — toggle input on an already-attached window.
/// * `detach` — drop the window/GPU and the presentation-only ECS systems.
/// * `quit` — exit cleanly.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))]
pub(super) fn run_headless_session(
    _owned: lodestone_auth::Entitlement,
    config: Config,
) -> anyhow::Result<()> {
    let event_loop = EventLoop::<ShellEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let proxy = event_loop.create_proxy();

    println!(
        "lodestone headless session: ticking with no window. Commands (stdin, one per \
         line): attach | attach input | arm | disarm | detach | quit"
    );
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.read_line(&mut line) {
                Ok(0) => break, // stdin closed
                Ok(_) => {}
                Err(e) => {
                    eprintln!("headless session: stdin read failed: {e}");
                    break;
                }
            }
            let event = match line.trim() {
                "attach" => Some(AppEvent::AttachPresentation { enable_input: false }),
                "attach input" => Some(AppEvent::AttachPresentation { enable_input: true }),
                "arm" => Some(AppEvent::ArmInput(true)),
                "disarm" => Some(AppEvent::ArmInput(false)),
                "detach" => Some(AppEvent::DetachPresentation),
                "quit" => Some(AppEvent::Quit),
                "" => None,
                other => {
                    eprintln!(
                        "headless session: unrecognised command {other:?} — expected one of \
                         attach / \"attach input\" / arm / disarm / detach / quit"
                    );
                    None
                }
            };
            if let Some(event) = event {
                let quitting = matches!(event, AppEvent::Quit);
                // The event loop may already have exited (e.g. the window was
                // closed directly). A closed proxy just means there is nothing
                // left to control — not a bug in this thread.
                if proxy.send_event(event).is_err() {
                    break;
                }
                if quitting {
                    break;
                }
            }
        }
    });

    let mut app = WindowApp::new_headless_session(config);
    event_loop.run_app(&mut app)?;
    Ok(())
}

// `Mode::Headless` and `Mode::Connect` moved to `crate::diagnostics` — see
// that module's own doc for why they do not live here.
