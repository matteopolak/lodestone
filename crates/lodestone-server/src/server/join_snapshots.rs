//! Inventory, experience and attribute snapshots: what a joining client is told, and republishing them to the player registry on change.

use super::*;

/// The join snapshot's window counter is **`1`, not `0`**. The initial content
/// frame increments the counter from zero before sending it.
///
/// # Why the other window-`0` sends in this file can keep their constant `0`
///
/// The client accepts the counter on content and slot frames; this server does
/// not validate the echoed value on clicks. Other window-`0` updates therefore
/// retain their constant `0`, while the join snapshot uses the initial counter
/// value required by the opening sequence.
pub(super) const JOIN_CONTENT_STATE_ID: i32 = 1;

/// Sends the joining player's window-`0` inventory snapshot. The snapshot
/// contains every menu slot and the carried cursor stack, so the client can
/// render the inventory before any click or movement packet arrives.
///
/// The snapshot is sent at the top of [`serve_play`], after the login metadata
/// and before the deferred chunk stream. Window `0` uses
/// `encode_container_content` because the content frame carries a slot list
/// and cursor; a single-slot frame cannot represent the whole inventory.
/// `JOIN_CONTENT_STATE_ID` is `1`, the first counter assigned to this window.
pub(super) fn join_inventory_snapshot<P: ServerProtocol>(
    proto: &P,
    inventory: &PlayerInventory,
) -> ServerDirective {
    let items = read_menu(
        &MenuLayout::player(),
        inventory,
        Some(inventory.crafting()),
        &[],
    );
    proto.encode_container_content(
        0,
        JOIN_CONTENT_STATE_ID,
        &items,
        inventory.click_state().carried.as_ref(),
    )
}

/// Sends the experience bar snapshot owed to a joining player.
///
/// # Why this exists
///
/// The frame is sent once at join and after every
/// [`crate::experience::PlayerExperience`] mutation, including furnace XP.
/// This keeps the bar populated in every game mode and after both level and
/// progress changes.
///
/// # Argument order
///
/// `(progress, level, total)` is the order required by the protocol encoder.
/// Keep the two integer fields explicit here because swapping adjacent VarInts
/// still produces a valid frame with incorrect values.
pub(super) fn join_experience<P: ServerProtocol>(
    proto: &P,
    experience: &crate::experience::PlayerExperience,
) -> ServerDirective {
    proto.encode_set_experience(
        experience.progress(),
        experience.level(),
        experience.total(),
    )
}

/// [`PlayerRegistry::set_experience`]'s producer half — call this everywhere
/// [`join_experience`]/`encode_set_experience` is sent to the owning
/// connection. The wrapper keeps the optional registry check in one place.
pub(super) fn republish_experience(players: Option<&PlayerRegistry>, uuid: uuid::Uuid, experience: &crate::experience::PlayerExperience) {
    if let Some(registry) = players {
        registry.set_experience(uuid, experience.level(), experience.query_points());
    }
}

/// Publishes the connection-owned inventory as an owned host-observation
/// snapshot. The registry never becomes an inventory writer.
pub(super) fn republish_inventory(players: Option<&PlayerRegistry>, uuid: uuid::Uuid, inventory: &PlayerInventory) {
    if let Some(registry) = players {
        registry.set_inventory(uuid, inventory);
    }
}

