//! Per-entity-type **interaction census** for protocol 776 (Minecraft 26.2):
//! which entity types can shove the local player or hard-block another entity's
//! movement, keyed by network registry id.
//!
//! # The push question this answers, precisely
//!
//! `lodestone_physics::push` models the local player as the *pushee* of the
//! game's crowd-push pass, which every living entity runs each tick. From that
//! side, the player's own pushability flag is what the candidate filter tests,
//! which is why `lodestone_physics::push::pair_admitted` takes `self_pushable`
//! and never reads the neighbour's. The neighbour's contribution is entirely a
//! *type* question: **does an entity of this type run a crowd pass that
//! reaches the player?**
//!
//! That is a conjunction of three facts, all read from the game's own
//! behaviour:
//!
//! 1. Only the living-entity hierarchy's per-tick update runs the crowd pass
//!    at all. A non-living entity therefore never runs it.
//! 2. The crowd pass itself must not have been overridden into something that
//!    cannot reach the player. One flying mob overrides it to an empty body;
//!    a decorative stand overrides it to only shove ridable minecarts.
//! 3. The pairwise-push step that the crowd pass ultimately calls must still
//!    push a player argument. One tameable bird overrides it to skip players
//!    outright. The flying mob and the decorative stand from (2) also empty
//!    it out. A few mobs with on-hit side effects (a retargeting golem, an
//!    exploding cube, a warden) add behaviour and then still call through, so
//!    they do push.
//!
//! # Why this is default-**deny**
//!
//! Only the living-entity hierarchy is pushable at all, and only it runs the
//! crowd pass — every other entity defaults to not-pushable. So "not a
//! pusher" is the game's own default and must be this table's too: an
//! unrecognised or future entity type is reported as **not** a pusher. The
//! inverse — a denylist of known-inert types — makes every type nobody
//! remembered, and every type a later version adds, silently able to shove
//! the player, which is the single most visible way for entity push to be
//! wrong (a dropped item nudging you across a floor).
//!
//! # Mechanisms deliberately **excluded**
//!
//! Boats and rideable minecarts *do* push players in the real game, through
//! their own tick-side passes rather than the ordinary crowd pass:
//!
//! * The boat family's own tick step seats or pushes everything in an
//!   inflated query box, and its push override adds a vertical-ordering
//!   condition on top.
//! * The newer minecart movement behaviour runs its own push pass only while
//!   the minecart is in its rideable state, querying a slightly inflated box
//!   and pushing anything that is a player.
//!
//! Neither can be folded into this census without also changing the gate:
//! different query-box inflation, an extra vertical condition, and a
//! rideable-state test that is per-*type* but not one of the columns dumped
//! here. They are reported as `false` — matching what `lodestone_physics::push`
//! models today — rather than approximated. The census dump carries each
//! type's implementation class precisely so those passes can be modelled
//! later without re-dumping.
//!
//! The separate hard-collision column answers whether another entity can
//! hard-block movement against this one. The exhaustive override families are
//! boats, the shulker, and the happy ghast; the generated table records their
//! implementation classes, with per-instance alive/state refinements left to
//! the consumer. Unknown ids default-deny this capability too.
//!
//! # Data source: interrogate the real game
//!
//! Generated from `tests/support/entity_census_jvm.txt`, a dump from a
//! headless build of the real 26.2 server that walks the live entity-type
//! registry and reports, per type, its implementation class, whether that
//! class is part of the living-entity hierarchy, and which class in its
//! hierarchy declares the crowd-push override and the pairwise-push override.
//! Every column is mechanically derived; the reduction to the boolean below
//! happens in `tests/entity_census.rs`, where it sits next to the citations
//! above and fails closed on an override site it has never seen.
//! `vendor/minecraft-data` has no 26.x data at all and is not a source here.
//!
//! # Memory design
//!
//! Pure rodata, zero heap, O(1) by id: a `[bool; TYPE_COUNT]` indexed by the
//! network entity-type registry id — the same id space as [`crate::entity_types`]
//! and [`crate::entity_dimensions`].

