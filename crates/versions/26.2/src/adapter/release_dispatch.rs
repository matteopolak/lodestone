use super::{AdapterError, ConnectionState, Directive, V770Adapter, decode_full, entity};
use crate::packets::release_layout::{AddTransientBlock, PostEffects};
use lodestone_data::GameDataVersion;

impl V770Adapter {
    pub(super) fn handle_release_packet(
        &self,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Option<Vec<Directive>>, AdapterError> {
        if self.dialect.game_data_version() != GameDataVersion::V26_3 {
            return Ok(None);
        }
        match (state, self.dialect.clientbound_name(state, packet_id)) {
            (ConnectionState::Play, Some("minecraft:swing_animation")) => {
                entity::handle_swing_animation(payload).map(Some)
            }
            (ConnectionState::Configuration | ConnectionState::Play, Some("minecraft:post_effects")) => {
                // An empty replacement is the default unfiltered screen, which is
                // exactly what the renderer draws. A non-empty one names
                // resource-pack shader chains the renderer has no pipeline for;
                // dropping it costs a screen tint, failing the session costs the game.
                let body: PostEffects = decode_full(payload)?;
                if !body.effects.is_empty() {
                    tracing::warn!(effects = ?body.effects, "screen post effects are not rendered");
                }
                Ok(Some(Vec::new()))
            }
            (ConnectionState::Play, Some("minecraft:add_transient_block")) => {
                let body: AddTransientBlock = decode_full(payload)?;
                let state = self.dialect.game_data_version().state_from_wire(body.state.raw())
                    .ok_or_else(|| AdapterError::Decode(format!(
                        "invalid transient block wire state {}", body.state.raw(),
                    )))?;
                // The server sends this right after the block update that already placed
                // a landed falling block. It asks the renderer to draw that block for a
                // second to hide the section re-mesh latency; the terrain is already
                // correct, so ignoring it costs one frame of pop-in, not a wrong world.
                let _ = state;
                Ok(Some(Vec::new()))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::{PacketTables, ProtocolDialect, StatePackets, V26_2_PACKET_TABLES};
    use crate::packet_ids::{configuration, play};
    use lodestone_model::{ClientEvent, Hand, ItemAnimationKind, VersionAdapter};
    use lodestone_world::World;

    fn selected(swing_id: i32) -> V770Adapter {
        let play = match swing_id {
            123 => StatePackets {
                clientbound: &[
                    ("minecraft:add_transient_block", 37),
                    ("minecraft:post_effects", 83),
                    ("minecraft:swing_animation", 123),
                ],
                serverbound: &[],
            },
            271 => StatePackets {
                clientbound: &[("minecraft:swing_animation", 271)],
                serverbound: &[],
            },
            _ => panic!("unexpected fixture swing ID"),
        };
        let tables = PacketTables {
            configuration: StatePackets {
                clientbound: &[("minecraft:post_effects", 10)],
                serverbound: &[],
            },
            play,
            ..V26_2_PACKET_TABLES
        };
        V770Adapter::with_connection_dialect(
            ProtocolDialect::connection_only(777, &["26.3"], tables).unwrap()
                .with_game_data_version(GameDataVersion::V26_3),
        )
    }

    fn swing_event(directives: &[Directive]) {
        assert!(matches!(directives, [Directive::Emit(ClientEvent::EntitySwingAnimation {
            entity_id: 291,
            hand: Hand::Off,
            kind: ItemAnimationKind::Stab,
            duration_ticks: 129,
        })]));
    }

    #[test]
    fn selected_names_dispatch_swing_before_legacy_identifier_translation() {
        let bytes = [0xa3, 0x02, 0x01, 0x02, 0x81, 0x01];
        assert_eq!(play::clientbound::TAG_QUERY, 123);
        let adapter = selected(123);
        let directives = adapter.handle_release_packet(ConnectionState::Play, 123, &bytes)
            .unwrap().unwrap();
        swing_event(&directives);
        let moved = selected(271);
        let directives = moved.handle_release_packet(ConnectionState::Play, 271, &bytes)
            .unwrap().unwrap();
        swing_event(&directives);
        assert!(moved.handle_release_packet(ConnectionState::Play, 123, &bytes).unwrap().is_none());
        let base = V770Adapter::new();
        assert!(base.handle_release_packet(ConnectionState::Play, 123, &bytes).unwrap().is_none());
        let base_names = V770Adapter::with_connection_dialect(
            ProtocolDialect::v26_2().with_game_data_version(GameDataVersion::V26_3),
        );
        assert!(base_names.handle_release_packet(ConnectionState::Play, 123, &bytes).unwrap().is_none());
    }

    #[test]
    fn release_swing_dispatch_rejects_incomplete_and_extended_bodies() {
        let adapter = selected(123);
        for bytes in [&[0xa3, 0x02, 0x01, 0x02, 0x81][..], &[0xa3, 0x02, 0x01, 0x02, 0x81, 0x01, 0][..]] {
            assert!(matches!(adapter.handle_release_packet(ConnectionState::Play, 123, bytes),
                Err(AdapterError::Decode(_))));
        }
    }

    #[test]
    fn public_play_admission_remains_closed_before_release_dispatch() {
        let adapter = selected(123);
        let mut world = World::new();
        assert!(matches!(adapter.handle_packet(&mut world, ConnectionState::Play, 123,
            &[0xa3, 0x02, 0x01, 0x02, 0x81, 0x01]),
            Err(AdapterError::Unsupported(message)) if message.contains("no reviewed Play")));
    }

    #[test]
    fn post_effects_use_the_selected_name_and_are_accepted_without_a_pipeline() {
        let adapter = selected(123);
        let mut world = World::new();
        assert_eq!(configuration::clientbound::STORE_COOKIE, 10);
        assert_eq!(play::clientbound::ROTATE_HEAD, 83);
        let bytes = b"\x02\x10minecraft:spider\x11minecraft:creeper";
        assert!(adapter.handle_packet(&mut world, ConnectionState::Configuration, 10, bytes)
            .unwrap().is_empty());
        assert!(adapter.handle_release_packet(ConnectionState::Play, 83, bytes).unwrap().unwrap().is_empty());
        assert!(adapter.handle_packet(&mut world, ConnectionState::Configuration, 10, &[0])
            .unwrap().is_empty());
        assert!(matches!(adapter.handle_packet(&mut world, ConnectionState::Configuration, 10, &[0, 0]),
            Err(AdapterError::Decode(_))));
        assert!(matches!(adapter.handle_packet(&mut world, ConnectionState::Configuration, 10, &bytes[..bytes.len() - 1]),
            Err(AdapterError::Decode(_))));
    }

    #[test]
    fn transient_block_validates_its_wire_state_and_leaves_terrain_alone() {
        let adapter = selected(123);
        assert_eq!(play::clientbound::FORGET_LEVEL_CHUNK, 37);
        let bytes = [0xff, 0xff, 0xfa, 0x40, 0, 0x07, 0x5f, 0xd1, 0xe2, 0x61];
        assert_eq!(GameDataVersion::V26_3.state_from_wire(12514).unwrap().raw(), 10771);
        assert!(adapter.handle_release_packet(ConnectionState::Play, 37, &bytes).unwrap().unwrap().is_empty());
        let invalid = [0xff, 0xff, 0xfa, 0x40, 0, 0x07, 0x5f, 0xd1, 0xff, 0xff, 0xff, 0xff, 0x07];
        assert!(matches!(adapter.handle_release_packet(ConnectionState::Play, 37, &invalid),
            Err(AdapterError::Decode(message)) if message.contains("wire state 2147483647")));
        assert!(matches!(adapter.handle_release_packet(ConnectionState::Play, 37, &bytes[..bytes.len() - 1]),
            Err(AdapterError::Decode(_))));
        let mut extended = bytes.to_vec();
        extended.push(0);
        assert!(matches!(adapter.handle_release_packet(ConnectionState::Play, 37, &extended),
            Err(AdapterError::Decode(_))));
    }

    #[test]
    fn player_action_ordinals_shift_past_the_inserted_destroy_direction() {
        let release = selected(123);
        let base = V770Adapter::new();
        // Start-destroy (0) is unchanged; abort (1) and every later action move up one.
        let got: Vec<i32> = (0..4).map(|n| release.player_action_ordinal(n)).collect();
        assert_eq!(got, [0, 2, 3, 4]);
        let same: Vec<i32> = (0..4).map(|n| base.player_action_ordinal(n)).collect();
        assert_eq!(same, [0, 1, 2, 3]);
    }
}
