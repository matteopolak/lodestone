//! Cross-cutting unit tests for the server driver: chunk admission and initial
//! encoding, join batches, armour and attribute snapshots, and the protocol
//! fakes they share. Tests scoped to one submodule live in that module's own
//! `tests.rs`.

use super::*;
use crate::chunk::ChunkColumn;
use lodestone_model::Vec3;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

#[test]
fn player_tick_gate_releases_world_and_vitals_together() {
    let world = crate::world_state::WorldStateHandle::new();
    world.pause_initial_ticks();
    assert!(!player_tick_ready(&world, false));
    assert!(world.initial_ticks_paused());
    assert!(player_tick_ready(&world, true));
    assert!(!world.initial_ticks_paused());
}

#[test]
fn a_silent_client_counts_as_loaded_after_sixty_vitals_ticks() {
    let (mut loaded, mut waited) = (false, 0);
    for _ in 0..59 {
        tick_client_load_timeout(&mut loaded, &mut waited);
    }
    assert!(!loaded, "59 ticks is still inside the loading window");
    tick_client_load_timeout(&mut loaded, &mut waited);
    assert!(loaded, "the 60th tick ends the wait");
    // A respawn clears `loaded`; the wait starts over from zero.
    loaded = false;
    for _ in 0..59 {
        tick_client_load_timeout(&mut loaded, &mut waited);
    }
    assert!(!loaded);
}

#[test]
fn streamed_centre_admission_requests_only_its_missing_neighbours() {
    let neighbours = column_admission_neighbours(7, -3, 1);
    assert_eq!(neighbours.len(), 8);
    assert!(!neighbours.contains(&(7, -3)));

    let expected: HashSet<_> = column_admission_footprint(7, -3, 1)
        .into_iter()
        .filter(|&pos| pos != (7, -3))
        .collect();
    assert_eq!(neighbours.into_iter().collect::<HashSet<_>>(), expected);

    assert!(
        column_admission_neighbours(7, -3, 0).is_empty(),
        "one-column protocols already received their centre from the stream"
    );
}

pub(super) struct NetherPacketAdmissionProtocol;

impl ServerProtocol for NetherPacketAdmissionProtocol {
    fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
        ServerBound::Ignored
    }

    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _join: &crate::protocol::JoinGame) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, _cx: i32, _cz: i32, column: &ChunkColumn) -> ServerDirective {
        ServerDirective::Send {
            packet_id: 1,
            payload: vec![u8::from(
                column.block_state_id(0, 2, 12)
                    == StateId::from_state_str("minecraft:gravel").expect("fixture gravel state"),
            )],
        }
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
}

struct RequestAdmissionProtocol;

impl ServerProtocol for RequestAdmissionProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        NetherPacketAdmissionProtocol.decode(state, packet_id, payload)
    }
    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        NetherPacketAdmissionProtocol.login_success(username, uuid)
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_play(&self, _join: &crate::protocol::JoinGame) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        NetherPacketAdmissionProtocol.encode_chunk(cx, cz, column)
    }
    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
    fn uses_cross_column_light(&self) -> bool {
        true
    }
}

#[derive(Default)]
struct RequestAdmissionSource {
    requests: Mutex<Vec<(i32, i32)>>,
}