use crate::generated_entity_census::{
    ENTITY_CAN_BE_COLLIDED_WITH, ENTITY_IS_LIVING, ENTITY_IS_MOB, ENTITY_PUSHES_PLAYERS,
};
pub use crate::generated_entity_census::TYPE_COUNT;

/// Whether an entity of this network type id belongs to the game's
/// living-entity hierarchy.
///
/// Returns `None` for ids outside `0..TYPE_COUNT`.
///
/// # This is not [`pushes_players`] and must not be derived from it
///
/// The push census is a *reduction* of this column plus two override sites, and
/// three living types reduce to `false`: the bat, the parrot and the armor
/// stand. Reading the push table as an is-living test therefore misclassifies
/// exactly the entities whose arm poses matter (an armour stand holds items).
///
/// # Why a consumer wants it: metadata index 8 is ambiguous
///
/// The living-entity flags byte (the using-item bitfield behind a bow draw)
/// is assigned metadata index 8 by the metadata-id allocator's
/// declaration-order counter — and a non-living projectile's own flags byte
/// lands at the same index. Both are plain-byte fields, so the wire cannot
/// tell them apart and a decoder that surfaced every index-8 byte as "living
/// flags" would read an arrow's crit bit as "this arrow is drawing a bow".
/// The disambiguation needs the entity's concrete *type*, which is what this
/// table supplies.
#[must_use]
pub fn is_living(id: i32) -> Option<bool> {
    usize::try_from(id)
        .ok()
        .and_then(|index| ENTITY_IS_LIVING.get(index).copied())
}

/// Whether an entity of this network type id belongs to the game's AI-mob
/// subset of the living-entity hierarchy.
///
/// Returns `None` for ids outside `0..TYPE_COUNT`. An unrecognised id must be
/// read as **not** a mob, for the same default-deny reason as everything else
/// here: the consumer is a metadata guard, and guessing wrong surfaces a byte
/// that means something else.
///
/// # This is not [`is_living`], and index 15 is why
///
/// The AI-mob flags byte — no-AI, left-handed, and **aggressive** — lands at
/// metadata index **15**. Index 15 has three claimants in 26.2, all plain
/// bytes:
///
/// | owner | field | its high bit means |
/// |---|---|---|
/// | AI mobs | mob flags | aggressive |
/// | armor stands | client flags | show arms |
/// | display entities | billboard-constraint flags | (an enum ordinal) |
///
/// This is the same shape of hazard as index 8 with one extra twist: **an
/// armor stand is part of the living-entity hierarchy**, so [`is_living`]
/// does *not* resolve it. An armour stand with arms shown — the common
/// decorative case — would report itself as an aggressive mob and, holding a
/// bow, draw it. Hence a third column rather than a reuse of the second.
///
/// The collision was read off the real game, not reasoned about: see
/// `crates/versions/26.2/tests/support/entity_data_index_jvm.txt`, which dumps
/// every metadata field in the game sorted by index so collisions are
/// adjacent lines.
#[must_use]
pub fn is_mob(id: i32) -> Option<bool> {
    usize::try_from(id)
        .ok()
        .and_then(|index| ENTITY_IS_MOB.get(index).copied())
}

/// Whether an entity of this network type id can shove the local player
/// through the game's ordinary crowd-push pass.
///
/// Returns `None` for ids outside `0..TYPE_COUNT`. Callers that cannot
/// distinguish "unknown id" from "not a pusher" should treat both as **not a
/// pusher** — see the module docs on why the default is deny.
///
/// This is a *type*-level capability: the per-instance refinements the real
/// game layers on top (being alive, being a spectator, climbing something,
/// being ridden, digging or emerging, or a creaking's own mobility gate) are
/// runtime state, not per-type facts, and are the consumer's to apply where
/// it has them. The value here is the maximum over runtime state, so a `true`
/// means "can, in some state", never "always does".
#[must_use]
pub fn pushes_players(id: i32) -> Option<bool> {
    usize::try_from(id)
        .ok()
        .and_then(|index| ENTITY_PUSHES_PLAYERS.get(index).copied())
}

