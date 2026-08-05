//! Plugin-defined custom items as a shared resource — issue #147.
//!
//! # What this is
//!
//! The `World`-owned half of [`lodestone_game::custom_item`]. The definitions and
//! the recognition logic live in that crate (which has no ECS dependency); this
//! module is what makes **one** registry shared by every plugin in the process,
//! so two plugins can recognise each other's items instead of each keeping a
//! private table that the other's stacks are invisible to.
//!
//! # Why sharing matters here and did not for recipes
//!
//! `lodestone_ecs::recipes` exists because the *engine* has to consume the
//! registration — the container screen matches against the corpus. Custom items
//! are different: the plugin is usually both producer and consumer, so a private
//! table would work for a single plugin. It is the multi-plugin case that needs
//! one owner: an economy plugin has to be able to ask "is this token one of the
//! shop plugin's?", and `CustomItemRegistry::identify` can only answer that if
//! both registered into the same place.
//!
//! # Usage
//!
//! ```
//! use lodestone_ecs::app::{App, Plugin};
//! use lodestone_ecs::items::{CustomItemsExt, CustomItemsPlugin};
//! use lodestone_game::custom_item::CustomItem;
//! use lodestone_model::Text;
//!
//! struct MyRpgPlugin;
//!
//! impl Plugin for MyRpgPlugin {
//!     fn build(&self, app: &mut App) {
//!         app.add_custom_item(
//!             CustomItem::new(
//!                 "myrpg:flamebrand".parse().unwrap(),
//!                 "minecraft:diamond_sword".parse().unwrap(),
//!             )
//!             .with_display_name(Text::literal("Flamebrand"))
//!             .with_custom_model_data(7),
//!         )
//!         .expect("a fresh id in our own namespace");
//!     }
//! }
//!
//! let mut app = App::new();
//! app.add_plugins((CustomItemsPlugin, MyRpgPlugin));
//! ```
//!
//! # How to change it
//!
//! This module is transport. Definition validation and recognition belong in
//! `lodestone_game::custom_item`; adding a rule here would mean a plugin holding
//! its own `CustomItemRegistry` obeyed different rules from one going through the
//! resource, which is the shape that produces "works in my test, not in the game".
//!
//! # Dependencies
//!
//! `lodestone_game::custom_item` and `bevy_ecs`.

use bevy_app::{App, Plugin};
use bevy_ecs::resource::Resource;
use lodestone_game::custom_item::{CustomItem, CustomItemError, CustomItemRegistry};

/// The process's one [`CustomItemRegistry`].
#[derive(Resource, Debug, Default)]
pub struct CustomItems(pub CustomItemRegistry);

/// Installs [`CustomItems`].
///
/// `init_resource`, not `insert_resource`, so a plugin that registered an item
/// before this plugin was added — via [`CustomItemsExt::add_custom_item`], which
/// installs the resource on demand — does not have its definitions wiped. Pinned
/// by a test, as for `RecipeRegistryPlugin`.
#[derive(Debug, Default)]
pub struct CustomItemsPlugin;

impl Plugin for CustomItemsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CustomItems>();
    }
}

/// `App`-level custom-item registration, so a plugin's `build` reads as one call.
pub trait CustomItemsExt {
    /// Registers a custom item, installing [`CustomItems`] first if absent.
    ///
    /// # Errors
    ///
    /// [`CustomItemError`], as `CustomItemRegistry::register` — the two namespace
    /// rules plus a duplicate id.
    fn add_custom_item(&mut self, item: CustomItem) -> Result<&mut Self, CustomItemError>;
}

impl CustomItemsExt for App {
    fn add_custom_item(&mut self, item: CustomItem) -> Result<&mut Self, CustomItemError> {
        self.init_resource::<CustomItems>();
        self.world_mut()
            .resource_mut::<CustomItems>()
            .0
            .register(item)?;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::{Identifier, Text};

    fn id(s: &str) -> Identifier {
        s.parse().expect("valid id")
    }

    fn flamebrand() -> CustomItem {
        CustomItem::new(id("myrpg:flamebrand"), id("minecraft:diamond_sword"))
            .with_display_name(Text::literal("Flamebrand"))
    }

    /// Two plugins share one registry, which is the entire reason this resource
    /// exists: the second plugin can identify the first's item.
    #[test]
    fn two_plugins_share_one_registry_and_can_identify_each_others_items() {
        struct ShopPlugin;
        impl Plugin for ShopPlugin {
            fn build(&self, app: &mut App) {
                app.add_custom_item(CustomItem::new(
                    id("shop:token"),
                    id("minecraft:emerald"),
                ))
                .expect("registers");
            }
        }
        struct RpgPlugin;
        impl Plugin for RpgPlugin {
            fn build(&self, app: &mut App) {
                app.add_custom_item(flamebrand()).expect("registers");
            }
        }

        let mut app = App::new();
        app.add_plugins((CustomItemsPlugin, ShopPlugin, RpgPlugin));

        let registry = &app.world().resource::<CustomItems>().0;
        assert_eq!(registry.len(), 2);

        // The RPG plugin's stack is identifiable through the same registry the
        // shop plugin registered into.
        let token = registry
            .stack(id("shop:token"), 1)
            .expect("the shop's item is registered");
        assert_eq!(
            registry.identify(&token).map(CustomItem::id),
            Some(&id("shop:token"))
        );
        let sword = registry
            .stack(id("myrpg:flamebrand"), 1)
            .expect("the rpg's item is registered");
        assert_eq!(
            registry.identify(&sword).map(CustomItem::id),
            Some(&id("myrpg:flamebrand"))
        );
    }

    /// The control: with only one plugin installed, the *other* plugin's id is
    /// unknown — so the test above is measuring shared registration rather than a
    /// registry that says yes to everything.
    #[test]
    fn control_an_unregistered_id_is_not_identifiable() {
        struct RpgPlugin;
        impl Plugin for RpgPlugin {
            fn build(&self, app: &mut App) {
                app.add_custom_item(flamebrand()).expect("registers");
            }
        }
        let mut app = App::new();
        app.add_plugins((CustomItemsPlugin, RpgPlugin));
        let registry = &app.world().resource::<CustomItems>().0;
        assert_eq!(registry.len(), 1);
        assert!(
            registry.stack(id("shop:token"), 1).is_none(),
            "an unregistered id must not produce a stack"
        );
    }

    #[test]
    fn add_custom_item_installs_the_resource_and_the_plugin_does_not_zero_it() {
        let mut app = App::new();
        app.add_custom_item(flamebrand())
            .expect("registers with no plugin added");
        app.add_plugins(CustomItemsPlugin);
        assert_eq!(
            app.world().resource::<CustomItems>().0.len(),
            1,
            "CustomItemsPlugin must not reset a live registry"
        );
    }
}
