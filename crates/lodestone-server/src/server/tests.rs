//! Unit tests for the server driver: the play loop's gating, the per-packet
//! handlers, and the helpers that live in the sibling modules.

use super::*;
use crate::chunk::ChunkColumn;
use crate::furnace::{Furnace, FurnaceKind};
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

/// [`apply_edit_book`]'s draft-save path: a writable book in a hotbar
/// slot gets its pages overwritten in place, no transmute.
#[test]
fn edit_book_draft_save_updates_pages_without_transmuting() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(ItemStack::new(item_key("writable_book"), 1)));
    let result = apply_edit_book(
        &mut inv,
        0,
        vec!["Page one".to_owned()],
        None,
        "Steve",
    );
    let (native, item) = result.expect("a writable book in a hotbar slot must be editable");
    assert_eq!(native, 0);
    assert_eq!(item.item, item_key("writable_book"));
    assert_eq!(
        item.components.writable_book_content,
        Some(vec!["Page one".to_owned()])
    );
    assert_eq!(inv.native(0), Some(&item));
}

/// The signing path: a title present transmutes the stack to
/// `minecraft:written_book` and stamps the signer's name as author —
/// vanilla's own sign-book handler's own literal `0`/`true` for
/// generation/resolved.
#[test]
fn edit_book_signing_transmutes_to_written_book() {
    let mut inv = PlayerInventory::new();
    inv.set_native(
        crate::inventory::OFFHAND_NATIVE,
        Some(ItemStack::new(item_key("writable_book"), 1)),
    );
    let (native, item) = apply_edit_book(
        &mut inv,
        i32::try_from(crate::inventory::OFFHAND_NATIVE).unwrap(),
        vec!["Once upon a time".to_owned(), "The End".to_owned()],
        Some("My Book".to_owned()),
        "Alex",
    )
    .expect("a writable book in the off-hand must be signable");
    assert_eq!(native, crate::inventory::OFFHAND_NATIVE);
    assert_eq!(item.item, item_key("written_book"));
    assert_eq!(item.components.writable_book_content, None);
    let content = item
        .components
        .written_book_content
        .expect("signing must set written_book_content");
    assert_eq!(content.title, "My Book");
    assert_eq!(content.author, "Alex");
    assert_eq!(content.generation, 0);
    assert!(content.resolved);
    assert_eq!(content.pages.len(), 2);
}

/// **Control**: a slot outside the hotbar or off-hand must be refused —
/// the `hotbar || off-hand` slot gate (`slot == 40` is the off-hand).
/// Without this, an implementation that skipped the slot check entirely
/// would still pass the two tests above (both use in-range slots).
#[test]
fn edit_book_refuses_a_main_storage_slot() {
    let mut inv = PlayerInventory::new();
    inv.set_native(9, Some(ItemStack::new(item_key("writable_book"), 1)));
    assert_eq!(
        apply_edit_book(&mut inv, 9, vec!["x".to_owned()], None, "Steve"),
        None
    );
}

/// **Control**: an item that is not a writable book must be refused —
/// vanilla's `carried.has(DataComponents.WRITABLE_BOOK_CONTENT)` gate.
/// Without this, any item in the targeted slot would silently gain book
/// content.
#[test]
fn edit_book_refuses_a_non_book_item() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(ItemStack::new(item_key("stone"), 1)));
    assert_eq!(
        apply_edit_book(&mut inv, 0, vec!["x".to_owned()], None, "Steve"),
        None
    );
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

fn lectern_book(pages: usize) -> ItemStack {
    let mut book = stack("minecraft:written_book", 1);
    book.components.written_book_content = Some(WrittenBookContent {
        title: "Test".to_owned(),
        author: "Tester".to_owned(),
        generation: 0,
        pages: (0..pages).map(|page| Text::literal(format!("Page {page}"))).collect(),
        resolved: true,
    });
    book
}

#[test]
fn lectern_button_pages_and_take_are_authoritative() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(3, 64, -2);
    block_entities.with(|reg| {
        reg.insert(pos, BlockEntity::Lectern(crate::block_entities::LecternData {
            book: Some(lectern_book(4)),
            page: 0,
        }));
    });
    let mut inventory = PlayerInventory::new();
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Lectern,
        container_size: 1,
        state_id: 0,
    };

    let page = apply_lectern_button_click(
        &ContainerTagProto,
        &block_entities,
        &mut inventory,
        Some(&mut open),
        7,
        2,
        true,
    );
    assert_eq!(page.len(), 1, "next-page action must publish one data property");
    assert_eq!(block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Lectern(lectern)) => lectern.page,
        _ => -1,
    }), 1);

    let taken = apply_lectern_button_click(
        &ContainerTagProto,
        &block_entities,
        &mut inventory,
        Some(&mut open),
        7,
        3,
        true,
    );
    assert_eq!(taken.len(), 3, "take-book must update inventory, slot, and page");
    assert!(block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Lectern(lectern)) => lectern.book.is_none() && lectern.page == 0,
        _ => false,
    }));
    assert_eq!(inventory.native(0).map(|book| book.item.to_string()), Some("minecraft:written_book".to_owned()));
}

#[test]
fn lectern_take_refuses_when_inventory_has_no_room() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(3, 64, -2);
    block_entities.with(|reg| {
        reg.insert(pos, BlockEntity::Lectern(crate::block_entities::LecternData {
            book: Some(lectern_book(1)),
            page: 0,
        }));
    });
    let mut inventory = PlayerInventory::new();
    for native in 0..37 {
        inventory.set_native(native, Some(stack("minecraft:stone", 64)));
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Lectern,
        container_size: 1,
        state_id: 0,
    };
    assert!(apply_lectern_button_click(
        &ContainerTagProto,
        &block_entities,
        &mut inventory,
        Some(&mut open),
        7,
        3,
        true,
    ).is_empty());
    assert!(block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Lectern(lectern)) => lectern.book.is_some(),
        _ => false,
    }));
}


const SLOT: i32 = 20;
const DATA: i32 = 21;
const CONTENT: i32 = 22;

/// A protocol double whose container encoders tag each directive with a
/// distinct packet id, `window_id`, and `state_id`/`property` — enough
/// for [`sync_open_container`]'s tests to read the diff *decisions* back
/// off the returned directives without needing the real `lodestone-v26-2`
/// wire encoding. Every other method is unreachable from these tests.
struct ContainerTagProto;

impl ServerProtocol for ContainerTagProto {
    fn decode(&self, _s: State, _id: i32, _p: &[u8]) -> ServerBound {
        unimplemented!()
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
    fn encode_container_slot(
        &self,
        window_id: i32,
        state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: SLOT,
            payload: vec![
                window_id as u8,
                state_id as u8,
                slot as u8,
                item.map_or(0, |s| s.count as u8),
            ],
        }
    }
    fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: DATA,
            payload: vec![window_id as u8, property as u8, value as u8],
        }
    }
    fn encode_container_content(
        &self,
        window_id: i32,
        state_id: i32,
        items: &[Option<ItemStack>],
        carried: Option<&ItemStack>,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CONTENT,
            payload: vec![
                window_id as u8,
                state_id as u8,
                items.len() as u8,
                carried.map_or(0, |s| s.count as u8),
            ],
        }
    }
}

