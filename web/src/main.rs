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
//!    handed to the shared embedding/session boundary. Everything downstream — the
//!    zip parser, the atlas builder, the model baker, the font loader — is the same
//!    synchronous code the native client runs. **The filesystem wall is crossed
//!    exactly once, here, at the byte source.**
//! 3. **Starting the app**, then getting out of the way. The page transfers its
//!    source canvas to a dedicated render worker; the worker imports the shared
//!    embedding boundary and owns the renderer for the rest of its lifetime.
//!
//! ## The ordering is load-bearing
//!
//! Asset installation must happen **before** session startup. `Config::resolve_persisted` and the
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

#[cfg(target_arch = "wasm32")]
mod embed;

#[cfg(target_arch = "wasm32")]
thread_local! {
    static STANDALONE_WORKER: std::cell::RefCell<Option<web_sys::Worker>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(target_arch = "wasm32")]
use std::{cell::Cell, rc::Rc};

use lodestone_web::client_jar;
use futures_util::future::{join, join_all};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Event, HtmlCanvasElement, KeyboardEvent, MessageEvent, MouseEvent, Request, RequestCache,
    RequestInit, Response, WheelEvent, Worker, window,
};

/// The deterministic browser resource pack staged from the local archive.
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

async fn fetch_buffer(url: &str, no_store: bool) -> Result<js_sys::ArrayBuffer, String> {
    let win = window().ok_or("no window")?;
    let response = if no_store {
        let init = RequestInit::new();
        init.set_cache(RequestCache::NoStore);
        let request = Request::new_with_str_and_init(url, &init)
            .map_err(|e| format!("request {url} failed: {e:?}"))?;
        JsFuture::from(win.fetch_with_request(&request))
    } else {
        JsFuture::from(win.fetch_with_str(url))
    };
    let resp_val = response
        .await
        .map_err(|e| format!("fetch {url} failed: {e:?}"))?;
    let resp: Response = resp_val.dyn_into().map_err(|_| "not a Response")?;
    if !resp.ok() {
        return Err(format!("HTTP {} for {url}", resp.status()));
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("array_buffer for {url} failed: {e:?}"))?;
    buf.dyn_into()
        .map_err(|_| format!("array_buffer for {url} was not an ArrayBuffer"))
}

/// Fetches a deployment manifest without a browser cache entry. Its parts have
/// content-addressed names, but the manifest is the mutable pointer to a new
/// deployment and must not point a fresh page at a previous deployment's parts.
async fn fetch_bytes_no_store(url: &str) -> Result<Vec<u8>, String> {
    Ok(buffer_bytes(&fetch_buffer(url, true).await?))
}

fn buffer_bytes(buffer: &js_sys::ArrayBuffer) -> Vec<u8> {
    js_sys::Uint8Array::new(buffer).to_vec()
}

