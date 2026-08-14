//! Hermetic tests for the protocol 776 world-state packets `respawn` and
//! `set_time`.
//!
//! Clientbound golden byte vectors are hand-built from the wire specification
//! (`ClientboundRespawnPacket` / `ClientboundSetTimePacket`, behavioural
//! reference only), so a symmetric encode/decode bug cannot pass silently.

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_v770::packets::game::{GlobalPos, Respawn};
use lodestone_v770::packets::time::SetTime;
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 776 };

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    let value = T::decode(&mut reader, CTX).expect("decode");
    reader.ensure_empty().expect("no trailing bytes");
    value
}

/// A `respawn` body: dimension-type holder id `0`, dimension
/// `minecraft:the_nether`, zero seed, survival game type, previous game type
/// `-1`, not debug, not flat, no last death location, zero portal cooldown, sea
/// level `63`, and a `data_to_keep` mask of `0`.
fn respawn_golden() -> Vec<u8> {
    let mut bytes = vec![0x00]; // dimension_type varint 0
    let dim = b"minecraft:the_nether";
    bytes.push(dim.len() as u8); // string length varint (20)
    bytes.extend_from_slice(dim);
    bytes.extend_from_slice(&[0x00; 8]); // seed i64 = 0
    bytes.push(0x00); // game_type survival
    bytes.push(0xFF); // previous_game_type -1
    bytes.push(0x00); // is_debug false
    bytes.push(0x00); // is_flat false
    bytes.push(0x00); // last_death_location None
    bytes.push(0x00); // portal_cooldown varint 0
    bytes.push(0x3F); // sea_level varint 63
    bytes.push(0x00); // data_to_keep 0
    bytes
}

#[test]
fn respawn_decodes_from_golden_bytes() {
    let body: Respawn = decode(&respawn_golden());
    assert_eq!(body.dimension_type, 0);
    assert_eq!(body.dimension, "minecraft:the_nether");
    assert_eq!(body.seed, 0);
    assert_eq!(body.game_type, 0);
    assert_eq!(body.previous_game_type, -1);
    assert!(!body.is_debug);
    assert!(!body.is_flat);
    assert_eq!(body.last_death_location, None);
    assert_eq!(body.portal_cooldown, 0);
    assert_eq!(body.sea_level, 63);
    assert_eq!(body.data_to_keep, 0);
}

#[test]
fn respawn_re_encodes_to_the_same_bytes() {
    // Symmetric check against the hand-built vector, so the decoder and encoder
    // are pinned to the wire layout rather than to each other.
    let body: Respawn = decode(&respawn_golden());
    assert_eq!(encode(&body), respawn_golden());
}

#[test]
fn respawn_decodes_present_last_death_location() {
    let mut bytes = vec![0x00];
    let dim = b"minecraft:overworld";
    bytes.push(dim.len() as u8);
    bytes.extend_from_slice(dim);
    bytes.extend_from_slice(&[0x00; 8]); // seed
    bytes.push(0x01); // creative
    bytes.push(0x00); // previous survival
    bytes.push(0x00);
    bytes.push(0x01); // is_flat true
    bytes.push(0x01); // last_death_location Some
    let death_dim = b"minecraft:overworld";
    bytes.push(death_dim.len() as u8);
    bytes.extend_from_slice(death_dim);
    bytes.extend_from_slice(&123_i64.to_be_bytes()); // packed BlockPos
    bytes.push(0x00); // portal_cooldown
    bytes.push(0x3F); // sea_level 63
    bytes.push(0x02); // data_to_keep
    let body: Respawn = decode(&bytes);
    assert_eq!(
        body.last_death_location,
        Some(GlobalPos {
            dimension: "minecraft:overworld".to_owned(),
            position: 123,
        })
    );
    assert!(body.is_flat);
    assert_eq!(body.data_to_keep, 2);
}

