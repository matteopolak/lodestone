//! Construction of the worker-owned integrated server.
//!
//! The world source, server, tick tasks and the duplex-to-port bridge all live
//! in one Worker; the page keeps only its client, rendering and input state.

use std::cell::RefCell;
use std::sync::Arc;

use lodestone_server::WorldPreset;
use web_sys::MessagePort;

use crate::horizon::HorizonService;

thread_local! {
    static HORIZON_SERVICE: RefCell<Option<HorizonService>> = const { RefCell::new(None) };
}

/// The page's plugin registry is not copied into this worker. A command that
/// needs that page-side dispatch is refused with a visible reply instead of
/// being accepted and silently dropped.
struct WorkerCommandSink;

impl lodestone_server::CommandSink for WorkerCommandSink {
    fn run(
        &self,
        _caller: &lodestone_server::CommandCaller,
        command: &str,
    ) -> lodestone_server::CommandResponse {
        lodestone_server::CommandResponse::unknown_command(command)
    }
}

const DEFAULT_VIEW_RADIUS: i32 = 8;

/// Starts the server on the supplied protocol port and returns its tick monitor.
pub(crate) fn start(
    port: MessagePort,
    horizon_port: MessagePort,
    protocol: i32,
    seed: i64,
    preset: u8,
    epoch: u32,
    view_radius: Option<i32>,
    transport_diagnostics: Option<fn(lodestone_net::MessagePortProgress)>,
    game_mode: lodestone_model::GameMode,
) -> Result<Option<lodestone_server::IntegratedTickMonitor>, String> {
    let view_radius = view_radius.unwrap_or(DEFAULT_VIEW_RADIUS);
    if view_radius < 0 {
        return Err("invalid browser worker view radius".to_string());
    }
    let preset = WorldPreset::from_wire_id(preset)
        .ok_or_else(|| format!("unknown browser worker world preset {preset}"))?;
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)
        .ok_or_else(|| format!("no server protocol compiled for protocol {protocol}"))?;
    let (source, _min_y, _height) = preset
        .chunk_source(server_protocol.worldgen_scope(), seed)
        .map_err(|error| format!("cannot build browser worker world: {error}"))?;
    HORIZON_SERVICE.with(|service| service.borrow_mut().take());
    if source.horizon_sample(0, 0).is_some() {
        HORIZON_SERVICE.with(|service| {
            *service.borrow_mut() = Some(HorizonService::new(Arc::clone(&source), epoch, horizon_port));
        });
    } else {
        horizon_port.close();
    }
    let commands = lodestone_server::CommandDispatch::installed(Arc::new(WorkerCommandSink));
    let mut worker_io = lodestone_net::MessagePortTransport::new(port);
    if let Some(sink) = transport_diagnostics {
        worker_io.set_diagnostics_sink(sink);
    }
    Ok(lodestone_server::IntegratedServer::serve_with_transport_in_mode(
        server_protocol,
        source,
        view_radius,
        commands,
        worker_io,
        game_mode,
    ))
}
