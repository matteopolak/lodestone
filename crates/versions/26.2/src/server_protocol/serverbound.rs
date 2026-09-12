//! Serverbound packet decoding primitives.
//!
//! This private module is part of the V770ServerProtocol facade. Its
//! re-exported helpers preserve the existing public API and wire behaviour.

use super::*;

/// Decodes a packet body, asserting the payload was consumed to the last
/// byte. Returns `None` on any decode error or trailing bytes rather than
/// panicking: a malformed packet from the wire should drop that packet, not
/// take down the connection.
pub(super) fn decode_full<T: Decode>(payload: &[u8]) -> Option<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX).ok()?;
    reader.ensure_empty().ok()?;
    Some(value)
}

/// Decodes a `ServerboundCustomPayloadPacket`: a length-prefixed
/// channel identifier (`string(32767)`, the same bound the clientbound
/// direction encodes under in `adapter/connection.rs`), then the channel-specific payload
/// as the remaining bytes verbatim.
///
/// Every channel is lifted into [`ServerBound::CustomPayload`] unchanged, where
/// this crate used to model only `minecraft:brand` and drop everything else.
/// The channel registry and the register/unregister interpretation now live in
/// the version-free server (`lodestone-server`'s `plugin_channels` module); an
/// unregistered channel is dropped there, exactly vanilla's `DiscardedPayload`
/// fallback. `None` on a channel that fails to parse as a [`ResourceKey`] — a
/// malformed packet drops that packet, not the connection (the same convention
/// as [`decode_full`]).
pub(super) fn decode_custom_payload(payload: &[u8]) -> Option<ServerBound> {
    let mut r = Reader::new(payload);
    let channel = r.string(32767).ok()?;
    let channel: ResourceKey = channel.parse().ok()?;
    Some(ServerBound::CustomPayload {
        channel,
        data: r.remaining_bytes().to_vec(),
    })
}

/// Decodes one serverbound container-click item written as a `HashedStack`
/// (vanilla's own codec library's own optional(vanilla's own hashed-stack shape's own actual item.STREAM_CODEC)`), the
/// inverse of the client-side encoder of the same name
/// (`crate::adapter::write_hashed_stack`): a bool presence flag, then, only
/// if present, the item registry id (VarInt), the count (VarInt), and two
/// VarInt component-patch entry counts (added, removed).
///
/// Our own client always writes `0`/`0` for the two patch counts — see
/// `write_hashed_stack`'s own doc comment, which notes creative slot-set with
/// custom components is out of scope for that encoder too. A **nonzero**
/// count here is therefore either a future client carrying real
/// component-patch entries this decoder has no byte-accurate per-entry
/// layout for, or a malformed packet; either way the safest response is to
/// fail the whole decode rather than guess a skip length and misalign every
/// byte that follows (the same "malformed packet drops the packet, not the
/// connection" convention this module already follows elsewhere).
///
/// Returns `None` on any decode failure, `Some(None)` for an explicitly
/// empty slot, `Some(Some(stack))` for a resolved item. An item id with no
/// entry in the generated table, or a name the wire item-key vocabulary does
/// not accept, is treated as a decode failure for the same reason.
pub(super) fn read_hashed_stack(r: &mut Reader) -> Option<Option<ItemStack>> {
    if !r.bool().ok()? {
        return Some(None);
    }
    let item_id = r.var_i32().ok()?;
    let count = r.var_i32().ok()?;
    let added = r.var_i32().ok()?;
    let removed = r.var_i32().ok()?;
    if added != 0 || removed != 0 {
        return None;
    }
    let item = item_from_wire_id(item_id)?.name().parse().ok()?;
    let count = u32::try_from(count).ok()?;
    Some(Some(ItemStack::new(item, count)))
}

