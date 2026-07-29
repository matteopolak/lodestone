//! The core plugin every `App` in the tree installs.

use bevy_app::{App, Plugin, Update};
use bevy_ecs::schedule::IntoScheduleConfigs;

use crate::schedules::{Extract, GameTick, NetIngest};
use crate::sets::{ExtractSet, FrameSet, IngestSet, TickSet};

/// Registers the Stage-0 schedule/set scaffolding
/// (`docs/bevy-migration.md` §4.2) on an `App`: the three schedules this
/// crate owns (`NetIngest`, `GameTick`, `Extract`), plus the internal
/// ordering of all four schedules' public sets, including bevy's own
/// `Update`.
///
/// Deliberately carries **no game state of its own** — not even
/// [`crate::WorldTime`]. Whoever owns the authoritative `World` inserts that
/// resource explicitly; if `CorePlugin` did it, every `App` built with it
/// (there will be more than one before this migration is done — see the note
/// on two-`World`s-in-one-process in `docs/bevy-migration.md`'s Stage 0
/// report) would get its own silently-diverging copy, which is exactly the
/// "two sources of truth" failure the whole migration exists to delete.
#[derive(Debug, Default)]
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.init_schedule(NetIngest);
        app.configure_sets(
            NetIngest,
            (IngestSet::Drain, IngestSet::Apply, IngestSet::Index).chain(),
        );

        app.init_schedule(GameTick);
        app.configure_sets(
            GameTick,
            (
                TickSet::Input,
                TickSet::Physics,
                TickSet::Predict,
                TickSet::Animate,
                TickSet::Send,
            )
                .chain(),
        );

        // `Update` already exists (installed by `MainSchedulePlugin` as part
        // of `App::new()`/`App::default()`); `configure_sets` creates it if
        // it does not, so this is safe even against `App::empty()`.
        app.configure_sets(
            Update,
            (FrameSet::Input, FrameSet::Interpolate, FrameSet::Camera).chain(),
        );

        app.init_schedule(Extract);
        app.configure_sets(
            Extract,
            (ExtractSet::Terrain, ExtractSet::Entities, ExtractSet::Hud).chain(),
        );
    }
}