impl ChunkSource for RequestAdmissionSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.requests.lock().unwrap().push((cx, cz));
        let mut column = ChunkColumn::new(0, 16);
        column.set_block_id(0, 2, 12, Block::Gravel.default_state());
        column
    }
    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
        crate::chunk::air_state()
    }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_owned()
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
    fn packet_generation_stage(&self, _stage: ChunkGenerationStage) -> Option<ChunkGenerationStage> {
        Some(ChunkGenerationStage::Full)
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn owned_packet_admission_generates_the_neighbour_halo() {
    let source = Arc::new(RequestAdmissionSource::default());
    let mut column = ChunkColumn::new(0, 16);
    column.set_block_id(0, 2, 12, Block::Gravel.default_state());
    let owned = encode_column_owned(
        &RequestAdmissionProtocol,
        source.clone(),
        7,
        -3,
        None,
        crate::join_scheduler::ColumnPayload::Column(column.clone()),
    ).await.unwrap();
    let mut expected = column_admission_neighbours(7, -3, 1);
    expected.sort_unstable();
    let mut actual = source.requests.lock().unwrap().clone();
    actual.sort_unstable();
    assert_eq!(actual, expected);
    let inline_source = Arc::new(RequestAdmissionSource::default());
    let inline = encode_column(
        &RequestAdmissionProtocol,
        SourceRef::Shared(&inline_source),
        7,
        -3,
        None,
        crate::join_scheduler::ColumnPayload::Column(column),
    ).await.unwrap();
    assert_eq!(owned.directive, inline.directive);
    assert!(matches!(owned.directive, ServerDirective::Send { packet_id: 1, payload } if payload == [1]));
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test(flavor = "current_thread")]
async fn shaped_nether_packet_admission_includes_the_source_ore_spill() {
    let source = Arc::new(crate::chunk_store::ChunkStore::with_capacity(
        crate::worldgen_data::nether_chunk_source(42),
        32,
    ));
    let shaped = source.column_at(
        2,
        7,
        crate::chunk::ChunkGenerationStage::Shaped,
    );
    assert_eq!(
        shaped.generation_stage(),
        crate::chunk::ChunkGenerationStage::Shaped,
        "live view admission must retain the shaped stage until packet encoding"
    );
    assert_ne!(
        shaped.block_state_id(0, 2, 12),
        StateId::from_state_str("minecraft:gravel").expect("fixture gravel state"),
        "the shaped target starts without the source's border ore"
    );

    let owned_directive = encode_column_owned(
        &NetherPacketAdmissionProtocol,
        source.clone(),
        2,
        7,
        None,
        crate::join_scheduler::ColumnPayload::Column(shaped.clone()),
    )
    .await
    .expect("owned packet admission must include the source spill");
    let directive = encode_column(
        &NetherPacketAdmissionProtocol,
        SourceRef::Shared(&source),
        2,
        7,
        None,
        crate::join_scheduler::ColumnPayload::Column(shaped),
    )
    .await
    .expect("the shaped target should encode after full admission");
    assert_eq!(owned_directive.directive, directive.directive);
    let payload = match directive.directive {
        ServerDirective::Send { payload, .. } => payload,
        other => panic!("unexpected Nether packet directive: {other:?}"),
    };
    assert_eq!(
        payload,
        vec![1],
        "the packet must include source (1,7)'s gravel spill at target (2,7)"
    );

    let resident = source
        .resident_column(2, 7)
        .expect("packet admission must retain the upgraded target");
    assert_eq!(
        resident.generation_stage(),
        crate::chunk::ChunkGenerationStage::Full,
        "packet admission must replace the shaped cache entry with its full result"
    );
    assert_eq!(
        resident.block_state_id(0, 2, 12),
        StateId::from_state_str("minecraft:gravel").expect("fixture gravel state")
    );
}


struct ActionAdmissionProbe {
    column_threads: std::sync::Mutex<Vec<std::thread::ThreadId>>,
    admitted: std::sync::Mutex<HashSet<(i32, i32)>>,
    events: std::sync::Mutex<Vec<&'static str>>,
}

impl ActionAdmissionProbe {
    fn new(runtime_thread: std::thread::ThreadId) -> Self {
        let _ = runtime_thread;
        Self {
            column_threads: std::sync::Mutex::new(Vec::new()),
            admitted: std::sync::Mutex::new(HashSet::new()),
            events: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl ChunkSource for ActionAdmissionProbe {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.column_threads
            .lock()
            .expect("column probe lock")
            .push(std::thread::current().id());
        self.admitted
            .lock()
            .expect("admission probe lock")
            .insert((cx, cz));
        self.events.lock().expect("event probe lock").push("column");
        ChunkColumn::new(0, 256)
    }

    fn block_state_id(&self, x: i32, _y: i32, z: i32) -> StateId {
        let chunk = (x.div_euclid(16), z.div_euclid(16));
        assert!(
            self.admitted
                .lock()
                .expect("admission probe lock")
                .contains(&chunk),
            "an action read must run only after its target column is admitted"
        );
        self.events.lock().expect("event probe lock").push("action");
        crate::chunk::air_state()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        self.admitted
            .lock()
            .expect("admission probe lock")
            .contains(&(cx, cz))
            .then(|| ChunkColumn::new(0, 256))
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test(flavor = "current_thread")]
async fn action_admission_offloads_cold_columns_and_orders_the_read() {
    let runtime_thread = std::thread::current().id();
    let source = Arc::new(ActionAdmissionProbe::new(runtime_thread));
    let source_ref = SourceRef::Shared(&source);
    let packet = ServerBound::BlockAction {
        action: BlockActionKind::AbortDestroy,
        pos: BlockPos::new(17, 64, -1),
        face: BlockFace::North,
        sequence: 0,
    };

    admit_action_footprint(source_ref, &packet).await.unwrap();
    assert!(
        source_ref.get().resident_column(1, -1).is_some(),
        "the target must be resident before the synchronous action read"
    );
    source_ref.get().block_state_id(17, 64, -1);

    let threads = source
        .column_threads
        .lock()
        .expect("column probe lock")
        .clone();
    assert!(!threads.is_empty(), "the cold action must admit at least one column");
    assert!(
        threads.iter().all(|thread| *thread != runtime_thread),
        "cold action generation must not run on the connection runtime thread"
    );
    let events = source.events.lock().expect("event probe lock").clone();
    let action = events
        .iter()
        .position(|event| *event == "action")
        .expect("the action read must be observed");
    assert!(
        events[..action].iter().all(|event| *event == "column"),
        "the action read must follow every admission event"
    );
}

#[test]
fn resident_fall_probe_defers_a_cold_column_without_generation() {
    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: false,
        center_only: false,
    };

    assert!(
        resident_fall_sample(&source, 0.5, 64.0, 0.5, false).is_none(),
        "a cold movement probe must defer rather than synthesize terrain"
    );
    assert_eq!(
        source.column_reads.load(Ordering::Relaxed),
        0,
        "resident-only movement probes must never call a cold source's column"
    );
}

#[test]
fn resident_beacon_probes_defer_a_cold_column_without_generation() {
    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: false,
        center_only: false,
    };

    assert!(resident_beacon_levels(&source, 0, 64, 0).is_none());
    assert!(resident_beam_unobstructed(&source, 0, 64, 0, 384).is_none());
    assert_eq!(
        source.column_reads.load(Ordering::Relaxed),
        0,
        "resident-only beacon probes must never call a cold source's column"
    );
}

#[test]
fn client_tick_boundary_clears_stale_launch_momentum() {
    let launch = Vec3::new(4.0, 5.0, 6.0);
    let mut movement = ClientMovement::default();
    movement.observe(Vec3::new(1.0, 2.0, 3.0), false);

    assert_eq!(
        movement.add_to_launch(launch),
        Vec3::new(5.0, 7.0, 9.0),
        "an airborne launch inherits this tick's complete movement sample"
    );

    movement.finish_tick();
    assert_eq!(
        movement.add_to_launch(launch),
        Vec3::new(5.0, 7.0, 9.0),
        "the boundary retains movement reported during the tick it closes"
    );

    movement.finish_tick();
    assert_eq!(
        movement.add_to_launch(launch),
        launch,
        "a following tick with no movement must clear stale launch momentum"
    );

    movement.observe(Vec3::new(1.0, 2.0, 3.0), true);
    assert_eq!(
        movement.add_to_launch(launch),
        Vec3::new(5.0, 5.0, 9.0),
        "a grounded launch inherits horizontal movement but not vertical movement"
    );
}


pub(super) struct RefusingChunkProtocol;

impl ServerProtocol for RefusingChunkProtocol {
    fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
        unreachable!("these tests only write server directives")
    }

    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        unreachable!("these tests only write server directives")
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        unreachable!("these tests only write server directives")
    }

    fn begin_play(&self, _join: &crate::protocol::JoinGame) -> Vec<ServerDirective> {
        unreachable!("these tests only write server directives")
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::Send {
            packet_id: 40,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
        unreachable!("the checked encoder must be used")
    }

    fn try_encode_chunk(
        &self,
        _cx: i32,
        _cz: i32,
        _column: &ChunkColumn,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        Err(ChunkEncodeError::new("fixture rejected chunk"))
    }

    fn detached_source_encode(&self) -> Option<crate::protocol::DetachedSourceEncode> {
        #[cfg(not(target_arch = "wasm32"))]
        fn encode(
            _source: &dyn ChunkSource,
            _cx: i32,
            _cz: i32,
            _column: &ChunkColumn,
        ) -> Result<ServerDirective, ChunkEncodeError> {
            Ok(ServerDirective::Send {
                packet_id: 44,
                payload: format!("{:?}", std::thread::current().id()).into_bytes(),
            })
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Some(std::sync::Arc::new(encode))
        }
        #[cfg(target_arch = "wasm32")]
        {
            None
        }
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: 41,
            payload: vec![batch_size as u8],
        }
    }

    fn encode_disconnect(&self, _state: State, _reason: &Text) -> ServerDirective {
        ServerDirective::Send {
            packet_id: 42,
            payload: Vec::new(),
        }
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, _state: StateId) -> ServerDirective {
        ServerDirective::Send {
            packet_id: 43,
            payload: vec![x as u8, y as u8, z as u8],
        }
    }
}

struct InitialSeedFailureProtocol;

impl ServerProtocol for InitialSeedFailureProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        RefusingChunkProtocol.decode(state, packet_id, payload)
    }
    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        RefusingChunkProtocol.login_success(username, uuid)
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        RefusingChunkProtocol.begin_configuration()
    }
    fn begin_play(&self, join: &crate::protocol::JoinGame) -> Vec<ServerDirective> {
        RefusingChunkProtocol.begin_play(join)
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        RefusingChunkProtocol.begin_chunk_batch()
    }
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        RefusingChunkProtocol.encode_chunk(cx, cz, column)
    }
    fn end_chunk_batch(&self, size: i32) -> ServerDirective {
        RefusingChunkProtocol.end_chunk_batch(size)
    }
    fn encode_disconnect(&self, state: State, reason: &Text) -> ServerDirective {
        assert_eq!(state, State::Play);
        ServerDirective::Send {
            packet_id: 42,
            payload: reason.to_plain_string().into_bytes(),
        }
    }
}

