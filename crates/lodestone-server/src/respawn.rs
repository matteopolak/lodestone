//! Per-player respawn: which block a player respawns at, in which dimension,
//! and how that choice is saved.
//!
//! # What it is
//!
//! A player's respawn point is a block in a dimension ([`RespawnPoint`]). At
//! respawn, [`plan`] re-reads that block in its own dimension and answers where
//! the player stands, which dimension that is, whether an anchor charge was
//! spent, and whether the point turned out to be unusable. Death and leaving the
//! End both go through it.
//!
//! # How it works
//!
//! * The block decides the rule, not a stored kind. A respawn anchor with a
//!   charge (or a forced point) in a dimension where anchors work stands the
//!   player beside it ([`crate::respawn_anchor::find_stand_up`]) and, for a
//!   death respawn, spends one charge. A bed in the overworld stands the player
//!   beside it ([`crate::world_spawn::resolve_bed_respawn`]). Any other block
//!   works only for a forced point, which respawns one tenth of a block above the
//!   position if both its cell and the cell above are passable.
//! * A point whose block fails those rules is `missing`: the player goes to the
//!   world spawn, the point is cleared and the caller sends the client its
//!   no-respawn-block event.
//! * The point lives in a dimension, so the plan carries a [`Route`] telling the
//!   caller whether that is the home dimension, the one the player is already in,
//!   or a sibling that must be travelled to. A point in a dimension this world
//!   does not host, or one a protocol cannot travel to, falls back to the world
//!   spawn silently.
//! * The save format is the vanilla `respawn` compound (`pos` int array,
//!   `dimension`, `yaw`, `pitch`, optional `forced`), kept in the player's
//!   preserved fields so it survives reconnects and restarts ([`store`] and
//!   [`load`]).
//!
//! # How to change it
//!
//! Block rules live in [`resolve_in`]. A new dimension that allows beds or
//! anchors changes [`bed_works_in`] or [`crate::respawn_anchor::works_in`].
//!
//! # Not modelled
//!
//! The stand-up facing (towards the anchor or bed) is not sent, and the world
//! border is not consulted when choosing a stand-up cell.

use std::sync::Arc;

use lodestone_core::Nbt;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, Vec3};

use crate::chunk::ChunkSource;
use crate::dimension::Dimension;
use crate::respawn_anchor::{self, AnchorRespawn};
use crate::world_spawn::{self, RespawnPoint};

/// The player-data key the point is saved under.
pub(crate) const RESPAWN_FIELD: &str = "respawn";

/// Where a respawn lands relative to the connection's current view.
#[derive(Clone)]
pub(crate) enum Route {
    /// The home dimension the connection joined in.
    Home,
    /// The dimension the connection is already viewing, when that is not home.
    Current,
    /// Another dimension, reached through the world's sibling lookup.
    Sibling(Arc<dyn ChunkSource>),
}

impl PartialEq for Route {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Home, Self::Home) | (Self::Current, Self::Current) => true,
            (Self::Sibling(a), Self::Sibling(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl std::fmt::Debug for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Home => f.write_str("Home"),
            Self::Current => f.write_str("Current"),
            Self::Sibling(_) => f.write_str("Sibling"),
        }
    }
}

/// The outcome of resolving a respawn.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    /// Where the player's feet go.
    pub feet: Vec3,
    /// The dimension they land in.
    pub dimension: Dimension,
    /// How to reach it.
    pub route: Route,
    /// The stored point existed but could not be used.
    pub missing_block: bool,
    /// The anchor the respawn used, if any.
    pub anchor: Option<BlockPos>,
    /// The block change from a spent anchor charge: position, old state, new state.
    pub spent: Option<(BlockPos, StateId, StateId)>,
}

/// Whether a bed sets the respawn point in `dimension`.
#[must_use]
pub(crate) fn bed_works_in(dimension: Dimension) -> bool {
    dimension == Dimension::Overworld
}

/// What a point's block resolved to.
enum Found {
    Bed(Vec3),
    Anchor(AnchorRespawn),
    Forced(Vec3),
}