/// Fetches a deployment's split jar when its manifest is present, preserving
/// the one-file `client.jar` fallback for local `trunk`/`just run-wasm` work.
///
/// A manifest is authoritative once served: corruption must fail visibly rather
/// than quietly falling back to a potentially stale direct jar. Only a 404 means
/// the deployment deliberately has no multipart asset. All paths remain plain
/// relative URLs, so a page served as `/lodestone/` fetches
/// `/lodestone/client.jar.parts.json` and its sibling parts.
async fn fetch_client_jar() -> Result<js_sys::ArrayBuffer, String> {
    let manifest_bytes = match fetch_bytes_no_store(client_jar::PARTS_MANIFEST_URL).await {
        Ok(bytes) => bytes,
        Err(error) if error.starts_with("HTTP 404 ") => {
            log::info!(
                "[boot] {} is absent; checking direct archive manifest",
                client_jar::PARTS_MANIFEST_URL
            );
            let direct_manifest = match fetch_bytes_no_store(client_jar::DIRECT_MANIFEST_URL).await {
                Ok(bytes) => bytes,
                Err(error) if error.starts_with("HTTP 404 ") => {
                    log::info!(
                        "[boot] {} is absent; using direct {CLIENT_JAR_URL}",
                        client_jar::DIRECT_MANIFEST_URL
                    );
                    let jar = fetch_buffer(CLIENT_JAR_URL, false).await?;
                    return Ok(jar);
                }
                Err(error) => {
                    return Err(format!(
                        "fetch {} failed: {error}",
                        client_jar::DIRECT_MANIFEST_URL
                    ));
                }
            };
            let manifest = client_jar::ClientJarManifest::parse(&direct_manifest)?;
            let jar = fetch_buffer(CLIENT_JAR_URL, false).await?;
            manifest.verify_download(&buffer_bytes(&jar))?;
            return Ok(jar);
        }
        Err(error) => return Err(format!("fetch {} failed: {error}", client_jar::PARTS_MANIFEST_URL)),
    };
    let manifest = client_jar::ClientJarParts::parse(&manifest_bytes)?;
    let parts = join_all(manifest.parts().iter().map(|part| async move {
        let bytes = fetch_buffer(&part.name, false).await?;
        let bytes_for_hash = buffer_bytes(&bytes);
        part.verify_download(&bytes_for_hash)?;
        Ok::<js_sys::ArrayBuffer, String>(bytes)
    }))
    .await;
    let jar = js_sys::Uint8Array::new_with_length(manifest.total_len() as u32);
    let mut offset = 0;
    for bytes in parts {
        let bytes = js_sys::Uint8Array::new(&bytes?.into());
        jar.set(&bytes, offset);
        offset += bytes.length();
    }
    let jar_buffer = jar.buffer();
    manifest.verify_complete(&buffer_bytes(&jar_buffer))?;
    Ok(jar_buffer)
}

/// Fetches the two required blobs for transfer to the render worker.
#[cfg(target_arch = "wasm32")]
struct WorkerAssets {
    client_jar: js_sys::ArrayBuffer,
    blocks_report: js_sys::ArrayBuffer,
}

#[cfg(target_arch = "wasm32")]
async fn install_assets() -> Result<WorkerAssets, String> {
    status("fetching client.jar and blocks.json …");
    let (client_jar_result, blocks_report_result) = join(
        fetch_client_jar(),
        fetch_buffer(BLOCKS_REPORT_URL, false),
    )
    .await;
    let client_jar = client_jar_result?;
    let blocks_report = blocks_report_result?;
    status(&format!(
        "required assets ready: client.jar {:.1} MiB, blocks.json {:.1} MiB — starting worker …",
        client_jar.byte_length() as f64 / (1024.0 * 1024.0),
        blocks_report.byte_length() as f64 / (1024.0 * 1024.0),
    ));
    Ok(WorkerAssets {
        client_jar,
        blocks_report,
    })
}

/// Boot: install the assets, then start the worker-owned real shell.
#[cfg(target_arch = "wasm32")]
async fn boot(canvas: web_sys::HtmlCanvasElement) {
    let bundle = match install_assets().await {
        Ok(bundle) => bundle,
        Err(e) => {
            status(&format!(
                "ASSET LOAD FAILED — {e}. The browser build needs client.jar and \
                 blocks.json served beside the page; see web/README.md. Nothing is drawn \
                 on purpose, so this cannot be mistaken for a working session."
            ));
            return;
        }
    };

    status("starting the shell worker …");
    if let Err(error) = launch_render_worker(canvas, bundle, standalone_log_level()) {
        status(&format!("shell failed to start: {error}"));
    }
}

