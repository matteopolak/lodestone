//! A native hotbar-lock plugin: one claimed key holds a chosen hotbar slot
//! until the same key releases it.
//!
//! # What it is
//!
//! [`HotbarLockPlugin`] is the reference consumer for the public
//! [`lodestone_ecs::SelectSlotIntent`] seam. A toolbelt, kit, or accessibility
//! plugin can keep a preferred item selected without writing [`SelectedSlot`]
//! directly and silently desynchronising the server-held item.
//!
//! # How it works
//!
//! The plugin claims one [`PhysicalKey`] in [`KeyInterceptMode::Consume`]. A
//! press toggles its lock state in [`TickSet::Intent`]. While enabled, its
//! system inserts a one-shot [`SelectSlotIntent`] for the local player every
//! tick. The existing shell consumer owns the actual mutation: it consumes the
//! intent in `TickSet::Send`, updates [`SelectedSlot`], and queues exactly one
//! carried-item echo when the selection changed. Reissuing the desired value
//! is harmless: the consumer treats an already-selected slot as a no-op.
//!
//! The second press stops producing intents. It does not restore a remembered
//! slot or synthesize a later selection; after release, the next human slot
//! selection remains selected. That is the important ownership boundary: this
//! plugin claims only the period it is enabled, and the shell remains the sole
//! selection-and-echo writer.
//!
//! # How to change it
//!
//! Keep the lock expressed as [`SelectSlotIntent`], never a direct
//! [`SelectedSlot`] write. A richer plugin may expose several lock profiles or
//! switch desired slots from its own systems, but the shell consumer must keep
//! owning validation and the carried-item echo. Valid hotbar slots are
//! `0..=8`; [`HotbarLockPlugin::new`] rejects every other value before the
//! plugin is installed.
//!
//! # Configuration
//!
//! Construction takes a physical key and a zero-based desired hotbar slot. No
//! runtime flag or environment variable is used.
//!
//! # Dependencies
//!
//! `lodestone-ecs` supplies the version-free key and intent API. The shell is
//! a dev-only dependency of the integration gate; production users link only
//! this plugin and `lodestone-ecs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::message::MessageReader;
use lodestone_ecs::ecs::prelude::{Commands, Query, Res, Resource, With};
use lodestone_ecs::ecs::schedule::IntoScheduleConfigs;
use lodestone_ecs::{
    GameTick, KeyInterceptMode, LocalPlayer, LocalPlayerPlugin, PhysicalKey, PluginKeyEvent,
    PluginKeybinds, SelectSlotIntent, TickSet,
};

/// Number of selectable hotbar slots. The selection consumer rejects values at
/// or above this bound, so rejecting them at construction makes a bad plugin
/// configuration observable instead of an enabled lock that can never act.
pub const HOTBAR_SLOT_COUNT: usize = 9;

/// Why [`HotbarLockPlugin::new`] refused its desired slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidHotbarSlot {
    /// The requested zero-based slot.
    pub slot: usize,
}

impl std::fmt::Display for InvalidHotbarSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "hotbar slot {} is outside 0..{}", self.slot, HOTBAR_SLOT_COUNT)
    }
}

impl std::error::Error for InvalidHotbarSlot {}

/// Read handle onto a [`HotbarLockPlugin`]'s enabled state.
#[derive(Clone, Debug)]
pub struct HotbarLockState(Arc<AtomicBool>);

impl HotbarLockState {
    /// Whether this plugin is currently issuing slot intents.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The key, desired slot, and state shared by the plugin's systems.
#[derive(Resource, Clone, Debug)]
struct HotbarLockTarget {
    key: PhysicalKey,
    slot: usize,
    enabled: Arc<AtomicBool>,
}

/// An opt-in native plugin that keeps a configured hotbar slot selected while
/// its claimed key has toggled the lock on.
#[derive(Debug)]
pub struct HotbarLockPlugin {
    key: PhysicalKey,
    slot: usize,
    enabled: Arc<AtomicBool>,
}

impl HotbarLockPlugin {
    /// Construct a hotbar lock and a read handle for its state.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidHotbarSlot`] when `slot` is not a selectable hotbar
    /// index. The plugin is then never installed, so an invalid configuration
    /// cannot masquerade as an active lock with no effect.
    pub fn new(
        key: PhysicalKey,
        slot: usize,
    ) -> Result<(Self, HotbarLockState), InvalidHotbarSlot> {
        if slot >= HOTBAR_SLOT_COUNT {
            return Err(InvalidHotbarSlot { slot });
        }
        let enabled = Arc::new(AtomicBool::new(false));
        Ok((
            Self {
                key,
                slot,
                enabled: enabled.clone(),
            },
            HotbarLockState(enabled),
        ))
    }
}

impl Plugin for HotbarLockPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<LocalPlayerPlugin>() {
            app.add_plugins(LocalPlayerPlugin);
        }
        app.world_mut()
            .resource_mut::<PluginKeybinds>()
            .register(self.key.clone(), KeyInterceptMode::Consume);
        app.insert_resource(HotbarLockTarget {
            key: self.key.clone(),
            slot: self.slot,
            enabled: self.enabled.clone(),
        });
        app.add_systems(
            GameTick,
            (toggle_hotbar_lock, issue_locked_slot)
                .chain()
                .in_set(TickSet::Intent),
        );
    }
}

/// Toggle the lock on press edges only. Releases intentionally produce no
/// selection write, so dropping the lock cannot undo a human's later choice.
fn toggle_hotbar_lock(
    mut events: MessageReader<PluginKeyEvent>,
    target: Res<HotbarLockTarget>,
) {
    for event in events.read() {
        if event.pressed && event.key == target.key {
            target.enabled.fetch_xor(true, Ordering::SeqCst);
        }
    }
}

/// While enabled, place one existing one-shot intent on the local player.
/// `Commands` keeps the insertion on the owning schedule; the shell consumes
/// the resulting intent on its normal selection pass.
fn issue_locked_slot(
    mut commands: Commands,
    target: Res<HotbarLockTarget>,
    players: Query<bevy_ecs::entity::Entity, With<LocalPlayer>>,
) {
    if !target.enabled.load(Ordering::SeqCst) {
        return;
    }
    let Ok(player) = players.single() else {
        return;
    };
    commands.entity(player).insert(SelectSlotIntent(target.slot));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_slot_is_refused_before_plugin_installation() {
        assert!(matches!(
            HotbarLockPlugin::new(PhysicalKey::named("KeyH"), HOTBAR_SLOT_COUNT),
            Err(InvalidHotbarSlot { slot }) if slot == HOTBAR_SLOT_COUNT
        ));
    }
}
