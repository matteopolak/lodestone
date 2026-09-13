//! GENERATED — do not edit by hand.
//!
//! Produced by `tests/biome_ambient_table.rs` from
//! `crates/lodestone-server/assets/worldgen/biome/*.json`. Refresh with:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_ambient_table
//! ```
//!
//! Sorted by biome id; `biome_ambient` binary-searches it.

use std::borrow::Cow;

use crate::ambient::{AmbientAdditionsSettings, AmbientMoodSettings, AmbientSounds};

/// Every biome that declares `minecraft:audio/ambient_sounds`, sorted by id.
pub static BIOME_AMBIENT: &[(&str, AmbientSounds)] = &[
    (
        "basalt_deltas",
        AmbientSounds {
            loop_sound: Some(Cow::Borrowed("ambient.basalt_deltas.loop")),
            mood: Some(AmbientMoodSettings::of("ambient.basalt_deltas.mood", 6000, 8, 2.0)),
            additions: Cow::Borrowed(&[
                AmbientAdditionsSettings::of("ambient.basalt_deltas.additions", 0.0111),
            ]),
        },
    ),
    (
        "crimson_forest",
        AmbientSounds {
            loop_sound: Some(Cow::Borrowed("ambient.crimson_forest.loop")),
            mood: Some(AmbientMoodSettings::of("ambient.crimson_forest.mood", 6000, 8, 2.0)),
            additions: Cow::Borrowed(&[
                AmbientAdditionsSettings::of("ambient.crimson_forest.additions", 0.0111),
            ]),
        },
    ),
    (
        "nether_wastes",
        AmbientSounds {
            loop_sound: Some(Cow::Borrowed("ambient.nether_wastes.loop")),
            mood: Some(AmbientMoodSettings::of("ambient.nether_wastes.mood", 6000, 8, 2.0)),
            additions: Cow::Borrowed(&[
                AmbientAdditionsSettings::of("ambient.nether_wastes.additions", 0.0111),
            ]),
        },
    ),
    (
        "soul_sand_valley",
        AmbientSounds {
            loop_sound: Some(Cow::Borrowed("ambient.soul_sand_valley.loop")),
            mood: Some(AmbientMoodSettings::of("ambient.soul_sand_valley.mood", 6000, 8, 2.0)),
            additions: Cow::Borrowed(&[
                AmbientAdditionsSettings::of("ambient.soul_sand_valley.additions", 0.0111),
            ]),
        },
    ),
    (
        "warped_forest",
        AmbientSounds {
            loop_sound: Some(Cow::Borrowed("ambient.warped_forest.loop")),
            mood: Some(AmbientMoodSettings::of("ambient.warped_forest.mood", 6000, 8, 2.0)),
            additions: Cow::Borrowed(&[
                AmbientAdditionsSettings::of("ambient.warped_forest.additions", 0.0111),
            ]),
        },
    ),
];
