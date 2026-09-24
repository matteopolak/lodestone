//! Entity-type id resolution for protocol 5.
//!
//! # Two id spaces, and one that is not an id space at all
//!
//! Spawning is split across four packets in this era, and only two of them
//! carry a type:
//!
//! - `spawn_entity_living` carries a `u8` **mob** id.
//! - `spawn_entity` carries an `i8` **object** id.
//! - `named_entity_spawn` carries no type; it is always [`PLAYER`].
//! - `spawn_entity_painting` and `spawn_entity_experience_orb` carry no type
//!   either: the packet *is* the type. [`PAINTING`] and [`EXPERIENCE_ORB`]
//!   name them so a caller never has to invent an id for a packet that has
//!   none.
//!
//! The two id spaces overlap and disagree: 50 is a creeper as a mob and
//! primed TNT as an object, 63 is an ender dragon as a mob and a fireball as
//! an object, 66 is a witch as a mob and a wither skull as an object. Reading
//! an object spawn through the mob table is therefore not a near-miss — it
//! names a real, wrong entity every time. That is why they resolve through
//! independent tables and why the caller must pass the right one.
//!
//! # What the tables are built from, and what that corrected
//!
//! Both come from a transcript of a real 1.7.10 server's wire, with
//! `minecraft-data` as the cross-check rather than the source — the reverse
//! of the 1.8 era's arrangement. The comparison is worth stating because the
//! dataset is wrong about this era's object space in five ways and
//! incomplete in two more; `tests/entity_types.rs` holds the full
//! enumeration and asserts it, so the claim is checked rather than recorded.
//!
//! # Naming
//!
//! The tables carry this era's own internal names, snake-cased, not modern
//! resource keys: `minecraft:pig_zombie` rather than `zombie_pigman`,
//! `minecraft:lava_slime` rather than `magma_cube`, `minecraft:ozelot`
//! rather than `ocelot`, `minecraft:primed_tnt` rather than `tnt`. This keeps
//! the family honest about what the wire actually identifies rather than
//! implying a mapping onto the modern registry that no one has checked. It is
//! also what the 1.8 and 1.9 eras do, so a consumer translating across the
//! legacy families has one convention rather than two.

pub use crate::generated_entity_types::{MOB_TYPES_COUNT, OBJECT_TYPES_COUNT};
use crate::generated_entity_types::{MOB_TYPES, OBJECT_TYPES};

/// Canonical identifier for a player, spawned by `named_entity_spawn`.
pub const PLAYER: &str = "minecraft:player";

/// Canonical identifier for a painting, which has its own spawn packet and
/// no type id.
pub const PAINTING: &str = "minecraft:painting";

/// Canonical identifier for an experience orb, which has its own spawn
/// packet and no type id.
pub const EXPERIENCE_ORB: &str = "minecraft:xp_orb";

/// Resolves a `spawn_entity_living` mob id.
///
/// Returns `None` for an id absent from the table, so an id this era does not
/// number surfaces as an explicit miss rather than as a wrong entity.
#[must_use]
pub fn mob_type_name(id: i32) -> Option<&'static str> {
    lookup(&MOB_TYPES, id)
}

/// Resolves a `spawn_entity` object id.
///
/// Returns `None` for an id absent from the table. One id is genuinely
/// missing rather than absent from the era: the fishing bobber, which no
/// `/summon` form can produce, so the wire oracle could not confirm a name
/// for it. Resolving to `None` drops that spawn with a log line, which is the
/// honest outcome for a row no evidence supports.
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
