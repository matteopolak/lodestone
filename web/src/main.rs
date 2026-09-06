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
use lodestone_web::client_jar;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{Request, RequestCache, RequestInit, Response, window};

/// The renderable corpus: blockstates, models, textures, lang, fonts, GUI sprites,
/// sounds index. Copied into `dist/` by trunk from the local `.cache/mc/26.2` —
/// see `index.html`, and `web/README.md` for how to populate it.
///
/// **Does not carry the real title-screen panorama.** `client.jar` ships only a
/// 1×1 grey stub for each of the six panorama faces — the real 1024×1024 art is a
/// separate, content-addressed asset-object, fetched (best-effort) by
/// [`fetch_panorama_faces`] instead.
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

/// Removes the boot overlay, if it is still in the page.
///
/// Called just before the shell starts rather than after, because `app::run` returns
/// immediately in a browser (winit's `spawn_app` hands the loop to the page), so
/// "after" and "before" are the same instant — and the shell's first frame is what
/// should be visible next.
fn remove_boot_overlay() {
    if let Some(el) = window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("boot"))
    {
        el.remove();
    }
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

/// Fetches a deployment manifest without a browser cache entry. Its parts have
/// content-addressed names, but the manifest is the mutable pointer to a new
/// deployment and must not point a fresh page at a previous deployment's parts.
async fn fetch_bytes_no_store(url: &str) -> Result<Vec<u8>, String> {
    let win = window().ok_or("no window")?;
    let init = RequestInit::new();
    init.set_cache(RequestCache::NoStore);
    let request = Request::new_with_str_and_init(url, &init)
        .map_err(|e| format!("request {url} failed: {e:?}"))?;
    let resp_val = JsFuture::from(win.fetch_with_request(&request))
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

/// Fetches a deployment's split jar when its manifest is present, preserving
/// the one-file `client.jar` fallback for local `trunk`/`just run-wasm` work.
///
/// A manifest is authoritative once served: corruption must fail visibly rather
/// than quietly falling back to a potentially stale direct jar. Only a 404 means
/// the deployment deliberately has no multipart asset. All paths remain plain
/// relative URLs, so a page served as `/lodestone/` fetches
/// `/lodestone/client.jar.parts.json` and its sibling parts.
async fn fetch_client_jar() -> Result<Vec<u8>, String> {
    let manifest_bytes = match fetch_bytes_no_store(client_jar::PARTS_MANIFEST_URL).await {
        Ok(bytes) => bytes,
        Err(error) if error.starts_with("HTTP 404 ") => {
            log::info!(
                "[boot] {} is absent; using direct {CLIENT_JAR_URL}",
                client_jar::PARTS_MANIFEST_URL
            );
            return fetch_bytes(CLIENT_JAR_URL).await;
        }
        Err(error) => return Err(format!("fetch {} failed: {error}", client_jar::PARTS_MANIFEST_URL)),
    };
    let manifest = client_jar::ClientJarParts::parse(&manifest_bytes)?;
    let mut jar = Vec::with_capacity(manifest.total_len());
    for part in manifest.parts() {
        let bytes = fetch_bytes(&part.name).await?;
        part.verify_download(&bytes)?;
        jar.extend_from_slice(&bytes);
    }
    manifest.verify_complete(&jar)?;
    Ok(jar)
}

/// Best-effort fetch of the real (non-stub) title-screen panorama faces.
///
/// `client.jar` ships only a 69-byte 1×1 grey stub for each of the six panorama
/// faces — the real 1024×1024 art is a *separate* asset-object, content-addressed
/// and not part of the jar at all (see
/// `crates/lodestone-shell/src/asset_objects.rs`'s module doc for the measurement
/// and the eight-name scope this applies to). `web/Trunk.toml`'s `post_build` hook
/// resolves those six objects out of a local `.cache/mc/<version>` asset-object
/// store, when one is populated, and stages them beside the page as flat
/// `panorama_0.png`..`panorama_5.png` — plain filenames, since a browser has no
/// use for the store's hash-addressed path scheme.
///
/// **Unlike `client.jar`/`blocks.json`, a missing face here is not fatal.** Those
/// two have no fallback and `install_assets` still bails hard on either; the
/// panorama already has one per face (the jar's own stub), wired through
/// `crate::resources::load_panorama`'s `WasmObjectBytes` path, so this simply
/// fetches whichever of the six respond with 200 and leaves the rest for that
/// fallback to cover. A checkout with no populated `.cache/mc` therefore still
/// boots to a title screen — just a flat-grey one, exactly as a native run
/// outside a populated store already does.
async fn fetch_panorama_faces() -> Vec<(String, Vec<u8>)> {
    let mut faces = Vec::with_capacity(6);
    for n in 0..6 {
        match fetch_bytes(&format!("panorama_{n}.png")).await {
            Ok(bytes) => {
                log::info!("[boot] panorama face {n}: {} B", bytes.len());
                // The asset-index name `panorama::face_index_key` produces for
                // whichever layer this suffix maps to — see `FACE_SUFFIXES`. The
                // *number* here is vanilla's file suffix, not the cubemap layer
                // index, so this must build the same string the index uses
                // rather than re-deriving a layer order.
                faces.push((
                    format!("minecraft/textures/gui/title/background/panorama_{n}.png"),
                    bytes,
                ));
            }
            Err(e) => {
                log::info!(
                    "[boot] panorama face {n} not staged ({e}); falling back to \
                     client.jar's 1x1 stub for it"
                );
            }
        }
    }
    faces
}

/// Parses `web/scripts/stage_sounds.py`'s manifest — a flat JSON array of
/// object-name strings — via the browser's own `JSON.parse` rather than
/// using a dynamically-typed deserializer for one small, fixed shape. `None` covers
/// malformed JSON or anything other than an all-string array; the caller
/// degrades to "no samples staged" either way, exactly as a missing manifest
/// does.
fn parse_sound_manifest(bytes: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let value = js_sys::JSON::parse(text).ok()?;
    let array: js_sys::Array = value.dyn_into().ok()?;
    array.iter().map(|v| v.as_string()).collect()
}

/// Best-effort fetch of the curated sound corpus `web/Trunk.toml`'s
/// `post_build` hook staged via `scripts/stage_sounds.py`, when a local
/// asset-object store was populated — that script's module doc has the full
/// curation rationale and the measured byte counts.
///
/// **Unlike `client.jar`/`blocks.json`, a miss here is not fatal**, matching
/// [`fetch_panorama_faces`]: `ShellAudio::from_env` already degrades an empty
/// `Bundle::sounds_json`/`sound_objects` to a logged "audio disabled" (no
/// registry) or "audio enabled, N/M events have a sample" (a partial corpus),
/// never a broken page. Three independent misses are tolerated in sequence —
/// the registry itself, the manifest naming which objects were staged, and
/// each individual object — because each is a *different* thing that can be
/// absent (no store at all, a store with `fetch-assets` but not
/// `fetch-sounds`, or a store with some but not all curated samples on disk).
async fn fetch_sound_bundle() -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let sounds_json = match fetch_bytes("sounds.json").await {
        Ok(bytes) => bytes,
        Err(e) => {
            log::info!(
                "[boot] sounds.json not staged ({e}); browser audio stays disabled \
                 (see web/scripts/stage_sounds.py)"
            );
            return (Vec::new(), Vec::new());
        }
    };
    let manifest_bytes = match fetch_bytes("sounds/manifest.json").await {
        Ok(bytes) => bytes,
        Err(e) => {
            log::info!(
                "[boot] sounds/manifest.json not staged ({e}); the registry alone \
                 resolves every event to \"no sample on disk\", same as native with an \
                 empty corpus"
            );
            return (sounds_json, Vec::new());
        }
    };
    let Some(names) = parse_sound_manifest(&manifest_bytes) else {
        log::warn!("[boot] sounds/manifest.json did not parse as a JSON string array");
        return (sounds_json, Vec::new());
    };
    let mut objects = Vec::with_capacity(names.len());
    for name in names {
        match fetch_bytes(&format!("sounds/{name}")).await {
            Ok(bytes) => objects.push((name, bytes)),
            Err(e) => log::info!("[boot] sound object {name} not staged ({e}), skipping"),
        }
    }
    log::info!("[boot] sound corpus: {} object(s) staged", objects.len());
    (sounds_json, objects)
}