/// Whether a respawn may stand inside this state: nothing solid and no fluid,
/// except pressure plates, signs and banners, which a player may stand in.
fn passable_for_respawn(state: StateId) -> bool {
    let name = state.block().name();
    if name.ends_with("_pressure_plate")
        || name.ends_with("_sign")
        || name.ends_with("_wall_sign")
        || name.ends_with("_banner")
        || name.ends_with("_wall_banner")
    {
        return true;
    }
    crate::fluid::fluid_state_of_id(state).is_none()
        && lodestone_data::collision_shapes::collision_boxes(state).is_empty()
}

/// Applies the block rules at `point` in `source`, which must be that point's
/// dimension. `consume` spends an anchor charge when the point is not forced.
fn resolve_in<S: ChunkSource + ?Sized>(source: &S, point: &RespawnPoint, consume: bool) -> Option<Found> {
    let pos = point.pos;
    let state = source.block_state_id(pos.x, pos.y, pos.z);
    if respawn_anchor::is_anchor(state)
        && (point.forced || respawn_anchor::charges(state) > 0)
        && respawn_anchor::works_in(point.dimension)
    {
        return respawn_anchor::respawn_at(source, pos, consume && !point.forced).map(Found::Anchor);
    }
    if world_spawn::is_bed_block(state) && bed_works_in(point.dimension) {
        return world_spawn::resolve_bed_respawn(source, pos).map(Found::Bed);
    }
    if !point.forced {
        return None;
    }
    let above = source.block_state_id(pos.x, pos.y + 1, pos.z);
    (passable_for_respawn(state) && passable_for_respawn(above)).then(|| {
        Found::Forced(Vec3::new(f64::from(pos.x) + 0.5, f64::from(pos.y) + 0.1, f64::from(pos.z) + 0.5))
    })
}

/// Resolves a respawn.
///
/// `home` is the dimension the connection joined in and `current` the one it is
/// viewing. `consume` is set for a death respawn, which spends an anchor charge;
/// leaving the End keeps the player's data and spends nothing. `can_travel` is
/// whether the protocol can send a dimension change: without it only the home
/// dimension is reachable.
pub(crate) fn plan(
    home: &dyn ChunkSource,
    current: &dyn ChunkSource,
    point: Option<RespawnPoint>,
    world_spawn: Vec3,
    consume: bool,
    can_travel: bool,
) -> Plan {
    let home_dimension = home.dimension().unwrap_or(Dimension::Overworld);
    let current_dimension = current.dimension().unwrap_or(Dimension::Overworld);
    let world = |missing_block: bool| Plan {
        feet: world_spawn,
        dimension: home_dimension,
        route: Route::Home,
        missing_block,
        anchor: None,
        spent: None,
    };
    let Some(point) = point else {
        return world(false);
    };
    let (found, route) = if point.dimension == home_dimension {
        (resolve_in(home, &point, consume), Route::Home)
    } else if !can_travel {
        return world(false);
    } else if point.dimension == current_dimension {
        (resolve_in(current, &point, consume), Route::Current)
    } else {
        let Some(sibling) = home.sibling(point.dimension) else {
            return world(false);
        };
        (resolve_in(sibling.as_ref(), &point, consume), Route::Sibling(sibling))
    };
    match found {
        None => world(true),
        Some(Found::Bed(feet) | Found::Forced(feet)) => Plan {
            feet,
            dimension: point.dimension,
            route,
            missing_block: false,
            anchor: None,
            spent: None,
        },
        Some(Found::Anchor(anchor)) => Plan {
            feet: anchor.feet,
            dimension: point.dimension,
            route,
            missing_block: false,
            anchor: Some(anchor.anchor),
            spent: (anchor.before != anchor.after).then_some((anchor.anchor, anchor.before, anchor.after)),
        },
    }
}

