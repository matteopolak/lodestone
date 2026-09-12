//! Server protocol wire tests.
//!
//! This private module is part of the V770ServerProtocol facade. Its
//! re-exported helpers preserve the existing public API and wire behaviour.

use super::*;

#[cfg(test)]
mod block_edit_tests {
    use super::*;
    use lodestone_core::State;

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    /// `PLAYER_ACTION` ordinal `0` (`START_DESTROY_BLOCK`) round-trips
    /// through the real derived `Encode`/decode into
    /// `ServerBound::BlockAction`, with `pos`/`face` unpacked correctly —
    /// pinning both [`unpack_block_pos`] against [`pack_block_pos`] and
    /// [`face_from_ordinal`] against a non-trivial (non-zero) face.
    #[test]
    fn decode_player_action_start_destroy() {
        let proto = V770ServerProtocol;
        let body = encode(&PlayerAction {
            action: 0,
            pos: pack_block_pos(1, 2, 3),
            direction: 1, // Up
            sequence: 42,
        });
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body);
        assert_eq!(
            decoded,
            ServerBound::BlockAction {
                action: BlockActionKind::StartDestroy,
                pos: BlockPos::new(1, 2, 3),
                face: BlockFace::Up,
                sequence: 42,
            }
        );
    }

    /// The other two destroy ordinals decode to their matching
    /// `BlockActionKind`, proving the ordinal mapping is not just
    /// coincidentally right for `0`.
    #[test]
    fn decode_player_action_abort_and_stop() {
        let proto = V770ServerProtocol;
        for (ordinal, expected) in [
            (1, BlockActionKind::AbortDestroy),
            (2, BlockActionKind::StopDestroy),
        ] {
            let body = encode(&PlayerAction {
                action: ordinal,
                pos: pack_block_pos(0, 0, 0),
                direction: 0,
                sequence: 0,
            });
            let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body);
            assert_eq!(
                decoded,
                ServerBound::BlockAction {
                    action: expected,
                    pos: BlockPos::new(0, 0, 0),
                    face: BlockFace::Down,
                    sequence: 0,
                },
                "ordinal {ordinal}"
            );
        }
    }

    /// The item-action ordinals share the wire packet with the three destroy
    /// phases and must not fall into one of them.
    ///
    /// **This test used to require `3..=7` to be `Ignored`, and that made it a
    /// gate asserting a bug.** Its stated premise — *"this crate has no inventory
    /// model to act on them"* — was true when written and had stopped being:
    /// `lodestone-server` owns `PlayerInventory` and already spawns item entities
    /// for block drops. Meanwhile the *client* half was complete (a keybind, four
    /// adapters encoding ordinals 3 and 4), so `Q` did nothing whatsoever and this
    /// test required that it keep doing nothing. The premise had to be re-checked
    /// rather than the assertion trusted; see `DESIGN.md` §12.150.
    ///
    /// `7` (STAB) is still genuinely unmodelled, and keeping it in its own
    /// assertion is what stops the two drop arms from having been written as a
    /// `3..=7` catch-all.
    #[test]
    fn decode_player_action_drop_ordinals_lift_and_the_rest_are_ignored() {
        let proto = V770ServerProtocol;
        let body = |ordinal: i32| {
            encode(&PlayerAction {
                action: ordinal,
                pos: 0,
                direction: 0,
                sequence: 0,
            })
        };
        // 3 is DROP_ALL_ITEMS and 4 is DROP_ITEM, per the jar's own enum order —
        // backwards from the keys, where `Q` is one item and `Ctrl+Q` is the stack.
        for (ordinal, whole_stack) in [(3, true), (4, false)] {
            let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body(ordinal));
            assert_eq!(
                decoded,
                ServerBound::ItemDropped { whole_stack },
                "ordinal {ordinal}"
            );
        }
        // 5 is RELEASE_USE_ITEM, and it lifts now: it is the packet that fires a
        // drawn bow, so leaving it `Ignored` meant a player could draw and never
        // shoot. This assertion used to be part of the `5..=7` sweep below, which
        // is the shape CLAUDE.md warns about — a new subsystem silently breaking a
        // test that asserted its absence.
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body(5)),
            ServerBound::ReleaseUseItem,
        );
        // 6 is SWAP_ITEM_WITH_OFFHAND, and it lifts now — see
        // `ServerBound::SwapItemInHand`'s own doc comment for the consumer.
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body(6)),
            ServerBound::SwapItemInHand,
        );
        // 7 is STAB: still no server-side model.
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body(7));
        assert_eq!(decoded, ServerBound::Ignored, "ordinal 7");
        // Past the enum: still ignored, so the arm above is a specific lift rather
        // than a catch-all that swallowed the tail.
        for ordinal in [8, 99, -1] {
            let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body(ordinal));
            assert_eq!(decoded, ServerBound::Ignored, "ordinal {ordinal}");
        }
    }

    /// `USE_ITEM` lifts with its hand and the facing it carries — the launch
    /// direction for every player-thrown projectile.
    ///
    /// The yaw/pitch are what make a throw aimable without this crate tracking
    /// rotation per connection, so a decode that dropped them would leave every
    /// snowball flying due south. Asserted by value, and with a non-zero,
    /// non-symmetric pair so a transposed yaw/pitch read is caught too.
    #[test]
    fn decode_use_item_lifts_hand_and_facing() {
        let proto = V770ServerProtocol;
        let body = encode(&UseItem {
            hand: 1,
            sequence: 7,
            yaw: 137.5,
            pitch: -22.25,
        });
        assert_eq!(
            proto.decode(State::Play, play::serverbound::USE_ITEM, &body),
            ServerBound::UseItem {
                hand: 1,
                yaw: 137.5,
                pitch: -22.25,
            },
        );
        // A hand ordinal outside `0..=1` is carried through rather than dropping
        // the packet; the *consumer* treats anything but `1` as the main hand
        // (`apply_use_item`'s own branch), which is where the degradation belongs
        // because only it knows what a hand means. A **negative** ordinal cannot
        // survive the `u8` conversion and lands on `0`, which is the main hand too —
        // asserted so the two malformed shapes are known to agree.
        for wire in [42, -1] {
            let odd = encode(&UseItem {
                hand: wire,
                sequence: 0,
                yaw: 0.0,
                pitch: 0.0,
            });
            let ServerBound::UseItem { hand, .. } =
                proto.decode(State::Play, play::serverbound::USE_ITEM, &odd)
            else {
                panic!("a malformed hand must not drop the packet");
            };
            assert_ne!(hand, 1, "wire {wire} must not read as the off hand");
        }
    }

    /// The two packets a game-mode change writes, byte-exact. The flags byte is
    /// the whole reason creative flight works or does not: `0x0D` is
    /// `invulnerable | can_fly | instabuild`, and `flying` is deliberately
    /// **not** set for creative (vanilla's own game-type enum's own update player abilities sets it only
    /// for spectator). A fully-connected wire carrying the wrong byte here looks
    /// identical to a correct one from every coverage angle.
    #[test]
    fn encode_creative_writes_game_event_3_and_abilities_flags() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { packet_id, payload } =
            proto.encode_game_mode(GameMode::Creative)
        else {
            panic!("game-mode change must be a Send");
        };
        assert_eq!(packet_id, play::clientbound::GAME_EVENT);
        // `u8` event code then a big-endian `f32` parameter: code 3, param 1.0.
        assert_eq!(payload, vec![3, 0x3F, 0x80, 0x00, 0x00]);

        let ServerDirective::Send { packet_id, payload } =
            proto.encode_player_abilities(Abilities::for_mode(GameMode::Creative))
        else {
            panic!("abilities must be a Send");
        };
        assert_eq!(packet_id, play::clientbound::PLAYER_ABILITIES);
        assert_eq!(payload[0], 0x0D, "invulnerable | can_fly | instabuild");

        // Survival is the negative arm: same two packets, no flags at all.
        let ServerDirective::Send { payload, .. } =
            proto.encode_player_abilities(Abilities::for_mode(GameMode::Survival))
        else {
            panic!("abilities must be a Send");
        };
        assert_eq!(payload[0], 0x00);

        // Spectator is the one mode that ships `flying` already set.
        let ServerDirective::Send { payload, .. } =
            proto.encode_player_abilities(Abilities::for_mode(GameMode::Spectator))
        else {
            panic!("abilities must be a Send");
        };
        assert_eq!(payload[0], 0x07, "invulnerable | flying | can_fly");
    }

    /// The F4 switcher round-trips into a real `ServerBound` variant rather than
    /// the `Ignored` it used to decode to, and an out-of-range id is dropped.
    #[test]
    fn decode_change_game_mode() {
        let proto = V770ServerProtocol;
        for (id, mode) in [
            (0, GameMode::Survival),
            (1, GameMode::Creative),
            (2, GameMode::Adventure),
            (3, GameMode::Spectator),
        ] {
            let body = encode(&ChangeGameMode { mode: id });
            assert_eq!(
                proto.decode(State::Play, play::serverbound::CHANGE_GAME_MODE, &body),
                ServerBound::ChangeGameMode { mode }
            );
        }
        let body = encode(&ChangeGameMode { mode: 9 });
        assert_eq!(
            proto.decode(State::Play, play::serverbound::CHANGE_GAME_MODE, &body),
            ServerBound::Ignored
        );
    }

    /// `USE_ITEM_ON` round-trips into `ServerBound::UseItemOn`, including a
    /// negative Y (below `y = 0`) to pin `unpack_block_pos`'s sign extension.
    #[test]
    fn decode_use_item_on() {
        let proto = V770ServerProtocol;
        let body = encode(&UseItemOn {
            hand: 0,
            pos: pack_block_pos(5, -10, -7),
            face: 3, // South
            cursor_x: 0.5,
            cursor_y: 1.0,
            cursor_z: 0.5,
            inside_block: false,
            world_border_hit: false,
            sequence: 7,
        });
        let decoded = proto.decode(State::Play, play::serverbound::USE_ITEM_ON, &body);
        assert_eq!(
            decoded,
            ServerBound::UseItemOn {
                pos: BlockPos::new(5, -10, -7),
                face: BlockFace::South,
                cursor: Vec3f {
                    x: 0.5,
                    y: 1.0,
                    z: 0.5,
                },
                sequence: 7,
                hand: 0,
            }
        );
    }

    /// The off-hand ordinal (`1`) must survive decode — this is the field that
    /// used to be read off the wire and then discarded, which is why off-hand
    /// placement was impossible: every `UseItemOn` reached the server-side
    /// model reporting main hand regardless of which hand the client used.
    #[test]
    fn decode_use_item_on_carries_the_off_hand_ordinal() {
        let proto = V770ServerProtocol;
        let body = encode(&UseItemOn {
            hand: 1,
            pos: pack_block_pos(1, 2, 3),
            face: 0,
            cursor_x: 0.25,
            cursor_y: 0.0,
            cursor_z: 0.75,
            inside_block: false,
            world_border_hit: false,
            sequence: 11,
        });
        let decoded = proto.decode(State::Play, play::serverbound::USE_ITEM_ON, &body);
        let ServerBound::UseItemOn { hand, .. } = decoded else {
            panic!("expected UseItemOn, got {decoded:?}");
        };
        assert_eq!(hand, 1);
    }

    #[test]
    fn decode_use_item_on_rejects_truncated_payload() {
        let proto = V770ServerProtocol;
        // A truncated packet is invalid input, not sequence zero. The decoder
        // must reject it before it reaches the server consumer.
        assert_eq!(
            proto.decode(State::Play, play::serverbound::USE_ITEM_ON, &[0x00]),
            ServerBound::Ignored
        );
    }

    /// Same malformed-input convention as `USE_ITEM`'s own hand field: `hand`
    /// is decoded with `u8::try_from(..).unwrap_or(0)`, which degrades to main
    /// hand rather than dropping the packet — but only for a wire value
    /// outside `u8`'s own range (`256` and up, or negative). `300` rather
    /// than, say, `99`: the latter fits in a `u8` and survives the conversion
    /// unclamped, which is worth recording rather than assuming — this
    /// decoder does not validate the ordinal is `0` or `1`, only that it fits
    /// in a byte, matching `USE_ITEM`'s own established (if narrower than it
    /// sounds) convention.
    #[test]
    fn decode_use_item_on_clamps_a_malformed_hand_ordinal_to_main() {
        let proto = V770ServerProtocol;
        let body = encode(&UseItemOn {
            hand: 300,
            pos: pack_block_pos(0, 0, 0),
            face: 0,
            cursor_x: 0.0,
            cursor_y: 0.0,
            cursor_z: 0.0,
            inside_block: false,
            world_border_hit: false,
            sequence: 0,
        });
        let decoded = proto.decode(State::Play, play::serverbound::USE_ITEM_ON, &body);
        let ServerBound::UseItemOn { hand, .. } = decoded else {
            panic!("expected UseItemOn, got {decoded:?}");
        };
        assert_eq!(hand, 0);
    }

    /// [`resolve_state_id`] round-trips the two propertyless states this
    /// crate ever writes back to themselves via [`block_name`].
    #[test]
    fn resolve_state_id_round_trips_stone_and_air() {
        assert_eq!(
            block_name(resolve_state_id("minecraft:stone")),
            Some("minecraft:stone")
        );
        assert_eq!(
            block_name(resolve_state_id("minecraft:air")),
            Some("minecraft:air")
        );
    }

    /// [`resolve_state_id`] must match on properties too, not just the block
    /// name — otherwise every propertied state of a block would resolve to
    /// whichever one happens to be first in the table. Picks a real
    /// propertied entry straight from the generated table rather than
    /// guessing a property string, so this cannot pass by coincidence.
    #[test]
    fn resolve_state_id_matches_properties_not_just_name() {
        let propertied_id = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| !properties(id).unwrap().is_empty())
            .expect("generated table has at least one propertied state");
        let name = block_name(propertied_id).unwrap();
        let props = properties(propertied_id).unwrap();
        let state_str = format!(
            "{name}[{}]",
            props
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(resolve_state_id(&state_str), propertied_id);
    }

    /// [`resolve_state_id`] falls back to air rather than panicking on a
    /// string the generated table has no match for.
    #[test]
    fn resolve_state_id_falls_back_to_air_on_no_match() {
        assert_eq!(resolve_state_id("minecraft:not_a_real_block"), air_id());
    }

    /// The regression this fix exists for: `lodestone-worldgen`'s
    /// `OverworldGenerator` writes its default fluid as the **bare** literal
    /// `"minecraft:water"`, with no `level` property
    /// (`crates/lodestone-worldgen/src/overworld.rs`'s `default_fluid`) — and
    /// real water has no propertyless state (every id in `86..=101` carries
    /// `level=0..15`). Before `resolve_state_id`'s same-name-default
    /// fallback tier existed, this fell all the way through to **air** —
    /// found by this crate's own hermetic
    /// `encode_chunk_carries_real_block_states_including_a_fluid` gate below,
    /// which failed its `assert_ne!(water_id, air_id())` sanity check the
    /// first time it ran, exactly the trap a
    /// solids-only fix falls into. Pins the exact expected id
    /// (`blocks.json`'s own `"default": true` entry for `minecraft:water` is
    /// `level=0`, id `86` — `.cache/mc/26.2/generated/reports/blocks.json`),
    /// not just "not air", so a future table regeneration that changes which
    /// state is default cannot silently regress this to a *different* wrong
    /// answer.
    #[test]
    fn resolve_state_id_resolves_bare_water_to_its_default_level_state() {
        let bare_water_id = resolve_state_id("minecraft:water");
        assert_ne!(
            bare_water_id,
            air_id(),
            "bare `minecraft:water` (no `level` property) must not resolve to air"
        );
        assert_eq!(
            properties(bare_water_id),
            Some([("level", "0")].as_slice()),
            "expected the default (level=0) water state"
        );
        assert_eq!(
            bare_water_id,
            resolve_state_id("minecraft:water[level=0]"),
            "the bare-name fallback must agree with the fully-qualified default state"
        );
    }

    /// A bare name must resolve to the block's **default** state,
    /// not to its lowest id — and `grass_block` is the case where those differ
    /// visibly: `blocks.json` marks `snowy=false` (id 9) default while id 8 is
    /// `snowy=true`, so the old lowest-id fallback put every spread grass block
    /// on the wire as snowy. `lodestone-data`'s jar-derived default-state column
    /// supplies the expected id; nothing here asks the resolver what it thinks
    /// the default is.
    ///
    /// Also pins the property-override tier on the same block, since a merge
    /// that silently ignored the caller's value would still pass the first half.
    #[test]
    fn resolve_state_id_resolves_a_bare_name_to_the_jar_marked_default_state() {
        let lowest = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| block_name(id) == Some("minecraft:grass_block"))
            .expect("no grass_block in the generated table");
        let jar_default = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| {
                block_name(id) == Some("minecraft:grass_block")
                    && lodestone_data::block_states::StateId::new(id)
                        .expect("generated state-table index is valid")
                        .is_default()
            })
            .expect("no default grass_block state in the jar-derived column");

        assert_ne!(jar_default, lowest, "grass_block's default is not its lowest id");
        assert_eq!(resolve_state_id("minecraft:grass_block"), jar_default);
        assert_eq!(properties(jar_default), Some([("snowy", "false")].as_slice()));
        assert_eq!(
            properties(resolve_state_id("minecraft:grass_block[snowy=true]")),
            Some([("snowy", "true")].as_slice())
        );
    }

    /// The hermetic half of the whole-column wire-fidelity gate: a whole-column `encode_chunk`
    /// send, decoded back through the real wire codec
    /// ([`crate::packets::chunk::LevelChunkWithLight::decode`], the same
    /// decoder `tests/join_flow.rs`'s golden vectors and `tests/live_chunk
    /// .rs`'s live capture pin), must carry the real per-block state rather
    /// than a collapsed solid/air pair — including a **fluid**, the case a
    /// fix that only thinks about solids is most likely to miss (the old
    /// collapse mapped every fluid to air, not stone, so a half-fix would
    /// still pass a solids-only check here).
    ///
    /// This round-trips through this crate's own encode/decode, which
    /// `CLAUDE.md` flags as weaker evidence than an independent oracle
    /// (`decode(encode(x)) == x` is satisfiable by two symmetric
    /// misunderstandings) — the real-client gate in
    /// `tests/block_edit.rs`'s `dig_and_place_persist_through_forget_and
    /// _reload` is the honest one, checking a real `lodestone-client`'s
    /// `block_at` against an independent generator instance. This test is
    /// the fast, hermetic complement: no client/server machinery, so it
    /// pins the exact ids a regression would have to break.
    #[test]
    fn encode_chunk_carries_real_block_states_including_a_fluid() {
        use crate::packets::chunk::LevelChunkWithLight;
        use lodestone_server::{ChunkSource, overworld_chunk_source};

        // Same fixture `tests/block_edit.rs` uses (seed 1234, chunk (0, 0)):
        // real per-block content sampled from an *independent* generator
        // instance, per `CLAUDE.md`'s "an expected value must originate
        // outside the code under test" — this crate's own `resolve_state_id`
        // resolves the id, but the state *strings* being asserted come from
        // nothing this test constructs by hand.
        let seed: i64 = 1234;
        let independent_generator = lodestone_server::overworld_generator(seed);
        let real_column = independent_generator.column(0, 0);
        let deepslate_state = real_column.block_state(0, -50, 0);
        let gravel_state = real_column.block_state(0, 37, 0);
        let water_state = real_column.block_state(0, 38, 0);
        assert_eq!(deepslate_state.split('[').next(), Some("minecraft:deepslate"));
        assert_eq!(gravel_state, "minecraft:gravel");
        assert_eq!(water_state.split('[').next(), Some("minecraft:water"));

        let deepslate_id = resolve_state_id(deepslate_state);
        let gravel_id = resolve_state_id(gravel_state);
        let water_id = resolve_state_id(water_state);
        assert_ne!(
            water_id,
            air_id(),
            "fixture sanity: the real water state must not itself resolve to air"
        );

        // The column actually served, from a second, separately-constructed
        // source — proving `encode_chunk` (not this test) is what produces
        // the fidelity, the same source `V770ServerProtocol` would be given
        // in the live server.
        let source = overworld_chunk_source(seed);
        let served_column = source.column(0, 0);

        let proto = V770ServerProtocol;
        // Named through the trait: `V770ServerProtocol` implements both
        // `ServerProtocol` and `ChunkEncoder`, whose `encode_chunk` methods are
        // deliberately the same body (see the `ChunkEncoder` impl), so an
        // unqualified call is ambiguous rather than wrong.
        let directive = ServerProtocol::encode_chunk(&proto, 0, 0, &served_column);
        let payload = match directive {
            ServerDirective::Send { payload, .. } => payload,
            other => panic!("expected Send, got {other:?}"),
        };

        let shape = ChunkShape::overworld_1_21();
        let mut r = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut r, &shape).expect("decode column");
        r.ensure_empty().expect("no trailing bytes");

        assert_eq!(decoded.column.get_block(0, -50, 0), deepslate_id);
        assert_eq!(decoded.column.get_block(0, 37, 0), gravel_id);
        assert_eq!(
            decoded.column.get_block(0, 38, 0),
            water_id,
            "fluid cell must carry the real water id on the wire, not collapse to air"
        );

        // An untouched, definitely-air cell (well above this column's
        // terrain) still reads as air — the fix does not smear a stray
        // non-air write across cells the source itself reports as air.
        assert_eq!(decoded.column.get_block(5, 300, 5), air_id());
    }

    /// The wire half of server-side light: a served column carries the generator's real
    /// `MOTION_BLOCKING` map, not the zero-entry NBT this encoder sent for every
    /// column until now. The expected values come from the **generator's own**
    /// array through a second, independently constructed source — nothing here
    /// re-derives a height.
    #[test]
    fn a_served_column_carries_the_generators_motion_blocking_heightmap() {
        use crate::packets::chunk::LevelChunkWithLight;
        use lodestone_server::{ChunkSource, overworld_chunk_source};

        let seed: i64 = 1234;
        let expected = *lodestone_server::overworld_generator(seed)
            .column(0, 0)
            .motion_blocking_heightmap()
            .expect("the bundled generator computes MOTION_BLOCKING");

        let source = overworld_chunk_source(seed);
        let directive =
            ServerProtocol::encode_chunk(&V770ServerProtocol, 0, 0, &source.column(0, 0));
        let payload = match directive {
            ServerDirective::Send { payload, .. } => payload,
            other => panic!("expected Send, got {other:?}"),
        };

        let shape = ChunkShape::overworld_1_21();
        let mut r = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut r, &shape).expect("decode column");
        r.ensure_empty().expect("no trailing bytes");

        let map = decoded
            .heightmaps
            .get(MOTION_BLOCKING_HEIGHTMAP_TYPE_ID)
            .expect("MOTION_BLOCKING must be on the wire");
        for lz in 0..16usize {
            for lx in 0..16usize {
                assert_eq!(
                    map.get(lx, lz),
                    u32::from(expected[lx + lz * 16]),
                    "at ({lx}, {lz})"
                );
            }
        }
        // Non-degenerate: an all-zero map is what an empty `Heightmaps` would
        // decode to under a bug that framed 256 entries of nothing, so the
        // element-wise check above must be comparing real heights. (Chunk (0, 0)
        // at this seed is an ocean surface, so the values are *uniform* — a
        // variance assertion here would be false, not stronger.)
        assert!(expected.iter().all(|&h| h > 0), "{expected:?}");

        // An all-air column still carries all three client maps, each at its
        // documented zero (the first available Y is the build minimum).
        let empty = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        let directive = ServerProtocol::encode_chunk(&V770ServerProtocol, 0, 0, &empty);
        let payload = match directive {
            ServerDirective::Send { payload, .. } => payload,
            other => panic!("expected Send, got {other:?}"),
        };
        let mut r = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut r, &shape).expect("decode empty column");
        r.ensure_empty().expect("no trailing bytes");
        assert_eq!(decoded.heightmaps.len(), 3);
        let mut ids: Vec<_> = decoded.heightmaps.iter().map(|(id, _)| id).collect();
        ids.sort_unstable();
        assert_eq!(ids, [1, 4, 5]);
        for (_, map) in decoded.heightmaps.iter() {
            assert!(
                (0..16).all(|z| (0..16).all(|x| map.get(x, z) == 0)),
                "all-air heightmap must be zero: {map:?}"
            );
        }
    }

    /// Both chunk-section count shorts are client-visible fields. This is the
    /// control the older all-zero fluid counter could not pass: two distinct
    /// fluid levels must retain their exact state ids *and* raise the section
    /// fluid count to two, while non-fluid air remains out of both counts.
    #[test]
    fn encode_chunk_counts_and_preserves_fluid_levels() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::overworld_1_21();
        let mut source = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        source.set_block(1, shape.min_y, 2, "minecraft:water[level=7]");
        source.set_block(3, shape.min_y, 4, "minecraft:lava[level=3]");

        let directive = ServerProtocol::encode_chunk(&V770ServerProtocol, 0, 0, &source);
        let payload = match directive {
            ServerDirective::Send { payload, .. } => payload,
            other => panic!("expected Send, got {other:?}"),
        };

        // Read the section prefix directly. The public decoder deliberately
        // discards these redundant counters after consuming them, so decoding
        // a round trip alone cannot distinguish a truthful count from zero.
        let mut r = Reader::new(&payload);
        r.i32().expect("chunk x");
        r.i32().expect("chunk z");
        Heightmaps::decode(shape.world_height, &mut r).expect("heightmaps");
        let blob_len = r.var_i32().expect("section blob length") as usize;
        let mut blob = r.take_reader(blob_len).expect("section blob");
        assert_eq!(blob.i16().expect("non-air count"), 2);
        assert_eq!(blob.i16().expect("fluid count"), 2);

        let mut decoder = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut decoder, &shape).expect("decode chunk");
        decoder.ensure_empty().expect("no trailing bytes");
        for (x, z, expected_level) in [(1, 2, "7"), (3, 4, "3")] {
            let state = decoded.column.get_block(x, shape.min_y, z);
            assert_eq!(
                lodestone_data::block_states::StateId::new(state)
                    .expect("built-in state")
                    .properties(),
                [("level", expected_level)],
                "fluid level at ({x}, {z})"
            );
        }
    }

    #[test]
    fn encode_chunk_excludes_all_three_air_variants_from_header_count() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::overworld_1_21();
        let mut source = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        let entries = [
            (0, "minecraft:air"),
            (1, "minecraft:cave_air"),
            (2, "minecraft:void_air"),
            (3, "minecraft:stone"),
            (4, "minecraft:water[level=7]"),
        ];
        for (x, state) in entries {
            source.set_block(x as i32, shape.min_y, 0, state);
        }

        let ServerDirective::Send { payload, .. } =
            ServerProtocol::encode_chunk(&V770ServerProtocol, 0, 0, &source)
        else {
            panic!("expected Send");
        };
        let mut header = Reader::new(&payload);
        header.i32().expect("chunk x");
        header.i32().expect("chunk z");
        Heightmaps::decode(shape.world_height, &mut header).expect("heightmaps");
        let blob_len = header.var_i32().expect("section blob length") as usize;
        let mut blob = header.take_reader(blob_len).expect("section blob");
        assert_eq!(blob.i16().expect("non-empty block count"), 2);
        assert_eq!(blob.i16().expect("fluid count"), 1);

        let mut decoded_reader = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut decoded_reader, &shape).expect("decode chunk");
        decoded_reader.ensure_empty().expect("no trailing bytes");
        for (x, state) in entries {
            assert_eq!(
                decoded.column.get_block(x, shape.min_y, 0),
                resolve_state_id(state),
                "decoded state at x={x}"
            );
        }
    }

    #[test]
    fn encode_chunk_retains_cave_air_payload_when_header_count_is_zero() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::overworld_1_21();
        let mut source = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        source.set_block(0, shape.min_y, 0, "minecraft:cave_air");

        let ServerDirective::Send { payload, .. } =
            ServerProtocol::encode_chunk(&V770ServerProtocol, 0, 0, &source)
        else {
            panic!("expected Send");
        };
        let mut header = Reader::new(&payload);
        header.i32().expect("chunk x");
        header.i32().expect("chunk z");
        Heightmaps::decode(shape.world_height, &mut header).expect("heightmaps");
        let blob_len = header.var_i32().expect("section blob length") as usize;
        let mut blob = header.take_reader(blob_len).expect("section blob");
        assert_eq!(blob.i16().expect("non-empty block count"), 0);
        assert_eq!(blob.i16().expect("fluid count"), 0);

        let mut decoded_reader = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut decoded_reader, &shape).expect("decode chunk");
        decoded_reader.ensure_empty().expect("no trailing bytes");
        assert!(decoded.column.section(0).is_some(), "cave-air payload must be retained");
        assert_eq!(
            decoded.column.get_block(0, shape.min_y, 0),
            resolve_state_id("minecraft:cave_air")
        );
    }

    /// The three sent heightmaps do not share one predicate: a top leaf and
    /// water distinguish the visible surface, motion-blocking, and no-leaves
    /// maps.
    /// The expected stored heights use the external registry's three predicates
    /// and the `first free Y - min Y` representation, not this encoder's scan.
    #[test]
    fn encode_chunk_carries_each_client_heightmap() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::overworld_1_21();
        let mut source = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        source.set_block(0, -60, 0, "minecraft:stone");
        source.set_block(0, -56, 0, "minecraft:water[level=0]");
        source.set_block(0, -52, 0, "minecraft:oak_leaves[persistent=true,distance=7,waterlogged=false]");

        let directive = ServerProtocol::encode_chunk(&V770ServerProtocol, 0, 0, &source);
        let payload = match directive {
            ServerDirective::Send { payload, .. } => payload,
            other => panic!("expected Send, got {other:?}"),
        };
        let mut r = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut r, &shape).expect("decode chunk");
        r.ensure_empty().expect("no trailing bytes");

        assert_eq!(decoded.heightmaps.len(), 3);
        assert_eq!(
            decoded
                .heightmaps
                .get(WORLD_SURFACE_HEIGHTMAP_TYPE_ID)
                .expect("WORLD_SURFACE")
                .get(0, 0),
            13
        );
        assert_eq!(
            decoded
                .heightmaps
                .get(MOTION_BLOCKING_HEIGHTMAP_TYPE_ID)
                .expect("MOTION_BLOCKING")
                .get(0, 0),
            13
        );
        assert_eq!(
            decoded
                .heightmaps
                .get(MOTION_BLOCKING_NO_LEAVES_HEIGHTMAP_TYPE_ID)
                .expect("MOTION_BLOCKING_NO_LEAVES")
                .get(0, 0),
            9
        );
    }

    /// The server's initial-chunk path supplies resident neighbours to the
    /// version encoder. A source in the east column must illuminate the centre
    /// column's east edge; the one-column encoder is the control that must not
    /// accidentally pass this test.
    #[test]
    fn neighbour_aware_chunk_encoding_lights_across_the_east_seam() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::overworld_1_21();
        let center = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        let mut east = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        east.set_block(0, 0, 0, "minecraft:glowstone");

        let proto = V770ServerProtocol;
        let decode_light = |directive: ServerDirective| {
            let ServerDirective::Send { payload, .. } = directive else {
                panic!("chunk encoder must send a packet");
            };
            let mut r = Reader::new(&payload);
            let decoded = LevelChunkWithLight::decode(&mut r, &shape).expect("decode chunk");
            r.ensure_empty().expect("no trailing bytes");
            decoded.light.section_light(5).block_at(15, 0, 0)
        };
        let isolated = decode_light(ServerProtocol::encode_chunk(&proto, 0, 0, &center));
        let with_east = decode_light(
            proto
                .try_encode_chunk_with_neighbours(0, 0, &center, &[(1, 0, east)])
                .expect("neighbour-aware encoding"),
        );

        assert_eq!(isolated, 0, "control: no local source reaches this cell");
        assert_eq!(with_east, 14, "east-neighbour source crosses one air cell");
    }

    /// Initial Nether masks follow light-section storage allocation, not just
    /// the highest local block or the presence of an emitter. This models the
    /// accepted (-7,-8) row with a high diagonal section and keeps (-8,-8) as
    /// the negative control: a same-height neighbour must not extend its mask.
    #[test]
    fn nether_initial_masks_follow_neighbour_section_allocation_with_minus_eight_control() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::nether_or_end_1_21();
        let proto = V770ServerProtocol;
        let decode = |center: &ServerChunkColumn,
                      neighbours: &[(i32, i32, ServerChunkColumn)]| {
            let ServerDirective::Send { payload, .. } = proto
                .try_encode_chunk_with_neighbours_in_dimension(
                    0,
                    0,
                    center,
                    neighbours,
                    Dimension::Nether,
                )
                .expect("initial Nether chunk")
            else {
                panic!("initial Nether chunk must send a packet");
            };
            let mut reader = Reader::new(&payload);
            let packet =
                LevelChunkWithLight::decode(&mut reader, &shape).expect("decode Nether chunk");
            reader.ensure_empty().expect("no Nether trailing bytes");
            packet.light
        };

        let mut center = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        center.set_block(8, 127, 8, "minecraft:netherrack");

        // This is intentionally non-emissive: the extra Empty mask comes from
        // the allocated section, not from a computed non-zero block-light cell.
        let mut high_diagonal = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        high_diagonal.set_block(8, 128, 8, "minecraft:netherrack");
        let with_high_diagonal = decode(&center, &[(1, 1, high_diagonal)]);
        assert_eq!(with_high_diagonal.block(9), &LightData::Uniform(0));
        assert_eq!(
            with_high_diagonal.block(10),
            &LightData::Uniform(0),
            "a non-air diagonal section allocates the one-section vertical apron"
        );
        assert_eq!(with_high_diagonal.block(11), &LightData::Missing);

        let mut same_height = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        same_height.set_block(8, 127, 8, "minecraft:netherrack");
        let without_high_section = decode(&center, &[(1, 1, same_height)]);
        for section in 0..7 {
            assert_eq!(
                without_high_section.block(section),
                &LightData::Missing,
                "(-8,-8) control omits the unallocated lower light section {section}"
            );
        }
        for section in 7..=9 {
            assert_eq!(
                without_high_section.block(section),
                &LightData::Uniform(0),
                "(-8,-8) control keeps its allocated light section {section}"
            );
        }
        for section in 10..18 {
            assert_eq!(
                without_high_section.block(section),
                &LightData::Missing,
                "(-8,-8) control omits unallocated light section {section}"
            );
        }
    }

    /// Initial chunks and later light updates must use the dimension carried by
    /// the source, not infer skylight from the column's shared 0..256 window.
    #[test]
    fn dimension_aware_initial_chunks_and_light_updates_preserve_sky_rules() {
        use crate::packets::chunk::LevelChunkWithLight;

        let proto = V770ServerProtocol;
        let nether_source = lodestone_server::nether_chunk_source(42);
        let nether = lodestone_server::ChunkSource::column(&nether_source, 0, 0);
        {
            let shape = ChunkShape::nether_or_end_1_21();
            let ServerDirective::Send { payload, .. } = proto
                .try_encode_chunk_with_neighbours_in_dimension(
                    0,
                    0,
                    &nether,
                    &[],
                    Dimension::Nether,
                )
                .expect("initial chunk")
            else {
                panic!("nether initial chunk must send a packet");
            };
            let mut chunk_reader = Reader::new(&payload);
            let initial =
                LevelChunkWithLight::decode(&mut chunk_reader, &shape).expect("decode initial chunk");
            chunk_reader.ensure_empty().expect("no initial trailing bytes");
            assert_eq!(initial.light.light_section_count(), 18, "nether light window");
            for section in 0..18 {
                assert_eq!(
                    *initial.light.sky(section),
                    LightData::Missing,
                    "external initial Nether chunks omit sky section {section}"
                );
            }

            let light = proto
                .compute_column_light_with_neighbours_in_dimension(
                    &nether,
                    &[],
                    Dimension::Nether,
                )
                .expect("light update computation");
            let ServerDirective::Send { payload, .. } = proto.encode_light_update(0, 0, &light) else {
                panic!("nether light update must send a packet");
            };
            let mut update_reader = Reader::new(&payload);
            assert_eq!(update_reader.var_i32().expect("update chunk x"), 0);
            assert_eq!(update_reader.var_i32().expect("update chunk z"), 0);
            let update = ColumnLight::decode(16, &mut update_reader).expect("decode light update");
            update_reader.ensure_empty().expect("no update trailing bytes");
            for section in 0..18 {
                assert_eq!(
                    *update.sky(section),
                    LightData::Uniform(0),
                    "nether light update sky section {section}"
                );
            }
        }

        let end_shape = ChunkShape::nether_or_end_1_21();
        let end = ServerChunkColumn::new(0, 256);
        let ServerDirective::Send { payload, .. } = proto
            .try_encode_chunk_with_neighbours_in_dimension(0, 0, &end, &[], Dimension::End)
            .expect("end initial chunk")
        else {
            panic!("end initial chunk must send a packet");
        };
        let mut reader = Reader::new(&payload);
        let initial =
            LevelChunkWithLight::decode(&mut reader, &end_shape).expect("decode end initial");
        reader.ensure_empty().expect("no end initial trailing bytes");
        assert_eq!(
            initial.light.sky(1),
            &LightData::Missing,
            "an all-air End initial chunk has no allocated light sections"
        );
        let update = proto
            .compute_column_light_with_neighbours_in_dimension(&end, &[], Dimension::End)
            .expect("end light update computation");
        assert_eq!(
            update.section_light(1).sky_at(0, 0, 0),
            15,
            "End light updates retain sky light"
        );

        let overworld_shape = ChunkShape::overworld_1_21();
        let overworld = ServerChunkColumn::new(
            overworld_shape.min_y,
            overworld_shape.world_height as i32,
        );
        let ServerDirective::Send { payload, .. } =
            ServerProtocol::try_encode_chunk_in_dimension(
                &proto,
                0,
                0,
                &overworld,
                Dimension::Overworld,
            )
            .expect("overworld initial chunk")
        else {
            panic!("overworld initial chunk must send a packet");
        };
        let mut overworld_reader = Reader::new(&payload);
        let initial =
            LevelChunkWithLight::decode(&mut overworld_reader, &overworld_shape)
                .expect("decode overworld initial");
        overworld_reader
            .ensure_empty()
            .expect("no overworld initial trailing bytes");
        assert_eq!(
            initial.light.section_light(1).sky_at(0, 0, 0),
            15,
            "overworld initial chunks retain sky light"
        );
        let update = proto
            .compute_column_light_in_dimension(&overworld, Dimension::Overworld)
            .expect("overworld light update computation");
        assert_eq!(
            update.section_light(1).sky_at(0, 0, 0),
            15,
            "overworld light updates retain sky light"
        );
    }

    /// A generated End column has no persisted settlement snapshot in this
    /// direct source control. Its initial fallback must nevertheless use the
    /// sparse storage shape: the lower apron stays absent, the two full sky
    /// sections above the island remain present, and the next section is
    /// omitted. This is the negative control for accidentally applying the
    /// Overworld one-section trim to End columns.
    #[test]
    fn end_generated_initial_fallback_keeps_storage_shape_without_snapshot() {
        use crate::packets::chunk::LevelChunkWithLight;

        let source = lodestone_server::end_chunk_source(42);
        let center = lodestone_server::ChunkSource::column(&source, -250, -250);
        assert!(
            center.retained_light().is_none(),
            "the generated-source control must exercise the fallback, not persisted light"
        );
        let mut neighbours = Vec::with_capacity(8);
        for dz in -1..=1 {
            for dx in -1..=1 {
                if (dx, dz) != (0, 0) {
                    neighbours.push((
                        dx,
                        dz,
                        lodestone_server::ChunkSource::column(&source, -250 + dx, -250 + dz),
                    ));
                }
            }
        }

        let ServerDirective::Send { payload, .. } = V770ServerProtocol
            .try_encode_chunk_with_neighbours_in_dimension(
                -250,
                -250,
                &center,
                &neighbours,
                Dimension::End,
            )
            .expect("generated End initial chunk")
        else {
            panic!("generated End initial chunk must send a packet");
        };
        let shape = ChunkShape::nether_or_end_1_21();
        let mut reader = Reader::new(&payload);
        let packet = LevelChunkWithLight::decode(&mut reader, &shape)
            .expect("decode generated End initial chunk");
        reader.ensure_empty().expect("no End packet trailing bytes");

        assert_eq!(packet.light.sky(0), &LightData::Missing);
        assert_eq!(packet.light.sky(5), &LightData::Uniform(15));
        assert_eq!(packet.light.sky(6), &LightData::Uniform(15));
        assert_eq!(packet.light.sky(7), &LightData::Missing);
        assert_eq!(packet.light.block(0), &LightData::Missing);
        assert_eq!(packet.light.block(6), &LightData::Uniform(0));
        assert_eq!(packet.light.block(7), &LightData::Missing);
    }

    /// End initial storage is derived from admitted occupancy. A high
    /// neighbour extends the same relative corridor, while a footprint with
    /// no non-air section allocates nothing; neither result depends on a world
    /// coordinate or a fixed section count.
    #[test]
    fn end_initial_storage_corridor_follows_admitted_occupancy() {
        let column = || {
            WorldChunkColumn::new(
                0,
                16,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                0,
                0,
            )
        };
        let mut center = column();
        center.set_block(8, 80, 8, 1);
        let empty = column();
        let storage = initial_end_light_storage_sections(&center, std::slice::from_ref(&empty));
        assert!(!storage[0]);
        assert!(storage[5..=7].iter().all(|&stored| stored));
        assert!(storage[..5].iter().all(|&stored| !stored));
        assert!(storage[8..].iter().all(|&stored| !stored));

        let mut high_neighbour = empty.clone();
        high_neighbour.set_block(8, 128, 8, 1);
        let extended = initial_end_light_storage_sections(&center, &[high_neighbour]);
        assert!(extended[5..=10].iter().all(|&stored| stored));
        assert!(extended[..5].iter().all(|&stored| !stored));
        assert!(extended[11..].iter().all(|&stored| !stored));

        let all_air = initial_end_light_storage_sections(&empty, &[]);
        assert!(all_air.iter().all(|&stored| !stored));
    }

    /// End storage follows the admitted footprint, even when the selected
    /// centre is all air. The neighbour's two occupied sections induce the
    /// exact four-section corridor in both fresh centre and dependency
    /// snapshots; an all-air footprint remains entirely Missing.
    #[test]
    fn end_initial_storage_follows_admitted_footprint_for_fresh_columns() {
        let column = || {
            WorldChunkColumn::new(
                0,
                16,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                0,
                0,
            )
        };
        let center = column();
        let mut neighbour = column();
        neighbour.set_block(8, 16, 8, 1);
        neighbour.set_block(8, 32, 8, 1);
        let borrowed = initial_end_light_storage_sections(&center, &[neighbour.clone()]);
        assert!(
            borrowed[1..=4].iter().all(|&stored| stored),
            "an all-air centre inherits the admitted neighbour corridor"
        );
        assert!(
            borrowed
                .iter()
                .enumerate()
                .filter(|(section, _)| !(1..=4).contains(section))
                .all(|(_, &stored)| !stored),
            "the neighbour corridor must not allocate unrelated sections"
        );
        let all_air = initial_end_light_storage_sections(&center, &[]);
        assert!(all_air.iter().all(|&stored| !stored));
    }

    /// A dependency snapshot describes the admission that initialized it, not
    /// the centre admission that is happening now. In particular, an old
    /// all-sections dependency layer must not make a fresh all-air centre
    /// retain all 18 light sections when the new footprint only has a
    /// four-section terrain corridor.
    #[test]
    fn end_dependency_snapshot_does_not_define_centre_allocation() {
        let mut center = ServerChunkColumn::new(0, 256);
        let mut dependency_snapshot = ColumnLight::new(16);
        for section in 0..dependency_snapshot.light_section_count() {
            *dependency_snapshot.sky_mut(section) = LightData::Uniform(0);
            *dependency_snapshot.block_mut(section) = LightData::Uniform(0);
        }
        center.set_retained_light_with_status(
            dependency_snapshot,
            RetainedLightStatus::DependencyInitialized,
        );
        let mut neighbour = ServerChunkColumn::new(0, 256);
        neighbour.set_block(8, 16, 8, "minecraft:stone");
        neighbour.set_block(8, 32, 8, "minecraft:stone");

        let settlement = V770ServerProtocol
            .compute_initial_column_lights_with_neighbours_in_dimension(
                &center,
                &[(1, 0, neighbour)],
                Dimension::End,
            )
            .expect("fresh End centre admission");
        let light = settlement.centre_light();
        for section in 0..light.light_section_count() {
            let expected_stored = (1..=4).contains(&section);
            assert_eq!(
                !matches!(light.sky(section), LightData::Missing),
                expected_stored,
                "dependency-initialized centre sky allocation at section {section}",
            );
            assert_eq!(
                !matches!(light.block(section), LightData::Missing),
                expected_stored,
                "dependency-initialized centre block allocation at section {section}",
            );
        }
    }

    /// A retained snapshot in one neighbour must not cause fresh terrain in a
    /// different neighbour to disappear from the End storage walk. The
    /// retained sky seed makes the old non-zero-value filter active; the east
    /// terrain is the control that proves allocation follows the complete
    /// admitted footprint rather than retained sky values alone.
    #[test]
    fn end_storage_keeps_fresh_terrain_with_another_retained_sky_snapshot() {
        let center = ServerChunkColumn::new(0, 256);
        let mut east = ServerChunkColumn::new(0, 256);
        east.set_block(8, 16, 8, "minecraft:stone");
        east.set_block(8, 32, 8, "minecraft:stone");

        let mut west = ServerChunkColumn::new(0, 256);
        let mut west_light = ColumnLight::new(16);
        *west_light.sky_mut(1) = LightData::Uniform(15);
        *west_light.block_mut(1) = LightData::Uniform(0);
        west.set_retained_light_with_status(west_light, RetainedLightStatus::CentreSettled);

        let settlement = V770ServerProtocol
            .compute_initial_column_lights_with_neighbours_in_dimension(
                &center,
                &[(1, 0, east), (-1, 0, west)],
                Dimension::End,
            )
            .expect("fresh End centre admission");
        let light = settlement.centre_light();
        for section in 0..light.light_section_count() {
            let expected_stored = (1..=4).contains(&section);
            assert_eq!(
                !matches!(light.sky(section), LightData::Missing),
                expected_stored,
                "fresh-neighbour centre sky allocation at section {section}",
            );
            assert_eq!(
                !matches!(light.block(section), LightData::Missing),
                expected_stored,
                "fresh-neighbour centre block allocation at section {section}",
            );
        }
        assert_eq!(
            light.section_light(1).sky_at(0, 0, 0),
            15,
            "the retained neighbour must not suppress fresh-centre sky values",
        );
    }

    /// A dependency's retained sky values belong to its own settled terrain.
    /// They must not seed a fresh End centre admission, where the complete
    /// current 3x3 terrain footprint is the authoritative sky source.
    #[test]
    fn end_retained_dependency_sky_does_not_seed_fresh_centre() {
        let mut center = ServerChunkColumn::new(0, 256);
        let mut west = ServerChunkColumn::new(0, 256);
        for z in 0..16 {
            for x in 0..16 {
                center.set_block(x, 0, z, "minecraft:end_stone");
                west.set_block(x, 0, z, "minecraft:end_stone");
            }
        }

        let baseline = V770ServerProtocol
            .compute_initial_column_lights_with_neighbours_in_dimension(
                &center,
                &[(-1, 0, west.clone())],
                Dimension::End,
            )
            .expect("fresh End centre admission without retained dependency");

        let mut retained_west = west;
        let mut retained = ColumnLight::new(16);
        for section in 0..retained.light_section_count() {
            *retained.sky_mut(section) = LightData::Uniform(15);
            *retained.block_mut(section) = LightData::Uniform(0);
        }
        retained_west.set_retained_light_with_status(
            retained,
            RetainedLightStatus::CentreSettled,
        );
        let with_retained_dependency = V770ServerProtocol
            .compute_initial_column_lights_with_neighbours_in_dimension(
                &center,
                &[(-1, 0, retained_west)],
                Dimension::End,
            )
            .expect("fresh End centre admission with retained dependency");

        assert_eq!(
            with_retained_dependency.centre_light(),
            baseline.centre_light(),
            "a retained dependency must not brighten the fresh centre's sky field",
        );
    }

    /// A sparse centre-settled snapshot remains authoritative even when its
    /// terrain is all air. This is distinct from the dependency-initialized
    /// all-sections control above: the centre's own saved allocation is the
    /// one prior mask that may survive a new admission.
    #[test]
    fn end_centre_settled_sparse_storage_is_preserved() {
        let mut center = ServerChunkColumn::new(0, 256);
        let mut saved = ColumnLight::new(16);
        for section in 1..=4 {
            *saved.sky_mut(section) = LightData::Uniform(15);
            *saved.block_mut(section) = LightData::Uniform(0);
        }
        center.set_retained_light_with_status(saved, RetainedLightStatus::CentreSettled);

        let settlement = V770ServerProtocol
            .compute_initial_column_lights_with_neighbours_in_dimension(
                &center,
                &[],
                Dimension::End,
            )
            .expect("centre-settled End admission");
        let light = settlement.centre_light();
        for section in 0..light.light_section_count() {
            let expected_stored = (1..=4).contains(&section);
            assert_eq!(
                !matches!(light.sky(section), LightData::Missing),
                expected_stored,
                "centre-settled sky allocation at section {section}",
            );
            assert_eq!(
                !matches!(light.block(section), LightData::Missing),
                expected_stored,
                "centre-settled block allocation at section {section}",
            );
        }
    }

    #[test]
    fn end_initial_dependency_layers_use_admitted_storage_shape() {
        let center = ServerChunkColumn::new(0, 256);
        let mut neighbour = ServerChunkColumn::new(0, 256);
        neighbour.set_block(8, 16, 8, "minecraft:stone");
        neighbour.set_block(8, 32, 8, "minecraft:stone");
        let proto = V770ServerProtocol;

        let all_air = proto
            .compute_initial_column_lights_with_neighbours_in_dimension(
                &center,
                &[(1, 0, neighbour.clone())],
                Dimension::End,
            )
            .expect("fresh End settlement");
        let shape = shape_for_column(&center);
        let centre_world = build_world_column(&shape, &center);
        let neighbour_world = build_world_column(&shape, &neighbour);
        let storage = initial_end_light_storage_sections(&centre_world, &[neighbour_world]);
        assert!(storage[1..=4].iter().all(|&stored| stored));
        let mut expected_lights = proto
            .compute_initial_column_lights_with_neighbours_and_storage_in_dimension(
                &center,
                &[(1, 0, neighbour.clone())],
                &[None; 9],
                Dimension::End,
            )
            .expect("raw End settlement");
        normalize_initial_chunk_light(
            &mut expected_lights[4],
            Dimension::End,
            Some(&storage),
        );
        normalize_initial_chunk_light(
            &mut expected_lights[5],
            Dimension::End,
            Some(&storage),
        );
        let expected = ColumnLightSettlement::with_neighbours(
            expected_lights[4].clone(),
            [(1, 0, expected_lights[5].clone())],
        )
        .expect("expected centre and dependency settlement");
        assert_eq!(all_air, expected);
        for section in 0..expected_lights[4].light_section_count() {
            let expected_stored = (1..=4).contains(&section);
            assert_eq!(
                !matches!(expected_lights[4].sky(section), LightData::Missing),
                expected_stored,
                "centre sky storage at section {section}"
            );
            assert_eq!(
                !matches!(expected_lights[5].block(section), LightData::Missing),
                expected_stored,
                "dependency block storage at section {section}"
            );
        }

        let empty_neighbours = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
            .filter(|&(dx, dz)| (dx, dz) != (0, 0))
            .map(|(dx, dz)| (dx, dz, ServerChunkColumn::new(0, 256)))
            .collect::<Vec<_>>();
        let empty_footprint = proto
            .compute_initial_column_lights_with_neighbours_in_dimension(
                &center,
                &empty_neighbours,
                Dimension::End,
            )
            .expect("fresh all-air settlement");
        let empty = ColumnLight::new(shape.section_count);
        let expected_empty = ColumnLightSettlement::with_neighbours(
            empty.clone(),
            [
                (-1, -1, empty.clone()),
                (0, -1, empty.clone()),
                (1, -1, empty.clone()),
                (-1, 0, empty.clone()),
                (1, 0, empty.clone()),
                (-1, 1, empty.clone()),
                (0, 1, empty.clone()),
                (1, 1, empty),
            ],
        )
        .expect("expected all-air settlement");
        assert_eq!(empty_footprint, expected_empty);
        for section in 0..shape.section_count + 2 {
            assert!(matches!(expected_empty.centre_light().sky(section), LightData::Missing));
            assert!(matches!(expected_empty.centre_light().block(section), LightData::Missing));
        }
    }

    /// Dependency-initialized light is valid input to a later light admission,
    /// but it is not yet safe to serialize as the centre's initial packet.
    #[test]
    fn initial_encoder_only_consumes_centre_settled_light() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::nether_or_end_1_21();
        let decode = |column: &ServerChunkColumn| {
            let ServerDirective::Send { payload, .. } = V770ServerProtocol
                .try_encode_chunk_with_neighbours_in_dimension(
                    0,
                    0,
                    column,
                    &[],
                    Dimension::End,
                )
                .expect("End initial chunk")
            else {
                panic!("End initial chunk must send a packet");
            };
            let mut reader = Reader::new(&payload);
            let packet = LevelChunkWithLight::decode(&mut reader, &shape)
                .expect("decode End initial chunk");
            reader.ensure_empty().expect("no End packet trailing bytes");
            packet.light
        };

        let mut dependency = ServerChunkColumn::new(0, 256);
        dependency.set_block(8, 0, 8, "minecraft:end_stone");
        let empty_light = ColumnLight::new(shape.section_count);
        dependency.set_retained_light_with_status(
            empty_light.clone(),
            RetainedLightStatus::DependencyInitialized,
        );
        let recomputed = decode(&dependency);
        assert_ne!(
            recomputed, empty_light,
            "dependency light must be recomputed before its centre admission"
        );

        dependency.set_retained_light_with_status(
            empty_light.clone(),
            RetainedLightStatus::CentreSettled,
        );
        assert_eq!(
            decode(&dependency),
            empty_light,
            "centre-settled light must be consumed verbatim"
        );
    }

    /// Persistence can omit an all-zero pair in the middle of an End corridor
    /// while retaining the non-zero layers around it. The initial packet must
    /// restore that allocated section as explicit empty sky and block light;
    /// the unallocated layer above the corridor must remain missing.
    #[test]
    fn end_initial_encoder_restores_interior_retained_storage_gap() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::nether_or_end_1_21();
        let mut column = ServerChunkColumn::new(0, 256);
        let mut retained = ColumnLight::new(shape.section_count);
        for section in [0, 1, 2, 4, 5] {
            *retained.sky_mut(section) = LightData::Uniform(15);
            *retained.block_mut(section) = LightData::Uniform(0);
        }
        column.set_retained_light_with_status(retained, RetainedLightStatus::CentreSettled);

        let ServerDirective::Send { payload, .. } = V770ServerProtocol
            .try_encode_chunk_with_neighbours_in_dimension(1, -5, &column, &[], Dimension::End)
            .expect("End initial chunk")
        else {
            panic!("End initial chunk must send a packet");
        };
        let mut reader = Reader::new(&payload);
        let packet = LevelChunkWithLight::decode(&mut reader, &shape)
            .expect("decode End initial chunk");
        reader.ensure_empty().expect("no End packet trailing bytes");
        assert_eq!(packet.light.sky(3), &LightData::Uniform(0));
        assert_eq!(packet.light.block(3), &LightData::Uniform(0));
        assert!(matches!(packet.light.sky(6), LightData::Missing));
        assert!(matches!(packet.light.block(6), LightData::Missing));
    }

    /// A persisted End centre can omit the zero-valued lower part of its
    /// allocation corridor because those sections have no retained arrays.
    /// Reconstruct the corridor from a current terrain neighbour and emit the
    /// allocated lower layers as explicit empty sky and block sections.
    #[test]
    fn end_initial_encoder_restores_persisted_storage_apron() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::nether_or_end_1_21();
        let mut column = ServerChunkColumn::new(0, 256);
        let mut retained = ColumnLight::new(shape.section_count);
        *retained.sky_mut(2) = LightData::Uniform(15);
        *retained.block_mut(2) = LightData::Uniform(0);
        column.set_retained_light_with_status(retained, RetainedLightStatus::CentreSettled);

        let mut neighbour = ServerChunkColumn::new(0, 256);
        neighbour.set_block(8, 0, 8, "minecraft:end_stone");
        let ServerDirective::Send { payload, .. } = V770ServerProtocol
            .try_encode_chunk_with_neighbours_in_dimension(
                1,
                -5,
                &column,
                &[(1, 0, neighbour)],
                Dimension::End,
            )
            .expect("End initial chunk")
        else {
            panic!("End initial chunk must send a packet");
        };
        let mut reader = Reader::new(&payload);
        let packet = LevelChunkWithLight::decode(&mut reader, &shape)
            .expect("decode End initial chunk");
        reader.ensure_empty().expect("no End packet trailing bytes");

        for section in 0..=1 {
            assert_eq!(packet.light.sky(section), &LightData::Uniform(0));
            assert_eq!(packet.light.block(section), &LightData::Uniform(0));
        }
        assert_eq!(packet.light.sky(2), &LightData::Uniform(15));
        assert_eq!(packet.light.block(2), &LightData::Uniform(0));
        assert!(matches!(packet.light.sky(3), LightData::Missing));
        assert!(matches!(packet.light.block(3), LightData::Missing));
    }

    /// End's initial path must actually consume the supplied east column. The
    /// lower apron is the discriminating cell: with the east input present,
    /// sky can cross the seam below a centre island; without it the barrier
    /// leaves that cell dark. The all-neighbour packet path is otherwise easy
    /// to mistake for an isolated computation because upper open air is full
    /// sky in either case.
    #[test]
    fn end_initial_fallback_uses_east_neighbour_for_lower_apron_sky() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::nether_or_end_1_21();
        let mut center = ServerChunkColumn::new(0, 256);
        for z in 0..16 {
            for x in 0..16 {
                center.set_block(x, 0, z, "minecraft:end_stone");
            }
        }
        let east = ServerChunkColumn::new(0, 256);
        let all_neighbours = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
            .filter(|&(dx, dz)| (dx, dz) != (0, 0))
            .map(|(dx, dz)| (dx, dz, ServerChunkColumn::new(0, 256)))
            .collect::<Vec<_>>();
        let proto = V770ServerProtocol;
        let decode = |neighbours: &[(i32, i32, ServerChunkColumn)]| {
            let ServerDirective::Send { payload, .. } = proto
                .try_encode_chunk_with_neighbours_in_dimension(
                    0,
                    0,
                    &center,
                    neighbours,
                    Dimension::End,
                )
                .expect("End initial chunk")
            else {
                panic!("End initial chunk must send a packet");
            };
            let mut reader = Reader::new(&payload);
            let packet = LevelChunkWithLight::decode(&mut reader, &shape)
                .expect("decode End initial chunk");
            reader.ensure_empty().expect("no End packet trailing bytes");
            packet.light
        };

        let isolated = decode(&[]);
        let with_east = decode(&[(1, 0, east)]);
        let without_east = decode(
            &all_neighbours
                .iter()
                .filter(|&&(dx, dz, _)| (dx, dz) != (1, 0))
                .map(|(dx, dz, column)| (*dx, *dz, column.clone()))
                .collect::<Vec<_>>(),
        );
        let with_all = decode(&all_neighbours);
        assert!(matches!(with_east.sky(0), LightData::Values(_)));
        assert_eq!(isolated.section_light(0).sky_at(15, 15, 8), 0);
        assert_eq!(with_east.section_light(0).sky_at(15, 15, 8), 14);
        assert_eq!(with_all.section_light(0).sky_at(15, 15, 8), 14);
        assert_eq!(without_east.section_light(0).sky_at(15, 15, 8), 7);
    }

    /// Initial End packets consume the exact light snapshot retained at the
    /// settlement fence. Reload captures contain both stored empty sections
    /// and stored full sections, and one capture has a varied lower section;
    /// preserving those distinctions is the control against reconstructing a
    /// mask from terrain, coordinates, or neighbour order.
    #[test]
    fn initial_chunk_light_uses_the_persisted_end_snapshot() {
        use crate::packets::chunk::LevelChunkWithLight;
        use lodestone_world::NibbleArray;

        let proto = V770ServerProtocol;
        assert!(
            ServerProtocol::retains_initial_column_light(&proto),
            "the initial End encoder consumes retained light snapshots"
        );
        let shape = ChunkShape::nether_or_end_1_21();
        let decode = |column: &ServerChunkColumn, dimension: Dimension| {
            let ServerDirective::Send { payload, .. } = proto
                .try_encode_chunk_with_neighbours_in_dimension(0, 0, column, &[], dimension)
                .expect("initial chunk")
            else {
                panic!("chunk encoder must send a packet");
            };
            let mut reader = Reader::new(&payload);
            let packet = LevelChunkWithLight::decode(&mut reader, &shape).expect("decode chunk");
            reader.ensure_empty().expect("no trailing bytes");
            packet
        };

        let mut nether = ServerChunkColumn::new(0, 256);
        nether.set_block(8, 127, 8, "minecraft:netherrack");
        let nether = decode(&nether, Dimension::Nether);
        for section in 0..18 {
            assert_eq!(
                nether.light.sky(section),
                &LightData::Missing,
                "the external Nether initial form has no sky section {section}"
            );
            assert_eq!(
                nether.light.block(section),
                if (7..=9).contains(&section) {
                    &LightData::Uniform(0)
                } else {
                    &LightData::Missing
                },
                "a non-air block section allocates its own light section and one-section apron {section}"
            );
        }

        let decode_end = |column: &ServerChunkColumn| {
            let ServerDirective::Send { payload, .. } = proto
                .try_encode_chunk_with_neighbours_in_dimension(
                    0,
                    0,
                    column,
                    &[],
                    Dimension::End,
                )
                .expect("End initial chunk from retained snapshot")
            else {
                panic!("End initial chunk must send a packet");
            };
            let mut reader = Reader::new(&payload);
            let packet = LevelChunkWithLight::decode(&mut reader, &shape)
                .expect("decode End chunk from retained snapshot");
            reader.ensure_empty().expect("no End trailing bytes");
            packet
        };

        // The reload row for the first captured target has sky sections 0..=6
        // present, with a varied section 0 and full sections above it. Its
        // block-light sections 0..=6 are explicitly empty.
        let mut first_light = ColumnLight::new(shape.section_count);
        let mut varied = NibbleArray::filled(15);
        varied.set(NibbleArray::index(3, 5, 7), 11);
        *first_light.sky_mut(0) = LightData::Values(varied);
        for section in 1..=6 {
            *first_light.sky_mut(section) = LightData::Uniform(15);
        }
        for section in 0..=6 {
            *first_light.block_mut(section) = LightData::Uniform(0);
        }
        let mut first_column = ServerChunkColumn::new(0, 256);
        first_column.set_retained_light(first_light.clone());
        let first_packet = decode_end(&first_column);
        assert_eq!(
            first_packet.light, first_light,
            "initial End encoding must consume the retained varied snapshot verbatim"
        );

        // A second reload row stores an empty sky range 0..=3 and full sky
        // range 4..=7, while every block-light section 0..=7 is explicitly
        // empty. The same encoder must preserve Missing versus Uniform(0).
        let mut reload_light = ColumnLight::new(shape.section_count);
        for section in 0..=3 {
            *reload_light.sky_mut(section) = LightData::Uniform(0);
            *reload_light.block_mut(section) = LightData::Uniform(0);
        }
        for section in 4..=7 {
            *reload_light.sky_mut(section) = LightData::Uniform(15);
            *reload_light.block_mut(section) = LightData::Uniform(0);
        }
        let mut reload_column = ServerChunkColumn::new(0, 256);
        reload_column.set_retained_light(reload_light.clone());
        let reload_packet = decode_end(&reload_column);
        assert_eq!(
            reload_packet.light, reload_light,
            "initial End encoding must preserve persisted empty/full masks"
        );
        assert_eq!(reload_packet.light.sky(8), &LightData::Missing);
        assert_eq!(reload_packet.light.block(8), &LightData::Missing);
    }

    /// Initial chunk packets omit uniformly dark block-light sections, while a
    /// real emitter still produces a present block-light array. This is the
    /// wire distinction observed in independent chunk captures: an omitted
    /// initial section means there is no prior block-light state, whereas an
    /// explicit zero remains reserved for clearing an existing state in a
    /// light-update packet.
    #[test]
    fn initial_chunk_elides_zero_block_light_but_keeps_emission() {
        use crate::packets::chunk::LevelChunkWithLight;

        let shape = ChunkShape::overworld_1_21();
        let proto = V770ServerProtocol;
        let decode = |column: &ServerChunkColumn| {
            let ServerDirective::Send { payload, .. } =
                ServerProtocol::encode_chunk(&proto, 0, 0, column)
            else {
                panic!("chunk encoder must send a packet");
            };
            let mut reader = Reader::new(&payload);
            let packet = LevelChunkWithLight::decode(&mut reader, &shape).expect("decode chunk");
            reader.ensure_empty().expect("no trailing bytes");
            packet
        };

        let empty = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        let empty_packet = decode(&empty);
        assert!(
            (0..empty_packet.light.light_section_count())
                .all(|section| matches!(empty_packet.light.block(section), LightData::Missing)),
            "all-zero initial block-light sections must be omitted"
        );

        let mut emitted = empty.clone();
        emitted.set_block(8, 0, 8, "minecraft:glowstone");
        let emitted_packet = decode(&emitted);
        assert!(
            (0..emitted_packet.light.light_section_count())
                .any(|section| matches!(emitted_packet.light.block(section), LightData::Values(_))),
            "a block-light emitter must keep a present per-cell array"
        );
        assert!(
            emitted_packet
                .light
                .section_light(5)
                .block_at(8, 0, 8)
                > 0,
            "the emitted cell must carry non-zero block light"
        );
    }

    /// Standalone light updates retain an explicit zero block-light section:
    /// unlike an initial chunk, an update merges into an existing client
    /// column, so the zero is the clear operation rather than an omission.
    #[test]
    fn light_update_preserves_explicit_zero_block_light() {
        let mut light = ColumnLight::new(1);
        *light.block_mut(1) = LightData::Uniform(0);

        let ServerDirective::Send { payload, .. } =
            ServerProtocol::encode_light_update(&V770ServerProtocol, 7, -3, &light)
        else {
            panic!("light update encoder must send a packet");
        };
        let mut reader = Reader::new(&payload);
        assert_eq!(reader.var_i32().expect("chunk x"), 7);
        assert_eq!(reader.var_i32().expect("chunk z"), -3);
        let decoded = ColumnLight::decode(1, &mut reader).expect("decode light update");
        reader.ensure_empty().expect("no trailing bytes");

        assert_eq!(
            *decoded.block(1),
            LightData::Uniform(0),
            "an update's empty block mask must remain an explicit zero"
        );
    }

    /// The island check for real per-quart biome assignment: it must
    /// reach the **encoded wire bytes**, not just [`ServerChunkColumn`] —
    /// the exact chain CLAUDE.md's rule 1 asks for (climate sample -> biome
    /// -> the column the encoder sends -> the actual bytes a client would
    /// decode). Chunk (0, 0) at seed 42 is the same fixture
    /// `biome_matches_vanilla_at_known_coordinates_seed_42`
    /// (`lodestone-server::worldgen_data`) proves against the JVM: quart
    /// (0, 0) is `dark_forest`, quart (2, 2) is `river` — two *different*
    /// biomes in the same chunk, so this also proves the wire encoder
    /// doesn't collapse a chunk to one id the way it used to have to (no
    /// per-quart biome existed before this issue).
    #[test]
    fn encode_chunk_carries_real_per_quart_biome() {
        use crate::packets::chunk::LevelChunkWithLight;
        use lodestone_server::{ChunkSource, overworld_chunk_source};

        let seed: i64 = 42;
        let source = overworld_chunk_source(seed);
        let served_column = source.column(0, 0);
        assert_eq!(served_column.biome_state(0, 0), "minecraft:dark_forest");
        assert_eq!(served_column.biome_state(8, 8), "minecraft:river");
        let dark_forest_id = biome_registry_id("minecraft:dark_forest");
        let river_id = biome_registry_id("minecraft:river");
        assert_ne!(
            dark_forest_id, river_id,
            "fixture sanity: the two biomes must resolve to different wire ids"
        );

        let proto = V770ServerProtocol;
        // Named through the trait: `V770ServerProtocol` implements both
        // `ServerProtocol` and `ChunkEncoder`, whose `encode_chunk` methods are
        // deliberately the same body (see the `ChunkEncoder` impl), so an
        // unqualified call is ambiguous rather than wrong.
        let directive = ServerProtocol::encode_chunk(&proto, 0, 0, &served_column);
        let payload = match directive {
            ServerDirective::Send { payload, .. } => payload,
            other => panic!("expected Send, got {other:?}"),
        };

        let shape = ChunkShape::overworld_1_21();
        let mut r = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut r, &shape).expect("decode column");
        r.ensure_empty().expect("no trailing bytes");

        // `lodestone_world::ChunkColumn::get_biome`'s `x`/`z` are in-chunk
        // **biome cells** (`0..4`, quart resolution), not block coordinates
        // — world block (8, 8) is biome cell (2, 2) (`8 >> 2`). World y=70
        // lands well inside this column's generated terrain range for both
        // probes, and biome is constant across y for a given cell per this
        // port's Phase 1 scope, so the exact y does not matter here.
        assert_eq!(
            decoded.column.get_biome(0, 70, 0),
            dark_forest_id,
            "quart (0,0) must carry dark_forest's real wire id, not a constant default"
        );
        assert_eq!(
            decoded.column.get_biome(2, 70, 2),
            river_id,
            "quart (2,2) must carry river's real wire id, distinct from quart (0,0)'s"
        );
    }

    /// Pins `encode_block_update`'s wire layout end to end: packed `BlockPos`
    /// then a VarInt state id, nothing else — the shape vanilla's own
    /// clientbound block-update packet's own stream codec specifies
    /// (confirmed against the decompiled 26.2 source).
    #[test]
    fn encode_block_update_wire_layout() {
        let proto = V770ServerProtocol;
        let directive = proto.encode_block_update(1, 2, 3, "minecraft:stone");
        match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, play::clientbound::BLOCK_UPDATE);
                let mut r = Reader::new(&payload);
                let packed = r.i64().expect("packed pos");
                assert_eq!(packed, pack_block_pos(1, 2, 3));
                let id = r.var_i32().expect("state id");
                assert_eq!(id as u32, stone_id());
                r.ensure_empty().expect("no trailing bytes");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    /// A moving piston reaches the wire as **two** packets, and the whole point of
    /// this gate is the pair: a `block_update` establishing the `moving_piston`
    /// state and a `block_entity_data` carrying the record that says which block is
    /// travelling. Either alone draws nothing.
    ///
    /// Decoded with the exact sequence `V770Adapter`'s own `BLOCK_ENTITY_DATA` arm
    /// uses — packed i64, VarInt type id, network NBT, `ensure_empty` — so the
    /// expectation for the *layout* comes from the reader that has to consume real
    /// server bytes, not from this encoder. The record's field names and tag types
    /// come from vanilla's own piston moving block-entity class's own save additional and are gated in
    /// `lodestone_server::block_entities`.
    ///
    /// The two packets are asserted in the order the server's own drain emits them
    /// (block-change lane first, effect lane second): reversed, a client applies a
    /// record to a cell whose state is still the *old* block, and
    /// `sync_block_entity` may discard it as a type mismatch.
    #[test]
    fn a_moving_piston_reaches_the_wire_as_a_state_then_a_record() {
        use lodestone_server::piston::{Direction, MovingBlockEntity, moving_piston_state};

        let proto = V770ServerProtocol;
        let entity = MovingBlockEntity::new(
            "minecraft:piston_head[facing=east,short=false,type=sticky]".to_string(),
            Direction::East,
            true,
            true,
        );
        let pos = lodestone_model::BlockPos::new(11, 64, -4);

        // 1. The state. A `moving_piston` must resolve to a real 26.2 state id —
        // a fallback to the default would silently animate the wrong facing.
        let moving = moving_piston_state(Direction::East, true);
        let state_directive = proto.encode_block_update(pos.x, pos.y, pos.z, &moving);
        let ServerDirective::Send {
            packet_id: state_id_packet,
            payload: state_payload,
        } = state_directive
        else {
            panic!("the moving_piston state must reach the wire");
        };
        assert_eq!(state_id_packet, play::clientbound::BLOCK_UPDATE);
        let mut r = Reader::new(&state_payload);
        assert_eq!(r.i64().expect("packed pos"), pack_block_pos(pos.x, pos.y, pos.z));
        let wire_state = r.var_i32().expect("state id") as u32;
        r.ensure_empty().expect("no trailing bytes");
        assert_eq!(
            lodestone_data::block_states::state_id(&moving),
            Some(wire_state),
            "the moving_piston state must resolve exactly, not fall back to a default"
        );

        // 2. The record, through the same dispatch a world tick uses — so this also
        // proves `encode_world_effect` routes the new variant instead of dropping it.
        let effect = lodestone_server::effects::WorldEffect::BlockEntityData {
            pos,
            block_entity_type: lodestone_server::BlockEntityKind::from_name(
                lodestone_server::piston::PISTON_BLOCK_ENTITY,
            ),
            nbt: entity.update_tag(),
        };
        let ServerDirective::Send {
            packet_id: record_packet,
            payload: record_payload,
        } = proto.encode_world_effect(&effect)
        else {
            panic!("the moving piston record must reach the wire");
        };
        assert_eq!(record_packet, play::clientbound::BLOCK_ENTITY_DATA);

        let mut r = Reader::new(&record_payload);
        let packed = r.i64().expect("packed pos");
        assert_eq!(unpack_block_pos(packed), pos);
        let type_id = r.var_i32().expect("type id") as u32;
        assert_eq!(
            lodestone_data::block_entity_types::block_entity_type_name(
                lodestone_data::block_entity_types::BlockEntityType::new(type_id)
                    .expect("wire type validates"),
            ),
            "minecraft:piston",
            "the block's key is `moving_piston` and its entity's key is `piston`; \
             sending the block's would resolve to some other entity"
        );
        let nbt = lodestone_core::read_network_nbt(&mut r).expect("network nbt");
        r.ensure_empty().expect("no trailing bytes");

        let lodestone_core::Nbt::Compound(fields) = &nbt else {
            panic!("the record must be a compound");
        };
        let field = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
                .unwrap_or(lodestone_core::Nbt::End)
        };
        assert_eq!(
            field("facing"),
            lodestone_core::Nbt::Byte(5),
            "`facing` must survive the NBT round trip as a Byte — as an Int a client \
             reads it as absent and animates toward DOWN"
        );
        assert_eq!(field("progress"), lodestone_core::Nbt::Float(0.0));
        assert_eq!(field("extending"), lodestone_core::Nbt::Byte(1));
        assert_eq!(field("source"), lodestone_core::Nbt::Byte(1));

        // 3. And the two together resolve to the head a client draws: the record's
        // own `blockState` must be a real state id, or `PistonHeadRenderer`'s first
        // arm never fires and nothing is drawn at all.
        assert!(
            entity.moved_state.is_some(),
            "the travelling head state must resolve in the 26.2 table"
        );

        // Control: a type key this version does not have must emit nothing rather
        // than a packet carrying a made-up registry id.
        assert!(matches!(
            proto.encode_block_entity_data(pos, "minecraft:not_a_block_entity", &nbt),
            ServerDirective::None
        ));
    }

    /// Pins `encode_game_event`'s wire layout end to end: one unsigned byte
    /// event id, then a big-endian `f32` param, nothing else — the shape
    /// vanilla's own clientbound game-event packet writes (confirmed against
    /// the decompiled 26.2 source)
    /// and the shape this crate's own `GameEvent` decode reads back. The param
    /// is pinned to a non-integral value (`0.5`) so a big-endian `f32` that
    /// somehow slid a byte cannot alias the integer `0`.
    #[test]
    fn encode_game_event_wire_layout() {
        let proto = V770ServerProtocol;
        let directive = proto.encode_game_event(7, 0.5);
        match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, play::clientbound::GAME_EVENT);
                let mut r = Reader::new(&payload);
                assert_eq!(r.u8().expect("event id"), 7);
                assert_eq!(r.f32().expect("param"), 0.5);
                r.ensure_empty().expect("no trailing bytes");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }
}

