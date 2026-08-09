//! Does dropping an undecodable packet leave the reader at the next frame
//! boundary?
//!
//! `lodestone_client`'s driver is deliberately fail-open on
//! `AdapterError::Decode`: it logs *"dropping undecodable packet and continuing
//! session"* and keeps reading, on the argument that the wire is
//! forward-compatible and open-ended so a client that dies on the first
//! unrecognised structure turns every future server-side addition into an
//! outage. That argument is only sound if the drop **resynchronises**. If it
//! does not, the policy is strictly worse than failing fast: it logs a
//! reassuring message and then corrupts the session, so the visible failure is a
//! generic codec error with no link to the real cause.
//!
//! # Why this file is in `lodestone-v770` and not `lodestone-client`
//!
//! `lodestone-client`'s own `decode_error_drops_packet_and_keeps_session` asserts
//! the same *shape* against a `FakeAdapter` whose failing arm is
//! `fail_on(state, id)` — it rejects the packet **without reading a single byte
//! of the payload**, and the payloads it is given are empty. So it cannot
//! distinguish "the reader resynchronised" from "there was nothing to consume":
//! it is a *world*-species vacuous test for this question, exemplary source and
//! all. The consumption profile is the whole variable, so the decoder has to be
//! the real one.
//!
//! Both arms here therefore drive the **real** `V770Adapter` through the **real**
//! driver over a **real** `Connection`, with compression on, and the two arms
//! bracket the failing decode's byte count from both sides:
//!
//! | arm | failing decode consumes | mechanism |
//! |---|---|---|
//! | [`a_dropped_packet_that_stops_early_does_not_desync`] | far **fewer** bytes than the frame holds | an advancement icon carrying an unmodeled component: `read_component_patch` returns at the unmodeled arm with the entire rest of the packet unread |
//! | [`a_dropped_packet_that_reads_past_its_end_does_not_desync`] | the whole frame, **and then asks for more** | the same packet with its advancement count inflated, so the decode walks off the end and fails with `unexpected end of input` |
//!
//! Either would desync a decoder reading from the stream. Neither may desync one
//! reading from a fully-buffered frame, which is what `Connection::read_packet`
//! hands the adapter.
//!
//! # The frame is deliberately larger than one transport read
//!
//! `Connection`'s scratch buffer is 8 KiB, so a frame bigger than that is
//! assembled from several `read` calls inside `Codec`'s receive buffer. That is
//! the second candidate failure mode — an error path discarding the wrong amount
//! of a multi-read frame — and an advancements packet is exactly the packet big
//! enough to hit it in the wild. [`big_undecodable_advancements`] builds a
//! poorly-compressible body so the *wire* frame stays over 8 KiB even with
//! compression on, and [`the_undecodable_frame_really_spans_several_reads`] is
//! the control that the payload is actually that big — without it, a fixture
//! that quietly compressed down to 300 bytes would leave both arms above
//! measuring the single-read case only.

use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress};
use lodestone_core::{Ctx, Encode, Nbt, Writer, write_network_nbt};
use lodestone_data::items::item_id;
use lodestone_net::{Codec, Connection, memory_pair};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::{configuration, login, play};
use lodestone_v770::packets::login::{LoginCompression, LoginFinished};
use tokio::io::DuplexStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 776 };

/// Vanilla's own default `network-compression-threshold`. Using it rather than
/// disabling compression keeps `Codec::next_packet`'s `decompress_frame` arm in
/// the path, which is where a frame-boundary mistake would actually live.
const THRESHOLD: i32 = 256;

/// `Connection`'s per-read scratch size. Restated rather than imported because it
/// is private to that module; the assertion that depends on it is
/// [`the_undecodable_frame_really_spans_several_reads`], which fails loudly if
/// this drifts in the wrong direction.
const READ_CHUNK: usize = 8 * 1024;

/// Health values with no plausible default: a decoder that delivered a
/// zeroed-out or partially-read `set_health` cannot match these.
const HEALTH: f32 = 13.5;
const FOOD: i32 = 17;
const SATURATION: f32 = 2.25;

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

