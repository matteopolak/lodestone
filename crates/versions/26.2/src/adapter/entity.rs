//! Entity packets: spawn/remove/move, metadata, attributes, damage and
//! status effects. Split out of the former monolithic `adapter.rs`.
use super::*;

impl V770Adapter {
    /// Clientbound play-state packets in the entity domain, split out of the
    /// former monolithic `handle_play` (see `adapter::mod` for the coordinator).
    pub(super) fn handle_play_entity(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == play::clientbound::SET_EQUIPMENT {
            // An entity id, then a continuation-flagged list: each entry is a
            // slot byte whose low 7 bits are the `EquipmentSlot` ordinal and
            // whose high bit signals another entry follows, then an item stack.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let mut equipment = Vec::new();
            let mut complete = true;
            loop {
                let slot_byte = reader.u8().map_err(dec_err)?;
                let ordinal = slot_byte & 0x7F;
                let slot = EquipmentSlot::from_ordinal(ordinal).ok_or_else(|| {
                    AdapterError::Decode(format!("unknown equipment slot ordinal {ordinal}"))
                })?;
                let decoded = read_item_stack(&mut reader)?;
                let (item, partial) = match decoded {
                    DecodedStack::Complete(stack) => (stack, false),
                    DecodedStack::Partial(stack) => (stack, true),
                };
                equipment.push(EntityEquipment { slot, item });
                if partial {
                    // An unmodeled component ended the patch; further list
                    // entries are unreadable. Deliver what decoded and stop.
                    complete = false;
                    break;
                }
                if slot_byte & 0x80 == 0 {
                    break;
                }
            }
            if complete {
                reader.ensure_empty().map_err(dec_err)?;
            }
            return Ok(vec![Directive::Emit(ClientEvent::EntityEquipmentUpdated {
                entity_id,
                equipment,
            })]);
        }
        if packet_id == play::clientbound::ADD_ENTITY {
            return handle_add_entity(payload, &self.variants);
        }
        if packet_id == play::clientbound::REMOVE_ENTITIES {
            return handle_remove_entities(payload, &self.variants);
        }
        if packet_id == play::clientbound::MOVE_ENTITY_POS {
            return handle_move_entity(payload, true, false);
        }
        if packet_id == play::clientbound::MOVE_ENTITY_POS_ROT {
            return handle_move_entity(payload, true, true);
        }
        if packet_id == play::clientbound::MOVE_ENTITY_ROT {
            return handle_move_entity(payload, false, true);
        }
        if packet_id == play::clientbound::TELEPORT_ENTITY {
            return handle_entity_position(payload, true);
        }
        if packet_id == play::clientbound::ENTITY_POSITION_SYNC {
            return handle_entity_position(payload, false);
        }
        if packet_id == play::clientbound::SET_ENTITY_MOTION {
            return handle_set_entity_motion(payload);
        }
        if packet_id == play::clientbound::MOVE_MINECART_ALONG_TRACK {
            return handle_move_minecart_along_track(payload);
        }
        if packet_id == play::clientbound::SET_ENTITY_DATA {
            return Ok(handle_set_entity_data(payload, &self.variants));
        }
        if packet_id == play::clientbound::UPDATE_ATTRIBUTES {
            return Ok(handle_update_attributes(payload));
        }
        if packet_id == play::clientbound::ENTITY_EVENT {
            // Raw `int` entity id (NOT a VarInt — one of the few remaining
            // fixed-width ids in play) then a raw status byte.
            let mut reader = Reader::new(payload);
            let entity_id = reader.i32().map_err(dec_err)?;
            let status = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityStatus {
                entity_id,
                status,
            })]);
        }
        if packet_id == play::clientbound::ROTATE_HEAD {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let packed = reader.i8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityHeadRotation {
                entity_id,
                head_yaw: unpack_degrees(packed),
            })]);
        }
        if packet_id == play::clientbound::SET_PASSENGERS {
            // A VarInt vehicle id then a VarInt-length-prefixed VarInt array —
            // `readVarIntArray`, not the general `Vec<T>` derive shape, so read
            // by hand.
            let mut reader = Reader::new(payload);
            let vehicle_id = reader.var_i32().map_err(dec_err)?;
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count)
                .map_err(|_| AdapterError::Decode(format!("negative passenger count {count}")))?;
            let mut passenger_ids = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                passenger_ids.push(reader.var_i32().map_err(dec_err)?);
            }
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::EntityPassengersChanged {
                    vehicle_id,
                    passenger_ids,
                },
            )]);
        }
        if packet_id == play::clientbound::SET_ENTITY_LINK {
            // Two raw `int`s (source, dest); dest `0` means "no holder", matching
            // vanilla's own sentinel (entity id 0 is never a valid entity).
            let mut reader = Reader::new(payload);
            let entity_id = reader.i32().map_err(dec_err)?;
            let holder_id = reader.i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityLeashed {
                entity_id,
                holder_id: (holder_id != 0).then_some(holder_id),
            })]);
        }
        if packet_id == play::clientbound::TAKE_ITEM_ENTITY {
            let mut reader = Reader::new(payload);
            let item_entity_id = reader.var_i32().map_err(dec_err)?;
            let player_id = reader.var_i32().map_err(dec_err)?;
            let amount = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ItemPickup {
                item_entity_id,
                player_id,
                amount,
            })]);
        }
        if packet_id == play::clientbound::DAMAGE_EVENT {
            return decode_damage_event(payload);
        }
        if packet_id == play::clientbound::HURT_ANIMATION {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let yaw = reader.f32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityHurtAnimation {
                entity_id,
                yaw,
            })]);
        }
        if packet_id == play::clientbound::ANIMATE {
            // A fixed, sparse set of named action constants (`1` is reserved and
            // never sent); anything else travels through `Other` rather than
            // being rejected, since a future action byte is still meaningful to
            // a consumer even if this table does not name it.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let action = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let action = match action {
                0 => AnimationAction::SwingMainHand,
                2 => AnimationAction::WakeUp,
                3 => AnimationAction::SwingOffHand,
                4 => AnimationAction::CriticalHit,
                5 => AnimationAction::MagicCriticalHit,
                other => AnimationAction::Other(other),
            };
            return Ok(vec![Directive::Emit(ClientEvent::EntityAnimation {
                entity_id,
                action,
            })]);
        }
        if packet_id == play::clientbound::UPDATE_MOB_EFFECT {
            // entity id, a `minecraft:mob_effect` registry VarInt id (a fixed,
            // built-in registry — unlike damage_type — so resolved to a name via
            // the generated table), amplifier, duration, then a bitset byte.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let effect_id = reader.var_i32().map_err(dec_err)?;
            let amplifier = reader.var_i32().map_err(dec_err)?;
            let duration_ticks = reader.var_i32().map_err(dec_err)?;
            let flags = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let name = mob_effect_name(effect_id).ok_or_else(|| {
                AdapterError::Decode(format!("unknown mob effect id {effect_id}"))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::MobEffectApplied {
                entity_id,
                effect: parse_key(name, "mob effect")?,
                amplifier,
                duration_ticks,
                ambient: flags & 0x1 != 0,
                visible: flags & 0x2 != 0,
                show_icon: flags & 0x4 != 0,
                blend: flags & 0x8 != 0,
            })]);
        }
        if packet_id == play::clientbound::REMOVE_MOB_EFFECT {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let effect_id = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let name = mob_effect_name(effect_id).ok_or_else(|| {
                AdapterError::Decode(format!("unknown mob effect id {effect_id}"))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::MobEffectRemoved {
                entity_id,
                effect: parse_key(name, "mob effect")?,
            })]);
        }
        if packet_id == play::clientbound::MOVE_VEHICLE {
            let mut reader = Reader::new(payload);
            let x = reader.f64().map_err(dec_err)?;
            let y = reader.f64().map_err(dec_err)?;
            let z = reader.f64().map_err(dec_err)?;
            let yaw = reader.f32().map_err(dec_err)?;
            let pitch = reader.f32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::VehicleMoved {
                pos: Vec3 { x, y, z },
                yaw,
                pitch,
            })]);
        }
        if packet_id == play::clientbound::PROJECTILE_POWER {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let acceleration_power = reader.f64().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ProjectilePowerChanged {
                entity_id,
                acceleration_power,
            })]);
        }
        Ok(Vec::new())
    }
}