/// `CHANGE_DIFFICULTY`/`LOCK_DIFFICULTY`/`SET_GAME_RULE` decode
/// and their two confirmation encoders. Expected values come from
/// `.cache/mc/26.2/src`'s own record types
/// (`ServerboundChangeDifficultyPacket`, `ServerboundLockDifficultyPacket`,
/// `ServerboundSetGameRulePacket`, `ClientboundChangeDifficultyPacket`), not
/// from this module's own encoder — each decode test hand-builds wire bytes
/// with the *encode* side of the same struct (a real, if self-authored,
/// round trip through the derive macro) and each encode test independently
/// re-parses the produced bytes field by field instead of comparing structs,
/// so a decode bug and its mirror-image encode bug cannot cancel out.
#[cfg(test)]
mod world_admin_tests {
    use super::*;
    use lodestone_core::State;
    use lodestone_model::Difficulty;

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    #[test]
    fn decode_change_difficulty() {
        let proto = V770ServerProtocol;
        let body = encode(&ChangeDifficultyServerbound { difficulty: 3 });
        let decoded = proto.decode(State::Play, play::serverbound::CHANGE_DIFFICULTY, &body);
        assert_eq!(
            decoded,
            ServerBound::DifficultyChanged {
                difficulty: Difficulty::Hard
            }
        );
    }

