//! The initial join must admit a complete playable footprint before the centre
//! column can be the loading signal.
//!
//! This is intentionally a server-side wire/admission gate. Client residency
//! is not the same thing as mesh settlement, so this test observes the source
//! generation and the source-aware initial encoder directly, then checks that
//! the authoritative integrated tick loop still advances beside the join.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::{Reader, State, Writer};
use lodestone_net::Connection;
use lodestone_server::dimension::Dimension;
use lodestone_server::{
    ChunkColumn, ChunkEncodeError, ChunkGenerationStage, ChunkSource, ColumnLightSettlement,
    IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
const CHUNK_BATCH_START: i32 = 10;
const CHUNK: i32 = 0x27;
const CHUNK_BATCH_FINISHED: i32 = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Generated {
        coord: (i32, i32),
        stage: ChunkGenerationStage,
    },
    Encoded {
        coord: (i32, i32),
        neighbours: usize,
    },
}

#[derive(Clone, Default)]
struct AdmissionProbe {
    events: Arc<Mutex<Vec<Event>>>,
}

impl AdmissionProbe {
    fn record(&self, event: Event) {
        self.events
            .lock()
            .expect("admission event log lock poisoned")
            .push(event);
    }

    fn snapshot(&self) -> Vec<Event> {
        self.events
            .lock()
            .expect("admission event log lock poisoned")
            .clone()
    }

    fn column(&self, cx: i32, cz: i32, stage: ChunkGenerationStage) -> ChunkColumn {
        self.record(Event::Generated {
            coord: (cx, cz),
            stage,
        });
        let mut column = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                column.set_block(x, 8, z, "minecraft:stone");
            }
        }
        column
    }
}

impl ChunkSource for AdmissionProbe {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.column(cx, cz, ChunkGenerationStage::Full)
    }

    fn column_at(&self, cx: i32, cz: i32, stage: ChunkGenerationStage) -> ChunkColumn {
        self.column(cx, cz, stage)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz, ChunkGenerationStage::Full)
            .block_state(lx, y, lz)
            .to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz, ChunkGenerationStage::Full)
            .biome_state_at(lx, y, lz)
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// A small wire vocabulary that reaches the real shared integrated join path.
struct ProbeProtocol {
    probe: AdmissionProbe,
}

impl ServerProtocol for ProbeProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match (state, packet_id) {
            (State::Handshaking, HANDSHAKE) => ServerBound::Handshake {
                next_state: State::Login,
            },
            (State::Login, LOGIN_START) => {
                let mut reader = Reader::new(payload);
                ServerBound::LoginStart {
                    username: reader.string(16).expect("username"),
                    uuid: Uuid::nil(),
                }
            }
            (State::Login, LOGIN_ACKNOWLEDGED) => ServerBound::LoginAcknowledged,
            (State::Configuration, FINISH_CONFIGURATION) => {
                ServerBound::ConfigurationFinished
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        let mut writer = Writer::default();
        writer.string(username);
        vec![ServerDirective::Send {
            packet_id: LOGIN_SUCCESS,
            payload: writer.as_slice().to_vec(),
        }]
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_START,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
        self.record_encoded((cx, cz), 0);
        Self::chunk_packet(cx, cz)
    }

    fn try_encode_chunk_with_neighbours_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        _column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        _dimension: Dimension,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        self.record_encoded((cx, cz), neighbours.len());
        Ok(Self::chunk_packet(cx, cz))
    }

    fn compute_initial_column_lights_with_neighbours_in_dimension(
        &self,
        _column: &ChunkColumn,
        _neighbours: &[(i32, i32, ChunkColumn)],
        _dimension: Dimension,
    ) -> Option<ColumnLightSettlement> {
        // Returning None drives the source-aware encoder's explicit fallback,
        // while still requiring it to capture the complete neighbour footprint.
        None
    }

    fn uses_cross_column_light(&self) -> bool {
        true
    }

    fn retains_initial_column_light(&self) -> bool {
        true
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        let mut writer = Writer::default();
        writer.var_i32(batch_size);
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_FINISHED,
            payload: writer.as_slice().to_vec(),
        }
    }
}

impl ProbeProtocol {
    fn record_encoded(
        &self,
        coord: (i32, i32),
        neighbours: usize,
    ) {
        self.probe.record(Event::Encoded { coord, neighbours });
    }

    fn chunk_packet(cx: i32, cz: i32) -> ServerDirective {
        let mut writer = Writer::default();
        writer.var_i32(cx);
        writer.var_i32(cz);
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: writer.as_slice().to_vec(),
        }
    }
}

