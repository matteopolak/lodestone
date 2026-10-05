//! Unit tests for the server driver: the play loop's gating, the per-packet
//! handlers, and the helpers that live in the sibling modules.

use super::*;
use crate::chunk::ChunkColumn;
use crate::mob_effects::ActiveEffects;
use crate::protocol::MetadataField;
use lodestone_model::{Rotation, Vec3};
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

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
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
    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
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
    scalar_calls: AtomicUsize,
}

impl ChunkSource for RequestAdmissionSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.scalar_calls.fetch_add(1, Ordering::Relaxed);
        ChunkColumn::new(0, 16)
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
    fn request_generation(
        &self,
        request: crate::worldgen_session::GenerationRequest,
        _session: Option<&mut crate::worldgen_session::GenerationSession>,
    ) -> Result<Option<crate::worldgen_session::GenerationRequestResult>, crate::worldgen_session::GenerationRequestError> {
        self.requests.lock().unwrap().push(request.target());
        let mut column = ChunkColumn::new(0, 16);
        column.set_block_id(0, 2, 12, Block::Gravel.default_state());
        Ok(Some(crate::worldgen_session::GenerationRequestResult::Existing(column)))
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn owned_packet_admission_uses_the_request_boundary() {
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
    assert_eq!(source.scalar_calls.load(Ordering::Relaxed), 0);
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

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
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
    fn begin_play(&self, radius: i32) -> Vec<ServerDirective> {
        RefusingChunkProtocol.begin_play(radius)
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
    fn begin_play(&self, _: i32) -> Vec<ServerDirective> { Vec::new() }
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

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
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
fn detached_snapshot_reuses_initial_light_across_encodes() {
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };
    let relative_neighbours = (-1..=1)
        .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
        .filter(|&(dx, dz)| (dx, dz) != (0, 0))
        .map(|(dx, dz)| (dx, dz, ChunkColumn::new(0, 256)))
        .collect::<Vec<_>>();
    let snapshot = crate::worldgen_session::PacketSnapshot::for_test_with_neighbours(
        ChunkColumn::new(0, 256),
        relative_neighbours
            .into_iter()
            .map(|(dx, dz, column)| ((dx, dz), column))
            .collect(),
    );

    let first = encode_packet_snapshot_with_protocol(
        &protocol,
        0,
        0,
        &snapshot,
        crate::dimension::Dimension::End,
    )
    .expect("prepared packet snapshot encodes");
    let second = encode_packet_snapshot_with_protocol(
        &protocol,
        0,
        0,
        &snapshot,
        crate::dimension::Dimension::End,
    )
    .expect("settled packet snapshot encodes identically");

    assert_eq!(first, second);
    assert!(snapshot.is_light_settled());
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

/// A protocol double whose entity encoders tag each directive with a
/// distinct packet id and the entity id(s) involved, so a test can read the
/// streamer's diff *decisions* straight off the returned directives. It does
/// not implement the chunk/login half — the streamer never calls those.
struct TagProto;

#[test]
fn equal_dimension_revisions_need_a_full_stream_reset() {
    let world = crate::world_state::WorldStateHandle::new();
    let overworld = world.ensure_dimension_runtime(crate::dimension::Dimension::Overworld);
    let nether = world.ensure_dimension_runtime(crate::dimension::Dimension::Nether);
    for (runtime, species) in [(&overworld, "minecraft:cow"), (&nether, "minecraft:zombified_piglin")] {
        assert_eq!(runtime.mobs().with(|sim| sim.spawn_species(species.parse().unwrap(), Vec3::new(1.0, 61.0, 2.0)).id()), 1000);
        runtime.publish_entities();
    }
    let home = ActiveEntities::new(&world, &NoEntities, crate::dimension::Dimension::Overworld);
    let destination = ActiveEntities::new(&world, &NoEntities, crate::dimension::Dimension::Nether);
    let mut streamer = EntityStreamer::default();
    let mut roster = PlayerListStreamer::default();
    assert!(stream_pass(&TagProto, &home, &mut streamer, &mut roster, None).iter()
        .any(|directive| matches!(directive, ServerDirective::Send { packet_id: ADD, .. })));
    let control = stream_pass(&TagProto, &destination, &mut streamer, &mut roster, None);
    assert!(!control.iter().any(|directive| matches!(directive, ServerDirective::Send { packet_id: ADD, .. })),
        "the unchanged revision control actually suppresses the destination addition");
    assert_eq!(streamer.last_sent[&1000].entity_type.to_string(), "minecraft:cow");
    let _ = streamer.reset_dimension(&TagProto);
    let corrected = stream_pass(&TagProto, &destination, &mut streamer, &mut roster, None);
    assert!(corrected.iter().any(|directive| matches!(directive, ServerDirective::Send { packet_id: ADD, .. })));
    assert_eq!(streamer.last_sent[&1000].entity_type.to_string(), "minecraft:zombified_piglin");
}

const ADD: i32 = 1;
const UPDATE: i32 = 2;
const REMOVE: i32 = 3;
const METADATA: i32 = 4;
const LINK: i32 = 5;
const ATTRIBUTES: i32 = 6;
const HEALTH: i32 = 7;
const PARTICLES: i32 = 8;
const ROSTER_ADD: i32 = 9;
const ROSTER_REMOVE: i32 = 10;
const BOSS_ADD: i32 = 11;
const BOSS_PROGRESS: i32 = 12;
const BOSS_REMOVE: i32 = 13;

impl ServerProtocol for TagProto {
    fn decode(&self, _s: State, _id: i32, _p: &[u8]) -> ServerBound {
        unimplemented!("streamer never decodes")
    }
    fn login_success(&self, _u: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_play(&self, _r: i32) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        unimplemented!()
    }
    fn encode_chunk(&self, _cx: i32, _cz: i32, _c: &ChunkColumn) -> ServerDirective {
        unimplemented!()
    }
    fn end_chunk_batch(&self, _n: i32) -> ServerDirective {
        unimplemented!()
    }

    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        ServerDirective::Send {
            packet_id: ADD,
            payload: vec![entity.id as u8],
        }
    }
    fn encode_entity_update(
        &self,
        _prev: Option<&EntitySnapshot>,
        current: &EntitySnapshot,
    ) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: UPDATE,
            payload: vec![current.id as u8],
        }]
    }
    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        ServerDirective::Send {
            packet_id: REMOVE,
            payload: ids.iter().map(|id| *id as u8).collect(),
        }
    }

    fn encode_player_info_add(
        &self,
        players: &[crate::protocol::PlayerListing],
    ) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: ROSTER_ADD,
            payload: vec![players.len() as u8],
        }]
    }
    fn encode_player_info_remove(&self, uuids: &[Uuid]) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: ROSTER_REMOVE,
            payload: vec![uuids.len() as u8],
        }]
    }
    fn encode_boss_event_add(&self, _id: Uuid, _name: &Text, progress: f32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: BOSS_ADD,
            payload: progress.to_be_bytes().to_vec(),
        }
    }
    fn encode_boss_event_update_progress(&self, _id: Uuid, progress: f32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: BOSS_PROGRESS,
            payload: progress.to_be_bytes().to_vec(),
        }
    }
    fn encode_boss_event_remove(&self, _id: Uuid) -> ServerDirective {
        ServerDirective::Send {
            packet_id: BOSS_REMOVE,
            payload: Vec::new(),
        }
    }

    fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
        ServerDirective::Send {
            packet_id: METADATA,
            payload: std::iter::once(entity_id as u8)
                .chain(std::iter::once(fields.len() as u8))
                .collect(),
        }
    }
    fn encode_set_entity_link(&self, source_id: i32, target_id: Option<i32>) -> ServerDirective {
        ServerDirective::Send {
            packet_id: LINK,
            // `255` as the "no target" byte: every id this test file uses is
            // small and positive, so it cannot collide with a real target and
            // stays visually distinct from `0`, which is also a plausible id.
            payload: vec![source_id as u8, target_id.map_or(255, |id| id as u8)],
        }
    }
    fn encode_update_attributes(&self, attributes: &[EntityAttributeSnapshot]) -> ServerDirective {
        let max_health = attributes
            .iter()
            .find(|snapshot| snapshot.attribute.to_string() == "minecraft:max_health")
            .map(|snapshot| snapshot.base as u8)
            .unwrap_or_default();
        ServerDirective::Send {
            packet_id: ATTRIBUTES,
            payload: vec![max_health],
        }
    }
    fn encode_set_health(&self, health: f32, _food: i32, _saturation: f32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: HEALTH,
            payload: vec![health as u8],
        }
    }
    fn encode_level_particles(
        &self,
        particle: &str,
        pos: Vec3,
        _offset: lodestone_model::Vec3f,
        _max_speed: f32,
        _count: i32,
        _long_distance: bool,
    ) -> ServerDirective {
        (particle == "minecraft:gust_emitter_small")
            .then(|| ServerDirective::Send {
                packet_id: PARTICLES,
                payload: [pos.x.to_be_bytes(), pos.y.to_be_bytes(), pos.z.to_be_bytes()].concat(),
            })
            .unwrap_or(ServerDirective::None)
    }
}