#[tokio::test]
async fn initial_seed_failure_finishes_written_batch_and_preserves_the_cause() {
    for written in [None, Some(3)] {
        let (client_end, server_end) = lodestone_net::memory_pair();
        let mut conn = Connection::new(server_end);
        let mut state = State::Play;
        if written.is_some() {
            apply(&mut conn, &mut state, InitialSeedFailureProtocol.begin_chunk_batch())
                .await.unwrap();
        }
        let result: Result<(), ServerError> = return_initial_seed_error(
            &mut conn, &InitialSeedFailureProtocol, &mut state, written,
            ChunkEncodeError::new("cohort capacity 17"),
        ).await;
        assert!(matches!(result, Err(ServerError::InitialSeed(ref error))
            if error.message() == "cohort capacity 17"));
        drop(conn);
        let mut peer = Connection::new(client_end);
        if written.is_some() {
            assert_eq!(peer.read_packet().await.unwrap(), Some((40, Vec::new())));
            assert_eq!(peer.read_packet().await.unwrap(), Some((41, vec![3])));
        }
        assert_eq!(
            peer.read_packet().await.unwrap(),
            Some((42, b"Failed to prepare world population: cohort capacity 17".to_vec())),
        );
        assert_eq!(peer.read_packet().await.unwrap(), None);
    }
}

struct OneColumnSource;

impl ChunkSource for OneColumnSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 256)
    }

    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
        crate::chunk::air_state()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn existing_column_packet_encoding_uses_the_bounded_worker() {
    let source: Arc<dyn ChunkSource> = Arc::new(OneColumnSource);
    let directive = encode_column_owned(
        &RefusingChunkProtocol,
        source,
        2,
        -3,
        None,
        crate::join_scheduler::ColumnPayload::Column(ChunkColumn::new(0, 256)),
    )
    .await
    .expect("the detached encoder must handle an existing column");
    assert_eq!(directive.stage, None, "opaque legacy encoding cannot authenticate a stage");
    let ServerDirective::Send { packet_id, payload } = directive.directive else {
        panic!("the detached encoder must produce a packet");
    };
    assert_eq!(packet_id, 44);
    assert_ne!(
        payload,
        format!("{:?}", std::thread::current().id()).into_bytes(),
        "source-aware encoding must not run on the connection task"
    );
}

#[cfg(not(target_arch = "wasm32"))]
struct InitialWorkerProtocol;

#[cfg(not(target_arch = "wasm32"))]
impl ServerProtocol for InitialWorkerProtocol {
    fn decode(&self, state: State, id: i32, payload: &[u8]) -> ServerBound {
        RefusingChunkProtocol.decode(state, id, payload)
    }
    fn login_success(&self, name: &str, uuid: Uuid) -> Vec<ServerDirective> {
        RefusingChunkProtocol.login_success(name, uuid)
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> { Vec::new() }
    fn begin_play(&self, _join: &crate::protocol::JoinGame) -> Vec<ServerDirective> { Vec::new() }
    fn begin_chunk_batch(&self) -> ServerDirective { ServerDirective::None }
    fn encode_chunk(&self, _: i32, _: i32, _: &ChunkColumn) -> ServerDirective {
        panic!("initial packets must use owned preparation")
    }
    fn end_chunk_batch(&self, _: i32) -> ServerDirective { ServerDirective::None }
    fn retains_initial_column_light(&self) -> bool { true }
    fn uses_cross_column_light(&self) -> bool { true }
    fn detached_initial_packet_prepare(&self) -> Option<crate::protocol::DetachedInitialPacketPrepare> {
        Some(std::sync::Arc::new(|input: crate::initial_packet::InitialPacketInput| {
            assert_eq!(input.neighbours.len(), 8);
            std::thread::sleep(std::time::Duration::from_millis(40));
            Ok(crate::initial_packet::PreparedInitialPacket {
                directive: ServerDirective::Send {
                    packet_id: 44,
                    payload: format!("{:?}", std::thread::current().id()).into_bytes(),
                },
                stage: input.column.generation_stage(), settlement: None,
            })
        }))
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn initial_packet_worker_keeps_owner_timers_running() {
    let store = crate::chunk_store::ChunkStore::with_capacity(OneColumnSource, 64);
    let column = store.column(2, -3);
    let source: Arc<dyn ChunkSource> = Arc::new(store);
    let encode = encode_column_owned(
        &InitialWorkerProtocol, source, 2, -3, None,
        crate::join_scheduler::ColumnPayload::Column(column),
    );
    tokio::pin!(encode);
    tokio::select! {
        result = &mut encode => panic!("owner timer was starved: {:?}", result.err()),
        _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
    }
    let packet = encode.await.unwrap();
    assert_eq!(packet.stage, Some(ChunkGenerationStage::Full));
    let ServerDirective::Send { packet_id, payload } = packet.directive else {
        panic!("prepared packet must be returned after owner acceptance")
    };
    assert_eq!(packet_id, 44);
    assert_ne!(payload, format!("{:?}", std::thread::current().id()).into_bytes());
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn initial_packet_recaptures_a_missing_centre_through_admission() {
    for shaped in [false, true] {
        let store = crate::chunk_store::ChunkStore::with_capacity(OneColumnSource, 64);
        for (dx, dz) in light_neighbour_offsets(true) { store.column(2 + dx, -3 + dz); }
        if shaped {
            store.store_resident_column(2, -3, &ChunkColumn::new(0, 256)
                .test_with_generation_stage(ChunkGenerationStage::Shaped));
        }
        let admissions = AtomicUsize::new(0);
        let packet = try_encode_initial_column(&InitialWorkerProtocol, &store, 2, -3, |coordinates| {
            assert_eq!(coordinates, vec![
                (1, -4), (2, -4), (3, -4), (1, -3), (2, -3), (3, -3), (1, -2), (2, -2), (3, -2),
            ]);
            admissions.fetch_add(1, Ordering::Relaxed);
            store.store_resident_column(2, -3, &ChunkColumn::new(0, 256));
            std::future::ready(Ok(vec![store.column(2, -3)]))
        }).await.unwrap().unwrap();
        assert_eq!(admissions.load(Ordering::Relaxed), 1);
        assert_eq!(packet.stage, Some(ChunkGenerationStage::Full));
    }
}

pub(super) struct ColdColumnSource {
    pub(super) column_reads: AtomicUsize,
    pub(super) store_calls: AtomicUsize,
    pub(super) resident: bool,
    pub(super) center_only: bool,
}

impl ChunkSource for ColdColumnSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.column_reads.fetch_add(1, Ordering::Relaxed);
        ChunkColumn::new(0, 256)
    }

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        (self.resident && (!self.center_only || (cx, cz) == (0, 0)))
            .then(|| ChunkColumn::new(0, 256))
    }

    fn try_resident_block_state_id(
        &self,
        x: i32,
        _y: i32,
        z: i32,
    ) -> Option<crate::chunk_store::TryResident<lodestone_data::block_states::StateId>> {
        let resident = self.resident
            && (!self.center_only
                || (x.div_euclid(16), z.div_euclid(16)) == (0, 0));
        Some(if resident {
            crate::chunk_store::TryResident::Present(
                lodestone_data::block_states::StateId::new(
                    lodestone_data::block_states::state_id("minecraft:air")
                        .expect("fixture air state is registered"),
                )
                .expect("fixture air state id is valid"),
            )
        } else {
            crate::chunk_store::TryResident::Absent
        })
    }

    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
        crate::chunk::air_state()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}

