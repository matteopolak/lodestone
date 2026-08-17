//! A block change originating in the **world tick loop** must carry light.
//!
//! # The defect
//!
//! `serve_play`'s `container_sync_tick` arm drains `BlockTickFeed` — every block
//! change the world tick loop made, which no inbound packet drives — and forwarded
//! `encode_block_update` and nothing else. `block_update` carries no light, and the
//! client deliberately never recomputes light on the live path (`crate::net`'s own
//! invariant: *"MP consumes server light; SP computes it"*). So every block change
//! from the tick loop moved on the client and left its light behind, stale until the
//! player rejoined and the column was re-encoded from scratch.
//!
//! The owner's report was a torch placed underwater. That is a **compound edit
//! across two paths**, which is why it survived: `apply_use_item_on` relights
//! correctly for the placement, so the torch lights the water; a tick later the
//! fluid tick destroys it and that half arrives on *this* drain, so the torch
//! disappeared and its light did not. "Until I log out and back in" is the tell —
//! a rejoin re-sends light from scratch, which fits "the live update was never
//! sent" and not "the client miscomputed".
//!
//! Fire spreading and going out, crops and grass, a redstone torch flipping `lit`
//! and a falling block landing all ride the same drain and had the same defect.
//!
//! # What this gate asserts, and what it deliberately does not
//!
//! It asserts the **wire exists**: that a tick-loop block update is followed by a
//! light send for that column, over the real `IntegratedServer` tick loop rather
//! than a hand-driven feed. `BlockTickFeed::publish` is `pub(crate)`, so an
//! integration test cannot fake the producer — which is the right constraint here,
//! because a faked producer would not prove the tick loop reaches this arm at all.
//!
//! It does **not** assert the light *values*. Those are gated against an outside
//! expectation elsewhere (`crates/protocol/v770/tests/light_update.rs` pins the
//! encoder against a hand-written golden body, and `live_block_light.rs` diffs the
//! engine against a real vanilla server). Restating them here from our own
//! computation would be the closed loop this repo's evidence standard forbids. The
//! missing link was never the value; it was that nothing sent one.
//!
//! The negative control is `a_drain_with_no_block_changes_sends_no_light` — it must
//! observe zero, so a gate that counted light sends from some other cause could not
//! read as a pass.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::State;
use lodestone_net::Connection;
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

const MIN_Y: i32 = 0;
const HEIGHT: i32 = 16;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
/// Bounded, and generous enough for a loaded machine. Random ticks are random, so
/// this polls rather than asserting on a fixed tick — but it is a deadline, not a
/// sleep: the assertion is on what was observed, never on time having passed.
const DEADLINE: Duration = Duration::from_secs(25);

/// What the two counters below record. Shared with the protocol double so the test
/// reads the server's own encoder calls rather than parsing a stand-in wire format.
#[derive(Debug, Default)]
struct Observed {
    /// `encode_block_update` calls, by position.
    block_updates: Mutex<Vec<(i32, i32, i32)>>,
    /// `compute_column_light` calls — the first half of the light send, and the one
    /// that cannot be reached without the drain asking for it.
    light_sends: AtomicUsize,
}

#[derive(Debug)]
struct WatchingProtocol(Arc<Observed>);

impl ServerProtocol for WatchingProtocol {
    /// A real login state machine, and it is **required**, not scaffolding: the
    /// drain under test lives in `serve_play`'s `container_sync_tick` arm, which is
    /// only reached once the connection is in Play. The first version of this file
    /// answered `Ignored` to everything, so the connection never left Login and no
    /// drain ever ran — the gate failed on its own precondition rather than
    /// reporting a green that measured nothing, which is the whole reason that
    /// precondition assertion is there.
    fn decode(&self, state: State, packet_id: i32, _payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => ServerBound::LoginStart {
                username: "LightWatch".to_string(),
                uuid: Uuid::nil(),
            },
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            _ => ServerBound::Ignored,
        }
    }
    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: LOGIN_SUCCESS,
            payload: Vec::new(),
        }]
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }
    fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
        ServerDirective::None
    }
    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, _state: &str) -> ServerDirective {
        self.0
            .block_updates
            .lock()
            .expect("block update lock")
            .push((x, y, z));
        ServerDirective::None
    }

    /// The observation point. `send_column_light` calls this before
    /// `encode_light_update`, and it is unreachable unless the drain asks — which
    /// is exactly the link that was missing.
    fn compute_column_light(&self, _column: &ChunkColumn) -> Option<lodestone_world::ColumnLight> {
        self.0.light_sends.fetch_add(1, Ordering::SeqCst);
        // `None` so the caller takes its documented column-resend fallback, whose
        // directives this double answers with `None` too. What is under test is
        // whether the drain *asks* for light, not which of the two carriers it
        // then picks; the carriers are gated in the v770 crate.
        None
    }
}