fn snap(id: i32, x: f64) -> EntitySnapshot {
    EntitySnapshot {
        id,
        uuid: Uuid::nil(),
        entity_type: "minecraft:zombie".parse().unwrap(),
        position: Vec3::new(x, 0.0, 0.0),
        rotation: Rotation::new(0.0, 0.0),
        head_yaw: 0.0,
        velocity: Vec3::new(0.0, 0.0, 0.0),
        on_ground: false,
        metadata: Vec::new(),
        object_data: 0,
        leash_link: None,
    }
}

/// Extracts `(packet_id, payload)` from a `Send` directive for assertions.
fn sent(d: &ServerDirective) -> (i32, &[u8]) {
    match d {
        ServerDirective::Send { packet_id, payload } => (*packet_id, payload.as_slice()),
        other => panic!("expected Send, got {other:?}"),
    }
}

fn assert_sent(out: &[ServerDirective], expected: &[(i32, &[u8])]) {
    assert_eq!(out.iter().map(sent).collect::<Vec<_>>().as_slice(), expected);
}

#[derive(Default)]
struct CountedPublication {
    source: crate::LiveMobSource,
    snapshot_reads: std::sync::Arc<AtomicUsize>,
}

impl EntitySource for CountedPublication {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.snapshot_reads.fetch_add(1, Ordering::Relaxed);
        self.source.snapshots()
    }

    fn snapshots_if_changed(
        &self,
        previous_revision: Option<u64>,
    ) -> Option<(u64, Vec<EntitySnapshot>)> {
        let publication = self.source.snapshots_if_changed(previous_revision)?;
        self.snapshot_reads.fetch_add(1, Ordering::Relaxed);
        Some(publication)
    }

    fn boss_bars(&self) -> Vec<BossBarSnapshot> {
        self.source.boss_bars()
    }
}

