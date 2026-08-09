//! The browser build of Lodestone — **the real game shell, not a spike.**
//!
//! This crate used to be a separate application with its own `main`, its own camera,
//! its own HUD and no `lodestone-shell` dependency at all: it decoded a committed
//! fixture of real `level_chunk_with_light` bytes and drew them. That is why the page
//! showed a "demo world" and had no main menu — the shell was never wired in. It is
//! now, and this file is all that is left of `web/`'s own code: everything you see on
//! screen comes from `lodestone-shell`, the same crate the native binary is built from.
//!
//! ## What this file is responsible for
//!
//! Exactly the three things a browser has to do that a native `main` does not, and
//! nothing else:
//!
//! 1. **Logging.** `console_log` in place of the native `tracing-subscriber` (there is
//!    no stderr) and `console_error_panic_hook` so a panic names its Rust location
//!    instead of `unreachable`.
//! 2. **Assets, as bytes.** Native scans for a pack root and `std::fs::read`s
//!    `client.jar`; a browser has no filesystem, so the bytes are `fetch`ed once and
//!    handed to [`lodestone::platform::assets::install`]. Everything downstream — the
//!    zip parser, the atlas builder, the model baker, the font loader — is the same
//!    synchronous code the native client runs. **The filesystem wall is crossed
//!    exactly once, here, at the byte source.**
//! 3. **Starting the app**, then getting out of the way. `lodestone::app::run` returns
//!    immediately in a browser (winit's `spawn_app` hands the loop to the page), so
//!    there is deliberately nothing after it.
//!
//! ## The ordering is load-bearing
//!
//! `install` must happen **before** `app::run`. `Config::resolve_persisted` and the
//! whole `resources::load_*` family are called during bring-up and each resolve their
//! assets lazily but *once*, memoised; a bundle installed after the first call would be
//! ignored and the session would run on the demo palette with no error. Fetching first
//! costs a few seconds of blank page and removes that failure mode entirely.
//!
//! ## What it does NOT do, and why that matters
//!
//! There is no fallback that draws something when the fetch fails. A missing jar
//! reports the failure and stops. The whole point of the port is that the browser runs
//! the real thing, and a synthetic stand-in on screen is indistinguishable from success
//! — which is the defect class this repo keeps paying for.

use lodestone::{CliOutcome, Config};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{Response, window};

/// The renderable corpus: blockstates, models, textures, lang, fonts, GUI sprites,
/// panorama, sounds index. Copied into `dist/` by trunk from the local
/// `.cache/mc/26.2` — see `index.html`, and `web/README.md` for how to populate it.
const CLIENT_JAR_URL: &str = "client.jar";

/// The block-state id table the atlas and the model baker are built against. Mojang's
/// own generator output; `BlocksJsonRegistry::from_slice` parses these bytes.
const BLOCKS_REPORT_URL: &str = "blocks.json";

/// Sets a status line in the boot overlay, if the page still has one.
///
/// The overlay exists only until the canvas has something on it; once the shell is
/// drawing, this is a no-op because `index.html` removes the element. Deliberately
/// tolerant of a missing element so the boot path never depends on the DOM shape.
fn status(text: &str) {
    if let Some(el) = window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("boot"))
    {
        el.set_text_content(Some(text));
    }
    log::info!("[boot] {text}");
}

/// Fetches a URL into bytes.
///
/// The only asynchronous step in the whole asset path — everything downstream is the
/// existing synchronous parser stack.
async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    let win = window().ok_or("no window")?;
    let resp_val = JsFuture::from(win.fetch_with_str(url))
        .await
        .map_err(|e| format!("fetch {url} failed: {e:?}"))?;
    let resp: Response = resp_val.dyn_into().map_err(|_| "not a Response")?;
    if !resp.ok() {
        return Err(format!("HTTP {} for {url}", resp.status()));
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("array_buffer for {url} failed: {e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Fetch both blobs and install them as the session's asset bundle.
async fn install_assets() -> Result<(), String> {
    status("fetching client.jar …");
    let client_jar = fetch_bytes(CLIENT_JAR_URL).await?;
    status(&format!(
        "client.jar {:.1} MiB — fetching blocks.json …",
        client_jar.len() as f64 / (1024.0 * 1024.0)
    ));
    let blocks_report = fetch_bytes(BLOCKS_REPORT_URL).await?;
    let sizes = format!(
        "assets ready: client.jar {:.1} MiB, blocks.json {:.1} MiB",
        client_jar.len() as f64 / (1024.0 * 1024.0),
        blocks_report.len() as f64 / (1024.0 * 1024.0),
    );
    lodestone::platform::assets::install(lodestone::platform::assets::Bundle {
        client_jar,
        blocks_report,
    })?;
    status(&sizes);
    Ok(())
}

/// Boot: install the assets, then start the real shell.
async fn boot() {
    if let Err(e) = install_assets().await {
        status(&format!(
            "ASSET LOAD FAILED — {e}. The browser build needs client.jar and \
             blocks.json served beside the page; see web/README.md. Nothing is drawn \
             on purpose, so this cannot be mistaken for a working session."
        ));
        return;
    }

    // The version-free `Config`, from an *empty* argument list. `std::env::args()` on
    // wasm32 yields only the program name, so there is nothing to parse and no
    // `--help`/error outcome to handle — but going through `from_args` rather than
    // `Config::default()` keeps one construction path, so a future query-string
    // front end lowers into the same parser the CLI uses.
    let mut config = match Config::from_args(std::iter::empty::<String>()) {
        CliOutcome::Run(config) => config,
        CliOutcome::Help(text) => {
            log::info!("{text}");
            return;
        }
        CliOutcome::Error(msg) => {
            status(&format!("config error: {msg}"));
            return;
        }
    };

    // Fold persisted settings in, exactly as `main.rs` does on native. This is the
    // read half of `platform::store`: in a browser it comes from `localStorage`, so a
    // player's four dozen options survive a reload.
    config.resolve_persisted(&lodestone::config::Options::load());

    status("starting the shell …");
    // Returns immediately: winit's `spawn_app` has handed the loop to the browser.
    // Nothing may go after this that assumes the session has ended.
    if let Err(e) = lodestone::app::run(config) {
        status(&format!("shell failed to start: {e}"));
    }
}

fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    spawn_local(boot());
}
