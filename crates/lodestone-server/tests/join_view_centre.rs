//! The join view must be centred on the column the player actually joins in.
//!
//! # The defect this gate exists for
//!
//! `crate::server`'s `join_view_rings` yields Chebyshev-ring **offsets**
//! `(dx, dz)`, and the join loop handed them straight to `encode_chunk` as
//! absolute chunk coordinates. So the square that went out was always centred on
//! chunk `(0, 0)`, while `begin_play_at` teleported the player to the real spawn
//! and `ViewTracker::new` seeded its "already sent" set around the player's own
//! column. Three consequences, all reported by the owner as one symptom:
//!
//! * terrain appeared away from the player rather than around them;
//! * the column under the player's feet never arrived, so the client's
//!   `Loading terrain...` predicate (the player's own column being loaded) stayed
//!   true;
//! * the tracker believed those columns *had* been sent, so walking did not
//!   repair it either — only a rejoin whose spawn happened to floor to `(0, 0)`.
//!
//! # Where the expected value comes from
//!
//! Not from either construction of the square. The assertion is a **cross-arm
//! invariant between two independent server outputs**: the chunk-cache centre the
//! server announces to the client (derived in the protocol layer from the spawn
//! position) and the centre of the set of columns it then streams (derived in the
//! server layer from the ring walk). Those must be the same column or the client
//! is being told its view is somewhere it is not. Neither side is this test's own
//! arithmetic, and the defect was exactly their disagreement.
//!
//! # Why every existing join gate was blind to it
//!
//! `tests/serve_play.rs`'s two join gates assert against `square(0, 0, radius)`
//! with a fixture whose spawn floors to chunk `(0, 0)` — the one input at which
//! "offsets" and "absolute coordinates" are the same numbers. That is the *world*
//! species of vacuous test in `DESIGN.md` §12.43: the source reads as rigorous and
//! the flaw is in the input. This fixture puts land **only** in a non-origin chunk
//! so the spawn spiral in `world_spawn::find_initial_spawn` has to walk off the
//! origin, and the two readings then differ by exactly the spawn chunk.
//!
//! The negative control is `origin_land_is_the_input_that_cannot_discriminate`
//! below: the same assertions over an origin-centred fixture, which pass both
//! before and after the fix, run so the discriminating input is a measured fact
//! rather than a claim.

use std::collections::HashSet;
use std::sync::Mutex;

use lodestone_core::{Reader, State, Writer};
use lodestone_model::{GameMode, Vec3};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, ServerBound,
    ServerDirective, ServerProtocol, serve_connection,
};
use uuid::Uuid;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;

/// This fixture's own packet ids. Deliberately not the real v770 numbers: this
/// gate is about which *columns* `serve_connection` chooses, and a stand-in wire
/// format keeps the wire-fidelity question where it belongs (the v770 crate's own
/// encoder tests).
const CHUNK_S2C: i32 = 90;
const CACHE_CENTER_S2C: i32 = 91;

/// The Y of the fixture's floor, inside the column's `0..16` extent and clear of
/// the top so `world_spawn`'s downward scan reaches it through air.
const FLOOR_Y: i32 = 8;

/// The one chunk that has ground in [`IslandSource`].
///
/// `(3, -2)` rather than something further out because
/// `world_spawn::spiral_chunk_offsets` only visits the ±5-chunk box: a fixture
/// whose only land sits outside that box falls back to `(8, 64, 8)` — chunk
/// `(0, 0)` — and would silently become the non-discriminating input this file
/// exists to avoid. `island_land_is_inside_the_spawn_spirals_reach` asserts that
/// premise rather than trusting it.
const ISLAND: (i32, i32) = (3, -2);

/// Land in exactly one chunk, air everywhere else, so the world spawn search has
/// to leave the origin.
struct IslandSource {
    /// Which chunk holds the ground. `None` means "every chunk does" — the
    /// control fixture, whose spawn lands at the origin.
    island: Option<(i32, i32)>,
}

impl ChunkSource for IslandSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 16);
        if self.island.is_none_or(|island| island == (cx, cz)) {
            for lx in 0..16 {
                for lz in 0..16 {
                    column.set_block(lx, FLOOR_Y, lz, "minecraft:stone");
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
        // No storage; this fixture serves fresh columns by design.
    }
}

/// Emits only the two things this gate reads: the chunk-cache centre and one
/// packet per streamed column, each carrying its coordinates.
#[derive(Default)]
struct CoordProto {
    /// The centre `encode_chunk_cache_center` was called with, so the test can
    /// compare the server's own announcement against the columns it streamed
    /// without re-deriving either.
    announced_centre: Mutex<Option<(i32, i32)>>,
}

impl ServerProtocol for CoordProto {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut r = Reader::new(payload);
                ServerBound::LoginStart {
                    username: r.string(16).expect("username"),
                    uuid: Uuid::nil(),
                }
            }
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

    /// The announcing arm. `encode_chunk_cache_center` is never called by
    /// `serve_connection` directly — in the real families it is emitted from
    /// inside `begin_play_at`, which is where the spawn position is in scope, so
    /// a double that does not override this one records nothing.
    ///
    /// The `/ 16.0` floor is transcribed from `V770ServerProtocol::begin_play_at`,
    /// which is the point: this arm's only input is the `spawn` the server passed
    /// down, and the other arm's only input is the ring walk. Two derivations from
    /// two different places in the join, which is what makes their agreement worth
    /// asserting — it is not a round trip through one expression.
    fn begin_play_at(
        &self,
        _view_radius: i32,
        spawn: Vec3,
        _mode: GameMode,
    ) -> Vec<ServerDirective> {
        vec![self.encode_chunk_cache_center(
            (spawn.x / 16.0).floor() as i32,
            (spawn.z / 16.0).floor() as i32,
        )]
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
        let mut w = Writer::default();
        w.i32(cx);
        w.i32(cz);
        ServerDirective::Send {
            packet_id: CHUNK_S2C,
            payload: w.as_slice().to_vec(),
        }
    }

    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        *self.announced_centre.lock().expect("centre lock") = Some((cx, cz));
        let mut w = Writer::default();
        w.i32(cx);
        w.i32(cz);
        ServerDirective::Send {
            packet_id: CACHE_CENTER_S2C,
            payload: w.as_slice().to_vec(),
        }
    }
}