#[test]
fn publication_stream_skips_reads_and_keeps_connection_lifecycles() {
    let source = CountedPublication::default();
    assert_eq!(
        source.source.snapshots_if_changed(None),
        Some((0, Vec::new())),
    );
    assert_eq!(source.source.snapshots_if_changed(Some(0)), None);
    source.source.publish(vec![snap(10, 1.25)]);
    let mut first = EntityStreamer::default();
    let mut first_list = PlayerListStreamer::default();
    let out = stream_pass(&TagProto, &source, &mut first, &mut first_list, None);
    assert_sent(&out, &[(ADD, &[10])]);
    for _ in 0..100 {
        assert!(stream_pass(&TagProto, &source, &mut first, &mut first_list, None).is_empty());
    }
    assert_eq!(source.snapshot_reads.load(Ordering::Relaxed), 1);

    source.source.publish(vec![snap(10, 3.75)]);
    let out = stream_pass(&TagProto, &source, &mut first, &mut first_list, None);
    assert_sent(&out, &[(UPDATE, &[10])]);
    assert_eq!(first.last_sent[&10].position.x, 3.75);

    let mut second = EntityStreamer::default();
    let mut second_list = PlayerListStreamer::default();
    let out = stream_pass(&TagProto, &source, &mut second, &mut second_list, None);
    assert_sent(&out, &[(ADD, &[10])]);
    source.source.publish(vec![snap(10, 3.75)]);
    assert!(stream_pass(&TagProto, &source, &mut first, &mut first_list, None).is_empty());
    assert_eq!(first.last_publication, Some(3));
    assert_eq!(source.snapshot_reads.load(Ordering::Relaxed), 4);

    source.source.publish(Vec::new());
    for (streamer, list) in [(&mut first, &mut first_list), (&mut second, &mut second_list)] {
        let out = stream_pass(&TagProto, &source, streamer, list, None);
        assert_sent(&out, &[(REMOVE, &[10])]);
    }
    let mut reconnected = EntityStreamer::default();
    let mut reconnected_list = PlayerListStreamer::default();
    let out = stream_pass(&TagProto, &source, &mut reconnected, &mut reconnected_list, None);
    assert!(out.is_empty());
    assert_eq!(reconnected.last_publication, Some(4));
    assert_eq!(source.snapshot_reads.load(Ordering::Relaxed), 7);
}

#[test]
fn unversioned_source_still_streams_direct_mutations() {
    struct DirectSource(std::sync::Mutex<Vec<EntitySnapshot>>);
    impl EntitySource for DirectSource {
        fn snapshots(&self) -> Vec<EntitySnapshot> {
            self.0.lock().unwrap().clone()
        }
    }
    let source = DirectSource(std::sync::Mutex::new(vec![snap(20, 1.25)]));
    let mut streamer = EntityStreamer::default();
    let mut list = PlayerListStreamer::default();
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(ADD, &[20])]);
    *source.0.lock().unwrap() = vec![snap(20, 3.75)];
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(UPDATE, &[20])]);
    source.0.lock().unwrap().clear();
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(REMOVE, &[20])]);
}

