//! Cosmetic per-mob state: wool and collar dye, custom names, and the
//! appearance variant a mob is given when it spawns.
//!
//! # What it is
//!
//! [`Appearance`] is the part of a mob a player can see and change that has no
//! behaviour of its own. It is real server state: it reaches clients as entity
//! metadata ([`SimMob::snapshot`]) and survives a reload as the vanilla-named
//! fields in `sim_persistence_state`.
//!
//! # Spawn rules
//!
//! [`choose_variant`] picks the variant a freshly spawned mob gets from the
//! biome it spawns in and a random roll, per the tag memberships and weights in
//! the 26.3 data files. The roll is derived from the mob's uuid, which is random
//! per spawn and needs no extra generator on the sim.
//!
//! Not modeled: all-black cats at full moon or in structures that spawn them,
//! mooshroom colour change by lightning at spawn, the inherited variant of a
//! bred baby (a baby gets the roll a wild spawn would).

use lodestone_core::Nbt;
use uuid::Uuid;

/// The sixteen dye names in vanilla's ordinal order.
pub(super) const DYE_NAMES: [&str; 16] = [
    "white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray",
    "cyan", "purple", "blue", "brown", "green", "red", "black",
];

/// Red: the collar a freshly tamed wolf or cat wears.
pub(super) const DEFAULT_COLLAR: u8 = 14;

/// The dye ordinal of a `*_dye` item path such as `red_dye`.
pub(super) fn dye_of_item(item_path: &str) -> Option<u8> {
    let name = item_path.strip_suffix("_dye")?;
    DYE_NAMES.iter().position(|dye| *dye == name).map(|i| i as u8)
}

/// A mob's variant, in the shape its saved field takes.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum MobVariant {
    /// A registry key (`minecraft:temperate`) or bare name (`snow`).
    Name(String),
    /// A packed or ordinal int (horse, llama, parrot, axolotl, rabbit).
    Int(i32),
}

/// Cosmetic state carried by every mob; the fields only matter for the species
/// that read them.
#[derive(Debug, Clone, Default)]
pub(super) struct Appearance {
    /// Sheep wool dye ordinal.
    pub wool: u8,
    /// Whether the sheep has been sheared.
    pub sheared: bool,
    /// Wolf or cat collar dye ordinal.
    pub collar: u8,
    /// The custom name component, as saved.
    pub custom_name: Option<Nbt>,
    /// Whether the custom name floats above the mob.
    pub name_visible: bool,
    /// The species' variant, when it has one.
    pub variant: Option<MobVariant>,
}

/// The saved field a species stores its variant in.
pub(super) fn variant_field(species: &str) -> Option<&'static str> {
    match species {
        "cat" | "wolf" | "cow" | "pig" | "chicken" | "frog" => Some("variant"),
        "horse" | "llama" | "trader_llama" | "parrot" | "axolotl" => Some("Variant"),
        "rabbit" => Some("RabbitType"),
        "fox" | "mooshroom" => Some("Type"),
        _ => None,
    }
}

/// Every saved field the appearance model owns for `species`.
pub(super) fn owned_fields(species: &str) -> Vec<&'static str> {
    let mut fields = vec!["CustomName", "CustomNameVisible"];
    match species {
        "sheep" => fields.extend(["Color", "Sheared"]),
        "wolf" | "cat" => fields.push("CollarColor"),
        _ => {}
    }
    fields.extend(variant_field(species));
    fields
}

fn roll(uuid: Uuid, salt: u64, bound: u32) -> u32 {
    let (hi, lo) = uuid.as_u64_pair();
    let mut z = hi ^ lo.rotate_left(17) ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    ((z >> 32) as u32) % bound
}

fn short(biome: &str) -> &str {
    biome.strip_prefix("minecraft:").unwrap_or(biome)
}

const JUNGLE: [&str; 3] = ["bamboo_jungle", "jungle", "sparse_jungle"];
const SAVANNA: [&str; 3] = ["savanna", "savanna_plateau", "windswept_savanna"];
const NETHER: [&str; 5] =
    ["nether_wastes", "soul_sand_valley", "crimson_forest", "warped_forest", "basalt_deltas"];
const BADLANDS: [&str; 3] = ["badlands", "eroded_badlands", "wooded_badlands"];
const END: [&str; 5] =
    ["the_end", "end_highlands", "end_midlands", "small_end_islands", "end_barrens"];
const WARM_COMMON: [&str; 3] = ["desert", "warm_ocean", "mangrove_swamp"];
const WARM_FARM_EXTRA: [&str; 2] = ["deep_lukewarm_ocean", "lukewarm_ocean"];
const COLD_COMMON: [&str; 12] = [
    "snowy_plains", "ice_spikes", "frozen_peaks", "jagged_peaks", "snowy_slopes", "frozen_ocean",
    "deep_frozen_ocean", "grove", "deep_dark", "frozen_river", "snowy_taiga", "snowy_beach",
];
const COLD_FARM_EXTRA: [&str; 11] = [
    "cold_ocean", "deep_cold_ocean", "old_growth_pine_taiga", "old_growth_spruce_taiga", "taiga",
    "windswept_forest", "windswept_gravelly_hills", "windswept_hills", "stony_peaks",
    "dappled_forest", "dappled_forest",
];
const SNOW_FOXES_AND_WHITE_RABBITS: [&str; 10] = [
    "snowy_plains", "ice_spikes", "frozen_ocean", "snowy_taiga", "frozen_river", "snowy_beach",
    "frozen_peaks", "jagged_peaks", "snowy_slopes", "grove",
];