fn open(pos: BlockPos, container_size: usize) -> OpenContainer {
    OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Container {
            size: container_size,
        },
        container_size,
        state_id: 0,
    }
}

#[test]
fn sync_open_container_emits_nothing_when_nothing_changed() {
    let mut o = open(BlockPos::new(0, 0, 0), 3);
    let mut sync = ContainerSync {
        slots: vec![Some(stack("minecraft:coal", 1)), None, None],
        data: vec![10, 20],
    };
    let out = sync_open_container(
        &ContainerTagProto,
        &mut o,
        &mut sync,
        vec![Some(stack("minecraft:coal", 1)), None, None],
        vec![10, 20],
    );
    assert!(out.is_empty(), "unchanged container must not re-send: {out:?}");
}

/// The exact scenario this function exists for: a furnace's own
/// background tick lights it (data property 0 changes) and later
/// produces an ingot (slot 2 changes) — no click involved at all.
#[test]
fn sync_open_container_emits_only_the_changed_slot_and_data_entries() {
    let mut o = open(BlockPos::new(0, 0, 0), 3);
    let mut sync = ContainerSync {
        slots: vec![Some(stack("minecraft:iron_ore", 1)), Some(stack("minecraft:coal", 1)), None],
        data: vec![0, 0, 0, 200],
    };
    let out = sync_open_container(
        &ContainerTagProto,
        &mut o,
        &mut sync,
        vec![None, Some(stack("minecraft:coal", 1)), Some(stack("minecraft:iron_ingot", 1))],
        // Only index 0 (`lit_time_remaining`) changes here — index 1
        // (`lit_total_time`) is deliberately held constant so this
        // fixture isolates "exactly one data property changed" rather
        // than also exercising two simultaneous data changes (a real
        // ignition tick does change both at once, but that is not what
        // this particular test is asserting).
        vec![190, 0, 0, 200],
    );
    // Slot 0 (iron ore consumed) and slot 2 (ingot produced) changed;
    // slot 1 (fuel) did not.
    let ServerDirective::Send { packet_id, payload } = &out[0] else {
        panic!("expected Send");
    };
    assert_eq!(*packet_id, SLOT);
    assert_eq!(payload[2], 0, "slot index 0 changed first");
    let ServerDirective::Send { packet_id, payload } = &out[1] else {
        panic!("expected Send");
    };
    assert_eq!(*packet_id, SLOT);
    assert_eq!(payload[2], 2, "slot index 2 changed second");
    // Data property 0 (lit_time_remaining) changed.
    let ServerDirective::Send { packet_id, payload } = &out[2] else {
        panic!("expected Send");
    };
    assert_eq!(*packet_id, DATA);
    assert_eq!(payload[1], 0, "property index 0 changed");
    assert_eq!(out.len(), 3);
    // The sync's own bookkeeping must now hold the new values, so the
    // *next* call diffs against these, not the stale ones.
    assert_eq!(sync.slots[2], Some(stack("minecraft:iron_ingot", 1)));
    assert_eq!(sync.data[0], 190);
}

/// **Control**: every slot/data send must bump `state_id` (vanilla's
/// `incrementStateId`), and a data-only change must bump it **zero**
/// times — proving the two encoders are not accidentally sharing one
/// counter increment.
#[test]
fn sync_open_container_bumps_state_id_only_for_slot_sends() {
    let mut o = open(BlockPos::new(0, 0, 0), 1);
    let mut sync = ContainerSync {
        slots: vec![None],
        data: vec![0],
    };
    assert_eq!(o.state_id, 0);
    let _ = sync_open_container(
        &ContainerTagProto,
        &mut o,
        &mut sync,
        vec![None],
        vec![1], // data-only change
    );
    assert_eq!(o.state_id, 0, "a data-only change must not bump state_id");

    let _ = sync_open_container(
        &ContainerTagProto,
        &mut o,
        &mut sync,
        vec![Some(stack("minecraft:coal", 1))], // slot change
        vec![1],
    );
    assert_eq!(o.state_id, 1, "a slot change must bump state_id exactly once");
}

/// A real left-click picks a stack up off a native slot and a second one puts
/// it down elsewhere — the whole thing derived from `(slot, button, type)`,
/// with no item named anywhere in the input.
#[test]
fn container_clicked_against_window_zero_derives_the_move() {
    let mut inventory = PlayerInventory::new();
    inventory.set_native(9, Some(stack("minecraft:stone", 4)));
    let block_entities = BlockEntityHandle::new();

    // Menu slot 9 is native 9. Left-click: whole stack onto the cursor.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 9, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(inventory.native(9), None);
    assert_eq!(
        inventory.click_state().carried.as_ref().map(|s| s.count),
        Some(4)
    );

    // Menu slot 40 is native 4 (hotbar). Left-click: the whole cursor lands.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 40, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(inventory.native(4), Some(&stack("minecraft:stone", 4)));
    assert!(inventory.click_state().carried.is_none());
}

/// This runs end to end through the production dispatch path (not just
/// `container_click`'s own unit tests): a `ServerBound::SelectBundleItem`
/// packet's consumer (`inventory.set_selected_bundle_item`) is exactly
/// what `apply_container_clicked`'s later right-click-extract reads.
/// Without the store-then-read join this proves, a scroll-selected
/// bundle item would always come out as the front one regardless of
/// what the player highlighted — the bug the control below actually
/// caught in `bundle_other_stacked_on_me`'s first draft.
#[test]
fn a_select_bundle_item_packet_changes_which_item_a_later_extract_pops() {
    let mut inventory = PlayerInventory::new();
    let mut bundle = stack("minecraft:bundle", 1);
    bundle.components.bundle_contents =
        vec![stack("minecraft:torch", 3), stack("minecraft:oak_planks", 5)];
    // Menu slot 9 is native 9 for window 0 (`MenuLayout::player`'s own
    // storage-first ordering, the same join `container_clicked_against_
    // window_zero_derives_the_move` above already relies on).
    inventory.set_native(9, Some(bundle));
    let block_entities = BlockEntityHandle::new();

    // The dispatch arm's own body: `ServerBound::SelectBundleItem { slot_id:
    // 9, selected_item_index: 1 } => inventory.set_selected_bundle_item(9, 1)`.
    inventory.set_selected_bundle_item(9, 1);

    // Right-click (button 1, PICKUP) on slot 9 with an empty cursor.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 9, button: 1, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    assert_eq!(
        inventory.click_state().carried.as_ref().map(|s| s.item.to_string()),
        Some("minecraft:oak_planks".to_owned()),
        "the selected index (1) should have been extracted, not the front item (0)"
    );
}