/// Starts the real driver against a hand-scripted server on an in-memory pair,
/// and walks it to `Play` with compression enabled.
///
/// Only the packets the adapter needs to change state are scripted; the client's
/// own outbound packets (handshake, login hello, the brand announcement, the
/// login and configuration acknowledgements) are left in the 64 KiB duplex
/// buffer unread, which is well inside it.
async fn play_session() -> (
    lodestone_client::ClientHandle,
    lodestone_client::EventStream,
    Connection<DuplexStream>,
) {
    let (client_io, server_io) = memory_pair();
    let (handle, events) = ClientBuilder::new(
        // RFC 2606 `.invalid`, so nothing here can resolve to a real host.
        ServerAddress {
            host: "resync.invalid".into(),
            port: 25565,
        },
        LoginProfile {
            username: "resync".into(),
            uuid: Uuid::nil(),
        },
        Box::new(V770Adapter::new()),
    )
    .connect_with(client_io);
    let mut server = Connection::new(server_io);

    // Compression first, exactly as vanilla orders it: the client's codec flips
    // on the `SetCompression` directive this packet decodes to, so ours must flip
    // immediately afterwards or every later frame is misread by one side.
    server
        .write_packet(
            login::clientbound::LOGIN_COMPRESSION,
            &encode(&LoginCompression {
                threshold: THRESHOLD,
            }),
        )
        .await
        .expect("write login_compression");
    server.set_compression(THRESHOLD);

    server
        .write_packet(
            login::clientbound::LOGIN_FINISHED,
            &encode(&LoginFinished {
                profile_id: Uuid::from_u128(0x1234),
                name: "resync".to_owned(),
                properties: Vec::new(),
                session_id: Uuid::from_u128(0x5678),
            }),
        )
        .await
        .expect("write login_finished");

    // Configuration -> Play. The adapter answers with its own
    // `finish_configuration` and a `SetState(Play)`, which is all this gate needs
    // from the join; no registry or `login` packet is required to make the
    // adapter dispatch a Play packet.
    server
        .write_packet(configuration::clientbound::FINISH_CONFIGURATION, &[])
        .await
        .expect("write finish_configuration");

    (handle, events, server)
}

/// Waits for a `HealthChanged` event, failing rather than hanging.
///
/// Every event the join emits before it is skipped, so this asserts *delivery of
/// a specific packet's content*, not "some event arrived" — the latter would pass
/// against a session that had lost the `set_health` entirely and merely emitted
/// something from the login.
async fn expect_health(events: &mut lodestone_client::EventStream) -> (f32, i32, f32) {
    let deadline = Duration::from_secs(5);
    let found = tokio::time::timeout(deadline, async {
        while let Some(event) = events.recv().await {
            if let ClientEvent::HealthChanged {
                health,
                food,
                saturation,
            } = event
            {
                return Some((health, food, saturation));
            }
        }
        None
    })
    .await;
    match found {
        Ok(Some(values)) => values,
        Ok(None) => panic!(
            "the event stream ended before the set_health after the dropped packet was \
             delivered — the drop did not resynchronise"
        ),
        Err(_) => panic!(
            "no set_health event within {deadline:?} after the dropped packet — the drop \
             did not resynchronise"
        ),
    }
}

/// A `set_health` body with the distinctive values above
/// (`SetHealth`: f32 health, VarInt food, f32 saturation).
fn set_health_payload() -> Vec<u8> {
    let mut w = Writer::default();
    w.f32(HEALTH);
    w.var_i32(FOOD);
    w.f32(SATURATION);
    w.into_vec()
}

/// An `update_advancements` payload over 8 KiB whose **last** advancement's icon
/// is a `minecraft:decorated_pot` carrying `component_name` as its only
/// component.
///
/// Pass a component this build models and the packet decodes; pass one it does
/// not and `read_item_stack_template` raises the fatal icon cliff with the whole
/// remaining payload — thousands of bytes — still unread. That difference is the
/// consumption profile the first arm measures.
///
/// The filler advancements carry deliberately low-redundancy titles so the frame
/// does not compress away; see
/// [`the_undecodable_frame_really_spans_several_reads`].
fn big_undecodable_advancements(component_name: &str, filler: usize) -> Vec<u8> {
    let mut w = Writer::default();
    w.bool(false); // reset
    w.var_i32(i32::try_from(filler + 1).expect("filler fits"));

    // A cheap LCG, so the titles are text a zlib window cannot fold. Fixed seed:
    // the fixture must be identical on every run.
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut noise = |len: usize| {
        let mut out = String::with_capacity(len);
        for _ in 0..len {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let byte = ((state >> 33) % 62) as u8;
            out.push(match byte {
                0..=9 => (b'0' + byte) as char,
                10..=35 => (b'a' + byte - 10) as char,
                _ => (b'A' + byte - 36) as char,
            });
        }
        out
    };

    for index in 0..filler {
        write_advancement(&mut w, &format!("minecraft:filler/n{index}"), &noise(96), None);
    }
    // The last one is the interesting one: its icon carries `component_name`.
    write_advancement(
        &mut w,
        "minecraft:adventure/craft_decorated_pot_using_only_sherds",
        &noise(96),
        Some(component_name),
    );

    w.var_i32(0); // no removed advancements
    w.var_i32(0); // no progress entries
    w.bool(true); // showAdvancements
    w.into_vec()
}