/// Decodes `damage_event`: entity id, a `minecraft:damage_type` registry id
/// (vanilla's own holder-registry codec, a plain VarInt — carried raw, see
/// [`ClientEvent::EntityDamaged`] for why), then the cause/direct entity ids
/// each wire-encoded as `id + 1` (so `0` means "none", decoded here back to
/// `-1` via `varint - 1` to match vanilla's own `readOptionalEntityId`), and
/// finally a self-contained `Optional<Vec3>` (a bool presence flag then, only
/// if set, three plain `f64`s) — the one shape in this packet the `Decode`
/// derive's `present_if` (which only reads a *prior named field*, not an
/// inline bool) cannot express, so it is read by hand like the rest of this
/// packet.
fn decode_damage_event(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let damage_type_id = reader.var_i32().map_err(dec_err)?;
    let cause_id = reader.var_i32().map_err(dec_err)? - 1;
    let direct_id = reader.var_i32().map_err(dec_err)? - 1;
    let has_pos = reader.bool().map_err(dec_err)?;
    let source_pos = if has_pos {
        let x = reader.f64().map_err(dec_err)?;
        let y = reader.f64().map_err(dec_err)?;
        let z = reader.f64().map_err(dec_err)?;
        Some(Vec3 { x, y, z })
    } else {
        None
    };
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::EntityDamaged {
        entity_id,
        damage_type_id,
        cause_id: (cause_id != -1).then_some(cause_id),
        direct_id: (direct_id != -1).then_some(direct_id),
        source_pos,
    })])
}

