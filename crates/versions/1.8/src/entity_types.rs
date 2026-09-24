//! Public entity-type id→name resolution for protocol 47 (Minecraft 1.8.x).
//!
//! 1.8 splits entity spawning across three packets and, unlike the modern flat
//! entity-type registry, uses **two separate numeric id spaces**:
//!
//! * `spawn_entity_living` (mobs) carries a `u8` **mob** type id.
//! * `spawn_entity` (objects) carries an `i8` **object** type id.
//! * `named_entity_spawn` (players) carries no type — it is always
//!   [`PLAYER`].
//!
//! The two id spaces overlap (mob `50` is a creeper, object `50` is primed
//! TNT), so they resolve through independent tables. That id→name mapping is
//! version-specific data — the ids and even the names differ between releases —
//! so it lives here in the version crate, generated from the community
//! `minecraft-data` project, and never in a shared crate.
//!
//! **Naming judgement call (surfaced deliberately, not silently resolved):**
//! `minecraft-data` records 1.8's *legacy internal* names, not modern resource
//! keys. We snake_case them verbatim, so 1.8's `PigZombie` becomes
//! `minecraft:pig_zombie` (not the modern `minecraft:zombie_pigman`),
//! `LavaSlime` becomes `minecraft:lava_slime` (not `magma_cube`), and `Ozelot`
//! becomes `minecraft:ozelot` (not `ocelot`). This keeps the 1.8 family honest
//! about what the wire actually identifies rather than pretending the ids map
//! onto the modern registry. Consumers that need cross-version identity must
//! translate deliberately.

pub use crate::generated_entity_types::{MOB_TYPE_COUNT, OBJECT_TYPE_COUNT};
use crate::generated_entity_types::{MOB_TYPES, OBJECT_TYPES};

/// Canonical identifier for a player entity (`named_entity_spawn`).
pub const PLAYER: &str = "minecraft:player";

/// Resolves a `spawn_entity_living` mob type id to its canonical identifier.
///
/// Returns `None` for ids absent from the 1.8 mob table, so a malformed or
/// future-version id surfaces as an explicit miss rather than a wrong type.
#[must_use]
pub fn mob_type_name(id: i32) -> Option<&'static str> {
    lookup(&MOB_TYPES, id)
}

/// Resolves a `spawn_entity` object type id to its canonical identifier.
///
/// Returns `None` for ids absent from the 1.8 object table.
#[must_use]
pub fn object_type_name(id: i32) -> Option<&'static str> {
    lookup(&OBJECT_TYPES, id)
}

fn lookup(table: &[(i32, &'static str)], id: i32) -> Option<&'static str> {
    table
        .binary_search_by_key(&id, |&(key, _)| key)
        .ok()
        .map(|index| table[index].1)
}
