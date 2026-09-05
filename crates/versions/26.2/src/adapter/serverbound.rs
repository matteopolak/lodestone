//! Serverbound encoding: [`crate::adapter::V770Adapter`]'s `encode_action`
//! and every helper it alone uses. Split out of the former monolithic
//! `adapter.rs`.
use super::*;

/// Packs block coordinates into vanilla's own packed block-position long
/// value: `x` in bits 38–63, `z` in bits 12–37, `y` in bits 0–11, each a
/// signed field.
fn pack_block_pos(pos: BlockPos) -> i64 {
    ((i64::from(pos.x) & 0x3FF_FFFF) << 38)
        | ((i64::from(pos.z) & 0x3FF_FFFF) << 12)
        | (i64::from(pos.y) & 0xFFF)
}

/// Maps an interaction hand to its vanilla ordinal (`0` main, `1` off).
fn hand_ordinal(hand: Hand) -> i32 {
    match hand {
        Hand::Main => 0,
        Hand::Off => 1,
    }
}

/// Maps a block face to vanilla's own direction data-value ordinal (`0`
/// down … `5` east).
fn face_ordinal(face: BlockFace) -> i32 {
    match face {
        BlockFace::Down => 0,
        BlockFace::Up => 1,
        BlockFace::North => 2,
        BlockFace::South => 3,
        BlockFace::West => 4,
        BlockFace::East => 5,
    }
}

/// Writes a `Vec3` using vanilla's own low-precision quantised position codec: a
/// single `0` byte for the (near-)zero vector, otherwise a packed 48-bit buffer
/// (two bytes plus a big-endian int) carrying three 15-bit components and a
/// 2-bit scale, with an optional trailing scale varint when the scale overflows.
fn write_lp_vec3(w: &mut Writer, x: f64, y: f64, z: f64) {
    fn sanitize(v: f64) -> f64 {
        if v.is_nan() {
            0.0
        } else {
            v.clamp(-1.717_986_918_3E10, 1.717_986_918_3E10)
        }
    }
    // Vanilla's own rounding helper, i.e. floor(a + 0.5); the argument is always >= 0.
    fn pack(v: f64) -> i64 {
        ((v * 0.5 + 0.5) * 32766.0 + 0.5).floor() as i64
    }
    let x = sanitize(x);
    let y = sanitize(y);
    let z = sanitize(z);
    let chess = x.abs().max(y.abs()).max(z.abs());
    if chess < 3.051_944_088_384_301E-5 {
        w.u8(0);
        return;
    }
    let scale = chess.ceil() as i64;
    let is_partial = (scale & 3) != scale;
    let markers = if is_partial { (scale & 3) | 4 } else { scale };
    let buffer = markers
        | (pack(x / scale as f64) << 3)
        | (pack(y / scale as f64) << 18)
        | (pack(z / scale as f64) << 33);
    w.u8(buffer as u8);
    w.u8((buffer >> 8) as u8);
    w.i32((buffer >> 16) as i32);
    if is_partial {
        w.var_i32((scale >> 2) as i32);
    }
}

/// Encodes a serverbound `interact` payload: VarInt entity id, VarInt hand,
/// low-precision quantised location, then the secondary-action bool. `location` is `None` for a
/// plain interact, which vanilla encodes as the zero vector (a single `0` byte).
fn encode_interact(entity_id: i32, hand: Hand, location: Option<Vec3>, sneaking: bool) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(entity_id);
    w.var_i32(hand_ordinal(hand));
    let loc = location.unwrap_or(Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    });
    write_lp_vec3(&mut w, loc.x, loc.y, loc.z);
    w.bool(sneaking);
    w.into_vec()
}

/// Maps a container click mode to `ContainerInput`'s ordinal
/// (vanilla's own id-mapper codec, a direct VarInt id: `0` pickup … `6` pickup_all).
fn container_input_ordinal(click_type: ContainerClickType) -> i32 {
    match click_type {
        ContainerClickType::Pickup => 0,
        ContainerClickType::QuickMove => 1,
        ContainerClickType::Swap => 2,
        ContainerClickType::Clone => 3,
        ContainerClickType::Throw => 4,
        ContainerClickType::QuickCraft => 5,
        ContainerClickType::PickupAll => 6,
    }
}

/// Encodes the serverbound `container_click` packet body.
///
/// Wire layout (vanilla's own serverbound container-click packet): VarInt container id, VarInt
/// state id, big-endian `short` slot, big-endian `byte` button,
/// `ContainerInput` ordinal (VarInt), a changed-slots map (VarInt entry count,
/// then per entry a big-endian `short` slot key and a hashed-stack value),
/// then the carried cursor stack, also a hashed stack. Map iteration order is
/// not semantically significant (vanilla holds it in a hash map), so the
/// model's `Vec` order is used as-is.
fn encode_container_click(
    window_id: i32,
    state_id: i32,
    slot: i32,
    button: i32,
    click_type: ContainerClickType,
    changed_slots: &[ContainerSlotChange],
    carried_item: Option<&ItemStack>,
) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.var_i32(window_id);
    w.var_i32(state_id);
    let slot_i16 = i16::try_from(slot)
        .map_err(|_| AdapterError::Encode(format!("container click slot {slot} overflows i16")))?;
    w.i16(slot_i16);
    let button_i8 = i8::try_from(button).map_err(|_| {
        AdapterError::Encode(format!("container click button {button} overflows i8"))
    })?;
    w.i8(button_i8);
    w.var_i32(container_input_ordinal(click_type));
    let count = i32::try_from(changed_slots.len()).map_err(|_| {
        AdapterError::Encode("too many changed slots in container click".to_owned())
    })?;
    w.var_i32(count);
    for change in changed_slots {
        let change_slot = i16::try_from(change.slot).map_err(|_| {
            AdapterError::Encode(format!("changed slot {} overflows i16", change.slot))
        })?;
        w.i16(change_slot);
        write_hashed_stack(&mut w, change.item.as_ref())?;
    }
    write_hashed_stack(&mut w, carried_item)?;
    Ok(w.into_vec())
}

/// Maps the canonical [`GameMode`] to vanilla's own game-type id, the inverse of
/// [`game_mode_from_ordinal`].
pub(crate) fn game_mode_to_ordinal(mode: GameMode) -> i32 {
    match mode {
        GameMode::Survival => 0,
        GameMode::Creative => 1,
        GameMode::Adventure => 2,
        GameMode::Spectator => 3,
    }
}

