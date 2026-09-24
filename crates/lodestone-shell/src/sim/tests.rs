use std::collections::{HashMap, HashSet};

use super::*;
use crate::config::{Config, Mode};
use lodestone_game::placement::{Axis, Half, PlacedState};
use lodestone_ecs::player::SWIMMING_EYE_HEIGHT;
use lodestone_physics::UseEffects;

fn test_config() -> Config {
    Config {
        mode: Mode::Headless,
        render_distance: 2,
        ..Config::default()
    }
}

/// Fold one `ClientEvent` into this `Sim`'s `World` exactly the way the net
/// thread's `lodestone_client::state::SharedState::apply` does — enqueue,
/// run `NetIngest` once, one event per run.
///
/// # Why the loopback feed is not enough for these
///
/// `NetClient::loopback_with_feed` models the `NetUpdate` channel — the
/// *driver's* reaction path. It does not model `SharedState::apply`, which is
/// where the local player's server-reported state (vitals, xp, the entity id,
/// game mode, dimension, liveness) is folded, and there is no `SharedState` in
/// a loopback harness at all. Production runs **both** paths for one packet,
/// so a test that needs both drives both — which is closer to production than
/// the `NetUpdate::Health` these tests used to feed, because that arm was the
/// duplicate fold the collapse deleted.
fn ingest(sim: &mut Sim, event: lodestone_client::ClientEvent) {
    sim.write(|w| {
        w.resource_mut::<lodestone_ecs::ingest::IngestQueue>()
            .push(event);
        w.run_schedule(lodestone_ecs::NetIngest);
    });
}
#[path = "tests/world-mining.rs"]
mod world_mining;
#[path = "tests/sim-lifecycle.rs"]
mod sim_lifecycle;
#[path = "tests/ingest-effects.rs"]
mod ingest_effects;
#[path = "tests/session-overlays.rs"]
mod session_overlays;
#[path = "tests/placement.rs"]
mod placement;
#[path = "tests/player-actions.rs"]
mod player_actions;
#[path = "tests/item-books.rs"]
mod item_books;
#[path = "tests/item-use.rs"]
mod item_use;
#[path = "tests/combat-entities.rs"]
mod combat_entities;
#[path = "tests/world-render.rs"]
mod world_render;
#[path = "tests/world-lifecycle.rs"]
mod world_lifecycle;
#[path = "tests/presentation.rs"]
mod presentation;

pub(super) use item_books::give_main_hand_item;
pub(super) use player_actions::peak_swing_over;
pub(super) use item_books::install_player_interact_veto;
pub(super) use world_mining::{client_config, displayed_sidebar, login_event};
