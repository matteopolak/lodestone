//! Player/session packets: movement, respawn, abilities, health/experience,
//! combat, and the player list. Split out of the former monolithic
//! `adapter.rs`.
use super::*;
use super::scoreboard::decode_play;

impl V770Adapter {
    /// Clientbound play-state packets in the player domain, split out of the
    /// former monolithic `handle_play` (see `adapter::mod` for the coordinator).
    pub(super) fn handle_play_player(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == play::clientbound::SET_HEALTH {
            let body: SetHealth = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::HealthChanged {
                health: body.health,
                food: body.food,
                saturation: body.saturation,
            })]);
        }
        if packet_id == play::clientbound::SET_HELD_SLOT {
            let mut reader = Reader::new(payload);
            let slot = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::HeldSlotChanged { slot })]);
        }
        if packet_id == play::clientbound::SET_EXPERIENCE {
            // Field order on the wire is progress (float), level, then total —
            // not alphabetical/declaration order.
            let mut reader = Reader::new(payload);
            let progress = reader.f32().map_err(dec_err)?;
            let level = reader.var_i32().map_err(dec_err)?;
            let total = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ExperienceChanged {
                progress,
                level,
                total,
            })]);
        }
        if packet_id == play::clientbound::COOLDOWN {
            // `Identifier.STREAM_CODEC` is `STRING_UTF8.map(Identifier::parse, ...)`
            // — a single length-prefixed "namespace:path" string, the same shape
            // `parse_key` already expects, not a separate namespace/path pair.
            let mut reader = Reader::new(payload);
            let group = reader.string(32767).map_err(dec_err)?;
            let duration_ticks = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ItemCooldown {
                group: parse_key(&group, "cooldown group")?,
                duration_ticks,
            })]);
        }
        if packet_id == play::clientbound::CHANGE_DIFFICULTY {
            // `Difficulty.STREAM_CODEC` wraps out-of-range ids in vanilla
            // (`ByIdMap.OutOfBoundsStrategy.WRAP`); this adapter instead treats an
            // id outside `0..=3` as an explicit decode error rather than silently
            // aliasing it to a different difficulty.
            let mut reader = Reader::new(payload);
            let difficulty_id = reader.var_i32().map_err(dec_err)?;
            let locked = reader.bool().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let difficulty = match difficulty_id {
                0 => Difficulty::Peaceful,
                1 => Difficulty::Easy,
                2 => Difficulty::Normal,
                3 => Difficulty::Hard,
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown difficulty id {other}"
                    )));
                }
            };
            return Ok(vec![Directive::Emit(ClientEvent::DifficultyChanged {
                difficulty,
                locked,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_COMBAT_KILL {
            let mut reader = Reader::new(payload);
            // VarInt player id, then a network-NBT text component death message.
            reader
                .var_i32()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let component = read_network_nbt(&mut reader)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            return Ok(vec![Directive::Emit(ClientEvent::Death {
                message: Text::from_nbt(&component),
            })]);
        }
        if packet_id == play::clientbound::PLAYER_INFO_UPDATE {
            // Action-bitmask packet: decode the selected per-entry fields and
            // lift them into canonical player-list entries. Zero trailing bytes
            // is the misparse detector, since the field layout is conditional.
            let mut reader = Reader::new(payload);
            let update = PlayerInfoUpdate::decode(&mut reader, CTX)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let entries = update
                .entries
                .into_iter()
                .map(|entry| PlayerListEntry {
                    uuid: entry.uuid,
                    name: entry.name,
                    game_mode: entry.game_mode.and_then(tab_game_mode),
                    latency: entry.latency,
                    display_name: entry.display_name,
                    listed: entry.listed,
                    // Carried through rather than dropped. The v770
                    // `ProfileProperty` and the model's are separate types by the
                    // usual version-seam rule, so this is a lower, not a move.
                    properties: entry.properties.map(|properties| {
                        properties
                            .into_iter()
                            .map(|property| ModelProfileProperty {
                                name: property.name,
                                value: property.value,
                                signature: property.signature,
                            })
                            .collect()
                    }),
                    // Issue #283's real gap: this used to be dropped here —
                    // `entry.chat_session` was decoded (see
                    // `PlayerInfoEntry::chat_session`'s own doc) and had
                    // nowhere to go, so no consumer could ever verify a
                    // signed message from this player. `key_signature` is
                    // deliberately not carried further; see
                    // `lodestone_model::event::ChatSessionInfo`'s doc.
                    chat_session: entry.chat_session.map(|session| {
                        lodestone_model::event::ChatSessionInfo {
                            session_id: session.session_id,
                            public_key: session.public_key,
                            expires_at: session.expires_at,
                        }
                    }),
                })
                .collect();
            return Ok(vec![Directive::Emit(ClientEvent::PlayerListUpdate {
                entries,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_INFO_REMOVE {
            // The zero-trailing check still guards the wire: a misparse of the
            // UUID list would leave bytes that ensure_empty rejects.
            let remove: PlayerInfoRemove = decode_play(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::PlayerListRemove {
                profile_ids: remove.uuids,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_POSITION {
            return handle_player_position(payload);
        }
        if packet_id == play::clientbound::RESPAWN {
            // A dimension change (or post-death respawn) resets the build-height
            // window that frames every subsequent chunk. Decode the spawn info
            // in full — the trailing zero-length check is the misparse detector
            // for the conditional last-death-location field — and record the new
            // dimension so `level_chunk_with_light` stays aligned across the
            // nether/end boundary.
            let mut reader = Reader::new(payload);
            let respawn = Respawn::decode(&mut reader, CTX)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            // Respawn is also how the server reports portal travel, so the
            // dimension type moves here too — and it is the *only* place a
            // Nether trip's `min_y`/`height` change can be picked up.
            let dimension_type = self.enter_dimension(respawn.dimension_type, &respawn.dimension);
            let dimension = respawn.dimension.parse().map_err(|_| {
                AdapterError::Decode(format!("invalid dimension {}", respawn.dimension))
            })?;
            let mode = game_mode(respawn.game_type)?;
            let previous_game_mode = if respawn.previous_game_type < 0 {
                None
            } else {
                Some(game_mode(respawn.previous_game_type as u8)?)
            };
            let last_death_location = respawn
                .last_death_location
                .map(|loc| -> Result<DeathLocation, AdapterError> {
                    let dimension = loc.dimension.parse().map_err(|_| {
                        AdapterError::Decode(format!(
                            "invalid death location dimension {}",
                            loc.dimension
                        ))
                    })?;
                    Ok(DeathLocation {
                        dimension,
                        pos: unpack_block_pos(loc.position),
                    })
                })
                .transpose()?;
            return Ok(vec![
                Directive::Emit(ClientEvent::DimensionTypeChanged {
                    holder_id: respawn.dimension_type,
                    dimension_type,
                }),
                Directive::Emit(ClientEvent::Respawned {
                    dimension,
                    game_mode: mode,
                    previous_game_mode,
                    last_death_location,
                }),
            ]);
        }
        if packet_id == play::clientbound::PLAYER_ABILITIES {
            let abilities: PlayerAbilities = decode_full(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::AbilitiesChanged {
                invulnerable: abilities.flags & ABILITY_FLAG_INVULNERABLE != 0,
                flying: abilities.flags & ABILITY_FLAG_FLYING != 0,
                can_fly: abilities.flags & ABILITY_FLAG_CAN_FLY != 0,
                instabuild: abilities.flags & ABILITY_FLAG_INSTABUILD != 0,
                flying_speed: abilities.flying_speed,
                walking_speed: abilities.walking_speed,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_ROTATION {
            let mut reader = Reader::new(payload);
            let y_rot = reader.f32().map_err(dec_err)?;
            let relative_y = reader.bool().map_err(dec_err)?;
            let x_rot = reader.f32().map_err(dec_err)?;
            let relative_x = reader.bool().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::PlayerRotationSet {
                y_rot,
                relative_y,
                x_rot,
                relative_x,
            })]);
        }
        if packet_id == play::clientbound::SET_CAMERA {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::CameraSet { entity_id })]);
        }
        if packet_id == play::clientbound::OPEN_BOOK {
            // `InteractionHand` ordinal: 0 = main hand, 1 = off hand.
            let mut reader = Reader::new(payload);
            let ordinal = reader.var_i32().map_err(dec_err)?;
            let main_hand = match ordinal {
                0 => true,
                1 => false,
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown interaction hand ordinal {other}"
                    )));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::BookOpened { main_hand })]);
        }
        if packet_id == play::clientbound::TAB_LIST {
            let mut reader = Reader::new(payload);
            let header = read_network_nbt(&mut reader).map_err(dec_err)?;
            let footer = read_network_nbt(&mut reader).map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TabListChanged {
                header: Text::from_nbt(&header),
                footer: Text::from_nbt(&footer),
            })]);
        }
        if packet_id == play::clientbound::PLAYER_COMBAT_ENTER {
            // `ClientboundPlayerCombatEnterPacket` is a singleton with no
            // fields (`StreamCodec.unit`).
            let reader = Reader::new(payload);
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::PlayerCombatEntered)]);
        }
        if packet_id == play::clientbound::PLAYER_COMBAT_END {
            let mut reader = Reader::new(payload);
            let duration_ticks = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::PlayerCombatEnded {
                duration_ticks,
            })]);
        }
        if packet_id == play::clientbound::OPEN_SIGN_EDITOR {
            let mut reader = Reader::new(payload);
            let packed = reader.i64().map_err(dec_err)?;
            let is_front_text = reader.bool().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::SignEditorOpened {
                pos: unpack_block_pos(packed),
                is_front_text,
            })]);
        }
        if packet_id == play::clientbound::SELECT_ADVANCEMENTS_TAB {
            let mut reader = Reader::new(payload);
            let has_tab = reader.bool().map_err(dec_err)?;
            let tab = if has_tab {
                let name = reader.string(32767).map_err(dec_err)?;
                Some(parse_key(&name, "advancement tab")?)
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::AdvancementsTabSelected {
                tab,
            })]);
        }
        if packet_id == play::clientbound::MOUNT_SCREEN_OPEN {
            // Unlike most entity ids on the wire, `entityId` here is a raw
            // 4-byte `int` (`FriendlyByteBuf::readInt`), not a VarInt.
            let mut reader = Reader::new(payload);
            let container_id = reader.var_i32().map_err(dec_err)?;
            let inventory_columns = reader.var_i32().map_err(dec_err)?;
            let entity_id = reader.i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::MountScreenOpened {
                container_id,
                inventory_columns,
                entity_id,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_LOOK_AT {
            let mut reader = Reader::new(payload);
            let from_anchor = read_look_anchor(&mut reader)?;
            let x = reader.f64().map_err(dec_err)?;
            let y = reader.f64().map_err(dec_err)?;
            let z = reader.f64().map_err(dec_err)?;
            let at_entity_flag = reader.bool().map_err(dec_err)?;
            let at_entity = if at_entity_flag {
                let entity_id = reader.var_i32().map_err(dec_err)?;
                let to_anchor = read_look_anchor(&mut reader)?;
                Some(PlayerLookAtEntity {
                    entity_id,
                    to_anchor,
                })
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::PlayerLookAt {
                from_anchor,
                target: Vec3 { x, y, z },
                at_entity,
            })]);
        }
        Ok(Vec::new())
    }
}