#[cfg(target_arch = "wasm32")]
fn launch_render_worker(
    canvas: web_sys::HtmlCanvasElement,
    assets: WorkerAssets,
    log_level: String,
) -> Result<(), String> {
    resize_canvas(&canvas);
    let worker = Worker::new("lodestone-render-worker.js")
        .map_err(|error| format!("cannot create render worker: {error:?}"))?;
    let pointer_lock_requested = Rc::new(Cell::new(false));
    install_input_bridge(&canvas, &worker, Rc::clone(&pointer_lock_requested))
        .map_err(|error| format!("cannot install canvas input bridge: {error:?}"))?;
    let offscreen = canvas
        .transfer_control_to_offscreen()
        .map_err(|error| format!("cannot transfer canvas to worker: {error:?}"))?;
    let pointer_lock_for_message = Rc::clone(&pointer_lock_requested);
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let value = event.data();
        let kind = js_sys::Reflect::get(&value, &JsValue::from_str("kind"))
            .ok()
            .and_then(|value| value.as_string());
        match kind.as_deref() {
            Some("progress") => {
                let progress = js_sys::Reflect::get(&value, &JsValue::from_str("event"))
                    .ok()
                    .unwrap_or(JsValue::UNDEFINED);
                let phase = js_sys::Reflect::get(&progress, &JsValue::from_str("phase"))
                    .ok()
                    .and_then(|value| value.as_string());
                let message = js_sys::Reflect::get(&progress, &JsValue::from_str("message"))
                    .ok()
                    .and_then(|value| value.as_string());
                let elapsed_ms = js_sys::Reflect::get(&progress, &JsValue::from_str("elapsedMs"))
                    .ok()
                    .and_then(|value| value.as_f64());
                if let (Some(phase), Some(elapsed_ms)) = (phase.as_deref(), elapsed_ms) {
                    let loaded = numeric_progress_field(&progress, "loadedColumns");
                    let expected = numeric_progress_field(&progress, "expectedColumns");
                    let settled = numeric_progress_field(&progress, "settledColumns");
                    let pending = numeric_progress_field(&progress, "pendingMeshes");
                    web_sys::console::info_1(
                        &format!(
                            "lodestone join: phase={phase} elapsed_ms={elapsed_ms:.3} \
                             loaded_columns={loaded:.0}/{expected:.0} \
                             settled_columns={settled:.0} pending_meshes={pending:.0}"
                        )
                        .into(),
                    );
                }
                if phase.as_deref() == Some("first-frame") {
                    remove_boot_overlay();
                }
                if let Some(message) = message {
                    status(&message);
                }
            }
            Some("host-action") => {
                let action = js_sys::Reflect::get(&value, &JsValue::from_str("action"))
                    .ok()
                    .unwrap_or(JsValue::UNDEFINED);
                let action_type = js_sys::Reflect::get(
                    &action,
                    &JsValue::from_str("type"),
                )
                .ok()
                .and_then(|value| value.as_string());
                if action_type.as_deref() == Some("pointer-lock") {
                    let locked = js_sys::Reflect::get(
                        &action,
                        &JsValue::from_str("locked"),
                    )
                    .ok()
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                    pointer_lock_for_message.set(locked);
                    if !locked {
                        if let Some(document) = window().and_then(|window| window.document()) {
                            document.exit_pointer_lock();
                        }
                    }
                }
            }
            Some("ready") => status("renderer worker ready"),
            Some("error") => {
                let message = js_sys::Reflect::get(&value, &JsValue::from_str("message"))
                    .ok()
                    .and_then(|value| value.as_string())
                    .unwrap_or_else(|| "renderer worker failed".to_string());
                status(&format!("shell failed to start: {message}"));
            }
            _ => {}
        }
    });
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    let launch = js_sys::Object::new();
    js_sys::Reflect::set(&launch, &JsValue::from_str("kind"), &JsValue::from_str("mount"))
        .map_err(|error| format!("cannot build render-worker request: {error:?}"))?;
    js_sys::Reflect::set(&launch, &JsValue::from_str("canvas"), &offscreen)
        .map_err(|error| format!("cannot build render-worker canvas request: {error:?}"))?;
    js_sys::Reflect::set(
        &launch,
        &JsValue::from_str("clientJar"),
        &assets.client_jar,
    )
    .map_err(|error| format!("cannot build render-worker jar request: {error:?}"))?;
    js_sys::Reflect::set(
        &launch,
        &JsValue::from_str("blocksJson"),
        &assets.blocks_report,
    )
    .map_err(|error| format!("cannot build render-worker blocks request: {error:?}"))?;
    js_sys::Reflect::set(
        &launch,
        &JsValue::from_str("logLevel"),
        &JsValue::from_str(&log_level),
    )
    .map_err(|error| format!("cannot build render-worker log request: {error:?}"))?;
    let transfer = js_sys::Array::new();
    transfer.push(&offscreen);
    transfer.push(&assets.client_jar);
    transfer.push(&assets.blocks_report);
    worker
        .post_message_with_transfer(&launch, &transfer)
        .map_err(|error| format!("cannot start render worker: {error:?}"))?;
    STANDALONE_WORKER.with_borrow_mut(|slot| *slot = Some(worker));
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn standalone_log_level() -> String {
    let search = window()
        .and_then(|window| window.location().search().ok())
        .unwrap_or_default();
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix("log="))
        .filter(|level| matches!(*level, "off" | "error" | "warn" | "info" | "debug" | "trace"))
        .unwrap_or("warn")
        .to_string()
}

