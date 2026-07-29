//! Runtime configuration for the game shell, parsed from argv, plus the
//! **persisted** user options ([`Options`]) that survive a restart.
//!
//! Kept tiny and version-free: the shell never names a protocol version. The
//! only network knob is `protocol`, a *number* the shell hands to
//! [`lodestone_registry::adapter_for_protocol`] — the registry decides which
//! version crate (if any) answers it.
//!
//! ## GUI scale
//!
//! [`calculate_gui_scale`] reproduces vanilla's `Window.calculateScale`
//! (`.cache/mc/26.2/client-src/com/mojang/blaze3d/platform/Window.java:445-463`)
//! exactly: an integer scale, `0` meaning "auto" (pick the largest scale that
//! still fits a minimum logical size), clamped so the framebuffer is never
//! divided into less than that minimum. See the function's own docs for the
//! one deliberate omission (the legacy `enforceUnicode` even-scale rounding).
//!
//! [`Options`] is the persisted settings model built on top of it — today just
//! `gui_scale`. It is written to `options.json` next to `servers.json`, in the
//! **same** platform data directory [`crate::menu::servers::data_dir`] already
//! discovers; see [`options_path`]. That reuse is deliberate — see that
//! module's docs for why the directory lookup lives there and not here.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Sentinel `gui_scale` value meaning "auto": the largest integer scale that
/// still fits [`MIN_SCALED_WIDTH`]x[`MIN_SCALED_HEIGHT`] into the framebuffer.
/// Matches vanilla's `Options.AUTO_GUI_SCALE`
/// (`.cache/mc/26.2/client-src/net/minecraft/client/Options.java:903`).
pub const AUTO_GUI_SCALE: u32 = 0;

/// Vanilla clamps the auto-picked (and any manual) scale so the resulting
/// *scaled* GUI resolution never drops below this many logical pixels wide —
/// `Window.calculateScale`'s `>= 320` check (`Window.java:452`).
const MIN_SCALED_WIDTH: u32 = 320;
/// As [`MIN_SCALED_WIDTH`], vertical — `Window.java:453`'s `>= 240`.
const MIN_SCALED_HEIGHT: u32 = 240;

/// Highest `gui_scale` the settings screen will manually cycle to. Vanilla's
/// own ceiling is effectively unbounded (`Options.MAX_GUI_SCALE_INCLUSIVE` =
/// `2147483646`, `Options.java:904`) and its slider's *dynamic* max is
/// `calculate_gui_scale(AUTO_GUI_SCALE, ..)` for the live window
/// (`Options.java:909-912`) — but that means threading the live framebuffer
/// size into the menu's pure, GPU-free navigation layer just to bound a
/// cycle. [`calculate_gui_scale`] still clamps the *effective* scale to
/// whatever the framebuffer actually fits regardless of what is requested, so
/// a manual value above what the window can show just saturates rather than
/// doing anything unsafe or even visible.
pub const MAX_MANUAL_GUI_SCALE: u32 = 8;

/// Reproduces `Window.calculateScale(maxScale, enforceUnicode)`
/// (`.cache/mc/26.2/client-src/com/mojang/blaze3d/platform/Window.java:445-463`)
/// exactly, **minus** the legacy `enforceUnicode` even-scale rounding:
/// Lodestone has no unicode-font mode to enforce (that option exists in
/// vanilla only for a legacy glyph-page font), so the branch is dropped
/// rather than wired to a setting that would always read `false`.
///
/// `desired` is the persisted `gui_scale` option: [`AUTO_GUI_SCALE`] (`0`)
/// means "pick the largest scale that fits" (vanilla passes `maxScale = 0` for
/// auto and relies on `scale` — which only ever counts up from `1` — never
/// equalling it; reproduced here with an unreachable ceiling rather than a
/// literal `0` so the same loop serves both cases without a divide-by-zero).
/// Any other value is a hard upper bound that itself gets reduced if the
/// framebuffer is too small for it.
///
/// `framebuffer_width`/`framebuffer_height` must be **physical** pixels — what
/// winit calls `PhysicalSize`, i.e. already DPI-scaled — matching vanilla's
/// `Window.framebufferWidth/Height`. That is the only place a display's DPI
/// factor enters this model: there is no separate "DPI scale" input, because
/// on a Retina/HiDPI display the physical framebuffer size already *is* the
/// logical window size times the OS scale factor. Dividing the framebuffer by
/// the returned integer scale (what vanilla calls `guiScaledWidth/Height`,
/// `Window.java:465-472`) is what turns a fixed-pixel-sized menu layout into
/// the right *visual* size instead of half-size on a 2x display.
#[must_use]
pub fn calculate_gui_scale(desired: u32, framebuffer_width: u32, framebuffer_height: u32) -> u32 {
    // Vanilla's `scale != maxScale` loop guard never fires for `maxScale == 0`
    // because `scale` starts at 1 and only increases — `i32::MAX` reproduces
    // that "unreachable ceiling" behaviour for `desired == AUTO_GUI_SCALE`.
    let ceiling = if desired == AUTO_GUI_SCALE {
        i32::MAX
    } else {
        desired as i32
    };
    let fb_w = framebuffer_width as i32;
    let fb_h = framebuffer_height as i32;
    let mut scale: i32 = 1;
    while scale != ceiling
        && scale < fb_w
        && scale < fb_h
        && fb_w / (scale + 1) >= MIN_SCALED_WIDTH as i32
        && fb_h / (scale + 1) >= MIN_SCALED_HEIGHT as i32
    {
        scale += 1;
    }
    // Vanilla can return a `0` framebuffer-sized scale (e.g. an iconified
    // window reporting 0x0); a menu that divides by that would be a fresh
    // crash, so the effective scale is floored at 1 here rather than in every
    // caller.
    scale.max(1) as u32
}