/// Fetch the required blobs and the optional panorama faces and sound corpus,
/// and install them all as the session's asset bundle.
async fn install_assets() -> Result<(), String> {
    status("fetching client.jar …");
    let client_jar = fetch_client_jar().await?;
    status(&format!(
        "client.jar {:.1} MiB — fetching blocks.json …",
        client_jar.len() as f64 / (1024.0 * 1024.0)
    ));
    let blocks_report = fetch_bytes(BLOCKS_REPORT_URL).await?;
    status(&format!(
        "client.jar {:.1} MiB, blocks.json {:.1} MiB — fetching panorama …",
        client_jar.len() as f64 / (1024.0 * 1024.0),
        blocks_report.len() as f64 / (1024.0 * 1024.0),
    ));
    let panorama = fetch_panorama_faces().await;
    status(&format!(
        "client.jar {:.1} MiB, blocks.json {:.1} MiB, panorama {}/6 faces — fetching sound \
         corpus …",
        client_jar.len() as f64 / (1024.0 * 1024.0),
        blocks_report.len() as f64 / (1024.0 * 1024.0),
        panorama.len(),
    ));
    let (sounds_json, sound_objects) = fetch_sound_bundle().await;
    let sizes = format!(
        "assets ready: client.jar {:.1} MiB, blocks.json {:.1} MiB, panorama {}/6 faces, \
         sound corpus {} object(s) ({:.1} KiB registry)",
        client_jar.len() as f64 / (1024.0 * 1024.0),
        blocks_report.len() as f64 / (1024.0 * 1024.0),
        panorama.len(),
        sound_objects.len(),
        sounds_json.len() as f64 / 1024.0,
    );
    lodestone::platform::assets::install(lodestone::platform::assets::Bundle {
        client_jar,
        blocks_report,
        panorama,
        sounds_json,
        sound_objects,
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
    // Drop the boot overlay before the shell draws. It is a plain DOM element sitting
    // *over* the canvas, so leaving it up would print "starting the shell…" across the
    // title screen's first button — which is what the first successful run did.
    remove_boot_overlay();
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
