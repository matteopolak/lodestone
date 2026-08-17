//! **The first `set_experience`**: the XP bar has values to draw before the player
//! does anything.
//!
//! # The defect this guards
//!
//! Reported as *"the XP bar never appears"*, and confirmed missing **in survival as
//! well as creative** — which is what ruled out the obvious explanation. Vanilla does
//! hide the bar in creative, but it does so *client-side* (`Player.hasExperience`)
//! and still sends the packet; a server-side game-mode gate was never the cause.
//!
//! The cause was a missing producer. `ServerProtocol::encode_set_experience` and its
//! `V770ServerProtocol` implementation both existed, the client decodes
//! `SET_EXPERIENCE` into `ClientEvent::ExperienceChanged`, and the HUD draws the bar
//! from the folded value — but the only call site in `lodestone-server` was the
//! furnace-close arm, paying out banked smelting XP. A player who had never closed a
//! furnace was sent the packet zero times, so the bar had nothing to draw from.
//!
//! # Where the expected values come from
//!
//! The 26.2 decompile, read as a record definition rather than as a call site — the
//! distinction matters more here than usual:
//!
//! * **That it is sent at join at all**: `ServerPlayer.doTick` sends whenever
//!   `this.totalExperience != this.lastSentExp`, and `lastSentExp` is initialised to
//!   `-99999999`. So the comparison is true on the first tick after any join, even
//!   for a player with zero experience.
//! * **The wire order**: `ClientboundSetExperiencePacket`'s `write` method emits
//!   `writeFloat(experienceProgress)`, `writeVarInt(experienceLevel)`,
//!   `writeVarInt(totalExperience)`. Its *constructor* takes
//!   `(progress, total, level)`, and `doTick` calls it in that order — so reading the
//!   call site rather than the codec transposes level and total. They are adjacent
//!   VarInts, so a swap is wire-legal and silently wrong.
//!
//! The values themselves are decoded through the real [`V770Adapter`], which is the
//! mirror side the encoder was written against, so this is two independent
//! transcriptions agreeing rather than a round-trip through one.
//!
//! # What this file does not claim
//!
//! Only the **join** producer is gated here. Whether the client's
//! `ExperienceChanged` fold reaches the HUD frame is a separate question on the
//! client half, and a mis-routed consumer would present identically to a missing
//! producer.

use std::time::Duration;

use lodestone_core::Writer;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, VersionAdapter,
};
use lodestone_net::{Connection, Transport, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, IntegratedServer, MobHandle, NoEntities,
    serve_connection,
};
use lodestone_v770::packet_ids::{configuration, login, play};
use lodestone_v770::{V770Adapter, V770ServerProtocol};
use lodestone_world::World;
use uuid::Uuid;

mod common;
use common::unique_username;

/// A fresh player's experience: `Player`'s `experienceProgress`/`experienceLevel`/
/// `totalExperience` all start at zero.
///
/// A player file *without* XP now also produces this, which is what makes it the
/// control arm for [`a_rejoining_player_is_sent_the_experience_they_earned`] rather
/// than only the fresh-join expectation.
const FRESH: (f32, i32, i32) = (0.0, 0, 0);

/// The seeded rejoin state: 1557 lifetime points.
///
/// Derived from `Player.getXpNeededForNextLevel`, not from a run and not from a
/// memorable round number — the running sum of the curve is 1507 at level 31, so 1557
/// leaves 50 points against level 31's own cost of `112 + 1*9 = 121`. Every one of the
/// three numbers is different and none is 0 or 1, which is what makes a transposition
/// of the two adjacent VarInts (or of the two adjacent `Int` NBT fields) visible: a
/// level/total swap reads back as level 1557.
const REJOIN_TOTAL: i32 = 1_557;
const REJOIN_LEVEL: i32 = 31;
const REJOIN_PROGRESS: f32 = 50.0 / 121.0;

/// How far the restored bar may sit from the exact ratio.
///
/// `give_points` reaches level 31 through 31 carry re-expressions, each a multiply and a
/// divide in `f32`, so the landed value is `0.41322213` against `50/121 =
/// 0.41322314` — a drift of about `1e-6` that is arithmetic, not a bug. `1e-5` is loose
/// enough to absorb it and still an order of magnitude tighter than the nearest wrong
/// answer: the level-30 hypothesis would put the bar at `50/112 = 0.446`.
const PROGRESS_TOLERANCE: f32 = 1e-5;

/// Flat ground at y=63.
///
/// **Not an all-air world, and that is load-bearing rather than incidental.** The
/// creative arm below opens a *persistent* world, whose constructor resolves a world
/// spawn; with nothing solid anywhere that resolution never yields a placement and
/// the connection never reaches Play at all. Measured while writing this file: the
/// all-air version of this fixture drained two configuration packets and then went
/// quiet, which reads exactly like "the packet is missing" rather than like a broken
/// fixture. A floor costs nothing and removes the whole failure mode.
#[derive(Debug)]
struct FlatWorld;

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 63, z, "minecraft:grass_block[snowy=false]");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

