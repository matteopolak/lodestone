//! Web Worker entry point for browser singleplayer.
//!
//! The page starts this module through `worker.js`, transfers one endpoint of a
//! `MessageChannel`, and keeps the other endpoint as the client's transport.
//! The worker then owns the mutable server world and its ticks for the entire
//! session; no decoded world state crosses the boundary.

use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

/// Constructs the authoritative worker-side integrated server synchronously,
/// before the JavaScript bootstrap reports `ready` to the page.
#[wasm_bindgen]
pub fn start_worker(port: MessagePort, protocol: i32, seed: i64, preset: u8) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    lodestone::net::start_browser_integrated_worker(port, protocol, seed, preset)
        .map_err(|error| JsValue::from_str(&error))
}