    /// Control for [`decode_change_difficulty`]: an ordinal outside `0..=3`
    /// must drop the packet (`ServerBound::Ignored`), not alias to some other
    /// difficulty — see [`difficulty_from_ordinal`]'s own doc comment for why
    /// this departs from vanilla's `WRAP` strategy.
    #[test]
    fn decode_change_difficulty_rejects_out_of_range_ordinal() {
        let proto = V770ServerProtocol;
        let body = encode(&ChangeDifficultyServerbound { difficulty: 9 });
        let decoded = proto.decode(State::Play, play::serverbound::CHANGE_DIFFICULTY, &body);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    #[test]
    fn decode_lock_difficulty() {
        let proto = V770ServerProtocol;
        let body = encode(&LockDifficulty { locked: true });
        let decoded = proto.decode(State::Play, play::serverbound::LOCK_DIFFICULTY, &body);
        assert_eq!(decoded, ServerBound::DifficultyLockChanged { locked: true });
    }

    #[test]
    fn decode_set_game_rule() {
        let proto = V770ServerProtocol;
        let body = encode(&SetGameRule {
            entries: vec![
                GameRuleEntry {
                    key: "minecraft:doDaylightCycle".to_string(),
                    value: "false".to_string(),
                },
                GameRuleEntry {
                    key: "minecraft:randomTickSpeed".to_string(),
                    value: "0".to_string(),
                },
            ],
        });
        let decoded = proto.decode(State::Play, play::serverbound::SET_GAME_RULE, &body);
        assert_eq!(
            decoded,
            ServerBound::GameRuleChanged {
                entries: vec![
                    (
                        "minecraft:doDaylightCycle".to_string(),
                        "false".to_string()
                    ),
                    ("minecraft:randomTickSpeed".to_string(), "0".to_string()),
                ]
            }
        );
    }

    /// Pins `encode_change_difficulty`'s wire layout: VarInt difficulty
    /// ordinal, then a bool locked flag, nothing else
    /// (vanilla's own clientbound change-difficulty packet's own stream codec).
    #[test]
    fn encode_change_difficulty_wire_layout() {
        let proto = V770ServerProtocol;
        let directive = proto.encode_change_difficulty(Difficulty::Hard, true);
        match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, play::clientbound::CHANGE_DIFFICULTY);
                let mut r = Reader::new(&payload);
                assert_eq!(r.var_i32().expect("difficulty"), 3);
                assert!(r.bool().expect("locked"));
                r.ensure_empty().expect("no trailing bytes");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    /// Pins `encode_game_rule_values`'s wire layout: a VarInt-prefixed list
    /// of (string key, string value) pairs, in the order given — and that it
    /// carries exactly the entries passed in, not some full default table
    /// (this crate has none to send).
    #[test]
    fn encode_game_rule_values_wire_layout() {
        let proto = V770ServerProtocol;
        let entries = vec![("minecraft:doDaylightCycle".to_string(), "false".to_string())];
        let directive = proto.encode_game_rule_values(&entries);
        match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, play::clientbound::GAME_RULE_VALUES);
                let decoded = decode_full::<GameRuleValues>(&payload)
                    .expect("well-formed GameRuleValues decodes");
                assert_eq!(decoded.entries.len(), 1);
                assert_eq!(decoded.entries[0].key, "minecraft:doDaylightCycle");
                assert_eq!(decoded.entries[0].value, "false");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }
}