#[test]
fn handle_play_respawn_emits_respawned_event() {
    // Previously respawn only updated internal chunk shape and emitted
    // nothing — a decode-and-discard gap. It now also surfaces a
    // `ClientEvent::Respawned` so a consumer (HUD gamemode, dimension change,
    // last-death compass) actually receives it.
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::RESPAWN,
            &respawn_golden(),
        )
        .expect("handle respawn");
    // Two directives: the dimension **type** (resolved against
    // the ingested `registry_data`, `None` here because no registry was fed)
    // then the `Respawned` event, in that order.
    match directives.as_slice() {
        [
            Directive::Emit(ClientEvent::DimensionTypeChanged {
                holder_id,
                dimension_type,
            }),
            Directive::Emit(ClientEvent::Respawned {
                dimension,
                game_mode,
                previous_game_mode,
                last_death_location,
            }),
        ] => {
            assert_eq!(*holder_id, 0, "the golden respawn's dimension_type varint");
            assert_eq!(
                *dimension_type, None,
                "no registry_data was fed, so the holder id must not resolve — \
                 and must not silently become the overworld"
            );
            assert_eq!(dimension.to_string(), "minecraft:the_nether");
            assert_eq!(*game_mode, lodestone_model::GameMode::Survival);
            assert_eq!(*previous_game_mode, None);
            assert_eq!(*last_death_location, None);
        }
        other => panic!("expected DimensionTypeChanged then Respawned, got {other:?}"),
    }
}

#[test]
fn handle_play_respawn_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = respawn_golden();
    payload.push(0xAB); // one byte too many
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::RESPAWN,
        &payload,
    );
    assert!(result.is_err(), "a misaligned respawn must be rejected");
}

#[test]
fn handle_play_respawn_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = respawn_golden();
    payload.truncate(payload.len() - 1); // drop data_to_keep
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::RESPAWN,
        &payload,
    );
    assert!(result.is_err(), "a truncated respawn must error, not panic");
}

/// A `set_time` body: world age `1000`, one clock update — holder id `1`, total
/// ticks `6000`, partial tick `0.0`, rate `1.0`.
fn set_time_golden() -> Vec<u8> {
    let mut bytes = 1000_i64.to_be_bytes().to_vec(); // game_time
    bytes.push(0x01); // clock count varint 1
    bytes.push(0x01); // holder_id varint 1
    bytes.extend_from_slice(&[0xF0, 0x2E]); // total_ticks varlong 6000
    bytes.extend_from_slice(&0.0_f32.to_be_bytes()); // partial_tick
    bytes.extend_from_slice(&1.0_f32.to_be_bytes()); // rate
    bytes
}

/// A `set_time` body with `game_time` and **no** clock updates — the shape
/// `MinecraftServer::forceGameTimeSynchronization` broadcasts roughly once a
/// second, forever, and therefore the shape that dominates a real session.
fn set_time_sync_only(game_time: i64) -> Vec<u8> {
    let mut bytes = game_time.to_be_bytes().to_vec();
    bytes.push(0x00); // zero clock updates
    bytes
}

/// A `set_time` body carrying one clock update, as `/time set` produces.
fn set_time_with_clock(game_time: i64, holder_id: u8, total_ticks: u32, rate: f32) -> Vec<u8> {
    let mut bytes = game_time.to_be_bytes().to_vec();
    bytes.push(0x01); // clock count varint 1
    bytes.push(holder_id);
    // VarLong, unsigned LEB128 over the i64 bit pattern; `total_ticks` is small
    // and positive so plain 7-bit groups suffice.
    let mut v = u64::from(total_ticks);
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            bytes.push(byte);
            break;
        }
        bytes.push(byte | 0x80);
    }
    bytes.extend_from_slice(&0.0_f32.to_be_bytes()); // partial_tick
    bytes.extend_from_slice(&rate.to_be_bytes());
    bytes
}

/// Drive `adapter` with one `set_time` payload and return the `time_of_day` it
/// surfaced.
fn time_of_day_after(adapter: &V770Adapter, payload: &[u8]) -> i64 {
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SET_TIME,
            payload,
        )
        .expect("handle set_time");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::TimeChanged { time_of_day, .. })] => *time_of_day,
        other => panic!("expected one TimeChanged, got {other:?}"),
    }
}

#[test]
fn set_time_decodes_from_golden_bytes() {
    let body: SetTime = decode(&set_time_golden());
    assert_eq!(body.game_time, 1000);
    assert_eq!(body.clocks.len(), 1);
    let clock = &body.clocks[0];
    assert_eq!(clock.holder_id, 1);
    assert_eq!(clock.total_ticks, 6000);
    assert_eq!(clock.partial_tick, 0.0);
    assert_eq!(clock.rate, 1.0);
    assert_eq!(body.day_clock().map(|c| c.total_ticks), Some(6000));
}