/// Grass **covered by stone**, everywhere — so `crate::random_tick`'s grass↔dirt
/// family really mutates and the tick loop publishes a block change with no player
/// action at all.
///
/// The cover is the load-bearing part, and the first version of this fixture got it
/// wrong: grass with *air* above it survives, so the tick loop published nothing and
/// the precondition assertion below fired on the first run. Vanilla's
/// `GrassBlock.canBeGrass` kills grass under a light-blocking block, which is what
/// `dampening` 15 above it means — see `random_tick`'s own table.
///
/// **The light-relevant case is a torch destroyed by water, not this.** Grass↔dirt
/// moves neither emission nor dampening, and using it here is deliberate: it makes
/// the gate a test of the *plumbing* at the cheapest input that reaches it, while
/// the fix is unconditional per drained column precisely because the feed carries
/// no old state to predicate on. A fixture that needed the value to change would be
/// testing the light engine, which is gated elsewhere against a vanilla server.
#[derive(Debug)]
struct GrassWorld;

impl ChunkSource for GrassWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 4, z, "minecraft:stone");
                column.set_block(x, 5, z, "minecraft:grass_block");
                // The cover. Without it the grass survives and nothing publishes.
                column.set_block(x, 6, z, "minecraft:stone");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; the tick loop's mutations are observed on the wire, not here.
    }
}

/// A world the tick loop can never publish a block change for: bare stone, no
/// randomly-ticking block anywhere. The control fixture.
#[derive(Debug)]
struct InertWorld;

impl ChunkSource for InertWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                for y in 0..6 {
                    column.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage.
    }
}

async fn run<S: ChunkSource + 'static>(source: S, deadline: Duration) -> Arc<Observed> {
    let observed = Arc::new(Observed::default());
    let (server, client) = IntegratedServer::open_in_memory_with_mobs(
        WatchingProtocol(Arc::clone(&observed)),
        source,
        (0..=0, 0..=0),
        (8, 8),
        0,
        1,
    );
    // Drive the login so the connection actually reaches Play — see
    // `WatchingProtocol::decode` for why that is a precondition and not ceremony.
    let mut client = Connection::new(client);
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    client
        .write_packet(LOGIN_START, &[0])
        .await
        .expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let start = tokio::time::Instant::now();
    while start.elapsed() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !observed
            .block_updates
            .lock()
            .expect("block update lock")
            .is_empty()
            && observed.light_sends.load(Ordering::SeqCst) > 0
        {
            break;
        }
    }
    server.shutdown().await;
    observed
}

/// **The gate.** Once the tick loop has published a block change, the drain that
/// forwards it must also send that column's light.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tick_loop_block_change_is_followed_by_a_light_send() {
    let observed = run(GrassWorld, DEADLINE).await;

    let updates = observed
        .block_updates
        .lock()
        .expect("block update lock")
        .len();
    // The precondition, checked rather than assumed: with no tick-loop change at
    // all the light assertion below would be about nothing, which is the
    // precondition species of vacuous test.
    assert!(
        updates > 0,
        "no tick-loop block change was published in {DEADLINE:?}, so this gate would \
         assert nothing about the light that follows one"
    );
    assert!(
        observed.light_sends.load(Ordering::SeqCst) > 0,
        "{updates} tick-loop block update(s) reached the client and not one light send \
         followed: block_update carries no light and the client never recomputes it, \
         so the column's light is now stale until the player rejoins"
    );
}

/// **The negative control, and it must observe zero.** Bare stone has nothing that
/// randomly ticks, so the drain never runs a change — and therefore never asks for
/// light. Without this, a light counter incremented by anything else (the join
/// encode, a mob, a timer) would make the gate above pass regardless.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drain_with_no_block_changes_sends_no_light() {
    // Short: this one is waiting to observe *nothing*, so the deadline is a cost
    // rather than a headroom. Several container-sync drains fit in it.
    let observed = run(InertWorld, Duration::from_secs(3)).await;

    assert_eq!(
        observed
            .block_updates
            .lock()
            .expect("block update lock")
            .len(),
        0,
        "bare stone has no randomly-ticking block, so the tick loop must publish nothing"
    );
    assert_eq!(
        observed.light_sends.load(Ordering::SeqCst),
        0,
        "with no block change drained there is nothing to relight; a non-zero count \
         here means the gate above is counting light sends from some other cause and \
         proves nothing"
    );
}