#[test]
fn player_view_changes_stream_without_a_mob_publication() {
    let players = PlayerRegistry::new();
    let viewer = players.join("Viewer", Uuid::from_u128(1), Vec3::new(1.0, 70.0, 2.0));
    let counted = CountedPublication::default();
    let publication = counted.source.clone();
    let snapshot_reads = std::sync::Arc::clone(&counted.snapshot_reads);
    let source = crate::players::PlayerAwareSource::new(counted, players.clone());
    let mut item = snap(20, 2.25);
    item.entity_type = "minecraft:item".parse().unwrap();
    item.metadata = vec![MetadataField::Item {
        item: "minecraft:stone".parse().unwrap(),
        count: 3,
    }];
    publication.publish(vec![snap(10, 1.25), item.clone()]);
    let mut streamer = EntityStreamer::default();
    let mut list = PlayerListStreamer::default();
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[
        (ROSTER_ADD, &[1]),
        (ADD, &[10]),
        (ADD, &[20]),
        (METADATA, &[20, 1]),
    ]);
    for _ in 0..100 {
        let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
        assert!(out.is_empty());
    }
    assert_eq!(snapshot_reads.load(Ordering::Relaxed), 1);
    assert_eq!(streamer.last_sent.len(), 2);
    assert!(streamer.players_last_sent.is_empty());
    let peer = players.join("Peer", Uuid::from_u128(2), Vec3::new(3.0, 70.0, 4.0));
    let peer_id = peer.entity_id();
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[
        (ROSTER_ADD, &[1]),
        (ADD, &[peer_id as u8]),
        (METADATA, &[peer_id as u8, 1]),
    ]);
    assert!(!streamer.players_last_sent.contains_key(&viewer.entity_id()));
    players.set_position(peer_id, Vec3::new(6.25, 70.0, 4.0));
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[(UPDATE, &[peer_id as u8])]);
    assert_eq!(streamer.players_last_sent[&peer_id].position.x, 6.25);
    players.set_shared_flags(peer_id, 0x20);
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[
        (UPDATE, &[peer_id as u8]),
        (METADATA, &[peer_id as u8, 1]),
    ]);
    assert_eq!(
        streamer.players_last_sent[&peer_id].metadata,
        vec![MetadataField::SharedFlags(0x20)],
    );
    drop(peer);
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[(ROSTER_REMOVE, &[1]), (REMOVE, &[peer_id as u8])]);
    assert_eq!(snapshot_reads.load(Ordering::Relaxed), 1);
    assert_eq!(streamer.last_sent.len(), 2);
    assert!(streamer.players_last_sent.is_empty());

    item.metadata = vec![MetadataField::Item {
        item: "minecraft:stone".parse().unwrap(),
        count: 1,
    }];
    publication.publish(vec![snap(10, 3.75), item]);
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[
        (UPDATE, &[10]),
        (UPDATE, &[20]),
        (METADATA, &[20, 1]),
    ]);
    assert_eq!(streamer.last_sent[&10].position.x, 3.75);
    assert_eq!(snapshot_reads.load(Ordering::Relaxed), 2);

    publication.publish(Vec::new());
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_eq!(out.len(), 1);
    let (packet_id, payload) = sent(&out[0]);
    assert_eq!(packet_id, REMOVE);
    let mut removed = payload.to_vec();
    removed.sort_unstable();
    assert_eq!(removed, vec![10, 20]);
    assert_eq!(snapshot_reads.load(Ordering::Relaxed), 3);
    assert!(streamer.last_sent.is_empty());
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert!(out.is_empty());
    assert_eq!(snapshot_reads.load(Ordering::Relaxed), 3);

    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(value) = std::env::var("LODESTONE_ENTITY_PUBLICATION_PERF_ITERATIONS") {
        let iterations = value.parse::<usize>()
            .expect("LODESTONE_ENTITY_PUBLICATION_PERF_ITERATIONS must be an integer in 1..=128");
        assert!((1..=128).contains(&iterations),
            "LODESTONE_ENTITY_PUBLICATION_PERF_ITERATIONS must be in 1..=128");

        struct UnversionedPublication(CountedPublication);
        impl EntitySource for UnversionedPublication {
            fn snapshots(&self) -> Vec<EntitySnapshot> {
                self.0.snapshots()
            }
        }

        let publication = crate::LiveMobSource::default();
        publication.publish((0..512).map(|offset| {
            let mut entity = snap(1000 + offset, 1.25 + f64::from(offset) * 0.125);
            entity.metadata = vec![MetadataField::SharedFlags(0)];
            entity
        }).collect());
        let unversioned_counted = CountedPublication {
            source: publication.clone(),
            ..Default::default()
        };
        let versioned_counted = CountedPublication {
            source: publication,
            ..Default::default()
        };
        let reads = [
            std::sync::Arc::clone(&unversioned_counted.snapshot_reads),
            std::sync::Arc::clone(&versioned_counted.snapshot_reads),
        ];
        let unversioned = crate::players::PlayerAwareSource::new(
            UnversionedPublication(unversioned_counted),
            players.clone(),
        );
        let versioned = crate::players::PlayerAwareSource::new(versioned_counted, players.clone());
        let mut streamers: [EntityStreamer; 2] =
            std::array::from_fn(|_| EntityStreamer::default());
        let mut lists: [PlayerListStreamer; 2] =
            std::array::from_fn(|_| PlayerListStreamer::default());
        let mut pass = |arm: usize| {
            if arm == 0 {
                stream_pass(
                    &TagProto,
                    &unversioned,
                    &mut streamers[arm],
                    &mut lists[arm],
                    Some(&viewer),
                )
            } else {
                stream_pass(
                    &TagProto,
                    &versioned,
                    &mut streamers[arm],
                    &mut lists[arm],
                    Some(&viewer),
                )
            }
        };
        for arm in 0..2 {
            let warm = std::hint::black_box(pass(arm));
            assert_eq!(warm.len(), 1025);
            assert_eq!(reads[arm].load(Ordering::Relaxed), 1);
        }
        let mut elapsed = [std::time::Duration::ZERO; 2];
        let mut captures = [0_usize; 2];
        #[cfg(target_os = "macos")]
        let mut retired = [(0_u64, 0_u64); 2];
        for pair in 0..5 {
            let order = if pair % 2 == 0 { [0, 1] } else { [1, 0] };
            for arm in order {
                let reads_before = reads[arm].load(Ordering::Relaxed);
                #[cfg(target_os = "macos")]
                let counters_before = lodestone_testsupport::process_counters::ProcessCounters::read()
                    .expect("entity publication retired counters available");
                let started = Instant::now();
                for _ in 0..iterations {
                    let out = std::hint::black_box(pass(arm));
                    assert!(out.is_empty());
                }
                elapsed[arm] += started.elapsed();
                #[cfg(target_os = "macos")]
                {
                    let counters = lodestone_testsupport::process_counters::ProcessCounters::read()
                        .expect("entity publication retired counters available")
                        .since(counters_before).expect("monotonic retired counters");
                    retired[arm].0 += counters.instructions;
                    retired[arm].1 += counters.cycles;
                }
                let captures_now = reads[arm].load(Ordering::Relaxed) - reads_before;
                assert_eq!(captures_now, if arm == 0 { iterations } else { 0 });
                captures[arm] += captures_now;
            }
        }
        #[cfg(target_os = "macos")]
        let retired_report = format!(
            "unversioned_instructions={} versioned_instructions={} \
             unversioned_cycles={} versioned_cycles={}",
            retired[0].0, retired[1].0, retired[0].1, retired[1].1,
        );
        #[cfg(not(target_os = "macos"))]
        let retired_report = "retired_counters=unavailable";
        eprintln!(
            "ENTITY_PUBLICATION_PERF comparison=unversioned-v-versioned-publication \
             historical_baseline=false process_wide_counters=true entities=512 pairs=5 \
             alternating_order=true passes_per_arm={} unversioned_ns={} versioned_ns={} \
             unversioned_snapshot_reads={} versioned_snapshot_reads={} {retired_report}",
            iterations * 5, elapsed[0].as_nanos(), elapsed[1].as_nanos(),
            captures[0], captures[1],
        );
    }
}