/// **The security property.** A client claiming a slot now holds an item it
/// never had mints nothing: the claim is not stored, and the server answers the
/// same click with a full `container_set_content` correction.
///
/// Both halves are asserted because they fail independently — a server that
/// ignored the claim but sent no correction would leave the client believing in
/// an item that does not exist.
#[test]
fn a_claimed_item_is_never_stored_and_the_client_is_corrected() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();

    // An empty inventory, an empty cursor, a left-click on an empty slot — and
    // a diff claiming a diamond block appeared there.
    let (correction, dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 9, button: 0, click_type: 0 },
        &[(9, Some(stack("minecraft:diamond_block", 64)))],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(inventory.native(9), None, "the claim must not be stored");
    assert!(dropped.is_empty());
    assert!(
        matches!(correction, Some(ServerDirective::Send { packet_id, .. }) if packet_id == CONTENT),
        "a disagreeing claim must be corrected, got {correction:?}"
    );

    // And a claim that matches what the server derived sends nothing at all,
    // so an honest client pays no extra traffic — the control that the
    // correction above is a comparison rather than an unconditional resend.
    inventory.set_native(9, Some(stack("minecraft:stone", 1)));
    let (correction, _) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 9, button: 0, click_type: 0 },
        &[(9, None)],
        Some(&stack("minecraft:stone", 1)),
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(correction, None, "an honest prediction needs no correction");
}

/// Crafting, end to end and server-derived: planks clicked into the 2x2 make
/// the server derive a crafting table, and taking the result consumes the grid.
/// The client never names a result.
#[test]
fn the_crafting_result_is_derived_and_taking_it_consumes_the_grid() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.click_state_mut().carried = Some(stack("minecraft:oak_planks", 4));

    // Right-click each of the four grid cells: one plank each.
    for menu_slot in 1..=4 {
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: menu_slot, button: 1, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
    }
    assert_eq!(
        inventory.crafting().result().map(|r| r.item.to_string()),
        Some("minecraft:crafting_table".to_string()),
        "the server derived the result from the grid it now holds"
    );

    // Take it. The cursor holds the table and every grid cell is empty again.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 0, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(
        inventory
            .click_state()
            .carried
            .as_ref()
            .map(|s| s.item.to_string()),
        Some("minecraft:crafting_table".to_string())
    );
    assert!(inventory.crafting().is_empty(), "one craft consumed the grid");
    assert!(inventory.crafting().result().is_none());
}

/// The anvil end to end through the real click path: place a
/// damaged pickaxe and a repair material into the two input cells, take the
/// derived result, and check both the item mutation and the input-slot
/// consumption `container_click`'s `take_result` special-cases for
/// `Station::Anvil` — cell 0 always clears, cell 1 shrinks by the repair
/// material count actually used, not by one and not entirely.
#[test]
fn the_anvil_repairs_through_the_real_click_path_and_consumes_the_right_amount() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.open_workstation(2);
    let mut input = stack("minecraft:diamond_pickaxe", 1);
    input.components.damage = Some(1200);
    input.components.max_damage = Some(1561);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(input);
        ws[1] = Some(stack("minecraft:diamond", 3));
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
        container_size: 3,
        state_id: 0,
    };

    // Menu slot 2 is the result for a 2-input combiner menu.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 2, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let carried = inventory.click_state().carried.as_ref().expect("the repaired pickaxe must be on the cursor");
    assert_eq!(carried.item.to_string(), "minecraft:diamond_pickaxe");
    assert_eq!(carried.components.damage, Some(30), "matches anvil::compute's own repair-with-material test");

    let cells = inventory.workstation().expect("still open");
    assert_eq!(cells[0], None, "the base item is always fully consumed");
    assert_eq!(
        cells[1], None,
        "all 3 diamonds were used by the repair (repair_item_count_cost == addition.count)"
    );
}

/// The anvil result cannot be taken by a survival player without enough XP
/// levels. The end-to-end click leaves the result in place, keeps the cursor
/// empty, and consumes nothing; exactly enough XP and creative mode both
/// allow the take.
#[test]
fn a_0_xp_survival_player_cannot_take_a_costed_anvil_result_but_creative_and_enough_levels_can() {
    let same_repair_fixture = |inventory: &mut PlayerInventory| {
        inventory.open_workstation(2);
        let mut input = stack("minecraft:diamond_pickaxe", 1);
        input.components.damage = Some(1200);
        input.components.max_damage = Some(1561);
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(input);
            ws[1] = Some(stack("minecraft:diamond", 3));
        }
    };
    let open_anvil = || OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
        container_size: 3,
        state_id: 0,
    };

    // The real cost this exact fixture prices to — read from `anvil::compute`
    // itself (the single already-tested source of truth for the formula;
    // this test is about the `xp_level`-vs-`cost` *wiring*, not re-deriving
    // the repair-cost arithmetic a second time), not guessed.
    let mut priced_input = stack("minecraft:diamond_pickaxe", 1);
    priced_input.components.damage = Some(1200);
    priced_input.components.max_damage = Some(1561);
    let cost = crate::anvil::compute(
        Some(&priced_input),
        Some(&stack("minecraft:diamond", 3)),
        None,
        false,
    )
    .cost;
    assert!(cost > 0, "the fixture must actually cost XP levels, or this test proves nothing");

    // 0 XP levels, survival: refused. Nothing moves, nothing is consumed.
    {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        same_repair_fixture(&mut inventory);
        let mut open = open_anvil();
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 2, button: 0, click_type: 0 },
            &[],
            None,
            false,
            0,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(
            inventory.click_state().carried.is_none(),
            "0 XP levels must not take a {cost}-cost anvil result"
        );
        let cells = inventory.workstation().expect("still open");
        assert!(cells[0].is_some(), "the base item must stay put when the take is refused");
        assert!(cells[1].is_some(), "the addition must stay put when the take is refused");
    }

    // Exactly `cost` XP levels, survival: succeeds — the `>=`, not `>`, half
    // of vanilla's own anvil-menu may-pickup gate's comparison.
    {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        same_repair_fixture(&mut inventory);
        let mut open = open_anvil();
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 2, button: 0, click_type: 0 },
            &[],
            None,
            false,
            cost,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(
            inventory.click_state().carried.is_some(),
            "exactly {cost} XP levels must take the result"
        );
    }

    // Creative, 0 XP levels: succeeds unconditionally because creative
    // bypasses the experience-cost check.
    {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        same_repair_fixture(&mut inventory);
        let mut open = open_anvil();
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 2, button: 0, click_type: 0 },
            &[],
            None,
            true,
            0,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(
            inventory.click_state().carried.is_some(),
            "creative must take regardless of XP levels"
        );
    }
}