/// `CHAT_COMMAND_SIGNED` — see the decode arm's own comment for why this
/// routes through the same `ServerBound::ChatCommand` consumer as the plain
/// `CHAT_COMMAND` rather than a dedicated variant.
#[cfg(test)]
mod chat_command_signed_tests {
    use super::*;
    use lodestone_core::State;

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    /// Pairwise-distinct fixture: `command`, `timestamp` and `salt` are each
    /// individually distinguishable, and a non-empty `argument_signatures`
    /// list plus a non-zero `last_seen_offset`/`acknowledged`/`checksum` tail
    /// are present precisely so a decoder that stops early (or misreads the
    /// frame length) fails loudly rather than by coincidence.
    #[test]
    fn decode_chat_command_signed_runs_the_same_command_as_the_unsigned_form() {
        let proto = V770ServerProtocol;
        let mut sig_bytes = [0u8; 256];
        for (i, b) in sig_bytes.iter_mut().enumerate() {
            *b = i as u8;
        }
        let body = encode(&ChatCommandSigned {
            command: "gamemode creative Notch".to_owned(),
            timestamp: 1_700_000_000_123,
            salt: 42,
            argument_signatures: vec![crate::packets::game::ArgumentSignatureEntry {
                name: "player".to_owned(),
                signature: crate::packets::game::MessageSignature(sig_bytes),
            }],
            last_seen_offset: 3,
            acknowledged: [0b0000_0001, 0, 0],
            checksum: 7,
        });
        let decoded = proto.decode(State::Play, play::serverbound::CHAT_COMMAND_SIGNED, &body);
        assert_eq!(
            decoded,
            ServerBound::ChatCommand {
                command: "gamemode creative Notch".to_owned(),
            }
        );
    }