/// Maps the canonical [`RecipeBookType`] to vanilla's own `RecipeBookType`
/// ordinal, as written by vanilla's clientbound recipe-book
/// change-settings packet's own enum writer.
fn recipe_book_type_to_ordinal(book_type: RecipeBookType) -> i32 {
    match book_type {
        RecipeBookType::Crafting => 0,
        RecipeBookType::Furnace => 1,
        RecipeBookType::BlastFurnace => 2,
        RecipeBookType::Smoker => 3,
    }
}

/// Resolves an [`ItemStack`]'s canonical item key to protocol 776's numeric
/// item-registry id, attributing an unknown item loudly rather than silently
/// substituting a placeholder.
fn item_registry_id(stack: &ItemStack) -> Result<i32, AdapterError> {
    Item::from_name(&stack.item.to_string())
        .map(|item| i32::from(item.registry_id()))
        .ok_or_else(|| AdapterError::Encode(format!("unknown item key {}", stack.item)))
}

/// Writes a serverbound `set_creative_mode_slot` item
/// (vanilla's own untrusted-optional item-stack stream codec): a VarInt
/// count (`<= 0` is the empty stack), then, only if non-empty, the item
/// registry id as a VarInt and an empty component patch (VarInt `0` added,
/// VarInt `0` removed).
///
/// Note: an [`ItemStack`] can now carry decoded components, but this serverbound
/// encoder deliberately writes the **empty** patch and does not re-serialise
/// them. Creative slot-set with custom components is out of Phase 1's scope; the
/// server accepts the empty patch and applies its own defaults. If creative
/// component round-tripping is ever needed, this is the single site to extend.
fn write_optional_item_stack(w: &mut Writer, item: Option<&ItemStack>) -> Result<(), AdapterError> {
    match item {
        None => w.var_i32(0),
        Some(stack) => {
            let count = i32::try_from(stack.count).map_err(|_| {
                AdapterError::Encode(format!("item count {} overflows i32", stack.count))
            })?;
            w.var_i32(count);
            w.var_i32(item_registry_id(stack)?);
            w.var_i32(0); // added components
            w.var_i32(0); // removed components
        }
    }
    Ok(())
}

/// Writes a serverbound container-click item as vanilla's own hashed-stack
/// shape (an optional-value codec over its own actual-item stream codec): a
/// bool presence flag, then, only if present, the item registry id as a
/// VarInt, the count as a VarInt, and an empty hashed patch map (VarInt `0`
/// added, VarInt `0` removed).
///
/// The canonical [`ItemStack`] carries no components, so the patch is always
/// empty — the only shape this model can produce, and the common case for a
/// plain vanilla stack.
fn write_hashed_stack(w: &mut Writer, item: Option<&ItemStack>) -> Result<(), AdapterError> {
    match item {
        None => w.bool(false),
        Some(stack) => {
            w.bool(true);
            w.var_i32(item_registry_id(stack)?);
            let count = i32::try_from(stack.count).map_err(|_| {
                AdapterError::Encode(format!("item count {} overflows i32", stack.count))
            })?;
            w.var_i32(count);
            w.var_i32(0); // added components
            w.var_i32(0); // removed components
        }
    }
    Ok(())
}

/// Writes an `Optional<Holder<MobEffect>>` for the serverbound `set_beacon`
/// packet (vanilla's own optional-value codec over its mob-effect stream
/// codec): a bool presence flag, then, only if present, the effect's
/// `minecraft:mob_effect` registry id as a direct VarInt (vanilla's own
/// holder-registry codec, unlike the sound-holder codec, has no
/// inline-definition escape id).
fn write_optional_mob_effect(
    w: &mut Writer,
    effect: Option<&ResourceKey>,
) -> Result<(), AdapterError> {
    match effect {
        None => w.bool(false),
        Some(key) => {
            w.bool(true);
            let id = mob_effect_id(&key.to_string())
                .ok_or_else(|| AdapterError::Encode(format!("unknown mob effect {key}")))?;
            w.var_i32(id.registry_id());
        }
    }
    Ok(())
}

/// Encodes the serverbound `set_beacon` packet body: two `Optional<Holder<MobEffect>>`
/// values (primary then secondary power), each written by
/// [`write_optional_mob_effect`].
fn encode_set_beacon(
    primary: Option<&ResourceKey>,
    secondary: Option<&ResourceKey>,
) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    write_optional_mob_effect(&mut w, primary)?;
    write_optional_mob_effect(&mut w, secondary)?;
    Ok(w.into_vec())
}

/// Encodes the serverbound `spectator_action` packet body
/// (vanilla's own serverbound spectator-action packet): a single VarInt using
/// `vanilla's own byte buf codecs's own var int`'s offset encoding, **not** the common
/// bool-then-value optional shape — `0` means "not spectating an entity"
/// and a present id `i` is written as `i + 1`. This must be hand-written
/// rather than a derived `Option<i32>` field, since a naive bool-prefixed
/// encoder would silently produce a wire-incompatible packet that still
/// parses.
fn encode_spectator_action(target_entity_id: Option<i32>) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.var_i32(target_entity_id.map_or(0, |id| id + 1));
    Ok(w.into_vec())
}

/// Encodes the serverbound `seen_advancements` packet body
/// (vanilla's own seen-advancements packet): a VarInt `Action` ordinal
/// (`OPENED_TAB` = 0, `CLOSED_SCREEN` = 1, via vanilla's own enum writer),
/// followed *only when opening a tab* by that tab's `minecraft:*` identifier
/// string (vanilla's own identifier writer, a plain UTF-8 string write).
/// Closing writes
/// nothing further — the identifier's presence depends on the ordinal, so
/// this can't be a plain derived struct.
fn encode_seen_advancements(tab: Option<&ResourceKey>) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    match tab {
        Some(key) => {
            w.var_i32(0); // OPENED_TAB
            w.string(&key.to_string());
        }
        None => w.var_i32(1), // CLOSED_SCREEN
    }
    Ok(w.into_vec())
}
// ---- the operator/debug serverbound encoders --------------------------------
//
// Thirteen packets a vanilla client can send that this adapter could not encode
// at all. Every layout below was read off the record definition in
// `.cache/mc/26.2/src` — the `write` method or the `StreamCodec` composition, not
// a summary — because there is no encoder of ours to round-trip against and
// `decode(encode(x)) == x` would be satisfied by two symmetric misunderstandings.
//
// Three of these have a shape a transliterating encoder gets wrong, and each is
// called out at its own function:
//
// * `set_structure_block`'s offset/size are **signed bytes**, not `Vec3i`
//   VarInts, and its flags byte is **last**;
// * `set_jigsaw_block`'s `joint` is a **string**, not an enum ordinal;
// * `custom_click_action`'s payload is **double-framed** — a VarInt byte length
//   wrapping an optional-NBT body.