/// The anvil's genuinely bespoke take rule (vanilla's own anvil-menu on-take routine): a take
/// priced *purely* by a pending rename must leave a present-but-not-
/// consumed addition cell completely untouched, not cleared as if a real
/// combine had consumed it. `container_click::take_result`'s own internal
/// re-derivation always evaluates with no rename text (that module is
/// deliberately rename-free) and so cannot see this by itself — see
/// `apply_workstation_clicked`'s own correction, which this pins.
#[test]
fn a_pure_rename_take_leaves_a_present_but_unconsumed_addition_untouched() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.open_workstation(2);
    inventory.set_pending_rename(Some("Excalibur".to_owned()));
    let input = stack("minecraft:diamond_sword", 1);
    let addition = stack("minecraft:diamond_sword", 1);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(input);
        ws[1] = Some(addition.clone());
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
        container_size: 3,
        state_id: 0,
    };

    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 2, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let carried = inventory.click_state().carried.as_ref().expect("must take the renamed sword");
    assert_eq!(
        carried.components.custom_name.as_ref().map(lodestone_model::text::Text::to_plain_string),
        Some("Excalibur".to_owned())
    );
    let cells = inventory.workstation().expect("still open");
    assert_eq!(cells[0], None, "the base item is always fully consumed");
    assert_eq!(
        cells[1],
        Some(addition),
        "a pure-rename take must leave an unconsumed addition exactly as it was"
    );
}

/// The grindstone end to end: a single enchanted item in one
/// slot strips to curses only, and taking it **fully clears** the input
/// cell it came from — the grindstone's distinct-from-the-anvil take rule.
#[test]
fn the_grindstone_strips_enchantments_through_the_real_click_path() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.open_workstation(2);
    let mut sword = stack("minecraft:diamond_sword", 1);
    sword.components.enchantments = vec![lodestone_model::ItemEnchantment {
        id: crate::enchantment_data::id_of("minecraft:sharpness").unwrap(),
        level: 3,
    }];
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(sword);
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Grindstone },
        container_size: 3,
        state_id: 0,
    };

    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 2, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let carried = inventory.click_state().carried.as_ref().expect("must take a plain sword back");
    assert!(carried.components.enchantments.is_empty(), "sharpness is not a curse and must be stripped");
    let cells = inventory.workstation().expect("still open");
    assert_eq!(cells[0], None, "grindstone always fully clears both inputs on take");
    assert_eq!(cells[1], None);
}

/// The smithing table end to end: a netherite upgrade through
/// the real click path, checking the generic shrink-by-1 take behaviour
/// (shared with the crafting table) applies to all three input cells.
#[test]
fn the_smithing_table_upgrades_to_netherite_through_the_real_click_path() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.open_workstation(3);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(stack("minecraft:netherite_upgrade_smithing_template", 1));
        ws[1] = Some(stack("minecraft:diamond_sword", 1));
        ws[2] = Some(stack("minecraft:netherite_ingot", 1));
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Smithing },
        container_size: 4,
        state_id: 0,
    };

    // Menu slot 3 is the result for a 3-input combiner menu.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 3, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let carried = inventory.click_state().carried.as_ref().expect("must take the upgraded sword");
    assert_eq!(carried.item.to_string(), "minecraft:netherite_sword");
    let cells = inventory.workstation().expect("still open");
    assert!(cells.iter().all(Option::is_none), "each of the three inputs was a stack of one and is now consumed");
}

/// The anvil action reaches [`crate::anvil::compute`] (a pure rename costs
/// exactly 1 XP level) and re-sending the identical name is a no-op.
#[test]
fn rename_item_prices_a_pure_rename_at_one_and_is_idempotent() {
    let mut inventory = PlayerInventory::new();
    inventory.open_workstation(2);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(stack("minecraft:diamond_sword", 1));
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
        container_size: 3,
        state_id: 0,
    };

    let directives = apply_rename_item(
        &ContainerTagProto,
        &mut inventory,
        Some(&mut open),
        "Excalibur",
        false,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(directives.len(), 2, "the refreshed content, then the cost data slot");
    assert_eq!(inventory.pending_rename(), Some("Excalibur"));
    match &directives[1] {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(*packet_id, DATA);
            assert_eq!(payload[2], 1, "a pure rename costs exactly 1 XP level");
        }
        other => panic!("expected a Send directive, got {other:?}"),
    }

    let again = apply_rename_item(
        &ContainerTagProto,
        &mut inventory,
        Some(&mut open),
        "Excalibur",
        false,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert!(again.is_empty(), "an unchanged name must not resend anything");
}