    fn store_resident_column(
        &self,
        _cx: i32,
        _cz: i32,
        _column: &ChunkColumn,
    ) -> bool {
        self.store_calls.fetch_add(1, Ordering::Relaxed);
        true
    }
}

/// Exercises the actual initial-settlement consumer across the persistent
/// source and cache layers. The light is deliberately supplied by the
/// protocol's compute hook and then recovered only from the saved column;
/// the test never installs a snapshot directly on a fixture column.
#[derive(Debug)]
pub(super) struct RetainedLifecycleProtocol {
    pub(super) computes: Arc<AtomicUsize>,
    pub(super) retain_initial_light: bool,
    pub(super) fallback_encodes: Arc<AtomicUsize>,
    pub(super) dependency_light: Option<lodestone_world::ColumnLight>,
}

impl ServerProtocol for RetainedLifecycleProtocol {
    fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
        ServerBound::Ignored
    }

    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _join: &crate::protocol::JoinGame) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
        ServerDirective::None
    }

    fn try_encode_chunk_in_dimension(
        &self,
        _cx: i32,
        _cz: i32,
        _column: &ChunkColumn,
        _dimension: crate::dimension::Dimension,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        if self.retain_initial_light {
            self.fallback_encodes.fetch_add(1, Ordering::AcqRel);
            return Ok(ServerDirective::Send {
                packet_id: 2,
                payload: Vec::new(),
            });
        }
        Ok(ServerDirective::None)
    }

    fn try_encode_chunk_with_neighbours_in_dimension(
        &self,
        _cx: i32,
        _cz: i32,
        column: &ChunkColumn,
        _neighbours: &[(i32, i32, &ChunkColumn)],
        _dimension: crate::dimension::Dimension,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        let light = column
            .retained_light()
            .expect("the production initial consumer must attach retained light first");
        let mut writer = lodestone_core::Writer::default();
        light.encode(&mut writer);
        Ok(ServerDirective::Send {
            packet_id: 1,
            payload: writer.as_slice().to_vec(),
        })
    }

    fn compute_initial_column_light_with_neighbours_in_dimension(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, &ChunkColumn)],
        _dimension: crate::dimension::Dimension,
    ) -> Option<lodestone_world::ColumnLight> {
        assert_eq!(neighbours.len(), 8, "initial settlement must admit the full footprint");
        self.computes.fetch_add(1, Ordering::AcqRel);
        let mut light = lodestone_world::ColumnLight::new(column.section_count());
        *light.sky_mut(0) = lodestone_world::LightData::Uniform(4);
        *light.sky_mut(1) = lodestone_world::LightData::Uniform(12);
        // End persistence keeps explicit zero block storage beside a
        // retained sky layer; use that canonical representation in the
        // fixture so a save/reload round trip compares like with like.
        *light.block_mut(0) = lodestone_world::LightData::Uniform(0);
        *light.block_mut(1) = lodestone_world::LightData::Uniform(0);
        *light.block_mut(2) = lodestone_world::LightData::Uniform(6);
        Some(light)
    }

    fn compute_initial_column_lights_with_neighbours_in_dimension(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, &ChunkColumn)],
        dimension: crate::dimension::Dimension,
    ) -> Option<crate::chunk::ColumnLightSettlement> {
        let centre = self.compute_initial_column_light_with_neighbours_in_dimension(
            column,
            neighbours,
            dimension,
        )?;
        if let Some(dependency) = self.dependency_light.as_ref() {
            crate::chunk::ColumnLightSettlement::with_neighbours(
                centre,
                [(1, 0, dependency.clone())],
            )
        } else {
            Some(crate::chunk::ColumnLightSettlement::centre(centre))
        }
    }

    fn uses_cross_column_light(&self) -> bool {
        true
    }

    fn retains_initial_column_light(&self) -> bool {
        self.retain_initial_light
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
}

#[test]
fn detached_initial_packet_light_is_attached_to_the_packet_copy() {
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };
    let column = ChunkColumn::new(0, 256);
    let neighbours = (-1..=1)
        .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
        .filter(|&(dx, dz)| (dx, dz) != (0, 0))
        .map(|(dx, dz)| (dx, dz, ChunkColumn::new(0, 256)))
        .collect::<Vec<_>>();

    let neighbour_refs = borrowed_neighbours(&neighbours);
    let packet_column = detached_initial_packet_columns(
        &protocol,
        &column,
        &neighbour_refs,
        crate::dimension::Dimension::End,
    );

    assert_eq!(neighbour_refs.len(), neighbours.len());
    assert_eq!(
        packet_column.retained_light_status(),
        Some(crate::chunk::RetainedLightStatus::CentreSettled)
    );
    assert_eq!(
        packet_column
            .retained_light()
            .expect("packet copy has computed light")
            .sky(0),
        &lodestone_world::LightData::Uniform(4),
    );
    assert_eq!(protocol.computes.load(Ordering::Acquire), 1);
}