/// The save form of a respawn point.
#[must_use]
pub(crate) fn to_nbt(point: &RespawnPoint) -> Nbt {
    let mut fields = vec![
        ("pos".to_owned(), Nbt::IntArray(vec![point.pos.x, point.pos.y, point.pos.z])),
        ("dimension".to_owned(), Nbt::String(point.dimension.key().to_owned())),
        ("yaw".to_owned(), Nbt::Float(point.yaw)),
        ("pitch".to_owned(), Nbt::Float(point.pitch)),
    ];
    if point.forced {
        fields.push(("forced".to_owned(), Nbt::Byte(1)));
    }
    Nbt::Compound(fields)
}

/// Reads a point back from its save form. `None` when malformed or in a
/// dimension this server does not host.
#[must_use]
pub(crate) fn from_nbt(nbt: &Nbt) -> Option<RespawnPoint> {
    let Nbt::Compound(fields) = nbt else {
        return None;
    };
    let get = |key: &str| fields.iter().find(|(name, _)| name == key).map(|(_, value)| value);
    let Some(Nbt::IntArray(pos)) = get("pos") else {
        return None;
    };
    let [x, y, z] = pos.as_slice() else {
        return None;
    };
    let Some(Nbt::String(dimension)) = get("dimension") else {
        return None;
    };
    let float = |key: &str| match get(key) {
        Some(Nbt::Float(value)) => *value,
        _ => 0.0,
    };
    let forced = matches!(get("forced"), Some(Nbt::Byte(flag)) if *flag != 0);
    Some(RespawnPoint {
        pos: BlockPos::new(*x, *y, *z),
        dimension: Dimension::from_key(dimension)?,
        yaw: float("yaw"),
        pitch: float("pitch"),
        forced,
    })
}

/// The saved point among a player's preserved fields.
#[must_use]
pub(crate) fn load(preserved: &[(String, Nbt)]) -> Option<RespawnPoint> {
    preserved.iter().find(|(key, _)| key == RESPAWN_FIELD).and_then(|(_, value)| from_nbt(value))
}