/// Writes one `AdvancementHolder`.
///
/// `DisplayInfo`'s wire order is the packet's, not the datapack schema's: title,
/// description, icon, frame ordinal, then a **raw big-endian `int`** flag word
/// (`serializeToNetwork` uses `writeInt`, not a byte), then the background
/// identifier only when bit 0 is set, then x and y as floats. The icon is an
/// `ItemStackTemplate`, whose fields are item-then-count — the reverse of
/// `ItemStack.OPTIONAL_STREAM_CODEC`.
fn write_advancement(w: &mut Writer, id: &str, title: &str, icon_component: Option<&str>) {
    w.string(id);
    w.bool(false); // no parent
    w.bool(true); // has display info
    write_network_nbt(w, &Nbt::String(title.to_owned())).expect("title nbt");
    write_network_nbt(w, &Nbt::String("d".to_owned())).expect("description nbt");
    match icon_component {
        None => {
            w.var_i32(item_id("minecraft:stone").expect("known item"));
            w.var_i32(1);
            w.var_i32(0); // no added components
            w.var_i32(0); // none removed
        }
        Some(name) => {
            w.var_i32(item_id("minecraft:decorated_pot").expect("known item"));
            w.var_i32(1);
            w.var_i32(1); // one added component
            w.var_i32(0); // none removed
            w.var_i32(component_id(name));
            // Every component this is used with takes a network-NBT payload, so
            // the bytes are well-formed either way — what varies is only whether
            // the decoder recognises the id.
            write_network_nbt(w, &Nbt::Compound(vec![("x".to_owned(), Nbt::Int(1))]))
                .expect("component nbt");
        }
    }
    w.var_i32(0); // frame ordinal: task
    w.i32(0); // flag word: no background, no toast, not hidden
    w.f32(0.0); // x
    w.f32(0.0); // y
    w.var_i32(0); // no requirement groups
    w.bool(false); // sends_telemetry_event
}

/// Resolves a data-component-type id from its canonical name, so no numeric
/// component id is hardcoded here.
fn component_id(name: &str) -> i32 {
    (0..)
        .find(|&id| lodestone_data::data_component_types::component_type_name(id) == Some(name))
        .expect("known component type")
}

// ---------------------------------------------------------------------------
// The control: the fixture really is bigger than one transport read
// ---------------------------------------------------------------------------

/// Both resync arms claim to exercise a frame assembled from several transport
/// reads. This is what makes that claim checkable: it frames the fixture through
/// the real `Codec` at the real threshold and asserts the **wire** bytes exceed
/// `Connection`'s scratch buffer.
///
/// Without it the premise could be false in the safe-looking direction — a
/// repetitive filler string would compress to a few hundred bytes, both arms
/// would still pass, and they would be measuring only the single-read case. The
/// magnitude is predicted, not merely signed: 400 filler advancements at ~96
/// random alphanumerics each is ~40 KiB of near-incompressible text, so the
/// framed result must clear 8 KiB by a wide margin rather than by one byte.
#[test]
fn the_undecodable_frame_really_spans_several_reads() {
    let payload = big_undecodable_advancements("minecraft:custom_data", 400);
    let mut body = Writer::default();
    body.var_i32(play::clientbound::UPDATE_ADVANCEMENTS);
    body.bytes(&payload);

    let mut codec = Codec::new();
    codec.set_compression(THRESHOLD);
    let mut wire = Vec::new();
    codec.encode(body.as_slice(), &mut wire).expect("frame it");

    assert!(
        wire.len() > READ_CHUNK * 2,
        "the fixture must span several {READ_CHUNK}-byte reads to exercise the \
         partial-frame path, but framed to only {} bytes — the filler is \
         compressing away and both resync arms are measuring the single-read \
         case",
        wire.len()
    );
}

// ---------------------------------------------------------------------------
// Arm 1: the failing decode stops early
// ---------------------------------------------------------------------------