    /// Control: a truncated frame (missing the acknowledgement tail) must
    /// drop to `Ignored`, not silently accept a shorter-than-declared packet.
    #[test]
    fn decode_chat_command_signed_rejects_a_truncated_frame() {
        let proto = V770ServerProtocol;
        let mut body = encode(&ChatCommandSigned {
            command: "help".to_owned(),
            timestamp: 1,
            salt: 2,
            argument_signatures: vec![],
            last_seen_offset: 0,
            acknowledged: [0, 0, 0],
            checksum: 0,
        });
        body.truncate(body.len() - 1);
        let decoded = proto.decode(State::Play, play::serverbound::CHAT_COMMAND_SIGNED, &body);
        assert_eq!(decoded, ServerBound::Ignored);
    }
}

/// Server-authoritative inventory: `SET_CARRIED_ITEM`/`CONTAINER_CLICK`
/// decode. Where possible the expected wire bytes come from the **real**
/// client-side encoder (`crate::adapter`'s `V770Adapter::encode_action`),
/// not a hand-authored fixture — this is the same "real client already sends
/// this packet in ordinary singleplayer play" encoder a prior investigation
/// found already existed with zero server-side consumer, so decoding what it
/// actually produces (rather than a bespoke test-only byte layout) is the
/// strongest hermetic evidence available that this module's decoder agrees
/// with production. The two malformed-input controls below (nonzero
/// component-patch counts, an out-of-range hotbar slot) *are* hand-built,
/// deliberately, because the real encoder can never produce them — they are
/// the negative-control class CLAUDE.md's evidence standard asks for, run
/// and watched failing (`ServerBound::Ignored`, not a panic or a
/// misdecoded slot).
#[cfg(test)]
mod inventory_decode_tests {
    use super::*;
    use lodestone_core::State;
    use lodestone_model::{
        ClientAction, ConnectionState, ContainerClickType, ContainerSlotChange, VersionAdapter,
    };

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    fn stack(name: &str, count: u32) -> ItemStack {
        ItemStack::new(name.parse().expect("valid resource key"), count)
    }

    /// The real client's `SetCarriedItem` encoder, decoded back into
    /// [`ServerBound::CarriedItemChanged`].
    #[test]
    fn decode_set_carried_item_from_real_client_encoder() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = crate::adapter()
            .encode_action(ConnectionState::Play, &ClientAction::SetCarriedItem { slot: 4 })
            .expect("encodes")
            .expect("SetCarriedItem always encodes in Play");
        assert_eq!(packet_id, play::serverbound::SET_CARRIED_ITEM);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(decoded, ServerBound::CarriedItemChanged { slot: 4 });
    }

    /// Control: a slot outside `0..HOTBAR_SIZE` must drop the packet
    /// (`ServerBound::Ignored`), never alias into some other hotbar slot —
    /// mirrors `decode_change_difficulty_rejects_out_of_range_ordinal`'s
    /// pattern above. The real client encoder can never produce this (it
    /// only ever selects a real hotbar key), so this is hand-built directly
    /// against the wire struct.
    #[test]
    fn decode_set_carried_item_rejects_out_of_range_slot() {
        let proto = V770ServerProtocol;
        let body = encode(&SetCarriedItem { slot: 9 });
        let decoded = proto.decode(State::Play, play::serverbound::SET_CARRIED_ITEM, &body);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    // ---- `SET_CREATIVE_MODE_SLOT` / `CLIENT_COMMAND`, the two variants that
    // had consumers but no constructor.
    //
    // Every byte below is hand-derived from sources **outside this crate**, so
    // no `decode(encode(x))` symmetry can satisfy them:
    //
    // - Vanilla's own serverbound set-creative-mode-slot packet's composite
    //   stream codec:
    //   its own fixed-width `SHORT` codec then its own untrusted-optional
    //   item-stack stream codec.
    //   The `SHORT` codec is a plain big-endian `i16` write.
    // - Vanilla's own serverbound client-command packet's whole body is one
    //   plain enum-ordinal write, i.e. a VarInt of the ordinal, over
    //   `Action { PERFORM_RESPAWN, REQUEST_STATS, REQUEST_GAMERULE_VALUES }`.
    // - `minecraft:cobblestone`'s item protocol id `62` is read from Mojang's
    //   own `generated/reports/registries.json`, the authoritative generator
    //   output — not from our registry tables.
    // - The menu-slot number `36` is vanilla's own inventory-menu's first
    //   hotbar slot,
    //   which vanilla's own server-side set-creative-mode-slot handler
    //   accepts as a valid slot (`1..=45`) and writes via that menu's own
    //   slot lookup.

    /// A creative-mode palette write of a full stack into the first hotbar
    /// slot, decoded from bytes laid out by hand against vanilla's own
    /// `STREAM_CODEC` (see the block comment above for every byte's source).
    ///
    /// This arm returned [`ServerBound::Ignored`] until a wiring
    /// pass fixed it, while `apply_creative_mode_slot_set` and
    /// `ServerBound::CreativeModeSlotSet` had both already existed since
    /// `c4ad474` — so a real client's entire creative inventory was silently
    /// discarded. `tests/serverbound_wiring.rs` now gates that class
    /// structurally; this gates the wire layout.
    #[test]
    fn decode_set_creative_mode_slot_from_hand_built_vanilla_bytes() {
        let proto = V770ServerProtocol;
        let body = [
            0x00, 0x24, // vanilla's own fixed-width SHORT codec: big-endian i16 36
            0x40, // optional-stack count, VarInt 64
            0x3E, // item id, VarInt 62 = minecraft:cobblestone
            0x00, // added components, VarInt 0
            0x00, // removed components, VarInt 0
        ];
        let decoded = proto.decode(State::Play, play::serverbound::SET_CREATIVE_MODE_SLOT, &body);
        assert_eq!(
            decoded,
            ServerBound::CreativeModeSlotSet {
                slot: 36,
                item: Some(stack("minecraft:cobblestone", 64)),
            }
        );
    }

    /// The clear-a-slot case: vanilla's own item-stack type's own create optional stream codec uses a
    /// `count` of zero as the absence marker rather than a leading presence
    /// bool (see [`read_optional_item_stack`]'s doc comment), so an empty
    /// write is three bytes with no item id at all. A decoder that expected a
    /// bool prefix here would read the `0x00` count as "absent" and then
    /// choke on `ensure_empty`, or read a spurious id — this pins the real
    /// shape.
    #[test]
    fn decode_set_creative_mode_slot_clears_a_slot_with_a_zero_count() {
        let proto = V770ServerProtocol;
        let body = [0x00, 0x2D, 0x00]; // slot 45 (off-hand), count 0 = empty
        let decoded = proto.decode(State::Play, play::serverbound::SET_CREATIVE_MODE_SLOT, &body);
        assert_eq!(decoded, ServerBound::CreativeModeSlotSet { slot: 45, item: None });
    }

    /// Vanilla's `slotNum() < 0` "drop into the world" case, which this crate
    /// has no model for. The variant must still carry the raw negative slot
    /// rather than the decoder swallowing the packet, because the decision to
    /// drop it belongs to the consumer — `apply_creative_mode_slot_set`
    /// no-ops on any slot `player_menu_native_index` does not recognise, and
    /// its doc comment says so.
    ///
    /// `-1` as big-endian `i16` is `0xFF 0xFF`.
    #[test]
    fn decode_set_creative_mode_slot_preserves_vanillas_negative_drop_slot() {
        let proto = V770ServerProtocol;
        let body = [0xFF, 0xFF, 0x01, 0x3E, 0x00, 0x00];
        let decoded = proto.decode(State::Play, play::serverbound::SET_CREATIVE_MODE_SLOT, &body);
        assert_eq!(
            decoded,
            ServerBound::CreativeModeSlotSet {
                slot: -1,
                item: Some(stack("minecraft:cobblestone", 1)),
            }
        );
    }

    /// Control for the three gates above: the detector must reject a payload
    /// with a trailing byte rather than accepting a prefix.
    ///
    /// Without this, a decoder that stopped reading early would satisfy every
    /// positive assertion above while misaligning any future field, and the
    /// `ensure_empty` in the arm would be unproven. Observed to fail when
    /// `ensure_empty` is removed from the arm.
    #[test]
    fn decode_set_creative_mode_slot_rejects_a_trailing_byte() {
        let proto = V770ServerProtocol;
        let body = [0x00, 0x24, 0x40, 0x3E, 0x00, 0x00, 0x99];
        let decoded = proto.decode(State::Play, play::serverbound::SET_CREATIVE_MODE_SLOT, &body);
        assert_eq!(
            decoded,
            ServerBound::Ignored,
            "a trailing byte must fail the whole decode; if this passes, the positive gates \
             above prove nothing about field alignment"
        );
    }

    /// `PERFORM_RESPAWN`, ordinal `0` — the packet a real client sends when
    /// the player clicks **Respawn** on the death screen.
    ///
    /// This arm returned [`ServerBound::Ignored`] until a wiring
    /// pass fixed it, while `apply_client_command`'s respawn path already existed, so
    /// the button did nothing on a `lodestone` server. That is not a cosmetic
    /// gap: per `CLAUDE.md`'s live-server hazards a dead player is held on the
    /// death screen and is sent **no chunks**, so the connection became a
    /// permanent silent chunk blackout with keep-alives still flowing.
    #[test]
    fn decode_client_command_perform_respawn_from_hand_built_vanilla_bytes() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::CLIENT_COMMAND, &[0x00]);
        assert_eq!(decoded, ServerBound::ClientCommand { action: 0 });
    }

    /// `REQUEST_GAMERULE_VALUES`, ordinal `2`. The ordinal is passed through
    /// unmapped, so this also proves the arm does not collapse distinct
    /// actions onto one value — a decoder that hardcoded `action: 0` would
    /// pass the respawn gate above and fail here.
    #[test]
    fn decode_client_command_distinguishes_the_gamerule_request_ordinal() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::CLIENT_COMMAND, &[0x02]);
        assert_eq!(decoded, ServerBound::ClientCommand { action: 2 });
        // `REQUEST_STATS`, ordinal 1 — decoded and passed through even though
        // the consumer documents it as a no-op (no stats model in this crate).
        let decoded = proto.decode(State::Play, play::serverbound::CLIENT_COMMAND, &[0x01]);
        assert_eq!(decoded, ServerBound::ClientCommand { action: 1 });
    }

    /// Control: an empty `client_command` body carries no ordinal at all and
    /// must not decode to a plausible-looking `action: 0`, which would make
    /// the respawn gate above satisfiable by a decoder that read nothing.
    #[test]
    fn decode_client_command_rejects_an_empty_body() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::CLIENT_COMMAND, &[]);
        assert_eq!(
            decoded,
            ServerBound::Ignored,
            "an empty body must not produce `action: 0`; if it does, the respawn gate is \
             satisfied by a decoder that never reads the wire"
        );
    }

    /// The real client's `ContainerClick` encoder (a hotbar-swap style
    /// click predicting one changed slot and an empty cursor), decoded back
    /// into [`ServerBound::ContainerClicked`] — the packet's changed-slots
    /// map, which is what this crate's consumer actually applies, survives
    /// the round trip through the real production wire layout.
    #[test]
    fn decode_container_click_from_real_client_encoder() {
        let proto = V770ServerProtocol;
        let action = ClientAction::ContainerClick {
            window_id: 0,
            state_id: lodestone_model::ContainerStateId::new(7),
            slot: 36,
            button: 0,
            click_type: ContainerClickType::Pickup,
            changed_slots: vec![ContainerSlotChange {
                slot: 36,
                item: Some(stack("minecraft:diamond_pickaxe", 1)),
            }],
            carried_item: None,
        };
        let (packet_id, payload) = crate::adapter()
            .encode_action(ConnectionState::Play, &action)
            .expect("encodes")
            .expect("ContainerClick always encodes in Play");
        assert_eq!(packet_id, play::serverbound::CONTAINER_CLICK);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(
            decoded,
            ServerBound::ContainerClicked {
                window_id: 0,
                state_id: 7,
                slot: 36,
                button: 0,
                click_type: 0,
                changed_slots: vec![(36, Some(stack("minecraft:diamond_pickaxe", 1)))],
                carried_item: None,
            }
        );
    }

    /// The same real-encoder round trip, but with a non-empty carried
    /// (cursor) stack and two changed slots — proves the loop over multiple
    /// entries and the trailing carried-item read both land correctly, not
    /// just the single-entry case above.
    #[test]
    fn decode_container_click_carries_cursor_stack_and_multiple_changes() {
        let proto = V770ServerProtocol;
        let action = ClientAction::ContainerClick {
            window_id: 0,
            state_id: lodestone_model::ContainerStateId::new(12),
            slot: 9,
            button: 0,
            click_type: ContainerClickType::Swap,
            changed_slots: vec![
                ContainerSlotChange {
                    slot: 9,
                    item: Some(stack("minecraft:cobblestone", 32)),
                },
                ContainerSlotChange { slot: 40, item: None },
            ],
            carried_item: Some(stack("minecraft:torch", 16)),
        };
        let (packet_id, payload) = crate::adapter()
            .encode_action(ConnectionState::Play, &action)
            .expect("encodes")
            .expect("ContainerClick always encodes in Play");
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(
            decoded,
            ServerBound::ContainerClicked {
                window_id: 0,
                state_id: 12,
                slot: 9,
                // `ContainerClickType::Swap` is ordinal 2 — the whole point of
                // carrying these three now, so they are asserted rather than
                // wildcarded.
                button: 0,
                click_type: 2,
                changed_slots: vec![
                    (9, Some(stack("minecraft:cobblestone", 32))),
                    (40, None),
                ],
                carried_item: Some(stack("minecraft:torch", 16)),
            }
        );
    }

    /// Control: a `HashedStack` carrying a nonzero added-component count is
    /// something the real client encoder can never produce (it always
    /// writes `0`/`0`, `write_hashed_stack`'s own doc comment), so this is
    /// hand-built directly against the documented wire layout — a container
    /// id, state id, slot, button, click type, one changed-slot entry whose
    /// item claims one added component. [`read_hashed_stack`]'s guard must
    /// fail the *whole* decode rather than silently misalign the reader on
    /// the (nonexistent, in this crate) per-component bytes that would
    /// follow.
    #[test]
    fn decode_container_click_rejects_nonzero_component_patch() {
        let proto = V770ServerProtocol;
        let mut w = Writer::default();
        w.var_i32(0); // window id
        w.var_i32(0); // state id
        w.i16(0); // slot
        w.i8(0); // button
        w.var_i32(0); // click type: pickup
        w.var_i32(1); // one changed slot
        w.i16(9); // slot 9
        w.bool(true); // present
        w.var_i32(0); // item id 0 (whatever it resolves to; irrelevant, decode must fail first)
        w.var_i32(1); // count
        w.var_i32(1); // added components: nonzero
        w.var_i32(0); // removed components
        w.bool(false); // carried item: empty
        let body = w.into_vec();
        let decoded = proto.decode(State::Play, play::serverbound::CONTAINER_CLICK, &body);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// Control: a truncated payload (claims one changed slot but supplies no
    /// bytes for it) must drop the packet, not panic or read garbage.
    #[test]
    fn decode_container_click_rejects_truncated_payload() {
        let proto = V770ServerProtocol;
        let mut w = Writer::default();
        w.var_i32(0);
        w.var_i32(0);
        w.i16(0);
        w.i8(0);
        w.var_i32(0);
        w.var_i32(1); // claims one changed slot, but the packet ends here
        let body = w.into_vec();
        let decoded = proto.decode(State::Play, play::serverbound::CONTAINER_CLICK, &body);
        assert_eq!(decoded, ServerBound::Ignored);
    }
}

/// Decode tests for `minecraft:attack` and
/// `minecraft:player_input`, the two packets the melee-damage/knockback
/// pipeline depends on.
#[cfg(test)]
mod combat_decode_tests {
    use super::*;
    use lodestone_core::State;
    use lodestone_model::{
        ClientAction, ConnectionState, EntityInteraction, Hand, PlayerInput, VersionAdapter,
    };

    /// Round-trips through the **real client encoder**
    /// (`ClientAction::InteractEntity { interaction: EntityInteraction::Attack,
    /// .. }`) rather than hand-building the wire body — the same "prove the
    /// two sides actually agree" style `decode_set_carried_item_from_real_
    /// client_encoder` already established, and the one that matters most
    /// here: `Sim::attack_entity` (`lodestone-shell`) already sends exactly
    /// this action in production (`docs/combat.md`'s "Sending the attack"
    /// section) — this is the decode side finally meeting a real producer.
    #[test]
    fn decode_attack_from_the_real_client_encoder() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = crate::adapter()
            .encode_action(
                ConnectionState::Play,
                &ClientAction::InteractEntity {
                    entity_id: 1234,
                    interaction: EntityInteraction::Attack,
                    sneaking: true, // must be dropped — the wire body carries no such bit.
                },
            )
            .expect("encodes")
            .expect("Attack always encodes in Play");
        assert_eq!(packet_id, play::serverbound::ATTACK);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(decoded, ServerBound::Attack { entity_id: 1234 });
    }

    /// Control: a malformed/truncated `Attack` payload must drop the packet,
    /// not panic.
    #[test]
    fn decode_attack_rejects_a_truncated_payload() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::ATTACK, &[]);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// `minecraft:interact` (a plain `Interact`, not `Attack`) now decodes into
    /// [`ServerBound::InteractEntity`], through the **real client encoder**.
    ///
    /// This test used to assert `Ignored`, with a doc comment saying the variant was
    /// deliberately absent because *"this crate has no interaction model"* — and it
    /// asked whoever added taming to change it rather than discover a gap. That is
    /// what this is.
    ///
    /// The expected value comes from the other side of the seam: `V770Adapter`'s own
    /// `encode_action` builds the payload, so nothing here restates the field order.
    ///
    /// Values are **pairwise distinct** so the two adjacent VarInts cannot transpose
    /// unnoticed: entity `1234`, hand `1` (off hand), and `sneaking: true` — the
    /// trailing boolean set deliberately *different* from what a default fixture
    /// would carry, because two adjacent booleans (or a boolean and a defaulted
    /// field) coincide half the time by chance and a fixture that sets them equal
    /// cannot see a swap at all.
    #[test]
    fn decode_plain_interact_reaches_the_interact_entity_variant() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = crate::adapter()
            .encode_action(
                ConnectionState::Play,
                &ClientAction::InteractEntity {
                    entity_id: 1234,
                    interaction: EntityInteraction::Interact { hand: Hand::Off },
                    sneaking: true,
                },
            )
            .expect("encodes")
            .expect("Interact always encodes in Play");
        assert_eq!(packet_id, play::serverbound::INTERACT);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(
            decoded,
            ServerBound::InteractEntity {
                entity_id: 1234,
                hand: 1,
                using_secondary_action: true,
            },
            "the right-click half must reach MobSim::interact; `Ignored` here is \
             what made a real client's right-click on a wolf do nothing"
        );
    }

    /// Control: a truncated `interact` payload must drop the packet rather than
    /// panic or produce a half-decoded variant.
    ///
    /// Without this, the `unwrap_or(Ignored)` in the decode arm is an untested
    /// branch — and it is the branch that stands between a malformed frame and a
    /// `MobSim::interact` call against a garbage entity id.
    #[test]
    fn decode_interact_rejects_a_truncated_payload() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::INTERACT, &[0x01]);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// Round-trips through the real client encoder: `sprint`, `shift` and
    /// `jump` survive, bit-identical, out the other side; the other four
    /// `Input` flags are decoded off the wire (so a malformed byte still
    /// fails cleanly) but do not appear in `ServerBound::PlayerInput` — see
    /// that variant's own doc comment for why. One-hot across the three
    /// arms (exactly one of `jump`/`shift`/`sprint` true per arm, the other
    /// two false) so a transposition of any adjacent pair of the three bits
    /// (`0x10`/`0x20`/`0x20`/`0x40`) cannot survive this round trip.
    #[test]
    fn decode_player_input_jump_sprint_and_shift_from_the_real_client_encoder() {
        let proto = V770ServerProtocol;
        for (jump, shift, sprint) in [(true, false, false), (false, true, false), (false, false, true)] {
            let (packet_id, payload) = crate::adapter()
                .encode_action(
                    ConnectionState::Play,
                    &ClientAction::SetPlayerInput(PlayerInput {
                        forward: true,
                        backward: false,
                        left: false,
                        right: false,
                        jump,
                        shift,
                        sprint,
                    }),
                )
                .expect("encodes")
                .expect("SetPlayerInput always encodes in Play");
            assert_eq!(packet_id, play::serverbound::PLAYER_INPUT);
            let decoded = proto.decode(State::Play, packet_id, &payload);
            assert_eq!(
                decoded,
                ServerBound::PlayerInput { sprint, shift, jump },
                "sprint={sprint} shift={shift} jump={jump}"
            );
        }
    }

    /// Control: an empty payload must drop the packet, not panic on the
    /// missing flags byte.
    #[test]
    fn decode_player_input_rejects_an_empty_payload() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[]);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// Sanity check on the bit layout itself, independent of the real
    /// encoder: bit `0x40` alone must decode to `sprint: true` (the other two
    /// false), `0x20` alone to `shift: true`, and `0x10` alone to
    /// `jump: true`, so a future change to `ServerBound::PlayerInput`'s
    /// fields can be checked against a known byte, not only against the
    /// adapter's own (also-changeable) encoder. Covers a transposition of
    /// any of the three adjacent bits, not just their presence.
    #[test]
    fn decode_player_input_bit_layout_pins_sprint_at_0x40_shift_at_0x20_jump_at_0x10() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[0x40]);
        assert_eq!(
            decoded,
            ServerBound::PlayerInput {
                sprint: true,
                shift: false,
                jump: false,
            }
        );
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[0x20]);
        assert_eq!(
            decoded,
            ServerBound::PlayerInput {
                sprint: false,
                shift: true,
                jump: false,
            }
        );
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[0x10]);
        assert_eq!(
            decoded,
            ServerBound::PlayerInput {
                sprint: false,
                shift: false,
                jump: true,
            }
        );
        // forward|backward|left|right — none of the three modelled flags.
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[0x0F]);
        assert_eq!(
            decoded,
            ServerBound::PlayerInput {
                sprint: false,
                shift: false,
                jump: false,
            }
        );
    }
}