#[test]
fn initial_packet_preparation_promotes_dependencies_and_reuses_settled_light() {
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)), retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)), dependency_light: None,
    };
    let mut column = ChunkColumn::new(0, 256);
    column.set_retained_light_with_status(
        lodestone_world::ColumnLight::new(column.section_count()),
        crate::chunk::RetainedLightStatus::DependencyInitialized,
    );
    let neighbours = light_neighbour_offsets(true).into_iter()
        .map(|(dx, dz)| (dx, dz, ChunkColumn::new(0, 256))).collect::<Vec<_>>();
    let input = |column| crate::initial_packet::InitialPacketInput {
        coordinate: (3, -2), dimension: crate::dimension::Dimension::End,
        column, neighbours: neighbours.clone(),
    };
    let prepared = crate::initial_packet::prepare_initial_packet_with_protocol(
        &protocol, input(column.clone()),
    ).unwrap();
    let light = prepared.settlement.as_ref().unwrap().centre_light();
    assert_eq!(light.sky(0), &lodestone_world::LightData::Uniform(4));
    assert_eq!(light.sky(1), &lodestone_world::LightData::Uniform(12));
    assert_eq!(light.block(2), &lodestone_world::LightData::Uniform(6));
    assert_eq!(column.retained_light_status(), Some(crate::chunk::RetainedLightStatus::DependencyInitialized));
    column.set_retained_light(light.clone());
    let reused = crate::initial_packet::prepare_initial_packet_with_protocol(
        &protocol, input(column),
    ).unwrap();
    assert!(reused.settlement.is_none());
    assert_eq!(reused.directive, prepared.directive);
    assert_eq!(protocol.computes.load(Ordering::Acquire), 1);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn retained_light_survives_source_cache_save_reload_and_initial_encode() {
    let world_dir = tempfile::tempdir().expect("create retained-light lifecycle world");
    let region = crate::region_source::RegionChunkSource::new(
        OneColumnSource,
        world_dir.path(),
        crate::dimension::Dimension::End,
        0,
        256,
    )
    .expect("open retained-light lifecycle source");
    let save = region.save_handle();
    let store = crate::chunk_store::ChunkStore::with_capacity(region.clone(), 64);
    let source = crate::dimension::DimensionalSource::alone(
        store,
        crate::dimension::Dimension::End,
        crate::portal::PortalIndex::default(),
    );
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };

    // Light is derived state and is persisted only on a column that is
    // already saved for its blocks, so make this one an edit first.
    let _ = source.column(0, 0);
    source.set_block(1, 200, 1, Block::Stone.default_state());
    let first_column = source.column(0, 0);
    let first = encode_chunk_with_source(&protocol, &source, 0, 0, &first_column)
        .expect("settle and encode the first End column");
    let first_light = source
        .resident_column(0, 0)
        .expect("settled center remains resident")
        .retained_light()
        .cloned()
        .expect("settlement must retain its exact light");
    assert_eq!(protocol.computes.load(Ordering::Acquire), 1);
    let first_payload = match first {
        ServerDirective::Send { payload, .. } => payload,
        other => panic!("initial lifecycle encode emitted {other:?}"),
    };
    assert_eq!(first_light.sky(0), &lodestone_world::LightData::Uniform(4));
    assert_eq!(first_light.sky(1), &lodestone_world::LightData::Uniform(12));
    assert_eq!(first_light.block(2), &lodestone_world::LightData::Uniform(6));

    assert_eq!(save.save().expect("persist the settled light snapshot"), 1);
    drop(source);
    drop(region);
    drop(save);

    let reloaded_region = crate::region_source::RegionChunkSource::new(
        OneColumnSource,
        world_dir.path(),
        crate::dimension::Dimension::End,
        0,
        256,
    )
    .expect("reopen retained-light lifecycle source");
    let reloaded_store = crate::chunk_store::ChunkStore::with_capacity(
        reloaded_region,
        64,
    );
    let reloaded_source = crate::dimension::DimensionalSource::alone(
        reloaded_store,
        crate::dimension::Dimension::End,
        crate::portal::PortalIndex::default(),
    );
    let reloaded_column = reloaded_source.column(0, 0);
    assert_eq!(
        reloaded_column.retained_light(),
        Some(&first_light),
        "reload must restore the persisted snapshot on the serving column"
    );
    let replay = encode_chunk_with_source(
        &protocol,
        &reloaded_source,
        0,
        0,
        &reloaded_column,
    )
    .expect("encode the reloaded End column");
    let replay_payload = match replay {
        ServerDirective::Send { payload, .. } => payload,
        other => panic!("reloaded lifecycle encode emitted {other:?}"),
    };
    assert_eq!(
        replay_payload, first_payload,
        "initial encode must consume the persisted light verbatim"
    );
    assert_eq!(
        protocol.computes.load(Ordering::Acquire),
        1,
        "reload serving must not recompute a retained snapshot"
    );
}

/// Exercises the same production admission consumer with a protocol that
/// returns one dependency snapshot as well as the centre. The dependency is
/// recovered through the source's normal later column admission, proving
/// that the batch source path retained it rather than only updating the
/// centre cache entry.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn retained_dependency_requires_later_centre_admission() {
    let world_dir = tempfile::tempdir().expect("create dependency retained-light world");
    let region = crate::region_source::RegionChunkSource::new(
        OneColumnSource,
        world_dir.path(),
        crate::dimension::Dimension::End,
        0,
        256,
    )
    .expect("open dependency retained-light source");
    let store = crate::chunk_store::ChunkStore::with_capacity(region.clone(), 64);
    let source = crate::dimension::DimensionalSource::alone(
        store,
        crate::dimension::Dimension::End,
        crate::portal::PortalIndex::default(),
    );
    let mut dependency_light = lodestone_world::ColumnLight::new(
        ChunkColumn::new(0, 256).section_count(),
    );
    *dependency_light.sky_mut(0) = lodestone_world::LightData::Uniform(3);
    *dependency_light.block_mut(1) = lodestone_world::LightData::Uniform(11);
    let computes = Arc::new(AtomicUsize::new(0));
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::clone(&computes),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: Some(dependency_light.clone()),
    };

    let first_column = source.column(0, 0);
    encode_chunk_with_source(&protocol, &source, 0, 0, &first_column)
        .expect("admit and encode the End centre with its dependency");
    assert_eq!(computes.load(Ordering::Acquire), 1);

    let retained_dependency = source
        .column(1, 0)
        .retained_light()
        .cloned()
        .expect("the source batch must retain the dependency snapshot");
    assert_eq!(retained_dependency, dependency_light);
    assert_eq!(
        source
            .column(1, 0)
            .retained_light_status(),
        Some(crate::chunk::RetainedLightStatus::DependencyInitialized)
    );

    let later_column = source.column(1, 0);
    let later = encode_chunk_with_source(&protocol, &source, 1, 0, &later_column)
        .expect("admit the retained dependency as the later centre");
    let payload = match later {
        ServerDirective::Send { payload, .. } => payload,
        other => panic!("later centre encode emitted {other:?}"),
    };
    let settled = source
        .column(1, 0)
        .centre_settled_light()
        .cloned()
        .expect("later centre admission must promote the snapshot");
    let mut expected = lodestone_core::Writer::default();
    settled.encode(&mut expected);
    assert_eq!(payload, expected.as_slice());
    assert_eq!(
        computes.load(Ordering::Acquire),
        2,
        "later centre admission must not consume dependency storage as final"
    );
}

#[test]
fn unsettled_retained_light_is_removed_before_a_full_column_fallback() {
    let mut column = ChunkColumn::new(0, 16);
    column.set_retained_light_with_status(
        lodestone_world::ColumnLight::new(column.section_count()),
        crate::chunk::RetainedLightStatus::DependencyInitialized,
    );

    let packet_column = column_for_initial_encode(&column);

    assert!(
        packet_column.retained_light().is_none(),
        "a dependency snapshot must not reach a full-column encoder"
    );
    assert_eq!(packet_column.retained_light_status(), None);
}

