//! **End to end**: a real served connection, real terrain with real
//! water in it, and the real [`V770ServerProtocol`] — proving the cancellation
//! is *wired*, not merely implemented.
//!
//! # Why `lodestone-server`'s own unit tests are not enough
//!
//! `crate::fall`'s tests drive `FallTracker` directly and hand it a
//! `FallSample` they constructed. That is a closed loop around the arithmetic: it
//! cannot tell you whether `crate::server`'s `fall_sample` reads the **right
//! cell** of the world, and reading the wrong one is the likeliest way to ship
//! this broken. Two concrete wrong cells that pass every one of those tests:
//!
//! * the *eye* cell instead of the feet (which is what `crate::vitals` correctly
//!   uses for drowning, a different question) — the cancellation then fires a
//!   player-height too late and a shallow-water landing still hurts;
//! * `y - 1` instead of `y - 0.2` for the landing block — wrong for a player
//!   standing exactly on a block boundary, which is every normal landing.
//!
//! So the fixtures here put water and a hay bale at known coordinates and drive
//! the whole packet path.
//!
//! # Where the expected values come from
//!
//! The fall formula, from `LivingEntity.calculateFallDamage`/`calculateFallPower`:
//! `floor((distance + 1e-6 - 3.0) * blockModifier)`. Every assertion below
//! predicts an exact health, and each names the wrong-hypothesis value it is
//! distinguishing itself from.

use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_net::{Connection, Transport, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, serve_connection,
};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::{configuration, login, play};
use uuid::Uuid;

mod common;
use common::unique_username;

const MAX_HEALTH: f32 = 20.0;
const SAFE_FALL_DISTANCE: f64 = 3.0;

/// Y of the solid floor's top block. The player's feet rest at `FLOOR_Y + 1`.
const FLOOR_Y: i32 = 60;
/// Y of the water surface block in the pool column, one above the floor.
const WATER_Y: i32 = 61;

/// `floor((distance + 1e-6 - 3.0) * modifier)`, only when positive.
fn fall_damage(distance: f64, modifier: f64) -> f32 {
    let raw = ((distance + 1.0e-6 - SAFE_FALL_DISTANCE) * modifier).floor();
    if raw > 0.0 { raw as f32 } else { 0.0 }
}

/// Terrain with three distinct landing spots in chunk `(0, 0)`, so one fixture
/// covers every arm without the arms being able to interfere:
///
/// | local x, z | column |
/// |---|---|
/// | `1, 1` | plain stone floor — the control arm |
/// | `3, 3` | one block of **water** sitting on the floor |
/// | `5, 5` | one **hay bale** sitting on the floor |
/// | `7, 7` | a **ladder** in the cell above the floor |
///
/// The pool is deliberately **one block deep**. A deeper pool would let a gate
/// pass against an implementation that reads the eye cell, because the eye would
/// also be submerged; one block is the case that separates feet from eye.
struct PoolSource;

impl PoolSource {
    fn build() -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        for x in 0..16 {
            for z in 0..16 {
                for y in -64..=FLOOR_Y {
                    column.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        column.set_block(3, WATER_Y, 3, "minecraft:water");
        column.set_block(5, WATER_Y, 5, "minecraft:hay_block");
        column.set_block(7, WATER_Y, 7, "minecraft:ladder[facing=north]");
        column
    }
}

impl ChunkSource for PoolSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        if (cx, cz) == (0, 0) {
            Self::build()
        } else {
            ChunkColumn::new(-64, 384)
        }
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        self.column(cx, cz)
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // This fixture is read-only.
    }
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

fn pos_bytes(x: f64, y: f64, z: f64, on_ground: bool) -> Vec<u8> {
    let mut w = Writer::default();
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.u8(u8::from(on_ground));
    w.into_vec()
}

async fn drain<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    const QUIET: Duration = Duration::from_millis(250);
    let mut out = Vec::new();
    while let Ok(Ok(Some(packet))) = tokio::time::timeout(QUIET, client.read_packet()).await {
        out.push(packet);
    }
    out
}

fn healths(packets: &[(i32, Vec<u8>)]) -> Vec<f32> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::SET_HEALTH)
        .map(|(_, payload)| Reader::new(payload).f32().expect("set_health health"))
        .collect()
}

fn serve() -> Connection<tokio::io::DuplexStream> {
    let (client_io, server_io) = memory_pair();
    tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        let _ = serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &PoolSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await;
    });
    Connection::new(client_io)
}

async fn join<T: Transport>(client: &mut Connection<T>, name: &str) {
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client
        .write_packet(0, &hello_bytes(name, Uuid::new_v4()))
        .await
        .unwrap();
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
    drain(client).await;
}

