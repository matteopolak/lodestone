//! The reference plugin for issue #150's crafting-station hook seam
//! (`lodestone_server::plugin_crafting`): two small, real hooks proving a
//! plugin can allow, deny, or replace a station's result before a player
//! ever sees it.
//!
//! [`AnvilBlessing`] demonstrates [`StationVerdict::Replace`] — the "renames
//! an anvil result" example from the issue itself: any anvil operation that
//! already produces a custom-named result gets `"[Blessed] "` prepended to
//! that name, tweaking the *real* vanilla-computed result
//! ([`StationInputs::computed`]) rather than reimplementing
//! `crate::anvil::compute`'s own rename/repair/combine rules from scratch.
//!
//! [`SmithingSwordBan`] demonstrates [`StationVerdict::Deny`] — the "vetoes a
//! smithing combination" example: it refuses one specific netherite upgrade
//! (`minecraft:diamond_sword` → `minecraft:netherite_sword`) while leaving
//! every other netherite upgrade and every armour trim untouched, showing
//! that a veto is scoped to the exact input it names rather than the whole
//! station.
//!
//! Both hooks are read-only with respect to everything except the
//! [`StationVerdict`] they return — neither touches a `World`, an inventory,
//! or XP, matching `docs/plugin-crafting-hooks.md`'s "cost is untouched"
//! note (a hook can change what a player receives, never what a take
//! costs).
//!
//! [`register`] is the one function a host calls, mirroring
//! `lodestone_void_world::register`'s own free-function convention for a
//! seam that is a plain registry rather than a `bevy_app::Plugin` — this
//! plugin has no `bevy_ecs` dependency at all, since the crafting-station
//! seam it exercises lives entirely in `lodestone-server`'s own,
//! non-ECS-schedule code (see `plugin_crafting`'s own module doc for why).
//!
//! `crates/lodestone-server/src/server.rs`'s own test module is the
//! end-to-end proof, not this crate: it depends on this crate as a
//! **dev-dependency** (a safe cycle — see that `Cargo.toml`'s existing
//! `lodestone-v26-2` dev-dependency comment for the identical reasoning) and
//! drives the *real* `apply_container_clicked`/`apply_workstation_clicked`/
//! `apply_container_button_click` production dispatch functions with these
//! exact hooks registered, because this crate cannot reach those
//! module-private functions itself. A test in this crate calling
//! [`AnvilBlessing::on_prepare`]/[`SmithingSwordBan::on_prepare`] directly
//! would be the closed loop `CLAUDE.md` warns about — proof that the logic
//! is correct, not that production ever calls it.

use std::sync::Arc;

use lodestone_model::text::Text;
use lodestone_server::container_click::Station;
use lodestone_server::plugin_crafting::{CraftingStationHook, CraftingStationHooks, StationInputs, StationVerdict};

/// Prepended to a blessed anvil result's custom name. Checked on the way in
/// so re-reading an already-blessed menu (a second `workstation_result` call
/// with no new click, e.g. redrawing after an unrelated slot change) does
/// not compound into `"[Blessed] [Blessed] …"`.
const BLESSED_PREFIX: &str = "[Blessed] ";

/// The one upgrade [`SmithingSwordBan`] refuses.
const BANNED_UPGRADE_BASE: &str = "minecraft:diamond_sword";

/// Tweaks a real anvil result rather than replacing it wholesale —
/// [`StationVerdict::Replace`]'s intended shape.
#[derive(Debug, Default)]
pub struct AnvilBlessing;

impl CraftingStationHook for AnvilBlessing {
    fn on_prepare(&self, inputs: &StationInputs) -> StationVerdict {
        if inputs.station != Station::Anvil {
            return StationVerdict::Allow;
        }
        let Some(computed) = inputs.computed.clone() else {
            return StationVerdict::Allow;
        };
        let Some(name) = computed.components.custom_name.as_ref() else {
            // Not a rename (a plain repair/combine) — nothing to bless.
            return StationVerdict::Allow;
        };
        let plain = name.to_plain_string();
        if plain.starts_with(BLESSED_PREFIX) {
            return StationVerdict::Allow;
        }
        let mut blessed = computed;
        blessed.components.custom_name = Some(Text::literal(format!("{BLESSED_PREFIX}{plain}")));
        StationVerdict::Replace(blessed)
    }
}

/// Refuses one specific netherite upgrade outright, regardless of what the
/// station itself computed — [`StationVerdict::Deny`]'s intended shape.
/// Every other smithing-table operation (any other netherite upgrade, every
/// armour trim) is untouched.
#[derive(Debug, Default)]
pub struct SmithingSwordBan;

impl CraftingStationHook for SmithingSwordBan {
    fn on_prepare(&self, inputs: &StationInputs) -> StationVerdict {
        if inputs.station != Station::Smithing {
            return StationVerdict::Allow;
        }
        // `[template, base, addition]` — see `workstation_result`'s own doc
        // for the smithing table's cell order.
        let base = inputs.cells.get(1).and_then(Option::as_ref);
        let is_banned = base.is_some_and(|item| item.item.to_string() == BANNED_UPGRADE_BASE);
        if is_banned {
            StationVerdict::Deny
        } else {
            StationVerdict::Allow
        }
    }
}

