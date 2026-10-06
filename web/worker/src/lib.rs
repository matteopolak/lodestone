//! Web Worker entry point for browser singleplayer.
//!
//! The page starts this module through `worker.js` and transfers protocol and
//! progress endpoints. The worker then owns the mutable server world and its
//! ticks for the entire session; no decoded world state crosses the boundary.

use wasm_bindgen::prelude::*;
use web_sys::MessagePort;
use std::cell::{Cell, RefCell};
use lodestone_server::worldgen_progress::{
    WorldgenTimingBuffer, WorldgenTimingPhase, WorldgenTimingSample,
};

thread_local! {
    static PROGRESS_PORT: RefCell<Option<(MessagePort, u32)>> = const { RefCell::new(None) };
    static TICK_MONITOR: RefCell<Option<lodestone_server::IntegratedTickMonitor>> = const { RefCell::new(None) };
    static PREVIOUS_TICK_COUNT: Cell<Option<u64>> = const { Cell::new(None) };
}

static PHASE_TIMINGS: WorldgenTimingBuffer = WorldgenTimingBuffer::new();

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
    game_mode: Option<u8>,
) -> Result<(), JsValue> {
    let game_mode = match game_mode.unwrap_or(0) {
        0 => lodestone_model::GameMode::Survival,
        1 => lodestone_model::GameMode::Creative,
        2 => lodestone_model::GameMode::Adventure,
        3 => lodestone_model::GameMode::Spectator,
        _ => return Err(JsValue::from_str("invalid browser worker game mode")),
    };
    console_error_panic_hook::set_once();
    install_logger(&log_level)?;
    tracing::info!(%log_level, protocol, seed, preset, epoch, "browser server worker starting");
    lodestone_server::join_scheduler::register_browser_worker_epoch(epoch);
    PROGRESS_PORT.with(|slot| *slot.borrow_mut() = Some((progress_port.clone(), epoch)));
    TICK_MONITOR.with(|slot| slot.borrow_mut().take());
    PREVIOUS_TICK_COUNT.with(|slot| slot.set(None));
    let _ = lodestone_server::worldgen_progress::install_sink(post_worldgen_event);
    if log::max_level() >= log::LevelFilter::Debug {
        PHASE_TIMINGS.drain();
        let _ = lodestone_server::worldgen_progress::install_timing_sink(record_phase_timing);
        let _ = lodestone_server::connection_progress::install_sink(post_connection_progress);
    }
    post_progress(&progress_port, epoch, "server-starting");
    let result = lodestone::net::start_browser_integrated_worker(
        port,
        horizon_port,
        protocol,
        seed,
        preset,
        epoch,
        view_radius,
        (log::max_level() >= log::LevelFilter::Debug).then_some(post_transport_progress),
        game_mode,
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
    let cancelled = lodestone_server::join_scheduler::cancel_browser_worker_epoch(epoch);
    if cancelled {
        TICK_MONITOR.with(|slot| slot.borrow_mut().take());
        PREVIOUS_TICK_COUNT.with(|slot| slot.set(None));
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
            let observed_tps = PREVIOUS_TICK_COUNT.with(|slot| {
                tick_throughput(slot.replace(Some(stats.tick_count)), stats.tick_count, callback_gap_ms)
            });
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
                ("observedTps", observed_tps.map_or(JsValue::NULL, JsValue::from_f64)),
                ("callbackGapMs", JsValue::from_f64(callback_gap_ms)),
                ("tickWaitCount", JsValue::from_f64(stats.schedule.wait_count as f64)),
                ("tickWakeP95Ms", JsValue::from_f64(stats.schedule.service_lateness_p95_ms)),
                ("tickWakeMaxMs", JsValue::from_f64(stats.schedule.service_lateness_max_ms)),
                ("tickDeadlineMaxMs", JsValue::from_f64(stats.schedule.deadline_lateness_max_ms)),
                ("tickCatchUpCount", JsValue::from_f64(stats.schedule.catch_up_ticks as f64)),
                ("tickCooperativeYields", JsValue::from_f64(stats.schedule.cooperative_yields as f64)),
                ("tickYieldMaxMs", JsValue::from_f64(stats.schedule.cooperative_yield_max_ms)),
                ("tickShedCount", JsValue::from_f64(stats.schedule.shed_ticks as f64)),
            ] {
                let _ = js_sys::Reflect::set(&message, &JsValue::from_str(key), &value);
            }
            let posted = port.post_message(&message).is_ok();
            if posted {
                post_phase_timings(port, epoch);
            }
            posted
        })
    })
}

fn tick_throughput(previous: Option<u64>, current: u64, elapsed_ms: f64) -> Option<f64> {
    if !elapsed_ms.is_finite() || elapsed_ms <= 0.0 {
        return None;
    }
    Some(current.checked_sub(previous?)? as f64 * 1000.0 / elapsed_ms)
}

#[cfg(test)]
#[test]
fn tick_throughput_measures_callback_delay_instead_of_tick_cost() {
    assert_eq!(tick_throughput(Some(100), 120, 1250.0), Some(16.0));
    assert_eq!(tick_throughput(Some(100), 100, 1000.0), Some(0.0));
    assert_eq!(tick_throughput(None, 120, 1000.0), None);
    assert_eq!(tick_throughput(Some(120), 100, 1000.0), None);
    assert_eq!(tick_throughput(Some(100), 120, 0.0), None);
}