/// Writes `point` into the preserved fields, replacing any earlier one and
/// removing the field for `None`. Returns whether anything changed.
pub(crate) fn store(preserved: &mut Vec<(String, Nbt)>, point: Option<&RespawnPoint>) -> bool {
    let wanted = point.map(to_nbt);
    let current = preserved.iter().find(|(key, _)| key == RESPAWN_FIELD).map(|(_, value)| value);
    if current == wanted.as_ref() {
        return false;
    }
    preserved.retain(|(key, _)| key != RESPAWN_FIELD);
    if let Some(value) = wanted {
        preserved.push((RESPAWN_FIELD.to_owned(), value));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(forced: bool) -> RespawnPoint {
        RespawnPoint {
            pos: BlockPos::new(-12, 70, 345),
            dimension: Dimension::Nether,
            yaw: -90.5,
            pitch: 12.25,
            forced,
        }
    }

    fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
        let Nbt::Compound(fields) = nbt else { return None };
        fields.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }

    /// The field names and tag types are the ones the reference game's own
    /// save codec declares for a respawn point: a position int array, a
    /// dimension key string, float yaw and pitch, and a `forced` flag that is
    /// only written when set.
    #[test]
    fn the_saved_form_has_the_reference_field_names_and_types() {
        let saved = to_nbt(&point(false));
        assert_eq!(field(&saved, "pos"), Some(&Nbt::IntArray(vec![-12, 70, 345])));
        assert_eq!(field(&saved, "dimension"), Some(&Nbt::String("minecraft:the_nether".to_owned())));
        assert_eq!(field(&saved, "yaw"), Some(&Nbt::Float(-90.5)));
        assert_eq!(field(&saved, "pitch"), Some(&Nbt::Float(12.25)));
        assert_eq!(field(&saved, "forced"), None, "an unset flag is omitted");
        assert_eq!(field(&to_nbt(&point(true)), "forced"), Some(&Nbt::Byte(1)));
    }

    #[test]
    fn a_point_round_trips_through_its_saved_form() {
        for forced in [false, true] {
            for dimension in [Dimension::Overworld, Dimension::Nether, Dimension::End] {
                let original = RespawnPoint { dimension, ..point(forced) };
                assert_eq!(from_nbt(&to_nbt(&original)), Some(original));
            }
        }
    }

    /// A record this server cannot place is dropped rather than guessed at.
    #[test]
    fn malformed_or_foreign_records_load_as_nothing() {
        let good = to_nbt(&point(false));
        let without = |key: &str| {
            let Nbt::Compound(fields) = &good else { unreachable!() };
            Nbt::Compound(fields.iter().filter(|(name, _)| name != key).cloned().collect())
        };
        assert_eq!(from_nbt(&without("pos")), None);
        assert_eq!(from_nbt(&without("dimension")), None);
        let mut short = good.clone();
        if let Nbt::Compound(fields) = &mut short {
            fields.iter_mut().for_each(|(name, value)| {
                if name == "pos" {
                    *value = Nbt::IntArray(vec![1, 2]);
                }
            });
        }
        assert_eq!(from_nbt(&short), None, "a position needs three components");
        let mut foreign = good;
        if let Nbt::Compound(fields) = &mut foreign {
            fields.iter_mut().for_each(|(name, value)| {
                if name == "dimension" {
                    *value = Nbt::String("example:mining".to_owned());
                }
            });
        }
        assert_eq!(from_nbt(&foreign), None, "a dimension this server does not host");
        assert_eq!(from_nbt(&Nbt::Byte(1)), None);
    }

    #[test]
    fn store_replaces_removes_and_reports_changes() {
        let mut preserved = vec![("seenCredits".to_owned(), Nbt::Byte(1))];
        assert!(!store(&mut preserved, None), "nothing to remove");
        assert!(store(&mut preserved, Some(&point(false))));
        assert!(!store(&mut preserved, Some(&point(false))), "unchanged");
        assert_eq!(load(&preserved), Some(point(false)));
        assert!(store(&mut preserved, Some(&point(true))), "a changed flag is a change");
        assert_eq!(load(&preserved), Some(point(true)));
        assert_eq!(preserved.iter().filter(|(key, _)| key == RESPAWN_FIELD).count(), 1);
        assert!(store(&mut preserved, None));
        assert_eq!(load(&preserved), None);
        assert_eq!(preserved, vec![("seenCredits".to_owned(), Nbt::Byte(1))], "neighbours are untouched");
    }

    /// The point survives the player file itself: written through the real
    /// store, read back by a second store over the same directory (a restart),
    /// and loaded from the preserved fields.
    #[test]
    fn a_point_survives_a_save_and_a_reopened_store() {
        use crate::player_data::{PlayerData, PlayerDataStore};
        let dir = tempfile::tempdir().expect("scratch directory");
        let uuid = uuid::Uuid::from_u128(7);
        let mut preserved = vec![("seenCredits".to_owned(), Nbt::Byte(1))];
        store(&mut preserved, Some(&point(true)));
        let data = PlayerData::capture(
            Vec3::new(1.0, 64.0, 2.0),
            lodestone_model::Rotation::new(0.0, 0.0),
            20.0,
            300,
            lodestone_model::GameMode::Survival,
            &crate::inventory::PlayerInventory::new(),
            crate::experience::PlayerExperience::default(),
            preserved,
            Dimension::Overworld,
        );
        PlayerDataStore::new(dir.path()).unwrap().write(uuid, &data).expect("saved");
        let reopened = PlayerDataStore::new(dir.path()).unwrap();
        let loaded = reopened.read(uuid).expect("readable").expect("present");
        assert_eq!(load(&loaded.preserved), Some(point(true)));
        assert!(
            loaded.preserved.iter().any(|(key, _)| key == "seenCredits"),
            "the credits flag rides alongside it"
        );
    }

    /// A world with one source for each dimension, for the routing tests.
    struct Floor {
        dimension: Dimension,
        siblings: Vec<Arc<Floor>>,
        blocks: std::sync::Mutex<std::collections::HashMap<(i32, i32, i32), StateId>>,
    }

    impl Floor {
        fn new(dimension: Dimension) -> Arc<Self> {
            Arc::new(Self {
                dimension,
                siblings: Vec::new(),
                blocks: std::sync::Mutex::new(std::collections::HashMap::new()),
            })
        }

        fn put(&self, x: i32, y: i32, z: i32, name: &str) {
            let state = StateId::from_state_str(name).expect("state");
            self.blocks.lock().unwrap().insert((x, y, z), state);
        }
    }

    impl ChunkSource for Floor {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            crate::chunk::ChunkColumn::new(0, 256)
        }
        fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
            self.blocks.lock().unwrap().get(&(x, y, z)).copied().unwrap_or_else(crate::chunk::air_state)
        }
        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:plains".to_owned()
        }
        fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
            self.blocks.lock().unwrap().insert((x, y, z), state);
        }
        fn dimension(&self) -> Option<Dimension> {
            Some(self.dimension)
        }
        fn sibling(&self, dimension: Dimension) -> Option<Arc<dyn ChunkSource>> {
            self.siblings
                .iter()
                .find(|other| other.dimension == dimension)
                .map(|other| Arc::clone(other) as Arc<dyn ChunkSource>)
        }
    }

    /// Beds work only in the overworld: the same bed block in the Nether is not
    /// a respawn block, so the point is missing there.
    #[test]
    fn a_bed_in_the_nether_is_not_a_respawn_block() {
        let nether = Floor::new(Dimension::Nether);
        for x in -2..=2 {
            for z in -2..=2 {
                nether.put(x, 63, z, "minecraft:stone");
            }
        }
        nether.put(0, 64, 0, "minecraft:red_bed[facing=north,part=foot]");
        let at = RespawnPoint::block(BlockPos::new(0, 64, 0), Dimension::Nether, 0.0);
        let spawn = Vec3::new(9.0, 70.0, 9.0);
        let planned = plan(&*nether, &*nether, Some(at), spawn, true, true);
        assert!(planned.missing_block);
        assert_eq!(planned.feet, spawn);
        // Control: the identical layout in the overworld resolves to the bed.
        let overworld = Floor::new(Dimension::Overworld);
        for x in -2..=2 {
            for z in -2..=2 {
                overworld.put(x, 63, z, "minecraft:stone");
            }
        }
        overworld.put(0, 64, 0, "minecraft:red_bed[facing=north,part=foot]");
        let at = RespawnPoint::block(BlockPos::new(0, 64, 0), Dimension::Overworld, 0.0);
        let planned = plan(&*overworld, &*overworld, Some(at), spawn, true, true);
        assert!(!planned.missing_block);
        assert_eq!(planned.feet, Vec3::new(1.5, 64.0, 0.5));
    }

    /// A protocol that cannot change dimension still reaches the home one, and
    /// anything else is the world spawn with the point kept.
    #[test]
    fn without_dimension_changes_only_the_home_dimension_is_reachable() {
        let nether = Floor::new(Dimension::Nether);
        let home = Arc::new(Floor {
            dimension: Dimension::Overworld,
            siblings: vec![Arc::clone(&nether)],
            blocks: std::sync::Mutex::new(std::collections::HashMap::new()),
        });
        nether.put(0, 63, 0, "minecraft:stone");
        nether.put(0, 64, 0, "minecraft:respawn_anchor[charges=2]");
        let at = RespawnPoint::block(BlockPos::new(0, 64, 0), Dimension::Nether, 0.0);
        let spawn = Vec3::new(9.0, 70.0, 9.0);
        let blocked = plan(&*home, &*home, Some(at), spawn, true, false);
        assert_eq!(blocked.feet, spawn);
        assert!(!blocked.missing_block, "an unreachable dimension is not a missing block");
        assert_eq!(charges_at(&nether), 2, "and nothing is spent");
        let reachable = plan(&*home, &*home, Some(at), spawn, true, true);
        assert!(matches!(reachable.route, Route::Sibling(_)));
        assert_eq!(charges_at(&nether), 1);
    }

    fn charges_at(world: &Floor) -> u8 {
        crate::respawn_anchor::charges(world.block_state_id(0, 64, 0))
    }
}