/// Regression coverage for the chunk-streaming investigation's bug
/// (see the doc comment on the `CLIENT_INFORMATION`/`CHUNK_BATCH_RECEIVED`
/// decode arms above): both packet ids used to hit the generic
/// decode-then-drop `Ignored` family from before this crate had any
/// consumer for either, and a later fix added
/// `ServerBound::ClientInformationChanged`/`ChunkBatchAcknowledged` plus
/// `crate::server`'s consumers without ever updating this decode arm to
/// construct them — so both variants were dead code, and every
/// view-streaming chunk batch after the connection's first queued behind a
/// permanently-`true` `awaiting_chunk_batch_ack` and was never flushed.
/// `cargo test -p lodestone-v26-2 --test block_edit -- \
/// dig_and_place_persist_through_forget_and_reload` reproduced this at
/// committed `main` before the fix (a real player walking back into a
/// forgotten chunk never got it re-sent) and passes after it.
#[cfg(test)]
mod view_streaming_decode_tests {
    use super::*;
    use lodestone_core::State;
    use lodestone_model::{
        ChatMode, ClientAction, ClientSettings, ConnectionState, DisplayedSkinParts, MainHand,
        Directive, ParticleStatus, VersionAdapter,
    };
    use crate::packets::game::ChunkBatchFinished;
    use lodestone_world::World;

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    /// The real client's `SetClientSettings` encoder (the same one a
    /// render-distance change in the shell's settings screen would send),
    /// decoded back into [`ServerBound::ClientInformationChanged`]. Before
    /// the fix this decoded to `ServerBound::Ignored` unconditionally, so
    /// `crate::server`'s `ViewTracker::set_view_radius` consumer was never
    /// reached by a real client no matter what it sent.
    #[test]
    fn decode_client_information_changed_from_real_client_encoder() {
        let proto = V770ServerProtocol;
        let settings = ClientSettings {
            locale: "en_us".to_owned(),
            view_distance: 12,
            chat_mode: ChatMode::Full,
            chat_colors: true,
            skin_parts: DisplayedSkinParts {
                cape: false,
                jacket: false,
                left_sleeve: false,
                right_sleeve: false,
                left_pants_leg: false,
                right_pants_leg: false,
                hat: false,
            },
            main_hand: MainHand::Right,
            text_filtering: false,
            allow_server_listing: true,
            particle_status: ParticleStatus::All,
        };
        let (packet_id, payload) = crate::adapter()
            .encode_action(ConnectionState::Play, &ClientAction::SetClientSettings(settings))
            .expect("encodes")
            .expect("SetClientSettings always encodes in Play");
        assert_eq!(packet_id, play::serverbound::CLIENT_INFORMATION);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(decoded, ServerBound::ClientInformationChanged { view_distance: 12 });
    }

    /// Control: a malformed payload must still drop the packet rather than
    /// panic on the missing fields.
    #[test]
    fn decode_client_information_changed_rejects_a_truncated_payload() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::CLIENT_INFORMATION, &[]);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// Chains the real *client* chunk-batch-flow-control reply — produced by
    /// feeding a genuine clientbound `CHUNK_BATCH_FINISHED` through the real
    /// `V770Adapter::handle_packet` (the same code path
    /// `crate::server`'s own connection loop drives, per this module's own
    /// `CHUNK_BATCH_START`/`CHUNK_BATCH_FINISHED` handling) — into this
    /// module's server-side decoder. This proves the whole
    /// server-sends-a-batch / client-acks-it / server-reads-the-ack loop
    /// closes through two independently-written, real production
    /// encode/decode paths, not a hand-rolled fixture on either end.
    #[test]
    fn decode_chunk_batch_acknowledged_from_the_real_client_adapter() {
        let finished_body = encode(&ChunkBatchFinished { batch_size: 7 });
        let mut world = World::new();
        let directives = crate::adapter()
            .handle_packet(
                &mut world,
                ConnectionState::Play,
                play::clientbound::CHUNK_BATCH_FINISHED,
                &finished_body,
            )
            .expect("a real client must accept its own CHUNK_BATCH_FINISHED body");
        let (ack_packet_id, ack_payload) = directives
            .into_iter()
            .find_map(|d| match d {
                Directive::Send { packet_id, payload } => Some((packet_id, payload)),
                _ => None,
            })
            .expect("CHUNK_BATCH_FINISHED must produce a CHUNK_BATCH_RECEIVED reply");
        assert_eq!(ack_packet_id, play::serverbound::CHUNK_BATCH_RECEIVED);

        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, ack_packet_id, &ack_payload);
        match decoded {
            ServerBound::ChunkBatchAcknowledged { desired_chunks_per_tick } => {
                assert!(
                    desired_chunks_per_tick > 0.0,
                    "a real client's desired rate must be positive, got {desired_chunks_per_tick}"
                );
            }
            other => panic!(
                "expected ServerBound::ChunkBatchAcknowledged (this is exactly the variant that \
                 was dead code before the fix — see this module's own doc comment), got {other:?}"
            ),
        }
    }

    /// Pins the exact numeric field against a hand-built payload too,
    /// independent of whatever rate-estimation formula the real client picks
    /// — a future change to that formula should not silently stop this test
    /// from noticing a decode regression.
    #[test]
    fn decode_chunk_batch_acknowledged_bit_layout() {
        let proto = V770ServerProtocol;
        let body = encode(&ChunkBatchReceived {
            desired_chunks_per_tick: 32.0,
        });
        let decoded = proto.decode(State::Play, play::serverbound::CHUNK_BATCH_RECEIVED, &body);
        assert_eq!(
            decoded,
            ServerBound::ChunkBatchAcknowledged { desired_chunks_per_tick: 32.0 }
        );
    }

    /// Control: a malformed payload must still drop the packet rather than
    /// panic.
    #[test]
    fn decode_chunk_batch_acknowledged_rejects_a_truncated_payload() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::CHUNK_BATCH_RECEIVED, &[]);
        assert_eq!(decoded, ServerBound::Ignored);
    }
}

/// Encode-side wire layouts for the six world-border packets.
///
/// Each test drives [`V770ServerProtocol`]'s `encode_*` and re-parses the
/// produced bytes field by field against the vanilla field order, instead of
/// comparing structs — so an encoder bug and a mirror-image decode bug in the
/// same derive cannot cancel out (the decode side of these packets is pinned
/// independently in `crates/versions/26.2/tests/world_border.rs`).
#[cfg(test)]
mod border_wire_tests {
    use super::*;

    fn unwrap_send(directive: ServerDirective) -> (i32, Vec<u8>) {
        match directive {
            ServerDirective::Send { packet_id, payload } => (packet_id, payload),
            other => panic!("expected Send, got {other:?}"),
        }
    }

    /// The join broadcast (`encode_initialize_border`), against the field
    /// order of vanilla's own clientbound initialize-border packet's own write: two `f64` centre
    /// coords, `old_size`, `new_size`, then VarLong lerp time and three
    /// VarInts. A static border carries `old_size == new_size` and lerp time
    /// `0`.
    #[test]
    fn encode_initialize_border_wire_layout() {
        let proto = V770ServerProtocol;
        let mut border = WorldBorder::default();
        border.set_center(10.0, -10.0);
        border.set_size(1000.0);
        border.set_warning_blocks(10);
        border.set_warning_time(20);
        let (packet_id, payload) = unwrap_send(proto.encode_initialize_border(&border));
        assert_eq!(packet_id, play::clientbound::INITIALIZE_BORDER);
        let mut r = Reader::new(&payload);
        assert_eq!(r.f64().expect("center_x"), 10.0);
        assert_eq!(r.f64().expect("center_z"), -10.0);
        assert_eq!(r.f64().expect("old_size"), 1000.0);
        assert_eq!(r.f64().expect("new_size"), 1000.0, "static border targets its own size");
        assert_eq!(r.var_i64().expect("lerp_time"), 0, "static border has no lerp");
        assert_eq!(r.var_i32().expect("absolute_max_size"), ABSOLUTE_MAX_SIZE);
        assert_eq!(r.var_i32().expect("warning_blocks"), 10);
        assert_eq!(r.var_i32().expect("warning_time"), 20);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// A mid-lerp join must carry the *remaining* time converted from ticks to
    /// the milliseconds the lodestone client interpolates on (this crate's
    /// deliberate divergence from vanilla's raw ticks — see
    /// [`InitializeBorder`]'s packet doc).
    #[test]
    fn encode_initialize_border_converts_remaining_ticks_to_ms() {
        let proto = V770ServerProtocol;
        let mut border = WorldBorder::default();
        border.lerp_size_between(500.0, 100.0, 200, 0); // 200 ticks remaining
        let (_, payload) = unwrap_send(proto.encode_initialize_border(&border));
        let mut r = Reader::new(&payload);
        let _ = r.f64().expect("center_x");
        let _ = r.f64().expect("center_z");
        assert_eq!(r.f64().expect("old_size"), 500.0, "the lerp's start size");
        assert_eq!(r.f64().expect("new_size"), 100.0, "the lerp's target");
        assert_eq!(
            r.var_i64().expect("lerp_time"),
            200 * 50,
            "remaining ticks are broadcast as milliseconds"
        );
    }

    /// `encode_set_border_center`: two big-endian `f64` coords, nothing else
    /// (`ClientboundSetBorderCenterPacket`).
    #[test]
    fn encode_set_border_center_wire_layout() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = unwrap_send(proto.encode_set_border_center(100.5, -200.25));
        assert_eq!(packet_id, play::clientbound::SET_BORDER_CENTER);
        let mut r = Reader::new(&payload);
        assert_eq!(r.f64().expect("center_x"), 100.5);
        assert_eq!(r.f64().expect("center_z"), -200.25);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// `encode_set_border_lerp_size`: `old_size`, `new_size`, then a VarLong
    /// lerp time in milliseconds (verbatim — this encoder is the last hop).
    #[test]
    fn encode_set_border_lerp_size_wire_layout() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = unwrap_send(proto.encode_set_border_lerp_size(200.0, 100.0, 30_000));
        assert_eq!(packet_id, play::clientbound::SET_BORDER_LERP_SIZE);
        let mut r = Reader::new(&payload);
        assert_eq!(r.f64().expect("old_size"), 200.0);
        assert_eq!(r.f64().expect("new_size"), 100.0);
        assert_eq!(r.var_i64().expect("lerp_time_ms"), 30_000);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// `encode_set_border_size`: a single big-endian `f64`
    /// (`ClientboundSetBorderSizePacket`).
    #[test]
    fn encode_set_border_size_wire_layout() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = unwrap_send(proto.encode_set_border_size(60_000_000.0));
        assert_eq!(packet_id, play::clientbound::SET_BORDER_SIZE);
        let mut r = Reader::new(&payload);
        assert_eq!(r.f64().expect("size"), 60_000_000.0);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// `encode_set_border_warning_delay`: a single VarInt seconds value.
    #[test]
    fn encode_set_border_warning_delay_wire_layout() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = unwrap_send(proto.encode_set_border_warning_delay(15));
        assert_eq!(packet_id, play::clientbound::SET_BORDER_WARNING_DELAY);
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("warning_time"), 15);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// `encode_set_border_warning_distance`: a single VarInt blocks value.
    #[test]
    fn encode_set_border_warning_distance_wire_layout() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = unwrap_send(proto.encode_set_border_warning_distance(5));
        assert_eq!(packet_id, play::clientbound::SET_BORDER_WARNING_DISTANCE);
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("warning_blocks"), 5);
        r.ensure_empty().expect("no trailing bytes");
    }
}

#[cfg(test)]
mod vehicle_wire_tests {
    use super::*;
    use lodestone_core::State;

    /// vanilla's own clientbound set-passengers packet's own write — a VarInt vehicle id then
    /// `writeVarIntArray`.
    ///
    /// The ids are **pairwise distinct and none is a small ordinal** (`517`, `41`,
    /// `9`), which is what makes a transposition of the vehicle and its first
    /// passenger fail: the two are adjacent VarInts of the same type, so
    /// `decode(encode(x))` through this crate's own pair is byte-perfect either way
    /// and the visible symptom would be a boat riding a player.
    ///
    /// The length prefix is asserted separately from the elements for the same
    /// reason: `writeVarIntArray` is not vanilla's own codec library's own var-int accessor.apply(list())`, and
    /// the two are only accidentally the same bytes.
    #[test]
    fn set_passengers_writes_the_vehicle_then_a_varint_array() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { packet_id, payload } =
            proto.encode_set_passengers(517, &[41, 9])
        else {
            panic!("a passenger list must be sent");
        };
        assert_eq!(packet_id, play::clientbound::SET_PASSENGERS);
        let mut r = Reader::new(&payload);
        let mut wrong = Vec::new();
        if r.var_i32().expect("vehicle id") != 517 {
            wrong.push("the vehicle id must come first");
        }
        if r.var_i32().expect("count") != 2 {
            wrong.push("then the array length");
        }
        if r.var_i32().expect("first passenger") != 41 {
            wrong.push("then the passengers, in order");
        }
        if r.var_i32().expect("second passenger") != 9 {
            wrong.push("both of them");
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        r.ensure_empty().expect("no trailing bytes");

        // **The dismount frame.** Vanilla announces a dismount as the same packet
        // with an empty list, and this client folds exactly that into "we got out"
        // (`lodestone_ecs::session`'s `Riding` fold). A zero-length array is
        // therefore a meaningful frame, not a degenerate one, and an encoder that
        // declined to send it would leave a dismounted player stuck in a seat.
        let ServerDirective::Send { payload: empty, .. } = proto.encode_set_passengers(517, &[])
        else {
            panic!("an empty list is still a real packet");
        };
        let mut r = Reader::new(&empty);
        assert_eq!(r.var_i32().expect("vehicle id"), 517);
        assert_eq!(r.var_i32().expect("count"), 0);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// `ServerboundMoveVehiclePacket` decodes into a real variant now that the
    /// server has a vehicle to apply it to.
    ///
    /// Every field value is distinct and none is a round number, because the packet
    /// is three `f64`s followed by two `f32`s: any transposition inside either run
    /// is wire-legal and survives a round trip through our own codec. `-40.25` for
    /// pitch versus `137.5` for yaw also separates them by *sign*, so swapping the
    /// pair is visible rather than merely numerically different.
    ///
    /// The fixture is built with the packet struct's own encoder rather than by
    /// hand, so this gate is about the **lift** (that `MOVE_VEHICLE` reaches
    /// `ServerBound::VehicleMoved` rather than `Ignored`); the byte layout itself is
    /// pinned by `crate::packets::game`'s own round-trip gates.
    #[test]
    fn move_vehicle_lifts_into_a_variant_rather_than_being_ignored() {
        let body = MoveVehicle {
            x: 118.5,
            y: 63.25,
            z: -204.75,
            yaw: 137.5,
            pitch: -40.25,
            on_ground: true,
        };
        let payload = encode_body(&body);
        let decoded = V770ServerProtocol.decode(
            State::Play,
            play::serverbound::MOVE_VEHICLE,
            &payload,
        );
        assert_eq!(
            decoded,
            ServerBound::VehicleMoved {
                position: Vec3::new(118.5, 63.25, -204.75),
                yaw: 137.5,
                pitch: -40.25,
            }
        );
        // A truncated frame must be `Ignored`, not a partially-read transform: a
        // half-decoded position would teleport the boat.
        assert_eq!(
            V770ServerProtocol.decode(
                State::Play,
                play::serverbound::MOVE_VEHICLE,
                &payload[..payload.len() - 3],
            ),
            ServerBound::Ignored
        );
    }
}

#[cfg(test)]
mod dimension_wire_tests {
    use super::*;

    /// The holder-id mapping is `DIMENSION_TYPE_REGISTRY`'s order, and the Nether is
    /// **3**, not 1 — `overworld_caves` and `the_end` sit between them.
    ///
    /// The expectation comes from the registry table this crate publishes, and the
    /// negative arm is what makes it a test rather than a restatement: an
    /// unrecognised key must be `None`, because guessing a holder id reframes every
    /// subsequent chunk against the wrong build height.
    #[test]
    fn dimension_type_holder_ids_follow_the_published_registry_order() {
        assert_eq!(dimension_type_holder_id("minecraft:overworld"), Some(0));
        assert_eq!(dimension_type_holder_id("minecraft:overworld_caves"), Some(1));
        assert_eq!(dimension_type_holder_id("minecraft:the_end"), Some(2));
        assert_eq!(dimension_type_holder_id("minecraft:the_nether"), Some(3));
        assert_eq!(dimension_type_holder_id("mypack:mine"), None);
    }