/// The delta-position scale for `move_entity_*` packets: each short is
/// `1/4096` of a block (`ClientboundMoveEntityPacket`).
const MOVE_DELTA_SCALE: f64 = 4096.0;
/// Decodes `add_entity` into a canonical spawn event, plus an initial
/// head-rotation event.
///
/// Wire layout (`ClientboundAddEntityPacket`): VarInt entity id, UUID, VarInt
/// entity-type registry id, position `f64`×3, low-precision velocity, three
/// signed-byte angles (pitch, yaw, head yaw), and a VarInt data field. The type
/// id is resolved to its canonical identifier through the version-specific
/// [`entity_type_name`] table.
///
/// Head yaw is carried separately from body yaw on the wire (they diverge
/// constantly once a mob starts looking around) and vanilla sends it
/// unconditionally at spawn, so it is surfaced through the same
/// [`ClientEvent::EntityHeadRotation`] outlet [`ROTATE_HEAD`](play::clientbound::ROTATE_HEAD)
/// uses for later updates, rather than widening [`ClientEvent::EntitySpawned`]
/// itself — that struct is shared across every protocol version's adapter, and
/// adding a field to it would force edits into v1-8/v1-9/v1-14 outside this
/// crate's scope.
fn handle_add_entity(
    payload: &[u8],
    variants: &Mutex<HashMap<i32, TrackedEntity>>,
) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let uuid = reader.uuid().map_err(dec_err)?;
    let type_id = reader.var_i32().map_err(dec_err)?;
    let x = reader.f64().map_err(dec_err)?;
    let y = reader.f64().map_err(dec_err)?;
    let z = reader.f64().map_err(dec_err)?;
    let (vx, vy, vz) = read_lp_vec3(&mut reader).map_err(dec_err)?;
    let pitch = reader.i8().map_err(dec_err)?;
    let yaw = reader.i8().map_err(dec_err)?;
    let head_yaw = reader.i8().map_err(dec_err)?;
    // The **Object Data** field: one trailing VarInt whose meaning is decided
    // entirely by the entity type, read in that type's own client-side
    // spawn-packet reconstruction. Most types ignore it; the falling-block
    // entity's own reconstruction resolves it through the block-state
    // registry's id lookup, resolved below once the type is known.
    let data = reader.var_i32().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;

    let name = entity_type_name(type_id).ok_or_else(|| {
        AdapterError::Decode(format!("unknown entity-type id {type_id} in add_entity"))
    })?;
    let entity_type = name.parse().map_err(|_| {
        AdapterError::Decode(format!(
            "entity-type id {type_id} is not a valid key: {name}"
        ))
    })?;

    // Remember the facts a later `set_entity_data` cannot recover from the wire:
    // the concrete class for mobs whose variant index is ambiguous, whether the
    // type is a `LivingEntity` (which decides whether index 8's byte is a
    // using-item bitfield or an arrow's crit flag — see `IDX_LIVING_FLAGS`), and
    // whether it is a `Mob` (index 15: mob flags, or an armour stand's client
    // flags — see `IDX_MOB_FLAGS`). Types with none of those stay out of the map,
    // so it is still bounded to the mobs actually present rather than every
    // entity in render distance.
    //
    // `is_living`/`is_mob` returning `None` for an id outside the census means "we
    // cannot establish it", which fails closed to `false`: a missing pose is a
    // visible gap, a wrongly-decoded flags byte is a silent lie.
    let tracked = TrackedEntity {
        class: metadata_class(name),
        living: lodestone_data::entity_census::is_living(type_id).unwrap_or(false),
        mob: lodestone_data::entity_census::is_mob(type_id).unwrap_or(false),
    };
    if tracked.is_tracked()
        && let Ok(mut map) = variants.lock()
    {
        map.insert(entity_id, tracked);
    }

    let mut directives = vec![
        Directive::Emit(ClientEvent::EntitySpawned {
            entity_id,
            uuid: Some(uuid),
            entity_type,
            pos: Vec3::new(x, y, z),
            rotation: Rotation::new(unpack_degrees(yaw), unpack_degrees(pitch)),
            velocity: Some(Vec3::new(vx, vy, vz)),
        }),
        Directive::Emit(ClientEvent::EntityHeadRotation {
            entity_id,
            head_yaw: unpack_degrees(head_yaw),
        }),
    ];

    // Vanilla's own tracked-entity-data mechanism only ever puts a field on
    // the wire when it differs from the accessor's own default (its own
    // non-default-value filter — the only source the server-side entity
    // wrapper ever feeds a spawn's initial `set_entity_data`; see the sheep
    // class's own accessor registration, whose wool index defaults to byte
    // `0`).
    // A naturally white, unsheared sheep (colour ordinal 0, sheared bit unset —
    // exactly byte `0`) therefore never puts index 18 on the wire at all, not
    // just at spawn: `read_entity_metadata` never sees the byte, `variant` stays
    // `None`, and every consumer keyed on `Some(EntityVariant::Dyed { .. })`
    // (`entities::sheep_wool`) draws no wool. A dyed or sheared sheep works
    // today because *that* state is non-default and is always on the wire.
    //
    // The fix is synthesizing the vanilla default here, once, as an ordinary
    // `EntityMetadataUpdated` event through the exact same channel a real
    // `set_entity_data` uses — so every downstream consumer (the ECS fold, the
    // shell snapshot) needs no special case for "unreported": a real
    // `set_entity_data` naming index 18 (dye, shear) is decoded afterward in
    // packet order and overwrites this default exactly as it would overwrite
    // any other synthesized-then-corrected value. Only sheep gets this: horse's
    // default variant is deferred (see `docs/entity-rendering.md`'s variant
    // census) rather than guessed at without the same wire confirmation.
    if tracked.class == Some(MetadataClass::Sheep) {
        directives.push(Directive::Emit(ClientEvent::EntityMetadataUpdated {
            entity_id,
            metadata: EntityMetadataUpdate {
                variant: Some(EntityVariant::Dyed {
                    color: 0,
                    sheared: false,
                }),
                ..EntityMetadataUpdate::default()
            },
        }));
    }

    // Same synthesis, same reason, for a creeper's three fields
    // (confirmed against the decompiled creeper source's own metadata-index
    // defaults: swell dir `-1` / powered `false` / ignited `false`). An ordinary,
    // uncharged, unlit creeper is *entirely* at its accessors' defaults, so a
    // real spawn's initial `set_entity_data` never mentions any of the three —
    // without this, a fresh creeper's `creeper_swell_dir` stays `None` forever
    // rather than the vanilla-true `Some(-1)`, until the moment it primes
    // changes it to a non-default value the wire actually carries.
    if tracked.class == Some(MetadataClass::Creeper) {
        directives.push(Directive::Emit(ClientEvent::EntityMetadataUpdated {
            entity_id,
            metadata: EntityMetadataUpdate {
                creeper_swell_dir: Some(-1),
                creeper_powered: Some(false),
                creeper_ignited: Some(false),
                ..EntityMetadataUpdate::default()
            },
        }));
    }

    // Same synthesis, same reason, for a painting's variant. The painting
    // class's own accessor defaults to vanilla's own any-entry-from-registry
    // helper — the registry's *first* entry, which for the vanilla
    // `painting_variant` registry is `minecraft:alban` (the registry is
    // loaded from data files and so is in sorted key order; see
    // `entity_variants::PAINTING`). A painting hung with that variant is
    // therefore entirely at its accessors' defaults and puts **no** index-9
    // field on the wire, so without this it would draw nothing at all while
    // every other painting drew fine — the most confusing possible failure,
    // because 50 of the 51 would work.
    //
    // Note there is nothing to synthesize for the painting's *facing*:
    // vanilla's own hanging-entity direction setter writes it into the
    // entity's ordinary yaw, which `EntitySpawned` above already carries.
    if name == PAINTING_TYPE {
        directives.push(Directive::Emit(ClientEvent::EntityMetadataUpdated {
            entity_id,
            metadata: EntityMetadataUpdate {
                painting_variant: crate::entity_variants::painting_variant(0)
                    .and_then(|key| key.parse().ok()),
                ..EntityMetadataUpdate::default()
            },
        }));
    }

    // Vanilla's own falling-block spawn-packet reconstruction: the Object
    // Data field read above is the block-state registry's own id lookup and
    // is the **only** place the imitated state appears on the wire — its own
    // accessor registration defines `DATA_START_POS` alone, so no
    // `set_entity_data` ever carries it. A consumer that never learns it
    // draws whatever state id `0` happens to be, with nothing logged.
    //
    // Emitted after `EntitySpawned` so a consumer keyed on the entity id always
    // sees the entity first. Guarded on the type rather than emitted for every
    // spawn: the field means something different for every type that reads it
    // (a display block, an item-frame rotation), and one event that claimed to
    // carry "a block state" for all of them would be wrong for most.
    // Vanilla's own fishing-hook spawn-packet builder puts the caster's
    // entity id in the same Object Data field — the owner's entity id, or
    // this entity's own id when ownerless, so it is never the `0` a bare
    // projectile would write for an ownerless shot. Nothing else carries it:
    // the fishing hook's own accessor registration defines only
    // `DATA_HOOKED_ENTITY` and `DATA_BITING`, so a client that drops this
    // field has no way to learn where the line is anchored and can only draw
    // the bobber floating unattached.
    //
    // Guarded on the type for the reason the falling block's arm below is: this
    // one VarInt means something different for every type that reads it.
    if name == FISHING_BOBBER_TYPE {
        directives.push(Directive::Emit(ClientEvent::ProjectileOwner {
            entity_id,
            owner_id: data,
        }));
    }

    if name == FALLING_BLOCK_TYPE {
        directives.push(Directive::Emit(ClientEvent::FallingBlockState {
            entity_id,
            // `max(0)` then a cast: the wire field is a signed VarInt and a
            // negative value is not a state id. Clamping to `0` (air, which bakes
            // no quads and therefore draws nothing) is the one reading that cannot
            // panic or wrap into a plausible-looking wrong block.
            block_state_id: data.max(0) as u32,
        }));
    }

    Ok(directives)
}