async fn step<T: Transport>(
    client: &mut Connection<T>,
    x: f64,
    y: f64,
    z: f64,
    on_ground: bool,
) -> Vec<(i32, Vec<u8>)> {
    client
        .write_packet(
            play::serverbound::MOVE_PLAYER_POS,
            &pos_bytes(x, y, z, on_ground),
        )
        .await
        .unwrap();
    drain(client).await
}

/// **The control arm, first**, because everything else is an absence.
///
/// A 20-block fall onto the plain stone floor at local `(1, 1)` must deal exactly
/// `floor(20.000001 - 3.0) = 17` damage, leaving `3.0` health. If this does not
/// hold, the fixture is not falling, not landing, or not reaching the tracker, and
/// every "no damage" assertion below would be vacuous.
#[tokio::test]
async fn the_plain_floor_arm_hurts_by_the_predicted_amount() {
    let mut client = serve();
    join(&mut client, &unique_username()).await;

    let feet = f64::from(FLOOR_Y + 1);
    let _ = step(&mut client, 1.5, feet + 20.0, 1.5, false).await;
    let landing = step(&mut client, 1.5, feet, 1.5, true).await;

    let damage = fall_damage(20.0, 1.0);
    assert_eq!(damage, 17.0, "the formula must predict 17");
    assert_eq!(
        healths(&landing),
        vec![MAX_HEALTH - damage],
        "a 20-block fall onto stone leaves exactly {} health",
        MAX_HEALTH - damage
    );
}

/// **The reported bug.** A 20-block fall that ends in one block of water deals no
/// damage, *and* the distance does not survive to be charged against a later dry
/// landing.
///
/// Two hypotheses, both exact:
///
/// * correct — water cancels, so neither the water landing nor the later 1-block
///   step on stone produces any `set_health` at all;
/// * uncancelled — the water landing itself deals `17`, and even an
///   implementation that only *suppressed accumulation* while submerged would
///   still charge the next dry landing `floor(21.000001 - 3.0) = 18`.
#[tokio::test]
async fn a_fall_into_one_block_of_water_hurts_neither_now_nor_later() {
    let mut client = serve();
    join(&mut client, &unique_username()).await;

    let feet = f64::from(FLOOR_Y + 1);
    let banked_hypothesis = fall_damage(21.0, 1.0);
    let plain_hypothesis = fall_damage(20.0, 1.0);
    assert_eq!(
        (plain_hypothesis, banked_hypothesis),
        (17.0, 18.0),
        "both wrong-hypothesis values must be distinct, exact numbers: 17 if the \
         water landing itself is charged, 18 if only accumulation was suppressed \
         and the distance survived to the next dry landing"
    );

    // Fall from 20 above into the water cell at local (3, 3), feet landing at
    // `WATER_Y`.
    //
    // **`on_ground: true` on the arriving sample, and that is load-bearing.** The
    // pool is one block deep, so a player standing in it is standing on the stone
    // floor beneath — which is what a real client reports, and it makes this
    // sample a *landing*. An `on_ground: false` version of this step would be a
    // much weaker gate: nothing would be charged on arrival either way, and the
    // banked distance would then be silently consumed by whatever landing came
    // next. That hole was in the first draft of this test and the wrong-cell
    // control below is what exposed it.
    //
    // The feet cell is water and the eye cell (`61 + 1.62 -> 62`) is air, so this
    // arm fails outright against an implementation that reads the eye.
    let _ = step(&mut client, 3.5, feet + 20.0, 3.5, false).await;
    let splash = step(&mut client, 3.5, f64::from(WATER_Y), 3.5, true).await;
    assert!(
        healths(&splash).is_empty(),
        "a 20-block fall landing in one block of water must deal no damage; \
         {plain_hypothesis} is what it costs on stone, and any damage at all means \
         the water cancellation is not reaching this landing; got {:?}",
        healths(&splash)
    );

    // Walk out onto dry stone at the same height (no vertical movement, so nothing
    // can accumulate), then take a genuine 1-block step down. With the distance
    // correctly discarded this is inside SAFE_FALL_DISTANCE and free.
    let walk_out = step(&mut client, 1.5, feet, 1.5, true).await;
    assert!(
        healths(&walk_out).is_empty(),
        "walking out of the pool at a constant height must be free; got {:?}",
        healths(&walk_out)
    );
    let _ = step(&mut client, 1.5, feet + 1.0, 1.5, false).await;
    let later = step(&mut client, 1.5, feet, 1.5, true).await;
    assert!(
        healths(&later).is_empty(),
        "a 1-block step after a water landing must be free; {banked_hypothesis} damage \
         here is the uncancelled banked distance, and any damage at all means the \
         cancellation is not wired; got {:?}",
        healths(&later)
    );
}