/// The wire-level half of the fullbright defect: a `set_time` with an empty clock
/// map names **no** day clock. It does *not* name the world age as the day time,
/// which is what `day_time()` used to return and what pinned `sky_darken` to a
/// session constant.
#[test]
fn an_empty_clock_map_names_no_day_clock() {
    let body: SetTime = decode(&set_time_sync_only(42));
    assert!(body.clocks.is_empty());
    assert!(
        body.day_clock().is_none(),
        "an empty clock map must name no clock; returning the world age here is the \
         permanent-noon bug"
    );
}

/// `clockUpdates` is a Java `HashMap`, so the join-time full sync's two entries
/// (`minecraft:overworld` = 0, `minecraft:the_end` = 1) can arrive in either
/// order. The day clock is selected by holder id, never by wire position.
#[test]
fn day_clock_selects_the_lowest_holder_id_not_the_wire_order() {
    // Two updates, the End clock (id 1) first on the wire.
    let mut bytes = 500_i64.to_be_bytes().to_vec();
    bytes.push(0x02); // two clock updates
    bytes.push(0x01); // holder_id 1 (the_end)
    bytes.push(0x0A); // total_ticks 10
    bytes.extend_from_slice(&0.0_f32.to_be_bytes());
    bytes.extend_from_slice(&1.0_f32.to_be_bytes());
    bytes.push(0x00); // holder_id 0 (overworld)
    bytes.extend_from_slice(&[0xF0, 0x2E]); // total_ticks 6000
    bytes.extend_from_slice(&0.0_f32.to_be_bytes());
    bytes.extend_from_slice(&1.0_f32.to_be_bytes());

    let body: SetTime = decode(&bytes);
    assert_eq!(body.clocks.len(), 2);
    assert_eq!(body.clocks[0].holder_id, 1, "the End clock is first on the wire");
    let day = body.day_clock().expect("a day clock");
    assert_eq!(
        (day.holder_id, day.total_ticks),
        (0, 6000),
        "the overworld clock (id 0) is the day clock even when it arrives second"
    );
}

/// **The regression gate for "the world is fullbright / the mobs look like
/// daytime".** A held day clock must survive the once-a-second empty-map sync and
/// advance at the server's rate — never be replaced by the world age.
///
/// The negative control is *run*, not described: the same packet sequence is
/// scored a second time under the old rule (day time = world age when the map is
/// empty), and this test asserts that rule produces a **different** answer. If
/// the two ever agreed the gate would be measuring nothing, which is exactly how
/// the defect survived — on a fresh world `age` and the day clock start out equal.
#[test]
fn an_empty_clock_map_does_not_overwrite_the_held_day_time() {
    let adapter = V770Adapter::new();

    // Join: the full sync anchors the overworld clock at 6000 (noon) while the
    // world is already 500_000 ticks old — the divergence every long-lived world
    // has and a fresh one does not.
    let anchored = time_of_day_after(&adapter, &set_time_with_clock(500_000, 0, 6000, 1.0));
    assert_eq!(anchored, 6000, "the full sync must be taken verbatim");

    // Then twenty seconds of game-time-only syncs, 20 ticks apart.
    let mut last = anchored;
    for step in 1..=20_i64 {
        let age = 500_000 + step * 20;
        let got = time_of_day_after(&adapter, &set_time_sync_only(age));
        assert_eq!(
            got,
            6000 + step * 20,
            "the held clock must advance at rate 1.0 from its anchor, not jump to the world age"
        );
        assert!(got > last, "the day clock must keep moving across sync-only packets");
        last = got;
    }

    // The control: what the retired rule would have reported for that last
    // packet. It must differ, or this gate cannot tell the fix from the bug.
    let old_rule_would_say = 500_000 + 20 * 20;
    assert_ne!(
        last, old_rule_would_say,
        "NEGATIVE CONTROL DID NOT FIRE: the held clock and the world age agree ({last}), so this \
         sequence cannot distinguish the fix from the permanent-noon bug. Widen the age/clock \
         divergence in the fixture."
    );
}

/// `/gamerule advanceTime false` and a paused clock both arrive as `rate = 0.0`
/// (`ClockInstance::packNetworkState`), so a frozen sun must stay frozen even as
/// the world age keeps climbing.
#[test]
fn a_paused_clock_does_not_advance_with_the_world_age() {
    let adapter = V770Adapter::new();
    assert_eq!(
        time_of_day_after(&adapter, &set_time_with_clock(1_000, 0, 18_000, 0.0)),
        18_000
    );
    for step in 1..=5_i64 {
        assert_eq!(
            time_of_day_after(&adapter, &set_time_sync_only(1_000 + step * 20)),
            18_000,
            "a rate-0 clock must not advance"
        );
    }
}