/// Whether an entity of this network type can participate in another entity's
/// hard movement collision.
///
/// This is independent of [`pushes_players`]: boats are hard colliders but do
/// not run the ordinary crowd-push pass, while ordinary mobs do the reverse.
/// Per-instance predicates remain the consumer's responsibility (the shulker
/// must be alive, and the happy ghast has its own state gate). Unknown ids
/// return `None`, which callers must treat as default-deny.
#[must_use]
pub fn can_be_collided_with(id: i32) -> Option<bool> {
    usize::try_from(id)
        .ok()
        .and_then(|index| ENTITY_CAN_BE_COLLIDED_WITH.get(index).copied())
}

/// The widest and tallest base hitbox among entity types that can push the
/// player.
///
/// A consumer sizing a coarse "is this candidate even close enough to check"
/// filter — e.g. `lodestone_shell::sim`'s `NEARBY_ENTITY_RADIUS` — needs the
/// bound to follow from *this* census rather than restate its maxima as a
/// literal, which is exactly how that constant went stale before: it was
/// `4.0`, sized for "a happy-ghast-sized neighbour" back when every candidate
/// was compared against the player's own `0.6 × 1.8` box, and stayed `4.0`
/// long after wider pushers existed in the census.
///
/// Returns `None` only if no entity type in the census currently pushes
/// players — i.e. the census itself is degenerate — so a caller can fail
/// closed (or fall back to a floor) rather than silently filter with `(0.0,
/// 0.0)`.
#[must_use]
pub fn pusher_max_dimensions() -> Option<(f32, f32)> {
    (0..TYPE_COUNT as i32)
        .filter(|&id| pushes_players(id) == Some(true))
        .filter_map(crate::entity_dimensions::base_dimensions)
        .fold(None, |acc, dims| {
            Some(match acc {
                Some((width, height)) => (width.max(dims.width), height.max(dims.height)),
                None => (dims.width, dims.height),
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_types::entity_type_id;

    fn by_name(name: &str) -> bool {
        let id = entity_type_id(name).unwrap_or_else(|| panic!("{name} is a real entity type"));
        pushes_players(id).unwrap_or_else(|| panic!("{name} is inside the census"))
    }

    #[test]
    fn out_of_range_ids_are_none() {
        assert_eq!(pushes_players(-1), None);
        assert_eq!(pushes_players(TYPE_COUNT as i32), None);
        assert_eq!(pushes_players(i32::MAX), None);
    }

    #[test]
    fn the_census_is_neither_all_true_nor_all_false() {
        // A table stuck at one value would satisfy every "X is (not) a pusher"
        // assertion of that polarity, so pin both populations by size. The split
        // is the dump's 93 living-hierarchy types minus the three that cannot
        // reach a player (armor_stand, bat, parrot): 90 pushers and 68
        // non-pushers out of 158.
        let pushers = (0..TYPE_COUNT as i32)
            .filter(|&id| pushes_players(id) == Some(true))
            .count();
        assert_eq!(pushers, 90, "unexpected pusher population");
        assert_eq!(TYPE_COUNT as usize - pushers, 68, "unexpected non-pusher population");
    }

    #[test]
    fn ordinary_living_mobs_push() {
        // Plain living-entity subtypes with no override: the majority case.
        for name in [
            "minecraft:zombie",
            "minecraft:creeper",
            "minecraft:cow",
            "minecraft:villager",
            "minecraft:player",
        ] {
            assert!(by_name(name), "{name} should push the player");
        }
    }

    #[test]
    fn non_living_entities_do_not_push() {
        // The control that matters: nothing outside the living-entity hierarchy
        // runs the crowd pass, so none of these may shove the player. `item` and
        // `arrow` are the two the briefing for this table names by hand;
        // `_display`/`marker`/`interaction` are the ones a denylist forgets.
        for name in [
            "minecraft:item",
            "minecraft:arrow",
            "minecraft:experience_orb",
            "minecraft:falling_block",
            "minecraft:tnt",
            "minecraft:item_display",
            "minecraft:block_display",
            "minecraft:text_display",
            "minecraft:marker",
            "minecraft:interaction",
            "minecraft:painting",
            "minecraft:end_crystal",
        ] {
            assert!(!by_name(name), "{name} must not push the player");
        }
    }

    #[test]
    fn the_three_living_types_that_cannot_reach_a_player_do_not_push() {
        // Read from the real game, not inferred from this table: the bat empties
        // its crowd pass, the armor stand narrows it to ridable minecarts, and
        // the parrot's pairwise-push override skips a player argument outright.
        // All three belong to the living-entity hierarchy, so an "is it living"
        // census alone would get all three wrong.
        for name in ["minecraft:bat", "minecraft:armor_stand", "minecraft:parrot"] {
            assert!(!by_name(name), "{name} must not push the player");
        }
    }

    #[test]
    fn living_types_that_only_decorate_do_push() {
        // The iron golem, the sulfur cube and the warden override their
        // pairwise-push step to add a side effect and then still call through,
        // so the push still lands. These are
        // the counterweight to the test above: an override site is not by itself
        // a reason to exclude.
        for name in [
            "minecraft:iron_golem",
            "minecraft:sulfur_cube",
            "minecraft:warden",
        ] {
            assert!(by_name(name), "{name} should push the player");
        }
    }

    #[test]
    fn the_vehicle_families_are_excluded_as_unmodelled() {
        // Not "the game says no" — boats and rideable minecarts *do* push
        // players in the real game, through their own tick-side passes. They
        // are excluded because `lodestone_physics::push` models the ordinary
        // crowd-push pass only; see the module docs. This test pins the
        // current, deliberate scope so a future pass that models them has to
        // update it on purpose.
        for name in [
            "minecraft:oak_boat",
            "minecraft:oak_chest_boat",
            "minecraft:bamboo_raft",
            "minecraft:minecart",
            "minecraft:chest_minecart",
            "minecraft:hopper_minecart",
        ] {
            assert!(!by_name(name), "{name} is out of scope and must report false");
        }
    }

    #[test]
    fn the_census_aligns_with_the_dimension_table() {
        // The census, the id→name table and the dimension table share one id
        // space; a table built from a mis-sorted dump would fail here.
        use crate::entity_dimensions::base_dimensions;
        assert_eq!(TYPE_COUNT, crate::entity_dimensions::TYPE_COUNT);
        assert_eq!(TYPE_COUNT, crate::entity_types::TYPE_COUNT);
        let zombie = entity_type_id("minecraft:zombie").expect("zombie is real");
        let dims = base_dimensions(zombie).expect("zombie has dimensions");
        assert_eq!((dims.width, dims.height), (0.6, 1.95));
    }

    #[test]
    fn pusher_max_dimensions_matches_the_known_widest_and_tallest() {
        use crate::entity_dimensions::base_dimensions;

        // The two claimants documented at `lodestone_shell::sim::NEARBY_ENTITY_RADIUS`:
        // `ender_dragon` is the widest pusher (16.0) and `giant` is the tallest
        // (12.0), both part of the living-entity hierarchy with no push-suppressing
        // override. Pinned by name so a census update that changes either maximum
        // fails here first, rather than silently shrinking a consumer's derived
        // radius.
        assert!(by_name("minecraft:ender_dragon"));
        assert!(by_name("minecraft:giant"));
        let dragon = base_dimensions(entity_type_id("minecraft:ender_dragon").unwrap()).unwrap();
        let giant = base_dimensions(entity_type_id("minecraft:giant").unwrap()).unwrap();
        assert_eq!(dragon.width, 16.0, "ender_dragon is expected to be the widest pusher");
        assert_eq!(giant.height, 12.0, "giant is expected to be the tallest pusher");

        let (max_width, max_height) =
            pusher_max_dimensions().expect("the push census is non-empty");
        assert_eq!(max_width, dragon.width, "widest pusher width should win the fold");
        assert_eq!(max_height, giant.height, "tallest pusher height should win the fold");
    }

    #[test]
    fn pusher_max_dimensions_is_none_only_for_a_degenerate_census() {
        // Control for the `None` branch a consumer must handle: it can only
        // occur if the census has no pushers at all. The real table has 90 (see
        // `the_census_is_neither_all_true_nor_all_false`), so this is a live
        // assertion the table is not that degenerate, not a description.
        assert!(pusher_max_dimensions().is_some());
    }
}