#[test]
fn both_entity_sources_batch_removals_before_spawns() {
    let mut streamer = EntityStreamer::default();
    let _ = streamer.sync_with_players(
        &TagProto,
        Some(&[snap(10, 1.25)]),
        &[snap(30, 2.25)],
    );
    let out = streamer.sync_with_players(
        &TagProto,
        Some(&[snap(20, 3.75)]),
        &[snap(40, 4.75)],
    );
    assert_sent(&out, &[(REMOVE, &[10, 30]), (ADD, &[20]), (ADD, &[40])]);
    assert_eq!(streamer.last_sent.len(), 1);
    assert!(streamer.last_sent.contains_key(&20));
    assert_eq!(streamer.players_last_sent.len(), 1);
    assert!(streamer.players_last_sent.contains_key(&40));
}

#[test]
fn boss_bar_publications_stream_while_entity_revision_is_unchanged() {
    let source = CountedPublication::default();
    let mut streamer = EntityStreamer::default();
    let mut list = PlayerListStreamer::default();
    assert!(stream_pass(&TagProto, &source, &mut streamer, &mut list, None).is_empty());
    let mut bar = BossBarSnapshot {
        id: Uuid::from_u128(3),
        name: Text::literal("Boss"),
        progress: 0.875,
        visible: true,
    };
    source.source.publish_boss_bars(vec![bar.clone()]);
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(BOSS_ADD, &[0x3f, 0x60, 0, 0])]);
    bar.progress = 0.625;
    source.source.publish_boss_bars(vec![bar]);
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(BOSS_PROGRESS, &[0x3f, 0x20, 0, 0])]);
    source.source.publish_boss_bars(Vec::new());
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(BOSS_REMOVE, &[])]);
    assert_eq!(source.snapshot_reads.load(Ordering::Relaxed), 1);
}