/// Registers both hooks on `hooks`. The one function a host (or a test)
/// calls — mirrors `lodestone_void_world::register`'s own free-function
/// convention.
pub fn register(hooks: &CraftingStationHooks) {
    hooks.register(0, Arc::new(AnvilBlessing));
    hooks.register(0, Arc::new(SmithingSwordBan));
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::ItemStack;

    fn stack(item: &str) -> ItemStack {
        ItemStack::new(item.parse().expect("valid key"), 1)
    }

    fn named(item: &str, name: &str) -> ItemStack {
        let mut s = stack(item);
        s.components.custom_name = Some(Text::literal(name));
        s
    }

    /// The discriminating case for `AnvilBlessing`: an anvil result that
    /// carries a custom name is blessed; a plain repair (no name at all) is
    /// left alone — proving the hook does not simply replace every anvil
    /// result unconditionally.
    #[test]
    fn anvil_blessing_prefixes_a_named_result_and_leaves_a_nameless_repair_alone() {
        let renamed = StationInputs {
            station: Station::Anvil,
            cells: vec![None, None],
            computed: Some(named("minecraft:diamond_sword", "Excalibur")),
        };
        match AnvilBlessing.on_prepare(&renamed) {
            StationVerdict::Replace(item) => {
                let name = item.components.custom_name.expect("still named");
                assert_eq!(name.to_plain_string(), "[Blessed] Excalibur");
            }
            other => panic!("expected Replace, got {other:?}"),
        }

        let plain_repair = StationInputs {
            station: Station::Anvil,
            cells: vec![None, None],
            computed: Some(stack("minecraft:diamond_sword")),
        };
        assert!(matches!(AnvilBlessing.on_prepare(&plain_repair), StationVerdict::Allow));
    }

    /// Blessing is idempotent: a second evaluation of an already-blessed
    /// result must not compound the prefix.
    #[test]
    fn anvil_blessing_does_not_double_bless() {
        let already = StationInputs {
            station: Station::Anvil,
            cells: vec![None, None],
            computed: Some(named("minecraft:diamond_sword", "[Blessed] Excalibur")),
        };
        assert!(matches!(AnvilBlessing.on_prepare(&already), StationVerdict::Allow));
    }

    /// A non-anvil station must never be touched by `AnvilBlessing` —
    /// otherwise a loom result carrying a custom name (bannners cannot be
    /// named, but the check must not rely on that) would be misclassified.
    #[test]
    fn anvil_blessing_ignores_every_other_station() {
        let other_station = StationInputs {
            station: Station::Smithing,
            cells: vec![None, None, None],
            computed: Some(named("minecraft:diamond_sword", "Excalibur")),
        };
        assert!(matches!(AnvilBlessing.on_prepare(&other_station), StationVerdict::Allow));
    }

    /// The discriminating case for `SmithingSwordBan`: a diamond-sword
    /// upgrade is denied, while a diamond-pickaxe upgrade (same template and
    /// addition, a different base) is allowed through unchanged.
    #[test]
    fn smithing_sword_ban_denies_only_the_named_base_item() {
        let sword = StationInputs {
            station: Station::Smithing,
            cells: vec![
                Some(stack("minecraft:netherite_upgrade_smithing_template")),
                Some(stack("minecraft:diamond_sword")),
                Some(stack("minecraft:netherite_ingot")),
            ],
            computed: Some(stack("minecraft:netherite_sword")),
        };
        assert!(matches!(SmithingSwordBan.on_prepare(&sword), StationVerdict::Deny));

        let pickaxe = StationInputs {
            station: Station::Smithing,
            cells: vec![
                Some(stack("minecraft:netherite_upgrade_smithing_template")),
                Some(stack("minecraft:diamond_pickaxe")),
                Some(stack("minecraft:netherite_ingot")),
            ],
            computed: Some(stack("minecraft:netherite_pickaxe")),
        };
        assert!(matches!(SmithingSwordBan.on_prepare(&pickaxe), StationVerdict::Allow));
    }

    /// A non-smithing station must never be denied by `SmithingSwordBan`.
    #[test]
    fn smithing_sword_ban_ignores_every_other_station() {
        let anvil = StationInputs {
            station: Station::Anvil,
            cells: vec![Some(stack("minecraft:diamond_sword")), None],
            computed: Some(stack("minecraft:diamond_sword")),
        };
        assert!(matches!(SmithingSwordBan.on_prepare(&anvil), StationVerdict::Allow));
    }

    #[test]
    fn register_installs_both_hooks() {
        let hooks = CraftingStationHooks::new();
        register(&hooks);
        assert_eq!(hooks.len(), 2);
    }
}