/// Maps a [`Difficulty`] to vanilla's own difficulty id getter, which is
/// what vanilla's own id-mapper codec writes — the declared enum order,
/// `PEACEFUL` first.
fn difficulty_id(difficulty: Difficulty) -> i32 {
    match difficulty {
        Difficulty::Peaceful => 0,
        Difficulty::Easy => 1,
        Difficulty::Normal => 2,
        Difficulty::Hard => 3,
    }
}

/// `Rotation`'s wire id, from its own declared order
/// (confirmed against the decompiled 26.2 block-rotation source).
fn structure_rotation_id(rotation: StructureRotation) -> i32 {
    match rotation {
        StructureRotation::None => 0,
        StructureRotation::Clockwise90 => 1,
        StructureRotation::Clockwise180 => 2,
        StructureRotation::CounterClockwise90 => 3,
    }
}

/// Encodes the serverbound `set_game_rule` body: a VarInt-counted list of
/// `(rule identifier, value string)` pairs.
///
/// The value is a `STRING_UTF8` whatever the rule's real type is — the server
/// parses it against its own typed registry — so nothing here validates it.
fn encode_set_game_rules(entries: &[(ResourceKey, String)]) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.var_i32(i32::try_from(entries.len()).map_err(|_| {
        AdapterError::Encode("set_game_rule entry count exceeds i32".to_owned())
    })?);
    for (key, value) in entries {
        w.string(&key.to_string());
        w.string(value);
    }
    Ok(w.into_vec())
}

/// Encodes the serverbound `set_structure_block` body
/// (vanilla's own structure-block packet writer).
///
/// **Two traps, both invisible to a round trip against ourselves.** `offset` and
/// `size` are six `writeByte`s, not a `Vec3i`'s three VarInts each — vanilla
/// clamps them to `-48..=48` and `0..=48` on read, so an out-of-range value is
/// narrowed rather than refused, and this encoder narrows the same way rather
/// than emitting a byte that would wrap. And the flags byte is written **last**,
/// after `seed`, not next to the booleans it packs.
#[allow(clippy::too_many_arguments)]
fn encode_set_structure_block(
    pos: BlockPos,
    update_type: StructureBlockUpdateType,
    mode: StructureBlockMode,
    name: &str,
    offset: (i8, i8, i8),
    size: (i8, i8, i8),
    mirror: StructureMirror,
    rotation: StructureRotation,
    data: &str,
    integrity: f32,
    seed: i64,
    flags: u8,
) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.i64(pack_block_pos(pos));
    w.var_i32(match update_type {
        StructureBlockUpdateType::UpdateData => 0,
        StructureBlockUpdateType::SaveArea => 1,
        StructureBlockUpdateType::LoadArea => 2,
        StructureBlockUpdateType::ScanArea => 3,
    });
    w.var_i32(match mode {
        StructureBlockMode::Save => 0,
        StructureBlockMode::Load => 1,
        StructureBlockMode::Corner => 2,
        StructureBlockMode::Data => 3,
    });
    w.string(name);
    for axis in [offset.0, offset.1, offset.2] {
        w.i8(axis.clamp(-48, 48));
    }
    for axis in [size.0, size.1, size.2] {
        w.i8(axis.clamp(0, 48));
    }
    w.var_i32(match mirror {
        StructureMirror::None => 0,
        StructureMirror::LeftRight => 1,
        StructureMirror::FrontBack => 2,
    });
    w.var_i32(structure_rotation_id(rotation));
    w.string(data);
    w.f32(integrity.clamp(0.0, 1.0));
    w.var_i64(seed);
    w.u8(flags);
    Ok(w.into_vec())
}

/// Encodes the serverbound `set_jigsaw_block` body
/// (vanilla's own jigsaw-block packet writer).
///
/// The trap is `joint`: vanilla writes `joint.getSerializedName()`, a UTF string,
/// and falls back to `ALIGNED` for anything it cannot parse. An encoder that
/// wrote a VarInt ordinal here — the shape every other enum field in this packet
/// family uses — would produce a packet the server silently reads as a
/// zero-length name and defaults, i.e. a wrong value on a fully connected wire.
#[allow(clippy::too_many_arguments)]
fn encode_set_jigsaw_block(
    pos: BlockPos,
    name: &ResourceKey,
    target: &ResourceKey,
    pool: &ResourceKey,
    final_state: &str,
    joint: JigsawJoint,
    selection_priority: i32,
    placement_priority: i32,
) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.i64(pack_block_pos(pos));
    w.string(&name.to_string());
    w.string(&target.to_string());
    w.string(&pool.to_string());
    w.string(final_state);
    w.string(joint.serialized_name());
    w.var_i32(selection_priority);
    w.var_i32(placement_priority);
    Ok(w.into_vec())
}

/// Encodes the serverbound `test_instance_block_action` body
/// (vanilla's own test-instance-block-action packet plus its block-entity
/// data record).
fn encode_test_instance_block_action(
    pos: BlockPos,
    action: TestInstanceAction,
    data: &TestInstanceData,
) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.i64(pack_block_pos(pos));
    w.var_i32(match action {
        TestInstanceAction::Init => 0,
        TestInstanceAction::Query => 1,
        TestInstanceAction::Set => 2,
        TestInstanceAction::Reset => 3,
        TestInstanceAction::Save => 4,
        TestInstanceAction::Export => 5,
        TestInstanceAction::Run => 6,
    });
    match &data.test {
        Some(key) => {
            w.bool(true);
            w.string(&key.to_string());
        }
        None => w.bool(false),
    }
    w.var_i32(data.size.0);
    w.var_i32(data.size.1);
    w.var_i32(data.size.2);
    w.var_i32(structure_rotation_id(data.rotation));
    w.bool(data.ignore_entities);
    w.var_i32(match data.status {
        TestInstanceStatus::Cleared => 0,
        TestInstanceStatus::Running => 1,
        TestInstanceStatus::Finished => 2,
    });
    match &data.error_message {
        Some(component) => {
            w.bool(true);
            w.bytes(component);
        }
        None => w.bool(false),
    }
    Ok(w.into_vec())
}

