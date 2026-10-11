//! The NBT view of command targets, shared by `/data ... entity` and the
//! `nbt=` selector option.
//!
//! # What it is
//!
//! [`target_nbt`] returns the NBT of any selectable entity. A world entity's
//! comes from `MobSim::entity_nbt`, which is the save path's own record, so the
//! view, a world save and `/summon` read and write one shape. A player's comes
//! from the player-data writer: the connection that owns the player puts that
//! compound on its `PlayerCandidate`; a candidate without one (a test, or
//! another connection's roster entry) falls back to a view built from the
//! fields the roster does hold. Players also carry `SelectedItem`, the held
//! stack, as the reference does for reads.
//!
//! # How to change it
//!
//! A new field on an entity is added to its save record, and the view, the
//! selector and `/data merge` pick it up with no change here. The conversion
//! between [`Nbt`] and [`SnbtValue`] lives here because the command layer
//! parses and prints SNBT while the sim stores binary NBT.
//!
//! # Gaps
//!
//! `/data` has no `block` target: reaching a block entity needs the chunk
//! source's block-entity store on `CommandWorld`, which commands do not hold.

use std::collections::HashMap;

use lodestone_command_mc::{SnbtValue, compare_nbt};
use lodestone_core::{Nbt, NbtTag};
use uuid::Uuid;

use super::registrar::{CommandWorld, Ctx};
use super::source::SelectorError;
use super::source::{CommandTarget, PlayerCandidate};

/// Binary NBT to the command layer's value type.
pub(super) fn nbt_to_snbt(nbt: &Nbt) -> SnbtValue {
    match nbt {
        Nbt::End => SnbtValue::Compound(Vec::new()),
        Nbt::Byte(v) => SnbtValue::Byte(*v),
        Nbt::Short(v) => SnbtValue::Short(*v),
        Nbt::Int(v) => SnbtValue::Int(*v),
        Nbt::Long(v) => SnbtValue::Long(*v),
        Nbt::Float(v) => SnbtValue::Float(*v),
        Nbt::Double(v) => SnbtValue::Double(*v),
        Nbt::String(v) => SnbtValue::String(v.clone()),
        Nbt::ByteArray(v) => SnbtValue::ByteArray(v.clone()),
        Nbt::IntArray(v) => SnbtValue::IntArray(v.clone()),
        Nbt::LongArray(v) => SnbtValue::LongArray(v.clone()),
        Nbt::List { elements, .. } => SnbtValue::List(elements.iter().map(nbt_to_snbt).collect()),
        Nbt::Compound(fields) => {
            SnbtValue::Compound(fields.iter().map(|(name, value)| (name.clone(), nbt_to_snbt(value))).collect())
        }
    }
}

/// The command layer's value type to binary NBT.
pub(super) fn snbt_to_nbt(value: &SnbtValue) -> Nbt {
    match value {
        SnbtValue::Byte(v) => Nbt::Byte(*v),
        SnbtValue::Short(v) => Nbt::Short(*v),
        SnbtValue::Int(v) => Nbt::Int(*v),
        SnbtValue::Long(v) => Nbt::Long(*v),
        SnbtValue::Float(v) => Nbt::Float(*v),
        SnbtValue::Double(v) => Nbt::Double(*v),
        SnbtValue::String(v) => Nbt::String(v.clone()),
        SnbtValue::ByteArray(v) => Nbt::ByteArray(v.clone()),
        SnbtValue::IntArray(v) => Nbt::IntArray(v.clone()),
        SnbtValue::LongArray(v) => Nbt::LongArray(v.clone()),
        SnbtValue::Compound(entries) => {
            Nbt::Compound(entries.iter().map(|(name, value)| (name.clone(), snbt_to_nbt(value))).collect())
        }
        SnbtValue::List(items) => {
            let element_type = match items.first() {
                None => NbtTag::End,
                Some(SnbtValue::Byte(_)) => NbtTag::Byte,
                Some(SnbtValue::Short(_)) => NbtTag::Short,
                Some(SnbtValue::Int(_)) => NbtTag::Int,
                Some(SnbtValue::Long(_)) => NbtTag::Long,
                Some(SnbtValue::Float(_)) => NbtTag::Float,
                Some(SnbtValue::Double(_)) => NbtTag::Double,
                Some(SnbtValue::String(_)) => NbtTag::String,
                Some(SnbtValue::ByteArray(_)) => NbtTag::ByteArray,
                Some(SnbtValue::IntArray(_)) => NbtTag::IntArray,
                Some(SnbtValue::LongArray(_)) => NbtTag::LongArray,
                Some(SnbtValue::List(_)) => NbtTag::List,
                Some(SnbtValue::Compound(_)) => NbtTag::Compound,
            };
            Nbt::List { element_type, elements: items.iter().map(snbt_to_nbt).collect() }
        }
    }
}