#[cfg(target_arch = "wasm32")]
fn resize_canvas(canvas: &HtmlCanvasElement) {
    let (width, height) = canvas_backing_size(canvas);
    canvas.set_width(width);
    canvas.set_height(height);
}

#[cfg(target_arch = "wasm32")]
fn canvas_backing_size(canvas: &HtmlCanvasElement) -> (u32, u32) {
    let dpr = window().map_or(1.0, |window| window.device_pixel_ratio());
    (
        (f64::from(canvas.client_width()) * dpr).round().max(1.0) as u32,
        (f64::from(canvas.client_height()) * dpr).round().max(1.0) as u32,
    )
}

#[cfg(target_arch = "wasm32")]
fn numeric_progress_field(progress: &JsValue, name: &str) -> f64 {
    js_sys::Reflect::get(progress, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
}

#[cfg(target_arch = "wasm32")]
fn send_input(worker: &Worker, input_type: &str, fields: &[(&str, JsValue)]) {
    let input = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &input,
        &JsValue::from_str("type"),
        &JsValue::from_str(input_type),
    );
    for (name, value) in fields {
        let _ = js_sys::Reflect::set(&input, &JsValue::from_str(name), value);
    }
    let message = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &message,
        &JsValue::from_str("kind"),
        &JsValue::from_str("input"),
    );
    let _ = js_sys::Reflect::set(&message, &JsValue::from_str("input"), &input);
    if let Err(error) = worker.post_message(&message) {
        log::warn!("render-worker input message failed: {error:?}");
    }
}

