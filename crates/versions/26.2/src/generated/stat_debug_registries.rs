// @generated from Minecraft 26.2 (protocol 776) by
// scripts/../ (see this file's module doc) -- DO NOT EDIT BY HAND.
// Source: .cache/mc/26.2/generated/reports/registries.json (sha1 ae2be53fc7655582a95df365f21b7ad18a403248).
//! Generated `stat_type` / `custom_stat` / `debug_subscription` id->identifier
//! tables for protocol 776 (Minecraft 26.2).
//!
//! # What it is
//!
//! Three small synchronized registries the `award_stats` and `debug_*` packets
//! carry as bare VarInt ids, so the adapter can hand `lodestone-game` canonical
//! identifiers rather than numbers. `lodestone-game` is version-free and keyed on
//! identifiers by design; resolving the id is the adapter's job, which is why
//! these tables live in the version crate and not in `lodestone-data`.
//!
//! # How it works
//!
//! All three registries are dense from id 0 (asserted at generation time), so a
//! lookup is a bounds-checked index. `stat_type` selects *which* registry the
//! second id in an `award_stats` entry belongs to -- see
//! [`stat_value_registry`].
//!
//! # How to change it
//!
//! `cargo xtask gen-registries` does **not** support these three registries
//! (it accepts only `sound_event`, `particle_type`, `menu`, `item`,
//! `data_component_type`). Regenerate with the script recorded in
//! `DESIGN.md` SS12.151, or extend the xtask command and delete this note.
//!
//! # Dependencies
//!
//! None at runtime. Generated from the repository's authoritative registry
//! report, recorded in the source header above.

/// Number of stat type entries (network ids are `0..STAT_TYPE_COUNT`).
pub const STAT_TYPE_COUNT: u32 = 9;

/// Canonical stat type identifier, indexed by network registry id.
pub static STAT_TYPE_ENTRIES: [&str; 9] = [
    "minecraft:mined",
    "minecraft:crafted",
    "minecraft:used",
    "minecraft:broken",
    "minecraft:picked_up",
    "minecraft:dropped",
    "minecraft:killed",
    "minecraft:killed_by",
    "minecraft:custom",
];

/// Number of custom stat entries (network ids are `0..CUSTOM_STAT_COUNT`).
pub const CUSTOM_STAT_COUNT: u32 = 77;

/// Canonical custom stat identifier, indexed by network registry id.
pub static CUSTOM_STAT_ENTRIES: [&str; 77] = [
    "minecraft:leave_game",
    "minecraft:play_time",
    "minecraft:total_world_time",
    "minecraft:time_since_death",
    "minecraft:time_since_rest",
    "minecraft:sneak_time",
    "minecraft:walk_one_cm",
    "minecraft:crouch_one_cm",
    "minecraft:sprint_one_cm",
    "minecraft:walk_on_water_one_cm",
    "minecraft:fall_one_cm",
    "minecraft:climb_one_cm",
    "minecraft:fly_one_cm",
    "minecraft:walk_under_water_one_cm",
    "minecraft:minecart_one_cm",
    "minecraft:boat_one_cm",
    "minecraft:pig_one_cm",
    "minecraft:happy_ghast_one_cm",
    "minecraft:horse_one_cm",
    "minecraft:aviate_one_cm",
    "minecraft:swim_one_cm",
    "minecraft:strider_one_cm",
    "minecraft:nautilus_one_cm",
    "minecraft:jump",
    "minecraft:drop",
    "minecraft:damage_dealt",
    "minecraft:damage_dealt_absorbed",
    "minecraft:damage_dealt_resisted",
    "minecraft:damage_taken",
    "minecraft:damage_blocked_by_shield",
    "minecraft:damage_absorbed",
    "minecraft:damage_resisted",
    "minecraft:deaths",
    "minecraft:mob_kills",
    "minecraft:animals_bred",
    "minecraft:player_kills",
    "minecraft:fish_caught",
    "minecraft:talked_to_villager",
    "minecraft:traded_with_villager",
    "minecraft:eat_cake_slice",
    "minecraft:fill_cauldron",
    "minecraft:use_cauldron",
    "minecraft:clean_armor",
    "minecraft:clean_banner",
    "minecraft:clean_shulker_box",
    "minecraft:interact_with_brewingstand",
    "minecraft:interact_with_beacon",
    "minecraft:inspect_dropper",
    "minecraft:inspect_hopper",
    "minecraft:inspect_dispenser",
    "minecraft:play_noteblock",
    "minecraft:tune_noteblock",
    "minecraft:pot_flower",
    "minecraft:trigger_trapped_chest",
    "minecraft:open_enderchest",
    "minecraft:enchant_item",
    "minecraft:play_record",
    "minecraft:interact_with_furnace",
    "minecraft:interact_with_crafting_table",
    "minecraft:open_chest",
    "minecraft:sleep_in_bed",
    "minecraft:open_shulker_box",
    "minecraft:open_barrel",
    "minecraft:interact_with_blast_furnace",
    "minecraft:interact_with_smoker",
    "minecraft:interact_with_lectern",
    "minecraft:interact_with_campfire",
    "minecraft:interact_with_cartography_table",
    "minecraft:interact_with_loom",
    "minecraft:interact_with_stonecutter",
    "minecraft:bell_ring",
    "minecraft:raid_trigger",
    "minecraft:raid_win",
    "minecraft:interact_with_anvil",
    "minecraft:interact_with_grindstone",
    "minecraft:target_hit",
    "minecraft:interact_with_smithing_table",
];