/// Before the join-time full sync there is no clock to hold, and this arm then
/// reports the world age — exactly what it always did. Pinned so the seeding
/// branch is a deliberate fallback rather than an accident, and so the window it
/// covers stays visible.
#[test]
fn an_unsynced_clock_falls_back_to_the_world_age() {
    let adapter = V770Adapter::new();
    assert_eq!(time_of_day_after(&adapter, &set_time_sync_only(777)), 777);
    // …and the first real update takes over permanently.
    assert_eq!(
        time_of_day_after(&adapter, &set_time_with_clock(800, 0, 13_000, 1.0)),
        13_000
    );
    assert_eq!(time_of_day_after(&adapter, &set_time_sync_only(820)), 13_020);
}

#[test]
fn handle_play_set_time_emits_time_changed() {
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SET_TIME,
            &set_time_golden(),
        )
        .expect("handle set_time");
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::TimeChanged {
            world_age: 1000,
            time_of_day: 6000,
        })]
    );
}

#[test]
fn handle_play_set_time_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = set_time_golden();
    payload.push(0x00);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_TIME,
        &payload,
    );
    assert!(result.is_err(), "a misaligned set_time must be rejected");
}

// --- The registry-resolved day clock ---------------------------------------

/// A `respawn` body pointing at dimension-type holder `holder_id` and level
/// `dimension`. Built off [`respawn_golden`] so the rest of the layout stays in
/// one place; only the two fields this section is about vary.
fn respawn_into(holder_id: u8, dimension: &str) -> Vec<u8> {
    let mut bytes = vec![holder_id];
    bytes.push(u8::try_from(dimension.len()).expect("short dimension name"));
    bytes.extend_from_slice(dimension.as_bytes());
    bytes.extend_from_slice(&respawn_golden()[1 + 1 + b"minecraft:the_nether".len()..]);
    bytes
}

/// A `registry_data` body with data-less entries, for `minecraft:world_clock`
/// (whose real entries *are* contentless — `record WorldClock()`).
fn world_clock_registry(entries: &[&str]) -> Vec<u8> {
    let mut w = Writer::default();
    w.string("minecraft:world_clock");
    w.var_i32(i32::try_from(entries.len()).expect("entry count"));
    for id in entries {
        w.string(id);
        w.bool(false);
    }
    w.into_vec()
}

/// The **real** `minecraft:dimension_type` payload a 26.2 server sent, captured
/// by `tests/live_registry_data.rs`. Used rather than a hand-built entry so the
/// `default_clock` values driving this selection come from the server, not from
/// this file.
fn captured_dimension_type_registry() -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/registry_data_dimension_type.hex");
    let text = std::fs::read_to_string(&path).expect("captured dimension_type fixture");
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect()
}

fn feed_configuration(adapter: &V770Adapter, payload: &[u8]) {
    adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Configuration,
            lodestone_v770::packet_ids::configuration::clientbound::REGISTRY_DATA,
            payload,
        )
        .expect("registry_data must decode");
}

fn respawn(adapter: &V770Adapter, holder_id: u8, dimension: &str) {
    adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::RESPAWN,
            &respawn_into(holder_id, dimension),
        )
        .expect("handle respawn");
}

/// A full-sync `set_time` carrying **both** clocks, as `createFullSyncPacket`
/// sends at join. The two tick counts differ, which is what makes the selection
/// observable at all — a fixture where both clocks agree passes either way.
fn set_time_full_sync(game_time: i64, overworld_ticks: u32, end_ticks: u32) -> Vec<u8> {
    let mut bytes = game_time.to_be_bytes().to_vec();
    bytes.push(0x02); // two clock updates
    for (holder, ticks) in [(0u8, overworld_ticks), (1u8, end_ticks)] {
        bytes.push(holder);
        let mut v = u64::from(ticks);
        loop {
            let byte = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                bytes.push(byte);
                break;
            }
            bytes.push(byte | 0x80);
        }
        bytes.extend_from_slice(&0.0_f32.to_be_bytes()); // partial_tick
        bytes.extend_from_slice(&1.0_f32.to_be_bytes()); // rate
    }
    bytes
}