fn is_warm(biome: &str, farm: bool) -> bool {
    let b = short(biome);
    WARM_COMMON.contains(&b)
        || JUNGLE.contains(&b)
        || SAVANNA.contains(&b)
        || NETHER.contains(&b)
        || BADLANDS.contains(&b)
        || (farm && WARM_FARM_EXTRA.contains(&b))
}

fn is_cold(biome: &str, farm: bool) -> bool {
    let b = short(biome);
    COLD_COMMON.contains(&b) || END.contains(&b) || (farm && COLD_FARM_EXTRA.contains(&b))
}

/// The temperature variant of a cow, pig, chicken (`farm`) or frog.
fn temperature_variant(biome: &str, farm: bool) -> &'static str {
    if is_warm(biome, farm) {
        "minecraft:warm"
    } else if is_cold(biome, farm) {
        "minecraft:cold"
    } else {
        "minecraft:temperate"
    }
}

const CAT_VARIANTS: [&str; 10] = [
    "tabby", "black", "red", "siamese", "british_shorthair", "calico", "persian", "ragdoll",
    "white", "jellie",
];

fn wolf_variant(biome: &str) -> &'static str {
    let b = short(biome);
    match b {
        "snowy_taiga" => "minecraft:ashen",
        "old_growth_pine_taiga" => "minecraft:black",
        "old_growth_spruce_taiga" => "minecraft:chestnut",
        "grove" => "minecraft:snowy",
        "forest" => "minecraft:woods",
        _ if JUNGLE.contains(&b) => "minecraft:rusty",
        _ if SAVANNA.contains(&b) => "minecraft:spotted",
        _ if BADLANDS.contains(&b) => "minecraft:striped",
        _ => "minecraft:pale",
    }
}

/// The wool colour of a sheep spawned in `biome`: weighted per climate, with a
/// 1-in-500 pink among the common colour.
pub(super) fn sheep_color(uuid: Uuid, biome: &str) -> u8 {
    // (colour, weight) lists; the last entry is the common colour (weight 82).
    let table: [(u8, u32); 5] = if is_warm(biome, true) {
        [(7, 5), (8, 5), (0, 5), (15, 3), (12, 82)]
    } else if is_cold(biome, true) {
        [(8, 5), (7, 5), (0, 5), (12, 3), (15, 82)]
    } else {
        [(15, 5), (7, 5), (8, 5), (12, 3), (0, 82)]
    };
    let mut r = roll(uuid, 1, 100);
    for (i, (color, weight)) in table.iter().enumerate() {
        if r < *weight {
            return if i == 4 && roll(uuid, 2, 500) == 0 { 6 } else { *color };
        }
        r -= weight;
    }
    table[4].0
}

/// The variant a mob of `species` spawned in `biome` gets, or `None` for a
/// species without one.
pub(super) fn choose_variant(species: &str, biome: &str, uuid: Uuid) -> Option<MobVariant> {
    let name = |s: &str| Some(MobVariant::Name(s.to_owned()));
    match species {
        "cow" | "pig" | "chicken" => name(temperature_variant(biome, true)),
        "frog" => name(temperature_variant(biome, false)),
        "wolf" => name(wolf_variant(biome)),
        "cat" => name(&format!("minecraft:{}", CAT_VARIANTS[roll(uuid, 3, 10) as usize])),
        "horse" => {
            let color = roll(uuid, 4, 7) as i32;
            let markings = roll(uuid, 5, 5) as i32;
            Some(MobVariant::Int(color | (markings << 8)))
        }
        "llama" | "trader_llama" => Some(MobVariant::Int(roll(uuid, 6, 4) as i32)),
        "parrot" => Some(MobVariant::Int(roll(uuid, 7, 5) as i32)),
        "axolotl" => Some(MobVariant::Int(if roll(uuid, 8, 1200) == 0 {
            4
        } else {
            roll(uuid, 9, 4) as i32
        })),
        "fox" => name(if SNOW_FOXES_AND_WHITE_RABBITS.contains(&short(biome)) { "snow" } else { "red" }),
        "mooshroom" => name("red"),
        "rabbit" => {
            let r = roll(uuid, 10, 100);
            let b = short(biome);
            Some(MobVariant::Int(if SNOW_FOXES_AND_WHITE_RABBITS.contains(&b) {
                if r < 80 { 1 } else { 3 }
            } else if b == "desert" {
                4
            } else if r < 50 {
                0
            } else if r < 90 {
                5
            } else {
                2
            }))
        }
        _ => None,
    }
}

impl MobVariant {
    /// The saved NBT value.
    pub(super) fn to_nbt(&self) -> Nbt {
        match self {
            Self::Name(name) => Nbt::String(name.clone()),
            Self::Int(value) => Nbt::Int(*value),
        }
    }

    /// Decodes a saved value; `None` for a tag of the wrong shape.
    pub(super) fn from_nbt(nbt: &Nbt) -> Option<Self> {
        match nbt {
            Nbt::String(name) => Some(Self::Name(name.clone())),
            Nbt::Int(value) => Some(Self::Int(*value)),
            Nbt::Short(value) => Some(Self::Int(i32::from(*value))),
            Nbt::Byte(value) => Some(Self::Int(i32::from(*value))),
            _ => None,
        }
    }
}
