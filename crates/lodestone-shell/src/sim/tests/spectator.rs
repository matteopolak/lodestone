use super::*;
use crate::hud::{DebugStats, HudFrame, HudGeometry};
use crate::menu::spectator_menu::{SpectatorMenuOutcome, SpectatorMenuState};
use lodestone_client::{ClientEvent, GameMode};

#[test]
fn normalized_spectator_packets_drive_controls_and_actual_hud_geometry() {
    let mut sim = Sim::new(client_config());
    ingest(&mut sim, login_event(1));
    ingest(&mut sim, ClientEvent::AbilitiesChanged {
        invulnerable: true,
        flying: true,
        can_fly: true,
        instabuild: false,
        flying_speed: 0.05,
        walking_speed: 0.1,
    });
    ingest(&mut sim, ClientEvent::GameModeChanged { game_mode: GameMode::Spectator });
    assert_eq!(sim.game_mode(), Some(GameMode::Spectator));
    assert!(sim.is_spectator());

    let stats = DebugStats::default();
    let populated = || HudFrame {
        show_debug: false,
        hotbar: Some(3),
        health: Some(17.0),
        food: Some(13),
        xp: Some((7, 0.43)),
        held_item: Some(("Inventory item".into(), 1.0)),
        ..HudFrame::new(&stats)
    };
    let mut control = populated();
    control.apply_game_mode(Some(GameMode::Survival));
    assert!(HudGeometry::build(&control, 640, 480).vertex_count() > 0);

    let mut spectator = populated();
    spectator.apply_game_mode(sim.game_mode());
    assert!(!spectator.can_hurt_player);
    assert!(spectator.hotbar.is_none());
    assert!(spectator.held_item.is_none());
    assert_eq!(HudGeometry::build(&spectator, 640, 480).vertex_count(), 0);

    let mut selector = SpectatorMenuState::default();
    assert_eq!(selector.select(None, 10.0), SpectatorMenuOutcome::None);
    let view = selector.view(10.0).expect("pick opens the HUD selector");
    spectator.spectator_hotbar = Some(&view);
    let geometry = HudGeometry::build(&spectator, 640, 480);
    assert!(geometry.vertex_count() > 0, "selector must reach the HUD draw path");
    assert!(geometry.verts.chunks_exact(6).all(|v| v[1] < 0.0),
        "spectator selector geometry must remain in the bottom HUD region");

    ingest(&mut sim, ClientEvent::GameModeChanged { game_mode: GameMode::Creative });
    let mut resumed = populated();
    resumed.spectator_hotbar = Some(&view);
    resumed.apply_game_mode(sim.game_mode());
    assert!(!sim.is_spectator());
    assert!(resumed.spectator_hotbar.is_none());
    assert!(resumed.hotbar.is_some());
    assert!(HudGeometry::build(&resumed, 640, 480).vertex_count() > 0);
}

#[test]
fn spectator_wheel_speed_is_local_clamped_and_mode_gated() {
    let mut sim = Sim::new(client_config());
    ingest(&mut sim, login_event(1));
    ingest(&mut sim, ClientEvent::AbilitiesChanged {
        invulnerable: true,
        flying: true,
        can_fly: true,
        instabuild: false,
        flying_speed: 0.05,
        walking_speed: 0.1,
    });
    let speed = |sim: &Sim| sim.read(|world| {
        world.get::<lodestone_ecs::session::Abilities>(sim.local)
            .expect("session abilities").flying_speed
    });
    sim.adjust_spectator_speed(3);
    assert_eq!(speed(&sim), 0.05);
    ingest(&mut sim, ClientEvent::GameModeChanged { game_mode: GameMode::Spectator });
    sim.adjust_spectator_speed(3);
    assert!((speed(&sim) - 0.065).abs() < 1e-7);
    sim.adjust_spectator_speed(50);
    assert_eq!(speed(&sim), 0.2);
    sim.adjust_spectator_speed(-50);
    assert_eq!(speed(&sim), 0.0);
}