/// In the End the day clock is `minecraft:the_end`, holder id `1`.
/// `day_clock`'s lowest-holder-id pick returned holder `0` — the *overworld's*
/// clock — in every dimension, so the End's sky followed overworld time. This is
/// vanilla's default behaviour, not a data-pack edge case.
///
/// The registry payloads are fed through `handle_packet` in the Configuration
/// state, exactly as a real join delivers them, so this also covers the
/// `REGISTRY_DATA` arm being reachable at all.
#[test]
fn in_the_end_the_resolved_clock_is_the_end_clock_not_the_lowest_holder_id() {
    let adapter = V770Adapter::new();
    feed_configuration(&adapter, &world_clock_registry(&["minecraft:overworld", "minecraft:the_end"]));
    feed_configuration(&adapter, &captured_dimension_type_registry());

    // Holder 2 is `minecraft:the_end` in the captured registry (entries arrive
    // alphabetically: overworld, overworld_caves, the_end, the_nether).
    respawn(&adapter, 2, "minecraft:the_end");
    assert_eq!(
        time_of_day_after(&adapter, &set_time_full_sync(1000, 6_000, 18_000)),
        18_000,
        "the End must follow its own clock (holder 1), not the overworld's"
    );

    // A portal trip back must move the selection with it.
    respawn(&adapter, 0, "minecraft:overworld");
    assert_eq!(
        time_of_day_after(&adapter, &set_time_full_sync(2000, 6_000, 18_000)),
        6_000,
        "back in the overworld the overworld clock wins again"
    );
}

/// The control for the test above, and the reason it is evidence of anything:
/// with **no** `registry_data` the very same packets select the lowest holder id,
/// i.e. the overworld's `6_000`, in the End. That is the old hardcoded
/// behaviour, and it is also the fallback a server sending no registries still
/// has to get.
#[test]
fn without_registry_data_the_end_still_falls_back_to_the_lowest_holder_id() {
    let adapter = V770Adapter::new();
    respawn(&adapter, 2, "minecraft:the_end");
    assert_eq!(
        time_of_day_after(&adapter, &set_time_full_sync(1000, 6_000, 18_000)),
        6_000,
        "no registry: the lowest-holder-id fallback, unchanged from before registry-driven resolution"
    );
}

/// A one-entry `modifyClock` broadcast for a clock we are **not** following must
/// not re-anchor us. Without this, a `/time set` in the overworld would drag an
/// End session's time of day with it — the original bug wearing a new hat.
#[test]
fn a_clock_update_for_another_dimension_does_not_re_anchor_us() {
    let adapter = V770Adapter::new();
    feed_configuration(&adapter, &world_clock_registry(&["minecraft:overworld", "minecraft:the_end"]));
    feed_configuration(&adapter, &captured_dimension_type_registry());
    respawn(&adapter, 2, "minecraft:the_end");

    // Anchor the End clock at 18_000.
    assert_eq!(
        time_of_day_after(&adapter, &set_time_full_sync(1000, 6_000, 18_000)),
        18_000
    );
    // Now the *overworld's* clock changes. We must keep extrapolating ours.
    assert_eq!(
        time_of_day_after(&adapter, &set_time_with_clock(1020, 0, 500, 1.0)),
        18_020,
        "an update for holder 0 must not become our time of day"
    );
}

/// The `respawn` arm emits the dimension type *before* `Respawned`, so a consumer
/// folding both sees the geometry before the level name that depends on it.
#[test]
fn respawn_emits_the_dimension_type_before_the_respawned_event() {
    let adapter = V770Adapter::new();
    feed_configuration(&adapter, &captured_dimension_type_registry());
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::RESPAWN,
            &respawn_into(3, "minecraft:the_nether"),
        )
        .expect("handle respawn");
    match directives.as_slice() {
        [
            Directive::Emit(ClientEvent::DimensionTypeChanged {
                holder_id,
                dimension_type,
            }),
            Directive::Emit(ClientEvent::Respawned { .. }),
        ] => {
            assert_eq!(*holder_id, 3);
            let info = dimension_type
                .as_ref()
                .expect("holder 3 resolves to minecraft:the_nether");
            assert_eq!(info.name.to_string(), "minecraft:the_nether");
            assert!(
                !info.has_skylight,
                "this is the bit `sky_default_for_dimension` reads"
            );
            assert_eq!(info.min_y, 0);
            assert_eq!(info.height, 256);
            assert_eq!(info.logical_height, 128);
        }
        other => panic!("expected DimensionTypeChanged then Respawned, got {other:?}"),
    }
}