#[test]
fn first_sync_spawns_every_entity_in_source_order() {
    let mut s = EntityStreamer::default();
    let out = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]);
    assert_eq!(out.len(), 2);
    assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
    assert_eq!(sent(&out[1]), (ADD, [20u8].as_slice()));
}

#[test]
fn resync_with_no_change_emits_nothing() {
    let mut s = EntityStreamer::default();
    let world = [snap(10, 0.0), snap(20, 0.0)];
    let _ = s.sync(&TagProto, &world);
    let out = s.sync(&TagProto, &world);
    assert!(out.is_empty(), "unchanged world must not re-send: {out:?}");
}

#[test]
fn moved_entity_emits_a_single_update() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap(10, 0.0)]);
    let out = s.sync(&TagProto, &[snap(10, 5.0)]);
    assert_eq!(out.len(), 1);
    assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
}

#[test]
fn vanished_entity_is_removed_and_removals_batch() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0), snap(30, 0.0)]);
    let out = s.sync(&TagProto, &[snap(10, 0.0)]);
    // Both 20 and 30 gone -> one batched REMOVE carrying both ids.
    assert_eq!(out.len(), 1);
    let (id, payload) = sent(&out[0]);
    assert_eq!(id, REMOVE);
    let mut ids: Vec<u8> = payload.to_vec();
    ids.sort_unstable();
    assert_eq!(ids, vec![20, 30]);
}

#[test]
fn readding_a_removed_id_spawns_it_again() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]);
    let _ = s.sync(&TagProto, &[snap(10, 0.0)]); // 20 removed
    let out = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]); // 20 back
    assert_eq!(out.len(), 1);
    assert_eq!(sent(&out[0]), (ADD, [20u8].as_slice()));
}

/// [`snap`] with a non-empty `metadata` — the metadata field list,
/// generic to any entity (not creeper-specific: `EntityStreamer::sync`
/// treats `metadata` uniformly, so a `CreeperSwellDir`/`CreeperIgnited`
/// pair exercises the same code path the next mob's fields will).
fn snap_with_metadata(id: i32, x: f64, metadata: Vec<MetadataField>) -> EntitySnapshot {
    EntitySnapshot { metadata, ..snap(id, x) }
}

/// A spawn whose snapshot already carries non-empty metadata must send
/// `ADD` followed by a metadata sync. The separate metadata frame carries
/// the initial non-default values, including a visible "no swelling
/// animation" transition.
#[test]
fn spawn_with_non_empty_metadata_sends_add_then_metadata() {
    let mut s = EntityStreamer::default();
    let fields = vec![MetadataField::CreeperSwellDir(1)];
    let out = s.sync(&TagProto, &[snap_with_metadata(10, 0.0, fields)]);
    assert_eq!(out.len(), 2);
    assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
    assert_eq!(sent(&out[1]).0, METADATA);
}

#[test]
fn effect_shared_flags_trigger_the_real_metadata_stream_path() {
    let mut streamer = EntityStreamer::default();
    let out = streamer.sync(
        &TagProto,
        &[snap_with_metadata(10, 0.0, vec![MetadataField::SharedFlags(0x20)])],
    );
    assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
    assert_eq!(sent(&out[1]), (METADATA, [10u8, 1].as_slice()));

    let cleared = streamer.sync(
        &TagProto,
        &[snap_with_metadata(10, 0.0, vec![MetadataField::SharedFlags(0)])],
    );
    assert_eq!(sent(&cleared[0]), (UPDATE, [10u8].as_slice()));
    assert_eq!(sent(&cleared[1]), (METADATA, [10u8, 1].as_slice()));
}

#[test]
fn live_air_supply_helper_consumes_the_active_effect_store() {
    let mut water_breathing = ActiveEffects::new();
    water_breathing.apply("minecraft:water_breathing", 200, 0);
    let mut refilling = PlayerVitals::restored(crate::vitals::MAX_HEALTH, 20);
    assert_eq!(
        tick_player_air_supply(&mut refilling, true, false, &water_breathing).air_changed,
        Some(24),
        "the production helper must carry Water Breathing through to the air ticker"
    );

    let mut ordinary = PlayerVitals::restored(crate::vitals::MAX_HEALTH, 20);
    assert_eq!(
        tick_player_air_supply(&mut ordinary, true, false, &ActiveEffects::new()).air_changed,
        Some(19),
        "the no-effect control must still deplete rather than universally refill"
    );
}

