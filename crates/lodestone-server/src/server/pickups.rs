//! Item and experience-orb pickups: which nearby entities a player absorbs on a tick and what the absorption does to the inventory.

use super::*;

/// Collects every dropped item within this player's pickup volume into their
/// inventory, and returns the native slots that changed (the item-pickup link).
///
/// This is the per-tick item-entity pickup → inventory-add chain, minus the
/// XP-orb branch. See
/// [`crate::block_drops::is_within_pickup_range`] for the volume and
/// [`PlayerInventory::add`] for the destination order — both are behavior that
/// a plausible simplification gets wrong.
///
/// # Why the whole thing happens inside one `mobs.with`
///
/// Query-then-remove across two lock acquisitions is a duplication bug with a
/// player on each side of it: two connections whose volumes overlap the same
/// drop would both see it collectable, both credit it, and one `remove_item`
/// would return `false` while the item had already been banked twice. Deciding
/// and removing under a single lock makes the loser's `remove_item` the thing
/// that fails, and it fails *before* the inventory write, so nothing is
/// duplicated.
///
/// # A full inventory leaves the item in the world
///
/// [`PlayerInventory::add`] reports its leftover, and the entity is removed
/// only when the inventory consumed everything.
/// A partial pickup therefore credits what fitted and puts the unfitted items back as
/// the item's new count — the entity stays, visibly, rather than the surplus
/// vanishing.
/// # Statistics and advancements
///
/// This is also the `minecraft:inventory_changed` seam, so it is where
/// [`AdvancementManager::on_inventory_changed`] and the `minecraft:picked_up`
/// counter are driven from. Both are credited **per item actually banked**, not
/// per entity seen: a pickup that only partly fitted credits what fitted, and one
/// that fitted nothing credits nothing — the same `written`/`leftover` split the
/// slot updates already key off.
/// One item entity a player just took, for [`ServerProtocol::encode_take_item_entity`].
#[derive(Debug, Clone, Copy)]
pub(super) struct TakenItem {
    pub(super) item_entity_id: i32,
    /// The entity's stack count **before** the inventory took any of it. Not the
    /// amount banked; see the encoder's own doc.
    pub(super) amount: i32,
}

/// What one pickup pass produced: the inventory slots to resend, and the takes to
/// announce.
///
/// **The takes are returned rather than sent here because the ordering matters more
/// than the plumbing.** The client keeps the item entity alive to interpolate it and
/// removes it once the animation finishes, so `TAKE_ITEM_ENTITY` has to reach the wire
/// *before* the `REMOVE_ENTITIES` that `stream_pass` derives from this same removal.
/// Returning them puts that ordering in the caller, where `stream_pass` is visible;
/// sending from inside `mobs.with` would also mean awaiting under the sim lock.
#[derive(Debug, Default)]
pub(super) struct Pickups {
    /// Native inventory slot indices whose contents changed.
    pub(super) changed: Vec<usize>,
    /// Items taken this pass, in pickup order.
    pub(super) takes: Vec<TakenItem>,
}