/// Number of debug subscription entries (network ids are `0..DEBUG_SUBSCRIPTION_COUNT`).
pub const DEBUG_SUBSCRIPTION_COUNT: u32 = 16;

/// Canonical debug subscription identifier, indexed by network registry id.
pub static DEBUG_SUBSCRIPTION_ENTRIES: [&str; 16] = [
    "minecraft:dedicated_server_tick_time",
    "minecraft:bees",
    "minecraft:brains",
    "minecraft:breezes",
    "minecraft:goal_selectors",
    "minecraft:entity_paths",
    "minecraft:entity_block_intersections",
    "minecraft:bee_hives",
    "minecraft:pois",
    "minecraft:redstone_wire_orientations",
    "minecraft:village_sections",
    "minecraft:raids",
    "minecraft:structures",
    "minecraft:game_event_listeners",
    "minecraft:neighbor_updates",
    "minecraft:game_events",
];

/// Which registry a stat's **value** id indexes, given its `stat_type` id.
///
/// Vanilla's own stat stream codec is a registry codec over the stat-type
/// registry, dispatching to each stat type's own value stream codec, and
/// each stat type carries its own value registry (confirmed against the
/// decompiled 26.2 source). So the second VarInt in an `award_stats` entry is
/// meaningless without the first: id 3 under `minecraft:mined` is a block, under
/// `minecraft:killed` an entity type, and under `minecraft:custom` one of the 77
/// [`CUSTOM_STAT_ENTRIES`].
///
/// Returns `None` for an id outside [`STAT_TYPE_ENTRIES`].
#[must_use]
pub fn stat_value_registry(stat_type_id: i32) -> Option<StatValueRegistry> {
    let name = *STAT_TYPE_ENTRIES.get(usize::try_from(stat_type_id).ok()?)?;
    Some(match name {
        "minecraft:mined" => StatValueRegistry::Block,
        "minecraft:killed" | "minecraft:killed_by" => StatValueRegistry::EntityType,
        "minecraft:custom" => StatValueRegistry::CustomStat,
        // crafted / used / broken / picked_up / dropped are all `vanilla's own registries's own item`.
        _ => StatValueRegistry::Item,
    })
}

/// The registry a stat value id indexes. See [`stat_value_registry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatValueRegistry {
    /// `vanilla's own registries's own block` -- `minecraft:mined`.
    Block,
    /// `vanilla's own registries's own item` -- crafted, used, broken, picked_up, dropped.
    Item,
    /// `vanilla's own registries's own entity type` -- killed, killed_by.
    EntityType,
    /// `vanilla's own registries's own custom stat` -- [`CUSTOM_STAT_ENTRIES`].
    CustomStat,
}

/// The `minecraft:custom` stat identifier for a value id, or `None` out of range.
#[must_use]
pub fn custom_stat_name(id: i32) -> Option<&'static str> {
    CUSTOM_STAT_ENTRIES
        .get(usize::try_from(id).ok()?)
        .copied()
}

/// The `stat_type` identifier for an id, or `None` out of range.
#[must_use]
pub fn stat_type_name(id: i32) -> Option<&'static str> {
    STAT_TYPE_ENTRIES.get(usize::try_from(id).ok()?).copied()
}

/// The `debug_subscription` identifier for an id, or `None` out of range.
#[must_use]
pub fn debug_subscription_name(id: i32) -> Option<&'static str> {
    DEBUG_SUBSCRIPTION_ENTRIES
        .get(usize::try_from(id).ok()?)
        .copied()
}

/// The `debug_subscription` id for an identifier, or `None` if unknown.
#[must_use]
pub fn debug_subscription_id(name: &str) -> Option<i32> {
    DEBUG_SUBSCRIPTION_ENTRIES
        .iter()
        .position(|entry| *entry == name)
        .and_then(|index| i32::try_from(index).ok())
}
