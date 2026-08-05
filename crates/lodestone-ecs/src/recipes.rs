//! Runtime recipe registration for plugins — issue #148, the `Bukkit.addRecipe`
//! analogue.
//!
//! # What this is
//!
//! The plugin-facing door onto [`lodestone_game::recipe::RecipeBook`]. The
//! recipe *system* already existed before this module: a 1585-recipe vanilla
//! corpus loaded from `client.jar`, a shaped/shapeless matcher, an
//! occupied-cell-count index, and a recipe-book UI (`docs/crafting.md`). What
//! did not exist was any way for a plugin to add to it — the corpus was loaded
//! once at GPU bring-up into a private field and never written again.
//!
//! # The load-order problem this solves
//!
//! A plugin's `Plugin::build` runs **before** the corpus exists. The shell loads
//! `client.jar`'s recipes at GPU bring-up, which is long after every
//! `add_plugins` call has returned, and a headless consumer may never load a
//! corpus at all. So a registration API that needed `&mut RecipeBook` would be
//! unusable from the one place a plugin author can actually write code.
//!
//! [`RecipeRegistry`] therefore holds registrations *pending* and replays them
//! onto whichever corpus the process later adopts
//! ([`RecipeRegistry::adopt_corpus`]). Registration is consequently
//! order-independent: a plugin may register before or after the corpus loads and
//! gets the same result either way, and the shell needs no knowledge of which
//! plugins exist.
//!
//! This matches issue #148's own scoping — "*during setup, before any session
//! starts*" — while still supporting the live case, because `adopt_corpus` is not
//! a one-shot: [`RecipeRegistry::register`] applies straight to the book once one
//! has been adopted, and bumps [`RecipeRegistry::revision`] so a cached reader
//! knows to refresh.
//!
//! # Usage, as a third-party plugin writes it
//!
//! ```
//! use lodestone_ecs::app::{App, Plugin};
//! use lodestone_ecs::recipes::{RecipeRegistryExt, RecipeRegistryPlugin};
//! use lodestone_game::item::ItemStack;
//! use lodestone_game::recipe::{
//!     Ingredient, Recipe, RecipeRegistration, ShapelessRecipe,
//! };
//!
//! struct SparkleStickPlugin;
//!
//! impl Plugin for SparkleStickPlugin {
//!     fn build(&self, app: &mut App) {
//!         let recipe = Recipe::Shapeless(ShapelessRecipe::new(
//!             vec![
//!                 Ingredient::Item("minecraft:stick".parse().unwrap()),
//!                 Ingredient::Item("minecraft:diamond".parse().unwrap()),
//!             ],
//!             ItemStack::new("minecraft:blaze_rod".parse().unwrap(), 1),
//!         ));
//!         app.add_recipe(RecipeRegistration::new(
//!             "sparkle:sparkle_stick".parse().unwrap(),
//!             recipe,
//!         ))
//!         .expect("a fresh id in our own namespace registers");
//!     }
//! }
//!
//! let mut app = App::new();
//! app.add_plugins((RecipeRegistryPlugin, SparkleStickPlugin));
//! ```
//!
//! [`RecipeRegistryExt::add_recipe`] installs the resource itself if it is
//! absent, so a plugin does not have to care whether `RecipeRegistryPlugin` was
//! added before it. Adding the plugin explicitly is still worth doing when the
//! plugin registers nothing at build time and only reads the book later.
//!
//! # How to change it
//!
//! The validation rules live in `lodestone_game::recipe::RecipeRegistration`,
//! not here — this module is transport. Two gotchas:
//!
//! * **`minecraft:` is refused.** A plugin cannot replace a vanilla recipe by
//!   id; that is a datapack's privilege. The check is the *only* thing making
//!   plugin-vs-vanilla id collisions impossible, which is why
//!   [`RecipeRegistry::register`] can validate duplicates against `pending`
//!   alone and still be correct before a corpus exists.
//! * **`adopt_corpus` replaces the book, then replays pending.** Calling it
//!   twice is legal (a session teardown and re-load) and does not lose plugin
//!   recipes. It does *not* preserve recipes inserted directly into the previous
//!   book by something other than this registry — there is no such caller today,
//!   and if one appears it should register instead.
//!
//! # Dependencies
//!
//! `lodestone_game::recipe` for the corpus, matcher and validation. Nothing
//! version-specific: recipes are `Identifier`-keyed, never numeric ids.

use bevy_app::{App, Plugin};
use bevy_ecs::resource::Resource;
use lodestone_game::recipe::{RecipeBook, RecipeRegisterError, RecipeRegistration};
use lodestone_model::Identifier;