fn post_connection_progress(progress: lodestone_server::connection_progress::ConnectionProgress) {
    PROGRESS_PORT.with(|slot| {
        if let Some((port, epoch)) = slot.borrow().as_ref() {
            let message = js_sys::Object::new();
            for (key, value) in [
                ("kind", JsValue::from_str("connection-progress")),
                ("epoch", JsValue::from_f64(f64::from(*epoch))),
                ("running", JsValue::from_bool(progress.running)),
                ("elapsedMs", JsValue::from_f64(progress.elapsed.as_secs_f64() * 1000.0)),
                ("passes", JsValue::from_f64(progress.passes as f64)),
                ("clientLoaded", JsValue::from_bool(progress.client_loaded)),
                ("centerX", JsValue::from_f64(f64::from(progress.center.0))),
                ("centerZ", JsValue::from_f64(f64::from(progress.center.1))),
                ("radius", JsValue::from_f64(f64::from(progress.radius))),
                ("owedColumns", JsValue::from_f64(progress.owed_columns as f64)),
                ("deliveredColumns", JsValue::from_f64(progress.delivered_columns as f64)),
                ("chunksSent", JsValue::from_f64(progress.chunks_sent as f64)),
                ("remaining", JsValue::from_f64(progress.remaining as f64)),
                ("activity", JsValue::from_str(progress.activity.as_str())),
                ("activityMs", JsValue::from_f64(progress.activity_elapsed.as_secs_f64() * 1000.0)),
                ("targetX", progress.target.map_or(JsValue::NULL, |pos| JsValue::from_f64(f64::from(pos.0)))),
                ("targetZ", progress.target.map_or(JsValue::NULL, |pos| JsValue::from_f64(f64::from(pos.1)))),
                ("packetId", progress.packet_id.map_or(JsValue::NULL, |id| JsValue::from_f64(f64::from(id)))),
            ] {
                let _ = js_sys::Reflect::set(&message, &JsValue::from_str(key), &value);
            }
            let _ = port.post_message(&message);
        }
    });
}

fn post_transport_progress(progress: lodestone_net::MessagePortProgress) {
    PROGRESS_PORT.with(|slot| {
        if let Some((port, epoch)) = slot.borrow().as_ref() {
            let message = js_sys::Object::new();
            for (key, value) in [
                ("kind", JsValue::from_str("transport-progress")),
                ("epoch", JsValue::from_f64(f64::from(*epoch))),
                ("endpoint", JsValue::from_f64(f64::from(progress.endpoint_id))),
                ("postedBytes", JsValue::from_f64(progress.posted_bytes as f64)),
                ("receivedBytes", JsValue::from_f64(progress.received_bytes as f64)),
                ("drainedBytes", JsValue::from_f64(progress.drained_bytes as f64)),
                ("sendCredit", JsValue::from_f64(progress.send_credit as f64)),
                ("receiveCredit", JsValue::from_f64(progress.receive_credit as f64)),
                ("longestPendingMs", JsValue::from_f64(progress.longest_write_pending.as_secs_f64() * 1000.0)),
                ("currentPendingMs", JsValue::from_f64(progress.current_write_pending.as_secs_f64() * 1000.0)),
                ("closed", JsValue::from_bool(progress.closed)),
            ] {
                let _ = js_sys::Reflect::set(&message, &JsValue::from_str(key), &value);
            }
            let _ = port.post_message(&message);
        }
    });
}

fn record_phase_timing(sample: WorldgenTimingSample) {
    PHASE_TIMINGS.record(sample);
}

fn post_phase_timings(port: &MessagePort, epoch: u32) {
    let timings = PHASE_TIMINGS.drain();
    if timings.iter().all(|totals| totals.calls == 0) {
        return;
    }
    let message = js_sys::Object::new();
    let phases = js_sys::Array::new();
    for phase in WorldgenTimingPhase::ALL {
        let totals = timings[phase.index()];
        if totals.calls == 0 {
            continue;
        }
        let row = js_sys::Object::new();
        for (key, value) in [
            ("phase", JsValue::from_str(phase.name())),
            ("calls", JsValue::from_f64(totals.calls as f64)),
            ("items", JsValue::from_f64(totals.items as f64)),
            ("elapsedMs", JsValue::from_f64(totals.elapsed.as_secs_f64() * 1000.0)),
            ("maximumMs", JsValue::from_f64(totals.maximum.as_secs_f64() * 1000.0)),
        ] {
            let _ = js_sys::Reflect::set(&row, &JsValue::from_str(key), &value);
        }
        phases.push(&row);
    }
    for (key, value) in [
        ("kind", JsValue::from_str("worldgen-timing")),
        ("epoch", JsValue::from_f64(f64::from(epoch))),
        ("phases", phases.into()),
    ] {
        let _ = js_sys::Reflect::set(&message, &JsValue::from_str(key), &value);
    }
    if port.post_message(&message).is_err() {
        PHASE_TIMINGS.restore(timings);
    }
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