/// `vanilla's own entity types's own painting`'s registry key — the type whose default variant is
/// synthesized at spawn, above.
const PAINTING_TYPE: &str = "minecraft:painting";
/// `vanilla's own entity types's own falling block`'s registry key — one of the two entity types
/// whose `ADD_ENTITY` Object Data field this adapter interprets.
const FALLING_BLOCK_TYPE: &str = "minecraft:falling_block";
/// `vanilla's own entity types's own fishing bobber`'s registry key — the other. Its Object Data is
/// the caster's entity id, not a block state; see the arm that reads it.
const FISHING_BOBBER_TYPE: &str = "minecraft:fishing_bobber";
/// Decodes `remove_entities` (a VarInt-length list of VarInt ids) into a removal
/// event.
fn handle_remove_entities(
    payload: &[u8],
    variants: &Mutex<HashMap<i32, TrackedEntity>>,
) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("negative remove_entities count {count}")))?;
    // Cap the reservation at the readable bytes, the same way `decode_vec`
    // does: `count` is attacker-controlled and each id costs at least one
    // byte, so no more than `remaining()` can be produced. Reserving `count`
    // outright lets a tiny payload demand an unbounded allocation.
    let mut entity_ids = Vec::with_capacity(count.min(reader.remaining()));
    for _ in 0..count {
        entity_ids.push(reader.var_i32().map_err(dec_err)?);
    }
    reader.ensure_empty().map_err(dec_err)?;
    if let Ok(mut map) = variants.lock() {
        for id in &entity_ids {
            map.remove(id);
        }
    }
    Ok(vec![Directive::Emit(ClientEvent::EntityRemoved {
        entity_ids,
    })])
}