/// Drives a real join to completion and returns `(announced centre, streamed
/// columns)`.
///
/// `view_radius` is small on purpose: this gate is about *which* columns, and the
/// centre-vs-offset difference is visible at any radius above 0. Radius 0 would
/// make the whole square one column and the two hypotheses coincide again, so
/// `2` is the smallest radius that can still fail.
async fn join(island: Option<(i32, i32)>, view_radius: i32) -> ((i32, i32), HashSet<(i32, i32)>) {
    let expected = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let (client_end, server_end) = memory_pair();
    let source = IslandSource { island };
    let proto = CoordProto::default();

    let mut client = Connection::new(client_end);
    let handshake = async {
        client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
        let mut w = Writer::default();
        w.string("Centred");
        client
            .write_packet(LOGIN_START, w.as_slice())
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

        let mut centre = None;
        let mut columns = HashSet::new();
        while columns.len() < expected {
            let Ok(Some((id, payload))) = client.read_packet().await else {
                break;
            };
            let mut r = Reader::new(&payload);
            match id {
                CACHE_CENTER_S2C => {
                    centre = Some((r.i32().expect("cx"), r.i32().expect("cz")));
                }
                CHUNK_S2C => {
                    columns.insert((r.i32().expect("cx"), r.i32().expect("cz")));
                }
                _ => {}
            }
        }
        (centre, columns)
    };

    let mut conn = Connection::new(server_end);
    let block_entities = BlockEntityHandle::default();
    let mobs = MobHandle::default();
    let server = serve_connection(
        &mut conn,
        &proto,
        &source,
        &NoEntities,
        view_radius,
        &block_entities,
        &mobs,
    );

    let (result, _) = tokio::join!(handshake, server);
    let (centre, columns) = result;
    (
        centre.expect("the server must announce a chunk cache centre at join"),
        columns,
    )
}

/// The square around `centre`, in columns — the geometry both server arms are
/// supposed to be describing, stated once here so neither implementation is the
/// expectation.
fn square(centre: (i32, i32), radius: i32) -> HashSet<(i32, i32)> {
    let mut out = HashSet::new();
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            out.insert((centre.0 + dx, centre.1 + dz));
        }
    }
    out
}

/// **The premise check.** If [`ISLAND`] were outside the spawn spiral's ±5-chunk
/// box the fixture would fall back to chunk `(0, 0)` and the gate below would
/// silently become the non-discriminating control — a *precondition*-species
/// vacuity, and the reason this is asserted rather than reasoned about.
#[test]
fn island_land_is_inside_the_spawn_spirals_reach() {
    assert!(
        ISLAND.0.abs() <= 5 && ISLAND.1.abs() <= 5,
        "the island must be inside the ±5-chunk spiral world_spawn searches, or the \
         spawn falls back to chunk (0, 0) and this file's gate measures nothing"
    );
    assert_ne!(
        ISLAND,
        (0, 0),
        "the island must NOT be the origin chunk — that is the input at which the \
         off-centre defect is invisible"
    );
}

/// The gate. The columns streamed at join must be the square around the column
/// the server told the client its view is centred on.
#[tokio::test]
async fn the_join_view_is_centred_on_the_column_the_server_announced() {
    let radius = 2;
    let (centre, streamed) = join(Some(ISLAND), radius).await;

    assert_eq!(
        centre, ISLAND,
        "the spawn search must have left the origin for this input, or the assertion \
         below cannot discriminate"
    );
    assert_eq!(
        streamed,
        square(centre, radius),
        "the streamed columns must be the square around the announced centre; a \
         difference means the client was told its view is somewhere the terrain is not"
    );
    assert!(
        !streamed.contains(&(0, 0)) || centre == (0, 0),
        "with a centre {centre:?} at radius {radius} the origin column is outside the \
         view — its presence would mean the ring offsets were used as absolute \
         coordinates"
    );
}

/// **The control, run rather than described.** The identical assertions over an
/// origin-centred fixture pass whether the centre is applied or not, which is
/// the measured reason every pre-existing join gate was blind to this.
#[tokio::test]
async fn origin_land_is_the_input_that_cannot_discriminate() {
    let radius = 2;
    let (centre, streamed) = join(None, radius).await;

    assert_eq!(
        centre,
        (0, 0),
        "land everywhere makes the origin chunk the first valid spiral candidate"
    );
    assert_eq!(streamed, square((0, 0), radius));
    // And the whole point: at this centre the raw offsets *are* the square, so
    // the gate above needs its non-origin fixture to say anything at all.
    assert_eq!(streamed, square(centre, radius));
}