/// Persisted user settings that must survive a restart — distinct from
/// [`Config`], which is parsed fresh from argv every run and never written
/// back. Currently just the GUI scale; add fields here as more settings need
/// to persist, following [`crate::menu::servers::ServerList`]'s rule that a
/// missing or corrupt file is the default, never an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The user's chosen `gui_scale`: [`AUTO_GUI_SCALE`] or an explicit
    /// ceiling. This is fed to [`calculate_gui_scale`] against the live
    /// framebuffer size — never used directly as a pixel count.
    pub gui_scale: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            gui_scale: AUTO_GUI_SCALE,
        }
    }
}

impl Options {
    /// Loads from the real on-disk location ([`options_path`]). Missing or
    /// corrupt is the default, never an error — a broken settings file must
    /// not stop the game from launching, same rule as the server list.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(&options_path())
    }

    /// As [`Options::load`], from an explicit path (for tests, so nothing
    /// touches the developer's real settings file).
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path).map_or_else(|_| Self::default(), |t| Self::from_json(&t))
    }

    fn from_json(text: &str) -> Self {
        let Ok(serde_json::Value::Object(obj)) = serde_json::from_str(text) else {
            return Self::default();
        };
        let gui_scale = obj
            .get("gui_scale")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(AUTO_GUI_SCALE);
        Self { gui_scale }
    }

    /// Writes to the real on-disk location.
    ///
    /// # Errors
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&options_path())
    }

    /// As [`Options::save`], to an explicit path (for tests).
    ///
    /// # Errors
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut obj = serde_json::Map::new();
        obj.insert("gui_scale".into(), self.gui_scale.into());
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(obj))
            .unwrap_or_else(|_| "{}".to_string());
        std::fs::write(path, text)
    }
}

