//! The server's recipe-book sync: unlocks, ghosts and property sets.
//!
//! ## What it is
//!
//! What the server has told us about *which* recipes this player knows, which
//! ghost preview is showing, and which items are valid in a given screen slot.
//! Folded from `recipe_book_add`, `recipe_book_remove`, `place_ghost_recipe` and
//! `update_recipes`.
//!
//! ## It is not the recipe corpus, and `update_recipes` is misnamed
//!
//! [`crate::recipe::RecipeBook`] holds the *recipes* — loaded from `client.jar`'s
//! datapack JSON. Nothing here duplicates that. `update_recipes` despite its name
//! carries **property sets**: the "which items are valid in this slot" lists
//! vanilla's screens grey out against (fuel, smithing template, and so on), plus
//! the stonecutter's own input→result list.
//!
//! ## 26.x replaced recipe names with per-session ids, and that matters
//!
//! A recipe is identified on the wire by `RecipeDisplayId`, **an `i32` index valid
//! only for this connection** — not by an `Identifier`. So
//! [`crate::recipe::RecipeUnlockState`], which keys on `Identifier`, cannot be fed
//! from this packet family: it was built against the pre-26 wire shape. This store
//! keys on the id the server actually sends, and a consumer that wants to join an
//! unlock to a corpus recipe does it through the display's **result item**, which
//! is why [`RecipeBookSync`] keeps those.
//!
//! ## How it works
//!
//! `recipe_book_add` carries a `replace` flag *after* its entry list — the server's
//! first-sync marker. `true` discards the known set; `false` merges. Treating every
//! packet as a merge leaves recipes from a previous datapack generation at ids the
//! new one reuses.
//!
//! The ghost is a single slot, cleared when a different container opens.
//!
//! ## How to change it
//!
//! Result items are registry ids, deliberately unresolved: mapping an id to a name
//! needs `lodestone-data`, and a store in this crate has no business reaching for
//! it when the caller already holds an item table.
//!
//! ## Dependencies
//!
//! [`lodestone_model::event::ClientEvent`] only.

use std::collections::BTreeMap;

use lodestone_model::event::{ClientEvent, RecipeBookEntry};
use lodestone_model::Identifier;

/// One recipe the server has unlocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownRecipe {
    /// Item registry ids the result slot can display.
    pub result_items: Vec<i32>,
    /// Item registry ids the recipe's station (corner icon — crafting table,
    /// furnace, etc.) can display. See
    /// [`lodestone_model::event::RecipeBookEntry::station_items`].
    pub station_items: Vec<i32>,
    /// Whether the unlock should raise a toast.
    pub notification: bool,
    /// Whether its book tab should highlight.
    pub highlight: bool,
}

/// The ghost recipe currently previewed in an open grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostRecipe {
    /// The container the ghost belongs to.
    pub window_id: i32,
    /// Item registry ids the ghost's result slot can display.
    pub result_items: Vec<i32>,
}

/// The server's recipe-book state for this session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecipeBookSync {
    known: BTreeMap<i32, KnownRecipe>,
    ghost: Option<GhostRecipe>,
    property_sets: BTreeMap<Identifier, Vec<i32>>,
    stonecutter_results: Vec<Vec<i32>>,
    /// Whether any `recipe_book_add` has arrived. Distinct from `known.is_empty()`
    /// for [`crate::recipe::RecipeUnlockState`]'s reason: an add followed by a
    /// remove empties `known` again, and without this flag a consumer could not
    /// tell "no data yet" from "genuinely nothing unlocked".
    has_data: bool,
}

impl RecipeBookSync {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The unlocked recipes, by `RecipeDisplayId`.
    #[must_use]
    pub fn known(&self) -> &BTreeMap<i32, KnownRecipe> {
        &self.known
    }

    /// Whether `display_id` is unlocked.
    #[must_use]
    pub fn is_unlocked(&self, display_id: i32) -> bool {
        self.known.contains_key(&display_id)
    }

    /// Whether any unlock data has arrived at all. `false` means a consumer must
    /// use its own fallback rather than conclude nothing is unlocked.
    #[must_use]
    pub fn has_data(&self) -> bool {
        self.has_data
    }

    /// The current ghost preview, if any.
    #[must_use]
    pub fn ghost(&self) -> Option<&GhostRecipe> {
        self.ghost.as_ref()
    }

    /// The valid-item set for a property key, e.g. `minecraft:furnace_input`.
    #[must_use]
    pub fn property_set(&self, key: &Identifier) -> Option<&[i32]> {
        self.property_sets.get(key).map(Vec::as_slice)
    }

    /// How many property sets the server sent.
    #[must_use]
    pub fn property_set_count(&self) -> usize {
        self.property_sets.len()
    }

    /// Per-stonecutter-recipe result item ids.
    #[must_use]
    pub fn stonecutter_results(&self) -> &[Vec<i32>] {
        &self.stonecutter_results
    }

    /// Every unlocked recipe whose result includes `item_id` — the join a recipe
    /// panel needs, given that a `RecipeDisplayId` carries no name.
    pub fn unlocked_producing(&self, item_id: i32) -> impl Iterator<Item = (i32, &KnownRecipe)> {
        self.known
            .iter()
            .filter(move |(_, recipe)| recipe.result_items.contains(&item_id))
            .map(|(id, recipe)| (*id, recipe))
    }

