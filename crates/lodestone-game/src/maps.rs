//! Filled-map contents, keyed by map id (issue #184).
//!
//! ## What it is
//!
//! The client-side mirror of vanilla's `MapItemSavedData`: for every map id the
//! server has told us about, a 128×128 grid of map-palette colour indices plus
//! the icons drawn over it. [`MapStore::apply`] folds
//! [`ClientEvent::MapItemData`] into it and nothing else.
//!
//! ## How it works
//!
//! [`ClientEvent::MapItemData`]'s colour half is a **sub-rectangle**, not a
//! frame: vanilla sends only the dirty columns, so a walking player produces a
//! 1-or-2-column-wide, 128-tall patch. [`MapState::apply_patch`] blits it at
//! `start_x`/`start_y` and leaves everything else alone, which is why the store
//! keeps the full grid rather than the last patch.
//!
//! Both halves of the packet are independently optional and `None` means
//! *unchanged*. Clearing the icons is `Some(vec![])`, so the fold must not treat
//! an absent list as an empty one — a player marker would flicker off on every
//! pixel-only update.
//!
//! ## How to change it
//!
//! Colour indices are raw vanilla `MapColor` bytes (`index * 4 + shade`); this
//! crate deliberately does not resolve them to RGB, because the palette is
//! presentation and belongs to the renderer. If you need a lookup, put it beside
//! the drawing code.

use std::{collections::BTreeMap, sync::Arc};

use lodestone_model::event::{ClientEvent, MapDecoration, MapPatch};

/// Side length of a map's colour grid, vanilla's `MapItemSavedData` 128×128.
pub const MAP_SIZE: usize = 128;

/// One map's contents.
#[derive(Debug, Clone, PartialEq)]
pub struct MapState {
    /// Zoom level 0–4.
    pub scale: i8,
    /// Locked by a cartography table.
    pub locked: bool,
    /// `MAP_SIZE * MAP_SIZE` raw map-palette colour bytes, row-major. `0` is
    /// vanilla's transparent/unexplored entry.
    pub colors: Arc<Vec<u8>>,
    /// Icons, in the order the server sent them.
    pub decorations: Vec<MapDecoration>,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            scale: 0,
            locked: false,
            colors: Arc::new(vec![0; MAP_SIZE * MAP_SIZE]),
            decorations: Vec::new(),
        }
    }
}

impl MapState {
    /// Blit one patch into the grid. Out-of-range pixels are dropped rather than
    /// wrapping: the width/height/start bytes are unsigned wire values and a
    /// malformed one must not corrupt a neighbouring row.
    pub fn apply_patch(&mut self, patch: &MapPatch) {
        let width = usize::from(patch.width);
        let height = usize::from(patch.height);
        let colors = Arc::make_mut(&mut self.colors);
        for row in 0..height {
            let y = usize::from(patch.start_y) + row;
            if y >= MAP_SIZE {
                break;
            }
            for column in 0..width {
                let x = usize::from(patch.start_x) + column;
                if x >= MAP_SIZE {
                    break;
                }
                if let Some(color) = patch.colors.get(column + row * width) {
                    colors[x + y * MAP_SIZE] = *color;
                }
            }
        }
    }

    /// The colour byte at a pixel, or `0` outside the grid.
    #[must_use]
    pub fn color_at(&self, x: usize, y: usize) -> u8 {
        if x >= MAP_SIZE || y >= MAP_SIZE {
            return 0;
        }
        self.colors[x + y * MAP_SIZE]
    }
}

/// Every map the server has sent contents for, by map id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapStore {
    maps: Arc<BTreeMap<i32, MapState>>,
}

