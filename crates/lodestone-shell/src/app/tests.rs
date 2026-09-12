//! `app`'s unit tests, split into cohesive thematic modules.

use super::*;
use super::session::container_cursor_center;
use crate::menu::Screen;
use lodestone_data::item::Item;

#[path = "tests/menus_loading.rs"]
mod menus_loading;
#[path = "tests/commands.rs"]
mod commands;
#[path = "tests/chat_click.rs"]
mod chat_click;
#[path = "tests/world_seed.rs"]
mod world_seed;
#[path = "tests/input_hotbar.rs"]
mod input_hotbar;
#[path = "tests/pacing.rs"]
mod pacing;
#[path = "tests/key_resolution.rs"]
mod key_resolution;
#[path = "tests/menu_shortcuts.rs"]
mod menu_shortcuts;
#[path = "tests/gameplay_keys.rs"]
mod gameplay_keys;
#[path = "tests/session_lifecycle.rs"]
mod session_lifecycle;
#[path = "tests/container_commands.rs"]
mod container_commands;
#[path = "tests/server_list.rs"]
mod server_list;
#[path = "tests/render_controls.rs"]
mod render_controls;

/// A cheap sim shared by pacing and session tests: headless mode with the
/// smallest render distance that still generates real terrain.
fn pacing_sim() -> Sim {
    Sim::with_demo_world(Config {
        mode: Mode::Headless,
        render_distance: 2,
        ..Config::default()
    })
}

fn ticks_for(sim: &mut Sim, dt: f64) -> u64 {
    let before = sim.tick_count();
    sim.step(dt);
    sim.tick_count() - before
}

fn resolve_ctrl(gate: KeyGate, code: KeyCode, pressed: bool) -> Option<KeyOutcome> {
    resolve_key(&Keybinds::new(), gate, Some(code), pressed, true, None)
}

/// Seed an account so menu tests exercise the owning account path.
fn seed_owning_account(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).expect("a temp dir for the seeded roster");
    let mut meta = lodestone_auth::AccountsMetadata::default();
    let id = uuid::Uuid::new_v4();
    meta.upsert(lodestone_auth::AccountProfile {
        profile_id: id,
        username: "OwnerAccount".to_owned(),
        skin_url: None,
        last_used: 1,
    });
    meta.selected = Some(id);
    meta.save_to(&dir.join("profiles.json"))
        .expect("the temp roster must be writable");
}