    /// `encode_dimension_change` carries `KEEP_ALL_DATA` and the destination's own
    /// `sea_level`, and emits **nothing** for a dimension this server's registry does
    /// not publish.
    ///
    /// The `data_to_keep` byte is the one field that separates this from
    /// `encode_respawn`: `0` there makes the client rebuild its player state, which
    /// for a portal trip would empty the inventory. Asserting the *byte* rather than
    /// "a respawn was sent" is what makes that checkable.
    #[test]
    fn a_dimension_change_keeps_player_data_and_declines_an_unknown_level() {
        let proto = V770ServerProtocol;
        let directives = proto.encode_dimension_change(
            "minecraft:the_nether",
            Vec3::new(215.5, 96.0, -65.5),
            GameMode::Survival,
        );
        assert_eq!(directives.len(), 2, "the respawn record, then the teleport");
        let ServerDirective::Send { packet_id, payload } = &directives[0] else {
            panic!("expected a Send, got {:?}", directives[0]);
        };
        assert_eq!(*packet_id, play::clientbound::RESPAWN);
        let mut r = Reader::new(payload);
        assert_eq!(r.var_i32().expect("dimension_type"), 3);
        assert_eq!(r.string(32767).expect("dimension"), "minecraft:the_nether");
        assert_eq!(r.i64().expect("seed"), 0);
        assert_eq!(r.u8().expect("game_type"), 0);
        assert_eq!(r.i8().expect("previous_game_type"), -1);
        assert!(!r.bool().expect("is_debug"));
        assert!(!r.bool().expect("is_flat"));
        assert!(!r.bool().expect("has last_death_location"));
        assert_eq!(r.var_i32().expect("portal_cooldown"), 0);
        assert_eq!(
            r.var_i32().expect("sea_level"),
            NETHER_SEA_LEVEL,
            "the destination's sea level, not the overworld's 63"
        );
        assert_eq!(
            r.u8().expect("data_to_keep"),
            0x03,
            "KEEP_ATTRIBUTE_MODIFIERS | KEEP_ENTITY_DATA — a portal trip keeps the \
             player's inventory, XP and health"
        );
        r.ensure_empty().expect("no trailing bytes");

        assert!(
            proto
                .encode_dimension_change("mypack:mine", Vec3::new(0.0, 0.0, 0.0), GameMode::Survival)
                .is_empty(),
            "an unpublished level must emit nothing rather than guess a holder id"
        );
    }

    /// A served column is framed against a **dimension window**, never against its
    /// own height.
    ///
    /// The first arm is the regression that six live loopback tests caught: fixtures
    /// serve deliberately tiny columns (`ChunkColumn::new(0, 16)`), and framing one
    /// against its own height emits a one-section packet to a 24-section client,
    /// which joins, spawns and then decodes nothing at all.
    #[test]
    fn a_columns_shape_comes_from_its_dimension_not_its_height() {
        let short = ServerChunkColumn::new(0, 16);
        assert_eq!(
            shape_for_column(&short).section_count,
            24,
            "an unrecognised window keeps the overworld's 24 sections"
        );
        assert_eq!(shape_for_column(&short).min_y, -64);

        let overworld = ServerChunkColumn::new(-64, 384);
        assert_eq!(shape_for_column(&overworld).section_count, 24);

        let nether = ServerChunkColumn::new(0, 256);
        assert_eq!(
            shape_for_column(&nether).section_count,
            16,
            "the Nether's own window is 16 sections"
        );
        assert_eq!(shape_for_column(&nether).min_y, 0);
    }

    #[test]
    fn dimension_shape_overrides_a_shared_storage_window() {
        let compact = ServerChunkColumn::new(0, 256);
        assert_eq!(shape_for_dimension(Dimension::Overworld).section_count, 24);
        assert_eq!(shape_for_dimension(Dimension::Overworld).min_y, -64);
        assert_eq!(shape_for_dimension(Dimension::Nether).section_count, 16);
        assert_eq!(shape_for_dimension(Dimension::Nether).min_y, 0);
        assert_eq!(shape_for_column(&compact).section_count, 16);
        assert_ne!(
            shape_for_dimension(Dimension::Overworld).section_count,
            shape_for_column(&compact).section_count,
            "the client's dimension, not a shared fixture allocation, frames the packet"
        );
    }
}

/// Index-18's four `BYTE` claimants, checked mechanically against the committed
/// jar dump rather than cited in prose — and the two flag *layouts*, which the
/// dump cannot check because it records the index and serializer, not the bits.
#[cfg(test)]
mod index_eighteen_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{
        METADATA_IDX_CREEPER_IGNITED, METADATA_IDX_HORSE_FLAGS, METADATA_IDX_TAMABLE_FLAGS,
        METADATA_SER_BOOLEAN, METADATA_SER_BYTE, V770ServerProtocol,
    };

    /// `EntityDataIndexOracle`'s output, committed so this gate does not need a JVM.
    /// The same file `crates/versions/26.2/src/packets/metadata.rs`'s own dump gate
    /// reads, for the same reason: the expected value has to come from the jar.
const INDEX_DUMP: &str = include_str!("../../tests/support/entity_data_index_jvm.txt");

    /// `(index, serializer)` for `Owner.FIELD`, or a panic naming the miss.
    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    /// The three constants this module uses at index 18 name accessors the jar
    /// really does put there, with the serializers this encoder writes.
    ///
    /// Collected rather than asserted inside the loop, so a failure reports every
    /// wrong row instead of aborting on the first — three rows named individually is
    /// what makes "which one drifted" answerable.
    #[test]
    fn every_index_eighteen_constant_matches_the_jar_dump() {
        let claims: &[(u8, i32, &str, &str)] = &[
            (
                METADATA_IDX_TAMABLE_FLAGS,
                METADATA_SER_BYTE,
                "TamableAnimal.DATA_FLAGS_ID",
                "METADATA_IDX_TAMABLE_FLAGS",
            ),
            (
                METADATA_IDX_HORSE_FLAGS,
                METADATA_SER_BYTE,
                "AbstractHorse.DATA_ID_FLAGS",
                "METADATA_IDX_HORSE_FLAGS",
            ),
            (
                METADATA_IDX_CREEPER_IGNITED,
                METADATA_SER_BOOLEAN,
                "Creeper.DATA_IS_IGNITED",
                "METADATA_IDX_CREEPER_IGNITED",
            ),
        ];
        let mut wrong: Vec<String> = Vec::new();
        for &(index, serializer, accessor, name) in claims {
            let (want_index, want_serializer) = dump_row(accessor);
            if index != want_index || serializer != want_serializer {
                wrong.push(format!(
                    "{name} says ({index}, ser {serializer}) but the jar says \
                     {accessor} is ({want_index}, ser {want_serializer})"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// **The collision itself**, asserted rather than described: at least four
    /// distinct `BYTE` fields share index 18.
    ///
    /// This is the premise the producer-side species switch in
    /// `lodestone_server::mobs::SimMob::snapshot` exists for. If a future version
    /// collapsed them, the switch would be pointless ceremony and this gate says so;
    /// if a *fifth* appears, the count moves and whoever is adding a metadata field
    /// at 18 is forced to look.
    #[test]
    fn index_eighteen_really_is_shared_by_several_byte_fields() {
        let byte_claimants: Vec<&str> = INDEX_DUMP
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let mut tok = l.split_whitespace();
                let index: u8 = tok.next()?.parse().ok()?;
                let owner = tok.next()?;
                let serializer: i32 = tok.next()?.parse().ok()?;
                (index == 18 && serializer == METADATA_SER_BYTE).then_some(owner)
            })
            .collect();
        for expected in [
            "TamableAnimal.DATA_FLAGS_ID",
            "AbstractHorse.DATA_ID_FLAGS",
            "Sheep.DATA_WOOL_ID",
            "Shulker.DATA_COLOR_ID",
        ] {
            assert!(
                byte_claimants.contains(&expected),
                "{expected} must be one of index 18's BYTE claimants; the dump lists \
                 {byte_claimants:?}"
            );
        }
        assert!(
            byte_claimants.len() >= 4,
            "index 18 must still be shared — if it is not, the species switch in \
             SimMob::snapshot is unnecessary. Claimants: {byte_claimants:?}"
        );
    }

    /// **The two layouts differ, and neither variant sets the other's bit.**
    ///
    /// This is the arm that would have caught one shared "tamed" variant. Note the
    /// direction of the failure it guards: `0x04` is not in `AbstractHorse`'s flag
    /// set at all (`FLAG_TAME` is `2`, `FLAG_BRED` is `8`) and `0x02` is not in
    /// `TamableAnimal`'s, so a shared variant does not set a *wrong* named flag — it
    /// sets an unnamed bit and the animal reads as **untamed**, with a
    /// perfectly-formed packet on the wire and nothing visibly wrong to chase.
    ///
    /// `sitting: false` with `tame: true` on purpose: setting both would make
    /// `0x01 | 0x04 = 0x05` and a gate that only checked "non-zero" could not tell
    /// the tame bit from the sitting bit. The `sitting` bit gets its own arm below.
    #[test]
    fn the_tamable_and_horse_flag_bytes_use_different_bits() {
        let proto = V770ServerProtocol;

        let tamable = flag_byte(
            &proto,
            &MetadataField::TamableFlags {
                tame: true,
                sitting: false,
            },
        );
        let horse = flag_byte(&proto, &MetadataField::HorseFlags { tame: true });

        assert_eq!(tamable, 0x04, "TamableAnimal.isTame() is `& 4`");
        assert_eq!(horse, 0x02, "AbstractHorse.FLAG_TAME is 2");
        assert_ne!(
            tamable, horse,
            "one shared variant would put the same bit on both species, and the one \
             it is wrong for reads as untamed"
        );
        // Neither sets the other's bit, stated as its own claim: equality above could
        // hold for two bytes that both happen to carry both bits.
        assert_eq!(tamable & 0x02, 0, "a wolf must not carry the horse's tame bit");
        assert_eq!(horse & 0x04, 0, "a horse must not carry the wolf's tame bit");
    }

    /// The sitting bit is `0x01` and is independent of tameness.
    ///
    /// Three inputs rather than one, because `sitting` and `tame` are two adjacent
    /// booleans in the same expression: a fixture that sets them equal coincides with
    /// a swapped implementation half the time and cannot see it at all.
    #[test]
    fn the_sitting_bit_is_independent_of_the_tame_bit() {
        let proto = V770ServerProtocol;
        let cases = [
            ((false, false), 0x00u8),
            ((true, false), 0x04),
            ((false, true), 0x01),
            ((true, true), 0x05),
        ];
        let mut wrong: Vec<String> = Vec::new();
        for ((tame, sitting), want) in cases {
            let got = flag_byte(&proto, &MetadataField::TamableFlags { tame, sitting });
            if got != want {
                wrong.push(format!("(tame {tame}, sitting {sitting}) gave {got:#04x}, want {want:#04x}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// The last byte of a one-field `SET_ENTITY_DATA` payload, after checking the
    /// index and serializer it was written under.
    fn flag_byte(proto: &V770ServerProtocol, field: &MetadataField) -> u8 {
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(11, std::slice::from_ref(field))
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("metadata index"), 18);
        assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_BYTE);
        let byte = r.i8().expect("flag byte") as u8;
        // The terminator vanilla's `SynchedEntityData` writes after the last entry.
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
        byte
    }
}

/// Index-13's two claimants, checked mechanically against the committed jar
/// dump — the same shape [`index_eighteen_tests`] establishes: a producer
/// disambiguation (here, "only the furnace-minecart loop ever builds
/// `MetadataField::MinecartFuel`") is only as trustworthy as the premise that
/// the two claimants really do carry different serializers, asserted here
/// rather than assumed.
#[cfg(test)]
mod index_thirteen_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{METADATA_IDX_MINECART_FUEL, METADATA_SER_BOOLEAN, V770ServerProtocol};

const INDEX_DUMP: &str = include_str!("../../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    /// `METADATA_IDX_MINECART_FUEL` names the accessor the jar really puts at
    /// index 13 with the `BOOLEAN` serializer this encoder writes.
    #[test]
    fn minecart_fuel_index_matches_the_jar_dump() {
        let (index, serializer) = dump_row("MinecartFurnace.DATA_ID_FUEL");
        assert_eq!(index, METADATA_IDX_MINECART_FUEL, "MinecartFurnace.DATA_ID_FUEL must be index 13");
        assert_eq!(serializer, METADATA_SER_BOOLEAN, "MinecartFurnace.DATA_ID_FUEL must be a BOOLEAN");
    }

    /// **The premise the producer-based disambiguation depends on**: index
    /// 13's other real claimant, the command-block-minecart class's own command-name accessor,
    /// really is a *different* serializer (`STRING`, not `BOOLEAN`). If a
    /// future jar ever made it a `BOOLEAN` too, this gate — not a silent wire
    /// collision discovered later — is what would catch it.
    #[test]
    fn index_thirteens_other_claimant_is_a_different_serializer() {
        let (index, serializer) = dump_row("MinecartCommandBlock.DATA_ID_COMMAND_NAME");
        assert_eq!(index, METADATA_IDX_MINECART_FUEL, "both claimants share index 13");
        assert_ne!(
            serializer, METADATA_SER_BOOLEAN,
            "MinecartCommandBlock.DATA_ID_COMMAND_NAME must not also be a BOOLEAN, or MinecartFuel is ambiguous on the wire"
        );
    }

    /// The encoder actually writes index 13 with the `BOOLEAN` serializer id,
    /// end to end through `encode_set_entity_data`.
    #[test]
    fn minecart_fuel_encodes_at_index_thirteen() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } = proto.encode_set_entity_data(9, &[MetadataField::MinecartFuel(true)]) else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 9);
        assert_eq!(r.u8().expect("metadata index"), METADATA_IDX_MINECART_FUEL);
        assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_BOOLEAN);
        assert!(r.bool().expect("fuel flag"));
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}

/// Index-16's `BOOLEAN` baby claimants, checked mechanically against the
/// committed jar dump — the wire-level twin of the species switch in
/// `lodestone_server::mobs::SimMob::snapshot`, which decides *which* species
/// this crate ever builds a `MetadataField::Baby` for.
#[cfg(test)]
mod index_sixteen_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{
        METADATA_IDX_BABY, METADATA_IDX_CREEPER_SWELL_DIR, METADATA_SER_BOOLEAN, METADATA_SER_INT,
        V770ServerProtocol,
    };

    /// `EntityDataIndexOracle`'s output, committed so this gate does not need a JVM.
const INDEX_DUMP: &str = include_str!("../../tests/support/entity_data_index_jvm.txt");

    /// `(index, serializer)` for `Owner.FIELD`, or a panic naming the miss.
    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    /// The three real baby accessors the producer-side species switch relies
    /// on — `AgeableMob` for the breedable-animal family, and `Zombie`
    /// (inherited by husk/zombie_villager/drowned/zombified_piglin) and
    /// `Zoglin` declaring their own — all land at [`METADATA_IDX_BABY`] under
    /// the `BOOLEAN` serializer. Collected rather than asserted per-row so a
    /// failure names every wrong one, not just the first.
    #[test]
    fn every_real_baby_accessor_matches_the_jar_dump() {
        let mut wrong: Vec<String> = Vec::new();
        for accessor in [
            "AgeableMob.DATA_BABY_ID",
            "Zombie.DATA_BABY_ID",
            "Zoglin.DATA_BABY_ID",
        ] {
            let (index, serializer) = dump_row(accessor);
            if index != METADATA_IDX_BABY || serializer != METADATA_SER_BOOLEAN {
                wrong.push(format!(
                    "{accessor} is ({index}, ser {serializer}) in the jar, expected \
                     ({METADATA_IDX_BABY}, ser {METADATA_SER_BOOLEAN})"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// **The collision that makes the producer-side species switch load-bearing,
    /// asserted rather than described.** the creeper class's own swell-dir accessor shares index 16
    /// with the baby accessors above but is an `INT`, not a `BOOLEAN` — so a
    /// `MetadataField::Baby` built for a creeper would put a boolean where the
    /// swell direction belongs, and `MobSim::snapshot` never emitting `Baby` for
    /// `"creeper"` is the only thing preventing that.
    #[test]
    fn the_shared_index_really_is_a_different_serializer_for_the_creeper() {
        let (index, serializer) = dump_row("Creeper.DATA_SWELL_DIR");
        assert_eq!(index, METADATA_IDX_CREEPER_SWELL_DIR);
        assert_eq!(index, METADATA_IDX_BABY, "the whole point is that these collide");
        assert_eq!(serializer, METADATA_SER_INT);
        assert_ne!(
            serializer, METADATA_SER_BOOLEAN,
            "if the creeper's swell direction ever became a BOOLEAN, index 16 would no \
             longer distinguish it from Baby and the producer-side guard would need \
             re-checking"
        );
    }

    /// Byte-accurate encode: index, serializer id, the boolean itself, then the
    /// `0xFF` terminator, with no trailing bytes.
    #[test]
    fn baby_encodes_to_the_exact_index_and_serializer() {
        let proto = V770ServerProtocol;
        for value in [true, false] {
            let ServerDirective::Send { payload, .. } =
                proto.encode_set_entity_data(7, &[MetadataField::Baby(value)])
            else {
                panic!("encode_set_entity_data must emit a Send");
            };
            let mut r = Reader::new(&payload);
            assert_eq!(r.var_i32().expect("entity id"), 7);
            assert_eq!(r.u8().expect("metadata index"), METADATA_IDX_BABY);
            assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_BOOLEAN);
            assert_eq!(r.bool().expect("baby bool"), value);
            assert_eq!(r.u8().expect("terminator"), 0xFF);
            assert!(r.ensure_empty().is_ok(), "no trailing bytes");
        }
    }
}

/// Index 16's `INT` claimants, checked mechanically against the committed jar
/// dump — the same shape [`index_sixteen_tests`] establishes for the
/// `BOOLEAN`-serializer baby collision, one level over: a producer
/// disambiguation ("only `MobSim::push_dragon_snapshots` ever builds
/// `MetadataField::DragonPhase`") is only as trustworthy as the premise that
/// index 16's *other* real claimants really do carry the collision this
/// module assumes.
#[cfg(test)]
mod index_sixteen_dragon_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{METADATA_IDX_DRAGON_PHASE, METADATA_SER_INT, V770ServerProtocol};

const INDEX_DUMP: &str = include_str!("../../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    /// the ender-dragon class's own phase accessor really is index 16 under the `INT` serializer
    /// this encoder writes.
    #[test]
    fn dragon_phase_index_matches_the_jar_dump() {
        let (index, serializer) = dump_row("EnderDragon.DATA_PHASE");
        assert_eq!(index, METADATA_IDX_DRAGON_PHASE, "EnderDragon.DATA_PHASE must be index 16");
        assert_eq!(serializer, METADATA_SER_INT, "EnderDragon.DATA_PHASE must be an INT");
    }

    /// **The premise the producer-based disambiguation depends on**: every
    /// other index-16 `INT` claimant the jar dump lists really is a different
    /// owner (so the producer, not the wire, is what keeps them apart).
    /// Collected rather than asserted per-row so a failure names every wrong
    /// one, not just the first.
    #[test]
    fn index_sixteens_other_int_claimants_are_all_distinct_from_the_dragon() {
        let mut wrong: Vec<String> = Vec::new();
        for accessor in [
            "Creeper.DATA_SWELL_DIR",
            "Display.DATA_BRIGHTNESS_OVERRIDE_ID",
            "Phantom.ID_SIZE",
            "Warden.CLIENT_ANGER_LEVEL",
            "WitherBoss.DATA_TARGET_A",
        ] {
            let (index, serializer) = dump_row(accessor);
            if index != METADATA_IDX_DRAGON_PHASE || serializer != METADATA_SER_INT {
                wrong.push(format!(
                    "{accessor} is ({index}, ser {serializer}) in the jar, expected the same \
                     collision ({METADATA_IDX_DRAGON_PHASE}, ser {METADATA_SER_INT}) `DragonPhase` shares"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// Byte-accurate encode: index, serializer id, the phase int itself, then
    /// the `0xFF` terminator, with no trailing bytes. Pairwise-distinct from
    /// the entity id so a transposition cannot survive.
    #[test]
    fn dragon_phase_encodes_to_the_exact_index_and_serializer() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(11, &[MetadataField::DragonPhase(4)])
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("metadata index"), METADATA_IDX_DRAGON_PHASE);
        assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_INT);
        assert_eq!(r.var_i32().expect("phase int"), 4);
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}

/// The end crystal's two claimed indices — 8 (`OPTIONAL_BLOCK_POS`, no
/// collision the encoder needs a producer guard for) and 9 (`BOOLEAN`, a
/// three-way collision) — checked against the committed jar dump.
#[cfg(test)]
mod end_crystal_index_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{
        METADATA_IDX_CRYSTAL_BEAM_TARGET, METADATA_IDX_CRYSTAL_SHOW_BOTTOM, METADATA_SER_BOOLEAN,
        METADATA_SER_OPTIONAL_BLOCK_POS, V770ServerProtocol,
    };

const INDEX_DUMP: &str = include_str!("../../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    #[test]
    fn crystal_beam_target_index_matches_the_jar_dump() {
        let (index, serializer) = dump_row("EndCrystal.DATA_BEAM_TARGET");
        assert_eq!(index, METADATA_IDX_CRYSTAL_BEAM_TARGET);
        assert_eq!(serializer, METADATA_SER_OPTIONAL_BLOCK_POS);
    }

    /// **The premise that lets the beam-target decode arm skip a class
    /// guard**: no *other* index-8 claimant in the jar carries
    /// `OPTIONAL_BLOCK_POS`. If one ever did, `(index, serializer)` alone
    /// would stop uniquely identifying the crystal and a class guard would
    /// become necessary, exactly as index 9 already needs one below.
    #[test]
    fn index_eight_has_exactly_one_optional_block_pos_claimant() {
        let claimants: Vec<&str> = INDEX_DUMP
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter(|l| {
                let mut tok = l.split_whitespace();
                let index: u8 = tok.next().expect("index").parse().expect("u8");
                let _owner = tok.next();
                let serializer: i32 = tok.next().expect("serializer").parse().expect("i32");
                index == METADATA_IDX_CRYSTAL_BEAM_TARGET && serializer == METADATA_SER_OPTIONAL_BLOCK_POS
            })
            .collect();
        assert_eq!(
            claimants.len(),
            1,
            "expected exactly EndCrystal.DATA_BEAM_TARGET at index 8 with OPTIONAL_BLOCK_POS, got {claimants:?}"
        );
    }

    #[test]
    fn crystal_show_bottom_index_matches_the_jar_dump() {
        let (index, serializer) = dump_row("EndCrystal.DATA_SHOW_BOTTOM");
        assert_eq!(index, METADATA_IDX_CRYSTAL_SHOW_BOTTOM);
        assert_eq!(serializer, METADATA_SER_BOOLEAN);
    }

    /// **The premise the producer-based disambiguation depends on for
    /// `CrystalShowBottom`**: index 9's other two `BOOLEAN` claimants really
    /// are different owners.
    #[test]
    fn index_nines_other_boolean_claimants_are_distinct_from_the_crystal() {
        let mut wrong: Vec<String> = Vec::new();
        for accessor in ["AreaEffectCloud.DATA_WAITING", "FishingHook.DATA_BITING"] {
            let (index, serializer) = dump_row(accessor);
            if index != METADATA_IDX_CRYSTAL_SHOW_BOTTOM || serializer != METADATA_SER_BOOLEAN {
                wrong.push(format!(
                    "{accessor} is ({index}, ser {serializer}) in the jar, expected the same \
                     collision ({METADATA_IDX_CRYSTAL_SHOW_BOTTOM}, ser {METADATA_SER_BOOLEAN}) \
                     `CrystalShowBottom` shares"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// Byte-accurate encode of a present beam target: presence bool, then the
    /// packed-long block position. Coordinates are pairwise-distinct so a
    /// transposition against `pack_block_pos`'s own `(x, y, z)` order cannot
    /// survive.
    #[test]
    fn crystal_beam_target_encodes_a_present_position() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } = proto.encode_set_entity_data(
            13,
            &[MetadataField::CrystalBeamTarget(Some(lodestone_model::BlockPos::new(11, 65, 4)))],
        ) else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 13);
        assert_eq!(r.u8().expect("metadata index"), METADATA_IDX_CRYSTAL_BEAM_TARGET);
        assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_OPTIONAL_BLOCK_POS);
        assert!(r.bool().expect("presence bool"));
        let packed = r.i64().expect("packed block pos");
        // Unpack the same way `crate::packets::metadata`'s decode side does,
        // independently re-derived here rather than calling that function, so
        // this assertion cannot pass by construction against a shared bug.
        let x = (packed >> 38) as i32;
        let y = ((packed << 52) >> 52) as i32;
        let z = ((packed << 26) >> 38) as i32;
        assert_eq!((x, y, z), (11, 65, 4));
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }

    /// Byte-accurate encode of a cleared beam target: presence bool `false`,
    /// no position bytes at all.
    #[test]
    fn crystal_beam_target_encodes_absence_as_a_bare_false() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(13, &[MetadataField::CrystalBeamTarget(None)])
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 13);
        assert_eq!(r.u8().expect("metadata index"), METADATA_IDX_CRYSTAL_BEAM_TARGET);
        assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_OPTIONAL_BLOCK_POS);
        assert!(!r.bool().expect("presence bool"));
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }

    /// Byte-accurate encode of `CrystalShowBottom`, both values — and
    /// deliberately alongside a `CrystalBeamTarget` set to the *other*
    /// boolean-shaped state (`Some`, not `None`) in the same field list, so a
    /// transposition between the two adjacent crystal fields cannot survive
    /// (`CLAUDE.md`: "two adjacent bools coincide half the time by chance").
    #[test]
    fn crystal_show_bottom_encodes_to_the_exact_index_and_serializer() {
        let proto = V770ServerProtocol;
        for value in [true, false] {
            let ServerDirective::Send { payload, .. } = proto.encode_set_entity_data(
                13,
                &[
                    MetadataField::CrystalBeamTarget(Some(lodestone_model::BlockPos::new(2, 70, -3))),
                    MetadataField::CrystalShowBottom(value),
                ],
            ) else {
                panic!("encode_set_entity_data must emit a Send");
            };
            let mut r = Reader::new(&payload);
            assert_eq!(r.var_i32().expect("entity id"), 13);
            // Beam target first: presence bool, packed position.
            assert_eq!(r.u8().expect("beam index"), METADATA_IDX_CRYSTAL_BEAM_TARGET);
            assert_eq!(r.var_i32().expect("beam serializer"), METADATA_SER_OPTIONAL_BLOCK_POS);
            assert!(r.bool().expect("beam presence"));
            r.i64().expect("packed pos");
            // Then show-bottom.
            assert_eq!(r.u8().expect("show-bottom index"), METADATA_IDX_CRYSTAL_SHOW_BOTTOM);
            assert_eq!(r.var_i32().expect("show-bottom serializer"), METADATA_SER_BOOLEAN);
            assert_eq!(r.bool().expect("show-bottom bool"), value);
            assert_eq!(r.u8().expect("terminator"), 0xFF);
            assert!(r.ensure_empty().is_ok(), "no trailing bytes");
        }
    }
}

/// `BOSS_EVENT`'s three operations this crate emits, checked against
/// vanilla's own clientbound boss-event packet's own `write` method
/// (confirmed against the decompiled 26.2 source)
/// rather than its constructors — see `encode_boss_event_add`'s own doc for
/// why the field order there differs from a naive transcription.
#[cfg(test)]
mod boss_event_tests {
    use lodestone_core::Reader;
    use lodestone_model::Text;
    use lodestone_server::{ServerDirective, ServerProtocol};
    use uuid::Uuid;

    use super::V770ServerProtocol;

    /// A fixed, non-nil UUID so a byte-order mistake in `Writer::uuid` cannot
    /// coincidentally read back correctly.
    fn id() -> Uuid {
        Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10)
    }

    #[test]
    fn add_writes_uuid_type_name_progress_color_overlay_flags_in_that_order() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_boss_event_add(id(), &Text::literal("Ender Dragon"), 0.75)
        else {
            panic!("encode_boss_event_add must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.uuid().expect("boss bar id"), id());
        assert_eq!(r.var_i32().expect("operation type"), 0, "ADD");
        // Network-NBT component: skip via the same path the decode side uses
        // elsewhere in this crate (`read_network_nbt`), so this assertion does
        // not re-implement NBT parsing.
        lodestone_core::read_network_nbt(&mut r).expect("name component");
        assert_eq!(r.f32().expect("progress"), 0.75);
        assert_eq!(r.var_i32().expect("color"), 0, "PINK");
        assert_eq!(r.var_i32().expect("overlay"), 0, "PROGRESS");
        assert_eq!(r.u8().expect("flags"), 0b110, "playMusic | createWorldFog");
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }

    #[test]
    fn update_progress_writes_uuid_type_then_a_bare_float() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } = proto.encode_boss_event_update_progress(id(), 0.25) else {
            panic!("encode_boss_event_update_progress must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.uuid().expect("boss bar id"), id());
        assert_eq!(r.var_i32().expect("operation type"), 2, "UPDATE_PROGRESS");
        assert_eq!(r.f32().expect("progress"), 0.25);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }

    #[test]
    fn remove_writes_uuid_and_type_with_no_payload_at_all() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } = proto.encode_boss_event_remove(id()) else {
            panic!("encode_boss_event_remove must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.uuid().expect("boss bar id"), id());
        assert_eq!(r.var_i32().expect("operation type"), 1, "REMOVE");
        assert!(r.ensure_empty().is_ok(), "no trailing bytes — REMOVE carries no payload");
    }

    /// Two different progress values must not collide on the wire — the
    /// control for `update_progress`'s own assertion above (a transposition
    /// or an endianness bug that happened to read back `0.25` correctly would
    /// still pass that test alone).
    #[test]
    fn add_and_update_progress_carry_genuinely_different_bytes_for_different_progress() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload: low, .. } = proto.encode_boss_event_update_progress(id(), 0.1)
        else {
            panic!("must emit a Send");
        };
        let ServerDirective::Send { payload: high, .. } = proto.encode_boss_event_update_progress(id(), 0.9)
        else {
            panic!("must emit a Send");
        };
        assert_ne!(low, high);
    }
}

#[cfg(test)]
mod play_ping_request_tests {
    use lodestone_core::State;
    use lodestone_server::{ServerBound, ServerProtocol};

    use super::V770ServerProtocol;
    use crate::packet_ids::play;

    /// `ServerboundPingRequestPacket`: a single big-endian `i64`. Bytes are
    /// hand-built rather than round-tripped through this crate's own encoder — a
    /// symmetric transposition/endianness bug would otherwise pass against
    /// itself — and the value is non-zero/non-palindromic so a byte-order
    /// mistake cannot coincidentally read back correctly.
    #[test]
    fn decode_play_ping_request_lifts_the_time() {
        let proto = V770ServerProtocol;
        let body = 0x0102_0304_0506_0708_i64.to_be_bytes().to_vec();
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PING_REQUEST, &body),
            ServerBound::PingRequest {
                time: 0x0102_0304_0506_0708,
            },
        );
    }

    /// A malformed (short) frame must not construct a variant with a
    /// truncated/zeroed time — the control for the assertion above: without
    /// it, an implementation that always returned `PingRequest { time: 0 }`
    /// regardless of the payload would also pass the happy-path test.
    #[test]
    fn decode_play_ping_request_rejects_a_short_frame() {
        let proto = V770ServerProtocol;
        let short = vec![1, 2, 3];
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PING_REQUEST, &short),
            ServerBound::Ignored,
        );
    }

    /// A valid `pong` is an explicit acknowledgement boundary rather than an
    /// ignored frame. Its raw fixed-width id must survive decoding so the
    /// connection can deliberately consume it without inventing state.
    #[test]
    fn decode_play_pong_lifts_the_big_endian_id() {
        let proto = V770ServerProtocol;
        let body = 0x0102_0304_i32.to_be_bytes().to_vec();
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PONG, &body),
            ServerBound::Pong { id: 0x0102_0304 },
        );
    }

    /// The acknowledgement needs its entire four-byte body. This control
    /// distinguishes the valid no-op above from an arm that lifted a constant
    /// id regardless of the received frame.
    #[test]
    fn decode_play_pong_rejects_a_short_frame() {
        let proto = V770ServerProtocol;
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PONG, &[1, 2, 3]),
            ServerBound::Ignored,
        );
    }
}