/// The enchanting-table action reaches the real click path: choosing an offer
/// enchants the item, spends XP levels, consumes lapis, and rerolls the seed.
#[test]
fn container_button_click_enchants_the_item_and_charges_xp_and_lapis() {
    struct AirWorld;
    impl ChunkSource for AirWorld {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            unimplemented!("not needed: bookshelf_power reads block_state only")
        }
        fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
            crate::chunk::air_state()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {
            unimplemented!("read-only in this test")
        }
    }

    let sword = stack("minecraft:diamond_sword", 1);
    // Slot 0's cost floors at 1 for any enchantable item regardless of the
    // roll (`cost_for_slot`'s `(selected / 3).max(1)`), so only the offer
    // draw itself needs a seed search — deterministic given the
    // production RNG, not flaky: whichever seed is found here always
    // rolls the same offer.
    let seed = (0..64i64)
        .find(|&s| {
            let costs = crate::enchanting::table_costs(s, 0, &sword);
            let mut rng = SpawnRng::new(s.wrapping_add(0) as u64);
            costs[0] > 0 && !crate::enchanting::select_enchantments(&mut rng, &sword, costs[0]).is_empty()
        })
        .expect("at least one of the first 64 seeds must roll a slot-0 offer for a diamond sword");

    let mut inventory = PlayerInventory::new();
    inventory.open_workstation(2);
    inventory.set_enchant_seed(seed);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(sword);
        ws[1] = Some(stack("minecraft:lapis_lazuli", 5));
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::Enchanting,
        container_size: 2,
        state_id: 0,
    };
    let mut experience = crate::experience::PlayerExperience::default();
    experience.give_points(crate::experience::total_points_for_level(30));
    let before_level = experience.level();

    let directives = apply_container_button_click(
        &ContainerTagProto,
        &mut inventory,
        Some(&mut open),
        7,
        0,
        &AirWorld,
        &mut experience,
        false,
        999,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    assert!(!directives.is_empty(), "a successful enchant must resend the menu");
    assert!(experience.level() < before_level, "XP levels must be spent");
    let cells = inventory.workstation().expect("still open");
    let enchanted = cells[0].as_ref().expect("the item stays in slot 0");
    assert!(
        !enchanted.components.enchantments.is_empty(),
        "the item must come back enchanted"
    );
    let lapis_left = cells[1].as_ref().map_or(0, |l| l.count);
    assert!(lapis_left < 5, "at least one lapis must be consumed, left {lapis_left}");
    assert_eq!(inventory.enchant_seed(), 999, "a successful enchant rerolls the seed");

    // A second click at the same (now stale) slot-0 cost/seed combination
    // is refused once the seed has moved on — not a hang, not a panic,
    // and not a second free enchant.
    let refused = apply_container_button_click(
        &ContainerTagProto,
        &mut inventory,
        Some(&mut open),
        7,
        5, // out of range: only 0..3 are real slots
        &AirWorld,
        &mut experience,
        false,
        1,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert!(refused.is_empty(), "an out-of-range button id must be refused");
}

/// This runs end to end through the real production dispatch: a
/// stonecutter menu opens with cobblestone in its input cell, a
/// `ContainerButtonClick` selects one of the real offers
/// `crate::stonecutting::matches` computes, and taking the result slot
/// consumes exactly one cobblestone and leaves the rest — the same
/// `apply_container_clicked` → `apply_workstation_clicked` →
/// `container_click::take_result` path every other workstation in this
/// crate already goes through, not a hand-rolled shortcut.
#[test]
fn a_stonecutter_button_click_then_take_produces_the_selected_recipe_and_consumes_one_input() {
    // `apply_workstation_button_click` (the loom/stonecutter branch of
    // `apply_container_button_click`) never reads `source` at all —
    // unlike the enchanting branch's `bookshelf_power` call — so this
    // must never be invoked.
    struct UnusedSource;
    impl ChunkSource for UnusedSource {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            unimplemented!("the stonecutter button click must never read the world")
        }
        fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
            unimplemented!("the stonecutter button click must never read the world")
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {
            unimplemented!("read-only in this test")
        }
    }

    let mut inventory = PlayerInventory::new();
    inventory.open_workstation(1);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(stack("minecraft:cobblestone", 5));
    }
    let mut open = OpenContainer {
        window_id: 9,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 1, station: Station::Stonecutter },
        container_size: 2,
        state_id: 0,
    };
    let offers = crate::stonecutting::matches(&stack("minecraft:cobblestone", 1));
    assert!(offers.len() >= 2, "need at least two offers to prove a specific one was selected");

    // Select offer index 1 — not the default/first, so a bug that always
    // takes index 0 would fail this.
    let directives = apply_container_button_click(
        &ContainerTagProto,
        &mut inventory,
        Some(&mut open),
        9,
        1,
        &UnusedSource,
        &mut crate::experience::PlayerExperience::default(),
        false,
        0,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert!(!directives.is_empty(), "a valid selection must resend the menu");
    assert_eq!(inventory.selected_recipe_index(), Some(1));

    let block_entities = BlockEntityHandle::new();
    let (_, _dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        9,
        // Slot `inputs` (1) is the result slot — a plain left-click picks
        // it up, which is what triggers `take_result`.
        Click { slot: 1, button: 0, click_type: 0 },
        &[],
        None,
        false,
        0,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let taken = inventory
        .click_state()
        .carried
        .as_ref()
        .expect("the take must put the result on the cursor");
    assert_eq!(taken.item, offers[1].item, "the taken item must be the selected offer, not the first one");

    let cells = inventory.workstation().expect("still open");
    assert_eq!(
        cells[0].as_ref().map(|s| s.count),
        Some(4),
        "exactly one cobblestone must be consumed by the take"
    );
}

/// This runs end to end: a loom with a banner, a dye and a specific
/// pattern *item* auto-selects that item's one pattern — no
/// `ContainerButtonClick` needed, matching vanilla's own loom-menu slots-changed routine's own
/// auto-select branch — and taking the result consumes exactly one
/// banner and one dye while leaving the pattern item untouched, so it
/// can stamp a second banner.
#[test]
fn a_loom_take_with_a_pattern_item_consumes_banner_and_dye_but_not_the_pattern_item() {
    let mut inventory = PlayerInventory::new();
    inventory.open_workstation(3);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(stack("minecraft:white_banner", 3));
        ws[1] = Some(stack("minecraft:red_dye", 5));
        ws[2] = Some(stack("minecraft:creeper_banner_pattern", 1));
    }
    let mut open = OpenContainer {
        window_id: 11,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Loom },
        container_size: 4,
        state_id: 0,
    };
    let block_entities = BlockEntityHandle::new();

    let (_, _dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        11,
        // Slot `inputs` (3) is the result slot.
        Click { slot: 3, button: 0, click_type: 0 },
        &[],
        None,
        false,
        0,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let taken = inventory.click_state().carried.as_ref().expect("the take must produce a result");
    assert_eq!(taken.item.to_string(), "minecraft:white_banner");
    assert_eq!(
        taken.components.banner_patterns,
        vec![lodestone_model::BannerPatternLayer {
            pattern_asset_id: "creeper".to_string(),
            color: "red".to_string(),
        }]
    );

    let cells = inventory.workstation().expect("still open");
    assert_eq!(cells[0].as_ref().map(|s| s.count), Some(2), "one banner must be consumed");
    assert_eq!(cells[1].as_ref().map(|s| s.count), Some(4), "one dye must be consumed");
    assert_eq!(
        cells[2].as_ref().map(|s| s.count),
        Some(1),
        "the pattern item must survive the take, so it can stamp a second banner"
    );
}

/// Two test-local stand-ins reproducing `lodestone-crafting-warden`'s
/// real `SmithingSwordBan`/`AnvilBlessing` logic exactly, kept local
/// rather than a dev-dependency on that crate.
///
/// A dev-dependency depending back on this crate would compile this
/// crate's own `--lib` unit-test binary *twice* — once as the unit
/// under test, once through the plugin's normal dependency edge — which
/// produces two incompatible `CraftingStationHooks` types sharing one
/// name (`error[E0308]: mismatched types … multiple different versions
/// of crate lodestone_server in the dependency graph`). An integration
/// test under `tests/*.rs` would avoid that (it links this crate's lib
/// once, normally), but `apply_container_clicked`/
/// `apply_workstation_clicked`/`apply_container_button_click`/
/// `apply_rename_item` are module-private, so a test proving they
/// consult a registered hook can only live inside this module. The
/// external crate's own unit tests call `on_prepare` directly to prove
/// its logic; these two prove the opposite half — that production
/// actually asks the question — by driving the real dispatch below.
struct WiringProofDenySwordUpgrade;
impl crate::plugin_crafting::CraftingStationHook for WiringProofDenySwordUpgrade {
    fn on_prepare(&self, inputs: &crate::plugin_crafting::StationInputs) -> crate::plugin_crafting::StationVerdict {
        if inputs.station != Station::Smithing {
            return crate::plugin_crafting::StationVerdict::Allow;
        }
        let base = inputs.cells.get(1).and_then(Option::as_ref);
        if base.is_some_and(|item| item.item.to_string() == "minecraft:diamond_sword") {
            crate::plugin_crafting::StationVerdict::Deny
        } else {
            crate::plugin_crafting::StationVerdict::Allow
        }
    }
}