/// Encodes the serverbound `debug_subscription_request` body: a VarInt-counted
/// list of `minecraft:debug_subscription` network ids, capped at 32 by the wire.
///
/// Unknown keys are **dropped** rather than failing the whole subscription — a
/// client asking for a feed this protocol does not have should get the rest,
/// which is also what makes an empty list (vanilla's "unsubscribe from
/// everything") indistinguishable from "all keys unknown". The caller sees the
/// difference through the returned count.
fn encode_debug_subscription_request(
    subscriptions: &[ResourceKey],
) -> Result<Vec<u8>, AdapterError> {
    let mut ids: Vec<i32> = subscriptions
        .iter()
        .filter_map(|key| {
            crate::stat_debug_registries::debug_subscription_id(&key.to_string())
        })
        .collect();
    ids.truncate(32);
    let mut w = Writer::default();
    w.var_i32(i32::try_from(ids.len()).map_err(|_| {
        AdapterError::Encode("debug subscription count exceeds i32".to_owned())
    })?);
    for id in ids {
        w.var_i32(id);
    }
    Ok(w.into_vec())
}

/// Encodes the serverbound `custom_click_action` body
/// (vanilla's own custom-click-action packet).
///
/// **Double-framed.** The codec is
/// `optionalTagCodec(...).apply(lengthPrefixed(65536))`: an outer VarInt *byte
/// length*, and inside it the optional-NBT body. `payload` is already that inner
/// body (a leading present/absent byte and, if present, the NBT), so this only
/// adds the length prefix — writing the NBT with no prefix, or prefixing an
/// element count instead of a byte count, both produce something the server
/// cannot read.
fn encode_custom_click_action(id: &ResourceKey, payload: &[u8]) -> Result<Vec<u8>, AdapterError> {
    if payload.len() > 65536 {
        return Err(AdapterError::Encode(format!(
            "custom_click_action payload is {} bytes, over the wire's 65536 limit",
            payload.len()
        )));
    }
    let mut w = Writer::default();
    w.string(&id.to_string());
    w.var_bytes(payload)
        .map_err(|err| AdapterError::Encode(err.to_string()))?;
    Ok(w.into_vec())
}

/// Maps a [`ResourcePackResponseKind`] to `vanilla's own serverbound resource pack packet's own action`'s
/// ordinal, matching its declared enum order.
fn resource_pack_response_ordinal(kind: ResourcePackResponseKind) -> i32 {
    match kind {
        ResourcePackResponseKind::SuccessfullyLoaded => 0,
        ResourcePackResponseKind::Declined => 1,
        ResourcePackResponseKind::FailedDownload => 2,
        ResourcePackResponseKind::Accepted => 3,
        ResourcePackResponseKind::Downloaded => 4,
        ResourcePackResponseKind::InvalidUrl => 5,
        ResourcePackResponseKind::FailedReload => 6,
        ResourcePackResponseKind::Discarded => 7,
    }
}

/// Maps a [`CommandBlockMode`] to `vanilla's own command block entity's own mode`'s ordinal
/// (`0` sequence, `1` auto, `2` redstone).
fn command_block_mode_ordinal(mode: CommandBlockMode) -> i32 {
    match mode {
        CommandBlockMode::Sequence => 0,
        CommandBlockMode::Auto => 1,
        CommandBlockMode::Redstone => 2,
    }
}

/// Packs [`DisplayedSkinParts`] into vanilla's `client_information`
/// model-customisation bitmask (`PlayerModelPart`'s bit order): cape `0x01`,
/// jacket `0x02`, left sleeve `0x04`, right sleeve `0x08`, left pants leg
/// `0x10`, right pants leg `0x20`, hat `0x40`.
fn skin_parts_bitmask(parts: DisplayedSkinParts) -> u8 {
    u8::from(parts.cape)
        | (u8::from(parts.jacket) << 1)
        | (u8::from(parts.left_sleeve) << 2)
        | (u8::from(parts.right_sleeve) << 3)
        | (u8::from(parts.left_pants_leg) << 4)
        | (u8::from(parts.right_pants_leg) << 5)
        | (u8::from(parts.hat) << 6)
}