#[cfg(test)]
mod seen_advancements_tests {
    use lodestone_core::{Reader, State};
    use lodestone_server::{AdvancementManager, ServerBound, ServerDirective, ServerProtocol};
    use uuid::Uuid;

    use super::V770ServerProtocol;
    use crate::packet_ids::play;

    /// The body is deliberately raw rather than produced by the client
    /// adapter: action 0, then the independently counted UTF-8 identifier.
    /// This catches a decoder that accepts the right action but consumes the
    /// wrong string framing.
    #[test]
    fn seen_advancements_opened_tab_lifts_from_raw_wire_bytes() {
        let proto = V770ServerProtocol;
        let body = [
            0, // OPENED_TAB
            20, // byte length of minecraft:story/root
            b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b's', b't', b'o', b'r',
            b'y', b'/', b'r', b'o', b'o', b't',
        ];
        assert_eq!(
            proto.decode(State::Play, play::serverbound::SEEN_ADVANCEMENTS, &body),
            ServerBound::SeenAdvancements {
                tab: Some("minecraft:story/root".to_owned()),
            },
        );
    }

    /// The close action carries no identifier; accepting an extra byte would
    /// hide a stream framing error in the packet immediately after it.
    #[test]
    fn seen_advancements_close_has_no_identifier_or_trailing_bytes() {
        let proto = V770ServerProtocol;
        assert_eq!(
            proto.decode(State::Play, play::serverbound::SEEN_ADVANCEMENTS, &[1]),
            ServerBound::SeenAdvancements { tab: None },
        );
        assert_eq!(
            proto.decode(State::Play, play::serverbound::SEEN_ADVANCEMENTS, &[1, 0]),
            ServerBound::Ignored,
        );
        assert_eq!(
            proto.decode(State::Play, play::serverbound::SEEN_ADVANCEMENTS, &[2]),
            ServerBound::Ignored,
        );
    }

    /// The production server consumes the lifted selection through
    /// `AdvancementManager` and emits this directive. Check both ends of that
    /// seam here so the new state cannot become a write-only counter.
    #[test]
    fn seen_advancements_selection_reaches_the_clientbound_tab_directive() {
        let proto = V770ServerProtocol;
        let mut manager = AdvancementManager::builtin();
        let selected = manager.select_tab(
            Uuid::nil(),
            Some("minecraft:adventure/root".to_owned()),
        );
        let ServerDirective::Send { packet_id, payload } =
            proto.encode_select_advancements_tab(selected.as_deref())
        else {
            panic!("selected advancement tab must be sent to the client");
        };
        assert_eq!(packet_id, play::clientbound::SELECT_ADVANCEMENTS_TAB);
        let mut reader = Reader::new(&payload);
        assert!(reader.bool().expect("tab-present flag"));
        assert_eq!(
            reader.string(32767).expect("tab identifier"),
            "minecraft:adventure/root"
        );
        assert!(reader.ensure_empty().is_ok(), "selection body has no trailing bytes");
    }
}

/// the goat class's own has-left-horn accessor/`DATA_HAS_RIGHT_HORN` at indices 19/20 — the
/// same census-premise-plus-encode-exactness shape `index_thirteen_tests`
/// already uses for its own claimed index.
#[cfg(test)]
mod goat_horns_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{
        METADATA_IDX_GOAT_HAS_LEFT_HORN, METADATA_IDX_GOAT_HAS_RIGHT_HORN, METADATA_SER_BOOLEAN,
        V770ServerProtocol,
    };

const INDEX_DUMP: &str = include_str!("../../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    /// `METADATA_IDX_GOAT_HAS_LEFT_HORN`/`_RIGHT_HORN` name the accessors the
    /// jar really puts at indices 19/20, both `BOOLEAN`.
    #[test]
    fn goat_horn_indices_match_the_jar_dump() {
        let (left_index, left_ser) = dump_row("Goat.DATA_HAS_LEFT_HORN");
        assert_eq!(left_index, METADATA_IDX_GOAT_HAS_LEFT_HORN);
        assert_eq!(left_ser, METADATA_SER_BOOLEAN);
        let (right_index, right_ser) = dump_row("Goat.DATA_HAS_RIGHT_HORN");
        assert_eq!(right_index, METADATA_IDX_GOAT_HAS_RIGHT_HORN);
        assert_eq!(right_ser, METADATA_SER_BOOLEAN);
    }

    /// The encoder writes both fields, in order, each with the `BOOLEAN`
    /// serializer id, end to end through `encode_set_entity_data` — with the
    /// two bools **deliberately different** (`false`/`true`) so a
    /// transposition of the pair cannot survive this assertion, per
    /// `DESIGN.md`'s own warning about adjacent same-typed fields.
    #[test]
    fn goat_horns_encode_at_indices_nineteen_and_twenty() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } = proto.encode_set_entity_data(
            11,
            &[MetadataField::GoatHorns { has_left: false, has_right: true }],
        ) else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("left horn index"), METADATA_IDX_GOAT_HAS_LEFT_HORN);
        assert_eq!(r.var_i32().expect("left horn serializer"), METADATA_SER_BOOLEAN);
        assert!(!r.bool().expect("left horn value"), "has_left was false");
        assert_eq!(r.u8().expect("right horn index"), METADATA_IDX_GOAT_HAS_RIGHT_HORN);
        assert_eq!(r.var_i32().expect("right horn serializer"), METADATA_SER_BOOLEAN);
        assert!(r.bool().expect("right horn value"), "has_right was true");
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}

/// the axolotl class's own playing-dead accessor at index 19 — the same census-premise-plus-
/// encode-exactness shape [`goat_horns_tests`] already uses for its own
/// claimed index.
#[cfg(test)]
mod axolotl_playing_dead_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{METADATA_IDX_AXOLOTL_PLAYING_DEAD, METADATA_SER_BOOLEAN, V770ServerProtocol};

const INDEX_DUMP: &str = include_str!("../../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    #[test]
    fn axolotl_playing_dead_index_matches_the_jar_dump() {
        let (index, ser) = dump_row("Axolotl.DATA_PLAYING_DEAD");
        assert_eq!(index, METADATA_IDX_AXOLOTL_PLAYING_DEAD);
        assert_eq!(ser, METADATA_SER_BOOLEAN);
    }

    #[test]
    fn playing_dead_encodes_at_index_nineteen() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(11, &[MetadataField::PlayingDead(true)])
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("index"), METADATA_IDX_AXOLOTL_PLAYING_DEAD);
        assert_eq!(r.var_i32().expect("serializer"), METADATA_SER_BOOLEAN);
        assert!(r.bool().expect("value"), "true was pushed");
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}

/// the camel class's own dash accessor at index 19 — the same census-premise-plus-encode-exactness
/// shape [`goat_horns_tests`] already uses for its own claimed index.
#[cfg(test)]
mod camel_dash_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{METADATA_IDX_CAMEL_DASH, METADATA_SER_BOOLEAN, V770ServerProtocol};

const INDEX_DUMP: &str = include_str!("../../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    #[test]
    fn camel_dash_index_matches_the_jar_dump() {
        let (index, ser) = dump_row("Camel.DASH");
        assert_eq!(index, METADATA_IDX_CAMEL_DASH);
        assert_eq!(ser, METADATA_SER_BOOLEAN);
    }

    #[test]
    fn dash_encodes_at_index_nineteen() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(11, &[MetadataField::Dash(true)])
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("index"), METADATA_IDX_CAMEL_DASH);
        assert_eq!(r.var_i32().expect("serializer"), METADATA_SER_BOOLEAN);
        assert!(r.bool().expect("value"), "true was pushed");
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}

/// the sniffer class's own state accessor at index 18, serializer 35 — the same census-
/// premise-plus-encode-exactness shape [`goat_horns_tests`] already uses for
/// its own claimed index, plus a check that the wire value is the real
/// the sniffer class's own state ordinal rather than a crate-local renumbering.
#[cfg(test)]
mod sniffer_state_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{METADATA_IDX_SNIFFER_STATE, METADATA_SER_SNIFFER_STATE, V770ServerProtocol};

const INDEX_DUMP: &str = include_str!("../../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, &'static str) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let _serializer_id: i32 = tok.next().expect("serializer column").parse().expect("i32");
            let serializer_name = tok.next().expect("serializer name column");
            if owner == owner_field {
                return (index, serializer_name);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    #[test]
    fn sniffer_state_index_matches_the_jar_dump() {
        let (index, serializer_name) = dump_row("Sniffer.DATA_STATE");
        assert_eq!(index, METADATA_IDX_SNIFFER_STATE);
        assert_eq!(serializer_name, "SNIFFER_STATE");
    }

    /// The same index also claims the armadillo class's own armadillo-state accessor under a
    /// *different* serializer id — the tell that species alone cannot
    /// disambiguate this index and the serializer id is load-bearing.
    #[test]
    fn index_eighteen_also_claims_a_different_armadillo_serializer() {
        let (armadillo_index, armadillo_serializer_name) = dump_row("Armadillo.ARMADILLO_STATE");
        assert_eq!(armadillo_index, METADATA_IDX_SNIFFER_STATE);
        assert_eq!(armadillo_serializer_name, "ARMADILLO_STATE");
    }

    #[test]
    fn sniffer_state_encodes_at_index_eighteen_as_a_real_ordinal() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(11, &[MetadataField::SnifferState(5)])
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("index"), METADATA_IDX_SNIFFER_STATE);
        assert_eq!(r.var_i32().expect("serializer"), METADATA_SER_SNIFFER_STATE);
        // `5` is the sniffer class's own state.DIGGING`'s real ordinal, not `0`/`1` — a
        // wrong-serializer or off-by-one bug would still pass a `true`/`false`
        // shaped assertion, so this pins the actual integer.
        assert_eq!(r.var_i32().expect("state ordinal"), 5);
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}
