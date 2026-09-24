//! Binary entry point for the Lodestone game shell.
//!
//! Intentionally minimal: initialise logging, parse the version-free [`Config`]
//! from argv, and hand off to [`lodestone::app::run`]. **No `#[global_allocator]`
//! is set** — the plan's measured verdict on this machine is that the system
//! allocator is the right default, and a library must never dictate one anyway.

//! ## The browser has a different entry point, and this file is not it
//!
//! Everything below is native-only. Not because it would fail to compile — it is
//! `argv` parsing and `tracing` setup — but because **none of it means anything in a
//! browser**: there is no command line to read (`std::env::args` yields just the
//! program name on wasm32), no stderr for the `fmt` subscriber to reach, no file for
//! `tracing-chrome` to write, and no `options.json` to fold in. The browser's
//! equivalent lives in `web/`, which fetches the asset bundle, installs it through
//! `lodestone::platform::assets`, and installs `console_log` in place of the
//! subscriber below.
//!
//! The `wasm32` arm is therefore a deliberately empty `main`. It exists only because
//! `cargo check --target wasm32-unknown-unknown` builds every target in the package,
//! including this `[[bin]]`, and a `bin` without a `main` is an error. It is never
//! run: `trunk` builds `web/`'s own crate, not this one. The **library** target is
//! the whole of what a browser consumes, and it is the thing that has to stay
//! wasm-clean.

// `lodestone::run`, not `lodestone::app::run` directly: `app` (and every
// winit type in it) compiles in only behind the `window` Cargo feature, so
// `app::run` does not exist for a `--no-default-features` build. `run`
// dispatches to it when `window` is on and to `lodestone::diagnostics`
// directly when it is off — see `lodestone`'s own crate doc.
#[cfg(not(target_arch = "wasm32"))]
use lodestone::{CliOutcome, Config, Mode, run};

/// The browser build's do-nothing `main`. See this module's docs — `web/` is the
/// real entry point, and this only satisfies the `[[bin]]` target.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    // Resolve --help and argument errors *before* logging, a window, a GPU, or a
    // world exist — so the binary is discoverable and `--help` never boots a game.
    match Config::from_args(std::env::args().skip(1)) {
        CliOutcome::Run(mut config) => {
            let _chrome_guard = init_logging(config.mode);
            // Fold `options.json` into the argv-parsed config, for the settings
            // that live in both. An explicit flag still wins for
            // this run; everything else takes the persisted value, so the
            // consumers in `sim`/`app` read the resolved number without knowing
            // a settings screen exists. Must happen before `run`, which
            // hands `config` straight to `Sim`.
            config.resolve_persisted(&lodestone::config::Options::load());
            tracing::info!(?config.mode, "starting lodestone");
            run(config)
        }
        CliOutcome::Help(text) => {
            print!("{text}");
            Ok(())
        }
        CliOutcome::Error(msg) => {
            eprintln!("error: {msg}\n");
            eprint!("{}", Config::usage());
            std::process::exit(2);
        }
    }
}

/// Where the process-wide tracing formatter writes its text output.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogDestination {
    Stdout,
    Stderr,
}

/// Keep tracing off the stream every non-window presentation surface owns.
#[cfg(not(target_arch = "wasm32"))]
fn logging_destination(mode: Mode) -> LogDestination {
    match mode {
        Mode::Window => LogDestination::Stdout,
        Mode::Headless | Mode::Connect | Mode::Stdio | Mode::Terminal => LogDestination::Stderr,
        #[cfg(feature = "runtime-presentation")]
        Mode::HeadlessSession => LogDestination::Stderr,
    }
}

/// Initialise `tracing` from `RUST_LOG`, defaulting to `info`.
/// When `LODESTONE_TRACE` is set, also writes a chrome://tracing flamegraph
/// to the named file. Only the window surface leaves stdout available to the
/// formatter; every other surface sends diagnostics to stderr instead.
#[cfg(not(target_arch = "wasm32"))]
fn init_logging(mode: Mode) -> Option<tracing_chrome::FlushGuard> {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::prelude::*;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let trace_path = std::env::var("LODESTONE_TRACE").ok().filter(|p| !p.is_empty());

    if let Some(path) = trace_path {
        let (chrome_layer, guard) = tracing_chrome::ChromeLayerBuilder::new().file(&path).build();
        match logging_destination(mode) {
            LogDestination::Stdout => {
                let _ = tracing_subscriber::registry()
                    .with(chrome_layer)
                    .with(tracing_subscriber::fmt::layer().with_filter(filter))
                    .try_init();
            }
            LogDestination::Stderr => {
                let _ = tracing_subscriber::registry()
                    .with(chrome_layer)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .with_writer(std::io::stderr)
                            .with_filter(filter),
                    )
                    .try_init();
            }
        }
        tracing::info!("chrome trace writing to {path}");
        Some(guard)
    } else {
        match logging_destination(mode) {
            LogDestination::Stdout => {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_target(false)
                    .try_init();
            }
            LogDestination::Stderr => {
                let _ = tracing_subscriber::fmt()
                    .with_writer(std::io::stderr)
                    .with_env_filter(filter)
                    .with_target(false)
                    .try_init();
            }
        }
        None
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn only_the_window_surface_leaves_stdout_for_tracing() {
        assert_eq!(logging_destination(Mode::Stdio), LogDestination::Stderr);
        assert_eq!(logging_destination(Mode::Terminal), LogDestination::Stderr);
        assert_eq!(logging_destination(Mode::Headless), LogDestination::Stderr);
        assert_eq!(logging_destination(Mode::Connect), LogDestination::Stderr);
        #[cfg(feature = "runtime-presentation")]
        assert_eq!(
            logging_destination(Mode::HeadlessSession),
            LogDestination::Stderr
        );
        assert_eq!(logging_destination(Mode::Window), LogDestination::Stdout);
    }
}