/// A player's NBT: the connection's save-shaped compound when the roster entry
/// has one, else a view built from the roster fields, plus `UUID`, `Tags` and
/// the held stack as `SelectedItem`.
fn player_nbt(world: &CommandWorld<'_>, player: &PlayerCandidate) -> Nbt {
    let mut fields = match player.nbt.as_deref() {
        Some(Nbt::Compound(fields)) => fields.clone(),
        _ => {
            let data = crate::player_data::PlayerData {
                pos: player.position,
                rotation: player.rotation,
                game_mode: Some(player.game_mode),
                ..crate::player_data::PlayerData::default()
            };
            match data.to_nbt() {
                Ok(Nbt::Compound(fields)) => fields,
                _ => Vec::new(),
            }
        }
    };
    fields.retain(|(name, _)| !matches!(name.as_str(), "UUID" | "Tags" | "SelectedItem"));
    fields.push(("UUID".to_owned(), Nbt::IntArray(crate::entity_record::uuid_to_ints(player.uuid))));
    if let Some(mobs) = world.mobs {
        let tags = mobs.with(|sim| sim.entity_tags(player.uuid));
        if !tags.is_empty() {
            fields.push((
                "Tags".to_owned(),
                Nbt::List { element_type: NbtTag::String, elements: tags.into_iter().map(Nbt::String).collect() },
            ));
        }
    }
    if let Some(held) = selected_item(&fields) {
        fields.push(("SelectedItem".to_owned(), held));
    }
    Nbt::Compound(fields)
}

/// The held stack of a player compound: the `Inventory` entry in the
/// `SelectedItemSlot` slot, without its `Slot` key.
fn selected_item(fields: &[(String, Nbt)]) -> Option<Nbt> {
    let selected = fields.iter().find_map(|(k, v)| match (k.as_str(), v) {
        ("SelectedItemSlot", Nbt::Int(slot)) => Some(*slot),
        _ => None,
    })?;
    let Some((_, Nbt::List { elements, .. })) = fields.iter().find(|(k, _)| k == "Inventory") else {
        return None;
    };
    elements.iter().find_map(|entry| {
        let Nbt::Compound(stack) = entry else { return None };
        let slot = stack.iter().find_map(|(k, v)| match (k.as_str(), v) {
            ("Slot", Nbt::Byte(slot)) => Some(i32::from(*slot)),
            _ => None,
        })?;
        (slot == selected)
            .then(|| Nbt::Compound(stack.iter().filter(|(k, _)| k != "Slot").cloned().collect()))
    })
}

/// The NBT of `target`; `None` when it vanished between resolution and now.
pub(super) fn target_nbt(world: &CommandWorld<'_>, target: &CommandTarget) -> Option<Nbt> {
    match target {
        CommandTarget::Player(player) => Some(player_nbt(world, player)),
        CommandTarget::World(entity) => world.mobs?.with(|sim| sim.entity_nbt(entity.uuid)),
    }
}

/// The NBT of every entity a selector could match, by uuid, for `nbt=`.
pub(super) fn nbt_table(world: &CommandWorld<'_>) -> HashMap<Uuid, SnbtValue> {
    let mut table: HashMap<Uuid, SnbtValue> = world
        .mobs
        .map(|mobs| mobs.with(|sim| sim.entity_nbt_table()))
        .unwrap_or_default()
        .into_iter()
        .map(|(uuid, nbt)| (uuid, nbt_to_snbt(&nbt)))
        .collect();
    for player in world.players {
        table.insert(player.uuid, nbt_to_snbt(&player_nbt(world, player)));
    }
    table
}

/// Whether the entity with `uuid` has NBT containing `pattern`.
pub(super) fn contains(table: &HashMap<Uuid, SnbtValue>, uuid: Uuid, pattern: &[(String, SnbtValue)]) -> bool {
    table.get(&uuid).is_some_and(|held| compare_nbt(&SnbtValue::Compound(pattern.to_vec()), held))
}

/// The one entity a single-entity selector argument names.
///
/// # Errors
///
/// "No entity was found" for no match; the reference refusal when the
/// selector allows more than one.
pub(super) fn resolve_single(
    ctx: &Ctx<'_>,
    selector: &lodestone_command_mc::EntitySelector,
) -> Result<CommandTarget, String> {
    let mut targets = ctx.resolve_targets(selector).map_err(|error| match error {
        SelectorError::NoPlayersFound => "No entity was found".to_owned(),
        other => other.to_string(),
    })?;
    if targets.len() > 1 {
        return Err("Only one entity is allowed, but the provided selector allows more than one".to_owned());
    }
    targets.pop().ok_or_else(|| "No entity was found".to_owned())
}