/// The `World`'s authoritative recipe corpus, plus every plugin registration
/// made against it.
///
/// A resource rather than a component: there is exactly one recipe corpus per
/// process, the same way there is one [`crate::WorldTime`].
///
/// Empty by default, which is a real and expected state — a headless consumer or
/// a jar-less run has no vanilla corpus, and plugin recipes still register and
/// still match. That is deliberate: it means a plugin's own test needs no
/// `client.jar`.
#[derive(Resource, Debug, Default)]
pub struct RecipeRegistry {
    /// The corpus, with every applied registration already folded in.
    book: RecipeBook,
    /// Registrations in the order they were made, replayed onto any corpus
    /// [`Self::adopt_corpus`] later installs.
    pending: Vec<RecipeRegistration>,
    /// Whether a corpus has been adopted. Distinguishes "no recipes because
    /// nothing loaded yet" from "no recipes because the jar had none", which a
    /// caller deciding whether to draw a recipe-book panel needs.
    corpus_adopted: bool,
    /// Bumped on every mutation, so a cached reader can tell whether its clone
    /// is stale without comparing corpora.
    revision: u64,
}

impl RecipeRegistry {
    /// Registers a recipe, applying it to the corpus immediately if one has been
    /// adopted and replaying it onto the next one otherwise.
    ///
    /// # Errors
    ///
    /// [`RecipeRegisterError`], unchanged from
    /// [`RecipeBook::register`] — with one addition this layer is responsible
    /// for: a [`RecipeRegisterError::Duplicate`] is raised against an id already
    /// *pending* as well as one already in the book, so two plugins colliding
    /// before the corpus loads is caught at the second registration rather than
    /// silently at adoption time.
    pub fn register(
        &mut self,
        registration: RecipeRegistration,
    ) -> Result<(), RecipeRegisterError> {
        registration.validate()?;
        if self.pending.iter().any(|p| p.id() == registration.id()) {
            return Err(RecipeRegisterError::Duplicate(registration.id().clone()));
        }
        if self.corpus_adopted {
            self.book.register(registration.clone())?;
        }
        self.pending.push(registration);
        self.revision += 1;
        Ok(())
    }

    /// Removes a registration by id, from both the pending list and the live
    /// corpus.
    ///
    /// Returns whether anything was removed. Vanilla recipes are removable this
    /// way too — a plugin disabling a vanilla recipe is a real Bukkit idiom
    /// (`Bukkit.removeRecipe`) — which is why this consults the book rather than
    /// only the pending list.
    pub fn unregister(&mut self, id: &Identifier) -> bool {
        let was_pending = self.pending.iter().any(|p| p.id() == id);
        self.pending.retain(|p| p.id() != id);
        let was_in_book = self.book.unregister(id).is_some();
        let removed = was_pending || was_in_book;
        if removed {
            self.revision += 1;
        }
        removed
    }

    /// Installs a freshly loaded corpus and replays every pending registration
    /// onto it, returning the merged book.
    ///
    /// This is the shell's call at GPU bring-up. Registrations that fail here
    /// are dropped rather than aborting the load, on the same principle as
    /// `lodestone_game::recipe_json::CorpusBuilder::failures`: one bad recipe
    /// must not cost the player the other 1584. The count is available as
    /// [`Self::pending_len`] versus [`RecipeBook::len`] for a caller that wants
    /// to notice.
    pub fn adopt_corpus(&mut self, book: RecipeBook) -> &RecipeBook {
        self.book = book;
        self.corpus_adopted = true;
        // `pending` is drained into a local first: `register` borrows `self`
        // mutably, and replaying in registration order is what makes two
        // plugins' registrations deterministic.
        let pending = std::mem::take(&mut self.pending);
        for registration in pending {
            if self.book.register(registration.clone()).is_ok() {
                self.pending.push(registration);
            }
        }
        self.revision += 1;
        &self.book
    }

    /// The corpus, with plugin registrations folded in.
    #[must_use]
    pub fn book(&self) -> &RecipeBook {
        &self.book
    }

    /// The corpus, for a caller that needs to mutate it directly. Bumps
    /// [`Self::revision`] unconditionally, since it cannot know whether the
    /// caller wrote anything.
    pub fn book_mut(&mut self) -> &mut RecipeBook {
        self.revision += 1;
        &mut self.book
    }

    /// A clone of the corpus, for a consumer that caches one outside the
    /// `World` (the shell's per-frame draw does, to avoid holding an `EcsHandle`
    /// guard across a render pass).
    #[must_use]
    pub fn snapshot(&self) -> RecipeBook {
        self.book.clone()
    }

    /// Whether a corpus has been adopted — as distinct from an empty one.
    #[must_use]
    pub fn corpus_adopted(&self) -> bool {
        self.corpus_adopted
    }