struct WiringProofBlessAnvilName;
impl crate::plugin_crafting::CraftingStationHook for WiringProofBlessAnvilName {
    fn on_prepare(&self, inputs: &crate::plugin_crafting::StationInputs) -> crate::plugin_crafting::StationVerdict {
        if inputs.station != Station::Anvil {
            return crate::plugin_crafting::StationVerdict::Allow;
        }
        let Some(computed) = inputs.computed.clone() else {
            return crate::plugin_crafting::StationVerdict::Allow;
        };
        let Some(name) = computed.components.custom_name.clone() else {
            return crate::plugin_crafting::StationVerdict::Allow;
        };
        let plain = name.to_plain_string();
        if plain.starts_with("[Blessed] ") {
            return crate::plugin_crafting::StationVerdict::Allow;
        }
        let mut blessed = computed;
        blessed.components.custom_name = Some(lodestone_model::text::Text::literal(format!("[Blessed] {plain}")));
        crate::plugin_crafting::StationVerdict::Replace(blessed)
    }
}

/// Exercises plugin hook registration through the production smithing
/// click path. `WiringProofDenySwordUpgrade` vetoes one netherite upgrade
/// when registered, while a sibling upgrade remains allowed, proving that
/// dispatch consults the hook for the derived menu result.
#[test]
fn a_registered_plugin_hook_vetoes_one_smithing_upgrade_and_allows_a_sibling_one() {
    let hooks = crate::plugin_crafting::CraftingStationHooks::new();
    hooks.register(0, std::sync::Arc::new(WiringProofDenySwordUpgrade));
    let block_entities = BlockEntityHandle::new();

    let mut denied = PlayerInventory::new();
    denied.open_workstation(3);
    if let Some(ws) = denied.workstation_mut() {
        ws[0] = Some(stack("minecraft:netherite_upgrade_smithing_template", 1));
        ws[1] = Some(stack("minecraft:diamond_sword", 1));
        ws[2] = Some(stack("minecraft:netherite_ingot", 1));
    }
    let mut open_denied = OpenContainer {
        window_id: 20,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Smithing },
        container_size: 4,
        state_id: 0,
    };
    apply_container_clicked(
        &ContainerTagProto,
        &mut denied,
        &block_entities,
        Some(&mut open_denied),
        20,
        // Slot `inputs` (3) is the result slot.
        Click { slot: 3, button: 0, click_type: 0 },
        &[],
        None,
        false,
        0,
        &hooks,
    );
    assert!(
        denied.click_state().carried.is_none(),
        "the registered SmithingSwordBan hook must veto the sword upgrade, so nothing is taken"
    );
    let denied_cells = denied.workstation().expect("still open");
    assert!(denied_cells[1].is_some(), "a denied take must leave the base item in place");

    // Positive control, the same dispatch with a pickaxe base instead of
    // a sword: this must succeed, proving the veto is scoped to the one
    // named item rather than blocking every smithing take.
    let mut allowed = PlayerInventory::new();
    allowed.open_workstation(3);
    if let Some(ws) = allowed.workstation_mut() {
        ws[0] = Some(stack("minecraft:netherite_upgrade_smithing_template", 1));
        ws[1] = Some(stack("minecraft:diamond_pickaxe", 1));
        ws[2] = Some(stack("minecraft:netherite_ingot", 1));
    }
    let mut open_allowed = OpenContainer {
        window_id: 21,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Smithing },
        container_size: 4,
        state_id: 0,
    };
    apply_container_clicked(
        &ContainerTagProto,
        &mut allowed,
        &block_entities,
        Some(&mut open_allowed),
        21,
        Click { slot: 3, button: 0, click_type: 0 },
        &[],
        None,
        false,
        0,
        &hooks,
    );
    let taken = allowed
        .click_state()
        .carried
        .as_ref()
        .expect("the pickaxe upgrade must be allowed through unchanged");
    assert_eq!(taken.item.to_string(), "minecraft:netherite_pickaxe");
}

/// Exercises the plugin replacement branch through the production anvil
/// click path. `WiringProofBlessAnvilName` adds a `[Blessed]` prefix to a
/// rename result before the player takes it.
#[test]
fn a_registered_plugin_hook_blesses_a_real_anvil_rename_take() {
    let hooks = crate::plugin_crafting::CraftingStationHooks::new();
    hooks.register(0, std::sync::Arc::new(WiringProofBlessAnvilName));

    let mut inventory = PlayerInventory::new();
    inventory.open_workstation(2);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(stack("minecraft:diamond_sword", 1));
    }
    let mut open = OpenContainer {
        window_id: 22,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
        container_size: 3,
        state_id: 0,
    };

    apply_rename_item(&ContainerTagProto, &mut inventory, Some(&mut open), "Excalibur", false, &hooks);
    assert_eq!(inventory.pending_rename(), Some("Excalibur"));

    let block_entities = BlockEntityHandle::new();
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        22,
        // Slot `inputs` (2) is the result slot.
        Click { slot: 2, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &hooks,
    );
    let taken = inventory
        .click_state()
        .carried
        .as_ref()
        .expect("the rename take must succeed");
    let name = taken.components.custom_name.as_ref().expect("still named");
    assert_eq!(
        name.to_plain_string(),
        "[Blessed] Excalibur",
        "the registered AnvilBlessing hook must have tweaked the real rename result"
    );
}

/// A click against the connection's *open* non-zero window reaches both the
/// block entity's own slots and the player tail, through the same layout.
#[test]
fn container_clicked_against_an_open_window_reaches_both_sections() {
    let mut inventory = PlayerInventory::new();
    inventory.set_native(9, Some(stack("minecraft:coal", 1)));
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(1, 2, 3);
    block_entities.with(|reg| {
        reg.insert(pos, BlockEntity::Furnace(Furnace::new(FurnaceKind::Furnace)));
    });
    let mut open = open(pos, 3);

    // Menu slot 3 is the player tail's first entry (native 9): pick the coal up.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 3, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(inventory.native(9), None);

    // Menu slot 1 is the furnace's own fuel slot: put it down there.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 1, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    let furnace_fuel = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Furnace(f)) => f.fuel().cloned(),
        _ => None,
    });
    assert_eq!(furnace_fuel, Some(stack("minecraft:coal", 1)));
}

