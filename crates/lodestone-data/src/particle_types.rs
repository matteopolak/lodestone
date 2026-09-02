//! Public particle-type id→identifier resolution for protocol 776.
//!
//! `level_particles` carries the particle as a `minecraft:particle_type`
//! registry id (a VarInt) before any per-particle payload data. The id→name
//! mapping is generated from Mojang's own `registries.json` for 26.2, the one
//! canonical internal version, so it lives here in this data crate
//! rather than in `lodestone-v770` — it is a game-data census,
//! not wire-format code.

pub use crate::generated_particle_types::PARTICLE_TYPE_COUNT;
use crate::generated_particle_types::PARTICLE_TYPE_NAMES;

/// Resolves a network particle-type id to its canonical `minecraft:*`
/// identifier.
///
/// Returns `None` for ids outside `0..PARTICLE_TYPE_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong particle.
#[must_use]
pub fn particle_type_name(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|index| PARTICLE_TYPE_NAMES.get(index).copied())
}

/// Whether a network particle-type id has **no** per-particle payload beyond
/// the type id itself — vanilla's own simple-particle-type marker, as opposed to a
/// payload-carrying particle type whose own stream codec reads further bytes
/// (dust colour, a block state, an item stack, …).
///
/// Derived from vanilla's own particle-type registration source's own two `register` overloads: the
/// two-argument form (`register(name, overrideLimiter)`) constructs a
/// simple particle type, the four-argument form (`register(name,
/// overrideLimiter, codec, streamCodec)`) constructs a payload-carrying particle type with
/// a real payload codec. Argument count is unambiguous in the source and this
/// table is exhaustive over the 26.2 registry (125 entries, 103 simple).
///
/// This is *not* itself proof a given packet's field is safe to skip on the
/// strength of this id alone — the id still has to be at a fixed byte offset,
/// which only holds for a field whose own codec is exactly
/// a registry-dispatch codec over the particle-type registry with no
/// preceding length prefix, e.g. the clientbound explode packet's own
/// explosion-particle field or the entity-motion packet's metadata
/// particle field(s). A caller that recognises the id this way
/// still consumes exactly zero further bytes for it — nothing else to read,
/// by construction.
///
/// Returns `false` (not skippable) for an id outside `0..PARTICLE_TYPE_COUNT`,
/// the same fail-closed convention [`particle_type_name`] uses.
///
/// Regenerate by re-running the derivation this table was built from: match
/// every `register("<name>", <bool>` call in vanilla's own particle-type
/// registration source for the
/// pinned decompile, split on whether the call has two or four arguments, and
/// join by name against `registries.json`'s `minecraft:particle_type`
/// `protocol_id`s.
#[must_use]
pub fn is_simple_particle_type(id: i32) -> bool {
    usize::try_from(id)
        .ok()
        .and_then(|index| PARTICLE_TYPE_IS_SIMPLE.get(index).copied())
        .unwrap_or(false)
}