#[test]
fn live_saturation_helper_updates_the_authoritative_food_snapshot() {
    let mut vitals = PlayerVitals::restored(crate::vitals::MAX_HEALTH, 300);
    vitals.set_food(crate::food::FoodData::restored(16, 0.0, 0.0, 0));
    assert!(apply_effect_saturation(&mut vitals, 3));
    assert_eq!(vitals.food().food_level(), 19);
    assert_eq!(vitals.food().saturation(), 6.0);

    let mut full = PlayerVitals::default();
    full.set_food(crate::food::FoodData::restored(20, 20.0, 0.0, 0));
    assert!(
        !apply_effect_saturation(&mut full, 1),
        "a full food/saturation snapshot must not request a redundant packet"
    );
}

/// The production publication helper must emit both the folded attribute
/// and the current health frame when Health Boost is added and removed.
/// The removal arm is the important control: it catches an implementation
/// that grows the extra hearts correctly but leaves their client-side row
/// after expiry.
#[tokio::test]
async fn health_boost_attribute_and_health_reach_the_real_connection_path() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut peer = Connection::new(client_end);
    let mut state = State::Play;
    let mut vitals = PlayerVitals::default();
    let mut effects = ActiveEffects::new();

    effects.apply("minecraft:health_boost", 1, 0);
    assert!(
        sync_effect_max_health(&mut conn, &mut state, &TagProto, &mut vitals, &effects)
            .await
            .expect("the active effect must publish")
    );
    assert_eq!(vitals.max_health(), 24.0);
    assert_eq!(peer.read_packet().await.expect("attribute frame"), Some((ATTRIBUTES, vec![24])));
    assert_eq!(peer.read_packet().await.expect("health frame"), Some((HEALTH, vec![20])));

    // The one-tick duration expires through the same store/tick path the
    // live timer uses; direct removal would not prove the expiry arm.
    let _ = effects.tick(0, vitals.health(), vitals.max_health());
    assert!(
        sync_effect_max_health(&mut conn, &mut state, &TagProto, &mut vitals, &effects)
            .await
            .expect("expiry must publish the restored ceiling")
    );
    assert_eq!(vitals.max_health(), crate::vitals::MAX_HEALTH);
    assert_eq!(peer.read_packet().await.expect("restored attribute frame"), Some((ATTRIBUTES, vec![20])));
    assert_eq!(peer.read_packet().await.expect("restored health frame"), Some((HEALTH, vec![20])));
}

/// The death choke point must consume Wind Charged exactly where health
/// crosses zero and put the existing small-gust world effect on the real
/// outbound connection.  The empty-effect control distinguishes a death
/// packet from the effect-specific particle frame.
#[tokio::test]
async fn wind_charged_death_reaches_the_real_connection_path() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut peer = Connection::new(client_end);
    let mut state = State::Play;
    let mut vitals = PlayerVitals::default();
    vitals.kill();
    let mut effects = ActiveEffects::new();
    effects.apply("minecraft:wind_charged", 200, 0);
    let mut advancements = AdvancementManager::new(Vec::new()).expect("empty advancement tree");

    publish_health(
        &mut conn,
        &mut state,
        &TagProto,
        &vitals,
        &effects,
        Vec3::new(4.0, 70.0, 9.0),
        LOCAL_PLAYER_ENTITY_ID,
        "Player",
        crate::vitals::DeathCause::GenericKill,
        &mut advancements,
        Uuid::nil(),
        None,
    )
    .await
    .expect("death publication");

    assert_eq!(peer.read_packet().await.expect("health frame"), Some((HEALTH, vec![0])));
    assert_eq!(
        peer.read_packet().await.expect("small-gust frame"),
        Some((
            PARTICLES,
            [4.0f64.to_be_bytes(), 70.9f64.to_be_bytes(), 9.0f64.to_be_bytes()].concat(),
        )),
        "the effect must use the existing level-particle consumer at the midpoint"
    );

    let no_effect = ActiveEffects::new();
    assert_eq!(no_effect.death_trigger(), None, "control: no active trigger means no gust frame");
}

/// Control: a spawn with *empty* metadata (every existing test's `snap`)
/// must send only `ADD` — proves the metadata branch above is
/// conditional, not unconditional padding on every spawn.
#[test]
fn spawn_with_empty_metadata_sends_only_add() {
    let mut s = EntityStreamer::default();
    let out = s.sync(&TagProto, &[snap(10, 0.0)]);
    assert_eq!(out.len(), 1);
    assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
}

/// A metadata-only change (position/rotation/velocity all unchanged)
/// must still be caught — `EntitySnapshot`'s derived `PartialEq` covers
/// `metadata`, so `Some(prev) if prev != entity` fires exactly as it
/// would for a moved entity, and re-encodes both the (redundant, but
/// harmless) position/rotation update and the metadata sync.
#[test]
fn metadata_only_change_is_caught_even_with_no_motion() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap_with_metadata(10, 0.0, vec![MetadataField::CreeperSwellDir(-1)])]);
    let out = s.sync(
        &TagProto,
        &[snap_with_metadata(10, 0.0, vec![MetadataField::CreeperSwellDir(1)])],
    );
    assert_eq!(out.len(), 2, "expected UPDATE then METADATA, got {out:?}");
    assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
    assert_eq!(sent(&out[1]).0, METADATA);
}