/// An `update_advancements` frame whose icon carries an unmodeled component is
/// dropped with **thousands of bytes of its own frame unread**, and the very next
/// packet still decodes and is delivered with its exact content.
///
/// This is the shape that took a real session down: the advancement
/// `adventure/craft_decorated_pot_using_only_sherds` has a
/// `minecraft:decorated_pot` icon, and before `minecraft:pot_decorations` was
/// modeled its patch stopped `read_component_patch`, which
/// `read_item_stack_template` turns into a fatal decode error for the whole
/// packet.
#[tokio::test]
async fn a_dropped_packet_that_stops_early_does_not_desync() {
    let (handle, mut events, mut server) = play_session().await;

    server
        .write_packet(
            play::clientbound::UPDATE_ADVANCEMENTS,
            // `minecraft:custom_data` is a real component id this build still
            // does not model, so this is genuinely undecodable rather than
            // malformed — the bytes are exactly what a server would send.
            &big_undecodable_advancements("minecraft:custom_data", 400),
        )
        .await
        .expect("write the undecodable advancements frame");
    server
        .write_packet(play::clientbound::SET_HEALTH, &set_health_payload())
        .await
        .expect("write set_health");

    let (health, food, saturation) = expect_health(&mut events).await;
    assert_eq!(health, HEALTH, "the packet after the drop lost its health");
    assert_eq!(food, FOOD, "the packet after the drop lost its food level");
    assert_eq!(
        saturation, SATURATION,
        "the packet after the drop lost its saturation"
    );
    assert!(
        !handle.is_finished(),
        "the session must survive a dropped packet"
    );
    drop(handle);
}

/// The positive control for the arm above: the **same** frame with a component
/// this build *does* model decodes fully, so the fixture is a real
/// `update_advancements` packet and the arm above is measuring a decode failure
/// rather than a packet the adapter ignores outright.
///
/// Ignoring an unhandled id is `handle_packet`'s other behaviour and it is
/// indistinguishable from a drop at the event stream. Without this, both resync
/// arms would pass against an adapter that had simply stopped dispatching
/// `update_advancements`.
#[tokio::test]
async fn the_same_frame_with_a_modeled_component_decodes_fully() {
    let (handle, mut events, mut server) = play_session().await;

    server
        .write_packet(
            play::clientbound::UPDATE_ADVANCEMENTS,
            &big_undecodable_advancements("minecraft:custom_name", 400),
        )
        .await
        .expect("write the advancements frame");

    let deadline = Duration::from_secs(5);
    let added = tokio::time::timeout(deadline, async {
        while let Some(event) = events.recv().await {
            if let ClientEvent::AdvancementsUpdated { added, .. } = event {
                return Some(added.len());
            }
        }
        None
    })
    .await
    .expect("an AdvancementsUpdated within the deadline")
    .expect("the stream must not end first");

    assert_eq!(
        added, 401,
        "the fixture is a well-formed advancements packet: 400 filler plus the \
         decorated pot"
    );
    assert!(!handle.is_finished());
    drop(handle);
}

// ---------------------------------------------------------------------------
// Arm 2: the failing decode reads past the end of its frame
// ---------------------------------------------------------------------------

/// A frame whose decode walks off the **end** of the payload is dropped, and the
/// next packet still decodes and is delivered.
///
/// The advancement count is inflated by one past what the body carries, so the
/// decoder consumes every byte of the frame and then asks for more, failing with
/// `unexpected end of input` — the same core error Matthew's session died with,
/// raised here from inside the adapter rather than from the transport. This is
/// the opposite consumption profile from the arm above and the one that would
/// desync a decoder reading from the stream instead of from a buffered frame.
#[tokio::test]
async fn a_dropped_packet_that_reads_past_its_end_does_not_desync() {
    let (handle, mut events, mut server) = play_session().await;

    // Rebuild with the count one higher than the number of advancements written.
    let mut over = big_undecodable_advancements("minecraft:custom_name", 8);
    // The count is the second field (`reset` is one byte) and 9 fits in one
    // VarInt byte, so bumping it in place keeps the rest of the payload intact.
    assert_eq!(over[1], 9, "the advancement count is where this expects it");
    over[1] = 10;

    server
        .write_packet(play::clientbound::UPDATE_ADVANCEMENTS, &over)
        .await
        .expect("write the over-reading advancements frame");
    server
        .write_packet(play::clientbound::SET_HEALTH, &set_health_payload())
        .await
        .expect("write set_health");

    let (health, food, saturation) = expect_health(&mut events).await;
    assert_eq!(health, HEALTH);
    assert_eq!(food, FOOD);
    assert_eq!(saturation, SATURATION);
    assert!(
        !handle.is_finished(),
        "the session must survive a packet whose decode ran off the end"
    );
    drop(handle);
}