/// Decodes the serverbound `container_click` packet body into
/// [`ServerBound::ContainerClicked`].
///
/// Wire layout (`ServerboundContainerClickPacket`, mirrors the client-side
/// encoder `crate::adapter::encode_container_click` exactly): VarInt
/// container id, VarInt state id, big-endian `short` slot, big-endian `byte`
/// button, `ContainerInput` ordinal (VarInt), a changed-slots map (VarInt
/// entry count, then per entry a big-endian `short` slot key and a
/// [`read_hashed_stack`] value), then the carried cursor stack, also a
/// [`read_hashed_stack`].
///
/// The clicked slot/button/click-type fields **are** carried into
/// [`ServerBound`]: `lodestone-server`'s `container_click::do_click` re-derives
/// the whole menu state from them, the way vanilla's own `doClick` does.
/// `changed_slots`/`carried_item` come along as the client's *prediction*, which
/// the consumer compares against and never stores — see that variant's own doc
/// comment.
pub(super) fn decode_container_click(payload: &[u8]) -> Option<ServerBound> {
    let mut r = Reader::new(payload);
    let window_id = r.var_i32().ok()?;
    let state_id = r.var_i32().ok()?;
    let slot = i32::from(r.i16().ok()?);
    let button = r.i8().ok()?;
    let click_type = r.var_i32().ok()?;
    let count = r.var_i32().ok()?;
    let count = usize::try_from(count).ok()?;
    // No `Vec::with_capacity(count)`: `count` is attacker-controlled and
    // unrelated to `payload`'s actual length until each entry is read, so
    // pre-allocating it would let a short, malformed packet request an
    // enormous allocation before the first bounds check ever fails.
    let mut changed_slots = Vec::new();
    for _ in 0..count {
        let slot = i32::from(r.i16().ok()?);
        let item = read_hashed_stack(&mut r)?;
        changed_slots.push((slot, item));
    }
    let carried_item = read_hashed_stack(&mut r)?;
    r.ensure_empty().ok()?;
    Some(ServerBound::ContainerClicked {
        window_id,
        state_id,
        slot,
        button,
        click_type,
        changed_slots,
        carried_item,
    })
}

/// Reads a serverbound `set_creative_mode_slot` item
/// (vanilla's own item-stack type's own optional-untrusted-stream-codec accessor, the inverse of the
/// client-side encoder `crate::adapter::write_optional_item_stack`): a VarInt
/// count where `<= 0` means empty, otherwise the item registry id as a
/// VarInt, then an empty `DataComponentPatch` (two VarInt `0`s, added then
/// removed).
///
/// Deliberately **not** the same shape as [`read_hashed_stack`]: that one has
/// a leading presence bool and puts the item id before the count
/// (vanilla's own hashed-stack actual-item stream codec); this one has no
/// presence bool at
/// all — a `count` of zero or less *is* the absence marker
/// (vanilla's own optional-item-stack-codec factory, verified against
/// the decompiled 26.2 item-stack source) — and puts
/// the count first. Conflating the two would silently misalign every byte
/// that follows.
///
/// A nonzero component-patch count is treated as a decode failure for the
/// same reason [`read_hashed_stack`] does: this crate's canonical
/// [`ItemStack`] carries no components, so there is no way to apply a
/// nonempty patch, and guessing a skip length would misalign the rest of the
/// packet.
pub(super) fn read_optional_item_stack(r: &mut Reader) -> Option<Option<ItemStack>> {
    let count = r.var_i32().ok()?;
    if count <= 0 {
        return Some(None);
    }
    let item_id = r.var_i32().ok()?;
    let added = r.var_i32().ok()?;
    let removed = r.var_i32().ok()?;
    if added != 0 || removed != 0 {
        return None;
    }
    let item = item_from_wire_id(item_id)?.name().parse().ok()?;
    let count = u32::try_from(count).ok()?;
    Some(Some(ItemStack::new(item, count)))
}

/// Reads one serverbound `set_beacon` mob-effect slot
/// (vanilla's own codec library's own optional(vanilla's own mob-effect type's own stream codec)`, the inverse of
/// `crate::adapter::write_optional_mob_effect`): a bool presence flag, then,
/// only if present, the effect's `minecraft:mob_effect` registry id as a
/// direct VarInt.
///
/// Returns the effect's canonical name on success — this module's own
/// `SET_BEACON` decode arm lifts both calls straight into a real
/// `ServerBound::SetBeacon`.
pub(super) fn read_optional_mob_effect(r: &mut Reader) -> Option<Option<&'static str>> {
    if !r.bool().ok()? {
        return Some(None);
    }
    let id = MobEffectId::from_registry_id(r.var_i32().ok()?)?;
    Some(Some(mob_effect_name_for(id)))
}

/// Packs a block position into vanilla's vanilla's own block-position type's own as long form: `x` in the
/// high 26 bits, `z` in the middle 26 bits, `y` in the low 12 bits.
pub(super) fn pack_block_pos(x: i32, y: i32, z: i32) -> i64 {
    ((i64::from(x) & 0x3FF_FFFF) << 38)
        | ((i64::from(z) & 0x3FF_FFFF) << 12)
        | (i64::from(y) & 0xFFF)
}