fn handshake_bytes() -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(776);
    w.string("localhost");
    w.u16(25565);
    w.var_i32(2);
    w.into_vec()
}

fn hello_bytes(name: &str, uuid: Uuid) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(name);
    w.uuid(uuid);
    w.into_vec()
}

async fn drain<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    const QUIET: Duration = Duration::from_millis(400);
    let mut out = Vec::new();
    while let Ok(Ok(Some(packet))) = tokio::time::timeout(QUIET, client.read_packet()).await {
        out.push(packet);
    }
    out
}

async fn join<T: Transport>(
    client: &mut Connection<T>,
    name: &str,
    uuid: Uuid,
) -> Vec<(i32, Vec<u8>)> {
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client.write_packet(0, &hello_bytes(name, uuid)).await.unwrap();
    let _ = common::read_login_packet(client).await;
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    let _ = common::read_login_packet(client).await;
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
    drain(client).await
}

/// Serves one connection over an in-memory pair and returns the client end.
///
/// This is the survival arm: `serve_connection_inner` hardcodes
/// `GameMode::Survival` for a join with no saved player file, so an in-memory world
/// is survival by construction. Creative needs a seeded file — see
/// [`a_creative_join_sends_the_experience_bar_too`].
fn serve() -> Connection<tokio::io::DuplexStream> {
    let (client_io, server_io) = memory_pair();
    tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        let _ = serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &FlatWorld,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await;
    });
    Connection::new(client_io)
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-join-xp-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}

/// Every `set_experience` in `packets`, decoded through the real client adapter as
/// `(progress, level, total)`.
fn experiences(packets: &[(i32, Vec<u8>)]) -> Vec<(f32, i32, i32)> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::SET_EXPERIENCE)
        .map(|(_, payload)| {
            let directives = V770Adapter::new()
                .handle_packet(
                    &mut World::new(),
                    ConnectionState::Play,
                    play::clientbound::SET_EXPERIENCE,
                    payload,
                )
                .expect("the server's own set_experience must decode");
            match directives.as_slice() {
                [
                    Directive::Emit(ClientEvent::ExperienceChanged {
                        progress,
                        level,
                        total,
                    }),
                ] => (*progress, *level, *total),
                other => panic!("expected an ExperienceChanged event, got {other:?}"),
            }
        })
        .collect()
}

/// A joining player is sent exactly one `set_experience`, carrying a fresh player's
/// zeroes, **in survival** — the mode the bug was confirmed in.
#[tokio::test]
async fn a_survival_join_sends_the_experience_bar() {
    let mut client = serve();
    let joined = join(&mut client, &unique_username(), Uuid::new_v4()).await;

    let sent = experiences(&joined);
    assert_eq!(
        sent.len(),
        1,
        "a join must carry exactly one set_experience — zero is the reported bug \
         (the bar has no values to draw and never appears). doTick sends it on the \
         first tick after any join because lastSentExp starts at -99999999"
    );
    assert_eq!(
        sent[0], FRESH,
        "a fresh player's bar is progress 0.0, level 0, total 0, in that wire order \
         (writeFloat progress, writeVarInt level, writeVarInt total). Note the \
         vanilla *constructor* takes progress/total/level -- transposing the two \
         VarInts is wire-legal and silently shows the wrong number"
    );
}

/// And in creative, for the same reason the report singled it out: vanilla hides the
/// bar client-side via `Player.hasExperience` but its **server** still sends the
/// packet, so a server-side game-mode gate would be a divergence.
///
/// A separate test rather than a loop over both modes so a failure names the mode.
///
/// Creative is reached the only way a join can reach it — a saved player file whose
/// `playerGameType` says so, which is what `serve_connection_inner` consults before
/// falling back to its hardcoded `GameMode::Survival`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_creative_join_sends_the_experience_bar_too() {
    let dir = tempdir("creative");
    let (_server, client_io, world) = IntegratedServer::open_persistent_with_mobs(
        V770ServerProtocol,
        &dir,
        FlatWorld,
        -64,
        384,
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
        Duration::from_secs(3600),
    )
    .expect("open persistent world");

    let store = world
        .world_registries()
        .expect("a persistent source answers Some")
        .player_data
        .expect("a persistent world exposes its player store");

    let uuid = Uuid::new_v4();
    store
        .write(
            uuid,
            &lodestone_server::player_data::PlayerData {
                pos: lodestone_model::Vec3::new(0.5, 64.0, 0.5),
                game_mode: Some(GameMode::Creative),
                ..Default::default()
            },
        )
        .expect("seed a creative player file");

    let mut client = Connection::new(client_io);
    let joined = join(&mut client, &unique_username(), uuid).await;

    let sent = experiences(&joined);
    assert_eq!(
        sent.len(),
        1,
        "creative is not a reason to withhold the packet: vanilla's hasExperience \
         check is in the client's HUD, not in ServerPlayer.doTick"
    );
    assert_eq!(sent[0], FRESH, "same fresh-player values as survival");
}