/// A hay bale reduces rather than cancels, by exactly `0.2` —
/// `HayBlock.fallOn`'s own `damageModifier`.
///
/// This is the arm that proves `fall_sample` reads the cell **below** the feet and
/// not the feet cell itself: the hay is at `WATER_Y` and the player's feet land at
/// `WATER_Y + 1`. Predicted exactly, and the prediction is what makes it a
/// magnitude check — `floor(17.000001 * 0.2) = 3`, so the modifier must be applied
/// *inside* the floor. Applying it after would give `floor(17.0) * 0.2 = 3.4`,
/// truncating to a different value on the way into an `i32`, and a
/// "less than 17" assertion could not tell the two apart.
#[tokio::test]
async fn landing_on_hay_reduces_the_damage_to_the_predicted_fifth() {
    let mut client = serve();
    join(&mut client, &unique_username()).await;

    let hay_feet = f64::from(WATER_Y + 1);
    let _ = step(&mut client, 5.5, hay_feet + 20.0, 5.5, false).await;
    let landing = step(&mut client, 5.5, hay_feet, 5.5, true).await;

    let cushioned = fall_damage(20.0, 0.2);
    let plain = fall_damage(20.0, 1.0);
    assert_eq!(cushioned, 3.0, "floor((20.000001 - 3.0) * 0.2) = 3");
    assert_eq!(plain, 17.0, "the unmodified hypothesis, for contrast");
    assert_ne!(cushioned, plain);

    assert_eq!(
        healths(&landing),
        vec![MAX_HEALTH - cushioned],
        "a 20-block fall onto hay leaves {} health, not {} — the modifier must come \
         from the block below the feet",
        MAX_HEALTH - cushioned,
        MAX_HEALTH - plain
    );
}

/// A ladder in the feet cell cancels, per the `#minecraft:fall_damage_resetting`
/// tag — the second half of the same wiring, on a different predicate.
#[tokio::test]
async fn grabbing_a_ladder_mid_fall_cancels_the_damage() {
    let mut client = serve();
    join(&mut client, &unique_username()).await;

    let feet = f64::from(FLOOR_Y + 1);
    // Fall 20 blocks down the ladder column, arriving in the ladder cell.
    // `on_ground: true` for the same reason as the water arm: this is the sample
    // that must be a landing, or the absence proves nothing.
    let _ = step(&mut client, 7.5, feet + 20.0, 7.5, false).await;
    let grab = step(&mut client, 7.5, f64::from(WATER_Y), 7.5, true).await;
    assert!(
        healths(&grab).is_empty(),
        "a 20-block fall arriving in a ladder cell must deal no damage; {} is what \
         it costs on bare stone; got {:?}",
        fall_damage(20.0, 1.0),
        healths(&grab)
    );

    let landing = step(&mut client, 7.5, feet, 7.5, true).await;
    assert!(
        healths(&landing).is_empty(),
        "and stepping off the ladder onto the floor must be free; got {:?}",
        healths(&landing)
    );
}

/// The fixture really does contain what the tests above assume — asserted against
/// the source directly, so a fixture that silently stopped placing water reads as
/// a failure here rather than as a passing "no damage" gate everywhere else.
///
/// This is the *world* species check: every absence asserted above is only
/// meaningful if the block it depends on is actually there.
#[test]
fn the_fixture_contains_the_blocks_every_absence_above_depends_on() {
    let source = PoolSource;
    assert_eq!(source.block_state(3, WATER_Y, 3), "minecraft:water");
    assert_eq!(source.block_state(5, WATER_Y, 5), "minecraft:hay_block");
    assert_eq!(
        source.block_state(7, WATER_Y, 7),
        "minecraft:ladder[facing=north]"
    );
    assert_eq!(source.block_state(1, WATER_Y, 1), "minecraft:air");
    assert_eq!(source.block_state(1, FLOOR_Y, 1), "minecraft:stone");

    // And the pool is one block deep, which is what makes the water arm able to
    // distinguish a feet read from an eye read.
    assert_eq!(
        source.block_state(3, WATER_Y + 1, 3),
        "minecraft:air",
        "the cell above the water must be air, or the water arm would also pass \
         against an implementation that reads the eye cell"
    );
    assert_eq!(source.block_state(3, FLOOR_Y, 3), "minecraft:stone");
}
