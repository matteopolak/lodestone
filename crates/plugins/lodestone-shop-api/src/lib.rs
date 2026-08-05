//! The toy `shop` family's **public message crate** (issue #107): the type a
//! third-party plugin depends on in order to hear about shop purchases, and
//! nothing else.
//!
//! # Why this is a separate crate
//!
//! `lodestone-shop` (the publisher) and `lodestone-shop-stats` (a subscriber)
//! both depend on this. The subscriber does **not** depend on the publisher —
//! that is the property issue #107 is about, and it is why the message type
//! cannot live in the publisher's own crate. A `-api` crate is cheap: one type,
//! one registration call, no logic, no systems of its own beyond registering the
//! message.
//!
//! See [`docs/cross-plugin-messages.md`](../../../../docs/cross-plugin-messages.md)
//! for the pattern and `lodestone_ecs::plugin_message` for the machinery.

use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::prelude::Message;
use lodestone_ecs::plugin_message::PluginMessageAppExt;

/// A purchase happened in the shop. The published contract of the `shop`
/// family — a plugin that reads this needs nothing else from the shop.
///
/// Public fields, matching `lodestone_ecs::GameEvent`'s own reasoning: a
/// subscriber already depends on this crate for the type, so there is nothing
/// to hide behind accessors.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShopPurchase {
    /// What was bought, as an opaque item id — deliberately not a
    /// `lodestone_model` type, so this toy crate stays a pure message crate
    /// with the smallest possible dependency surface.
    pub item: u32,
    /// What it cost.
    pub coins: u32,
}

/// Registers [`ShopPurchase`] on an `App`.
///
/// **Idempotent**, because it goes through
/// [`PluginMessageAppExt::add_plugin_message`]: the publisher and every
/// subscriber all add this plugin, none of them knows whether the others are
/// installed, and the `App` builds regardless. A bare `add_plugins` of the same
/// plugin twice panics — see
/// `lodestone_ecs::plugin_message`'s
/// `tests::adding_the_plugin_directly_twice_panics_which_is_why_the_ext_exists`.
#[derive(Debug, Default)]
pub struct ShopApiPlugin;

impl Plugin for ShopApiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugin_message::<ShopPurchase>();
    }

    /// Both the publisher and every subscriber add this plugin. Without
    /// `is_unique() == false`, bevy's duplicate-plugin check rejects the second
    /// one and the `App` panics at startup — the exact failure the message-crate
    /// pattern has to survive, since neither side can know about the other.
    fn is_unique(&self) -> bool {
        false
    }
}
