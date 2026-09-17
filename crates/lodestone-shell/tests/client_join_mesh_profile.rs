//! Bounded native profile for the client half of a singleplayer join.

#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use lodestone::config::{Config, Mode};
use lodestone::gpu::RenderState;
use lodestone::menu::loading::ConnectPhase;
use lodestone::mesher::Meshed;
use lodestone::net::NetClient;
use lodestone::sim::Sim;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};
use lodestone_time::Instant;
use lodestone_model::action::{
    ChatMode, ClientAction, ClientSettings, DisplayedSkinParts, MainHand, ParticleStatus,
};

const SEED: i64 = 4242;
const DEADLINE: Duration = Duration::from_secs(120);
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

fn profile_radius() -> u32 {
    std::env::var("LODESTONE_CLIENT_JOIN_RADIUS")
        .ok()
        .map(|value| value.parse().expect("LODESTONE_CLIENT_JOIN_RADIUS must be an integer"))
        .unwrap_or(lodestone::config::DEFAULT_RENDER_DISTANCE)
}

fn profile_config(radius: u32) -> Config {
    Config {
        mode: Mode::Window,
        render_distance: radius,
        ..Config::default()
    }
}

fn camera(sim: &Sim, radius: u32) -> Camera {
    let position = sim.player().position;
    Camera {
        position: glam::Vec3::new(position.x as f32, position.y as f32, position.z as f32),
        yaw: sim.player().yaw,
        pitch: sim.player().pitch,
        fov_y_degrees: 70.0,
        aspect: 1.0,
        near: 0.05,
        far: Camera::far_for_render_distance(radius, 0),
    }
}

fn main() {
    let radius = profile_radius();
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
    let mut target = HeadlessTarget::new(device, 64, 64, format);
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
    let mut render = RenderState::new(device, queue, format, 64, 64, sim.vanilla_atlas());
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
    let mut previous_frame = Instant::now();
    let mut first_column = None;
    let mut first_mesh = None;
    let mut first_presented_terrain = None;
    let mut joining = None;
    let mut loading_terrain = None;
    let mut overlay_ready = None;
    let mut all_visible_columns = None;
    let mut all_server_columns = None;
    let mut all_visible_meshes_settled = None;
    let mut max_columns = 0usize;
    let mut max_settled_visible_columns = 0usize;
    let mut max_pending = 0usize;
    let mut step_count = 0u64;
    let mut mesh_count = 0usize;
    let mut quad_count = 0usize;
    let mut step_ns = 0u128;
    let mut mesh_drain_ns = 0u128;
    let mut upload_ns = 0u128;
    let mut render_ns = 0u128;

    while started.elapsed() < DEADLINE {
        let frame_started = Instant::now();
        let dt = previous_frame.elapsed().as_secs_f64();
        previous_frame = frame_started;
        let step_started = Instant::now();
        sim.step(dt);
        step_ns += step_started.elapsed().as_nanos();
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
        if sim
            .terrain_progress()
            .is_some_and(|progress| progress.loaded >= expected_visible_columns)
        {
            all_visible_columns.get_or_insert(started.elapsed());
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
        }
        sim.refresh_terrain_readiness();
        upload_ns += upload_started.elapsed().as_nanos();

        if sim.world_wait().is_none() {
            overlay_ready.get_or_insert(started.elapsed());
        }
        if all_visible_columns.is_some() && sim.pending_meshes() == 0 {
            let (_, settled, expected) = sim
                .visible_view_settlement()
                .expect("the attached session has a visible view");
            max_settled_visible_columns = max_settled_visible_columns.max(settled);
            if settled >= expected {
                all_visible_meshes_settled.get_or_insert(started.elapsed());
            }
        }

        let render_started = Instant::now();
        let frame = target.acquire().expect("headless target acquire");
        let stats = render.render(device, queue, frame.view(), &camera(&sim, radius), None, &[]);
        frame.present(queue);
        render_ns += render_started.elapsed().as_nanos();
        if first_presented_terrain.is_none() && (stats.sections_drawn > 0 || stats.water_sections_drawn > 0) {
            first_presented_terrain = Some(started.elapsed());
        }

        if overlay_ready.is_some()
            && all_visible_columns.is_some()
            && all_visible_meshes_settled.is_some()
            && first_presented_terrain.is_some()
        {
            break;
        }
        std::thread::sleep(FRAME_INTERVAL.saturating_sub(frame_started.elapsed()));
    }

    let elapsed = started.elapsed();
    let ms = |value: Option<Duration>| value.map(|d| d.as_secs_f64() * 1000.0);
    let report = serde_json::json!({
        "schema": "lodestone-client-join-mesh-profile-v6",
        "seed": SEED,
        "visible_radius": radius,
        "requested_server_radius": server_radius,
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
        "all_visible_columns_ms": ms(all_visible_columns),
        "all_requested_server_columns_ms": ms(all_server_columns),
        "all_visible_meshes_settled_ms": ms(all_visible_meshes_settled),
        "max_loaded_columns": max_columns,
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
    });
    println!("CLIENT_JOIN_MESH_PROFILE {report}");
    assert!(!report["timed_out"].as_bool().unwrap_or(true), "{report}");
    assert!(first_presented_terrain.is_some(), "no terrain reached a presented frame: {report}");
}

#[test]
#[ignore = "bounded client join profile; run with --nocapture under Samply"]
fn client_join_mesh_profile() {
    main();
}
