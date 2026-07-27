//! Per-entity-type **base** dimensions for protocol 776 (Minecraft 26.2).
//!
//! Physics and navigation need each entity type's standing hitbox — the
//! `width`/`height` of its bounding box — to size collision and pathfinding.
//! Vanilla keys these on the entity *type* via `EntityDimensions`, and the
//! values shifted across the 1.9/1.14 pose refactors (a standing zombie was
//! `1.8` tall and is now `1.95`), so this is **version data** and lives here in
//! the version crate, never in a shared layer.
//!
//! # Data source: interrogate the real jar, not `minecraft-data`
//!
//! The table is generated from an authoritative dump produced by booting the
//! real 26.2 server headlessly (`SharedConstants::tryDetectVersion` +
//! `Bootstrap::bootStrap`) and reading `EntityType.getWidth()/getHeight()` — the
//! base `EntityDimensions` at scale 1 — for every registered type. That dump is
//! version-exact. `vendor/minecraft-data` measured stale/incomplete for 26.2 on
//! collision shapes (newest pc entry 1.21.11; ~92% state coverage, 30 blocks
//! missing by name), and there is no reason to assume its entity table is
//! fresher — so, as with block collision, "boot the jar and ask it" is the
//! source. See `tests/entity_dimensions.rs` for the generator and drift guard.
//!
//! # Base dimensions only
//!
//! This table holds **base** geometry. Two per-type quantities are deliberately
//! absent because they are *attribute*-sourced and resolved by the consumer, not
//! static per-type constants:
//!
//! * `step_height` — the resolved `STEP_HEIGHT` attribute *after* the modifier
//!   fold (vanilla `Entity.maxUpStep()`). Baking a static value here would
//!   silently disagree with the pathfinder the moment a modifier exists.
//! * `scale` — the `SCALE` attribute multiplies the box; the caller applies it
//!   *before* building its own dimension struct. The census is never scaled.
//!
//! # Memory design
//!
//! Pure rodata, zero heap, O(1) by id: a `[(f32, f32); TYPE_COUNT]` of
//! `(width, height)` indexed by the network entity-type registry id — the same
//! id space as [`crate::entity_types`]. `f32` is the game's own storage width
//! for these fields, so this is lossless versus vanilla.

use lodestone_model::EntityBaseDimensions;

use crate::generated_entity_dimensions::ENTITY_DIMENSIONS;
pub use crate::generated_entity_dimensions::TYPE_COUNT;

/// Resolves a network entity-type id to its **base** `(width, height)` hitbox.
///
/// Returns `None` for ids outside `0..TYPE_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a guessed box.
/// The values are base dimensions — see the module docs on why `step_height`
/// and `scale` are the consumer's responsibility.
#[must_use]
pub fn base_dimensions(id: i32) -> Option<EntityBaseDimensions> {
    usize::try_from(id)
        .ok()
        .and_then(|index| ENTITY_DIMENSIONS.get(index).copied())
        .map(|(width, height)| EntityBaseDimensions { width, height })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_types::entity_type_id;

    #[test]
    fn out_of_range_ids_are_none() {
        assert_eq!(base_dimensions(-1), None);
        assert_eq!(base_dimensions(TYPE_COUNT as i32), None);
        assert_eq!(base_dimensions(i32::MAX), None);
    }

    #[test]
    fn every_id_resolves_to_a_finite_box() {
        // The table is dense over the whole id space and every hitbox is
        // non-negative and finite. Some types are legitimately `0 x 0` — the
        // display entities (`block_display`/`item_display`/`text_display`),
        // `interaction`, `marker`, and `lightning_bolt` have no physical box —
        // so this asserts validity, not positivity; the bit-exact check against
        // the server dump is what catches a transposed or truncated table.
        for id in 0..TYPE_COUNT as i32 {
            let dims = base_dimensions(id).expect("id within TYPE_COUNT resolves");
            assert!(
                dims.width >= 0.0 && dims.width.is_finite(),
                "id {id} has an invalid width {}",
                dims.width
            );
            assert!(
                dims.height >= 0.0 && dims.height.is_finite(),
                "id {id} has an invalid height {}",
                dims.height
            );
        }
    }

    #[test]
    fn keys_align_with_the_entity_type_table() {
        // The dimension table and the id->name table share one id space; the
        // player's box is the canonical anchor (0.6 x 1.8, constant across
        // versions), so a misaligned table fails here.
        let player = entity_type_id("minecraft:player").expect("player is a real type");
        let dims = base_dimensions(player).expect("player has dimensions");
        assert_eq!(dims.width, 0.6);
        assert_eq!(dims.height, 1.8);
    }
}