/// Negative control for the test above: re-syncing the exact same
/// metadata (no change at all) must emit nothing, proving the branch is
/// a real diff and not "always resend metadata once present."
#[test]
fn unchanged_metadata_emits_nothing_on_resync() {
    let mut s = EntityStreamer::default();
    let snapshot = snap_with_metadata(10, 0.0, vec![MetadataField::CreeperIgnited(true)]);
    let _ = s.sync(&TagProto, &[snapshot.clone()]);
    let out = s.sync(&TagProto, &[snapshot]);
    assert!(out.is_empty(), "unchanged metadata must not re-send: {out:?}");
}

/// [`snap`] with a `leash_link` already set — the spawn-time link packet.
fn snap_leashed(id: i32, x: f64, target: i32) -> EntitySnapshot {
    EntitySnapshot { leash_link: Some(target), ..snap(id, x) }
}

/// **The discriminating case.** A mob that is *already* leashed by the time
/// a client first spawns it — a fresh join, or walking back into view range
/// after the attach happened — must still get the rope: `ADD` followed by
/// `LINK`. A test that only ever leashes a mob the streamer has already sent
/// once after spawn would not cover this branch. The snapshot therefore
/// includes the link at spawn and requires both records.
#[test]
fn a_mob_already_leashed_on_spawn_sends_add_then_link() {
    let mut s = EntityStreamer::default();
    // Pairwise-distinct ids (10, 0, 77): a transposition of `source_id` and
    // `target_id` inside the encoder would otherwise be invisible — the
    // wire-shape reason this repo's own CLAUDE.md gives for two adjacent
    // same-typed fields.
    let out = s.sync(&TagProto, &[snap_leashed(10, 0.0, 77)]);
    assert_eq!(out.len(), 2, "expected ADD then LINK, got {out:?}");
    assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
    assert_eq!(sent(&out[1]), (LINK, [10u8, 77u8].as_slice()));
}

/// A fresh attach — `None` on the first sync, `Some` on the second — must
/// send `UPDATE` then `LINK`, with the link payload carrying the real
/// target rather than the "no holder" sentinel.
#[test]
fn attaching_a_leash_after_spawn_sends_update_then_link() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap(10, 0.0)]);
    let out = s.sync(&TagProto, &[snap_leashed(10, 0.0, 77)]);
    assert_eq!(out.len(), 2, "expected UPDATE then LINK, got {out:?}");
    assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
    assert_eq!(sent(&out[1]), (LINK, [10u8, 77u8].as_slice()));
}

/// A detach — `Some` then `None` — must send `UPDATE` then `LINK` again,
/// this time carrying the "no holder" sentinel (`255` in this test
/// protocol's own encoding), proving the diff fires on the way down too,
/// not only on the way up.
#[test]
fn detaching_a_leash_sends_update_then_link_with_no_target() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap_leashed(10, 0.0, 77)]);
    let out = s.sync(&TagProto, &[snap(10, 0.0)]);
    assert_eq!(out.len(), 2, "expected UPDATE then LINK, got {out:?}");
    assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
    assert_eq!(sent(&out[1]), (LINK, [10u8, 255u8].as_slice()));
}

/// Negative control: re-syncing the same `leash_link` (still `Some`, same
/// target) must emit nothing extra beyond position/rotation — proving the
/// branch is a real diff, matching the metadata family's own control.
#[test]
fn unchanged_leash_link_emits_no_extra_link_on_resync() {
    let mut s = EntityStreamer::default();
    let snapshot = snap_leashed(10, 0.0, 77);
    let _ = s.sync(&TagProto, &[snapshot.clone()]);
    let out = s.sync(&TagProto, &[snapshot]);
    assert!(out.is_empty(), "unchanged leash_link must not re-send LINK: {out:?}");
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


#[test]
fn only_the_latest_teleport_acknowledgement_releases_movement() {
    let mut acknowledgements = TeleportAcknowledgements::after_initial(41);
    let replacement = acknowledgements.issue();

    assert_eq!(replacement, 42);
    assert!(
        !acknowledgements.accepts(41),
        "a late acknowledgement for the superseded join correction must stay pending"
    );
    assert!(
        acknowledgements.is_pending(),
        "a stale acknowledgement must not clear the newer correction"
    );
    assert!(acknowledgements.accepts(42));
    assert!(
        !acknowledgements.is_pending(),
        "the current acknowledgement must release the movement gate"
    );
    assert!(
        !acknowledgements.accepts(42),
        "a duplicate acknowledgement must not recreate an accepted state"
    );
}
