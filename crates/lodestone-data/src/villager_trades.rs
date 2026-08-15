//! Villager per-profession, per-level trade tables — issue #245's data half.
//!
//! Transcribed from the real 26.2 registry data under
//! `data/minecraft/{villager_trade,trade_set,tags/villager_trade}` — Mojang's
//! own generator output, not the old hardcoded `VillagerTrades.java`
//! `ItemListing[]` factories (26.2 replaced them with plain JSON records;
//! following this crate's own data-source rule, that JSON is transcribed
//! directly rather than routed through the decompiled Java, which only holds
//! the *codec* for these records, not the table). Every profession with a
//! real workstation (thirteen; `none`/`nitwit` have none) is covered across
//! all five levels — not just the one profession a prior pass on this issue
//! ported.
//!
//! # What is ported, and what is not
//!
//! Every trade record whose full behaviour is `wants(+wants_b) -> gives` is
//! transcribed verbatim from its own JSON file, cited in a comment above it
//! (bare resource path, e.g. `minecraft:farmer/1/wheat_emerald`). Eighteen
//! records across the whole table use a `given_item_modifiers` function to
//! compute part of their result at runtime — enchanted books (librarian),
//! cartographer treasure/explorer maps, and two enchanted-weapon/tipped-arrow
//! records — and are **not** ported: this crate has no port of the
//! enchantment-selection or loot-table machinery those functions call, and a
//! plain/default output (e.g. the `"wants": {"count": 0}` placeholder several
//! of them carry, meant to be overwritten at generation time) would be a
//! wrong number, not an absent one. This repo's evidence standard prefers an
//! honest gap: [`pool_for`] simply excludes these records from the resolved
//! pool, and each level's own doc comment names exactly which ids were
//! skipped and why. Two profession/levels (`armorer`/`toolsmith`/`weaponsmith`
//! at level 5, `armorer` at level 4 additionally) resolve to an entirely
//! skipped pool and [`pool_for`] answers `None` for them, same as an
//! unrecognised profession.
//!
//! # Selection: pool order, not vanilla's seeded RNG
//!
//! A real `TradeSet` draws `amount` trades **at random**, without
//! replacement (every one of these `trade_set/*/level_*.json` records omits
//! `allow_duplicates`, whose codec default is `false`), from its tag's
//! resolved pool, seeded per-position through vanilla's `RandomSequence`
//! (`WorldGenRandom` keyed by `random_sequence` + world seed) — a whole RNG
//! subsystem this crate has no port of. [`pool_for`] hands back the *whole*
//! resolved pool plus the tag's own `amount`, in the tag's declared order,
//! and leaves the actual pick to the caller. Every number in the table below
//! is the real jar value; only the *subset* a villager ends up offering will
//! not match a real vanilla server seeded identically — the same disclosed
//! simplification the original farmer-only port made, now stated once here
//! instead of per profession.
//!
//! # Tag composition: `common_smith`
//!
//! `armorer`/`toolsmith`/`weaponsmith`'s level tags each include a nested
//! `#minecraft:common_smith/level_N` tag — all three "smith" professions
//! sell the same coal/iron-ingot/bell trades, stored once under
//! `data/minecraft/villager_trade/smith/` rather than duplicated per
//! profession. That nesting is resolved once here, so [`pool_for`]'s pool
//! for those three professions already contains the shared `smith/*` entries
//! in the tag's own resolved order (smith entries first, matching the tag's
//! own `values` list) — there is no separate "smith" profession, and no
//! caller needs to know the table is composed this way.

/// One `VillagerTrade` record: what a villager wants (one or two items) for
/// what it gives, and the record's own use/xp limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeRecord {
    /// The primary cost — always present. A full `minecraft:*` resource id.
    pub wants_item: &'static str,
    pub wants_count: i32,
    /// The record's `additional_wants`, when the trade has a second cost:
    /// three records in the whole table (two fisherman raw-fish-plus-emerald
    /// trades, one fletcher `gravel_and_emerald_flint`) — `(item, count)`.
    /// `None` for every other record.
    pub wants_b: Option<(&'static str, i32)>,
    /// A full `minecraft:*` resource id.
    pub gives_item: &'static str,
    pub gives_count: i32,
    pub max_uses: i32,
    /// Codec default is `1`, not `0`, when the record's own JSON omits the
    /// `xp` key (`NumberProviders.CODEC.lenientOptionalFieldOf("xp",
    /// ConstantValue.exactly(1.0F))`) — already resolved at transcription
    /// time below, not deferred to a runtime default.
    pub xp: i32,
}

/// `tags/villager_trade/armorer/level_1.json` resolves to: `smith/1/coal_emerald`, `armorer/1/emerald_iron_leggings`, `armorer/1/emerald_iron_boots`, `armorer/1/emerald_iron_helmet`, `armorer/1/emerald_iron_chestplate`.
/// `trade_set/armorer/level_1.json`'s `amount` is `2`.
const ARMORER_LEVEL_1: &[TradeRecord] = &[
    // smith/1/coal_emerald
    TradeRecord {
        wants_item: "minecraft:coal",
        wants_count: 15,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // armorer/1/emerald_iron_leggings
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 7,
        wants_b: None,
        gives_item: "minecraft:iron_leggings",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
    // armorer/1/emerald_iron_boots
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:iron_boots",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
    // armorer/1/emerald_iron_helmet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 5,
        wants_b: None,
        gives_item: "minecraft:iron_helmet",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
    // armorer/1/emerald_iron_chestplate
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 9,
        wants_b: None,
        gives_item: "minecraft:iron_chestplate",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
];

/// `tags/villager_trade/armorer/level_2.json` resolves to: `smith/2/iron_ingot_emerald`, `smith/2/emerald_bell`, `armorer/2/emerald_chainmail_boots`, `armorer/2/emerald_chainmail_leggings`.
/// `trade_set/armorer/level_2.json`'s `amount` is `2`.
const ARMORER_LEVEL_2: &[TradeRecord] = &[
    // smith/2/iron_ingot_emerald
    TradeRecord {
        wants_item: "minecraft:iron_ingot",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // smith/2/emerald_bell
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 36,
        wants_b: None,
        gives_item: "minecraft:bell",
        gives_count: 1,
        max_uses: 12,
        xp: 5,
    },
    // armorer/2/emerald_chainmail_boots
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:chainmail_boots",
        gives_count: 1,
        max_uses: 12,
        xp: 5,
    },
    // armorer/2/emerald_chainmail_leggings
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:chainmail_leggings",
        gives_count: 1,
        max_uses: 12,
        xp: 5,
    },
];

/// `tags/villager_trade/armorer/level_3.json` resolves to: `armorer/3/lava_bucket_emerald`, `armorer/3/emerald_chainmail_helmet`, `armorer/3/emerald_chainmail_chestplate`, `armorer/3/emerald_shield`, `armorer/3/diamond_emerald`.
/// `trade_set/armorer/level_3.json`'s `amount` is `2`.
const ARMORER_LEVEL_3: &[TradeRecord] = &[
    // armorer/3/lava_bucket_emerald
    TradeRecord {
        wants_item: "minecraft:lava_bucket",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 20,
    },
    // armorer/3/emerald_chainmail_helmet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:chainmail_helmet",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // armorer/3/emerald_chainmail_chestplate
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:chainmail_chestplate",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // armorer/3/emerald_shield
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 5,
        wants_b: None,
        gives_item: "minecraft:shield",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // armorer/3/diamond_emerald
    TradeRecord {
        wants_item: "minecraft:diamond",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 20,
    },
];