/// Opens a persistent world, seeds one player file, joins as that uuid, and returns
/// every `set_experience` the join carried.
///
/// One helper for both arms below so the *only* difference between them is the
/// experience written into the file — a second copy of the fixture could differ
/// somewhere else and the control would prove nothing.
async fn join_with_saved_experience(
    scratch: &str,
    experience: lodestone_server::experience::PlayerExperience,
) -> Vec<(f32, i32, i32)> {
    let dir = tempdir(scratch);
    let (_server, client_io, world) = IntegratedServer::open_persistent_with_mobs(
        V770ServerProtocol,
        &dir,
        FlatWorld,
        -64,
        384,
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
        Duration::from_secs(3600),
    )
    .expect("open persistent world");

    let store = world
        .world_registries()
        .expect("a persistent source answers Some")
        .player_data
        .expect("a persistent world exposes its player store");

    let uuid = Uuid::new_v4();
    store
        .write(
            uuid,
            &lodestone_server::player_data::PlayerData {
                pos: lodestone_model::Vec3::new(0.5, 64.0, 0.5),
                experience,
                ..Default::default()
            },
        )
        .expect("seed the player file");

    let mut client = Connection::new(client_io);
    let joined = join(&mut client, &unique_username(), uuid).await;
    experiences(&joined)
}

/// **XP survives a rejoin.** The bar a returning player is sent is the one their file
/// carries, not zeros.
///
/// # The defect
///
/// `PlayerExperience::restored` had only test callers: a rejoining player's live XP was
/// `default()` while the `.dat` faithfully kept `XpP`/`XpLevel`/`XpTotal` through
/// `PlayerData::preserved` and wrote them back on every save. So XP survived the *file*
/// and not the *session* — earn 31 levels, quit, come back at zero, and the file still
/// says 31.
///
/// # Why the numbers discriminate
///
/// See [`REJOIN_TOTAL`]. The three values are distinct, so this fails loudly on a
/// level/total transposition in either the NBT schema or the packet — the two places
/// this data crosses a boundary between fields of the same type.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejoining_player_is_sent_the_experience_they_earned() {
    let mut experience = lodestone_server::experience::PlayerExperience::default();
    experience.give_points(REJOIN_TOTAL);
    assert_eq!(
        (experience.level(), experience.total()),
        (REJOIN_LEVEL, REJOIN_TOTAL),
        "the fixture itself must be level 31 before the join is asked about it"
    );

    let sent = join_with_saved_experience("rejoin", experience).await;
    assert_eq!(sent.len(), 1, "a rejoin carries exactly one set_experience too");
    let (progress, level, total) = sent[0];
    assert_eq!(
        level, REJOIN_LEVEL,
        "the restored level did not reach the wire; 0 means nothing reads the file's \
         XpLevel, and 1557 would be a level/total transposition"
    );
    assert_eq!(
        total, REJOIN_TOTAL,
        "the restored lifetime total did not reach the wire; 31 would be the same \
         transposition seen from the other side"
    );
    assert!(
        (progress - REJOIN_PROGRESS).abs() < PROGRESS_TOLERANCE,
        "the restored bar arrived as {progress}, not 50/121 — level 31 costs 121 points"
    );
}

/// **The control, and it fires.** The same fixture with a zero-XP file produces zeros.
///
/// Without this arm the gate above is satisfied by anything that happens to answer 31 —
/// a hardcoded seed, a default that drifted, a value read from the wrong player. Run it
/// and watch it disagree with the arm above at every one of the three fields.
///
/// (A file whose `Xp*` fields are *absent* entirely — what the pre-fix writer produced —
/// decodes to the same zeros through `PlayerData::from_nbt`'s per-field fallback, so
/// this is the same arm reached the cheaper way.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_a_saved_player_with_no_experience_still_joins_at_zero() {
    let sent = join_with_saved_experience(
        "rejoin-control",
        lodestone_server::experience::PlayerExperience::default(),
    )
    .await;
    assert_eq!(sent.len(), 1, "the control must reach Play the same way");
    assert_eq!(
        sent[0], FRESH,
        "a saved player who earned nothing must still be sent zeros — if this arm \
         reports 31 the restore is reading something other than the file"
    );
    assert_ne!(
        sent[0].1, REJOIN_LEVEL,
        "the two arms must disagree, or neither measures the restore"
    );
}