pub(super) fn collect_nearby_items(
    mobs: &MobHandle,
    inventory: &mut PlayerInventory,
    player_feet: Vec3,
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    // The world clock in milliseconds — `game_time * 50`. Vanilla stamps a
    // criterion with a real `Instant`; this crate must not call
    // `std::time::Instant::now()` anywhere in `lodestone-server`, because the crate
    // links into a wasm32 bundle where that compiles and then panics at runtime
    // under `panic = "abort"` with no log line. A tick-derived value is monotonic,
    // wasm-safe, and means "ms of world time", which is a more useful stamp for a
    // save file than wall clock anyway.
    obtained_millis: i64,
) -> Pickups {
    let mut changed: Vec<usize> = Vec::new();
    let mut takes: Vec<TakenItem> = Vec::new();
    mobs.with(|sim| {
        for (id, item, count) in sim.items_within_pickup_range(player_feet) {
            let stack = ItemStack::new(item, u32::from(count));
            let picked_up_key = crate::advancements::StatKey::new(
                crate::advancements::StatType::PickedUp,
                stack.item.to_string(),
            );
            let item_id = stack.item.to_string();
            let offered = stack.count;
            let (written, leftover) = inventory.add(stack);
            let banked = offered.saturating_sub(leftover.as_ref().map_or(0, |left| left.count));
            match leftover {
                None => {
                    // Fully banked, so the entity goes. `remove_item` returning
                    // `false` would mean another connection took it between the
                    // query and here — impossible under this one lock, which is
                    // the property the doc comment above is about.
                    sim.remove_item(id);
                }
                Some(remaining) => {
                    // Partial fit (or none at all): the inventory keeps what
                    // fitted and the entity keeps the rest, exactly as vanilla's
                    // in-place `ItemStack` shrink does. Clamped into `u8`
                    // because that is what the lifecycle counts in; `remaining`
                    // can never exceed the `count` we started from, which came
                    // from that same `u8`.
                    let left = u8::try_from(remaining.count).unwrap_or(u8::MAX);
                    if left == 0 {
                        sim.remove_item(id);
                    } else {
                        sim.set_item_count(id, left);
                    }
                }
            }
            // How much actually landed in the inventory. `leftover` is what did
            // not, so the banked amount is the difference — credited rather than
            // the offered count, so a full inventory credits nothing.
            if banked > 0 {
                advancements.award_stat(
                    player_uuid,
                    picked_up_key,
                    i32::try_from(banked).unwrap_or(i32::MAX),
                );
                // Vanilla's `inventory_changed` trigger. Fires once per pickup
                // regardless of stack size, because a criterion is satisfied by
                // *having* the item, not by how many.
                advancements.on_inventory_changed(player_uuid, &item_id, obtained_millis);
            }
            // The pickup *animation* cue. Gated on `banked > 0` because the
            // animation belongs only to a transfer that placed at least one
            // item. A pickup into a full inventory shows nothing, which is right:
            // nothing was taken.
            //
            // `offered`, not `banked`: vanilla passes `orgCount`, captured *before*
            // `add` shrinks the stack in place. The two differ exactly when the
            // pickup is partial, and `orgCount` is what drives the client's sound
            // pitch. See `ServerProtocol::encode_take_item_entity`.
            if banked > 0 {
                takes.push(TakenItem {
                    item_entity_id: id,
                    amount: i32::try_from(offered).unwrap_or(i32::MAX),
                });
            }
            for slot in written {
                if !changed.contains(&slot) {
                    changed.push(slot);
                }
            }
        }
    });
    Pickups { changed, takes }
}

/// Vanilla's own `takeXpDelay` field, the value its own experience-orb player-touch routine resets it to.
///
/// Two ticks, so a player standing in a pile absorbs one orb every other tick rather
/// than all of them at once. It is what makes a big drop *sound* and *look* like a
/// stream of orbs instead of a single silent jump on the bar, and it is the only thing
/// limiting the absorption rate — an orb has no pickup delay of its own.
pub(super) const TAKE_XP_DELAY_TICKS: i32 = 2;

/// One orb absorption, for the caller to announce.
#[derive(Debug, Clone, Copy)]
pub(super) struct AbsorbedOrb {
    pub(super) orb_entity_id: i32,
    /// Points paid out by this absorption — one orb's `value`, not the whole pile's.
    pub(super) points: i32,
}

/// Absorbs at most one nearby experience orb into `experience` during the pickup
/// sweep.
///
/// # Why at most one
///
/// The **player's** pickup delay rejects every orb while non-zero and resets to `2`
/// on each absorption, so the sweep can take only one orb per two
/// ticks no matter how many are overlapping. Draining every overlapping orb in one pass
/// would bank the same total, which is exactly why it is worth stating: the difference is
/// invisible in the final number and obvious on screen, because the client plays one
/// pickup sound per `TAKE_ITEM_ENTITY` and animates one orb per absorption.
///
/// `delay` is the caller's own copy of the pickup delay, decremented here once per call —
/// this runs on the same movement-driven cadence the item pickup does.
///
/// Returns the absorption to announce, if one happened. The points are already in
/// `experience`; the caller owes the wire a `set_experience`.
pub(super) fn collect_nearby_orbs(
    mobs: &MobHandle,
    player_feet: Vec3,
    experience: &mut crate::experience::PlayerExperience,
    delay: &mut i32,
) -> Option<AbsorbedOrb> {
    if *delay > 0 {
        *delay -= 1;
        return None;
    }
    mobs.with(|sim| {
        let (orb_entity_id, _) = sim.orbs_within_pickup_range(player_feet).into_iter().next()?;
        let points = sim.take_orb(orb_entity_id)?;
        *delay = TAKE_XP_DELAY_TICKS;
        experience.give_points(points);
        Some(AbsorbedOrb {
            orb_entity_id,
            points,
        })
    })
}