/// The local player's combat-relevant attributes as wire-shaped snapshots —
/// [`PlayerInventory::combat_stats`]'s already-folded `AttributeMap`, one
/// snapshot per **named** attribute below, each carrying its final value as
/// `base` and an empty modifier list.
///
/// # Every attribute is named explicitly — this is not `AttributeMap::iter`
///
/// `AttributeMap` is sparse: an attribute only appears in it once *something*
/// has touched it ([`lodestone_entity::equipment::apply_equipment`] calls
/// `get_or_default` only for a piece that is actually equipped). Iterating it
/// therefore **omits** `minecraft:armor` entirely the moment the last piece
/// comes off, rather than including it at `0.0` — and the client's own merge
/// (`lodestone_ecs::ingest::apply_entity_attributes`) treats an attribute
/// absent from a packet as *unchanged*, not as *reset to default*: it only
/// overwrites entries the packet actually names. The reported symptom was
/// exactly this — the bar tracked every equip and every partial removal
/// correctly (a non-zero value was always sent) and then froze on the last
/// piece, because that transition was the one case where the whole attribute
/// stopped being sent rather than being sent as zero. Reading each attribute
/// through [`lodestone_entity::attribute::AttributeMap::value`] instead —
/// which already falls back to the registry default for an attribute the map
/// has no entry for — closes that gap for every attribute named here, not
/// only `armor`.
///
/// # Why empty modifiers
///
/// Rather than re-publishing the per-item ones `apply_equipment` built the
/// fold from: the client's own fold
/// (`instance_from_snapshot`/`AttributeInstance::value`,
/// `crates/lodestone-entity/src/attribute.rs`) is a no-op over a bare base
/// value with no modifiers, and re-deriving the exact same modifier ids and
/// operations at the wire would be a second copy of
/// `lodestone_entity::equipment`'s table to keep in step for no observable
/// difference — the client never inspects an individual modifier, only the
/// folded result (the shell's `Session::armour_value` and the attack-speed
/// and water-efficiency readers documented alongside it).
pub(super) fn player_attribute_snapshots(inventory: &PlayerInventory) -> Vec<EntityAttributeSnapshot> {
    // Every attribute `lodestone_entity::equipment::item_modifiers` can ever
    // publish a modifier for. Adding a new equipment-driven attribute there
    // means adding its name here too, or it inherits this exact bug for
    // itself.
    const COMBAT_ATTRIBUTES: [&str; 4] = [
        "minecraft:armor",
        "minecraft:armor_toughness",
        "minecraft:knockback_resistance",
        "minecraft:attack_damage",
    ];
    let attrs = inventory.combat_stats().attributes;
    COMBAT_ATTRIBUTES
        .into_iter()
        .filter_map(|name| {
            let attribute: lodestone_model::Identifier = name.parse().ok()?;
            let base = attrs.value(&attribute)?;
            Some(EntityAttributeSnapshot {
                attribute,
                base,
                modifiers: Vec::new(),
            })
        })
        .collect()
}

/// One folded maximum-health snapshot for the local player's active effects.
///
/// This travels through the existing local-player attribute packet rather than
/// inventing a status-effect-specific health wire path. The client already
/// merges that packet into its attribute component, which is also where the
/// HUD obtains the number of heart rows.
pub(super) fn max_health_snapshot(max_health: f32) -> EntityAttributeSnapshot {
    EntityAttributeSnapshot {
        attribute: "minecraft:max_health"
            .parse()
            .expect("built-in max-health attribute identifier"),
        base: f64::from(max_health),
        modifiers: Vec::new(),
    }
}

/// Sends [`player_attribute_snapshots`] as an `update_attributes` packet —
/// the producer half of the armour bar. The client half
/// (`Session::armour_value`, `lodestone_shell::hud`, the v770 adapter's
/// `UPDATE_ATTRIBUTES` decode) was already complete; this crate had no
/// encoder at all, so the HUD row read a permanent `None` no matter what was
/// equipped.
///
/// Sent once at join (a client that never receives this packet has no armour
/// attribute at all, not a zero one) and
/// again after any player-inventory mutation that can change combat
/// equipment (`ServerBound::ContainerClicked`, the right-click armour swap in
/// [`apply_use_item_on`]).
pub(super) fn join_attributes<P: ServerProtocol>(proto: &P, inventory: &PlayerInventory) -> ServerDirective {
    proto.encode_update_attributes(&player_attribute_snapshots(inventory))
}