impl V770Adapter {
    /// Serverbound encode for every [`ClientAction`], moved verbatim out of the
    /// former monolithic `adapter.rs`'s `VersionAdapter::encode_action` — see
    /// `adapter::mod`'s trait impl for the one-line delegate.
    pub(super) fn encode_client_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        match action {
            ClientAction::KeepAliveResponse { id } => {
                let body = KeepAlive { id: *id };
                match state {
                    ConnectionState::Play => {
                        Ok(Some((play::serverbound::KEEP_ALIVE, encode_body(&body)?)))
                    }
                    ConnectionState::Configuration => Ok(Some((
                        configuration::serverbound::KEEP_ALIVE,
                        encode_body(&body)?,
                    ))),
                    _ => Ok(None),
                }
            }
            ClientAction::SendCommand { command } if state == ConnectionState::Play => {
                let body = ChatCommand {
                    command: command.clone(),
                };
                Ok(Some((play::serverbound::CHAT_COMMAND, encode_body(&body)?)))
            }
            ClientAction::ChatAck { offset } if state == ConnectionState::Play => {
                let body = ChatAck { offset: *offset };
                Ok(Some((play::serverbound::CHAT_ACK, encode_body(&body)?)))
            }
            ClientAction::SendChat { text } if state == ConnectionState::Play => Ok(Some((
                play::serverbound::CHAT,
                encode_body(&ChatMessage::unsigned(text.clone()))?,
            ))),
            ClientAction::SendSignedChat {
                text,
                timestamp_millis,
                salt,
                signature,
                last_seen_offset,
                acknowledged,
                checksum,
            } if state == ConnectionState::Play => {
                // `crate::packets::game::MessageSignature` fully-qualified: this
                // module's glob `use super::*` already brings
                // `lodestone_game::chat_ack::MessageSignature` into scope under
                // the same name (the driver's stateful last-seen tracker), and
                // that is a different type from the wire's fixed 256-byte body.
                let signature: [u8; 256] = signature.as_slice().try_into().map_err(|_| {
                    AdapterError::Encode(format!(
                        "signed chat signature was {} bytes, expected 256",
                        signature.len()
                    ))
                })?;
                let body = ChatMessage {
                    message: text.clone(),
                    timestamp: *timestamp_millis,
                    salt: *salt,
                    signature: Some(crate::packets::game::MessageSignature(signature)),
                    last_seen_offset: *last_seen_offset,
                    acknowledged: *acknowledged,
                    checksum: *checksum,
                };
                Ok(Some((play::serverbound::CHAT, encode_body(&body)?)))
            }
            ClientAction::AnnounceChatSession {
                session_id,
                expires_at_millis,
                public_key,
                key_signature,
            } if state == ConnectionState::Play => {
                let body = ChatSessionUpdate {
                    session_id: *session_id,
                    expires_at: *expires_at_millis,
                    public_key: public_key.clone(),
                    key_signature: key_signature.clone(),
                };
                Ok(Some((
                    play::serverbound::CHAT_SESSION_UPDATE,
                    encode_body(&body)?,
                )))
            }
            ClientAction::Respawn if state == ConnectionState::Play => {
                // `client_command` action 0 = perform_respawn; leaves the death screen.
                let body = ClientCommand { action: 0 };
                Ok(Some((
                    play::serverbound::CLIENT_COMMAND,
                    encode_body(&body)?,
                )))
            }
            ClientAction::Move {
                pos,
                rotation,
                on_ground,
                horizontal_collision,
            } if state == ConnectionState::Play => {
                self.select_move_packet(*pos, *rotation, *on_ground, *horizontal_collision)
            }
            ClientAction::SwingArm { hand } if state == ConnectionState::Play => {
                let body = Swing {
                    hand: match hand {
                        Hand::Main => 0,
                        Hand::Off => 1,
                    },
                };
                Ok(Some((play::serverbound::SWING, encode_body(&body)?)))
            }
            ClientAction::BlockAction {
                action,
                pos,
                face,
                sequence,
            } if state == ConnectionState::Play => {
                let body = PlayerAction {
                    action: match action {
                        BlockActionKind::StartDestroy => 0,
                        BlockActionKind::AbortDestroy => 1,
                        BlockActionKind::StopDestroy => 2,
                    },
                    pos: pack_block_pos(*pos),
                    direction: face_ordinal(*face) as u8,
                    sequence: *sequence,
                };
                Ok(Some((
                    play::serverbound::PLAYER_ACTION,
                    encode_body(&body)?,
                )))
            }
            ClientAction::DropSelectedItem
            | ClientAction::DropSelectedItemStack
            | ClientAction::SwapItemWithOffhand
            | ClientAction::ReleaseUseItem
            | ClientAction::Stab
                if state == ConnectionState::Play =>
            {
                // Item actions share the `player_action` packet with a zeroed
                // position and a `down` face; only the action ordinal varies.
                let ordinal = match action {
                    ClientAction::DropSelectedItemStack => 3, // DROP_ALL_ITEMS
                    ClientAction::DropSelectedItem => 4,      // DROP_ITEM
                    ClientAction::ReleaseUseItem => 5,        // RELEASE_USE_ITEM
                    ClientAction::SwapItemWithOffhand => 6,   // SWAP_ITEM_WITH_OFFHAND
                    ClientAction::Stab => 7,                  // STAB
                    _ => unreachable!("guarded by the arm's pattern"),
                };
                let body = PlayerAction {
                    action: ordinal,
                    pos: 0,
                    direction: 0,
                    sequence: 0,
                };
                Ok(Some((
                    play::serverbound::PLAYER_ACTION,
                    encode_body(&body)?,
                )))
            }
            ClientAction::UseItemOn {
                hand,
                pos,
                face,
                cursor,
                inside_block,
                sequence,
            } if state == ConnectionState::Play => {
                let body = UseItemOn {
                    hand: hand_ordinal(*hand),
                    pos: pack_block_pos(*pos),
                    face: face_ordinal(*face),
                    cursor_x: cursor.x,
                    cursor_y: cursor.y,
                    cursor_z: cursor.z,
                    inside_block: *inside_block,
                    world_border_hit: false,
                    sequence: *sequence,
                };
                Ok(Some((play::serverbound::USE_ITEM_ON, encode_body(&body)?)))
            }
            ClientAction::UseItem {
                hand,
                rotation,
                sequence,
            } if state == ConnectionState::Play => {
                let body = UseItem {
                    hand: hand_ordinal(*hand),
                    sequence: *sequence,
                    yaw: rotation.yaw,
                    pitch: rotation.pitch,
                };
                Ok(Some((play::serverbound::USE_ITEM, encode_body(&body)?)))
            }
            ClientAction::InteractEntity {
                entity_id,
                interaction,
                sneaking,
            } if state == ConnectionState::Play => match interaction {
                // 26.2 splits attack into its own packet, which carries only the
                // entity id (no hand, location, or secondary-action flag).
                EntityInteraction::Attack => {
                    let body = Attack {
                        entity_id: *entity_id,
                    };
                    Ok(Some((play::serverbound::ATTACK, encode_body(&body)?)))
                }
                EntityInteraction::Interact { hand } => Ok(Some((
                    play::serverbound::INTERACT,
                    encode_interact(*entity_id, *hand, None, *sneaking),
                ))),
                EntityInteraction::InteractAt { hand, target } => Ok(Some((
                    play::serverbound::INTERACT,
                    encode_interact(*entity_id, *hand, Some(*target), *sneaking),
                ))),
            },
            ClientAction::SetPlayerInput(input) if state == ConnectionState::Play => {
                let PlayerInput {
                    forward,
                    backward,
                    left,
                    right,
                    jump,
                    shift,
                    sprint,
                } = input;
                let flags = u8::from(*forward)
                    | (u8::from(*backward) << 1)
                    | (u8::from(*left) << 2)
                    | (u8::from(*right) << 3)
                    | (u8::from(*jump) << 4)
                    | (u8::from(*shift) << 5)
                    | (u8::from(*sprint) << 6);
                let body = PlayerInputPacket { flags };
                Ok(Some((play::serverbound::PLAYER_INPUT, encode_body(&body)?)))
            }
            ClientAction::PlayerCommand { entity_id, command }
                if state == ConnectionState::Play =>
            {
                let (ordinal, data) = match command {
                    PlayerCommand::StopSleeping => (0, 0),
                    PlayerCommand::StartSprinting => (1, 0),
                    PlayerCommand::StopSprinting => (2, 0),
                    PlayerCommand::StartRidingJump { boost } => (3, *boost),
                    PlayerCommand::StopRidingJump => (4, 0),
                    PlayerCommand::OpenInventory => (5, 0),
                    PlayerCommand::StartFallFlying => (6, 0),
                };
                let body = PlayerCommandPacket {
                    entity_id: *entity_id,
                    action: ordinal,
                    data,
                };
                Ok(Some((
                    play::serverbound::PLAYER_COMMAND,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetCarriedItem { slot } if state == ConnectionState::Play => {
                let body = SetCarriedItem { slot: *slot as i16 };
                Ok(Some((
                    play::serverbound::SET_CARRIED_ITEM,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ContainerClose { window_id } if state == ConnectionState::Play => {
                let body = ContainerClose {
                    window_id: *window_id,
                };
                Ok(Some((
                    play::serverbound::CONTAINER_CLOSE,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ContainerClick {
                window_id,
                state_id,
                slot,
                button,
                click_type,
                changed_slots,
                carried_item,
            } if state == ConnectionState::Play => {
                let payload = encode_container_click(
                    *window_id,
                    *state_id,
                    *slot,
                    *button,
                    *click_type,
                    changed_slots,
                    carried_item.as_ref(),
                )?;
                Ok(Some((play::serverbound::CONTAINER_CLICK, payload)))
            }
            ClientAction::SetCreativeModeSlot { slot, item } if state == ConnectionState::Play => {
                let mut w = Writer::default();
                let slot_i16 = i16::try_from(*slot).map_err(|_| {
                    AdapterError::Encode(format!("creative slot {slot} overflows i16"))
                })?;
                w.i16(slot_i16);
                write_optional_item_stack(&mut w, item.as_ref())?;
                Ok(Some((
                    play::serverbound::SET_CREATIVE_MODE_SLOT,
                    w.into_vec(),
                )))
            }
            ClientAction::SetClientSettings(settings)
                if matches!(
                    state,
                    ConnectionState::Configuration | ConnectionState::Play
                ) =>
            {
                let ClientSettings {
                    locale,
                    view_distance,
                    chat_mode,
                    chat_colors,
                    skin_parts,
                    main_hand,
                    text_filtering,
                    allow_server_listing,
                    particle_status,
                } = settings;
                let body = ClientInformation {
                    language: locale.clone(),
                    view_distance: *view_distance,
                    chat_visibility: match chat_mode {
                        ChatMode::Full => 0,
                        ChatMode::CommandsOnly => 1,
                        ChatMode::Hidden => 2,
                    },
                    chat_colors: *chat_colors,
                    model_customization: skin_parts_bitmask(*skin_parts),
                    main_hand: match main_hand {
                        MainHand::Left => 0,
                        MainHand::Right => 1,
                    },
                    text_filtering: *text_filtering,
                    allows_listing: *allow_server_listing,
                    particle_status: match particle_status {
                        ParticleStatus::All => 0,
                        ParticleStatus::Decreased => 1,
                        ParticleStatus::Minimal => 2,
                    },
                };
                let packet_id = match state {
                    ConnectionState::Configuration => {
                        configuration::serverbound::CLIENT_INFORMATION
                    }
                    _ => play::serverbound::CLIENT_INFORMATION,
                };
                Ok(Some((packet_id, encode_body(&body)?)))
            }
            ClientAction::SendBrand { brand }
                if matches!(
                    state,
                    ConnectionState::Configuration | ConnectionState::Play
                ) =>
            {
                let body = BrandPayload {
                    channel: "minecraft:brand".to_owned(),
                    brand: brand.clone(),
                };
                let packet_id = match state {
                    ConnectionState::Configuration => configuration::serverbound::CUSTOM_PAYLOAD,
                    _ => play::serverbound::CUSTOM_PAYLOAD,
                };
                Ok(Some((packet_id, encode_body(&body)?)))
            }
            // The general case `SendBrand` above is vanilla's one
            // built-in instance of. `custom_payload`'s wire body is just
            // channel + raw bytes (vanilla's own clientbound custom-payload
            // packet's
            // `DiscardedPayload`, mirrored on the serverbound side), so this
            // needs no dedicated packet struct — `BrandPayload`'s two-string
            // shape doesn't fit arbitrary bytes, but `send` only needs an
            // `Encode` body, and a `(String, Vec<u8>)`-shaped write is exactly
            // what `custom_payload` is on every channel that isn't `brand`.
            ClientAction::SendCustomPayload { channel, data }
                if matches!(
                    state,
                    ConnectionState::Configuration | ConnectionState::Play
                ) =>
            {
                let mut writer = Writer::default();
                writer.string(&channel.to_string());
                writer.bytes(data);
                let packet_id = match state {
                    ConnectionState::Configuration => configuration::serverbound::CUSTOM_PAYLOAD,
                    _ => play::serverbound::CUSTOM_PAYLOAD,
                };
                Ok(Some((packet_id, writer.into_vec())))
            }
            ClientAction::PongResponse { id }
                if matches!(
                    state,
                    ConnectionState::Configuration | ConnectionState::Play
                ) =>
            {
                let body = Pong { id: *id };
                let packet_id = match state {
                    ConnectionState::Configuration => configuration::serverbound::PONG,
                    _ => play::serverbound::PONG,
                };
                Ok(Some((packet_id, encode_body(&body)?)))
            }
            ClientAction::ResourcePackResponse { id, response }
                if matches!(
                    state,
                    ConnectionState::Configuration | ConnectionState::Play
                ) =>
            {
                let body = ResourcePackResponse {
                    id: *id,
                    action: resource_pack_response_ordinal(*response),
                };
                let packet_id = match state {
                    ConnectionState::Configuration => configuration::serverbound::RESOURCE_PACK,
                    _ => play::serverbound::RESOURCE_PACK,
                };
                Ok(Some((packet_id, encode_body(&body)?)))
            }
            ClientAction::EndClientTick if state == ConnectionState::Play => Ok(Some((
                play::serverbound::CLIENT_TICK_END,
                encode_body(&ClientTickEnd)?,
            ))),
            ClientAction::ContainerButtonClick {
                window_id,
                button_id,
            } if state == ConnectionState::Play => {
                let body = ContainerButtonClick {
                    window_id: *window_id,
                    button_id: *button_id,
                };
                Ok(Some((
                    play::serverbound::CONTAINER_BUTTON_CLICK,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetFlying { flying } if state == ConnectionState::Play => {
                let flags = if *flying {
                    SERVERBOUND_ABILITY_FLAG_FLYING
                } else {
                    0
                };
                let body = ServerboundPlayerAbilities { flags };
                Ok(Some((
                    play::serverbound::PLAYER_ABILITIES,
                    encode_body(&body)?,
                )))
            }
            ClientAction::RenameItem { name } if state == ConnectionState::Play => {
                let body = RenameItem { name: name.clone() };
                Ok(Some((play::serverbound::RENAME_ITEM, encode_body(&body)?)))
            }
            ClientAction::SelectTrade { index } if state == ConnectionState::Play => {
                let body = SelectTrade { index: *index };
                Ok(Some((play::serverbound::SELECT_TRADE, encode_body(&body)?)))
            }
            ClientAction::PickItemFromBlock { pos, include_data }
                if state == ConnectionState::Play =>
            {
                let body = PickItemFromBlock {
                    pos: pack_block_pos(*pos),
                    include_data: *include_data,
                };
                Ok(Some((
                    play::serverbound::PICK_ITEM_FROM_BLOCK,
                    encode_body(&body)?,
                )))
            }
            ClientAction::PickItemFromEntity {
                entity_id,
                include_data,
            } if state == ConnectionState::Play => {
                let body = PickItemFromEntity {
                    entity_id: *entity_id,
                    include_data: *include_data,
                };
                Ok(Some((
                    play::serverbound::PICK_ITEM_FROM_ENTITY,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetBeaconEffects { primary, secondary }
                if state == ConnectionState::Play =>
            {
                let payload = encode_set_beacon(primary.as_ref(), secondary.as_ref())?;
                Ok(Some((play::serverbound::SET_BEACON, payload)))
            }
            ClientAction::EditBook { slot, pages, title } if state == ConnectionState::Play => {
                let body = EditBook {
                    slot: *slot,
                    pages: pages.clone(),
                    title: title.clone(),
                };
                Ok(Some((play::serverbound::EDIT_BOOK, encode_body(&body)?)))
            }
            ClientAction::SignUpdate {
                pos,
                is_front_text,
                lines,
            } if state == ConnectionState::Play => {
                let [line0, line1, line2, line3] = lines.clone();
                let body = SignUpdate {
                    pos: pack_block_pos(*pos),
                    is_front_text: *is_front_text,
                    line0,
                    line1,
                    line2,
                    line3,
                };
                Ok(Some((play::serverbound::SIGN_UPDATE, encode_body(&body)?)))
            }
            ClientAction::SetCommandBlock {
                pos,
                command,
                mode,
                track_output,
                conditional,
                automatic,
            } if state == ConnectionState::Play => {
                let flags = (if *track_output {
                    COMMAND_BLOCK_FLAG_TRACK_OUTPUT
                } else {
                    0
                }) | (if *conditional {
                    COMMAND_BLOCK_FLAG_CONDITIONAL
                } else {
                    0
                }) | (if *automatic {
                    COMMAND_BLOCK_FLAG_AUTOMATIC
                } else {
                    0
                });
                let body = SetCommandBlock {
                    pos: pack_block_pos(*pos),
                    command: command.clone(),
                    mode: command_block_mode_ordinal(*mode),
                    flags,
                };
                Ok(Some((
                    play::serverbound::SET_COMMAND_BLOCK,
                    encode_body(&body)?,
                )))
            }
            ClientAction::PlayerLoaded if state == ConnectionState::Play => Ok(Some((
                play::serverbound::PLAYER_LOADED,
                encode_body(&PlayerLoaded)?,
            ))),
            ClientAction::SeenAdvancements { tab } if state == ConnectionState::Play => Ok(Some((
                play::serverbound::SEEN_ADVANCEMENTS,
                encode_seen_advancements(tab.as_ref())?,
            ))),
            ClientAction::CommandSuggestion { id, command } if state == ConnectionState::Play => {
                let body = CommandSuggestion {
                    id: *id,
                    command: command.clone(),
                };
                Ok(Some((
                    play::serverbound::COMMAND_SUGGESTION,
                    encode_body(&body)?,
                )))
            }
            ClientAction::PaddleBoat { left, right } if state == ConnectionState::Play => {
                let body = PaddleBoat {
                    left: *left,
                    right: *right,
                };
                Ok(Some((play::serverbound::PADDLE_BOAT, encode_body(&body)?)))
            }
            ClientAction::MoveVehicle {
                pos,
                rotation,
                on_ground,
            } if state == ConnectionState::Play => {
                let body = MoveVehicle {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                    yaw: rotation.yaw,
                    pitch: rotation.pitch,
                    on_ground: *on_ground,
                };
                Ok(Some((play::serverbound::MOVE_VEHICLE, encode_body(&body)?)))
            }
            ClientAction::SelectBundleItem {
                slot_id,
                selected_item_index,
            } if state == ConnectionState::Play => {
                let body = SelectBundleItem {
                    slot_id: *slot_id,
                    selected_item_index: *selected_item_index,
                };
                Ok(Some((
                    play::serverbound::BUNDLE_ITEM_SELECTED,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetContainerSlotState {
                slot_id,
                container_id,
                new_state,
            } if state == ConnectionState::Play => {
                let body = ContainerSlotStateChanged {
                    slot_id: *slot_id,
                    container_id: *container_id,
                    new_state: *new_state,
                };
                Ok(Some((
                    play::serverbound::CONTAINER_SLOT_STATE_CHANGED,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetRecipeBookSettings {
                book_type,
                open,
                filtering,
            } if state == ConnectionState::Play => {
                let body = RecipeBookChangeSettings {
                    book_type: recipe_book_type_to_ordinal(*book_type),
                    is_open: *open,
                    is_filtering: *filtering,
                };
                Ok(Some((
                    play::serverbound::RECIPE_BOOK_CHANGE_SETTINGS,
                    encode_body(&body)?,
                )))
            }
            ClientAction::RecipeBookSeenRecipe { recipe } if state == ConnectionState::Play => {
                let body = RecipeBookSeenRecipe { recipe: *recipe };
                Ok(Some((
                    play::serverbound::RECIPE_BOOK_SEEN_RECIPE,
                    encode_body(&body)?,
                )))
            }
            ClientAction::PlaceRecipe {
                container_id,
                recipe,
                use_max_items,
            } if state == ConnectionState::Play => {
                let body = PlaceRecipe {
                    container_id: *container_id,
                    recipe: *recipe,
                    use_max_items: *use_max_items,
                };
                Ok(Some((play::serverbound::PLACE_RECIPE, encode_body(&body)?)))
            }
            // Play-state only: the identically-shaped status-state ping is
            // driven by the ping flow, not by a canonical client action.
            ClientAction::PingRequest { time } if state == ConnectionState::Play => {
                let body = PingRequest { time: *time };
                Ok(Some((
                    play::serverbound::PING_REQUEST,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SpectatorAction { target_entity_id } if state == ConnectionState::Play => {
                Ok(Some((
                    play::serverbound::SPECTATOR_ACTION,
                    encode_spectator_action(*target_entity_id)?,
                )))
            }
            ClientAction::TeleportToEntity { target } if state == ConnectionState::Play => {
                let body = TeleportToEntity { uuid: *target };
                Ok(Some((
                    play::serverbound::TELEPORT_TO_ENTITY,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ChangeGameMode { mode } if state == ConnectionState::Play => {
                let body = ChangeGameMode {
                    mode: game_mode_to_ordinal(*mode),
                };
                Ok(Some((
                    play::serverbound::CHANGE_GAME_MODE,
                    encode_body(&body)?,
                )))
            }
            // `cookie_response` exists in Login, Configuration and
            // Play alike (`ServerCookiePacketListener` is common to all
            // three), so this is one arm with a per-state packet id rather
            // than three separate ones.
            ClientAction::CookieResponse { key, payload } => {
                let packet_id = match state {
                    ConnectionState::Login => login::serverbound::COOKIE_RESPONSE,
                    ConnectionState::Configuration => configuration::serverbound::COOKIE_RESPONSE,
                    ConnectionState::Play => play::serverbound::COOKIE_RESPONSE,
                    ConnectionState::Handshaking | ConnectionState::Status => return Ok(None),
                };
                let body = CookieResponse {
                    key: key.to_string(),
                    payload: payload.clone(),
                };
                Ok(Some((packet_id, encode_body(&body)?)))
            }

            // ---- the operator/debug set --------------------------------------
            ClientAction::QueryBlockEntityTag {
                transaction_id,
                pos,
            } if state == ConnectionState::Play => {
                let body = BlockEntityTagQuery {
                    transaction_id: *transaction_id,
                    pos: pack_block_pos(*pos),
                };
                Ok(Some((
                    play::serverbound::BLOCK_ENTITY_TAG_QUERY,
                    encode_body(&body)?,
                )))
            }
            ClientAction::QueryEntityTag {
                transaction_id,
                entity_id,
            } if state == ConnectionState::Play => {
                let body = EntityTagQuery {
                    transaction_id: *transaction_id,
                    entity_id: *entity_id,
                };
                Ok(Some((
                    play::serverbound::ENTITY_TAG_QUERY,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ChangeDifficulty { difficulty } if state == ConnectionState::Play => {
                let mut w = Writer::default();
                w.var_i32(difficulty_id(*difficulty));
                Ok(Some((
                    play::serverbound::CHANGE_DIFFICULTY,
                    w.into_vec(),
                )))
            }
            ClientAction::LockDifficulty { locked } if state == ConnectionState::Play => {
                let mut w = Writer::default();
                w.bool(*locked);
                Ok(Some((play::serverbound::LOCK_DIFFICULTY, w.into_vec())))
            }
            ClientAction::SetGameRules { entries } if state == ConnectionState::Play => Ok(Some((
                play::serverbound::SET_GAME_RULE,
                encode_set_game_rules(entries)?,
            ))),
            ClientAction::SetCommandMinecart {
                entity_id,
                command,
                track_output,
            } if state == ConnectionState::Play => {
                let mut w = Writer::default();
                w.var_i32(*entity_id);
                w.string(command);
                w.bool(*track_output);
                Ok(Some((
                    play::serverbound::SET_COMMAND_MINECART,
                    w.into_vec(),
                )))
            }
            ClientAction::SetStructureBlock {
                pos,
                update_type,
                mode,
                name,
                offset,
                size,
                mirror,
                rotation,
                data,
                integrity,
                seed,
                ignore_entities,
                show_air,
                show_bounding_box,
                strict,
            } if state == ConnectionState::Play => {
                // Vanilla's own structure-block packet writer's flag bits, in the
                // order the read side unpacks them.
                let flags = u8::from(*ignore_entities)
                    | (u8::from(*show_air) << 1)
                    | (u8::from(*show_bounding_box) << 2)
                    | (u8::from(*strict) << 3);
                Ok(Some((
                    play::serverbound::SET_STRUCTURE_BLOCK,
                    encode_set_structure_block(
                        *pos,
                        *update_type,
                        *mode,
                        name,
                        *offset,
                        *size,
                        *mirror,
                        *rotation,
                        data,
                        *integrity,
                        *seed,
                        flags,
                    )?,
                )))
            }
            ClientAction::SetJigsawBlock {
                pos,
                name,
                target,
                pool,
                final_state,
                joint,
                selection_priority,
                placement_priority,
            } if state == ConnectionState::Play => Ok(Some((
                play::serverbound::SET_JIGSAW_BLOCK,
                encode_set_jigsaw_block(
                    *pos,
                    name,
                    target,
                    pool,
                    final_state,
                    *joint,
                    *selection_priority,
                    *placement_priority,
                )?,
            ))),
            ClientAction::GenerateJigsawStructure {
                pos,
                levels,
                keep_jigsaws,
            } if state == ConnectionState::Play => {
                let mut w = Writer::default();
                w.i64(pack_block_pos(*pos));
                w.var_i32(*levels);
                w.bool(*keep_jigsaws);
                Ok(Some((play::serverbound::JIGSAW_GENERATE, w.into_vec())))
            }
            ClientAction::SetTestBlock { pos, mode, message }
                if state == ConnectionState::Play =>
            {
                let mut w = Writer::default();
                w.i64(pack_block_pos(*pos));
                w.var_i32(match mode {
                    ModelTestBlockMode::Start => 0,
                    ModelTestBlockMode::Log => 1,
                    ModelTestBlockMode::Fail => 2,
                    ModelTestBlockMode::Accept => 3,
                });
                w.string(message);
                Ok(Some((play::serverbound::SET_TEST_BLOCK, w.into_vec())))
            }
            ClientAction::TestInstanceBlockAction { pos, action, data }
                if state == ConnectionState::Play =>
            {
                Ok(Some((
                    play::serverbound::TEST_INSTANCE_BLOCK_ACTION,
                    encode_test_instance_block_action(*pos, *action, data)?,
                )))
            }
            ClientAction::SubscribeDebug { subscriptions } if state == ConnectionState::Play => {
                Ok(Some((
                    play::serverbound::DEBUG_SUBSCRIPTION_REQUEST,
                    encode_debug_subscription_request(subscriptions)?,
                )))
            }
            // Present in Configuration and Play alike: `custom_click_action` is a
            // `ServerCommonPacketListener` packet, like `custom_payload` itself,
            // because `show_dialog` can be sent in either state.
            ClientAction::CustomClickAction { id, payload } => {
                let packet_id = match state {
                    ConnectionState::Configuration => {
                        configuration::serverbound::CUSTOM_CLICK_ACTION
                    }
                    ConnectionState::Play => play::serverbound::CUSTOM_CLICK_ACTION,
                    ConnectionState::Handshaking
                    | ConnectionState::Status
                    | ConnectionState::Login => return Ok(None),
                };
                Ok(Some((packet_id, encode_custom_click_action(id, payload)?)))
            }
            _ => Ok(None),
        }
    }
}
