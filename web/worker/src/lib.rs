//! Web Worker entry point for browser singleplayer.
//!
//! The page starts this module through `worker.js` and transfers protocol and
//! progress endpoints. The worker then owns the mutable server world and its
//! ticks for the entire session; no decoded world state crosses the boundary.

use wasm_bindgen::prelude::*;
use web_sys::MessagePort;
use std::cell::RefCell;

thread_local! {
    static PROGRESS_PORT: RefCell<Option<(MessagePort, u32)>> = const { RefCell::new(None) };
    static TICK_MONITOR: RefCell<Option<lodestone_server::IntegratedTickMonitor>> = const { RefCell::new(None) };
}

#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
pub use wasm_bindgen_rayon::init_thread_pool;

/// Constructs the authoritative worker-side integrated server synchronously,
/// before the JavaScript bootstrap reports `ready` to the page.
#[wasm_bindgen]
pub fn start_worker(
    port: MessagePort,
    progress_port: MessagePort,
    horizon_port: MessagePort,
    protocol: i32,
    seed: i64,
    preset: u8,
    epoch: u32,
    log_level: String,
    view_radius: Option<i32>,
) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    install_logger(&log_level)?;
    tracing::info!(%log_level, protocol, seed, preset, epoch, "browser server worker starting");
    lodestone_server::worldgen_session::register_browser_worker_epoch(epoch);
    PROGRESS_PORT.with(|slot| *slot.borrow_mut() = Some((progress_port.clone(), epoch)));
    TICK_MONITOR.with(|slot| slot.borrow_mut().take());
    let _ = lodestone_server::worldgen_progress::install_sink(post_worldgen_event);
    post_progress(&progress_port, epoch, "server-starting");
    let result = lodestone::net::start_browser_integrated_worker(
        port,
        horizon_port,
        protocol,
        seed,
        preset,
        epoch,
        view_radius,
    )
        .map_err(|error| JsValue::from_str(&error));
    let monitor = result?;
    TICK_MONITOR.with(|slot| *slot.borrow_mut() = monitor);
    post_progress(&progress_port, epoch, "server-started");
    Ok(())
}

fn install_logger(value: &str) -> Result<(), JsValue> {
    let filter = match value {
        "off" => log::LevelFilter::Off,
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "info" => log::LevelFilter::Info,
        "debug" => log::LevelFilter::Debug,
        "trace" => log::LevelFilter::Trace,
        _ => return Err(JsValue::from_str("invalid browser log level")),
    };
    let initial = filter.to_level().unwrap_or(log::Level::Error);
    console_log::init_with_level(initial)
        .map_err(|error| JsValue::from_str(&format!("cannot install browser logger: {error}")))?;
    log::set_max_level(filter);
    Ok(())
}

#[wasm_bindgen]
pub fn cancel_worker(epoch: u32) -> bool {
    let cancelled = lodestone_server::worldgen_session::cancel_browser_worker_epoch(epoch);
    if cancelled {
        TICK_MONITOR.with(|slot| slot.borrow_mut().take());
    }
    cancelled
}

#[wasm_bindgen]
pub fn sample_worker(epoch: u32, callback_gap_ms: f64) -> bool {
    if !callback_gap_ms.is_finite() || callback_gap_ms < 0.0 {
        return false;
    }
    PROGRESS_PORT.with(|slot| {
        let slot = slot.borrow();
        let Some((port, active_epoch)) = slot.as_ref() else {
            return false;
        };
        if *active_epoch != epoch {
            return false;
        }
        TICK_MONITOR.with(|slot| {
            let slot = slot.borrow();
            let Some(monitor) = slot.as_ref() else {
                return false;
            };
            let (stats, witness) = monitor.snapshot();
            let message = js_sys::Object::new();
            for (key, value) in [
                ("kind", JsValue::from_str("worker-health")),
                ("epoch", JsValue::from_f64(f64::from(epoch))),
                ("tickCount", JsValue::from_f64(stats.tick_count as f64)),
                ("tickWitness", JsValue::from_f64(witness as f64)),
                ("overruns", JsValue::from_f64(stats.overrun_count as f64)),
                ("msptMs", JsValue::from_f64(stats.mspt_ms)),
                ("msptAvgMs", JsValue::from_f64(stats.mspt_avg_ms)),
                ("tps", JsValue::from_f64(stats.tps)),
                ("callbackGapMs", JsValue::from_f64(callback_gap_ms)),
            ] {
                let _ = js_sys::Reflect::set(&message, &JsValue::from_str(key), &value);
            }
            port.post_message(&message).is_ok()
        })
    })
}

fn post_progress(port: &MessagePort, epoch: u32, stage: &str) {
    let message = js_sys::Object::new();
    let set = |key: &str, value: JsValue| {
        js_sys::Reflect::set(&message, &JsValue::from_str(key), &value)
            .expect("progress message is extensible");
    };
    set("kind", JsValue::from_str("worldgen-progress"));
    set("epoch", JsValue::from_f64(f64::from(epoch)));
    set("admitted", JsValue::from_f64(0.0));
    set("completed", JsValue::from_f64(0.0));
    set("committed", JsValue::from_f64(0.0));
    set("queue", JsValue::from_f64(0.0));
    set("bytes", JsValue::from_f64(0.0));
    set("stage", JsValue::from_str(stage));
    let _ = port.post_message(&message);
}

fn post_worldgen_event(progress: lodestone_server::worldgen_progress::WorldgenProgress) {
    PROGRESS_PORT.with(|slot| {
        if let Some((port, epoch)) = slot.borrow().as_ref() {
            let message = js_sys::Object::new();
            let set = |key: &str, value: JsValue| {
                let _ = js_sys::Reflect::set(&message, &JsValue::from_str(key), &value);
            };
            set("kind", JsValue::from_str("worldgen-progress"));
            set("epoch", JsValue::from_f64(f64::from(*epoch)));
            set("session", JsValue::from_f64(progress.session as f64));
            set("targetX", JsValue::from_f64(f64::from(progress.target.0)));
            set("targetZ", JsValue::from_f64(f64::from(progress.target.1)));
            set("admitted", JsValue::from_f64(progress.admitted as f64));
            set("completed", JsValue::from_f64(progress.completed as f64));
            set("committed", JsValue::from_f64(progress.committed as f64));
            set("queue", JsValue::from_f64(progress.queued as f64));
            set("bytes", JsValue::from_f64(progress.retained_bytes as f64));
            set("stage", JsValue::from_str(progress.stage));
            let _ = port.post_message(&message);
        }
    });
}
