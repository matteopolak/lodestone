//! The three run modes: windowed, headless PPM capture, and connect-only.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

pub(super) fn run_windowed(config: Config) -> anyhow::Result<()> {
    run_windowed_with_app(Sim::client_app(), config)
}

/// [`run_windowed`], around a caller-composed [`lodestone_app::App`] instead of
/// [`Sim::client_app`]'s own — the entry point a downstream crate reaches through
/// [`crate::run_with_app`] to register a plugin into the real, on-screen client.
/// Everything past `WindowApp::new_with_app` is identical to `run_windowed`: the
/// composed `App` only changes what `Sim` the constructed `WindowApp` holds, never
/// how the winit loop drives it.
pub(super) fn run_windowed_with_app(
    mut plugin_app: lodestone_app::App,
    config: Config,
) -> anyhow::Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    crate::wasm_plugins::install_from_directory(
        &mut plugin_app,
        std::path::Path::new(lodestone_wasm_host::DEFAULT_PLUGIN_DIR),
    )?;

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

    // Native: `run_app` takes over this thread and returns when the loop exits.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut app = app;
        event_loop.run_app(&mut app)?;
        Ok(())
    }

    // Browser: `spawn_app` instead, and the difference is not cosmetic. `run_app`
    // never returns — on wasm winit implements that by throwing a JS exception to
    // unwind out of Rust, which works but shows up in the console as an uncaught
    // error and runs no destructors. `spawn_app` takes ownership of the app, hands
    // the loop to the browser's own event loop, and **returns immediately**, so the
    // caller (`web/`) keeps running normally.
    //
    // That is why this function still returns `Ok(())` here rather than blocking:
    // the game is now live and driven by `requestAnimationFrame`, and nothing after
    // this point may assume the session has ended.
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(app);
        Ok(())
    }
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
