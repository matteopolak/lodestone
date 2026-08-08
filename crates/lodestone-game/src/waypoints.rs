//! Tracked waypoints — vanilla's locator bar (issue #26).
//!
//! ## What it is
//!
//! The client-side set of waypoints the server is tracking for us, folded from
//! `ClientboundTrackedWaypointPacket`. Vanilla draws these on the locator bar at
//! the top of the HUD.
//!
//! ## How it works
//!
//! Three operations — track, untrack, update — over a set keyed by
//! [`lodestone_model::event::WaypointId`]. Track and update are both upserts and
//! are deliberately *not* distinguished: an update for a waypoint we never saw
//! tracked is a legal consequence of joining mid-flight, and dropping it would
//! leave a permanently invisible waypoint.
//!
//! **The position is a four-way degradation, not an `Option`.** Vanilla sends
//! exact coordinates for a nearby waypoint, chunk coordinates for a distant one,
//! and a bare compass bearing past the tracking range — see
//! [`lodestone_model::event::WaypointPosition`]. A consumer that treats
//! `Azimuth` or `Empty` as "unknown, skip it" makes the locator bar go blank at
//! exactly the distance it is most useful, which is the failure this store's
//! shape exists to prevent.
//!
//! ## How to change it
//!
//! Waypoints are cleared on dimension change server-side, and the server
//! re-tracks; nothing here needs a reset hook. If one is ever needed, note that
//! [`WaypointStore::apply`] is the only mutation path.
//!
//! ## Dependencies
//!
//! [`lodestone_model::event::ClientEvent`] only.

use std::collections::BTreeMap;

use lodestone_model::event::{
    ClientEvent, TrackedWaypoint, WaypointId, WaypointOperation, WaypointPosition,
};

/// The waypoints the server is currently tracking for us.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WaypointStore {
    waypoints: BTreeMap<WaypointId, TrackedWaypoint>,
}

impl WaypointStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many waypoints are tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.waypoints.len()
    }

    /// Whether nothing is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waypoints.is_empty()
    }

    /// Every tracked waypoint.
    pub fn iter(&self) -> impl Iterator<Item = &TrackedWaypoint> {
        self.waypoints.values()
    }

    /// One waypoint by identity.
    #[must_use]
    pub fn get(&self, id: &WaypointId) -> Option<&TrackedWaypoint> {
        self.waypoints.get(id)
    }

    /// Every waypoint whose position is precise enough to place on a map, as
    /// `(waypoint, x, z)`.
    ///
    /// A convenience for the map/minimap case specifically — the locator bar
    /// wants all four position kinds and should iterate [`Self::iter`] instead.
    /// [`WaypointPosition::Azimuth`] and [`WaypointPosition::Empty`] have no
    /// coordinates at all, so they are absent here by definition rather than
    /// dropped as unknown.
    pub fn positioned(&self) -> impl Iterator<Item = (&TrackedWaypoint, i32, i32)> {
        self.waypoints.values().filter_map(|waypoint| {
            match waypoint.position {
                WaypointPosition::Exact(pos) => Some((waypoint, pos.x, pos.z)),
                // A chunk position is the chunk's centre block.
                WaypointPosition::Chunk(chunk) => {
                    Some((waypoint, chunk.x * 16 + 8, chunk.z * 16 + 8))
                }
                WaypointPosition::Empty | WaypointPosition::Azimuth(_) => None,
            }
        })
    }

    /// Folds one event, returning whether it belonged to this store.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        let ClientEvent::WaypointUpdated {
            operation,
            waypoint,
        } = event
        else {
            return false;
        };
        match operation {
            // Track and Update are both upserts on purpose; see the module doc.
            WaypointOperation::Track | WaypointOperation::Update => {
                self.waypoints
                    .insert(waypoint.id.clone(), waypoint.clone());
            }
            WaypointOperation::Untrack => {
                self.waypoints.remove(&waypoint.id);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::WaypointStore;
    use lodestone_model::event::{
        ClientEvent, TrackedWaypoint, WaypointId, WaypointOperation, WaypointPosition,
    };
    use lodestone_model::BlockPos;

    fn waypoint(name: &str, position: WaypointPosition) -> TrackedWaypoint {
        TrackedWaypoint {
            id: WaypointId::Named(name.to_owned()),
            style: "minecraft:default".parse().expect("style parses"),
            color: None,
            position,
        }
    }

    fn update(operation: WaypointOperation, waypoint: TrackedWaypoint) -> ClientEvent {
        ClientEvent::WaypointUpdated {
            operation,
            waypoint,
        }
    }

    /// An `Update` for something never tracked must still land — joining
    /// mid-flight is the ordinary case, not an error.
    #[test]
    fn an_update_for_an_untracked_waypoint_is_an_upsert() {
        let mut store = WaypointStore::new();
        assert!(store.apply(&update(
            WaypointOperation::Update,
            waypoint("a", WaypointPosition::Exact(BlockPos { x: 1, y: 2, z: 3 })),
        )));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn untrack_removes_it() {
        let mut store = WaypointStore::new();
        store.apply(&update(
            WaypointOperation::Track,
            waypoint("a", WaypointPosition::Empty),
        ));
        store.apply(&update(
            WaypointOperation::Untrack,
            waypoint("a", WaypointPosition::Empty),
        ));
        assert!(store.is_empty());
    }

    /// The degradation is four-way and the store must keep all of it. This is
    /// the failure the module doc names: `positioned` legitimately drops two
    /// kinds, and `iter` must not.
    #[test]
    fn imprecise_waypoints_are_kept_even_though_positioned_skips_them() {
        let mut store = WaypointStore::new();
        for (name, position) in [
            ("exact", WaypointPosition::Exact(BlockPos { x: 5, y: 6, z: 7 })),
            (
                "chunk",
                WaypointPosition::Chunk(lodestone_model::ChunkPos { x: 2, z: 3 }),
            ),
            ("azimuth", WaypointPosition::Azimuth(1.5)),
            ("empty", WaypointPosition::Empty),
        ] {
            store.apply(&update(WaypointOperation::Track, waypoint(name, position)));
        }
        assert_eq!(store.iter().count(), 4, "all four kinds are tracked");
        assert_eq!(
            store.positioned().count(),
            2,
            "only exact and chunk have coordinates"
        );
        // The chunk case resolves to the chunk's centre block, not its corner.
        let chunk_xz = store
            .positioned()
            .find(|(w, _, _)| w.id == WaypointId::Named("chunk".to_owned()))
            .map(|(_, x, z)| (x, z));
        assert_eq!(chunk_xz, Some((2 * 16 + 8, 3 * 16 + 8)));
    }

    #[test]
    fn an_unrelated_event_is_rejected() {
        let mut store = WaypointStore::new();
        assert!(!store.apply(&ClientEvent::KeepAlive { id: 1 }));
    }
}
