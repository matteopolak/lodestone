use super::*;

/// Server implementation for protocol 498 (Minecraft 1.14.4).
#[derive(Clone, Copy, Debug, Default)]
pub struct V498ServerProtocol;

impl V498ServerProtocol {
    /// Converts one canonical state into the protocol-498 block-update packet.
    pub fn try_encode_block_update(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: &str,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        let canonical = lodestone_data::block_states::state_id(state)
            .ok_or_else(|| ChunkEncodeError::new(format!("unknown canonical block state {state}")))?;
        let wire = wire_state_498(canonical)?;
        let mut payload = Writer::default();
        payload.i64(pack_position(BlockPos::new(x, y, z)));
        payload.var_i32(i32::try_from(wire).expect("protocol-498 state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: play_498::clientbound::BLOCK_CHANGE,
            payload: payload.into_vec(),
        })
    }
}

impl ServerProtocol for V498ServerProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == handshaking_498::serverbound::SET_PROTOCOL => {
                let Some(handshake) = decode_full_498::<SetProtocol>(payload) else {
                    return ServerBound::Ignored;
                };
                if handshake.protocol_version != PROTOCOL_1_14_4 {
                    return ServerBound::Ignored;
                }
                let next_state = if handshake.next_state == 2 { State::Login } else { State::Status };
                ServerBound::Handshake { next_state }
            }
            State::Login if packet_id == login_498::serverbound::LOGIN_START => {
                decode_full_498::<LoginStart>(payload).map_or(ServerBound::Ignored, |start| {
                    ServerBound::LoginStart { username: start.username, uuid: Uuid::nil() }
                })
            }
            State::Play if packet_id == play_498::serverbound::CLIENT_COMMAND => {
                decode_full_498::<ClientCommand>(payload).map_or(ServerBound::Ignored, |command| {
                    ServerBound::ClientCommand { action: command.action }
                })
            }
            State::Play if packet_id == play_498::serverbound::BLOCK_DIG => {
                let Some(BlockDig { status, location: Position(pos), face }) =
                    decode_full_498(payload)
                else {
                    return ServerBound::Ignored;
                };
                let (Some(action), Some(face)) = (block_action(status), block_face(face)) else {
                    return ServerBound::Ignored;
                };
                ServerBound::BlockAction { action, pos, face, sequence: 0 }
            }
            State::Play if packet_id == play_498::serverbound::BLOCK_PLACE => {
                decode_full_498::<BlockPlace>(payload).map_or(ServerBound::Ignored, block_use)
            }
            State::Play if packet_id == play_498::serverbound::USE_ENTITY => {
                entity_use(payload, false)
            }
            State::Play if packet_id == play_498::serverbound::ARM_ANIMATION => {
                let Some(hand) = decode_full_498::<ServerboundArmAnimation>(payload)
                    .and_then(|packet| lodestone_model::Hand::from_wire_ordinal(packet.hand))
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::Swing { hand }
            }
            State::Play if packet_id == play_498::serverbound::HELD_ITEM_SLOT => {
                let Some(slot) = decode_full_498::<ServerboundHeldItemSlot>(payload)
                    .and_then(|packet| u8::try_from(packet.slot).ok())
                    .filter(|&slot| slot < 9)
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::CarriedItemChanged { slot }
            }
            State::Play if packet_id == play_498::serverbound::WINDOW_CLICK => {
                let Some(WindowClick { window_id, slot, button, mode, .. }) =
                    decode_full_498(payload)
                else {
                    return ServerBound::Ignored;
                };
                if !(0..=6).contains(&mode) {
                    return ServerBound::Ignored;
                }
                ServerBound::ContainerClicked {
                    window_id: i32::from(window_id),
                    state_id: 0,
                    slot: i32::from(slot),
                    button,
                    click_type: i32::from(mode),
                    changed_slots: Vec::new(),
                    carried_item: None,
                }
            }
            State::Play if packet_id == play_498::serverbound::CLOSE_WINDOW => {
                decode_full_498::<ServerboundCloseWindow>(payload).map_or(
                    ServerBound::Ignored,
                    |close| ServerBound::ContainerClosed {
                        window_id: i32::from(close.window_id),
                    },
                )
            }
            State::Play if packet_id == play_498::serverbound::KEEP_ALIVE => {
                decode_full_498::<KeepAliveResponse>(payload).map_or(ServerBound::Ignored, |response| {
                    ServerBound::KeepAlive { id: response.id }
                })
            }
            State::Play if packet_id == play_498::serverbound::POSITION => {
                decode_full_498::<ServerboundPosition>(payload).map_or(
                    ServerBound::Ignored,
                    |move_| ServerBound::PlayerMoved {
                        x: move_.x,
                        y: move_.y,
                        z: move_.z,
                        rotation: None,
                        on_ground: move_.on_ground,
                    },
                )
            }
            State::Play if packet_id == play_498::serverbound::POSITION_LOOK => {
                decode_full_498::<ServerboundPositionLook>(payload).map_or(
                    ServerBound::Ignored,
                    |move_| ServerBound::PlayerMoved {
                        x: move_.x,
                        y: move_.y,
                        z: move_.z,
                        rotation: Some(Rotation {
                            yaw: move_.yaw,
                            pitch: move_.pitch,
                        }),
                        on_ground: move_.on_ground,
                    },
                )
            }
            State::Play if packet_id == play_498::serverbound::LOOK => {
                decode_full_498::<ServerboundLook>(payload).map_or(
                    ServerBound::Ignored,
                    |look| ServerBound::PlayerRotated {
                        yaw: look.yaw,
                        pitch: look.pitch,
                        on_ground: look.on_ground,
                    },
                )
            }
            State::Play if packet_id == play_498::serverbound::FLYING => {
                decode_full_498::<ServerboundFlying>(payload).map_or(
                    ServerBound::Ignored,
                    |status| ServerBound::PlayerStatusOnly {
                        on_ground: status.on_ground,
                    },
                )
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        vec![
            send_498(login_498::clientbound::COMPRESS, &SetCompression { threshold: COMPRESSION_THRESHOLD }),
            ServerDirective::SetCompression(COMPRESSION_THRESHOLD),
            send_498(login_498::clientbound::SUCCESS, &LoginSuccessString {
                uuid: uuid.to_string(),
                username: username.to_owned(),
            }),
        ]
    }

    fn has_configuration_phase(&self) -> bool { false }

    fn begin_configuration(&self) -> Vec<ServerDirective> { Vec::new() }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        vec![
            send_498(play_498::clientbound::LOGIN, &JoinGameLegacy {
                entity_id: 1,
                game_mode: 0,
                dimension: 0,
                hashed_seed: 0,
                max_players: 20,
                level_type: "default".to_owned(),
                view_distance: view_radius,
                reduced_debug_info: false,
                enable_respawn_screen: true,
            }),
            send_498(play_498::clientbound::POSITION, &ClientboundPositionLook {
                x: 8.0,
                y: 100.0,
                z: 8.0,
                yaw: 0.0,
                pitch: 0.0,
                flags: 0,
                teleport_id: 0,
            }),
        ]
    }

    fn begin_chunk_batch(&self) -> ServerDirective { ServerDirective::None }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-498 column")
    }

    fn try_encode_chunk(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        Ok(ServerDirective::Send {
            packet_id: play_498::clientbound::MAP_CHUNK,
            payload: encode_chunk_body_498(cx, cz, column)?,
        })
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective { ServerDirective::None }

    fn encode_open_screen(&self, window_id: i32, menu: &str, title: &str) -> ServerDirective {
        let Some(inventory_type) = menu_id(menu) else {
            return ServerDirective::None;
        };
        send_498(
            play_498::clientbound::OPEN_WINDOW,
            &crate::packets::window::OpenWindow {
                window_id,
                inventory_type,
                window_title: format!("{{\"text\":\"{}\"}}", json_string(title)),
            },
        )
    }

    fn encode_container_content(
        &self,
        window_id: i32,
        _state_id: i32,
        items: &[Option<ItemStack>],
        _carried: Option<&ItemStack>,
    ) -> ServerDirective {
        let Ok(window_id) = u8::try_from(window_id) else {
            return ServerDirective::None;
        };
        send_498(
            play_498::clientbound::WINDOW_ITEMS,
            &crate::packets::window::WindowItems {
                window_id,
                items: legacy_slots(PROTOCOL_1_14_4, items),
            },
        )
    }

    fn encode_container_slot(
        &self,
        window_id: i32,
        _state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        let (Ok(window_id), Ok(slot)) = (i8::try_from(window_id), i16::try_from(slot)) else {
            return ServerDirective::None;
        };
        send_498(
            play_498::clientbound::SET_SLOT,
            &crate::packets::window::SetSlot {
                window_id,
                slot,
                item: legacy_slot(PROTOCOL_1_14_4, item),
            },
        )
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-498 state")
    }

    fn encode_animate(&self, entity_id: i32, action: u8) -> ServerDirective {
        encode_animate(play_498::clientbound::ANIMATION, entity_id, action)
    }

    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        send_498(play_498::clientbound::KEEP_ALIVE, &KeepAliveRequest { id })
    }

    fn encode_disconnect(&self, state: State, reason: &Text) -> ServerDirective {
        if state != State::Play {
            return ServerDirective::None;
        }
        send_498(play_498::clientbound::KICK_DISCONNECT, &KickDisconnect {
            reason: format!("{{\"text\":\"{}\"}}", json_string(&reason.to_plain_string())),
        })
    }
}
