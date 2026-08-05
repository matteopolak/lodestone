//! Binary entry point for the Lodestone game shell.
//!
//! Intentionally minimal: initialise logging, parse the version-free [`Config`]
//! from argv, and hand off to [`lodestone::app::run`]. **No `#[global_allocator]`
//! is set** — the plan's measured verdict on this machine is that the system
//! allocator is the right default, and a library must never dictate one anyway.

use lodestone::{CliOutcome, Config, app};

fn main() -> anyhow::Result<()> {
    // Resolve --help and argument errors *before* logging, a window, a GPU, or a
    // world exist — so the binary is discoverable and `--help` never boots a game.
    match Config::from_args(std::env::args().skip(1)) {
        CliOutcome::Run(mut config) => {
            let _chrome_guard = init_logging();
            // Fold `options.json` into the argv-parsed config, for the settings
            // that live in both (issue #443). An explicit flag still wins for
            // this run; everything else takes the persisted value, so the
            // consumers in `sim`/`app` read the resolved number without knowing
            // a settings screen exists. Must happen before `app::run`, which
            // hands `config` straight to `Sim`.
            config.resolve_persisted(&lodestone::config::Options::load());
            tracing::info!(?config.mode, "starting lodestone");
            app::run(config)
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

/// Initialise `tracing` from `RUST_LOG`, defaulting to `info`.
/// When `LODESTONE_TRACE` is set, also writes a chrome://tracing flamegraph
/// to the named file.
fn init_logging() -> Option<tracing_chrome::FlushGuard> {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::prelude::*;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let trace_path = std::env::var("LODESTONE_TRACE").ok().filter(|p| !p.is_empty());

    if let Some(path) = trace_path {
        let (chrome_layer, guard) = tracing_chrome::ChromeLayerBuilder::new().file(&path).build();
        let _ = tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_filter(filter))
            .with(chrome_layer)
            .try_init();
        tracing::info!("chrome trace writing to {path}");
        Some(guard)
    } else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .try_init();
        None
    }
}
