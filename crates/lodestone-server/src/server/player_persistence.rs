//! Player save and restore: the native player record, the legacy player-data snapshot, and publishing a saved player into the live registry.

use super::*;

/// This world's per-player `.dat` store, if it has one.
///
/// One accessor rather than the same `world_registries().and_then(...)` chain at
/// three call sites, because the failure mode of getting it wrong is invisible:
/// a chain that returns `None` where a store exists produces a server that joins,
/// plays and saves nothing, with no error. Keeping this lookup in one accessor
/// makes the save-store dependency explicit at each call site.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn player_store<S: ChunkSource + ?Sized>(source: &S) -> Option<crate::player_data::PlayerDataStore> {
    source
        .world_registries()
        .and_then(|registries| registries.player_data)
}

/// The native half of one live connection's bounded player persistence.
///
/// The complete Anvil [`PlayerData`](crate::player_data::PlayerData) remains
/// the source of truth whenever it has a value for inventory, health, game
/// mode or opaque fields. This session adds a typed locator and can fill the
/// independently consumable native game-mode gap only when Anvil has none. A
/// built-in saved dimension selects that world's sibling source before chunks
/// stream. A corrupt read, unavailable sibling, or protocol without the needed
/// dimension frame blocks a later overwrite so recovery evidence survives.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub(super) struct NativePlayerSession {
    pub(super) storage: Arc<crate::world_storage::WorldStorage>,
    pub(super) uuid: [u8; 16],
    pub(super) loaded: Option<crate::world_storage::NativePlayerData>,
    pub(super) save_blocked: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativePlayerSession {
    pub(super) fn load<S: ChunkSource + ?Sized>(source: &S, uuid: uuid::Uuid) -> Option<Self> {
        let storage = source.world_registries()?.native_storage?;
        let uuid = *uuid.as_bytes();
        match storage.load_player_data(uuid) {
            Ok(loaded) => {
                Some(Self {
                    storage,
                    uuid,
                    loaded,
                    save_blocked: false,
                })
            }
            Err(error) => {
                tracing::error!(
                    "native player locator for {uuid:02x?} could not be read and will NOT be overwritten this session: {error}"
                );
                Some(Self {
                    storage,
                    uuid,
                    loaded: None,
                    save_blocked: true,
                })
            }
        }
    }

    pub(super) fn join_position(&self, fallback: Vec3) -> Vec3 {
        if self.save_blocked {
            return fallback;
        }
        let Some(record) = self.loaded.as_ref().map(|data| data.locator) else {
            return fallback;
        };
        native_position(record)
    }

    pub(super) fn initial_rotation(&self) -> Option<Rotation> {
        self.loaded
            .as_ref()
            .map(|data| data.locator)
            .filter(|_| !self.save_blocked)
            .map(native_rotation)
    }

    pub(super) fn dimension(&self) -> Option<crate::dimension::Dimension> {
        if self.save_blocked {
            return None;
        }
        self.loaded.as_ref().map(|data| match data.locator.dimension {
            lodestone_storage_schema::BuiltinDimension::Overworld => {
                crate::dimension::Dimension::Overworld
            }
            lodestone_storage_schema::BuiltinDimension::Nether => {
                crate::dimension::Dimension::Nether
            }
            lodestone_storage_schema::BuiltinDimension::End => crate::dimension::Dimension::End,
            lodestone_storage_schema::BuiltinDimension::Unspecified => {
                unreachable!("native player decoder rejects unspecified dimensions")
            }
        })
    }

    pub(super) fn block_restore(&mut self, reason: &str) {
        tracing::warn!(
            "native player locator for {:?} cannot restore its dimension ({reason}); it will not be overwritten this session",
            self.uuid
        );
        self.save_blocked = true;
    }

    pub(super) fn restored_source(
        &mut self,
        home: &dyn ChunkSource,
        protocol_supports_dimension: bool,
    ) -> Option<Arc<dyn ChunkSource>> {
        let dimension = self.dimension()?;
        if dimension == home.dimension().unwrap_or(crate::dimension::Dimension::Overworld) {
            return None;
        }
        if !protocol_supports_dimension {
            self.block_restore("the selected protocol cannot encode the saved dimension");
            return None;
        }
        match home.sibling(dimension) {
            Some(sibling) => Some(sibling),
            None => {
                self.block_restore("the world has no source for the saved dimension");
                None
            }
        }
    }

    pub(super) fn game_mode(&self) -> Option<GameMode> {
        self.loaded.as_ref().and_then(|data| data.game_mode)
    }

    pub(super) fn runtime(&self) -> Option<crate::world_storage::NativePlayerRuntimeState> {
        self.loaded
            .as_ref()
            .filter(|_| !self.save_blocked)
            .and_then(|data| data.runtime)
    }

    pub(super) fn inventory(&self) -> Option<crate::inventory::PlayerInventory> {
        self.loaded
            .as_ref()
            .filter(|_| !self.save_blocked)
            .and_then(|data| data.inventory.clone())
    }

    pub(super) fn snapshot(
        &self,
        player_pos: Option<(f64, f64, f64)>,
        player_rot: Option<Rotation>,
        fallback: Vec3,
        dimension: crate::dimension::Dimension,
        game_mode: GameMode,
        vitals: &PlayerVitals,
        experience: &crate::experience::PlayerExperience,
        inventory: &PlayerInventory,
    ) -> Option<crate::world_storage::NativePlayerData> {
        if self.save_blocked {
            return None;
        }
        let position = player_pos
            .map(|(x, y, z)| Vec3::new(x, y, z))
            .or_else(|| {
                self.loaded
                    .as_ref()
                    .map(|data| data.locator)
                    .filter(|_| !self.save_blocked)
                    .map(native_position)
            })
            .unwrap_or(fallback);
        let rotation = player_rot
            .or_else(|| {
                self.loaded
                    .as_ref()
                    .map(|data| data.locator)
                    .filter(|_| !self.save_blocked)
                    .map(native_rotation)
            })
            .unwrap_or_default();
        Some(crate::world_storage::NativePlayerData {
            locator: crate::world_storage::NativePlayerRecord {
                uuid: self.uuid,
                dimension: native_dimension(dimension),
                x_fixed: native_fixed_coordinate(position.x)?,
                y_fixed: native_fixed_coordinate(position.y)?,
                z_fixed: native_fixed_coordinate(position.z)?,
                yaw_millidegrees: native_fixed_rotation(rotation.yaw)?,
                pitch_millidegrees: native_fixed_rotation(rotation.pitch)?,
            },
            game_mode: Some(game_mode),
            runtime: Some(crate::world_storage::NativePlayerRuntimeState {
                health: vitals.health(),
                air_supply: vitals.air_supply(),
                experience: *experience,
            }),
            inventory: Some(inventory.clone()),
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn native_dimension(
    dimension: crate::dimension::Dimension,
) -> lodestone_storage_schema::BuiltinDimension {
    match dimension {
        crate::dimension::Dimension::Overworld => lodestone_storage_schema::BuiltinDimension::Overworld,
        crate::dimension::Dimension::Nether => lodestone_storage_schema::BuiltinDimension::Nether,
        crate::dimension::Dimension::End => lodestone_storage_schema::BuiltinDimension::End,
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn native_position(record: crate::world_storage::NativePlayerRecord) -> Vec3 {
    let units = crate::anvil_player_storage::POSITION_UNITS_PER_BLOCK;
    Vec3::new(
        f64::from(record.x_fixed) / units,
        f64::from(record.y_fixed) / units,
        f64::from(record.z_fixed) / units,
    )
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn native_rotation(record: crate::world_storage::NativePlayerRecord) -> Rotation {
    Rotation::new(
        record.yaw_millidegrees as f32 / 1_000.0,
        record.pitch_millidegrees as f32 / 1_000.0,
    )
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn native_fixed_coordinate(value: f64) -> Option<i32> {
    native_fixed(value, crate::anvil_player_storage::POSITION_UNITS_PER_BLOCK)
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn native_fixed_rotation(value: f32) -> Option<i32> {
    native_fixed(f64::from(value), 1_000.0)
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn native_fixed(value: f64, scale: f64) -> Option<i32> {
    let scaled = value * scale;
    if !scaled.is_finite()
        || scaled.round() < f64::from(i32::MIN)
        || scaled.round() > f64::from(i32::MAX)
    {
        return None;
    }
    Some(scaled.round() as i32)
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn persist_native_player(
    session: Option<&NativePlayerSession>,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    fallback: Vec3,
    dimension: crate::dimension::Dimension,
    game_mode: GameMode,
    vitals: &PlayerVitals,
    experience: &crate::experience::PlayerExperience,
    inventory: &PlayerInventory,
) {
    let Some(session) = session else {
        return;
    };
    let Some(record) =
        session.snapshot(
            player_pos, player_rot, fallback, dimension, game_mode, vitals, experience, inventory,
        )
    else {
        tracing::error!(
            "native player locator for {:?} contains a non-finite or out-of-range live value; it was not written",
            session.uuid
        );
        return;
    };
    if let Err(error) = session.storage.write_dirty_player_data(record) {
        tracing::warn!("could not save native player data for {:?}: {error}", session.uuid);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn publish_native_player(
    session: Option<&NativePlayerSession>,
    live_save: &crate::live_save::LiveSaveSlot,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    fallback: Vec3,
    dimension: crate::dimension::Dimension,
    game_mode: GameMode,
    vitals: &PlayerVitals,
    experience: &crate::experience::PlayerExperience,
    inventory: &PlayerInventory,
) {
    let Some(session) = session else {
        return;
    };
    let Some(record) =
        session.snapshot(
            player_pos, player_rot, fallback, dimension, game_mode, vitals, experience, inventory,
        )
    else {
        return;
    };
    live_save.publish_native(Some(session.storage.clone()), record);
}

/// Writes this connection's live state to its `.dat` file.
///
/// # Why the position is an `Option`
///
/// `player_pos` is `None` until the client sends its first movement packet, and
/// this really does happen: a client that joins and closes the window
/// immediately never sends one. Persisting a `(0, 0, 0)` in that case would
/// teleport the player into the void on their next join, so `fallback` — the
/// position they joined at — is written instead. Neither value is a guess: both
/// are positions the server itself placed them at.
///
/// A `None` store is a world with nothing to save into, and this is then a no-op
/// rather than an error; that is the in-memory and browser case.
///
/// Blocking (a gzip encode plus two renames, a few hundred bytes) and called
/// from the connection task. Deliberately *not* `spawn_blocking`: it is orders of
/// magnitude smaller than a region write, and the call at the disconnect return
/// has to complete before the task ends — handing it to a pool there would race
/// the runtime shutting the pool down, which loses exactly the save that matters
/// most.
/// Where a returning player should re-enter the world, given their saved
/// state (if any, `saved`) and the world spawn as the fallback (`world_spawn`).
///
/// **Respawn uses a saved position only for the matching dimension.** A saved position
/// is trusted verbatim only when [`PlayerData::dimension`](crate::player_data::PlayerData)
/// identifies the overworld, the dimension used by a fresh connection before
/// any portal trip. Positions captured in another dimension fall back to the
/// world spawn instead of being interpreted in overworld coordinates.
///
/// [`crate::dimension::Dimension::from_key`] returning `None` — an
/// unparseable tag — degrades the same way a genuinely non-overworld tag
/// does: fall back to the world spawn rather than trust a position whose
/// dimension is not actually known. An ambiguous saved position cannot be
/// recovered; falling back to the world spawn avoids trusting a coordinate
/// whose dimension is unknown.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn join_position_for_saved_player(
    saved: Option<&crate::player_data::PlayerData>,
    world_spawn: Vec3,
) -> Vec3 {
    saved.map_or(world_spawn, |data| {
        if crate::dimension::Dimension::from_key(&data.dimension)
            == Some(crate::dimension::Dimension::Overworld)
        {
            data.spawn_state().pos
        } else {
            world_spawn
        }
    })
}

/// Builds the [`PlayerData`](crate::player_data::PlayerData) snapshot
/// [`persist_player`] would write and [`live_publish_player`] would mirror,
/// without doing either — the construction half, factored out so both the
/// two deliberate disk-write call sites and `serve_play`'s per-iteration
/// live-publish (see [`crate::live_save::LiveSaveSlot`]) build the
/// identical snapshot from the identical arguments rather than risking two
/// copies drifting.
///
/// See [`persist_player`]'s own doc comment for why `player_pos` is an
/// `Option` and `fallback` exists.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub(super) fn player_save_snapshot(
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    fallback: Vec3,
    vitals: &PlayerVitals,
    game_mode: GameMode,
    inventory: &PlayerInventory,
    experience: &crate::experience::PlayerExperience,
    preserved: &[(String, lodestone_core::Nbt)],
    // The dimension `player_pos`/`fallback` are expressed in — the caller's
    // own current `SourceRef::dimension()`, not always the overworld. See
    // `PlayerData::capture`'s own doc comment for why this is load-bearing
    // rather than a label: a Nether-relative position with the wrong (or no)
    // dimension tag must not be interpreted in overworld coordinates.
    dimension: crate::dimension::Dimension,
) -> crate::player_data::PlayerData {
    let pos = player_pos.map_or(fallback, |(x, y, z)| Vec3::new(x, y, z));
    crate::player_data::PlayerData::capture(
        pos,
        player_rot.unwrap_or(Rotation::new(0.0, 0.0)),
        vitals.health(),
        vitals.air_supply(),
        game_mode,
        inventory,
        *experience,
        preserved.to_vec(),
        dimension,
    )
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub(super) fn persist_player(
    store: Option<&crate::player_data::PlayerDataStore>,
    uuid: uuid::Uuid,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    fallback: Vec3,
    vitals: &PlayerVitals,
    game_mode: GameMode,
    inventory: &PlayerInventory,
    // The live level/bar/total. Saved *and* restored, which has to be one change:
    // modelling the three `Xp*` fields without reading them back would write this
    // session's zeroes over the file's real XP on the first save, which is strictly
    // worse than the bug it replaces.
    experience: &crate::experience::PlayerExperience,
    preserved: &[(String, lodestone_core::Nbt)],
    dimension: crate::dimension::Dimension,
) {
    let Some(store) = store else {
        return;
    };
    let data = player_save_snapshot(
        player_pos, player_rot, fallback, vitals, game_mode, inventory, experience, preserved,
        dimension,
    );
    if let Err(err) = store.write(uuid, &data) {
        tracing::warn!("could not save player data for {uuid}: {err}");
    }
}

/// Refreshes `live_save` with the current live state — see
/// [`crate::live_save::LiveSaveSlot`]'s own doc comment. Cheap and
/// in-memory only (no disk I/O), unlike [`persist_player`]: `serve_play`
/// calls this once per iteration of its own `select!` loop, not only at the
/// two deliberate save points.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub(super) fn live_publish_player(
    live_save: &crate::live_save::LiveSaveSlot,
    store: Option<&crate::player_data::PlayerDataStore>,
    uuid: uuid::Uuid,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    fallback: Vec3,
    vitals: &PlayerVitals,
    game_mode: GameMode,
    inventory: &PlayerInventory,
    experience: &crate::experience::PlayerExperience,
    preserved: &[(String, lodestone_core::Nbt)],
    dimension: crate::dimension::Dimension,
) {
    let data = player_save_snapshot(
        player_pos, player_rot, fallback, vitals, game_mode, inventory, experience, preserved,
        dimension,
    );
    live_save.publish(store.cloned(), uuid, data);
}

#[cfg(test)]
mod tests;
