//! WorldEdit-class bulk region editing: [`EditSession`] is the batched
//! fill/replace/undo primitive, built on
//! [`lodestone_world::World::fill_region_capturing`] and
//! [`lodestone_ecs::ChunkWorldWrite`]. [`WorldEditPlugin`] is the thin real
//! consumer wiring it into a `GameTick` schedule, so the crate is more than a
//! library of functions nothing calls.

pub mod session;

pub use session::{EditSession, EditSessions, Selection};

use bevy_ecs::prelude::ResMut;
use bevy_ecs::resource::Resource;
use bevy_ecs::system::Res;
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::{ChunkWorldWrite, GameTick};

/// A queued bulk-fill request — the shape a real plugin's chat-command
/// handler (`//set stone`, `//replace dirt stone`) would push, kept generic
/// here since command parsing is out of this issue's scope.
#[derive(Debug, Clone, Copy)]
pub struct FillRequest {
    /// Whichever key a plugin uses to keep one [`EditSession`] per author —
    /// a player entity id in the common case, but this crate does not
    /// interpret it.
    pub session_key: i32,
    pub selection: Selection,
    pub state: u32,
    pub physics: bool,
}

/// Requests queued since the last drain — a plain `Vec` resource, the same
/// shape `lodestone_ecs::player::ActionQueue` uses for the sanctioned plugin
/// egress (`docs/plugin-api.md`'s correction note on why `ActionQueue` won
/// over a bevy `Message` for exactly this "needs synchronous drain-time
/// application" case).
#[derive(Resource, Debug, Default)]
pub struct FillRequests(pub Vec<FillRequest>);

/// Drains [`FillRequests`], applying each through the requester's own
/// [`EditSession`] (creating one on first use).
fn apply_fill_requests(
    mut requests: ResMut<FillRequests>,
    mut sessions: ResMut<EditSessions>,
    store: Res<ChunkWorldWrite>,
) {
    for request in requests.0.drain(..) {
        let session = sessions
            .0
            .entry(request.session_key)
            .or_insert_with(|| EditSession::new(store.clone()));
        session.fill(request.selection, request.state, request.physics);
    }
}

/// Installs [`EditSessions`]/[`FillRequests`] and the system draining the
/// latter into the former, in `GameTick`.
///
/// Requires a [`ChunkWorldWrite`] resource to already be installed — this
/// plugin does not build a chunk store of its own, matching every other
/// consumer of that resource (`drive_placement`, `lodestone-autopilot`'s
/// reads via the paired [`lodestone_ecs::ChunkWorld`]).
#[derive(Debug, Default)]
pub struct WorldEditPlugin;

impl Plugin for WorldEditPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditSessions>();
        app.init_resource::<FillRequests>();
        app.add_systems(GameTick, apply_fill_requests);
    }
}
