use super::*;

impl WindowApp {
    pub(super) fn check_singleplayer_benchmark_deadline(&mut self, now: Instant) {
        if self.benchmark.as_ref().is_some_and(|driver| driver.join_timed_out(now)) {
            tracing::error!(target: "frame_benchmark", "singleplayer benchmark failed: join deadline expired");
            self.ui.request_quit();
        }
    }

    pub(super) fn observe_singleplayer_benchmark_present(
        &mut self, now: Instant, initial_ready: bool, terrain_drawn: bool,
    ) {
        if !self.benchmark.as_ref().is_some_and(|driver| {
            driver.workload() == crate::config::BenchmarkWorkload::Singleplayer
        }) {
            return;
        }
        let launch = self.benchmark.as_mut().is_some_and(|driver| driver.start_singleplayer(now));
        if launch {
            use crate::menu::create_world::{CreateWorldOutcome, WorldCreationConfig, WorldGameMode, WorldTypePreset};
            let config = WorldCreationConfig {
                name: "Surface responsiveness".into(),
                seed: "4242".into(),
                game_mode: WorldGameMode::Survival,
                world_type: WorldTypePreset::Normal,
                ..WorldCreationConfig::default()
            };
            tracing::info!(target: "frame_benchmark", saves_root = ?self.nav.saves_root(), "singleplayer world creation started");
            let action = self.nav.apply_create_world(&mut self.ui, CreateWorldOutcome::Create(config));
            let accepted = matches!(action, crate::menu::nav::MenuAction::Singleplayer(..));
            self.apply_menu_action(action);
            if !accepted {
                tracing::error!(target: "frame_benchmark", "singleplayer benchmark failed: normal world creation refused");
                self.ui.request_quit();
            }
            return;
        }
        if initial_ready {
            if let Some(driver) = self.benchmark.as_mut() {
                driver.player_loaded();
            }
        }
        if terrain_drawn && self.gameplay_input_ready() {
            if let Some(elapsed) = self.benchmark.as_mut().and_then(|driver| driver.presented_singleplayer(now)) {
                tracing::info!(target: "frame_benchmark", elapsed_ms = elapsed.as_secs_f64() * 1000.0, "singleplayer initial world presented");
            }
        }
        if !self.benchmark.as_mut().is_some_and(|driver| driver.sample_due(now)) {
            return;
        }
        let view = self.sim.view_presentation_at_radius(self.config.render_distance);
        let backlog = self.sim.mesh_backlog();
        tracing::info!(
            target: "frame_benchmark", position = ?self.sim.stats.position,
            pitch_degrees = self.sim.stats.pitch, yaw_degrees = self.sim.stats.yaw,
            render_status = %self.sim.stats.status,
            textured_atlas = self.sim.vanilla_atlas().is_some(),
            view = ?view, pending_meshes = self.sim.pending_meshes(),
            ready_columns = backlog.ready_columns, waiting_columns = backlog.waiting_columns,
            terrain_gpu_occupied_bytes = self.sim.stats.vram_bytes,
            terrain_gpu_reserved_bytes = self.sim.stats.vram_reserved_bytes,
            terrain_drawn, rss_bytes = crate::hud::process_rss_bytes(),
            "singleplayer surface sample"
        );
        if let Some((ticks, ecs_ticks)) = self.sim.net().and_then(|net| net.integrated_tick_monitor()).map(|monitor| monitor.snapshot()) {
            tracing::info!(
                target: "frame_benchmark", ticks = ticks.tick_count, ecs_ticks,
                mspt_ms = ticks.mspt_ms, overruns = ticks.overrun_count,
                wake_p95_ms = ticks.schedule.service_lateness_p95_ms,
                wake_max_ms = ticks.schedule.service_lateness_max_ms,
                deadline_max_ms = ticks.schedule.deadline_lateness_max_ms,
                catch_up_ticks = ticks.schedule.catch_up_ticks, shed_ticks = ticks.schedule.shed_ticks,
                mob_item_max_ms = ticks.mobs_and_items.max_ms,
                physics_max_ms = ticks.scheduled_and_physics.max_ms,
                "singleplayer tick sample"
            );
        }
    }
}