#[cfg(target_arch = "wasm32")]
fn install_input_bridge(
    canvas: &HtmlCanvasElement,
    worker: &Worker,
    pointer_lock_requested: Rc<Cell<bool>>,
) -> Result<(), JsValue> {
    let worker_for_pointer = worker.clone();
    let pointer_move = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Ok(event) = event.dyn_into::<MouseEvent>() else {
            return;
        };
        send_input(
            &worker_for_pointer,
            "pointerMove",
            &[
                ("x", JsValue::from_f64(f64::from(event.offset_x()))),
                ("y", JsValue::from_f64(f64::from(event.offset_y()))),
            ],
        );
        if event.movement_x() != 0 || event.movement_y() != 0 {
            send_input(
                &worker_for_pointer,
                "mouseMotion",
                &[
                    ("dx", JsValue::from_f64(f64::from(event.movement_x()))),
                    ("dy", JsValue::from_f64(f64::from(event.movement_y()))),
                ],
            );
        }
    });
    canvas.add_event_listener_with_callback(
        "pointermove",
        pointer_move.as_ref().unchecked_ref(),
    )?;
    pointer_move.forget();

    for event_name in ["mousedown", "mouseup"] {
        let worker_for_button = worker.clone();
        let canvas_for_button = canvas.clone();
        let pointer_lock_requested = Rc::clone(&pointer_lock_requested);
        let closure = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Ok(event) = event.dyn_into::<MouseEvent>() else {
                return;
            };
            send_input(
                &worker_for_button,
                "mouseButton",
                &[
                    ("button", JsValue::from_f64(f64::from(event.button()))),
                    ("pressed", JsValue::from_bool(event.type_() == "mousedown")),
                ],
            );
            if event.type_() == "mousedown" && pointer_lock_requested.get() {
                if let Some(document) = window().and_then(|window| window.document())
                    && document.pointer_lock_element().is_none()
                {
                    let _ = canvas_for_button.request_pointer_lock();
                }
            }
        });
        canvas.add_event_listener_with_callback(event_name, closure.as_ref().unchecked_ref())?;
        closure.forget();
    }

    let worker_for_wheel = worker.clone();
    let wheel = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Ok(event) = event.dyn_into::<WheelEvent>() else {
            return;
        };
        send_input(
            &worker_for_wheel,
            "wheel",
            &[
                ("dx", JsValue::from_f64(event.delta_x())),
                ("dy", JsValue::from_f64(event.delta_y())),
            ],
        );
        event.prevent_default();
    });
    canvas.add_event_listener_with_callback("wheel", wheel.as_ref().unchecked_ref())?;
    wheel.forget();

    for event_name in ["keydown", "keyup"] {
        let worker_for_key = worker.clone();
        let closure = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Ok(event) = event.dyn_into::<KeyboardEvent>() else {
                return;
            };
            let mut modifiers = 0u8;
            if event.shift_key() {
                modifiers |= 1;
            }
            if event.ctrl_key() {
                modifiers |= 2;
            }
            if event.alt_key() {
                modifiers |= 4;
            }
            if event.meta_key() {
                modifiers |= 8;
            }
            let text = event.key();
            let text = (!text.is_empty()).then_some(JsValue::from_str(&text));
            let mut fields = vec![
                ("code", JsValue::from_str(&event.code())),
                ("pressed", JsValue::from_bool(event.type_() == "keydown")),
                ("modifiers", JsValue::from_f64(f64::from(modifiers))),
            ];
            if let Some(text) = text {
                fields.push(("text", text));
            }
            send_input(&worker_for_key, "key", &fields);
            event.prevent_default();
        });
        canvas.add_event_listener_with_callback(event_name, closure.as_ref().unchecked_ref())?;
        closure.forget();
    }

    for event_name in ["focus", "blur"] {
        let worker_for_focus = worker.clone();
        let closure = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            send_input(
                &worker_for_focus,
                "focus",
                &[("focused", JsValue::from_bool(event.type_() == "focus"))],
            );
        });
        canvas.add_event_listener_with_callback(event_name, closure.as_ref().unchecked_ref())?;
        closure.forget();
    }

    let document = window()
        .and_then(|window| window.document())
        .ok_or_else(|| JsValue::from_str("no document for pointer-lock bridge"))?;
    let worker_for_lock = worker.clone();
    let canvas_value = JsValue::from(canvas.clone());
    let document_for_lock = document.clone();
    let pointer_lock = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let locked = document_for_lock
            .pointer_lock_element()
            .is_some_and(|element| JsValue::from(element) == canvas_value);
        send_input(
            &worker_for_lock,
            "pointerLock",
            &[("locked", JsValue::from_bool(locked))],
        );
    });
    document.add_event_listener_with_callback(
        "pointerlockchange",
        pointer_lock.as_ref().unchecked_ref(),
    )?;
    pointer_lock.forget();

    let Some(window) = window() else {
        return Ok(());
    };
    let worker_for_resize = worker.clone();
    let canvas_for_resize = canvas.clone();
    let resize = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let (width, height) = canvas_backing_size(&canvas_for_resize);
        send_input(
            &worker_for_resize,
            "resize",
            &[
                ("width", JsValue::from_f64(f64::from(width))),
                ("height", JsValue::from_f64(f64::from(height))),
            ],
        );
    });
    window.add_event_listener_with_callback("resize", resize.as_ref().unchecked_ref())?;
    resize.forget();
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn standalone_canvas() -> Option<web_sys::HtmlCanvasElement> {
    window()?
        .document()?
        .query_selector("canvas[data-lodestone-standalone]")
        .ok()??
        .dyn_into()
        .ok()
}

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Warn);
    let standalone = window()
        .and_then(|window| window.document())
        .and_then(|document| document.query_selector("[data-lodestone-standalone]").ok())
        .flatten()
        .is_some();
    if standalone {
        if let Some(canvas) = standalone_canvas() {
            spawn_local(boot(canvas));
        } else {
            status("standalone page has no canvas[data-lodestone-standalone]");
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {}
