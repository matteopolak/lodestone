//! Runtime configuration for the game shell, parsed from argv.
//!
//! Kept tiny and version-free: the shell never names a protocol version. The
//! only network knob is `protocol`, a *number* the shell hands to
//! [`lodestone_registry::adapter_for_protocol`] — the registry decides which
//! version crate (if any) answers it.

use std::time::Duration;

/// How the binary should run this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Open a window and run the interactive game loop.
    Window,
    /// Run headless: bring up the GPU, render one frame of the local world to an
    /// offscreen target, read the pixels back, and print the debug stats. This
    /// is the evidence path when no window server is reachable.
    Headless,
    /// Connect to the server, stream events for a bounded time, print them, and
    /// exit. Proves the live pipeline end to end without a GPU.
    Connect,
}

/// Parsed shell configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// What to do this run.
    pub mode: Mode,
    /// Server host to connect to (when connecting).
    pub host: String,
    /// Server port.
    pub port: u16,
    /// Protocol *number* to request an adapter for. `776` is vanilla 26.2.
    pub protocol: i32,
    /// Render distance in chunks (drives the camera far plane and worldgen span).
    pub render_distance: u32,
    /// Whether to also open a live connection while the window is up.
    pub connect_in_window: bool,
    /// How long the `Connect` mode streams events before exiting.
    pub connect_for: Duration,
    /// Mouse-look sensitivity as a vanilla `0..1` slider (fed through the cubic
    /// response curve in [`lodestone_controller::sensitivity_factor`]). `0.5` is the
    /// vanilla default and yields `0.15°`/pixel.
    pub sensitivity: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Window,
            host: "127.0.0.1".into(),
            port: 25565,
            protocol: 776,
            render_distance: 8,
            connect_in_window: false,
            connect_for: Duration::from_secs(15),
            sensitivity: 0.5,
        }
    }
}

impl Config {
    /// The outcome of parsing argv: either a runnable [`Config`], a request to
    /// print `--help`, or an error for an unrecognised argument. Help and errors
    /// are resolved by `main` **before** any window, GPU, or world init, so the
    /// binary is discoverable and `./lodestone --help` never opens a window.
    ///
    /// Recognised flags:
    /// `--headless`, `--connect`, `--window`, `--host <h>`, `--port <p>`,
    /// `--protocol <n>`, `--render-distance <n>`, `--live` (connect while
    /// windowed), `--seconds <n>`, `--sensitivity <f>`, `--help`/`-h`.
    #[must_use]
    pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> CliOutcome {
        let mut cfg = Config::default();
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--help" | "-h" => return CliOutcome::Help(Self::usage()),
                "--headless" => cfg.mode = Mode::Headless,
                "--connect" => cfg.mode = Mode::Connect,
                "--window" => cfg.mode = Mode::Window,
                "--live" => cfg.connect_in_window = true,
                "--host" => {
                    if let Some(v) = it.next() {
                        cfg.host = v;
                    }
                }
                "--port" => {
                    if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                        cfg.port = v;
                    }
                }
                "--protocol" => {
                    if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                        cfg.protocol = v;
                    }
                }
                "--render-distance" | "--rd" => {
                    if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                        cfg.render_distance = v;
                    }
                }
                "--seconds" => {
                    if let Some(v) = it.next().and_then(|v| v.parse::<u64>().ok()) {
                        cfg.connect_for = Duration::from_secs(v);
                    }
                }
                "--sensitivity" => {
                    if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                        cfg.sensitivity = v;
                    }
                }
                other => {
                    return CliOutcome::Error(format!("unrecognised argument: {other}"));
                }
            }
        }
        CliOutcome::Run(cfg)
    }

    /// The `--help` usage text. Kept in one place so the flag list can't drift
    /// from the parser above.
    #[must_use]
    pub fn usage() -> String {
        "\
lodestone — a multi-version Minecraft Java client (game shell)

USAGE:
    lodestone [OPTIONS]

MODES (default: --window):
    --window                 Open a window and play the interactive game loop
    --headless               Render one offscreen frame, print debug stats, exit
    --connect                Stream live server events for a bounded time, exit
    --live                   Also open a live connection while windowed

CONNECTION:
    --host <HOST>            Server host (default: 127.0.0.1)
    --port <PORT>            Server port (default: 25565)
    --protocol <N>           Protocol number to request an adapter for
                             (default: 776 = vanilla 26.2). Requires the `live`
                             build feature for an adapter to be compiled in.
    --seconds <N>            How long --connect streams before exiting (default: 15)

RENDER / INPUT:
    --render-distance <N>    Render distance in chunks (default: 8); also --rd
    --sensitivity <F>        Mouse-look sensitivity, 0..1 (default: 0.5)

    -h, --help               Print this help and exit
"
        .to_string()
    }
}

/// The result of parsing command-line arguments.
#[derive(Debug, Clone)]
pub enum CliOutcome {
    /// Parsed successfully; run the shell with this config.
    Run(Config),
    /// `--help`/`-h` was requested; the payload is the usage text to print.
    Help(String),
    /// An argument was not recognised; the payload is the error message.
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(a: &[&str]) -> Config {
        match Config::from_args(a.iter().map(|s| (*s).to_string())) {
            CliOutcome::Run(c) => c,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn defaults_are_window_and_262() {
        let c = Config::default();
        assert_eq!(c.mode, Mode::Window);
        assert_eq!(c.protocol, 776);
        assert_eq!(c.port, 25565);
    }

    #[test]
    fn flags_parse() {
        let c = parse(&[
            "--headless",
            "--host",
            "example",
            "--port",
            "1234",
            "--rd",
            "16",
        ]);
        assert_eq!(c.mode, Mode::Headless);
        assert_eq!(c.host, "example");
        assert_eq!(c.port, 1234);
        assert_eq!(c.render_distance, 16);
    }

    #[test]
    fn connect_and_live() {
        let c = parse(&["--connect", "--seconds", "3", "--live"]);
        assert_eq!(c.mode, Mode::Connect);
        assert_eq!(c.connect_for.as_secs(), 3);
        assert!(c.connect_in_window);
    }

    #[test]
    fn bad_values_keep_defaults() {
        let c = parse(&["--port", "notanumber"]);
        assert_eq!(c.port, 25565);
    }

    #[test]
    fn help_flag_requests_help_before_anything_runs() {
        // Both spellings must short-circuit to Help, never a runnable Config —
        // this is what stops `./lodestone --help` from opening a window.
        for flag in ["--help", "-h"] {
            match Config::from_args([flag.to_string()]) {
                CliOutcome::Help(text) => {
                    // The usage must actually document the flags, not be a stub,
                    // or it's the "green output that isn't evidence" shape.
                    assert!(text.contains("USAGE"), "usage missing header: {text}");
                    assert!(text.contains("--headless"), "usage omits --headless");
                    assert!(text.contains("--host"), "usage omits --host");
                }
                other => panic!("expected Help for {flag}, got {other:?}"),
            }
        }
    }

    #[test]
    fn unrecognised_argument_errors_rather_than_being_ignored() {
        // A stray/unknown token must be an explicit error (resolved before init)
        // rather than silently dropped — otherwise typos launch the default run.
        match Config::from_args(["--frobnicate".to_string()]) {
            CliOutcome::Error(msg) => assert!(msg.contains("--frobnicate"), "msg: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
        match Config::from_args(["stray".to_string()]) {
            CliOutcome::Error(msg) => assert!(msg.contains("stray"), "msg: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