async fn read_join_prefix(
    client: &mut Connection<tokio::net::TcpStream>,
    centre: (i32, i32),
) -> Vec<(i32, i32)> {
    let mut chunks = Vec::new();
    let mut in_batch = false;
    loop {
        let (id, payload) = client
            .read_packet()
            .await
            .expect("read join packet")
            .expect("join connection closed early");
        match id {
            CHUNK_BATCH_START => {
                assert!(!in_batch, "join opened a nested chunk batch");
                in_batch = true;
            }
            CHUNK => {
                assert!(in_batch, "join chunk escaped its batch");
                let mut reader = Reader::new(&payload);
                chunks.push((
                    reader.var_i32().expect("chunk x"),
                    reader.var_i32().expect("chunk z"),
                ));
                if chunks.last().copied() == Some(centre) {
                    return chunks;
                }
            }
            CHUNK_BATCH_FINISHED => {
                assert!(in_batch, "join closed a chunk batch that was not open");
                panic!("join batch finished before its centre column was sent");
            }
            _ => {}
        }
    }
}

async fn read_remaining_join_chunks(
    client: &mut Connection<tokio::net::TcpStream>,
    mut chunks: Vec<(i32, i32)>,
    expected: usize,
) -> Vec<(i32, i32)> {
    let mut batch_open = true;
    let mut batch_count = chunks.len();
    loop {
        let (id, payload) = client
            .read_packet()
            .await
            .expect("read remaining join packet")
            .expect("join connection closed before its batch finished");
        match id {
            CHUNK_BATCH_START => {
                assert!(!batch_open, "join opened a nested chunk batch");
                batch_open = true;
                batch_count = 0;
            }
            CHUNK => {
                assert!(batch_open, "join chunk escaped its batch");
                let mut reader = Reader::new(&payload);
                chunks.push((
                    reader.var_i32().expect("chunk x"),
                    reader.var_i32().expect("chunk z"),
                ));
                batch_count += 1;
            }
            CHUNK_BATCH_FINISHED => {
                assert!(batch_open, "join closed a chunk batch that was not open");
                let reported = Reader::new(&payload).var_i32().expect("batch size") as usize;
                assert_eq!(reported, batch_count, "batch marker disagrees with chunk packets");
                batch_open = false;
                if chunks.len() == expected {
                    break;
                }
            }
            _ => {}
        }
    }
    assert_eq!(chunks.len(), expected);
    chunks
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn initial_join_admits_full_near_terrain_before_the_centre_packet() {
    let probe = AdmissionProbe::default();
    let server = IntegratedServer::bind(
        "127.0.0.1:0",
        ProbeProtocol {
            probe: probe.clone(),
        },
        probe.clone(),
        9,
    )
    .await
    .expect("bind integrated server");
    let address = server.local_addr().expect("bound server has address");
    let stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect integrated server");
    let mut client = Connection::new(stream);

    client.write_packet(HANDSHAKE, &[2]).await.expect("handshake");
    let mut login = Writer::default();
    login.string("Readiness");
    client
        .write_packet(LOGIN_START, login.as_slice())
        .await
        .expect("login start");
    let (id, payload) = client
        .read_packet()
        .await
        .expect("read login success")
        .expect("login success present");
    assert_eq!(id, LOGIN_SUCCESS);
    assert_eq!(Reader::new(&payload).string(16).expect("login name"), "Readiness");
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login acknowledgement");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let prefix = read_join_prefix(&mut client, (0, 0)).await;
    assert!(
        prefix.len() < 361,
        "the centre packet must release before the full view is drained"
    );

    let events = probe.snapshot();
    let centre_event = events
        .iter()
        .position(|event| matches!(event, Event::Encoded { coord: (0, 0), .. }))
        .expect("the centre column must be encoded");
    let neighbours = match events[centre_event] {
        Event::Encoded { neighbours, .. } => neighbours,
        Event::Generated { .. } => unreachable!("centre_event selects an encoded event"),
    };
    assert_eq!(
        neighbours, 8,
        "the centre packet needs the complete 3x3 light footprint"
    );

    let readiness =
        lodestone_server::join_readiness::InitialJoinReadiness::new((0, 0), 9);
    let admitted_before_centre = events[..centre_event]
        .iter()
        .filter_map(|event| match event {
            Event::Generated {
                coord,
                stage: ChunkGenerationStage::Full,
            } => Some(*coord),
            Event::Generated { .. } | Event::Encoded { .. } => None,
        });
    assert!(
        readiness.area_is_admitted(admitted_before_centre),
        "the centre may not be released until its complete 3x3 terrain footprint is admitted"
    );
    let generated_before_centre: HashSet<_> = events[..centre_event]
        .iter()
        .filter_map(|event| match event {
            Event::Generated { coord, .. } => Some(*coord),
            Event::Encoded { .. } => None,
        })
        .collect();
    assert!(
        generated_before_centre.len() < 361,
        "initial admission must not generate the whole view before releasing the centre"
    );

    let before_ticks = server
        .tick_stats()
        .expect("integrated server exposes tick stats")
        .tick_count;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let after_ticks = server
        .tick_stats()
        .expect("integrated server exposes tick stats")
        .tick_count;
    assert!(
        after_ticks > before_ticks,
        "the authoritative tick loop must continue during join"
    );

    let chunks = read_remaining_join_chunks(&mut client, prefix, 361).await;
    assert!(chunks.contains(&(0, 0)), "the join must include its centre column");

    drop(client);
    server.shutdown().await;
}