impl MapStore {
    /// Fold one event. Returns `true` when this store changed.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        let ClientEvent::MapItemData {
            map_id,
            scale,
            locked,
            decorations,
            color_patch,
        } = event
        else {
            return false;
        };
        let state = Arc::make_mut(&mut self.maps).entry(*map_id).or_default();
        state.scale = *scale;
        state.locked = *locked;
        if let Some(decorations) = decorations {
            state.decorations.clear();
            state.decorations.extend(decorations.iter().cloned());
        }
        if let Some(patch) = color_patch {
            state.apply_patch(patch);
        }
        true
    }

    /// One map's contents, if the server has sent any.
    #[must_use]
    pub fn get(&self, map_id: i32) -> Option<&MapState> {
        self.maps.get(&map_id)
    }

    /// How many maps are known.
    #[must_use]
    pub fn len(&self) -> usize {
        self.maps.len()
    }

    /// Whether no map contents have arrived.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.maps.is_empty()
    }

    /// Every known map id, ascending.
    pub fn ids(&self) -> impl Iterator<Item = i32> + '_ {
        self.maps.keys().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::ids::Identifier;
    use std::sync::Arc;

    fn patch(start_x: u8, start_y: u8, width: u8, height: u8, fill: u8) -> MapPatch {
        MapPatch {
            start_x,
            start_y,
            width,
            height,
            colors: vec![fill; usize::from(width) * usize::from(height)],
        }
    }

    fn event(map_id: i32, decorations: Option<Vec<MapDecoration>>, color_patch: Option<MapPatch>) -> ClientEvent {
        ClientEvent::MapItemData {
            map_id,
            scale: 2,
            locked: false,
            decorations,
            color_patch,
        }
    }

    /// A patch lands at its offset and touches nothing outside it — the whole
    /// point of keeping the grid rather than the last patch.
    #[test]
    fn a_patch_blits_at_its_offset_only() {
        let mut store = MapStore::default();
        assert!(store.apply(&event(7, None, Some(patch(10, 20, 3, 2, 44)))));
        let map = store.get(7).expect("map 7 was sent");
        assert_eq!(map.color_at(10, 20), 44);
        assert_eq!(map.color_at(12, 21), 44);
        assert_eq!(map.color_at(13, 21), 0, "one column past the patch");
        assert_eq!(map.color_at(10, 22), 0, "one row past the patch");
        assert_eq!(map.scale, 2);

        // A second, disjoint patch must not clear the first.
        store.apply(&event(7, None, Some(patch(0, 0, 1, 1, 9))));
        let map = store.get(7).expect("map 7 is still there");
        assert_eq!(map.color_at(0, 0), 9);
        assert_eq!(map.color_at(10, 20), 44, "the earlier patch survived");
    }

    /// `None` decorations mean unchanged; `Some(vec![])` clears. Getting these
    /// two the same way round makes every player marker flicker.
    #[test]
    fn absent_decorations_are_not_an_empty_list() {
        let marker = MapDecoration {
            kind: "minecraft:player".parse::<Identifier>().unwrap(),
            x: 4,
            y: -8,
            rotation: 3,
            name: None,
        };
        let mut store = MapStore::default();
        store.apply(&event(1, Some(vec![marker.clone()]), None));
        store.apply(&event(1, None, Some(patch(0, 0, 2, 2, 1))));
        assert_eq!(
            store.get(1).unwrap().decorations,
            vec![marker],
            "a pixel-only update must leave the icons alone"
        );
        store.apply(&event(1, Some(Vec::new()), None));
        assert!(store.get(1).unwrap().decorations.is_empty());
    }

    #[test]
    fn cloning_a_store_shares_unchanged_map_storage() {
        let mut store = MapStore::default();
        store.apply(&event(7, None, Some(patch(0, 0, 1, 1, 9))));
        let snapshot = store.clone();

        assert!(Arc::ptr_eq(&store.maps, &snapshot.maps));
        assert!(Arc::ptr_eq(
            &store.get(7).unwrap().colors,
            &snapshot.get(7).unwrap().colors,
        ));
    }

    #[test]
    fn a_patch_copies_only_storage_observed_by_an_older_snapshot() {
        let mut store = MapStore::default();
        store.apply(&event(7, None, Some(patch(0, 0, 1, 1, 9))));
        let snapshot = store.clone();

        store.apply(&event(7, None, Some(patch(0, 0, 1, 1, 44))));

        assert_eq!(snapshot.get(7).unwrap().color_at(0, 0), 9);
        assert_eq!(store.get(7).unwrap().color_at(0, 0), 44);
        assert!(!Arc::ptr_eq(&store.maps, &snapshot.maps));
        assert!(!Arc::ptr_eq(
            &store.get(7).unwrap().colors,
            &snapshot.get(7).unwrap().colors,
        ));
    }
}