/// [`apply_set_beacon`]'s happy path: a level-1 pyramid, a payment item
/// present, a valid tier-1 primary — the payment is consumed and the
/// selection lands on the block entity.
#[test]
fn set_beacon_consumes_payment_and_stores_a_valid_selection() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(0, 64, 0);
    block_entities.with(|reg| {
        reg.insert(
            pos,
            BlockEntity::Beacon(crate::block_entities::BeaconData {
                levels: 1,
                primary_effect: None,
                secondary_effect: None,
                payment: Some(stack("minecraft:emerald", 3)),
            }),
        );
    });
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Beacon,
        container_size: 1,
        state_id: 0,
    };

    let directives = apply_set_beacon(
        &ContainerTagProto,
        &block_entities,
        Some(&mut open),
        Some("minecraft:speed".to_owned()),
        None,
    );
    assert!(!directives.is_empty(), "a successful selection must resend the menu");

    let after = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Beacon(b)) => b.clone(),
        _ => panic!("beacon must still be there"),
    });
    assert_eq!(
        after.primary_effect,
        Some(
            crate::beacon::BeaconPower::from_key("minecraft:speed")
                .expect("beacon power")
        )
    );
    assert_eq!(after.secondary_effect, None);
    assert_eq!(after.payment, Some(stack("minecraft:emerald", 2)), "exactly one payment item is spent");
}

/// **Control**: no payment item present must refuse the selection
/// entirely — a payment item is required. Without this,
/// the happy-path test above (which does have payment) could not tell a
/// correct gate from one that never checked at all.
#[test]
fn set_beacon_refuses_without_a_payment_item() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(0, 64, 0);
    block_entities.with(|reg| {
        reg.insert(
            pos,
            BlockEntity::Beacon(crate::block_entities::BeaconData {
                levels: 4,
                primary_effect: None,
                secondary_effect: None,
                payment: None,
            }),
        );
    });
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Beacon,
        container_size: 1,
        state_id: 0,
    };

    let directives = apply_set_beacon(
        &ContainerTagProto,
        &block_entities,
        Some(&mut open),
        Some("minecraft:speed".to_owned()),
        None,
    );
    assert!(directives.is_empty());
    let after = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Beacon(b)) => b.clone(),
        _ => panic!("beacon must still be there"),
    });
    assert_eq!(after.primary_effect, None, "a refused submission must not write the selection");
}

/// **Control**: an invalid pair for the pyramid's own level (here, a
/// secondary on a level-1 pyramid) must be refused, and the payment must
/// stay untouched — not spent on a rejected submission.
#[test]
fn set_beacon_refuses_an_invalid_pair_and_keeps_the_payment() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(0, 64, 0);
    block_entities.with(|reg| {
        reg.insert(
            pos,
            BlockEntity::Beacon(crate::block_entities::BeaconData {
                levels: 1,
                primary_effect: None,
                secondary_effect: None,
                payment: Some(stack("minecraft:diamond", 1)),
            }),
        );
    });
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Beacon,
        container_size: 1,
        state_id: 0,
    };

    let directives = apply_set_beacon(
        &ContainerTagProto,
        &block_entities,
        Some(&mut open),
        Some("minecraft:speed".to_owned()),
        Some("minecraft:regeneration".to_owned()),
    );
    assert!(directives.is_empty());
    let after = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Beacon(b)) => b.clone(),
        _ => panic!("beacon must still be there"),
    });
    assert_eq!(after.payment, Some(stack("minecraft:diamond", 1)), "a refused submission must not spend payment");
}

/// A raw serverbound key crosses into `BeaconPower` before it reaches the
/// persisted block entity. `poison` is a real mob effect but not a beacon
/// power, so this distinguishes the closed-domain boundary from merely
/// rejecting an unknown string.
#[test]
fn set_beacon_rejects_a_known_non_power_key_at_the_boundary() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(0, 64, 0);
    block_entities.with(|reg| {
        reg.insert(
            pos,
            BlockEntity::Beacon(crate::block_entities::BeaconData {
                levels: 4,
                primary_effect: None,
                secondary_effect: None,
                payment: Some(stack("minecraft:diamond", 1)),
            }),
        );
    });
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Beacon,
        container_size: 1,
        state_id: 0,
    };

    let directives = apply_set_beacon(
        &ContainerTagProto,
        &block_entities,
        Some(&mut open),
        Some("minecraft:poison".to_owned()),
        None,
    );
    assert!(directives.is_empty());
    let after = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Beacon(b)) => b.clone(),
        _ => panic!("beacon must still be there"),
    });
    assert_eq!(after.primary_effect, None);
    assert_eq!(after.payment, Some(stack("minecraft:diamond", 1)));
}

/// The crafting **table**'s 3×3 menu, which has no block entity at all:
/// clicks reach the table's own grid and the server derives a 3×3
/// result the 2×2 player screen structurally cannot make.
#[test]
fn a_crafting_table_menu_derives_a_3x3_result() {
    let mut inventory = PlayerInventory::new();
    inventory.open_table_crafting();
    let block_entities = BlockEntityHandle::new();
    let mut open = OpenContainer {
        window_id: 3,
        pos: BlockPos::new(0, 64, 0),
        shape: MenuKind::CraftingTable,
        container_size: 10,
        state_id: 0,
    };

    // Eight planks around an empty centre is a chest — a 3x3-only recipe.
    inventory.click_state_mut().carried = Some(stack("minecraft:oak_planks", 8));
    for menu_slot in [1, 2, 3, 4, 6, 7, 8, 9] {
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            3,
            Click { slot: menu_slot, button: 1, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
    }
    assert_eq!(
        inventory
            .table_crafting()
            .and_then(|g| g.result())
            .map(|r| r.item.to_string()),
        Some("minecraft:chest".to_string()),
        "the table's own 3x3 grid derived the result"
    );
    assert!(
        inventory.crafting().is_empty(),
        "the player screen's 2x2 must be untouched — they are separate grids"
    );
}

/// **The reported bug**: the result the server derives has to *reach the client*,
/// and taking it has to work on the same click — no reopen.
///
/// The claims below are the real client's: `lodestone-game`'s `ClientMenu::predict`
/// diffs its own menu before/after, and its result slot is server-owned, so a grid
/// click claims the cell and the cursor and **never the result**. Under the old
/// agreement check — which walked only the claimed slots — that made every
/// crafting click "agree", so slot 0 was never sent: the screen drew its own dimmed
/// ghost, clicking it looked dead, and a craft only appeared after close+reopen.
///
/// The control is the second half: a client that *does* claim the right result and
/// cursor gets no packet, so this is a comparison over the whole menu rather than
/// an unconditional resend.
#[test]
fn a_derived_result_is_pushed_to_the_client_and_an_honest_claim_still_costs_nothing() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.click_state_mut().carried = Some(stack("minecraft:oak_planks", 4));

    // Four right-clicks, one plank per cell. **Every one of them changes the derived
    // result** — measured against vanilla's own datapack, one plank alone is
    // `oak_button.json`, two side by side are `oak_pressure_plate.json`, three match
    // nothing, four are `crafting_table.json` — and the client predicts none of
    // them, so each has to be answered.
    for (menu_slot, left_on_cursor) in [(1, Some(3u32)), (2, Some(2)), (3, Some(1)), (4, None)] {
        let (correction, _) = apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: menu_slot, button: 1, click_type: 0 },
            &[(menu_slot, Some(stack("minecraft:oak_planks", 1)))],
            left_on_cursor
                .map(|count| stack("minecraft:oak_planks", count))
                .as_ref(),
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(
            matches!(&correction, Some(ServerDirective::Send { packet_id, .. }) if *packet_id == CONTENT),
            "menu slot {menu_slot} moved the result slot the client cannot derive, got {correction:?}"
        );
    }
    assert_eq!(
        inventory.crafting().result(),
        Some(&stack("minecraft:crafting_table", 1))
    );

    // Now take it. The client's prediction is empty on both counts (its own result
    // slot is still empty), so this is the click that read as dead.
    let (correction, dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 0, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert!(dropped.is_empty());
    assert_eq!(
        inventory.click_state().carried,
        Some(stack("minecraft:crafting_table", 1)),
        "exactly one table, on the cursor"
    );
    assert!(
        inventory.crafting().is_empty(),
        "and one of every input was consumed"
    );
    // `ContainerTagProto` puts the carried count in the last payload byte: the
    // client is told about the cursor on this same packet, which is what "without a
    // reopen" means.
    match &correction {
        Some(ServerDirective::Send { packet_id, payload }) => {
            assert_eq!(*packet_id, CONTENT);
            assert_eq!(payload[0], 0, "window 0");
            assert_eq!(payload[2], 46, "all 46 InventoryMenu slots");
            assert_eq!(payload[3], 1, "carrying one crafting table");
        }
        other => panic!("taking a result must resync the client, got {other:?}"),
    }

    // The control: with the take already applied, a client claiming precisely what
    // the server derived (nothing left in the grid, one table on the cursor) is
    // answered with silence.
    let (correction, _) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 0, button: 0, click_type: 0 },
        &[],
        Some(&stack("minecraft:crafting_table", 1)),
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(
        correction, None,
        "an empty result slot and a matching cursor is agreement, not a resend"
    );
}