    /// How many plugin registrations are live.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Every live plugin registration, in registration order.
    pub fn registrations(&self) -> impl Iterator<Item = &RecipeRegistration> {
        self.pending.iter()
    }

    /// Bumped on every mutation. A cached reader compares this against the
    /// revision its clone was taken at.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// Installs [`RecipeRegistry`].
///
/// `init_resource`, not `insert_resource`, so a plugin that registered a recipe
/// before this plugin was added — via [`RecipeRegistryExt::add_recipe`], which
/// installs the resource on demand — does not have its registration zeroed.
/// That property is the whole reason this is not `insert_resource`, and it is
/// pinned by a test.
#[derive(Debug, Default)]
pub struct RecipeRegistryPlugin;

impl Plugin for RecipeRegistryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RecipeRegistry>();
    }
}

/// `App`-level recipe registration, so a plugin's `build` reads as one call
/// rather than four lines of resource plumbing.
///
/// The shape mirrors `Bukkit.addRecipe` deliberately (issue #77's portability
/// goal): a plugin author coming from Paper looks for a one-call registration on
/// the server handle, and `App` is this framework's server handle.
pub trait RecipeRegistryExt {
    /// Registers a recipe, installing [`RecipeRegistry`] first if absent.
    ///
    /// # Errors
    ///
    /// [`RecipeRegisterError`], as [`RecipeRegistry::register`].
    fn add_recipe(
        &mut self,
        registration: RecipeRegistration,
    ) -> Result<&mut Self, RecipeRegisterError>;
}