/// Full path to the persisted options file — alongside `servers.json` in the
/// same platform data directory. Deliberately reuses
/// [`crate::menu::servers::data_dir`]'s discovery (and its
/// `LODESTONE_DATA_DIR` override) rather than inventing a second one.
#[must_use]
pub fn options_path() -> PathBuf {
    crate::menu::servers::data_dir().join("options.json")
}

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
    /// Whether argv actually named a connection target (`--host` or `--port`),
    /// as opposed to [`Self::host`]/[`Self::port`] merely holding their defaults.
    ///
    /// Recorded as its own flag because the *value* cannot answer the question:
    /// `--host 127.0.0.1 --port 25565` is byte-identical to passing nothing, and
    /// `app::requested_a_connection` used to compare against `Config::default()`
    /// and therefore sent that launch to the main menu instead of the server the
    /// user named. Not set by `--live`, which has its own flag.
    pub address_given: bool,
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
            address_given: false,
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
                        cfg.address_given = true;
                    }
                }
                "--port" => {
                    if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                        cfg.port = v;
                        cfg.address_given = true;
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
    Naming either of these connects on launch and skips the main menu — even when
    the value given is the default one.
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
    fn spelling_out_the_default_address_still_counts_as_asking_for_a_connection() {
        // The launch behind the two-worlds report: `app::requested_a_connection`
        // compared the *values* against `Config::default()`, so this argv was
        // indistinguishable from passing nothing and landed on the main menu.
        let c = parse(&["--host", "127.0.0.1", "--port", "25565"]);
        assert_eq!(c.host, Config::default().host, "same value as the default");
        assert_eq!(c.port, Config::default().port, "same value as the default");
        assert!(
            c.address_given,
            "the flag was seen, which is the question the menu bypass asks"
        );
        // Control: no address flag at all must stay false, or the field is a
        // constant `true` and cannot distinguish anything.
        assert!(!parse(&["--window"]).address_given);
        assert!(
            !parse(&["--live"]).address_given,
            "--live has its own flag; it must not imply an address was named"
        );
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

    // -- GUI scale ----------------------------------------------------------
    //
    // Expected values are hand-derived from vanilla's own algebra (the largest
    // integer S with `fb_w/S >= 320` and `fb_h/S >= 240`), not by re-tracing
    // this implementation, so a bug shared between the spec-reading and the
    // port would not cancel out.

    #[test]
    fn auto_scale_matches_vanillas_default_854x480_window() {
        // Vanilla's own default window is 854x480. Height is the binding
        // constraint: the largest S with 480/S >= 240 is S=2 (480/3=160 fails);
        // width allows up to S=2 as well (854/3=284 < 320). So S=2.
        assert_eq!(calculate_gui_scale(AUTO_GUI_SCALE, 854, 480), 2);
    }

    #[test]
    fn auto_scale_at_1280x720() {
        // Height binds: largest S with 720/S >= 240 is S=3 (720/4=180 fails).
        // Width would allow S=4 (1280/4=320 exactly), so height wins: S=3.
        assert_eq!(calculate_gui_scale(AUTO_GUI_SCALE, 1280, 720), 3);
    }

    #[test]
    fn auto_scale_at_4k_is_the_retina_style_case() {
        // A framebuffer this large is what a HiDPI/Retina display reports for
        // an ordinary-looking window — this is the case the menu's "half size
        // on Retina" report was about. Height binds: largest S with
        // 2160/S >= 240 is S=9 (2160/10=216 fails); width allows S=12
        // (3840/12=320 exactly), so S=9.
        assert_eq!(calculate_gui_scale(AUTO_GUI_SCALE, 3840, 2160), 9);
    }

    #[test]
    fn a_manual_scale_is_honoured_when_the_window_is_big_enough() {
        assert_eq!(calculate_gui_scale(2, 1280, 720), 2);
        assert_eq!(calculate_gui_scale(5, 3840, 2160), 5);
    }

    #[test]
    fn a_manual_scale_is_clamped_down_by_a_small_window() {
        // 200/2 = 100 < 320, so even a request for scale 2 cannot be honoured;
        // the window is too small for anything but scale 1.
        assert_eq!(calculate_gui_scale(2, 200, 200), 1);
    }

    #[test]
    fn scale_never_drops_below_one_even_for_a_degenerate_framebuffer() {
        // An iconified/minimised window can report 0x0; the menu must not
        // divide by zero laying itself out against that.
        assert_eq!(calculate_gui_scale(AUTO_GUI_SCALE, 0, 0), 1);
        assert_eq!(calculate_gui_scale(AUTO_GUI_SCALE, 1, 1), 1);
    }

    // -- persisted options ---------------------------------------------------

    fn temp_options_path(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lodestone-config-{}-{tag}/options.json",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        path
    }

    #[test]
    fn options_default_to_auto_scale() {
        assert_eq!(Options::default().gui_scale, AUTO_GUI_SCALE);
    }

    #[test]
    fn options_round_trip_through_a_real_file() {
        let path = temp_options_path("roundtrip");
        let opts = Options { gui_scale: 3 };
        opts.save_to(&path).expect("save should create parents");
        assert_eq!(Options::load_from(&path), opts);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_or_corrupt_options_file_is_the_default_not_an_error() {
        assert_eq!(
            Options::load_from(Path::new("/nonexistent/options.json")),
            Options::default()
        );
        let path = temp_options_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "}{ not json").unwrap();
        assert_eq!(Options::load_from(&path), Options::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_options_path_lives_beside_the_server_list() {
        // Same directory, same discovery — see the module docs on why this
        // must not invent a second config location.
        assert_eq!(
            options_path().parent(),
            crate::menu::servers::servers_path().parent()
        );
        assert_eq!(options_path().file_name().unwrap(), "options.json");
    }
}
