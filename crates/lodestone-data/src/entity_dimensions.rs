//! Per-entity-type **base** dimensions for protocol 776 (Minecraft 26.2).
//!
//! Physics and navigation need each entity type's standing hitbox — the
//! `width`/`height` of its bounding box — to size collision and pathfinding.
//! Vanilla keys these on the entity *type* via `EntityDimensions`, and the
//! values shifted across the 1.9/1.14 pose refactors (a standing zombie was
//! `1.8` tall and is now `1.95`), so this is **26.2 game data** and lives here
//! in this data crate rather than in `lodestone-v26-2` — a
//! version-free consumer needs no protocol dependency to read it.
//!
//! # Data source: interrogate the real jar, not `minecraft-data`
//!
//! The table is generated from an authoritative dump produced by booting the
//! real 26.2 server headlessly and reading vanilla's own entity-type
//! width/height accessors — the
//! base entity-dimensions record at scale 1 — for every registered type. That dump is
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
//!   fold (vanilla's own "max up step" accessor). Baking a static value here would
//!   silently disagree with the pathfinder the moment a modifier exists.
//! * `scale` — the `SCALE` attribute multiplies the box; the caller applies it
//!   *before* building its own dimension struct. The census is never scaled.
//!
//! # Memory design
//!
//! Pure rodata, zero heap, O(1) from a validated [`EntityType`]: a
//! `[(f32, f32); TYPE_COUNT]` of `(width, height)` indexed by that type's
//! registry id. `f32` is the game's own storage width for these fields, so this
//! is lossless versus the source data.

use lodestone_model::EntityBaseDimensions;

use crate::entity_type::EntityType;
use crate::generated_entity_dimensions::ENTITY_DIMENSIONS;
pub use crate::generated_entity_dimensions::TYPE_COUNT;

/// Resolves a validated built-in entity type to its **base** `(width, height)`
/// hitbox.
///
/// `EntityType` is constructed at the registry boundary from the raw wire id
/// or resource key. That makes this array access total: a custom, malformed,
/// or future-version type cannot be used accidentally as an index. The values
/// are base dimensions — see the module docs on why `step_height` and `scale`
/// are the consumer's responsibility.
#[must_use]
pub fn base_dimensions(entity_type: EntityType) -> EntityBaseDimensions {
    let (width, height) = ENTITY_DIMENSIONS[entity_type.registry_id() as usize];
    EntityBaseDimensions { width, height }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_type::{CustomEntityTypeId, EntityTypeRef};

    #[test]
    fn invalid_registry_ids_are_rejected_before_lookup() {
        // The raw wire boundary owns validation. This API cannot receive a
        // custom or out-of-range type, so no lookup caller can accidentally
        // turn either into a plausible built-in hitbox.
        assert_eq!(EntityType::from_registry_id(u8::MAX), None);
        assert_eq!(EntityType::from_name("not_a_real_entity"), None);
        let custom = EntityTypeRef::custom(CustomEntityTypeId::from_index(0));
        assert_eq!(custom.builtin_or_none().map(base_dimensions), None);
    }

    #[test]
    fn every_id_resolves_to_a_finite_box() {
        // The table is dense over the whole id space and every hitbox is
        // non-negative and finite. Some types are legitimately `0 x 0` — the
        // display entities (`block_display`/`item_display`/`text_display`),
        // `interaction`, `marker`, and `lightning_bolt` have no physical box —
        // so this asserts validity, not positivity; the bit-exact check against
        // the server dump is what catches a transposed or truncated table.
        for entity_type in EntityType::all() {
            let dims = base_dimensions(entity_type);
            assert!(
                dims.width >= 0.0 && dims.width.is_finite(),
                "{entity_type:?} has an invalid width {}",
                dims.width
            );
            assert!(
                dims.height >= 0.0 && dims.height.is_finite(),
                "{entity_type:?} has an invalid height {}",
                dims.height
            );
        }
    }

    #[test]
    fn dimensions_cover_every_generated_entity_type() {
        assert_eq!(u32::from(EntityType::COUNT), TYPE_COUNT);
        for entity_type in EntityType::all() {
            let dims = base_dimensions(entity_type);
            assert!(dims.width.is_finite() && dims.height.is_finite());
        }
    }

    #[test]
    fn keys_align_with_the_entity_type_table() {
        // The dimension table and the id->name table share one id space; the
        // player's box is the canonical anchor (0.6 x 1.8, constant across
        // versions), so a misaligned table fails here.
        let dims = base_dimensions(EntityType::Player);
        assert_eq!(dims.width, 0.6);
        assert_eq!(dims.height, 1.8);
    }
}