impl RecipeRegistryExt for App {
    fn add_recipe(
        &mut self,
        registration: RecipeRegistration,
    ) -> Result<&mut Self, RecipeRegisterError> {
        self.init_resource::<RecipeRegistry>();
        self.world_mut()
            .resource_mut::<RecipeRegistry>()
            .register(registration)?;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::item::ItemStack;
    use lodestone_game::recipe::{
        CraftingGrid, Ingredient, Recipe, RecipeCategory, ShapelessRecipe,
    };
    use lodestone_model::RecipeBookType;

    fn id(s: &str) -> Identifier {
        s.parse().expect("test id parses")
    }

    /// A shapeless stick + diamond -> blaze rod, the recipe every test below
    /// registers.
    fn sparkle_recipe() -> Recipe {
        Recipe::Shapeless(ShapelessRecipe::new(
            vec![
                Ingredient::Item(id("minecraft:stick")),
                Ingredient::Item(id("minecraft:diamond")),
            ],
            ItemStack::new(id("minecraft:blaze_rod"), 1),
        ))
    }

    /// A 3x3 grid holding exactly a stick and a diamond.
    fn sparkle_grid() -> CraftingGrid {
        let mut cells = vec![None; 9];
        cells[0] = Some(id("minecraft:stick"));
        cells[1] = Some(id("minecraft:diamond"));
        CraftingGrid::new(3, 3, cells)
    }

    #[test]
    fn a_registration_before_the_corpus_loads_survives_adoption() {
        let mut registry = RecipeRegistry::default();
        registry
            .register(RecipeRegistration::new(
                id("sparkle:sparkle_stick"),
                sparkle_recipe(),
            ))
            .expect("registers");
        // Before adoption the recipe is already matchable, because an empty book
        // is a real book.
        assert_eq!(registry.pending_len(), 1);
        assert!(!registry.corpus_adopted());

        // A "vanilla corpus" standing in for `client.jar`'s.
        let mut corpus = RecipeBook::new();
        corpus.insert(
            id("minecraft:blaze_rod_from_nothing"),
            Recipe::Special("dummy".to_owned()),
        );
        registry.adopt_corpus(corpus);

        assert!(registry.corpus_adopted());
        assert_eq!(registry.book().len(), 2, "vanilla recipe plus the plugin's");
        let result = registry
            .book()
            .match_grid(&sparkle_grid())
            .expect("the plugin recipe matches after adoption");
        assert_eq!(result.item(), &id("minecraft:blaze_rod"));
    }

    /// The control for the above: with the registration removed, the identical
    /// grid against the identical adopted corpus matches **nothing**. Without
    /// this, `match_grid` returning a blaze rod could be any recipe at all.
    #[test]
    fn control_without_the_registration_the_same_grid_matches_nothing() {
        let mut registry = RecipeRegistry::default();
        let mut corpus = RecipeBook::new();
        corpus.insert(
            id("minecraft:blaze_rod_from_nothing"),
            Recipe::Special("dummy".to_owned()),
        );
        registry.adopt_corpus(corpus);
        assert_eq!(registry.book().len(), 1);
        assert!(
            registry.book().match_grid(&sparkle_grid()).is_none(),
            "the detector fires only because of the registration"
        );
    }

    #[test]
    fn registering_after_adoption_reaches_the_book_immediately() {
        let mut registry = RecipeRegistry::default();
        registry.adopt_corpus(RecipeBook::new());
        let before = registry.revision();
        registry
            .register(RecipeRegistration::new(
                id("sparkle:sparkle_stick"),
                sparkle_recipe(),
            ))
            .expect("registers");
        assert!(registry.book().match_grid(&sparkle_grid()).is_some());
        assert!(
            registry.revision() > before,
            "a cached reader must be able to notice"
        );
    }

    #[test]
    fn adopting_a_second_corpus_does_not_lose_plugin_recipes() {
        let mut registry = RecipeRegistry::default();
        registry
            .register(RecipeRegistration::new(
                id("sparkle:sparkle_stick"),
                sparkle_recipe(),
            ))
            .expect("registers");
        registry.adopt_corpus(RecipeBook::new());
        assert!(registry.book().match_grid(&sparkle_grid()).is_some());
        // A reconnect reloads the corpus.
        registry.adopt_corpus(RecipeBook::new());
        assert!(
            registry.book().match_grid(&sparkle_grid()).is_some(),
            "the replay must be idempotent across adoptions"
        );
        assert_eq!(registry.pending_len(), 1, "and must not duplicate");
    }

    #[test]
    fn unregistering_removes_it_from_the_live_book() {
        let mut registry = RecipeRegistry::default();
        registry
            .register(RecipeRegistration::new(
                id("sparkle:sparkle_stick"),
                sparkle_recipe(),
            ))
            .expect("registers");
        registry.adopt_corpus(RecipeBook::new());
        assert!(registry.book().match_grid(&sparkle_grid()).is_some());

        assert!(registry.unregister(&id("sparkle:sparkle_stick")));
        assert!(
            registry.book().match_grid(&sparkle_grid()).is_none(),
            "removal must keep the grid index coherent, not just drop the entry"
        );
        assert!(!registry.unregister(&id("sparkle:sparkle_stick")));
    }

    #[test]
    fn the_vanilla_namespace_is_refused() {
        let mut registry = RecipeRegistry::default();
        let err = registry
            .register(RecipeRegistration::new(
                id("minecraft:sparkle_stick"),
                sparkle_recipe(),
            ))
            .expect_err("minecraft: is vanilla's");
        assert!(matches!(err, RecipeRegisterError::ReservedNamespace(_)));
        assert_eq!(registry.pending_len(), 0);
    }

    #[test]
    fn a_duplicate_id_is_refused_before_the_corpus_exists() {
        let mut registry = RecipeRegistry::default();
        let reg = RecipeRegistration::new(id("sparkle:sparkle_stick"), sparkle_recipe());
        registry.register(reg.clone()).expect("first registers");
        let err = registry.register(reg).expect_err("second does not");
        assert!(matches!(err, RecipeRegisterError::Duplicate(_)));
        assert_eq!(registry.pending_len(), 1);
    }

    #[test]
    fn a_registration_is_browsable_in_the_recipe_book_panel() {
        // The recipe-book UI reads `browse`, not `match_grid`, so a registration
        // that matched but did not browse would be craftable and invisible.
        let mut registry = RecipeRegistry::default();
        registry
            .register(
                RecipeRegistration::new(id("sparkle:sparkle_stick"), sparkle_recipe())
                    .with_category(RecipeCategory::Equipment),
            )
            .expect("registers");
        registry.adopt_corpus(RecipeBook::new());
        let listed = registry
            .book()
            .browse(RecipeBookType::Crafting, Some(RecipeCategory::Equipment), "");
        assert_eq!(listed, vec![&id("sparkle:sparkle_stick")]);
        // And it is findable by the panel's substring search on the result id.
        let found = registry
            .book()
            .browse(RecipeBookType::Crafting, None, "blaze");
        assert_eq!(found, vec![&id("sparkle:sparkle_stick")]);
    }

    #[test]
    fn add_recipe_installs_the_resource_and_the_plugin_does_not_zero_it() {
        // The `init_resource`-not-`insert_resource` property, pinned: a plugin
        // added *after* a registration must not wipe it.
        let mut app = App::new();
        app.add_recipe(RecipeRegistration::new(
            id("sparkle:sparkle_stick"),
            sparkle_recipe(),
        ))
        .expect("registers with no plugin added");
        app.add_plugins(RecipeRegistryPlugin);
        assert_eq!(
            app.world().resource::<RecipeRegistry>().pending_len(),
            1,
            "RecipeRegistryPlugin must not reset a live registry"
        );
    }
}