/// Indexed by network particle-type id, `true` for a simple particle type —
/// see [`is_simple_particle_type`]. Hand-derived (not `xtask gen-registries`
/// output): the generator only ever produces id→name tables, and a
/// simple/complex classification needs vanilla's own particle-type
/// registration source's call-site shape,
/// which is source structure rather than registry data.
#[rustfmt::skip]
const PARTICLE_TYPE_IS_SIMPLE: [bool; PARTICLE_TYPE_COUNT as usize] = [
    true,  //   0 angry_villager
    false, //   1 block
    false, //   2 block_marker
    true,  //   3 bubble
    true,  //   4 sulfur_bubbles
    true,  //   5 noxious_gas
    true,  //   6 noxious_gas_cloud
    false, //   7 geyser
    false, //   8 geyser_base
    false, //   9 geyser_poof
    false, //  10 geyser_plume
    true,  //  11 cloud
    true,  //  12 copper_fire_flame
    true,  //  13 crit
    true,  //  14 damage_indicator
    false, //  15 dragon_breath
    true,  //  16 dripping_lava
    true,  //  17 falling_lava
    true,  //  18 landing_lava
    true,  //  19 dripping_water
    true,  //  20 falling_water
    false, //  21 dust
    false, //  22 dust_color_transition
    false, //  23 effect
    true,  //  24 elder_guardian
    true,  //  25 enchanted_hit
    true,  //  26 enchant
    true,  //  27 end_rod
    false, //  28 entity_effect
    true,  //  29 explosion_emitter
    true,  //  30 explosion
    true,  //  31 gust
    true,  //  32 small_gust
    true,  //  33 gust_emitter_large
    true,  //  34 gust_emitter_small
    true,  //  35 sonic_boom
    false, //  36 falling_dust
    true,  //  37 firework
    true,  //  38 fishing
    true,  //  39 flame
    true,  //  40 infested
    true,  //  41 cherry_leaves
    true,  //  42 pale_oak_leaves
    false, //  43 tinted_leaves
    true,  //  44 sculk_soul
    false, //  45 sculk_charge
    true,  //  46 sculk_charge_pop
    true,  //  47 soul_fire_flame
    true,  //  48 soul
    false, //  49 flash
    true,  //  50 happy_villager
    true,  //  51 composter
    true,  //  52 heart
    false, //  53 instant_effect
    false, //  54 item
    false, //  55 vibration
    false, //  56 trail
    true,  //  57 pause_mob_growth
    true,  //  58 reset_mob_growth
    true,  //  59 item_slime
    true,  //  60 item_cobweb
    true,  //  61 item_snowball
    true,  //  62 large_smoke
    true,  //  63 lava
    true,  //  64 mycelium
    true,  //  65 note
    true,  //  66 poof
    true,  //  67 portal
    true,  //  68 rain
    true,  //  69 smoke
    true,  //  70 white_smoke
    true,  //  71 sneeze
    true,  //  72 spit
    true,  //  73 squid_ink
    true,  //  74 sweep_attack
    true,  //  75 totem_of_undying
    true,  //  76 underwater
    true,  //  77 splash
    true,  //  78 witch
    true,  //  79 bubble_pop
    true,  //  80 current_down
    true,  //  81 bubble_column_up
    true,  //  82 nautilus
    true,  //  83 dolphin
    true,  //  84 campfire_cosy_smoke
    true,  //  85 campfire_signal_smoke
    true,  //  86 dripping_honey
    true,  //  87 falling_honey
    true,  //  88 landing_honey
    true,  //  89 falling_nectar
    true,  //  90 falling_spore_blossom
    true,  //  91 ash
    true,  //  92 crimson_spore
    true,  //  93 warped_spore
    true,  //  94 spore_blossom_air
    true,  //  95 dripping_obsidian_tear
    true,  //  96 falling_obsidian_tear
    true,  //  97 landing_obsidian_tear
    true,  //  98 reverse_portal
    true,  //  99 white_ash
    true,  // 100 small_flame
    true,  // 101 snowflake
    true,  // 102 dripping_dripstone_lava
    true,  // 103 falling_dripstone_lava
    true,  // 104 dripping_dripstone_water
    true,  // 105 falling_dripstone_water
    true,  // 106 glow_squid_ink
    true,  // 107 glow
    true,  // 108 wax_on
    true,  // 109 wax_off
    true,  // 110 electric_spark
    true,  // 111 scrape
    false, // 112 shriek
    true,  // 113 egg_crack
    true,  // 114 dust_plume
    true,  // 115 trial_spawner_detection
    true,  // 116 trial_spawner_detection_ominous
    true,  // 117 vault_connection
    false, // 118 dust_pillar
    true,  // 119 ominous_spawning
    true,  // 120 raid_omen
    true,  // 121 trial_omen
    false, // 122 block_crumble
    true,  // 123 firefly
    true,  // 124 sulfur_cube_goo
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every id this test names is cross-checked against the pinned 26.2
    /// decompile's own particle-type registration source at the time this table was built —
    /// see [`PARTICLE_TYPE_IS_SIMPLE`]'s own doc for the exact derivation.
    /// `gust_emitter_small` (id 34) is the one that matters operationally:
    /// vanilla's own wind-charge explode step sends it as its own
    /// explosion-particle field, and
    /// it is a simple particle type exactly like `explosion_emitter`/
    /// `explosion`, so a decoder that only allowlists the latter two rejects
    /// a perfectly byte-skippable id.
    #[test]
    fn known_ids_classify_correctly() {
        assert!(is_simple_particle_type(29), "explosion_emitter");
        assert!(is_simple_particle_type(30), "explosion");
        assert!(is_simple_particle_type(33), "gust_emitter_large");
        assert!(is_simple_particle_type(34), "gust_emitter_small");
        assert!(!is_simple_particle_type(1), "block carries a BlockParticleOption");
        assert!(!is_simple_particle_type(21), "dust carries a DustParticleOptions");
        assert!(!is_simple_particle_type(54), "item carries an ItemParticleOption");
        assert!(
            !is_simple_particle_type(-1),
            "an out-of-range id must fail closed, not claim skippability"
        );
        assert!(
            !is_simple_particle_type(PARTICLE_TYPE_COUNT as i32),
            "one past the end must fail closed"
        );
    }

    /// The table's length must track the registry, or a version bump silently
    /// truncates classification for every id past the old length.
    #[test]
    fn table_length_matches_registry_count() {
        assert_eq!(PARTICLE_TYPE_IS_SIMPLE.len(), PARTICLE_TYPE_COUNT as usize);
    }
}