/// A legacy family that does not consume retained snapshots must keep the
/// one-column path: no neighbour generation, light settlement, or source
/// persistence is admitted merely because it can compute cross-column
/// light in another context.
#[test]
fn legacy_initial_encode_does_not_admit_retained_light_settlement() {
    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: true,
        center_only: false,
    };
    let computes = Arc::new(AtomicUsize::new(0));
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::clone(&computes),
        retain_initial_light: false,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };
    let column = ChunkColumn::new(0, 256);
    assert!(matches!(
        encode_chunk_with_source(&protocol, &source, 0, 0, &column),
        Ok(ServerDirective::None)
    ));
    assert_eq!(source.column_reads.load(Ordering::Relaxed), 0);
    assert_eq!(source.store_calls.load(Ordering::Relaxed), 0);
    assert_eq!(computes.load(Ordering::Relaxed), 0);
}

/// Light is derived state: settling it on an unedited column must not
/// turn that column into a saved edit. Many light-only columns can be
/// served before an autosave, and none of them may reach the region file
/// or stay pinned in the persistence layer; the next session recomputes.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn light_only_settlement_is_never_persisted_and_recomputes_after_reload() {
    let world_dir = tempfile::tempdir().expect("create light-only world");
    let region = crate::region_source::RegionChunkSource::new(
        OneColumnSource,
        world_dir.path(),
        crate::dimension::Dimension::End,
        0,
        256,
    )
    .expect("open light-only source");
    let save = region.save_handle();
    let store = crate::chunk_store::ChunkStore::with_capacity(region.clone(), 2);
    let source = crate::dimension::DimensionalSource::alone(
        store,
        crate::dimension::Dimension::End,
        crate::portal::PortalIndex::default(),
    );
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };

    for cx in 0..6 {
        let column = source.column(cx, 0);
        encode_chunk_with_source(&protocol, &source, cx, 0, &column)
            .expect("settle and encode a production light snapshot");
    }
    // The control: every column did settle light, so its absence on disk
    // below is the persistence rule rather than a missing computation.
    assert_eq!(protocol.computes.load(Ordering::Acquire), 6);
    assert_eq!(
        region.retained_columns(),
        0,
        "light-only settlement must not pin columns in region persistence"
    );
    assert_eq!(save.save().expect("save after light-only settlement"), 0);

    drop(source);
    drop(region);
    drop(save);

    let reloaded_region = crate::region_source::RegionChunkSource::new(
        OneColumnSource,
        world_dir.path(),
        crate::dimension::Dimension::End,
        0,
        256,
    )
    .expect("reopen light-only source");
    let reloaded_store = crate::chunk_store::ChunkStore::with_capacity(
        reloaded_region,
        2,
    );
    let reloaded_source = crate::dimension::DimensionalSource::alone(
        reloaded_store,
        crate::dimension::Dimension::End,
        crate::portal::PortalIndex::default(),
    );
    let reloaded_column = reloaded_source.column(0, 0);
    assert_eq!(reloaded_column.retained_light(), None);
    encode_chunk_with_source(&protocol, &reloaded_source, 0, 0, &reloaded_column)
        .expect("encode the reloaded column");
    assert_eq!(
        protocol.computes.load(Ordering::Acquire),
        7,
        "an unsaved light snapshot is recomputed on the next session"
    );
}


pub(super) fn default_block_state(block: Block) -> StateId {
    block.default_state()
}

pub(super) fn fixture_state(state: &str) -> StateId {
    StateId::from_state_str(state).expect("fixture state")
}


#[tokio::test]
async fn a_local_failed_view_batch_writes_no_batch_markers() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    let source = OneColumnSource;
    let mut awaiting_ack = false;
    let mut pending = VecDeque::new();
    let mut view = ViewTracker::new((0, 0), 0, 0);

    let error = send_view_update(
        &mut conn,
        &RefusingChunkProtocol,
        SourceRef::Borrowed(&source),
        None,
        &mut state,
        &mut view,
        ViewUpdate {
            immediate: Vec::new(),
            forgotten: HashSet::new(),
            added: vec![(0, 0)],
            upgrades: Vec::new(),
        },
        &mut awaiting_ack,
        &mut pending,
    )
    .await
    .expect_err("a rejecting protocol must fail the view update");
    assert!(matches!(error, ServerError::ChunkEncode(_)));

    let mut peer = Connection::new(client_end);
    assert_eq!(
        peer.read_packet().await.expect("disconnect frame decodes"),
        Some((42, Vec::new())),
        "the locally accumulated batch must not write a start or end marker"
    );
}

#[tokio::test]
async fn a_written_chunk_batch_ends_before_its_encoding_disconnect() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    let source = OneColumnSource;

    let error = send_column_light(
        &mut conn,
        &RefusingChunkProtocol,
        &source,
        &mut state,
        0,
        0,
    )
    .await
    .expect_err("a rejecting protocol must fail the column resend");
    assert!(matches!(error, ServerError::ChunkEncode(_)));

    let mut peer = Connection::new(client_end);
    let frames = [
        peer.read_packet().await.expect("batch start decodes"),
        peer.read_packet().await.expect("batch end decodes"),
        peer.read_packet().await.expect("disconnect decodes"),
    ];
    assert_eq!(
        frames,
        [Some((40, Vec::new())), Some((41, vec![0])), Some((42, Vec::new()))],
        "an already-written batch must end before the encoding disconnect"
    );
}

/// An empty hand must resolve to the player's canonical attribute base,
/// while the equipment fold can move off that base.
///
/// The control is that the equipment fold *can* move off the base — a diamond
/// sword resolves to `7.0` — so the equality below is not comparing a value
/// against a constant that nothing else could change.
#[test]
fn bare_hand_damage_is_the_player_attribute_base() {
    let empty = PlayerInventory::new();
    let bare = empty.combat_stats().attack_damage;
    assert!(
        (f64::from(bare) - lodestone_entity::equipment::PLAYER_BASE_ATTACK_DAMAGE).abs()
            < 1e-9,
        "an empty hand must resolve to the player's attribute base, got {bare}"
    );

    let mut armed = PlayerInventory::new();
    armed.set_native(0, Some(ItemStack::new(item_key("diamond_sword"), 1)));
    let with_sword = armed.combat_stats().attack_damage;
    assert!(
        (with_sword - 7.0).abs() < 1e-6,
        "control: a real weapon must move the number off the base, got {with_sword}"
    );
}

/// A held sword only counts from the **selected** hotbar slot. The wrong
/// implementation — reading native slot `0` — passes the test above and fails
/// this one, which is the reason this is a second case rather than an extra
/// assertion.
#[test]
fn only_the_selected_hotbar_slot_arms_the_player() {
    let mut inv = PlayerInventory::new();
    inv.set_native(3, Some(ItemStack::new(item_key("diamond_sword"), 1)));
    assert!(
        (f64::from(inv.combat_stats().attack_damage)
            - lodestone_entity::equipment::PLAYER_BASE_ATTACK_DAMAGE)
            .abs()
            < 1e-9,
        "a sword in an unselected slot is not in the main hand"
    );
    assert!(inv.set_selected_hotbar_slot(3));
    assert!(
        (inv.combat_stats().attack_damage - 7.0).abs() < 1e-6,
        "selecting the slot holding it must arm the player"
    );
}