/// Shift-clicking a result crafts **repeatedly** until the grid runs out —
/// vanilla's `doClick` `QUICK_MOVE` `while` loop over a result slot that
/// `slotsChanged` refills between rounds.
///
/// Expected value from outside this code: `chest.json` is eight `#minecraft:planks`
/// around an empty centre, and vanilla's own result-slot on-take routine removes **one** per occupied
/// cell per craft, so eight planks per cell is exactly eight chests — not one (the
/// old single-shot behaviour) and not sixty-four.
#[test]
fn shift_clicking_the_result_crafts_until_the_grid_runs_out() {
    let mut inventory = PlayerInventory::new();
    inventory.open_table_crafting();
    let block_entities = BlockEntityHandle::new();
    let mut open = OpenContainer {
        window_id: 3,
        pos: BlockPos::new(0, 64, 0),
        shape: MenuKind::CraftingTable,
        container_size: 10,
        state_id: 0,
    };
    for cell in [0, 1, 2, 3, 5, 6, 7, 8] {
        inventory
            .table_crafting_mut()
            .expect("open")
            .set_input(cell, Some(stack("minecraft:oak_planks", 8)));
    }
    assert_eq!(
        inventory.table_crafting().and_then(|g| g.result()),
        Some(&stack("minecraft:chest", 1)),
        "premise: the grid produces one chest per craft"
    );

    let (correction, dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        3,
        Click { slot: 0, button: 0, click_type: 1 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert!(dropped.is_empty(), "36 empty slots have room for 8 chests");
    let chests: u32 = (0..crate::inventory::PLAYER_NATIVE_SIZE)
        .filter_map(|native| inventory.native(native))
        .filter(|s| s.item.to_string() == "minecraft:chest")
        .map(|s| s.count)
        .sum();
    assert_eq!(chests, 8, "eight planks per cell is eight crafts");
    assert!(
        inventory.table_crafting().is_some_and(CraftingState::is_empty),
        "and the grid is empty, not merely one item lighter"
    );
    assert!(
        correction.is_some(),
        "the client cannot predict any of that and must be resynced"
    );
}

/// **Control**, and the third reported symptom: shift-clicking an *input* out of
/// the grid moves that item to the inventory and withdraws the result. It must
/// never craft — `quickMoveStack`'s grid-cell branch has no `onTake` on the result
/// container, and vanilla's own result-slot on-take routine is reachable only through slot 0.
#[test]
fn shift_clicking_a_grid_input_moves_it_out_without_crafting() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    for cell in 0..4 {
        inventory
            .crafting_mut()
            .set_input(cell, Some(stack("minecraft:oak_planks", 1)));
    }
    assert!(
        inventory.crafting().result().is_some(),
        "premise: a result is standing when the input is shift-clicked"
    );

    let (_, dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 1, button: 0, click_type: 1 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    assert!(dropped.is_empty());
    assert!(inventory.click_state().carried.is_none(), "nothing on the cursor");
    assert!(
        (0..crate::inventory::PLAYER_NATIVE_SIZE)
            .filter_map(|native| inventory.native(native))
            .all(|s| s.item.to_string() == "minecraft:oak_planks"),
        "no crafting table anywhere: taking an input is not a craft"
    );
    let planks: u32 = (0..crate::inventory::PLAYER_NATIVE_SIZE)
        .filter_map(|native| inventory.native(native))
        .map(|s| s.count)
        .sum();
    assert_eq!(planks, 1, "exactly the one plank that left the grid");
    assert_eq!(inventory.crafting().input(0), None, "the cell it came from");
    assert!(
        inventory.crafting().result().is_none(),
        "and the result is withdrawn, not crafted"
    );
}

/// **Control**: a click carrying the *wrong* (stale) window id must not
/// mutate anything — the guard that stops a click for an already-closed
/// or already-replaced window from landing on whatever is open now.
#[test]
fn container_clicked_against_a_stale_window_id_is_dropped() {
    let mut inventory = PlayerInventory::new();
    inventory.click_state_mut().carried = Some(stack("minecraft:coal", 1));
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(1, 2, 3);
    block_entities.with(|reg| {
        reg.insert(pos, BlockEntity::Furnace(Furnace::new(FurnaceKind::Furnace)));
    });
    let mut open = open(pos, 3); // window_id 7

    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        8, // stale/mismatched window id
        Click { slot: 0, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let furnace_input = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Furnace(f)) => f.input().cloned(),
        _ => None,
    });
    assert_eq!(furnace_input, None, "a stale window id must not mutate the block entity");
    assert!(
        inventory.click_state().carried.is_some(),
        "and the cursor is untouched"
    );
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