/// Decodes a `move_entity_*` packet into a relative-movement event. `has_pos`
/// and `has_rot` select which of the three variants (`pos`, `pos_rot`, `rot`)
/// is present: each short position delta is `1/4096` of a block and each angle
/// is a signed byte.
fn handle_move_entity(
    payload: &[u8],
    has_pos: bool,
    has_rot: bool,
) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let delta = if has_pos {
        let dx = f64::from(reader.i16().map_err(dec_err)?) / MOVE_DELTA_SCALE;
        let dy = f64::from(reader.i16().map_err(dec_err)?) / MOVE_DELTA_SCALE;
        let dz = f64::from(reader.i16().map_err(dec_err)?) / MOVE_DELTA_SCALE;
        Vec3::new(dx, dy, dz)
    } else {
        Vec3::new(0.0, 0.0, 0.0)
    };
    let rotation = if has_rot {
        let yaw = reader.i8().map_err(dec_err)?;
        let pitch = reader.i8().map_err(dec_err)?;
        Some(Rotation::new(unpack_degrees(yaw), unpack_degrees(pitch)))
    } else {
        None
    };
    let on_ground = reader.bool().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;

    Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
        entity_id,
        movement: EntityMovement::Relative(delta),
        rotation,
        on_ground,
    })])
}