/// Worn armour reaches [`PlayerVitals::apply_damage`]'s reduction, and the
/// value is the one a real vanilla 26.2 server produced for the same set and
/// the same raw hit: `10.0` of `minecraft:mob_attack` against full diamond
/// measures **3.0**, where an unarmoured player takes the whole `10.0`.
#[test]
fn worn_armour_reduces_an_incoming_hit_to_the_live_verified_value() {
    let mut inv = PlayerInventory::new();
    for (native, item) in [
        (crate::inventory::HEAD_NATIVE, "diamond_helmet"),
        (crate::inventory::CHEST_NATIVE, "diamond_chestplate"),
        (crate::inventory::LEGS_NATIVE, "diamond_leggings"),
        (crate::inventory::FEET_NATIVE, "diamond_boots"),
    ] {
        inv.set_native(native, Some(ItemStack::new(item_key(item), 1)));
    }
    let flags = lodestone_entity::DamageFlags::for_damage_type_name("mob_attack")
        .expect("mob_attack is a real damage type");

    let mut armoured = PlayerVitals::default();
    let dealt = armoured
        .apply_damage(10.0, &inv.combat_stats().defenses, flags)
        .expect("the hit lands");
    assert!((dealt - 3.0).abs() < 1e-3, "armoured hit dealt {dealt}");

    let mut bare_player = PlayerVitals::default();
    let bare_dealt = bare_player
        .apply_damage(10.0, &PlayerInventory::new().combat_stats().defenses, flags)
        .expect("the hit lands");
    assert!(
        (bare_dealt - 10.0).abs() < 1e-3,
        "control: an unarmoured player takes the full hit, got {bare_dealt}"
    );
}

/// A parsed `minecraft:` item key for the tests above.
pub(super) fn item_key(path: &str) -> lodestone_model::ResourceKey {
    lodestone_model::ResourceKey::new("minecraft", path).expect("a static item key parses")
}


/// The packet-shaping function itself, not the equipment maths
/// [`worn_armour_reduces_an_incoming_hit_to_the_live_verified_value`]
/// already covers: a full diamond set's folded `minecraft:armor` and
/// `minecraft:armor_toughness` must reach [`player_attribute_snapshots`]'s
/// output as a bare `base` with **no** modifiers (see that function's own
/// doc for why empty modifiers are correct, not merely simpler — the
/// client's fold is a no-op over a bare base). This is a magnitude check
/// against the same 20.0/8.0 pair the live server produced for the
/// identical set, not a "some armour value exists" check.
#[test]
fn player_attribute_snapshots_carries_the_folded_armor_with_no_modifiers() {
    let mut inv = PlayerInventory::new();
    for (native, item) in [
        (crate::inventory::HEAD_NATIVE, "diamond_helmet"),
        (crate::inventory::CHEST_NATIVE, "diamond_chestplate"),
        (crate::inventory::LEGS_NATIVE, "diamond_leggings"),
        (crate::inventory::FEET_NATIVE, "diamond_boots"),
    ] {
        inv.set_native(native, Some(ItemStack::new(item_key(item), 1)));
    }
    let snapshots = player_attribute_snapshots(&inv);
    let armor = snapshots
        .iter()
        .find(|s| s.attribute.to_string() == "minecraft:armor")
        .expect("a fully-armoured player must publish minecraft:armor");
    assert!((armor.base - 20.0).abs() < 1e-6, "armor {}", armor.base);
    assert!(
        armor.modifiers.is_empty(),
        "the wire snapshot must fold equipment into base, not re-publish per-item modifiers"
    );
    let toughness = snapshots
        .iter()
        .find(|s| s.attribute.to_string() == "minecraft:armor_toughness")
        .expect("a full diamond set must publish armor_toughness");
    assert!((toughness.base - 8.0).abs() < 1e-6, "toughness {}", toughness.base);

    // Control: an unarmoured player still publishes `minecraft:armor`
    // explicitly, at `0.0` — **not** an absent entry. This verifies that
    // removing the last piece resets the HUD value rather than leaving
    // its last non-zero reading, and the reason
    // `player_attribute_snapshots` reads named attributes through
    // `AttributeMap::value` rather than iterating the sparse map: an
    // omitted attribute is "unchanged" to the client's merge
    // (`lodestone_ecs::ingest::apply_entity_attributes`), not "reset".
    let bare = player_attribute_snapshots(&PlayerInventory::new());
    let bare_armor = bare
        .iter()
        .find(|s| s.attribute.to_string() == "minecraft:armor")
        .expect("an unarmoured player must still publish minecraft:armor, explicitly");
    assert!(
        bare_armor.base.abs() < 1e-6,
        "an unarmoured player's armor must be exactly 0.0, got {}",
        bare_armor.base
    );
}

/// **The removal sequence, reproduced directly.** Equip a helmet and a
/// chestplate (distinct per-piece values — `3.0` and `8.0` — so a
/// transposition or an off-by-one cannot hide), remove them one at a
/// time, and assert the *sequence* of published armour values: `11.0`
/// (both), `8.0` (helmet off), then `0.0` (chestplate off too). Collected
/// into one list and asserted together, so a failure identifies the
/// published value at the affected removal point.
#[test]
fn removing_the_last_piece_of_armor_publishes_an_explicit_zero() {
    let mut inv = PlayerInventory::new();
    inv.set_native(
        crate::inventory::HEAD_NATIVE,
        Some(ItemStack::new(item_key("diamond_helmet"), 1)),
    );
    inv.set_native(
        crate::inventory::CHEST_NATIVE,
        Some(ItemStack::new(item_key("diamond_chestplate"), 1)),
    );

    fn armor_of(inv: &PlayerInventory) -> Option<f64> {
        player_attribute_snapshots(inv)
            .into_iter()
            .find(|s| s.attribute.to_string() == "minecraft:armor")
            .map(|s| s.base)
    }
    fn check(mismatches: &mut Vec<String>, inv: &PlayerInventory, label: &str, expected: f64) {
        let Some(got) = armor_of(inv) else {
            mismatches.push(format!("{label}: minecraft:armor was not published at all"));
            return;
        };
        if (got - expected).abs() > 1e-6 {
            mismatches.push(format!("{label}: expected {expected}, got {got}"));
        }
    }

    let mut mismatches = Vec::new();
    check(&mut mismatches, &inv, "both pieces worn", 11.0);
    inv.set_native(crate::inventory::HEAD_NATIVE, None);
    check(&mut mismatches, &inv, "helmet removed, chestplate still worn", 8.0);
    inv.set_native(crate::inventory::CHEST_NATIVE, None);
    check(&mut mismatches, &inv, "last piece removed", 0.0);

    assert!(
        mismatches.is_empty(),
        "armour publication went stale: {mismatches:?}"
    );
}

