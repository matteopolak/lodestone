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
            init_logging();
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
fn init_logging() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}
