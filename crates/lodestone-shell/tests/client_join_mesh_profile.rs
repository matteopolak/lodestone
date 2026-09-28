//! Bounded native profile for the client half of a singleplayer join.

#![cfg(not(target_arch = "wasm32"))]
#![recursion_limit = "256"]

use std::time::Duration;

use lodestone::config::{Config, Mode};
use lodestone::gpu::RenderState;
use lodestone::menu::loading::ConnectPhase;
use lodestone::mesher::{MeshBacklog, Meshed};
use lodestone::net::NetClient;
use lodestone::sim::Sim;
use lodestone_controller::Action;
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};
use lodestone_time::Instant;
use lodestone_model::action::{
    ChatMode, ClientAction, ClientSettings, DisplayedSkinParts, MainHand, ParticleStatus,
};

const SEED: i64 = 4242;
const DEADLINE: Duration = Duration::from_secs(120);
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

struct EditProbe {
    aiming_since: Instant,
    target: Option<[i32; 3]>,
    initial_state: Option<u32>,
    clicked_at: Option<Instant>,
    changed: Option<Duration>,
    mesh_uploaded: Option<Duration>,
    presented: Option<Duration>,
}

impl EditProbe {
    fn new() -> Self {
        Self {
            aiming_since: Instant::now(),
            target: None,
            initial_state: None,
            clicked_at: None,
            changed: None,
            mesh_uploaded: None,
            presented: None,
        }
    }
}

fn edit_enabled() -> bool {
    match std::env::var("LODESTONE_CLIENT_JOIN_EDIT") {
        Ok(value) => {
            assert_eq!(value, "1", "LODESTONE_CLIENT_JOIN_EDIT must be 1 when set");
            true
        }
        Err(std::env::VarError::NotPresent) => false,
        Err(error) => panic!("invalid LODESTONE_CLIENT_JOIN_EDIT: {error}"),
    }
}

fn movement_duration() -> Duration {
    let seconds = std::env::var("LODESTONE_CLIENT_JOIN_MOVE_SECONDS")
        .ok()
        .map(|value| {
            value
                .parse::<u64>()
                .expect("LODESTONE_CLIENT_JOIN_MOVE_SECONDS must be an integer")
        })
        .unwrap_or(0);
    assert!(seconds <= 60, "movement profile must be at most 60 seconds");
    Duration::from_secs(seconds)
}

fn chunk_at(x: f64, z: f64) -> (i32, i32) {
    ((x / 16.0).floor() as i32, (z / 16.0).floor() as i32)
}

fn profile_radius() -> u32 {
    std::env::var("LODESTONE_CLIENT_JOIN_RADIUS")
        .ok()
        .map(|value| value.parse().expect("LODESTONE_CLIENT_JOIN_RADIUS must be an integer"))
        .unwrap_or(lodestone::config::DEFAULT_RENDER_DISTANCE)
}

fn target_size() -> (u32, u32) {
    std::env::var("LODESTONE_CLIENT_JOIN_TARGET_SIZE")
        .ok()
        .map(|value| {
            let (width, height) = value
                .split_once('x')
                .expect("LODESTONE_CLIENT_JOIN_TARGET_SIZE must be WIDTHxHEIGHT");
            let width: u32 = width.parse().expect("invalid target width");
            let height: u32 = height.parse().expect("invalid target height");
            assert!(width > 0 && height > 0, "target dimensions must be positive");
            (width, height)
        })
        .unwrap_or((64, 64))
}

fn percentile(samples: &[Duration], percent: usize) -> f64 {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    ordered[(ordered.len() * percent).div_ceil(100) - 1].as_secs_f64() * 1000.0
}

fn unsettled_columns(sim: &Sim, radius: u32) -> serde_json::Value {
    let columns = sim.unsettled_view_columns_at_radius(radius).unwrap_or_default();
    serde_json::json!(columns.iter().take(8).map(|status| serde_json::json!({
        "chunk": status.chunk,
        "missing_sections": status.missing_sections,
        "prior_presentations": status.prior_presentations,
        "waiting_for_halo": status.waiting_for_halo,
        "absent_halo": status.absent_halo,
    })).collect::<Vec<_>>())
}

fn profile_config(radius: u32) -> Config {
    Config {
        mode: Mode::Window,
        render_distance: radius,
        ..Config::default()
    }
}