/// **Control for the explicit zero.** Iterates only attributes present in
/// the sparse map and verifies that the final removal omits
/// `minecraft:armor` instead of publishing `0.0`. The assertion requires
/// the explicit armor entry, so omitting the final publication fails.
#[test]
fn the_sparse_iteration_bug_is_caught_by_the_removal_sequence_above() {
    fn buggy_snapshots(inventory: &PlayerInventory) -> Vec<EntityAttributeSnapshot> {
        inventory
            .combat_stats()
            .attributes
            .iter()
            .map(|(id, instance)| EntityAttributeSnapshot {
                attribute: id.clone(),
                base: instance.value(),
                modifiers: Vec::new(),
            })
            .collect()
    }

    let mut inv = PlayerInventory::new();
    inv.set_native(
        crate::inventory::HEAD_NATIVE,
        Some(ItemStack::new(item_key("diamond_helmet"), 1)),
    );
    inv.set_native(
        crate::inventory::CHEST_NATIVE,
        Some(ItemStack::new(item_key("diamond_chestplate"), 1)),
    );
    inv.set_native(crate::inventory::HEAD_NATIVE, None);
    inv.set_native(crate::inventory::CHEST_NATIVE, None);

    let armor = buggy_snapshots(&inv)
        .into_iter()
        .find(|s| s.attribute.to_string() == "minecraft:armor");
    assert!(
        armor.is_none(),
        "control did not reproduce the bug: the sparse-iteration version was expected to \
         omit minecraft:armor entirely once the last piece came off, but it published {armor:?}"
    );
}


// -- container screens (Job 1: OPEN_SCREEN/CONTAINER_SET_CONTENT/SLOT/DATA) --

pub(super) fn stack(item: &str, count: u32) -> ItemStack {
    ItemStack::new(item.parse().expect("valid resource key"), count)
}


// -----------------------------------------------------------------
// `apply_client_command`'s `dimension_reset` out-parameter — the reset for
// "die in the Nether, respawn to nothing": `encode_respawn` always tells
// the client `minecraft:overworld` (this crate's one respawn dimension),
// but nothing reset the *server's* own dimension tracking, so the client
// was correctly labelled and never sent any terrain for where it landed.
// -----------------------------------------------------------------

/// A `ChunkSource` fixture that reports whichever [`crate::dimension::Dimension`]
/// it was built with — the only thing `apply_client_command` reads off
/// `source` here (`respawn` is `None`, so the bed branch never runs).
pub(super) struct DimensionOnly(pub(super) crate::dimension::Dimension);

impl ChunkSource for DimensionOnly {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 256)
    }
    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
        crate::chunk::air_state()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_string()
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
    fn dimension(&self) -> Option<crate::dimension::Dimension> {
        Some(self.0)
    }
}

/// A [`ChunkSource`] double for [`dimension_scoped_handles`]: answers
/// `world_registries`/`block_tick_feed` with whatever the test hands it
/// and nothing else, so the fixture can stand in for a real
/// `DimensionalSource` sibling without pulling in `crate::integrated`.
struct HandleStubSource {
    block_entities: Option<BlockEntityHandle>,
    scheduled: crate::scheduled_tick::ScheduledTickHandle,
    block_tick_feed: Option<BlockTickFeed>,
}

impl ChunkSource for HandleStubSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 256)
    }
    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
        crate::chunk::air_state()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_string()
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
    fn world_registries(&self) -> Option<crate::chunk::WorldRegistries> {
        self.block_entities.clone().map(|block_entities| crate::chunk::WorldRegistries {
            block_entities,
            scheduled: self.scheduled.clone(),
            #[cfg(not(target_arch = "wasm32"))]
            player_data: None,
            #[cfg(not(target_arch = "wasm32"))]
            native_storage: None,
        })
    }
    fn block_tick_feed(&self) -> Option<BlockTickFeed> {
        self.block_tick_feed.clone()
    }
}

/// No trip at all: both handles fall back to "use the pair you joined
/// with" — the precondition every arm below builds on.
#[test]
fn dimension_scoped_handles_is_none_before_any_trip() {
    let handles = dimension_scoped_handles(None);
    assert!(handles.block_entities.is_none());
    assert!(handles.block_ticks.is_none());
}

/// **The discriminating gate for the validated break path.** A sibling with its own
/// registry and feed must hand back *that exact instance*, not merely
/// `Some(_)` — proven by writing a marker through the handle this
/// function returns and reading it back through the sibling source's own
/// accessor (a second path over the same data, not a restatement).
#[test]
fn dimension_scoped_handles_reaches_the_sibling_own_registry_and_feed() {
    let block_entities = BlockEntityHandle::default();
    let scheduled = crate::scheduled_tick::ScheduledTickHandle::default();
    let block_tick_feed = BlockTickFeed::default();
    let sibling: Arc<dyn ChunkSource> = Arc::new(HandleStubSource {
        block_entities: Some(block_entities.clone()),
        scheduled,
        block_tick_feed: Some(block_tick_feed.clone()),
    });

    let handles = dimension_scoped_handles(Some(&sibling));
    let routed_entities = handles
        .block_entities
        .expect("a sibling with its own registry must answer Some");
    let routed_ticks = handles
        .block_ticks
        .expect("a sibling with its own feed must answer Some");

    // Written through the *returned* handle, read back through the
    // sibling's own — if `dimension_scoped_handles` had handed back a
    // fresh default instead of the sibling's real one, this would find
    // nothing.
    let pos = BlockPos::new(11, 60, -4);
    routed_entities.with(|registry| {
        registry.insert(
            pos,
            BlockEntity::Container {
                id: "minecraft:chest".to_string().into(),
                slots: Vec::new(),
            },
        );
    });
    assert!(
        block_entities.with(|registry| registry.get(pos).is_some()),
        "a marker inserted through the routed handle must be visible through the \
         sibling's own — they must be the same instance, not a copy"
    );

    // `ScheduledTick` carries a private `sub_tick_order`, so it is built
    // through a real queue rather than a struct literal — same trick
    // `tick.rs`'s own `one_pending` test helper uses.
    let mut queue: crate::scheduled_tick::ScheduledTickQueue<ScheduledTickKind> =
        crate::scheduled_tick::ScheduledTickQueue::new();
    queue.schedule(
        (11, 60, -4),
        ScheduledTickKind::Extension("minecraft:redstone_wire".to_string()),
        2,
        crate::scheduled_tick::TickPriority::Normal,
    );
    routed_ticks.request_scheduled_ticks(queue.drain_due(u64::MAX, usize::MAX));
    assert_eq!(
        block_tick_feed.drain_scheduled_ticks().len(),
        1,
        "a tick requested through the routed feed must be drained through the \
         sibling's own — same-instance requirement as the registry above"
    );
}

/// The negative control proving the positive result above is not
/// vacuous: a source with **neither** registry nor feed of its own (an
/// in-memory sibling with no tick loop wired, or the degenerate case) must
/// fall back to `None` on both — never invent a private default that
/// silently discards a placement.
#[test]
fn dimension_scoped_handles_falls_back_when_the_sibling_has_neither() {
    let sibling: Arc<dyn ChunkSource> = Arc::new(HandleStubSource {
        block_entities: None,
        scheduled: crate::scheduled_tick::ScheduledTickHandle::default(),
        block_tick_feed: None,
    });
    let handles = dimension_scoped_handles(Some(&sibling));
    assert!(handles.block_entities.is_none());
    assert!(handles.block_ticks.is_none());
}


mod respawn_tests;