/// Decodes an absolute entity position update. `has_relatives` selects between
/// `teleport_entity` (which carries a trailing `Relative` bit set) and
/// `entity_position_sync` (which does not); both share a leading VarInt id and
/// `PositionMoveRotation`, then a trailing on-ground boolean. The delta-movement
/// is consumed for alignment; velocity is surfaced separately via
/// `set_entity_motion`.
fn handle_entity_position(
    payload: &[u8],
    has_relatives: bool,
) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let x = reader.f64().map_err(dec_err)?;
    let y = reader.f64().map_err(dec_err)?;
    let z = reader.f64().map_err(dec_err)?;
    let _dx = reader.f64().map_err(dec_err)?;
    let _dy = reader.f64().map_err(dec_err)?;
    let _dz = reader.f64().map_err(dec_err)?;
    let yaw = reader.f32().map_err(dec_err)?;
    let pitch = reader.f32().map_err(dec_err)?;
    if has_relatives {
        let _relatives = reader.i32().map_err(dec_err)?;
    }
    let on_ground = reader.bool().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;

    Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
        entity_id,
        movement: EntityMovement::Absolute(Vec3::new(x, y, z)),
        rotation: Some(Rotation::new(yaw, pitch)),
        on_ground,
    })])
}

/// Decodes `set_entity_motion` (VarInt id, low-precision velocity) into a
/// velocity event.
fn handle_set_entity_motion(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let (vx, vy, vz) = read_lp_vec3(&mut reader).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::EntityVelocity {
        entity_id,
        velocity: Vec3::new(vx, vy, vz),
    })])
}