fn main() {
    let radius = profile_radius();
    let move_for = movement_duration();
    let measure_edit = edit_enabled();
    let server_radius = radius.saturating_add(1);
    let expected_visible_columns = ((radius as usize) * 2 + 1).pow(2);
    let expected_server_columns = ((server_radius as usize) * 2 + 1).pow(2);
    let startup_started = Instant::now();
    let gpu_started = Instant::now();
    let ctx = GpuContext::new_headless_blocking().expect("a headless adapter is required");
    let gpu_context_ns = gpu_started.elapsed().as_nanos();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let (target_width, target_height) = target_size();
    let mut target = HeadlessTarget::new(device, target_width, target_height, format);
    let sim_started = Instant::now();
    let mut sim = Sim::new(profile_config(radius));
    let sim_setup_ns = sim_started.elapsed().as_nanos();
    assert!(
        sim.vanilla_atlas().is_some(),
        "the client join profile requires the vanilla block atlas: {:?}",
        sim.asset_banner()
    );
    let protocol = Config::default().protocol;
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)
        .expect("the default protocol must have an integrated server");
    let render_started = Instant::now();
    let mut render = RenderState::new(
        device,
        queue,
        format,
        target_width,
        target_height,
        sim.vanilla_atlas(),
    );
    let render_setup_ns = render_started.elapsed().as_nanos();
    let startup_ns = startup_started.elapsed().as_nanos();
    let started = Instant::now();
    let open_started = Instant::now();
    let net = NetClient::open_singleplayer(
        server_protocol,
        protocol,
        SEED,
        lodestone::menu::create_world::WorldTypePreset::Normal,
        i32::try_from(server_radius).expect("profile radius fits i32"),
        true,
        None,
        None,
    );
    let open_ns = open_started.elapsed().as_nanos();
    sim.attach_net(net);
    sim.net()
        .expect("singleplayer client is attached")
        .send_action(ClientAction::SetClientSettings(ClientSettings {
            locale: "en_us".to_string(),
            view_distance: i8::try_from(server_radius).unwrap_or(i8::MAX),
            chat_mode: ChatMode::Full,
            chat_colors: true,
            skin_parts: DisplayedSkinParts {
                cape: true,
                jacket: true,
                left_sleeve: true,
                right_sleeve: true,
                left_pants_leg: true,
                right_pants_leg: true,
                hat: true,
            },
            main_hand: MainHand::Right,
            text_filtering: false,
            allow_server_listing: true,
            particle_status: ParticleStatus::All,
        }));
    sim.arm_new_world_loading(radius);
    let expected_initial_columns = sim
        .terrain_progress()
        .expect("the new-world loading view is declared")
        .expected;
    let mut previous_frame = Instant::now();
    let mut first_column = None;
    let mut first_mesh = None;
    let mut first_presented_terrain = None;
    let mut joining = None;
    let mut loading_terrain = None;
    let mut overlay_ready = None;
    let mut player_loaded_ack = None;
    let mut all_initial_columns = None;
    let mut all_initial_meshes_settled = None;
    let mut all_visible_columns = None;
    let mut all_server_columns = None;
    let mut all_visible_meshes_settled = None;
    let mut max_columns = 0usize;
    let mut max_settled_initial_columns = 0usize;
    let mut max_settled_visible_columns = 0usize;
    let mut max_pending = 0usize;
    let mut step_count = 0u64;
    let mut mesh_count = 0usize;
    let mut quad_count = 0usize;
    let mut step_ns = 0u128;
    let mut mesh_drain_ns = 0u128;
    let mut upload_ns = 0u128;
    let mut render_ns = 0u128;
    let mut frame_samples = Vec::new();
    let mut step_samples = Vec::new();
    let mut upload_samples = Vec::new();
    let mut render_samples = Vec::new();
    let mut movement_started: Option<Instant> = None;
    let mut movement_stopped: Option<Instant> = None;
    let mut movement_origin: Option<(f64, f64)> = None;
    let mut movement_end_position: Option<(f64, f64)> = None;
    let mut movement_first_effect = None;
    let mut movement_first_chunk_change = None;
    let mut movement_first_settled_view = None;
    let mut movement_post_stop_settled = None;
    let mut movement_last_chunk = None;
    let mut movement_chunk_changes = 0usize;
    let mut movement_min_settled = usize::MAX;
    let mut movement_last_settled = 0usize;
    let mut movement_max_pending = 0usize;
    let mut movement_frames = Vec::new();
    let mut movement_steps = Vec::new();
    let mut movement_start_tick = None;
    let mut movement_end_tick = None;
    let mut movement_stop_view = None;
    let mut movement_stop_presentation = None;
    let mut movement_post_stop_presented = None;
    let mut movement_stop_backlog: Option<MeshBacklog> = None;
    let mut movement_settlement_tail = Vec::new();
    let mut movement_gap_snapshots = Vec::new();
    let mut last_gap_probe: Option<Instant> = None;
    let mut server_tick_at_ack = None;
    let mut server_tick_at_move_start = None;
    let mut server_tick_at_move_end = None;
    let mut server_tick_last_change: Option<(u64, Instant)> = None;
    let mut server_tick_max_gap = Duration::ZERO;
    let mut last_view_probe: Option<Instant> = None;
    let mut edit: Option<EditProbe> = None;

    while started.elapsed() < DEADLINE {
        let frame_started = Instant::now();
        let dt = previous_frame.elapsed().as_secs_f64();
        previous_frame = frame_started;
        if let Some(start) = movement_started
            && movement_stopped.is_none()
            && start.elapsed() >= move_for
        {
            sim.input_mut(|input| {
                input.set(Action::Forward, false);
                input.set(Action::Sprint, false);
                input.set(Action::Jump, false);
            });
            let position = sim.player().position;
            movement_end_position = Some((position.x, position.z));
            movement_end_tick = Some(sim.tick_count());
            movement_stop_view = sim.view_settlement_at_radius(radius);
            movement_stop_presentation = sim.view_presentation_at_radius(radius);
            if movement_stop_presentation.is_some_and(|(resident, presented, expected)| {
                resident == expected && presented == expected
            }) {
                movement_post_stop_presented = Some(Duration::ZERO);
            }
            movement_stop_backlog = Some(sim.mesh_backlog());
            server_tick_at_move_end = sim
                .net()
                .and_then(NetClient::integrated_tick_monitor)
                .map(|monitor| monitor.snapshot());
            movement_stopped = Some(Instant::now());
        }
        let step_started = Instant::now();
        sim.step(dt);
        sim.update_target(target_width as f32 / target_height as f32);
        if let Some(probe) = edit.as_mut() {
            if probe.clicked_at.is_none() {
                if let Some(hit) = sim.target() {
                    let state = sim.chunk_world().read().block_state_at(
                        hit.block[0], hit.block[1], hit.block[2],
                    );
                    if state.is_some_and(|id| id != lodestone_data::block_states::StateId::AIR.raw()) {
                        probe.target = Some(hit.block);
                        probe.initial_state = state;
                        probe.clicked_at = Some(Instant::now());
                        sim.begin_attack();
                    }
                }
            } else if probe.changed.is_none()
                && probe.target.is_some_and(|block| {
                    sim.chunk_world().read().block_state_at(block[0], block[1], block[2])
                        == Some(lodestone_data::block_states::StateId::AIR.raw())
                })
            {
                probe.changed = probe.clicked_at.map(|clicked| clicked.elapsed());
                sim.end_attack();
            }
            assert!(
                probe.aiming_since.elapsed() < Duration::from_secs(15) || probe.presented.is_some(),
                "block edit did not reach a presented mesh: target={:?} changed={:?} uploaded={:?}",
                probe.target,
                probe.changed,
                probe.mesh_uploaded,
            );
        }
        let step_elapsed = step_started.elapsed();
        step_ns += step_elapsed.as_nanos();
        step_samples.push(step_elapsed);
        if let Some((start, (origin_x, origin_z))) = movement_started.zip(movement_origin)
            && movement_stopped.is_none()
        {
            movement_steps.push(step_elapsed);
            let position = sim.player().position;
            let distance = (position.x - origin_x).hypot(position.z - origin_z);
            if distance > 0.01 {
                movement_first_effect.get_or_insert(start.elapsed());
            }
            let chunk = chunk_at(position.x, position.z);
            if movement_last_chunk != Some(chunk) {
                movement_chunk_changes += 1;
                movement_last_chunk = Some(chunk);
                movement_first_chunk_change.get_or_insert(start.elapsed());
            }
        }
        step_count += 1;

        match sim.connect_phase() {
            ConnectPhase::Connecting => {}
            ConnectPhase::Joining => {
                joining.get_or_insert(started.elapsed());
            }
            ConnectPhase::LoadingTerrain => {
                loading_terrain.get_or_insert(started.elapsed());
            }
        }

        let columns = sim.chunk_count();
        max_columns = max_columns.max(columns);
        if columns > 0 {
            first_column.get_or_insert(started.elapsed());
        }
        if columns >= expected_server_columns {
            all_server_columns.get_or_insert(started.elapsed());
        }
        max_pending = max_pending.max(sim.pending_meshes());
        let mesh_started = Instant::now();
        let meshes = sim.drain_meshes();
        mesh_drain_ns += mesh_started.elapsed().as_nanos();
        let upload_started = Instant::now();
        for Meshed { key, mesh } in meshes {
            first_mesh.get_or_insert(started.elapsed());
            quad_count += mesh.quad_count();
            mesh_count += 1;
            render.upload_section(device, queue, key, &mesh);
            sim.mark_mesh_uploaded(key);
            if let Some(probe) = edit.as_mut()
                && probe.changed.is_some()
                && probe.mesh_uploaded.is_none()
                && probe.target.is_some_and(|block| {
                    let origin = key.origin();
                    (0..3).all(|axis| block[axis] >= origin[axis] && block[axis] < origin[axis] + 16)
                })
            {
                probe.mesh_uploaded = probe.clicked_at.map(|clicked| clicked.elapsed());
            }
        }
        sim.refresh_terrain_readiness();
        let upload_elapsed = upload_started.elapsed();
        upload_ns += upload_elapsed.as_nanos();
        upload_samples.push(upload_elapsed);

        if sim.world_wait().is_none() {
            overlay_ready.get_or_insert(started.elapsed());
        }
        if all_initial_meshes_settled.is_none() {
            if let Some((resident, settled, expected)) = sim.visible_view_settlement() {
                assert_eq!(expected, expected_initial_columns);
                max_settled_initial_columns = max_settled_initial_columns.max(settled);
                if resident == expected {
                    all_initial_columns.get_or_insert(started.elapsed());
                }
                if settled >= expected {
                    all_initial_meshes_settled.get_or_insert(started.elapsed());
                }
            }
        }
        if player_loaded_ack.is_none()
            || last_view_probe.is_none_or(|last| last.elapsed() >= Duration::from_millis(100))
        {
            last_view_probe = Some(Instant::now());
            if let Some((resident, settled, expected)) = sim.view_settlement_at_radius(radius) {
                assert_eq!(expected, expected_visible_columns);
                max_settled_visible_columns = max_settled_visible_columns.max(settled);
                if resident == expected {
                    all_visible_columns.get_or_insert(started.elapsed());
                }
                if settled >= expected {
                    all_visible_meshes_settled.get_or_insert(started.elapsed());
                }
                if let Some(start) = movement_started {
                    if movement_stopped.is_none() {
                        movement_min_settled = movement_min_settled.min(settled);
                    }
                    movement_last_settled = settled;
                    if movement_first_chunk_change.is_some()
                        && resident == expected
                        && settled == expected
                    {
                        movement_first_settled_view.get_or_insert(start.elapsed());
                    }
                    if let Some(stop) = movement_stopped {
                        if movement_post_stop_presented.is_none()
                            && sim.view_presentation_at_radius(radius).is_some_and(
                                |(resident, presented, expected)| {
                                    resident == expected && presented == expected
                                },
                            )
                        {
                            movement_post_stop_presented = Some(stop.elapsed());
                        }
                        if movement_gap_snapshots.len() < 12
                            && last_gap_probe.is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
                        {
                            last_gap_probe = Some(Instant::now());
                            movement_gap_snapshots.push(serde_json::json!({
                                "elapsed_ms": stop.elapsed().as_secs_f64() * 1000.0,
                                "columns": unsettled_columns(&sim, radius),
                            }));
                        }
                        if movement_settlement_tail.len() < 64 {
                            let backlog = sim.mesh_backlog();
                            movement_settlement_tail.push(serde_json::json!({
                                "elapsed_ms": stop.elapsed().as_secs_f64() * 1000.0,
                                "resident": resident,
                                "settled": settled,
                                "ready_columns": backlog.ready_columns,
                                "waiting_columns": backlog.waiting_columns,
                                "forced_columns": backlog.forced_columns,
                                "pending_sections": backlog.pending_sections,
                            }));
                        }
                        if resident == expected && settled == expected {
                            movement_post_stop_settled.get_or_insert(stop.elapsed());
                        }
                    }
                }
            }
        }

        let render_started = Instant::now();
        let frame = target.acquire().expect("headless target acquire");
        let stats = render.render(
            device,
            queue,
            frame.view(),
            &sim.camera(target_width as f32 / target_height as f32),
            None,
            &[],
        );
        frame.present(queue);
        if let Some(probe) = edit.as_mut()
            && probe.mesh_uploaded.is_some()
            && probe.presented.is_none()
        {
            probe.presented = probe.clicked_at.map(|clicked| clicked.elapsed());
        }
        if sim.acknowledge_presented_initial_world() {
            player_loaded_ack = Some(started.elapsed());
            server_tick_at_ack = sim
                .net()
                .and_then(NetClient::integrated_tick_monitor)
                .map(|monitor| monitor.snapshot());
        }
        if player_loaded_ack.is_some()
            && let Some(monitor) = sim.net().and_then(NetClient::integrated_tick_monitor)
        {
            let count = monitor.server_tick_count();
            let now = Instant::now();
            match server_tick_last_change {
                Some((previous, changed)) if count != previous => {
                    server_tick_max_gap = server_tick_max_gap.max(now.duration_since(changed));
                    server_tick_last_change = Some((count, now));
                }
                Some((_, changed)) => {
                    server_tick_max_gap = server_tick_max_gap.max(now.duration_since(changed));
                }
                None => server_tick_last_change = Some((count, now)),
            }
        }
        let render_elapsed = render_started.elapsed();
        render_ns += render_elapsed.as_nanos();
        render_samples.push(render_elapsed);
        frame_samples.push(frame_started.elapsed());
        if movement_started.is_some() && movement_stopped.is_none() {
            movement_frames.push(frame_started.elapsed());
            movement_max_pending = movement_max_pending.max(sim.pending_meshes());
        }
        if first_presented_terrain.is_none() && (stats.sections_drawn > 0 || stats.water_sections_drawn > 0) {
            first_presented_terrain = Some(started.elapsed());
        }

        if measure_edit && edit.is_none() && player_loaded_ack.is_some() {
            sim.player_mut(|player| player.pitch = 80.0);
            edit = Some(EditProbe::new());
        }

        if move_for > Duration::ZERO
            && movement_started.is_none()
            && overlay_ready.is_some()
            && first_presented_terrain.is_some()
            && (!measure_edit || edit.as_ref().is_some_and(|probe| probe.presented.is_some()))
        {
            let position = sim.player().position;
            movement_origin = Some((position.x, position.z));
            movement_last_chunk = Some(chunk_at(position.x, position.z));
            movement_start_tick = Some(sim.tick_count());
            server_tick_at_move_start = sim
                .net()
                .and_then(NetClient::integrated_tick_monitor)
                .map(|monitor| monitor.snapshot());
            sim.input_mut(|input| {
                input.set(Action::Forward, true);
                input.set(Action::Sprint, true);
                input.set(Action::Jump, true);
            });
            movement_started = Some(Instant::now());
        }

        if overlay_ready.is_some()
            && all_server_columns.is_some()
            && all_visible_columns.is_some()
            && all_visible_meshes_settled.is_some()
            && first_presented_terrain.is_some()
            && (move_for == Duration::ZERO || movement_post_stop_settled.is_some())
            && (!measure_edit || edit.as_ref().is_some_and(|probe| probe.presented.is_some()))
        {
            break;
        }
        std::thread::sleep(FRAME_INTERVAL.saturating_sub(frame_started.elapsed()));
    }

    let elapsed = started.elapsed();
    let ms = |value: Option<Duration>| value.map(|d| d.as_secs_f64() * 1000.0);
    let movement_distance = movement_origin.map(|(x, z)| {
        let (end_x, end_z) = movement_end_position.unwrap_or_else(|| {
            let position = sim.player().position;
            (position.x, position.z)
        });
        (end_x - x).hypot(end_z - z)
    });
    let movement = movement_started.map(|start| serde_json::json!({
        "requested_seconds": move_for.as_secs(),
        "elapsed_ms": movement_stopped.map_or_else(|| start.elapsed(), |stop| stop.duration_since(start)).as_secs_f64() * 1000.0,
        "horizontal_distance_blocks": movement_distance,
        "first_position_effect_ms": ms(movement_first_effect),
        "first_chunk_change_ms": ms(movement_first_chunk_change),
        "first_shifted_view_settled_ms": ms(movement_first_settled_view),
        "post_stop_view_settle_ms": ms(movement_post_stop_settled),
        "post_stop_view_present_ms": ms(movement_post_stop_presented),
        "presentation_at_stop": movement_stop_presentation.map(|(resident, presented, expected)| serde_json::json!({
            "resident": resident,
            "presented": presented,
            "expected": expected,
        })),
        "view_at_stop": movement_stop_view.map(|(resident, settled, expected)| serde_json::json!({
            "resident": resident,
            "settled": settled,
            "expected": expected,
        })),
        "mesh_backlog_at_stop": movement_stop_backlog.map(|backlog| serde_json::json!({
            "ready_columns": backlog.ready_columns,
            "waiting_columns": backlog.waiting_columns,
            "forced_columns": backlog.forced_columns,
            "pending_sections": backlog.pending_sections,
        })),
        "settlement_tail": movement_settlement_tail,
        "gap_snapshots": movement_gap_snapshots,
        "chunk_changes": movement_chunk_changes,
        "min_settled_columns": movement_min_settled.min(expected_visible_columns),
        "last_settled_columns": movement_last_settled,
        "max_pending_meshes": movement_max_pending,
        "sim_ticks": movement_end_tick.unwrap_or_else(|| sim.tick_count()).saturating_sub(movement_start_tick.unwrap_or(0)),
        "frame_p99_ms": percentile(&movement_frames, 99),
        "frame_max_ms": percentile(&movement_frames, 100),
        "step_p99_ms": percentile(&movement_steps, 99),
        "step_max_ms": percentile(&movement_steps, 100),
        "frames_over_33ms": movement_frames.iter().filter(|elapsed| **elapsed > Duration::from_millis(33)).count(),
        "frames_over_100ms": movement_frames.iter().filter(|elapsed| **elapsed > Duration::from_millis(100)).count(),
    }));
    let server_tick_at_end = sim
        .net()
        .and_then(NetClient::integrated_tick_monitor)
        .map(|monitor| monitor.snapshot());
    let server_tick = serde_json::json!({
        "at_ack": server_tick_at_ack.map(|(_, count)| count),
        "at_move_start": server_tick_at_move_start.map(|(_, count)| count),
        "at_move_end": server_tick_at_move_end.map(|(_, count)| count),
        "at_end": server_tick_at_end.map(|(_, count)| count),
        "movement_tick_delta": server_tick_at_move_start.zip(server_tick_at_move_end).map(|((_, start), (_, end))| end.saturating_sub(start)),
        "movement_overrun_delta": server_tick_at_move_start.zip(server_tick_at_move_end).map(|((start, _), (end, _))| end.overrun_count.saturating_sub(start.overrun_count)),
        "tps_at_end": server_tick_at_end.map(|(stats, _)| stats.tps),
        "mspt_avg_at_end": server_tick_at_end.map(|(stats, _)| stats.mspt_avg_ms),
        "max_observed_tick_gap_ms": server_tick_max_gap.as_secs_f64() * 1000.0,
    });
    let edit = edit.map(|probe| serde_json::json!({
        "target": probe.target,
        "initial_state": probe.initial_state,
        "aim_ms": probe.clicked_at.map(|clicked| clicked.duration_since(probe.aiming_since).as_secs_f64() * 1000.0),
        "input_to_air_ms": ms(probe.changed),
        "input_to_mesh_upload_ms": ms(probe.mesh_uploaded),
        "input_to_present_ms": ms(probe.presented),
    }));
    let report = serde_json::json!({
        "schema": "lodestone-client-join-mesh-profile-v13",
        "seed": SEED,
        "target_size": [target_width, target_height],
        "visible_radius": radius,
        "requested_server_radius": server_radius,
        "expected_initial_columns": expected_initial_columns,
        "expected_visible_columns": expected_visible_columns,
        "requested_server_columns": expected_server_columns,
        "startup_cpu_ms": startup_ns as f64 / 1_000_000.0,
        "gpu_context_cpu_ms": gpu_context_ns as f64 / 1_000_000.0,
        "sim_setup_cpu_ms": sim_setup_ns as f64 / 1_000_000.0,
        "render_setup_cpu_ms": render_setup_ns as f64 / 1_000_000.0,
        "timed_out": elapsed >= DEADLINE,
        "elapsed_ms": elapsed.as_secs_f64() * 1000.0,
        "first_column_ms": ms(first_column),
        "first_mesh_ms": ms(first_mesh),
        "first_presented_terrain_ms": ms(first_presented_terrain),
        "joining_phase_ms": ms(joining),
        "loading_terrain_phase_ms": ms(loading_terrain),
        "overlay_ready_ms": ms(overlay_ready),
        "player_loaded_ack_ms": ms(player_loaded_ack),
        "all_initial_columns_ms": ms(all_initial_columns),
        "all_initial_meshes_settled_ms": ms(all_initial_meshes_settled),
        "all_visible_columns_ms": ms(all_visible_columns),
        "all_requested_server_columns_ms": ms(all_server_columns),
        "all_visible_meshes_settled_ms": ms(all_visible_meshes_settled),
        "max_loaded_columns": max_columns,
        "max_settled_initial_columns": max_settled_initial_columns,
        "max_settled_visible_columns": max_settled_visible_columns,
        "max_pending_meshes": max_pending,
        "steps": step_count,
        "meshes_uploaded": mesh_count,
        "uploaded_quads": quad_count,
        "open_singleplayer_cpu_ms": open_ns as f64 / 1_000_000.0,
        "step_cpu_ms": step_ns as f64 / 1_000_000.0,
        "mesh_drain_cpu_ms": mesh_drain_ns as f64 / 1_000_000.0,
        "mesh_upload_cpu_ms": upload_ns as f64 / 1_000_000.0,
        "render_cpu_ms": render_ns as f64 / 1_000_000.0,
        "average_step_ms": step_ns as f64 / step_count.max(1) as f64 / 1_000_000.0,
        "frame_p95_ms": percentile(&frame_samples, 95),
        "frame_p99_ms": percentile(&frame_samples, 99),
        "frame_max_ms": percentile(&frame_samples, 100),
        "step_p99_ms": percentile(&step_samples, 99),
        "step_max_ms": percentile(&step_samples, 100),
        "upload_p99_ms": percentile(&upload_samples, 99),
        "upload_max_ms": percentile(&upload_samples, 100),
        "render_p99_ms": percentile(&render_samples, 99),
        "render_max_ms": percentile(&render_samples, 100),
        "frames_over_33ms": frame_samples.iter().filter(|elapsed| **elapsed > Duration::from_millis(33)).count(),
        "frames_over_100ms": frame_samples.iter().filter(|elapsed| **elapsed > Duration::from_millis(100)).count(),
        "movement": movement,
        "server_tick": server_tick,
        "edit": edit,
    });
    println!("CLIENT_JOIN_MESH_PROFILE {report}");
    assert!(!report["timed_out"].as_bool().unwrap_or(true), "{report}");
    assert!(first_presented_terrain.is_some(), "no terrain reached a presented frame: {report}");
    assert!(player_loaded_ack.is_some(), "the new world was never acknowledged after presentation: {report}");
    assert!(server_tick_at_ack.is_some(), "the integrated server tick monitor was unavailable: {report}");
    if move_for > Duration::ZERO {
        assert!(movement_chunk_changes > 0, "movement never entered a new chunk: {report}");
    }
    if measure_edit {
        assert!(report["edit"]["input_to_present_ms"].is_number(), "block edit never reached a presented frame: {report}");
    }
}

#[test]
#[ignore = "bounded client join profile; run with --nocapture under Samply"]
fn client_join_mesh_profile() {
    main();
}