    /// Folds one event, returning whether it belonged to this store.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::RecipeBookAdded { entries, replace } => {
                if *replace {
                    self.known.clear();
                }
                for RecipeBookEntry {
                    display_id,
                    result_items,
                    station_items,
                    notification,
                    highlight,
                } in entries
                {
                    self.known.insert(
                        *display_id,
                        KnownRecipe {
                            result_items: result_items.clone(),
                            station_items: station_items.clone(),
                            notification: *notification,
                            highlight: *highlight,
                        },
                    );
                }
                self.has_data = true;
                true
            }
            ClientEvent::RecipeBookRemoved { display_ids } => {
                for id in display_ids {
                    self.known.remove(id);
                }
                self.has_data = true;
                true
            }
            ClientEvent::GhostRecipeShown {
                window_id,
                result_items,
            } => {
                self.ghost = Some(GhostRecipe {
                    window_id: *window_id,
                    result_items: result_items.clone(),
                });
                true
            }
            ClientEvent::RecipePropertySetsUpdated {
                item_sets,
                stonecutter_results,
            } => {
                // A whole replace: the packet carries the complete set.
                self.property_sets = item_sets.iter().cloned().collect();
                self.stonecutter_results = stonecutter_results.clone();
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RecipeBookSync;
    use lodestone_model::event::{ClientEvent, RecipeBookEntry};

    fn entry(display_id: i32, result: i32) -> RecipeBookEntry {
        RecipeBookEntry {
            display_id,
            result_items: vec![result],
            station_items: Vec::new(),
            notification: true,
            highlight: false,
        }
    }

    /// The `replace` flag is the one behaviour a naive `extend` gets wrong, and it
    /// arrives *after* the list on the wire.
    #[test]
    fn replace_discards_the_previous_generation() {
        let mut store = RecipeBookSync::new();
        store.apply(&ClientEvent::RecipeBookAdded {
            entries: vec![entry(1, 10), entry(2, 20)],
            replace: false,
        });
        assert_eq!(store.known().len(), 2);

        store.apply(&ClientEvent::RecipeBookAdded {
            entries: vec![entry(3, 30)],
            replace: false,
        });
        assert_eq!(store.known().len(), 3, "replace:false merges");

        store.apply(&ClientEvent::RecipeBookAdded {
            entries: vec![entry(9, 90)],
            replace: true,
        });
        assert_eq!(store.known().len(), 1, "replace:true discards");
        assert!(store.is_unlocked(9));
        assert!(!store.is_unlocked(1));
    }

    /// `has_data` must survive an add-then-remove that empties the set, for the
    /// same reason `RecipeUnlockState::has_data` exists.
    #[test]
    fn has_data_survives_an_add_then_remove() {
        let mut store = RecipeBookSync::new();
        assert!(!store.has_data());
        store.apply(&ClientEvent::RecipeBookAdded {
            entries: vec![entry(1, 10)],
            replace: false,
        });
        store.apply(&ClientEvent::RecipeBookRemoved {
            display_ids: vec![1],
        });
        assert!(store.known().is_empty());
        assert!(
            store.has_data(),
            "an emptied set is not the same as no data -- a consumer would otherwise \
             fall back to showing everything unlocked"
        );
    }

    /// The join a panel needs, given that a display id carries no recipe name.
    #[test]
    fn unlocked_recipes_can_be_found_by_their_result_item() {
        let mut store = RecipeBookSync::new();
        store.apply(&ClientEvent::RecipeBookAdded {
            entries: vec![entry(1, 10), entry(2, 20), entry(3, 10)],
            replace: true,
        });
        let producing: Vec<i32> = store.unlocked_producing(10).map(|(id, _)| id).collect();
        assert_eq!(producing, vec![1, 3]);
        assert_eq!(store.unlocked_producing(99).count(), 0);
    }

    /// The station item id must survive the fold — this is what a recipe-unlock
    /// toast's corner icon reads. Distinct from
    /// `result_items` so a transposition of the two would fail this.
    #[test]
    fn station_items_survive_the_fold() {
        let mut store = RecipeBookSync::new();
        store.apply(&ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 1,
                result_items: vec![10],
                station_items: vec![99],
                notification: true,
                highlight: false,
            }],
            replace: true,
        });
        let recipe = store.known().get(&1).expect("entry 1 is known");
        assert_eq!(recipe.result_items, vec![10]);
        assert_eq!(recipe.station_items, vec![99]);
    }

    #[test]
    fn property_sets_replace_wholesale() {
        let mut store = RecipeBookSync::new();
        let key: lodestone_model::Identifier = "minecraft:furnace_input".parse().unwrap();
        store.apply(&ClientEvent::RecipePropertySetsUpdated {
            item_sets: vec![(key.clone(), vec![1, 2, 3])],
            stonecutter_results: vec![vec![7]],
        });
        assert_eq!(store.property_set(&key), Some(&[1, 2, 3][..]));
        assert_eq!(store.stonecutter_results().len(), 1);

        store.apply(&ClientEvent::RecipePropertySetsUpdated {
            item_sets: Vec::new(),
            stonecutter_results: Vec::new(),
        });
        assert_eq!(store.property_set_count(), 0);
        assert!(store.stonecutter_results().is_empty());
    }

    #[test]
    fn a_ghost_is_one_slot() {
        let mut store = RecipeBookSync::new();
        store.apply(&ClientEvent::GhostRecipeShown {
            window_id: 3,
            result_items: vec![5],
        });
        assert_eq!(store.ghost().map(|g| g.window_id), Some(3));
    }

    #[test]
    fn an_unrelated_event_is_rejected() {
        let mut store = RecipeBookSync::new();
        assert!(!store.apply(&ClientEvent::KeepAlive { id: 1 }));
    }
}