/// Reads an `EntityAnchorArgument.Anchor` ordinal (a VarInt): `0` = feet,
/// `1` = eyes. Used by `ClientboundPlayerLookAtPacket`.
fn read_look_anchor(reader: &mut Reader<'_>) -> Result<LookAnchor, AdapterError> {
    match reader.var_i32().map_err(dec_err)? {
        0 => Ok(LookAnchor::Feet),
        1 => Ok(LookAnchor::Eyes),
        other => Err(AdapterError::Decode(format!(
            "invalid entity anchor ordinal {other}"
        ))),
    }
}

/// Lowers a `Relative` bit set (see `net.minecraft.world.entity.Relative`) to
/// the canonical [`TeleportFlags`]. Bits: X=0, Y=1, Z=2, Y_ROT=3, X_ROT=4.
fn teleport_flags(value: i32) -> TeleportFlags {
    TeleportFlags {
        relative_x: value & (1 << 0) != 0,
        relative_y: value & (1 << 1) != 0,
        relative_z: value & (1 << 2) != 0,
        relative_yaw: value & (1 << 3) != 0,
        relative_pitch: value & (1 << 4) != 0,
    }
}

/// Decodes `player_position` and returns the teleport-accept confirmation plus
/// the canonical teleport event.
///
/// Wire layout (`ClientboundPlayerPositionPacket`): VarInt teleport id, a
/// `PositionMoveRotation` (position `f64`×3, delta-movement `f64`×3, yaw `f32`,
/// pitch `f32`), then a big-endian `i32` `Relative` bit set. The delta-movement
/// is consumed for alignment but not surfaced here — player velocity is owned by
/// the physics layer, which applies it from the same packet. Zero trailing
/// bytes is the misparse detector.
fn handle_player_position(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let id = reader.var_i32().map_err(dec_err)?;
    let x = reader.f64().map_err(dec_err)?;
    let y = reader.f64().map_err(dec_err)?;
    let z = reader.f64().map_err(dec_err)?;
    let _dx = reader.f64().map_err(dec_err)?;
    let _dy = reader.f64().map_err(dec_err)?;
    let _dz = reader.f64().map_err(dec_err)?;
    let yaw = reader.f32().map_err(dec_err)?;
    let pitch = reader.f32().map_err(dec_err)?;
    let relatives = reader.i32().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;

    // This echo is unconditional and per-packet — there is no pending/latched
    // teleport-id state to get stuck here (see this crate's own doc on
    // `encode_teleport` for why the server side tracks no id either). If the
    // real server keeps rejecting movement after a transfer/reconfigure, this
    // line having fired with the same `id` the server just sent rules the
    // client's half of teleport confirmation out — look at whether the write
    // actually reached the wire (a `Directive::Send` failure stops the
    // session; see `Driver::execute`) or at the server's own bookkeeping.
    tracing::info!(
        target: "net",
        id,
        x, y, z,
        yaw, pitch,
        relatives,
        "PLAYER_POSITION received; echoing ACCEPT_TELEPORTATION with the same id"
    );

    Ok(vec![
        send(
            play::serverbound::ACCEPT_TELEPORTATION,
            &AcceptTeleportation { id },
        )?,
        Directive::Emit(ClientEvent::TeleportPlayer {
            pos: Vec3::new(x, y, z),
            rotation: Rotation::new(yaw, pitch),
            flags: teleport_flags(relatives),
        }),
    ])
}

/// Maps a numeric game-type byte to the canonical [`GameMode`].
pub(super) fn game_mode(value: u8) -> Result<GameMode, AdapterError> {
    match value {
        0 => Ok(GameMode::Survival),
        1 => Ok(GameMode::Creative),
        2 => Ok(GameMode::Adventure),
        3 => Ok(GameMode::Spectator),
        other => Err(AdapterError::Decode(format!("unknown game type {other}"))),
    }
}

/// Maps a tab-list game-mode id to the canonical [`GameMode`], returning `None`
/// for anything outside the four known modes (including the `-1` "no game mode"
/// sentinel a tab-list refresh may carry) rather than failing the whole packet.
fn tab_game_mode(id: i32) -> Option<GameMode> {
    match id {
        0 => Some(GameMode::Survival),
        1 => Some(GameMode::Creative),
        2 => Some(GameMode::Adventure),
        3 => Some(GameMode::Spectator),
        _ => None,
    }
}

