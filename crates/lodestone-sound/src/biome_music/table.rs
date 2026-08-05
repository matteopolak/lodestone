//! GENERATED — do not edit by hand.
//!
//! Produced by `tests/biome_music_table.rs` from
//! `crates/lodestone-server/assets/worldgen/biome/*.json`, which is where the
//! authority for these values lives. Refresh with:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_music_table
//! ```
//!
//! Sorted by biome id; `biome_audio` binary-searches it.

use super::BiomeMusic;
use crate::music::{BackgroundMusic, Music};

/// Every biome that declares `minecraft:audio/background_music` or
/// `minecraft:audio/music_volume`, namespace-stripped and sorted by id.
pub static BIOME_MUSIC: &[(&str, BiomeMusic)] = &[
    (
        "badlands",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.badlands", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "bamboo_jungle",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.bamboo_jungle", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "basalt_deltas",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.nether.basalt_deltas", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "birch_forest",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.forest", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "cherry_grove",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.cherry_grove", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "cold_ocean",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.game", 12000, 24000, false)),
                creative: Some(Music::of("music.creative", 12000, 24000, false)),
                underwater: Some(Music::of("music.under_water", 12000, 24000, false)),
            },
            music_volume: None,
        },
    ),
    (
        "crimson_forest",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.nether.crimson_forest", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "dark_forest",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.forest", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "deep_cold_ocean",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.game", 12000, 24000, false)),
                creative: Some(Music::of("music.creative", 12000, 24000, false)),
                underwater: Some(Music::of("music.under_water", 12000, 24000, false)),
            },
            music_volume: None,
        },
    ),
    (
        "deep_dark",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.deep_dark", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "deep_lukewarm_ocean",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.game", 12000, 24000, false)),
                creative: Some(Music::of("music.creative", 12000, 24000, false)),
                underwater: Some(Music::of("music.under_water", 12000, 24000, false)),
            },
            music_volume: None,
        },
    ),
    (
        "deep_ocean",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.game", 12000, 24000, false)),
                creative: Some(Music::of("music.creative", 12000, 24000, false)),
                underwater: Some(Music::of("music.under_water", 12000, 24000, false)),
            },
            music_volume: None,
        },
    ),
    (
        "desert",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.desert", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "dripstone_caves",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.dripstone_caves", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "eroded_badlands",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.badlands", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "flower_forest",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.flower_forest", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "forest",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.forest", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "frozen_peaks",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.frozen_peaks", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "frozen_river",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.game", 12000, 24000, false)),
                creative: Some(Music::of("music.creative", 12000, 24000, false)),
                underwater: Some(Music::of("music.under_water", 12000, 24000, false)),
            },
            music_volume: None,
        },
    ),
    (
        "grove",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.grove", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "jagged_peaks",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.jagged_peaks", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "jungle",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.jungle", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "lukewarm_ocean",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.game", 12000, 24000, false)),
                creative: Some(Music::of("music.creative", 12000, 24000, false)),
                underwater: Some(Music::of("music.under_water", 12000, 24000, false)),
            },
            music_volume: None,
        },
    ),
    (
        "lush_caves",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.lush_caves", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "mangrove_swamp",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.swamp", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "meadow",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.meadow", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "nether_wastes",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.nether.nether_wastes", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "ocean",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.game", 12000, 24000, false)),
                creative: Some(Music::of("music.creative", 12000, 24000, false)),
                underwater: Some(Music::of("music.under_water", 12000, 24000, false)),
            },
            music_volume: None,
        },
    ),
    (
        "old_growth_birch_forest",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.forest", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "old_growth_pine_taiga",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.old_growth_taiga", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "old_growth_spruce_taiga",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.old_growth_taiga", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "pale_garden",
        BiomeMusic {
            music: BackgroundMusic {
                default: None,
                creative: None,
                underwater: None,
            },
            music_volume: Some(0.0),
        },
    ),
    (
        "river",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.game", 12000, 24000, false)),
                creative: Some(Music::of("music.creative", 12000, 24000, false)),
                underwater: Some(Music::of("music.under_water", 12000, 24000, false)),
            },
            music_volume: None,
        },
    ),
    (
        "snowy_slopes",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.snowy_slopes", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "soul_sand_valley",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.nether.soul_sand_valley", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "sparse_jungle",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.sparse_jungle", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "stony_peaks",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.stony_peaks", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "sulfur_caves",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.sulfur_caves", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "swamp",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.swamp", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "warm_ocean",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.game", 12000, 24000, false)),
                creative: Some(Music::of("music.creative", 12000, 24000, false)),
                underwater: Some(Music::of("music.under_water", 12000, 24000, false)),
            },
            music_volume: None,
        },
    ),
    (
        "warped_forest",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.nether.warped_forest", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
    (
        "wooded_badlands",
        BiomeMusic {
            music: BackgroundMusic {
                default: Some(Music::of("music.overworld.badlands", 12000, 24000, false)),
                creative: None,
                underwater: None,
            },
            music_volume: None,
        },
    ),
];