/// Decodes `move_minecart_along_track`: a VarInt entity id followed by a
/// VarInt-counted list of `vanilla's own new minecart behavior's own minecart step` lerp steps, each
/// `(Vec3 position, Vec3 movement, ROTATION_BYTE yRot, ROTATION_BYTE xRot,
/// f32 weight)` in that order — verified against
/// `vanilla's own new minecart behavior's own minecart step's own stream codec` in 26.2 decompiled source.
/// `vanilla's own vec3's own stream codec` is three big-endian f64s (matching every other
/// absolute-position decode in this adapter); `ROTATION_BYTE` is the same
/// signed-byte-angle encoding [`unpack_degrees`] already inverts for
/// `rotate_head`/`move_entity_*`.
///
/// Vanilla spends the whole list smoothly interpolating the cart across the
/// tick window the steps span (a curved rail sends more than one step per
/// packet); this adapter has no multi-waypoint movement event, so every
/// step's bytes are read and validated — a wire-format drift is still
/// caught — but only the **terminal** step's position/velocity/rotation is
/// applied, as an absolute jump rather than a spline. That is a documented
/// fidelity loss (movement will look stepped on curved track), not a
/// misdecode: minecarts stopped receiving ordinary `move_entity_*` packets
/// once this one exists, so without it a minecart snaps to reachable but
/// visibly discrete positions.
fn handle_move_minecart_along_track(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let count = reader.var_i32().map_err(dec_err)?;
    if count < 0 {
        return Err(AdapterError::Decode(format!(
            "negative minecart lerp step count {count}"
        )));
    }
    let mut terminal = None;
    for _ in 0..count {
        let x = reader.f64().map_err(dec_err)?;
        let y = reader.f64().map_err(dec_err)?;
        let z = reader.f64().map_err(dec_err)?;
        let vx = reader.f64().map_err(dec_err)?;
        let vy = reader.f64().map_err(dec_err)?;
        let vz = reader.f64().map_err(dec_err)?;
        let yaw = reader.i8().map_err(dec_err)?;
        let pitch = reader.i8().map_err(dec_err)?;
        let _weight = reader.f32().map_err(dec_err)?;
        terminal = Some((
            Vec3::new(x, y, z),
            Vec3::new(vx, vy, vz),
            unpack_degrees(yaw),
            unpack_degrees(pitch),
        ));
    }
    reader.ensure_empty().map_err(dec_err)?;

    let Some((pos, velocity, yaw, pitch)) = terminal else {
        // An empty step list carries no new pose; nothing to apply.
        return Ok(Vec::new());
    };
    Ok(vec![
        Directive::Emit(ClientEvent::EntityMoved {
            entity_id,
            movement: EntityMovement::Absolute(pos),
            rotation: Some(Rotation::new(yaw, pitch)),
            // MinecartStep carries no on-rail/on-ground bit.
            on_ground: false,
        }),
        Directive::Emit(ClientEvent::EntityVelocity { entity_id, velocity }),
    ])
}

/// Decodes `set_entity_data` into a metadata update event.
///
/// A metadata payload is length-framed, so a misparse is contained to this one
/// packet and cannot corrupt the stream. Rather than fail the whole connection
/// when a rare, unmodelled serializer appears on some exotic entity, a decode
/// error (or any trailing bytes, the misparse detector) is swallowed and no
/// event is emitted — the entity simply keeps its prior metadata. A genuinely
/// missing seam therefore surfaces as *absent fields* in a live test, loudly,
/// rather than as a dropped connection.
///
/// The one case where trailing bytes are *expected* is a stack carrying an
/// unmodeled data component: the item codec cannot skip it, so the metadata
/// decoder abandons the rest of the list and reports `complete == false`.
/// Running the misparse detector there would discard the item identity that was
/// already decoded exactly — fail-closed on the very packet this seam exists to
/// deliver — so the check is skipped and the partial update is emitted. Metadata
/// is applied incrementally, so a partial update is ordinary, not lossy.
fn handle_set_entity_data(
    payload: &[u8],
    variants: &Mutex<HashMap<i32, TrackedEntity>>,
) -> Vec<Directive> {
    let mut reader = Reader::new(payload);
    let Ok(entity_id) = reader.var_i32() else {
        return Vec::new();
    };
    // An id with no entry is an entity we chose not to track, which means it is
    // neither an ambiguous-variant mob nor a `LivingEntity` — so the default's
    // `living: false` is the right answer for it, not a lost fact.
    let tracked = variants
        .lock()
        .ok()
        .and_then(|map| map.get(&entity_id).copied())
        .unwrap_or_default();
    match read_entity_metadata(&mut reader, tracked) {
        // `complete == false` short-circuits the trailing-bytes check: the
        // reader is deliberately parked mid-payload there.
        Ok(decoded)
            if (!decoded.complete || reader.ensure_empty().is_ok())
                && !decoded.metadata.is_empty() =>
        {
            vec![Directive::Emit(ClientEvent::EntityMetadataUpdated {
                entity_id,
                metadata: decoded.metadata,
            })]
        }
        _ => Vec::new(),
    }
}

/// Decodes `update_attributes` into an attributes event, swallowing per-packet
/// decode errors for the same framing reason as [`handle_set_entity_data`].
fn handle_update_attributes(payload: &[u8]) -> Vec<Directive> {
    let mut reader = Reader::new(payload);
    match read_update_attributes(&mut reader) {
        Ok((entity_id, attributes)) if reader.ensure_empty().is_ok() && !attributes.is_empty() => {
            vec![Directive::Emit(ClientEvent::EntityAttributesUpdated {
                entity_id,
                attributes,
            })]
        }
        _ => Vec::new(),
    }
}