// armorer level 4: no portable trades in the resolved tag (all 2 record(s) use given_item_modifiers: ['armorer/4/emerald_enchanted_diamond_leggings', 'armorer/4/emerald_enchanted_diamond_boots']).
// armorer level 5: no portable trades in the resolved tag (all 2 record(s) use given_item_modifiers: ['armorer/5/emerald_enchanted_diamond_helmet', 'armorer/5/emerald_enchanted_diamond_chestplate']).
/// `tags/villager_trade/butcher/level_1.json` resolves to: `butcher/1/chicken_emerald`, `butcher/1/porkchop_emerald`, `butcher/1/rabbit_emerald`, `butcher/1/emerald_rabbit_stew`.
/// `trade_set/butcher/level_1.json`'s `amount` is `2`.
const BUTCHER_LEVEL_1: &[TradeRecord] = &[
    // butcher/1/chicken_emerald
    TradeRecord {
        wants_item: "minecraft:chicken",
        wants_count: 14,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // butcher/1/porkchop_emerald
    TradeRecord {
        wants_item: "minecraft:porkchop",
        wants_count: 7,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // butcher/1/rabbit_emerald
    TradeRecord {
        wants_item: "minecraft:rabbit",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // butcher/1/emerald_rabbit_stew
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:rabbit_stew",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
];

/// `tags/villager_trade/butcher/level_2.json` resolves to: `butcher/2/coal_emerald`, `butcher/2/emerald_cooked_porkchop`, `butcher/2/emerald_cooked_chicken`.
/// `trade_set/butcher/level_2.json`'s `amount` is `2`.
const BUTCHER_LEVEL_2: &[TradeRecord] = &[
    // butcher/2/coal_emerald
    TradeRecord {
        wants_item: "minecraft:coal",
        wants_count: 15,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // butcher/2/emerald_cooked_porkchop
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:cooked_porkchop",
        gives_count: 5,
        max_uses: 16,
        xp: 5,
    },
    // butcher/2/emerald_cooked_chicken
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:cooked_chicken",
        gives_count: 8,
        max_uses: 16,
        xp: 5,
    },
];

/// `tags/villager_trade/butcher/level_3.json` resolves to: `butcher/3/mutton_emerald`, `butcher/3/beef_emerald`.
/// `trade_set/butcher/level_3.json`'s `amount` is `2`.
const BUTCHER_LEVEL_3: &[TradeRecord] = &[
    // butcher/3/mutton_emerald
    TradeRecord {
        wants_item: "minecraft:mutton",
        wants_count: 7,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
    // butcher/3/beef_emerald
    TradeRecord {
        wants_item: "minecraft:beef",
        wants_count: 10,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
];

/// `tags/villager_trade/butcher/level_4.json` resolves to: `butcher/4/dried_kelp_block_emerald`.
/// `trade_set/butcher/level_4.json`'s `amount` is `2`.
const BUTCHER_LEVEL_4: &[TradeRecord] = &[
    // butcher/4/dried_kelp_block_emerald
    TradeRecord {
        wants_item: "minecraft:dried_kelp_block",
        wants_count: 10,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/butcher/level_5.json` resolves to: `butcher/5/sweet_berries_emerald`.
/// `trade_set/butcher/level_5.json`'s `amount` is `2`.
const BUTCHER_LEVEL_5: &[TradeRecord] = &[
    // butcher/5/sweet_berries_emerald
    TradeRecord {
        wants_item: "minecraft:sweet_berries",
        wants_count: 10,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/cartographer/level_1.json` resolves to: `cartographer/1/paper_emerald`, `cartographer/1/emerald_map`.
/// `trade_set/cartographer/level_1.json`'s `amount` is `2`.
const CARTOGRAPHER_LEVEL_1: &[TradeRecord] = &[
    // cartographer/1/paper_emerald
    TradeRecord {
        wants_item: "minecraft:paper",
        wants_count: 24,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 2,
    },
    // cartographer/1/emerald_map
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 7,
        wants_b: None,
        gives_item: "minecraft:map",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
];

/// `tags/villager_trade/cartographer/level_2.json` resolves to: `cartographer/2/glass_pane_emerald`.
/// `trade_set/cartographer/level_2.json`'s `amount` is `2`. Skipped (7, `given_item_modifiers`): ['cartographer/2/emerald_and_compass_village_taiga_map', 'cartographer/2/emerald_and_compass_explorer_swamp_map', 'cartographer/2/emerald_and_compass_village_snowy_map', 'cartographer/2/emerald_and_compass_village_savanna_map', 'cartographer/2/emerald_and_compass_village_plains_map', 'cartographer/2/emerald_and_compass_explorer_jungle_map', 'cartographer/2/emerald_and_compass_village_desert_map'].
const CARTOGRAPHER_LEVEL_2: &[TradeRecord] = &[
    // cartographer/2/glass_pane_emerald
    TradeRecord {
        wants_item: "minecraft:glass_pane",
        wants_count: 11,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
];

/// `tags/villager_trade/cartographer/level_3.json` resolves to: `cartographer/3/compass_emerald`.
/// `trade_set/cartographer/level_3.json`'s `amount` is `2`. Skipped (2, `given_item_modifiers`): ['cartographer/3/emerald_and_compass_ocean_explorer_map', 'cartographer/3/emerald_and_compass_trial_chamber_map'].
const CARTOGRAPHER_LEVEL_3: &[TradeRecord] = &[
    // cartographer/3/compass_emerald
    TradeRecord {
        wants_item: "minecraft:compass",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 20,
    },
];

/// `tags/villager_trade/cartographer/level_4.json` resolves to: `cartographer/4/emerald_item_frame`, `cartographer/4/emerald_white_banner`, `cartographer/4/emerald_orange_banner`, `cartographer/4/emerald_magenta_banner`, `cartographer/4/emerald_blue_banner`, `cartographer/4/emerald_light_blue_banner`, `cartographer/4/emerald_yellow_banner`, `cartographer/4/emerald_lime_banner`, `cartographer/4/emerald_pink_banner`, `cartographer/4/emerald_gray_banner`, `cartographer/4/emerald_cyan_banner`, `cartographer/4/emerald_purple_banner`, `cartographer/4/emerald_brown_banner`, `cartographer/4/emerald_green_banner`, `cartographer/4/emerald_red_banner`, `cartographer/4/emerald_black_banner`.
/// `trade_set/cartographer/level_4.json`'s `amount` is `2`.
const CARTOGRAPHER_LEVEL_4: &[TradeRecord] = &[
    // cartographer/4/emerald_item_frame
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 7,
        wants_b: None,
        gives_item: "minecraft:item_frame",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_white_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:white_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_orange_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:orange_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_magenta_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:magenta_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_blue_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:blue_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_light_blue_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:light_blue_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_yellow_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:yellow_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_lime_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:lime_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_pink_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:pink_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_gray_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:gray_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_cyan_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:cyan_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_purple_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:purple_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_brown_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:brown_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_green_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:green_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_red_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:red_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // cartographer/4/emerald_black_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:black_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
];

/// `tags/villager_trade/cartographer/level_5.json` resolves to: `cartographer/5/emerald_globe_banner_pattern`.
/// `trade_set/cartographer/level_5.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['cartographer/5/emerald_and_compass_woodland_mansion_map'].
const CARTOGRAPHER_LEVEL_5: &[TradeRecord] = &[
    // cartographer/5/emerald_globe_banner_pattern
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 8,
        wants_b: None,
        gives_item: "minecraft:globe_banner_pattern",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/cleric/level_1.json` resolves to: `cleric/1/rotten_flesh_emerald`, `cleric/1/emerald_redstone`.
/// `trade_set/cleric/level_1.json`'s `amount` is `2`.
const CLERIC_LEVEL_1: &[TradeRecord] = &[
    // cleric/1/rotten_flesh_emerald
    TradeRecord {
        wants_item: "minecraft:rotten_flesh",
        wants_count: 32,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // cleric/1/emerald_redstone
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:redstone",
        gives_count: 2,
        max_uses: 12,
        xp: 1,
    },
];

/// `tags/villager_trade/cleric/level_2.json` resolves to: `cleric/2/gold_ingot_emerald`, `cleric/2/emerald_lapis_lazuli`.
/// `trade_set/cleric/level_2.json`'s `amount` is `2`.
const CLERIC_LEVEL_2: &[TradeRecord] = &[
    // cleric/2/gold_ingot_emerald
    TradeRecord {
        wants_item: "minecraft:gold_ingot",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // cleric/2/emerald_lapis_lazuli
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:lapis_lazuli",
        gives_count: 1,
        max_uses: 12,
        xp: 5,
    },
];

/// `tags/villager_trade/cleric/level_3.json` resolves to: `cleric/3/rabbit_foot_emerald`, `cleric/3/emerald_glowstone`.
/// `trade_set/cleric/level_3.json`'s `amount` is `2`.
const CLERIC_LEVEL_3: &[TradeRecord] = &[
    // cleric/3/rabbit_foot_emerald
    TradeRecord {
        wants_item: "minecraft:rabbit_foot",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 20,
    },
    // cleric/3/emerald_glowstone
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:glowstone",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
];

/// `tags/villager_trade/cleric/level_4.json` resolves to: `cleric/4/turtle_scute_emerald`, `cleric/4/glass_bottle_emerald`, `cleric/4/emerald_ender_pearl`.
/// `trade_set/cleric/level_4.json`'s `amount` is `2`.
const CLERIC_LEVEL_4: &[TradeRecord] = &[
    // cleric/4/turtle_scute_emerald
    TradeRecord {
        wants_item: "minecraft:turtle_scute",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // cleric/4/glass_bottle_emerald
    TradeRecord {
        wants_item: "minecraft:glass_bottle",
        wants_count: 9,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // cleric/4/emerald_ender_pearl
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 5,
        wants_b: None,
        gives_item: "minecraft:ender_pearl",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
];

/// `tags/villager_trade/cleric/level_5.json` resolves to: `cleric/5/nether_wart_emerald`, `cleric/5/emerald_experience_bottle`.
/// `trade_set/cleric/level_5.json`'s `amount` is `2`.
const CLERIC_LEVEL_5: &[TradeRecord] = &[
    // cleric/5/nether_wart_emerald
    TradeRecord {
        wants_item: "minecraft:nether_wart",
        wants_count: 22,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // cleric/5/emerald_experience_bottle
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:experience_bottle",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/farmer/level_1.json` resolves to: `farmer/1/wheat_emerald`, `farmer/1/potato_emerald`, `farmer/1/carrot_emerald`, `farmer/1/beetroot_emerald`, `farmer/1/emerald_bread`.
/// `trade_set/farmer/level_1.json`'s `amount` is `2`.
const FARMER_LEVEL_1: &[TradeRecord] = &[
    // farmer/1/wheat_emerald
    TradeRecord {
        wants_item: "minecraft:wheat",
        wants_count: 20,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // farmer/1/potato_emerald
    TradeRecord {
        wants_item: "minecraft:potato",
        wants_count: 26,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // farmer/1/carrot_emerald
    TradeRecord {
        wants_item: "minecraft:carrot",
        wants_count: 22,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // farmer/1/beetroot_emerald
    TradeRecord {
        wants_item: "minecraft:beetroot",
        wants_count: 15,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // farmer/1/emerald_bread
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:bread",
        gives_count: 6,
        max_uses: 16,
        xp: 1,
    },
];

/// `tags/villager_trade/farmer/level_2.json` resolves to: `farmer/2/pumpkin_emerald`, `farmer/2/emerald_pumpkin_pie`, `farmer/2/emerald_apple`.
/// `trade_set/farmer/level_2.json`'s `amount` is `2`.
const FARMER_LEVEL_2: &[TradeRecord] = &[
    // farmer/2/pumpkin_emerald
    TradeRecord {
        wants_item: "minecraft:pumpkin",
        wants_count: 6,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // farmer/2/emerald_pumpkin_pie
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:pumpkin_pie",
        gives_count: 4,
        max_uses: 12,
        xp: 5,
    },
    // farmer/2/emerald_apple
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:apple",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
];

/// `tags/villager_trade/farmer/level_3.json` resolves to: `farmer/3/emerald_cookie`, `farmer/3/melon_emerald`.
/// `trade_set/farmer/level_3.json`'s `amount` is `2`.
const FARMER_LEVEL_3: &[TradeRecord] = &[
    // farmer/3/emerald_cookie
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:cookie",
        gives_count: 18,
        max_uses: 12,
        xp: 10,
    },
    // farmer/3/melon_emerald
    TradeRecord {
        wants_item: "minecraft:melon",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 20,
    },
];

/// `tags/villager_trade/farmer/level_4.json` resolves to: `farmer/4/emerald_cake`.
/// `trade_set/farmer/level_4.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['farmer/4/emerald_suspicious_stew'].
const FARMER_LEVEL_4: &[TradeRecord] = &[
    // farmer/4/emerald_cake
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:cake",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
];

/// `tags/villager_trade/farmer/level_5.json` resolves to: `farmer/5/emerald_golden_carrot`, `farmer/5/emerald_glistening_melon_slice`.
/// `trade_set/farmer/level_5.json`'s `amount` is `2`.
const FARMER_LEVEL_5: &[TradeRecord] = &[
    // farmer/5/emerald_golden_carrot
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:golden_carrot",
        gives_count: 3,
        max_uses: 12,
        xp: 30,
    },
    // farmer/5/emerald_glistening_melon_slice
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:glistering_melon_slice",
        gives_count: 3,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/fisherman/level_1.json` resolves to: `fisherman/1/string_emerald`, `fisherman/1/coal_emerald`, `fisherman/1/raw_cod_and_emerald_cooked_cod`, `fisherman/1/emerald_cod_bucket`.
/// `trade_set/fisherman/level_1.json`'s `amount` is `2`.
const FISHERMAN_LEVEL_1: &[TradeRecord] = &[
    // fisherman/1/string_emerald
    TradeRecord {
        wants_item: "minecraft:string",
        wants_count: 20,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // fisherman/1/coal_emerald
    TradeRecord {
        wants_item: "minecraft:coal",
        wants_count: 10,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // fisherman/1/raw_cod_and_emerald_cooked_cod
    TradeRecord {
        wants_item: "minecraft:cod",
        wants_count: 6,
        wants_b: Some(("minecraft:emerald", 1)),
        gives_item: "minecraft:cooked_cod",
        gives_count: 6,
        max_uses: 16,
        xp: 1,
    },
    // fisherman/1/emerald_cod_bucket
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:cod_bucket",
        gives_count: 1,
        max_uses: 16,
        xp: 1,
    },
];

/// `tags/villager_trade/fisherman/level_2.json` resolves to: `fisherman/2/cod_emerald`, `fisherman/2/salmon_and_emerald_cooked_salmon`, `fisherman/2/emerald_campfire`.
/// `trade_set/fisherman/level_2.json`'s `amount` is `2`.
const FISHERMAN_LEVEL_2: &[TradeRecord] = &[
    // fisherman/2/cod_emerald
    TradeRecord {
        wants_item: "minecraft:cod",
        wants_count: 15,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 10,
    },
    // fisherman/2/salmon_and_emerald_cooked_salmon
    TradeRecord {
        wants_item: "minecraft:salmon",
        wants_count: 6,
        wants_b: Some(("minecraft:emerald", 1)),
        gives_item: "minecraft:cooked_salmon",
        gives_count: 6,
        max_uses: 16,
        xp: 5,
    },
    // fisherman/2/emerald_campfire
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:campfire",
        gives_count: 1,
        max_uses: 12,
        xp: 5,
    },
];

/// `tags/villager_trade/fisherman/level_3.json` resolves to: `fisherman/3/salmon_emerald`.
/// `trade_set/fisherman/level_3.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['fisherman/3/emerald_enchanted_fishing_rod'].
const FISHERMAN_LEVEL_3: &[TradeRecord] = &[
    // fisherman/3/salmon_emerald
    TradeRecord {
        wants_item: "minecraft:salmon",
        wants_count: 13,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
];

/// `tags/villager_trade/fisherman/level_4.json` resolves to: `fisherman/4/tropical_fish_emerald`.
/// `trade_set/fisherman/level_4.json`'s `amount` is `2`.
const FISHERMAN_LEVEL_4: &[TradeRecord] = &[
    // fisherman/4/tropical_fish_emerald
    TradeRecord {
        wants_item: "minecraft:tropical_fish",
        wants_count: 6,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/fisherman/level_5.json` resolves to: `fisherman/5/pufferfish_emerald`, `fisherman/5/oak_boat_emerald`, `fisherman/5/spruce_boat_emerald`, `fisherman/5/jungle_boat_emerald`, `fisherman/5/acacia_boat_emerald`, `fisherman/5/dark_oak_boat_emerald`.
/// `trade_set/fisherman/level_5.json`'s `amount` is `2`.
const FISHERMAN_LEVEL_5: &[TradeRecord] = &[
    // fisherman/5/pufferfish_emerald
    TradeRecord {
        wants_item: "minecraft:pufferfish",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // fisherman/5/oak_boat_emerald
    TradeRecord {
        wants_item: "minecraft:oak_boat",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // fisherman/5/spruce_boat_emerald
    TradeRecord {
        wants_item: "minecraft:spruce_boat",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // fisherman/5/jungle_boat_emerald
    TradeRecord {
        wants_item: "minecraft:jungle_boat",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // fisherman/5/acacia_boat_emerald
    TradeRecord {
        wants_item: "minecraft:acacia_boat",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // fisherman/5/dark_oak_boat_emerald
    TradeRecord {
        wants_item: "minecraft:dark_oak_boat",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/fletcher/level_1.json` resolves to: `fletcher/1/stick_emerald`, `fletcher/1/emerald_arrow`, `fletcher/1/gravel_and_emerald_flint`.
/// `trade_set/fletcher/level_1.json`'s `amount` is `2`.
const FLETCHER_LEVEL_1: &[TradeRecord] = &[
    // fletcher/1/stick_emerald
    TradeRecord {
        wants_item: "minecraft:stick",
        wants_count: 32,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // fletcher/1/emerald_arrow
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:arrow",
        gives_count: 16,
        max_uses: 12,
        xp: 1,
    },
    // fletcher/1/gravel_and_emerald_flint
    TradeRecord {
        wants_item: "minecraft:gravel",
        wants_count: 10,
        wants_b: Some(("minecraft:emerald", 1)),
        gives_item: "minecraft:flint",
        gives_count: 10,
        max_uses: 12,
        xp: 1,
    },
];

/// `tags/villager_trade/fletcher/level_2.json` resolves to: `fletcher/2/flint_emerald`, `fletcher/2/emerald_bow`.
/// `trade_set/fletcher/level_2.json`'s `amount` is `2`.
const FLETCHER_LEVEL_2: &[TradeRecord] = &[
    // fletcher/2/flint_emerald
    TradeRecord {
        wants_item: "minecraft:flint",
        wants_count: 26,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // fletcher/2/emerald_bow
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:bow",
        gives_count: 1,
        max_uses: 12,
        xp: 5,
    },
];

/// `tags/villager_trade/fletcher/level_3.json` resolves to: `fletcher/3/string_emerald`, `fletcher/3/emerald_crossbow`.
/// `trade_set/fletcher/level_3.json`'s `amount` is `2`.
const FLETCHER_LEVEL_3: &[TradeRecord] = &[
    // fletcher/3/string_emerald
    TradeRecord {
        wants_item: "minecraft:string",
        wants_count: 14,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
    // fletcher/3/emerald_crossbow
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:crossbow",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
];

/// `tags/villager_trade/fletcher/level_4.json` resolves to: `fletcher/4/feather_emerald`.
/// `trade_set/fletcher/level_4.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['fletcher/4/emerald_enchanted_bow'].
const FLETCHER_LEVEL_4: &[TradeRecord] = &[
    // fletcher/4/feather_emerald
    TradeRecord {
        wants_item: "minecraft:feather",
        wants_count: 24,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 30,
    },
];

/// `tags/villager_trade/fletcher/level_5.json` resolves to: `fletcher/5/tripwire_hook_emerald`.
/// `trade_set/fletcher/level_5.json`'s `amount` is `2`. Skipped (2, `given_item_modifiers`): ['fletcher/5/emerald_enchanted_crossbow', 'fletcher/5/arrow_and_emerald_tipped_arrow'].
const FLETCHER_LEVEL_5: &[TradeRecord] = &[
    // fletcher/5/tripwire_hook_emerald
    TradeRecord {
        wants_item: "minecraft:tripwire_hook",
        wants_count: 8,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/leatherworker/level_1.json` resolves to: `leatherworker/1/leather_emerald`.
/// `trade_set/leatherworker/level_1.json`'s `amount` is `2`. Skipped (2, `given_item_modifiers`): ['leatherworker/1/emerald_dyed_leather_leggings', 'leatherworker/1/emerald_dyed_leather_chestplate'].
const LEATHERWORKER_LEVEL_1: &[TradeRecord] = &[
    // leatherworker/1/leather_emerald
    TradeRecord {
        wants_item: "minecraft:leather",
        wants_count: 6,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
];

/// `tags/villager_trade/leatherworker/level_2.json` resolves to: `leatherworker/2/flint_emerald`.
/// `trade_set/leatherworker/level_2.json`'s `amount` is `2`. Skipped (2, `given_item_modifiers`): ['leatherworker/2/emerald_dyed_leather_helmet', 'leatherworker/2/emerald_dyed_leather_boots'].
const LEATHERWORKER_LEVEL_2: &[TradeRecord] = &[
    // leatherworker/2/flint_emerald
    TradeRecord {
        wants_item: "minecraft:flint",
        wants_count: 26,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
];

/// `tags/villager_trade/leatherworker/level_3.json` resolves to: `leatherworker/3/rabbit_hide_emerald`.
/// `trade_set/leatherworker/level_3.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['leatherworker/3/emerald_dyed_leather_chestplate'].
const LEATHERWORKER_LEVEL_3: &[TradeRecord] = &[
    // leatherworker/3/rabbit_hide_emerald
    TradeRecord {
        wants_item: "minecraft:rabbit_hide",
        wants_count: 9,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 20,
    },
];

/// `tags/villager_trade/leatherworker/level_4.json` resolves to: `leatherworker/4/turtle_scute_emerald`.
/// `trade_set/leatherworker/level_4.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['leatherworker/4/emerald_dyed_leather_horse_armor'].
const LEATHERWORKER_LEVEL_4: &[TradeRecord] = &[
    // leatherworker/4/turtle_scute_emerald
    TradeRecord {
        wants_item: "minecraft:turtle_scute",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/leatherworker/level_5.json` resolves to: `leatherworker/5/emerald_saddle`.
/// `trade_set/leatherworker/level_5.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['leatherworker/5/emerald_dyed_leather_helmet'].
const LEATHERWORKER_LEVEL_5: &[TradeRecord] = &[
    // leatherworker/5/emerald_saddle
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 6,
        wants_b: None,
        gives_item: "minecraft:saddle",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/librarian/level_1.json` resolves to: `librarian/1/paper_emerald`, `librarian/1/emerald_bookshelf`.
/// `trade_set/librarian/level_1.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['librarian/1/emerald_and_book_enchanted_book'].
const LIBRARIAN_LEVEL_1: &[TradeRecord] = &[
    // librarian/1/paper_emerald
    TradeRecord {
        wants_item: "minecraft:paper",
        wants_count: 24,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // librarian/1/emerald_bookshelf
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 9,
        wants_b: None,
        gives_item: "minecraft:bookshelf",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
];

/// `tags/villager_trade/librarian/level_2.json` resolves to: `librarian/2/book_emerald`, `librarian/2/emerald_lantern`.
/// `trade_set/librarian/level_2.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['librarian/2/emerald_and_book_enchanted_book'].
const LIBRARIAN_LEVEL_2: &[TradeRecord] = &[
    // librarian/2/book_emerald
    TradeRecord {
        wants_item: "minecraft:book",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // librarian/2/emerald_lantern
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:lantern",
        gives_count: 1,
        max_uses: 12,
        xp: 5,
    },
];

/// `tags/villager_trade/librarian/level_3.json` resolves to: `librarian/3/ink_sac_emerald`, `librarian/3/emerald_glass`.
/// `trade_set/librarian/level_3.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['librarian/3/emerald_and_book_enchanted_book'].
const LIBRARIAN_LEVEL_3: &[TradeRecord] = &[
    // librarian/3/ink_sac_emerald
    TradeRecord {
        wants_item: "minecraft:ink_sac",
        wants_count: 5,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 20,
    },
    // librarian/3/emerald_glass
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:glass",
        gives_count: 4,
        max_uses: 12,
        xp: 10,
    },
];

/// `tags/villager_trade/librarian/level_4.json` resolves to: `librarian/4/writable_book_emerald`, `librarian/4/emerald_clock`, `librarian/4/emerald_compass`.
/// `trade_set/librarian/level_4.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['librarian/4/emerald_book_and_enchanted_book'].
const LIBRARIAN_LEVEL_4: &[TradeRecord] = &[
    // librarian/4/writable_book_emerald
    TradeRecord {
        wants_item: "minecraft:writable_book",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // librarian/4/emerald_clock
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 5,
        wants_b: None,
        gives_item: "minecraft:clock",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // librarian/4/emerald_compass
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:compass",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
];

/// `tags/villager_trade/librarian/level_5.json` resolves to: `librarian/5/emerald_yellow_candle`, `librarian/5/emerald_red_candle`.
/// `trade_set/librarian/level_5.json`'s `amount` is `3`.
const LIBRARIAN_LEVEL_5: &[TradeRecord] = &[
    // librarian/5/emerald_yellow_candle
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:yellow_candle",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // librarian/5/emerald_red_candle
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:red_candle",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/mason/level_1.json` resolves to: `mason/1/clay_ball_emerald`, `mason/1/emerald_brick`.
/// `trade_set/mason/level_1.json`'s `amount` is `2`.
const MASON_LEVEL_1: &[TradeRecord] = &[
    // mason/1/clay_ball_emerald
    TradeRecord {
        wants_item: "minecraft:clay_ball",
        wants_count: 10,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // mason/1/emerald_brick
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:brick",
        gives_count: 10,
        max_uses: 16,
        xp: 1,
    },
];

/// `tags/villager_trade/mason/level_2.json` resolves to: `mason/2/stone_emerald`, `mason/2/emerald_chiseled_stone_bricks`.
/// `trade_set/mason/level_2.json`'s `amount` is `2`.
const MASON_LEVEL_2: &[TradeRecord] = &[
    // mason/2/stone_emerald
    TradeRecord {
        wants_item: "minecraft:stone",
        wants_count: 20,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 10,
    },
    // mason/2/emerald_chiseled_stone_bricks
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:chiseled_stone_bricks",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
];

/// `tags/villager_trade/mason/level_3.json` resolves to: `mason/3/granite_emerald`, `mason/3/andesite_emerald`, `mason/3/diorite_emerald`, `mason/3/emerald_dripstone_block`, `mason/3/emerald_polished_andesite`, `mason/3/emerald_polished_diorite`, `mason/3/emerald_polished_granite`.
/// `trade_set/mason/level_3.json`'s `amount` is `2`.
const MASON_LEVEL_3: &[TradeRecord] = &[
    // mason/3/granite_emerald
    TradeRecord {
        wants_item: "minecraft:granite",
        wants_count: 16,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
    // mason/3/andesite_emerald
    TradeRecord {
        wants_item: "minecraft:andesite",
        wants_count: 16,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
    // mason/3/diorite_emerald
    TradeRecord {
        wants_item: "minecraft:diorite",
        wants_count: 16,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
    // mason/3/emerald_dripstone_block
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:dripstone_block",
        gives_count: 4,
        max_uses: 16,
        xp: 10,
    },
    // mason/3/emerald_polished_andesite
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:polished_andesite",
        gives_count: 4,
        max_uses: 16,
        xp: 10,
    },
    // mason/3/emerald_polished_diorite
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:polished_diorite",
        gives_count: 4,
        max_uses: 16,
        xp: 10,
    },
    // mason/3/emerald_polished_granite
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:polished_granite",
        gives_count: 4,
        max_uses: 16,
        xp: 10,
    },
];

/// `tags/villager_trade/mason/level_4.json` resolves to: `mason/4/quartz_emerald`, `mason/4/emerald_white_terracotta`, `mason/4/emerald_orange_terracotta`, `mason/4/emerald_magenta_terracotta`, `mason/4/emerald_light_blue_terracotta`, `mason/4/emerald_yellow_terracotta`, `mason/4/emerald_lime_terracotta`, `mason/4/emerald_pink_terracotta`, `mason/4/emerald_gray_terracotta`, `mason/4/emerald_light_gray_terracotta`, `mason/4/emerald_cyan_terracotta`, `mason/4/emerald_purple_terracotta`, `mason/4/emerald_blue_terracotta`, `mason/4/emerald_brown_terracotta`, `mason/4/emerald_green_terracotta`, `mason/4/emerald_red_terracotta`, `mason/4/emerald_black_terracotta`, `mason/4/emerald_white_glazed_terracotta`, `mason/4/emerald_orange_glazed_terracotta`, `mason/4/emerald_magenta_glazed_terracotta`, `mason/4/emerald_light_blue_glazed_terracotta`, `mason/4/emerald_yellow_glazed_terracotta`, `mason/4/emerald_lime_glazed_terracotta`, `mason/4/emerald_pink_glazed_terracotta`, `mason/4/emerald_gray_glazed_terracotta`, `mason/4/emerald_light_gray_glazed_terracotta`, `mason/4/emerald_cyan_glazed_terracotta`, `mason/4/emerald_purple_glazed_terracotta`, `mason/4/emerald_blue_glazed_terracotta`, `mason/4/emerald_brown_glazed_terracotta`, `mason/4/emerald_green_glazed_terracotta`, `mason/4/emerald_red_glazed_terracotta`, `mason/4/emerald_black_glazed_terracotta`.
/// `trade_set/mason/level_4.json`'s `amount` is `2`.
const MASON_LEVEL_4: &[TradeRecord] = &[
    // mason/4/quartz_emerald
    TradeRecord {
        wants_item: "minecraft:quartz",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // mason/4/emerald_white_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:white_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_orange_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:orange_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_magenta_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:magenta_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_light_blue_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:light_blue_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_yellow_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:yellow_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_lime_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:lime_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_pink_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:pink_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_gray_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:gray_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_light_gray_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:light_gray_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_cyan_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:cyan_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_purple_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:purple_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_blue_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:blue_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_brown_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:brown_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_green_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:green_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_red_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:red_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_black_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:black_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_white_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:white_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_orange_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:orange_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_magenta_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:magenta_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_light_blue_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:light_blue_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_yellow_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:yellow_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_lime_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:lime_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_pink_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:pink_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_gray_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:gray_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_light_gray_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:light_gray_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_cyan_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:cyan_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_purple_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:purple_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_blue_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:blue_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_brown_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:brown_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_green_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:green_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_red_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:red_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // mason/4/emerald_black_glazed_terracotta
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:black_glazed_terracotta",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
];

/// `tags/villager_trade/mason/level_5.json` resolves to: `mason/5/emerald_quartz_pillar`, `mason/5/emerald_quartz_block`.
/// `trade_set/mason/level_5.json`'s `amount` is `2`.
const MASON_LEVEL_5: &[TradeRecord] = &[
    // mason/5/emerald_quartz_pillar
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:quartz_pillar",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
    // mason/5/emerald_quartz_block
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:quartz_block",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/shepherd/level_1.json` resolves to: `shepherd/1/white_wool_emerald`, `shepherd/1/brown_wool_emerald`, `shepherd/1/gray_wool_emerald`, `shepherd/1/black_wool_emerald`, `shepherd/1/emerald_shears`.
/// `trade_set/shepherd/level_1.json`'s `amount` is `2`.
const SHEPHERD_LEVEL_1: &[TradeRecord] = &[
    // shepherd/1/white_wool_emerald
    TradeRecord {
        wants_item: "minecraft:white_wool",
        wants_count: 18,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // shepherd/1/brown_wool_emerald
    TradeRecord {
        wants_item: "minecraft:brown_wool",
        wants_count: 18,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // shepherd/1/gray_wool_emerald
    TradeRecord {
        wants_item: "minecraft:gray_wool",
        wants_count: 18,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // shepherd/1/black_wool_emerald
    TradeRecord {
        wants_item: "minecraft:black_wool",
        wants_count: 18,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // shepherd/1/emerald_shears
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:shears",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
];

/// `tags/villager_trade/shepherd/level_2.json` resolves to: `shepherd/2/white_dye_emerald`, `shepherd/2/gray_dye_emerald`, `shepherd/2/black_dye_emerald`, `shepherd/2/light_blue_dye_emerald`, `shepherd/2/lime_dye_emerald`, `shepherd/2/emerald_white_wool`, `shepherd/2/emerald_orange_wool`, `shepherd/2/emerald_magenta_wool`, `shepherd/2/emerald_light_blue_wool`, `shepherd/2/emerald_yellow_wool`, `shepherd/2/emerald_lime_wool`, `shepherd/2/emerald_pink_wool`, `shepherd/2/emerald_gray_wool`, `shepherd/2/emerald_light_gray_wool`, `shepherd/2/emerald_cyan_wool`, `shepherd/2/emerald_purple_wool`, `shepherd/2/emerald_blue_wool`, `shepherd/2/emerald_brown_wool`, `shepherd/2/emerald_green_wool`, `shepherd/2/emerald_red_wool`, `shepherd/2/emerald_black_wool`, `shepherd/2/emerald_white_carpet`, `shepherd/2/emerald_orange_carpet`, `shepherd/2/emerald_magenta_carpet`, `shepherd/2/emerald_light_blue_carpet`, `shepherd/2/emerald_yellow_carpet`, `shepherd/2/emerald_lime_carpet`, `shepherd/2/emerald_pink_carpet`, `shepherd/2/emerald_gray_carpet`, `shepherd/2/emerald_light_gray_carpet`, `shepherd/2/emerald_cyan_carpet`, `shepherd/2/emerald_purple_carpet`, `shepherd/2/emerald_blue_carpet`, `shepherd/2/emerald_brown_carpet`, `shepherd/2/emerald_green_carpet`, `shepherd/2/emerald_red_carpet`, `shepherd/2/emerald_black_carpet`.
/// `trade_set/shepherd/level_2.json`'s `amount` is `2`.
const SHEPHERD_LEVEL_2: &[TradeRecord] = &[
    // shepherd/2/white_dye_emerald
    TradeRecord {
        wants_item: "minecraft:white_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 10,
    },
    // shepherd/2/gray_dye_emerald
    TradeRecord {
        wants_item: "minecraft:gray_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 10,
    },
    // shepherd/2/black_dye_emerald
    TradeRecord {
        wants_item: "minecraft:black_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 10,
    },
    // shepherd/2/light_blue_dye_emerald
    TradeRecord {
        wants_item: "minecraft:light_blue_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 10,
    },
    // shepherd/2/lime_dye_emerald
    TradeRecord {
        wants_item: "minecraft:lime_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 10,
    },
    // shepherd/2/emerald_white_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:white_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_orange_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:orange_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_magenta_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:magenta_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_light_blue_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:light_blue_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_yellow_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:yellow_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_lime_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:lime_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_pink_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:pink_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_gray_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:gray_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_light_gray_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:light_gray_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_cyan_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:cyan_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_purple_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:purple_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_blue_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:blue_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_brown_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:brown_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_green_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:green_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_red_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:red_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_black_wool
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:black_wool",
        gives_count: 1,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_white_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:white_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_orange_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:orange_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_magenta_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:magenta_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_light_blue_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:light_blue_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_yellow_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:yellow_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_lime_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:lime_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_pink_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:pink_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_gray_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:gray_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_light_gray_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:light_gray_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_cyan_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:cyan_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_purple_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:purple_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_blue_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:blue_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_brown_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:brown_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_green_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:green_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_red_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:red_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
    // shepherd/2/emerald_black_carpet
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:black_carpet",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
];

/// `tags/villager_trade/shepherd/level_3.json` resolves to: `shepherd/3/yellow_dye_emerald`, `shepherd/3/light_gray_dye_emerald`, `shepherd/3/orange_dye_emerald`, `shepherd/3/red_dye_emerald`, `shepherd/3/pink_dye_emerald`, `shepherd/3/emerald_white_bed`, `shepherd/3/emerald_orange_bed`, `shepherd/3/emerald_magenta_bed`, `shepherd/3/emerald_light_blue_bed`, `shepherd/3/emerald_yellow_bed`, `shepherd/3/emerald_lime_bed`, `shepherd/3/emerald_pink_bed`, `shepherd/3/emerald_gray_bed`, `shepherd/3/emerald_light_gray_bed`, `shepherd/3/emerald_cyan_bed`, `shepherd/3/emerald_purple_bed`, `shepherd/3/emerald_blue_bed`, `shepherd/3/emerald_brown_bed`, `shepherd/3/emerald_green_bed`, `shepherd/3/emerald_red_bed`, `shepherd/3/emerald_black_bed`.
/// `trade_set/shepherd/level_3.json`'s `amount` is `2`.
const SHEPHERD_LEVEL_3: &[TradeRecord] = &[
    // shepherd/3/yellow_dye_emerald
    TradeRecord {
        wants_item: "minecraft:yellow_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
    // shepherd/3/light_gray_dye_emerald
    TradeRecord {
        wants_item: "minecraft:light_gray_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
    // shepherd/3/orange_dye_emerald
    TradeRecord {
        wants_item: "minecraft:orange_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
    // shepherd/3/red_dye_emerald
    TradeRecord {
        wants_item: "minecraft:red_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
    // shepherd/3/pink_dye_emerald
    TradeRecord {
        wants_item: "minecraft:pink_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 20,
    },
    // shepherd/3/emerald_white_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:white_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_orange_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:orange_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_magenta_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:magenta_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_light_blue_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:light_blue_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_yellow_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:yellow_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_lime_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:lime_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_pink_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:pink_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_gray_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:gray_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_light_gray_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:light_gray_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_cyan_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:cyan_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_purple_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:purple_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_blue_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:blue_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_brown_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:brown_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_green_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:green_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_red_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:red_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // shepherd/3/emerald_black_bed
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:black_bed",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
];

/// `tags/villager_trade/shepherd/level_4.json` resolves to: `shepherd/4/brown_dye_emerald`, `shepherd/4/purple_dye_emerald`, `shepherd/4/blue_dye_emerald`, `shepherd/4/green_dye_emerald`, `shepherd/4/magenta_dye_emerald`, `shepherd/4/cyan_dye_emerald`, `shepherd/4/emerald_white_banner`, `shepherd/4/emerald_orange_banner`, `shepherd/4/emerald_magenta_banner`, `shepherd/4/emerald_light_blue_banner`, `shepherd/4/emerald_yellow_banner`, `shepherd/4/emerald_lime_banner`, `shepherd/4/emerald_pink_banner`, `shepherd/4/emerald_gray_banner`, `shepherd/4/emerald_light_gray_banner`, `shepherd/4/emerald_cyan_banner`, `shepherd/4/emerald_purple_banner`, `shepherd/4/emerald_blue_banner`, `shepherd/4/emerald_brown_banner`, `shepherd/4/emerald_green_banner`, `shepherd/4/emerald_red_banner`, `shepherd/4/emerald_black_banner`.
/// `trade_set/shepherd/level_4.json`'s `amount` is `2`.
const SHEPHERD_LEVEL_4: &[TradeRecord] = &[
    // shepherd/4/brown_dye_emerald
    TradeRecord {
        wants_item: "minecraft:brown_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 30,
    },
    // shepherd/4/purple_dye_emerald
    TradeRecord {
        wants_item: "minecraft:purple_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 30,
    },
    // shepherd/4/blue_dye_emerald
    TradeRecord {
        wants_item: "minecraft:blue_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 30,
    },
    // shepherd/4/green_dye_emerald
    TradeRecord {
        wants_item: "minecraft:green_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 30,
    },
    // shepherd/4/magenta_dye_emerald
    TradeRecord {
        wants_item: "minecraft:magenta_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 30,
    },
    // shepherd/4/cyan_dye_emerald
    TradeRecord {
        wants_item: "minecraft:cyan_dye",
        wants_count: 12,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 30,
    },
    // shepherd/4/emerald_white_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:white_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_orange_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:orange_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_magenta_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:magenta_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_light_blue_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:light_blue_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_yellow_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:yellow_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_lime_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:lime_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_pink_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:pink_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_gray_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:gray_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_light_gray_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:light_gray_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_cyan_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:cyan_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_purple_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:purple_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_blue_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:blue_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_brown_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:brown_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_green_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:green_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_red_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:red_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // shepherd/4/emerald_black_banner
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:black_banner",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
];

/// `tags/villager_trade/shepherd/level_5.json` resolves to: `shepherd/5/emerald_painting`.
/// `trade_set/shepherd/level_5.json`'s `amount` is `2`.
const SHEPHERD_LEVEL_5: &[TradeRecord] = &[
    // shepherd/5/emerald_painting
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 2,
        wants_b: None,
        gives_item: "minecraft:painting",
        gives_count: 3,
        max_uses: 12,
        xp: 30,
    },
];

/// `tags/villager_trade/toolsmith/level_1.json` resolves to: `smith/1/coal_emerald`, `toolsmith/1/emerald_stone_axe`, `toolsmith/1/emerald_stone_shovel`, `toolsmith/1/emerald_stone_pickaxe`, `toolsmith/1/emerald_stone_hoe`.
/// `trade_set/toolsmith/level_1.json`'s `amount` is `2`.
const TOOLSMITH_LEVEL_1: &[TradeRecord] = &[
    // smith/1/coal_emerald
    TradeRecord {
        wants_item: "minecraft:coal",
        wants_count: 15,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // toolsmith/1/emerald_stone_axe
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:stone_axe",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
    // toolsmith/1/emerald_stone_shovel
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:stone_shovel",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
    // toolsmith/1/emerald_stone_pickaxe
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:stone_pickaxe",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
    // toolsmith/1/emerald_stone_hoe
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:stone_hoe",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
];

/// `tags/villager_trade/toolsmith/level_2.json` resolves to: `smith/2/iron_ingot_emerald`, `smith/2/emerald_bell`.
/// `trade_set/toolsmith/level_2.json`'s `amount` is `2`.
const TOOLSMITH_LEVEL_2: &[TradeRecord] = &[
    // smith/2/iron_ingot_emerald
    TradeRecord {
        wants_item: "minecraft:iron_ingot",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // smith/2/emerald_bell
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 36,
        wants_b: None,
        gives_item: "minecraft:bell",
        gives_count: 1,
        max_uses: 12,
        xp: 5,
    },
];

/// `tags/villager_trade/toolsmith/level_3.json` resolves to: `toolsmith/3/flint_emerald`, `toolsmith/3/emerald_diamond_hoe`.
/// `trade_set/toolsmith/level_3.json`'s `amount` is `2`. Skipped (3, `given_item_modifiers`): ['toolsmith/3/emerald_enchanted_iron_axe', 'toolsmith/3/emerald_enchanted_iron_shovel', 'toolsmith/3/emerald_enchanted_iron_pickaxe'].
const TOOLSMITH_LEVEL_3: &[TradeRecord] = &[
    // toolsmith/3/flint_emerald
    TradeRecord {
        wants_item: "minecraft:flint",
        wants_count: 30,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 20,
    },
    // toolsmith/3/emerald_diamond_hoe
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:diamond_hoe",
        gives_count: 1,
        max_uses: 3,
        xp: 10,
    },
];

/// `tags/villager_trade/toolsmith/level_4.json` resolves to: `toolsmith/4/diamond_emerald`.
/// `trade_set/toolsmith/level_4.json`'s `amount` is `2`. Skipped (2, `given_item_modifiers`): ['toolsmith/4/emerald_enchanted_diamond_axe', 'toolsmith/4/emerald_enchanted_diamond_shovel'].
const TOOLSMITH_LEVEL_4: &[TradeRecord] = &[
    // toolsmith/4/diamond_emerald
    TradeRecord {
        wants_item: "minecraft:diamond",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

// toolsmith level 5: no portable trades in the resolved tag (all 1 record(s) use given_item_modifiers: ['toolsmith/5/emerald_enchanted_diamond_pickaxe']).
/// `tags/villager_trade/weaponsmith/level_1.json` resolves to: `smith/1/coal_emerald`, `weaponsmith/1/emerald_iron_axe`.
/// `trade_set/weaponsmith/level_1.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['weaponsmith/1/emerald_enchanted_iron_sword'].
const WEAPONSMITH_LEVEL_1: &[TradeRecord] = &[
    // smith/1/coal_emerald
    TradeRecord {
        wants_item: "minecraft:coal",
        wants_count: 15,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // weaponsmith/1/emerald_iron_axe
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        wants_b: None,
        gives_item: "minecraft:iron_axe",
        gives_count: 1,
        max_uses: 12,
        xp: 1,
    },
];

/// `tags/villager_trade/weaponsmith/level_2.json` resolves to: `smith/2/iron_ingot_emerald`, `smith/2/emerald_bell`.
/// `trade_set/weaponsmith/level_2.json`'s `amount` is `2`.
const WEAPONSMITH_LEVEL_2: &[TradeRecord] = &[
    // smith/2/iron_ingot_emerald
    TradeRecord {
        wants_item: "minecraft:iron_ingot",
        wants_count: 4,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // smith/2/emerald_bell
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 36,
        wants_b: None,
        gives_item: "minecraft:bell",
        gives_count: 1,
        max_uses: 12,
        xp: 5,
    },
];

/// `tags/villager_trade/weaponsmith/level_3.json` resolves to: `weaponsmith/3/flint_emerald`.
/// `trade_set/weaponsmith/level_3.json`'s `amount` is `2`.
const WEAPONSMITH_LEVEL_3: &[TradeRecord] = &[
    // weaponsmith/3/flint_emerald
    TradeRecord {
        wants_item: "minecraft:flint",
        wants_count: 24,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 20,
    },
];

/// `tags/villager_trade/weaponsmith/level_4.json` resolves to: `weaponsmith/4/diamond_emerald`.
/// `trade_set/weaponsmith/level_4.json`'s `amount` is `2`. Skipped (1, `given_item_modifiers`): ['weaponsmith/4/emerald_enchanted_diamond_axe'].
const WEAPONSMITH_LEVEL_4: &[TradeRecord] = &[
    // weaponsmith/4/diamond_emerald
    TradeRecord {
        wants_item: "minecraft:diamond",
        wants_count: 1,
        wants_b: None,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 30,
    },
];

// weaponsmith level 5: no portable trades in the resolved tag (all 1 record(s) use given_item_modifiers: ['weaponsmith/5/emerald_enchanted_diamond_sword']).
/// The pool a profession/level's own `TradeSet` draws from, plus the tag's
/// declared `amount` — `None` for a `(profession, level)` with no portable
/// pool: an unrecognised profession path, `level` outside `1..=5`, or a
/// level whose entire resolved tag is `given_item_modifiers`-only (see this
/// module's own doc for the exact list). `profession` is the bare registry
/// path (e.g. `"farmer"`, `"librarian"`) — the same string
/// `lodestone_server::mobs::villager::Profession::path` already returns, so
/// a caller there needs no enum translation to use this table.
#[must_use]
pub fn pool_for(profession: &str, level: i32) -> Option<(&'static [TradeRecord], usize)> {
    #[allow(clippy::match_same_arms)]
    match (profession, level) {
        ("armorer", 1) => Some((ARMORER_LEVEL_1, 2)),
        ("armorer", 2) => Some((ARMORER_LEVEL_2, 2)),
        ("armorer", 3) => Some((ARMORER_LEVEL_3, 2)),
        ("butcher", 1) => Some((BUTCHER_LEVEL_1, 2)),
        ("butcher", 2) => Some((BUTCHER_LEVEL_2, 2)),
        ("butcher", 3) => Some((BUTCHER_LEVEL_3, 2)),
        ("butcher", 4) => Some((BUTCHER_LEVEL_4, 2)),
        ("butcher", 5) => Some((BUTCHER_LEVEL_5, 2)),
        ("cartographer", 1) => Some((CARTOGRAPHER_LEVEL_1, 2)),
        ("cartographer", 2) => Some((CARTOGRAPHER_LEVEL_2, 2)),
        ("cartographer", 3) => Some((CARTOGRAPHER_LEVEL_3, 2)),
        ("cartographer", 4) => Some((CARTOGRAPHER_LEVEL_4, 2)),
        ("cartographer", 5) => Some((CARTOGRAPHER_LEVEL_5, 2)),
        ("cleric", 1) => Some((CLERIC_LEVEL_1, 2)),
        ("cleric", 2) => Some((CLERIC_LEVEL_2, 2)),
        ("cleric", 3) => Some((CLERIC_LEVEL_3, 2)),
        ("cleric", 4) => Some((CLERIC_LEVEL_4, 2)),
        ("cleric", 5) => Some((CLERIC_LEVEL_5, 2)),
        ("farmer", 1) => Some((FARMER_LEVEL_1, 2)),
        ("farmer", 2) => Some((FARMER_LEVEL_2, 2)),
        ("farmer", 3) => Some((FARMER_LEVEL_3, 2)),
        ("farmer", 4) => Some((FARMER_LEVEL_4, 2)),
        ("farmer", 5) => Some((FARMER_LEVEL_5, 2)),
        ("fisherman", 1) => Some((FISHERMAN_LEVEL_1, 2)),
        ("fisherman", 2) => Some((FISHERMAN_LEVEL_2, 2)),
        ("fisherman", 3) => Some((FISHERMAN_LEVEL_3, 2)),
        ("fisherman", 4) => Some((FISHERMAN_LEVEL_4, 2)),
        ("fisherman", 5) => Some((FISHERMAN_LEVEL_5, 2)),
        ("fletcher", 1) => Some((FLETCHER_LEVEL_1, 2)),
        ("fletcher", 2) => Some((FLETCHER_LEVEL_2, 2)),
        ("fletcher", 3) => Some((FLETCHER_LEVEL_3, 2)),
        ("fletcher", 4) => Some((FLETCHER_LEVEL_4, 2)),
        ("fletcher", 5) => Some((FLETCHER_LEVEL_5, 2)),
        ("leatherworker", 1) => Some((LEATHERWORKER_LEVEL_1, 2)),
        ("leatherworker", 2) => Some((LEATHERWORKER_LEVEL_2, 2)),
        ("leatherworker", 3) => Some((LEATHERWORKER_LEVEL_3, 2)),
        ("leatherworker", 4) => Some((LEATHERWORKER_LEVEL_4, 2)),
        ("leatherworker", 5) => Some((LEATHERWORKER_LEVEL_5, 2)),
        ("librarian", 1) => Some((LIBRARIAN_LEVEL_1, 2)),
        ("librarian", 2) => Some((LIBRARIAN_LEVEL_2, 2)),
        ("librarian", 3) => Some((LIBRARIAN_LEVEL_3, 2)),
        ("librarian", 4) => Some((LIBRARIAN_LEVEL_4, 2)),
        ("librarian", 5) => Some((LIBRARIAN_LEVEL_5, 3)),
        ("mason", 1) => Some((MASON_LEVEL_1, 2)),
        ("mason", 2) => Some((MASON_LEVEL_2, 2)),
        ("mason", 3) => Some((MASON_LEVEL_3, 2)),
        ("mason", 4) => Some((MASON_LEVEL_4, 2)),
        ("mason", 5) => Some((MASON_LEVEL_5, 2)),
        ("shepherd", 1) => Some((SHEPHERD_LEVEL_1, 2)),
        ("shepherd", 2) => Some((SHEPHERD_LEVEL_2, 2)),
        ("shepherd", 3) => Some((SHEPHERD_LEVEL_3, 2)),
        ("shepherd", 4) => Some((SHEPHERD_LEVEL_4, 2)),
        ("shepherd", 5) => Some((SHEPHERD_LEVEL_5, 2)),
        ("toolsmith", 1) => Some((TOOLSMITH_LEVEL_1, 2)),
        ("toolsmith", 2) => Some((TOOLSMITH_LEVEL_2, 2)),
        ("toolsmith", 3) => Some((TOOLSMITH_LEVEL_3, 2)),
        ("toolsmith", 4) => Some((TOOLSMITH_LEVEL_4, 2)),
        ("weaponsmith", 1) => Some((WEAPONSMITH_LEVEL_1, 2)),
        ("weaponsmith", 2) => Some((WEAPONSMITH_LEVEL_2, 2)),
        ("weaponsmith", 3) => Some((WEAPONSMITH_LEVEL_3, 2)),
        ("weaponsmith", 4) => Some((WEAPONSMITH_LEVEL_4, 2)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The discriminating assertion this table exists for: a **specific**
    /// generated record, checked against the real jar file, not "some
    /// trades were generated". Pairwise-distinct counts (20/1/16) so a
    /// transposition of wants/gives/max_uses cannot survive unnoticed.
    #[test]
    fn farmer_level_1_wheat_for_emerald_matches_the_jar_record_exactly() {
        let (pool, amount) = pool_for("farmer", 1).expect("farmer level 1 is ported");
        assert_eq!(amount, 2, "trade_set/farmer/level_1.json's amount is 2");
        assert_eq!(
            pool[0],
            TradeRecord {
                wants_item: "minecraft:wheat",
                wants_count: 20,
                wants_b: None,
                gives_item: "minecraft:emerald",
                gives_count: 1,
                max_uses: 16,
                xp: 2,
            }
        );
    }

    /// The codec-default trap, independent of the farmer table's own test:
    /// `armorer/1/emerald_iron_leggings` has no `xp` key. The codec default
    /// is 1, not the plausible-but-wrong 0.
    #[test]
    fn a_record_missing_its_xp_field_resolves_the_codecs_default_of_one() {
        let (pool, _) = pool_for("armorer", 1).unwrap();
        let leggings = pool
            .iter()
            .find(|t| t.gives_item == "minecraft:iron_leggings")
            .expect("emerald_iron_leggings is in armorer level 1's pool");
        assert_eq!(leggings.xp, 1);
        assert_eq!(leggings.wants_count, 7, "the jar record's own wants.count");
    }

    /// A two-cost trade: fisherman's raw-fish-for-cooked-fish records carry
    /// a real second cost (`additional_wants`), not a modelled-away one.
    #[test]
    fn a_two_cost_trade_carries_its_second_cost_item() {
        let (pool, _) = pool_for("fisherman", 1).unwrap();
        let cod = pool
            .iter()
            .find(|t| t.gives_item == "minecraft:cooked_cod")
            .expect("raw_cod_and_emerald_cooked_cod is in fisherman level 1's pool");
        assert_eq!(cod.wants_item, "minecraft:cod");
        assert_eq!(cod.wants_count, 6);
        assert_eq!(cod.wants_b, Some(("minecraft:emerald", 1)));
        assert_eq!(cod.gives_count, 6);
    }

    /// The `common_smith` tag composition: armorer level 1's resolved pool
    /// includes the shared `smith/1/coal_emerald` record, not just armorer's
    /// own four armor-piece trades.
    #[test]
    fn armorer_pool_includes_the_shared_smith_trade() {
        let (pool, _) = pool_for("armorer", 1).unwrap();
        assert!(
            pool.iter().any(|t| t.wants_item == "minecraft:coal" && t.gives_item == "minecraft:emerald"),
            "armorer/1 should include the common_smith coal_emerald trade"
        );
        assert_eq!(pool.len(), 5, "1 shared smith trade + 4 armorer-only trades");
    }

    /// A profession/level whose entire resolved tag is `given_item_modifiers`
    /// records (enchanted diamond armour) returns no pool at all, not an
    /// empty-but-present one, and not invented numbers.
    #[test]
    fn a_fully_unportable_level_returns_none() {
        assert_eq!(pool_for("armorer", 4), None);
        assert_eq!(pool_for("armorer", 5), None);
        assert_eq!(pool_for("toolsmith", 5), None);
        assert_eq!(pool_for("weaponsmith", 5), None);
    }

    #[test]
    fn an_unrecognised_profession_or_level_returns_none() {
        assert_eq!(pool_for("nitwit", 1), None);
        assert_eq!(pool_for("none", 1), None);
        assert_eq!(pool_for("farmer", 0), None);
        assert_eq!(pool_for("farmer", 6), None);
        assert_eq!(pool_for("not_a_profession", 1), None);
    }

    /// Librarian level 5 is the one profession/level whose `trade_set`
    /// `amount` is not the usual `2` — `3`. A hand-guessed table would very
    /// likely default every level to the common value.
    #[test]
    fn librarian_level_5_amount_is_three_not_the_common_two() {
        let (_, amount) = pool_for("librarian", 5).unwrap();
        assert_eq!(amount, 3);
    }

    /// Coverage magnitude: every one of the thirteen professions has *some*
    /// ported level, and the ported/unported split matches what the jar
    /// data actually contains (not a round-number guess). Collected into one
    /// assertion rather than an `assert!` inside the loop, so a coverage
    /// regression names every profession it affects, not just the first.
    #[test]
    fn every_profession_has_at_least_one_ported_level() {
        let professions = [
            "armorer", "butcher", "cartographer", "cleric", "farmer", "fisherman",
            "fletcher", "leatherworker", "librarian", "mason", "shepherd",
            "toolsmith", "weaponsmith",
        ];
        let mut missing = Vec::new();
        for profession in professions {
            if (1..=5).all(|level| pool_for(profession, level).is_none()) {
                missing.push(profession);
            }
        }
        assert!(missing.is_empty(), "professions with zero ported levels: {missing:?}");
    }

    /// Exact known-unportable set, both directions: nothing on this list is
    /// silently `Some`, and nothing off it is silently `None`.
    #[test]
    fn the_unportable_level_set_is_exactly_the_expected_four() {
        let professions = [
            "armorer", "butcher", "cartographer", "cleric", "farmer", "fisherman",
            "fletcher", "leatherworker", "librarian", "mason", "shepherd",
            "toolsmith", "weaponsmith",
        ];
        let expected_none: &[(&str, i32)] = &[
            ("armorer", 4),
            ("armorer", 5),
            ("toolsmith", 5),
            ("weaponsmith", 5),
        ];
        let mut mismatches = Vec::new();
        for profession in professions {
            for level in 1..=5 {
                let is_none = pool_for(profession, level).is_none();
                let should_be_none = expected_none.contains(&(profession, level));
                if is_none != should_be_none {
                    mismatches.push((profession, level, is_none));
                }
            }
        }
        assert!(mismatches.is_empty(), "unexpected pool_for results: {mismatches:?}");
    }
}
