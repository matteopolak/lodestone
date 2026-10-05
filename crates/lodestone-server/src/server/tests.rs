//! Unit tests for the server driver: the play loop's gating, the per-packet
//! handlers, and the helpers that live in the sibling modules.

use super::*;
use crate::composter::{Composter, MAX_FILL_LEVEL, READY_DELAY_TICKS};
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

struct NetherPacketAdmissionProtocol;

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

#[test]
fn attack_records_a_main_hand_swing_for_remote_connections() {
    let registry = PlayerRegistry::new();
    let mut cursor = registry.swing_cursor();

    record_attack_swing(Some(&registry), 42);

    assert_eq!(
        registry.swings_since(&mut cursor),
        vec![crate::players::SwingEvent {
            entity_id: 42,
            hand: lodestone_model::Hand::Main,
        }],
        "an attack must reach the remote animation broadcast as a main-hand swing"
    );
}

#[test]
fn attack_has_no_singleplayer_broadcast_sink() {
    let registry = PlayerRegistry::new();
    let mut cursor = registry.swing_cursor();

    record_attack_swing(None, 42);

    assert!(
        registry.swings_since(&mut cursor).is_empty(),
        "the singleplayer path must not manufacture a remote swing event"
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

#[test]
fn recipe_book_seen_accepts_only_advertised_display_ids() {
    let mut inventory = PlayerInventory::new();
    let valid = crate::crafting::recipe_book_entries()
        .first()
        .expect("the bundled recipe book has an entry")
        .id;
    assert!(inventory.recipe_book_entry_is_highlighted(valid));
    assert!(
        recipe_book_snapshot(&inventory)
            .into_iter()
            .find(|entry| entry.id == valid)
            .is_some_and(|entry| entry.highlight),
        "the join snapshot must expose an unacknowledged entry as highlighted"
    );

    let update = record_recipe_book_seen(&mut inventory, valid)
        .expect("an advertised display id must fold into a client update");
    assert!(
        !inventory.recipe_book_entry_is_highlighted(valid),
        "the validated packet must fold into the connection state"
    );
    assert!(
        !update.highlight,
        "the response must expose the cleared flag to the client read-model"
    );
    assert!(
        recipe_book_snapshot(&inventory)
            .into_iter()
            .find(|entry| entry.id == valid)
            .is_some_and(|entry| !entry.highlight),
        "the next snapshot must expose the acknowledgement to the client"
    );

    assert!(record_recipe_book_seen(&mut inventory, i32::MAX).is_none());
    assert!(
        inventory.recipe_book_entry_is_highlighted(i32::MAX),
        "an id absent from the advertised corpus must not manufacture seen state"
    );
}

struct RefusingChunkProtocol;

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

struct ColdColumnSource {
    column_reads: AtomicUsize,
    store_calls: AtomicUsize,
    resident: bool,
    center_only: bool,
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
struct RetainedLifecycleProtocol {
    computes: Arc<AtomicUsize>,
    retain_initial_light: bool,
    fallback_encodes: Arc<AtomicUsize>,
    dependency_light: Option<lodestone_world::ColumnLight>,
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

struct EndGatewaySource {
    state: StateId,
    generated: Option<(BlockPos, BlockEntity)>,
}

fn default_block_state(block: Block) -> StateId {
    block.default_state()
}

fn fixture_state(state: &str) -> StateId {
    StateId::from_state_str(state).expect("fixture state")
}

impl ChunkSource for EndGatewaySource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 256)
    }

    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
        self.state
    }

    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<BlockEntity> {
        self.generated.as_ref().and_then(|(pos, entity)| {
            (*pos == BlockPos::new(x, y, z)).then_some(entity.clone())
        })
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
}

#[test]
fn tick_relight_requires_a_resident_light_footprint_without_generating() {
    let cold = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: false,
        center_only: false,
    };
    assert!(resident_light_neighbourhood(&cold, 0, 0, 1).is_none());
    assert_eq!(
        cold.column_reads.load(Ordering::Relaxed),
        0,
        "a missing tick-light neighbour must defer to the future chunk snapshot, not generate"
    );

    let warm = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: true,
        center_only: false,
    };
    let (_, neighbours) = resident_light_neighbourhood(&warm, 0, 0, 1)
        .expect("a resident 3x3 footprint must be available for a live relight");
    assert_eq!(neighbours.len(), 8, "the cross-column footprint has all eight neighbours");
    assert_eq!(
        warm.column_reads.load(Ordering::Relaxed),
        0,
        "a complete resident footprint must also avoid the generating accessor"
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn detached_tick_light_is_invalidated_by_a_later_block_edit() {
    fn flat_light(
        centre: &ChunkColumn,
        neighbours: &[(i32, i32, &ChunkColumn)],
        _: crate::dimension::Dimension,
    ) -> lodestone_world::ColumnLight {
        assert_eq!(neighbours.len(), 8);
        let mut light = lodestone_world::ColumnLight::new(centre.section_count());
        *light.sky_mut(0) = lodestone_world::LightData::Uniform(7);
        light
    }

    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: true,
        center_only: false,
    };
    let store = crate::chunk_store::ChunkStore::with_capacity(source, 32);
    for dz in -1..=1 {
        for dx in -1..=1 {
            let _ = store.column(dx, dz);
        }
    }
    let light = compute_detached_relight(
        &store,
        (0, 0),
        crate::dimension::Dimension::Overworld,
        true,
        true,
        flat_light,
    )
    .expect("a resident footprint settles off the connection path");
    assert_eq!(
        store.resident_column(0, 0).unwrap().centre_settled_light(),
        Some(&light)
    );

    store.set_block(0, 0, 0, lodestone_data::block::Block::Stone.default_state());
    assert_ne!(
        store.resident_column(0, 0).unwrap().centre_settled_light(),
        Some(&light),
        "a completed worker result must not be sent after a newer edit"
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn direct_relight_can_complete_a_cold_neighbourhood_off_thread() {
    fn flat_light(
        centre: &ChunkColumn,
        neighbours: &[(i32, i32, &ChunkColumn)],
        _: crate::dimension::Dimension,
    ) -> lodestone_world::ColumnLight {
        assert_eq!(neighbours.len(), 8);
        lodestone_world::ColumnLight::new(centre.section_count())
    }

    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: false,
        center_only: false,
    };
    let store = crate::chunk_store::ChunkStore::with_capacity(source, 32);
    let _ = store.column(0, 0);
    assert!(store.resident_column(1, 0).is_none());
    assert!(compute_detached_relight(
        &store,
        (0, 0),
        crate::dimension::Dimension::Overworld,
        true,
        true,
        flat_light,
    )
    .is_none());
    assert!(store.resident_column(1, 0).is_none());
    assert!(compute_detached_relight(
        &store,
        (0, 0),
        crate::dimension::Dimension::Overworld,
        true,
        false,
        flat_light,
    )
    .is_some());
    assert!(store.resident_column(1, 0).is_some());
}

#[tokio::test]
async fn retained_tick_relight_defers_missing_footprint_but_keeps_no_light_fallback() {
    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: true,
        center_only: true,
    };
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;

    send_resident_column_light(&mut conn, &protocol, &source, &mut state, 0, 0)
        .await
        .expect("a missing resident footprint defers the tick relight");
    assert_eq!(
        protocol.fallback_encodes.load(Ordering::Acquire),
        0,
        "missing resident neighbours must not fall back to an isolated full-column packet"
    );
    assert_eq!(
        source.column_reads.load(Ordering::Acquire),
        0,
        "the resident-only path must not generate a missing neighbour"
    );
    assert_eq!(
        source.store_calls.load(Ordering::Acquire),
        0,
        "a deferred relight must not persist a partial footprint"
    );
    drop(conn);
    drop(client_end);

    // A complete footprint with a protocol that genuinely has no light
    // result remains on the existing compatible full-column fallback.
    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: true,
        center_only: false,
    };
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };
    let (_client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    send_resident_column_light(&mut conn, &protocol, &source, &mut state, 0, 0)
        .await
        .expect("a genuine protocol no-light result keeps the fallback");
    assert_eq!(
        protocol.fallback_encodes.load(Ordering::Acquire),
        1,
        "a genuine no-light result must remain distinguishable from a missing footprint"
    );
}

#[tokio::test]
async fn tick_block_updates_only_target_columns_already_delivered() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    let mut pending_relights = PendingRelights::default();

    send_tick_block_updates(
        &mut conn,
        &RefusingChunkProtocol,
        &mut state,
        &HashSet::from([(0, 0)]),
        &mut pending_relights,
        vec![
            crate::tick::TickBlockChange {
                x: 1,
                y: 64,
                z: 1,
                state: default_block_state(Block::GrassBlock),
                needs_relight: true,
            },
            crate::tick::TickBlockChange {
                x: 17,
                y: 64,
                z: 1,
                state: default_block_state(Block::Dirt),
                needs_relight: false,
            },
        ],
    )
    .await
    .expect("tick updates write without a complete join stream");

    let mut peer = Connection::new(client_end);
    assert_eq!(
        peer.read_packet().await.expect("first tick update frame decodes"),
        Some((43, vec![1, 64, 1]))
    );
    assert_eq!(pending_relights.pop_front(), Some((0, 0)));
    assert!(
        tokio::time::timeout(Duration::from_millis(1), peer.read_packet())
            .await
            .is_err(),
        "the pending column's later snapshot supersedes its tick update"
    );
}

#[tokio::test]
async fn tick_block_updates_without_light_changes_skip_relight() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    let mut pending_relights = PendingRelights::default();

    send_tick_block_updates(
        &mut conn,
        &RefusingChunkProtocol,
        &mut state,
        &HashSet::from([(0, 0)]),
        &mut pending_relights,
        vec![crate::tick::TickBlockChange {
            x: 1,
            y: 64,
            z: 1,
            state: default_block_state(Block::GrassBlock),
            needs_relight: false,
        }],
    )
    .await
    .expect("block updates are independent of light changes");

    let mut peer = Connection::new(client_end);
    assert_eq!(
        peer.read_packet().await.expect("block update decodes"),
        Some((43, vec![1, 64, 1]))
    );
    assert!(pending_relights.is_empty());
}

#[tokio::test]
async fn tick_block_update_burst_yields_after_one_ordered_batch() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut peer = Connection::new(client_end);
    let mut state = State::Play;
    let mut pending_relights = PendingRelights::default();
    let mut pending = VecDeque::new();
    let mut delivered = HashSet::from([(0, 0)]);
    let changes = (0..65)
        .map(|y| crate::tick::TickBlockChange {
            x: 1,
            y,
            z: 1,
            state: default_block_state(Block::Stone),
            needs_relight: false,
        })
        .chain(std::iter::once(crate::tick::TickBlockChange {
            x: 17,
            y: 0,
            z: 1,
            state: default_block_state(Block::Stone),
            needs_relight: false,
        }))
        .collect();
    queue_tick_block_updates(&mut pending, &delivered, changes);
    assert_eq!(pending.len(), 65);
    delivered.insert((1, 0));

    send_pending_tick_block_updates(
        &mut conn,
        &RefusingChunkProtocol,
        &mut state,
        &delivered,
        &mut pending_relights,
        &mut pending,
    )
    .await
    .expect("first bounded update batch");
    assert_eq!(pending.len(), 1);
    for y in 0..64 {
        assert_eq!(
            peer.read_packet().await.expect("ordered block update"),
            Some((43, vec![1, y, 1]))
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(1), peer.read_packet())
            .await
            .is_err(),
        "the next update waits for a separate connection pass"
    );

    send_pending_tick_block_updates(
        &mut conn,
        &RefusingChunkProtocol,
        &mut state,
        &delivered,
        &mut pending_relights,
        &mut pending,
    )
    .await
    .expect("second bounded update batch");
    assert_eq!(pending.len(), 0);
    assert_eq!(
        peer.read_packet().await.expect("final block update"),
        Some((43, vec![1, 64, 1]))
    );
    assert!(pending_relights.is_empty());
}

#[test]
fn tick_relight_targets_keep_the_delivered_cross_column_footprint() {
    let delivered = HashSet::from([
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (0, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
        (2, 0),
    ]);
    assert_eq!(
        tick_relight_targets([(0, 0)], &delivered, 1),
        delivered
            .iter()
            .copied()
            .filter(|&(cx, cz)| {
                (-1..=1).contains(&cx) && (-1..=1).contains(&cz)
            })
            .collect(),
        "a seam or corner edit must queue every delivered column in the 3x3 footprint"
    );
    assert_eq!(
        tick_relight_targets([(0, 0)], &delivered, 0),
        HashSet::from([(0, 0)]),
        "single-column protocols retain the isolated relight footprint"
    );
}

#[test]
fn pending_relights_deduplicate_batches_and_keep_fifo_fairness() {
    let mut pending = PendingRelights::default();
    assert_eq!(
        pending.enqueue_batch([(2, 0), (0, 0), (2, 0)]),
        2,
        "each batch is canonicalized and duplicate targets collapse"
    );
    assert_eq!(pending.enqueue_batch([(1, 0), (0, 0)]), 1);
    assert_eq!(pending.pop_front(), Some((0, 0)));
    assert_eq!(pending.enqueue_batch([(0, 0)]), 1);
    assert_eq!(pending.pop_front(), Some((2, 0)));
    assert_eq!(pending.pop_front(), Some((1, 0)));
    assert_eq!(pending.pop_front(), Some((0, 0)));
    assert!(pending.is_empty());
}

#[test]
fn deferred_relight_batch_rotates_and_retries_individual_outputs() {
    let delivered = HashSet::from([(-1, -1), (0, 0)]);
    let mut pending = PendingRelights::default();
    pending.enqueue_batch([(-1, -1), (0, 0)]);
    let batch = pending.batch(&delivered, true);
    assert_eq!(batch.coordinates, [(-1, -1), (0, 0)]);
    pending.admit(&batch);
    pending.defer(&batch);
    assert!(!pending.ready());
    assert_eq!(pending.front(), Some((0, 0)));
    let retry = pending.batch(&delivered, true);
    assert_eq!(retry.coordinates, [(0, 0)]);
    pending.admit(&retry);
    assert_eq!(pending.front(), Some((-1, -1)));
}

#[test]
fn relight_admission_preserves_new_edits_and_only_requeues_owed_permissions() {
    let delivered = HashSet::from([(0, 0), (1, 0)]);
    let mut pending = PendingRelights::default();
    pending.enqueue_edit(0, 0, 0);
    pending.enqueue_batch([(1, 0)]);
    let batch = pending.batch(&delivered, true);
    pending.admit(&batch);
    pending.enqueue_edit(0, 0, 0);
    pending.defer(&RelightBatch {
        coordinates: vec![(1, 0)],
        generation_required: batch.generation_required,
    });
    assert_eq!(pending.pop_front(), Some((0, 0)));
    assert!(!pending.requires_generation((0, 0)));
    assert_eq!(pending.pop_front(), Some((1, 0)));
    assert!(pending.generation_required.is_empty());
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn direct_block_edit_queues_a_generating_relight_without_reading_columns() {
    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: false,
        center_only: false,
    };
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };
    let (_client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    let mut pending = PendingRelights::default();
    resend_column_for_light(
        &mut conn,
        &protocol,
        &source,
        &mut state,
        Some(&mut pending),
        Block::Air.default_state(),
        Block::Air.default_state(),
        BlockPos::new(15, 64, 0),
    )
    .await
    .unwrap();
    assert!(pending.is_empty(), "a light-neutral edit must not queue a relight");

    resend_column_for_light(
        &mut conn,
        &protocol,
        &source,
        &mut state,
        Some(&mut pending),
        Block::Stone.default_state(),
        Block::Air.default_state(),
        BlockPos::new(15, 64, 0),
    )
    .await
    .unwrap();
    assert_eq!(pending.len(), 9);
    assert_eq!(source.column_reads.load(Ordering::Relaxed), 0);
    assert!(pending.requires_generation((0, 0)));
    assert!(pending.requires_generation((1, 1)));
    assert_eq!(pending.pop_front(), Some((-1, -1)));
    assert!(!pending.requires_generation((-1, -1)));
}

#[test]
fn gateway_contact_reads_generated_metadata_and_prefers_live_registry() {
    let gateway = BlockPos::new(12, 70, -4);
    let source = EndGatewaySource {
        state: default_block_state(crate::portal::END_GATEWAY_BLOCK),
        generated: Some((
            gateway,
            BlockEntity::EndGateway {
                exit: Some(BlockPos::new(100, 50, 0)),
                exact: true,
            },
        )),
    };
    let registry = BlockEntityHandle::new();

    assert_eq!(
        end_gateway_destination(&source, &registry, gateway),
        Some(Vec3::new(100.5, 50.0, 0.5)),
        "a generated gateway must consume its exact exit metadata"
    );

    registry.with(|entries| {
        entries.insert(
            gateway,
            BlockEntity::EndGateway {
                exit: Some(BlockPos::new(-20, 80, 30)),
                exact: true,
            },
        );
    });
    assert_eq!(
        end_gateway_destination(&source, &registry, gateway),
        Some(Vec3::new(-19.5, 80.0, 30.5)),
        "a live edited sidecar must outrank the generated snapshot"
    );

    let removed = EndGatewaySource {
        state: StateId::AIR,
        generated: source.generated.clone(),
    };
    assert_eq!(
        end_gateway_destination(&removed, &registry, gateway),
        None,
        "a stale sidecar must not teleport through a removed gateway block"
    );

    let missing_metadata = EndGatewaySource {
        state: default_block_state(crate::portal::END_GATEWAY_BLOCK),
        generated: None,
    };
    assert_eq!(
        end_gateway_destination(&missing_metadata, &BlockEntityHandle::new(), gateway),
        None,
        "a gateway without an exit must leave the player in place"
    );
}

#[test]
fn gateway_contact_is_end_player_only() {
    assert!(end_gateway_contact_allowed(
        crate::dimension::Dimension::End,
        true
    ));
    assert!(!end_gateway_contact_allowed(
        crate::dimension::Dimension::Overworld,
        true
    ));
    assert!(!end_gateway_contact_allowed(
        crate::dimension::Dimension::End,
        false
    ));
}

#[test]
fn gateway_contact_production_decision_updates_position_and_cooldown() {
    let gateway = BlockPos::new(12, 70, -4);
    let source = EndGatewaySource {
        state: default_block_state(crate::portal::END_GATEWAY_BLOCK),
        generated: Some((
            gateway,
            BlockEntity::EndGateway {
                exit: Some(BlockPos::new(100, 50, 0)),
                exact: true,
            },
        )),
    };
    let registry = BlockEntityHandle::new();

    let teleport = resolve_end_gateway_contact(
        &source,
        &registry,
        gateway,
        crate::dimension::Dimension::End,
        0,
        true,
        false,
    )
    .expect("the production contact seam must produce a visible teleport");
    assert_eq!(teleport.position, Vec3::new(100.5, 50.0, 0.5));
    assert_eq!(teleport.dimension, crate::dimension::Dimension::End);
    assert_eq!(teleport.cooldown, END_GATEWAY_CONTACT_COOLDOWN);

    assert!(resolve_end_gateway_contact(
        &source,
        &registry,
        gateway,
        crate::dimension::Dimension::End,
        END_GATEWAY_CONTACT_COOLDOWN,
        true,
        false,
    )
    .is_none());
    assert!(resolve_end_gateway_contact(
        &EndGatewaySource {
            state: default_block_state(crate::portal::END_GATEWAY_BLOCK),
            generated: None,
        },
        &registry,
        gateway,
        crate::dimension::Dimension::End,
        0,
        true,
        false,
    )
    .is_none());
    assert!(resolve_end_gateway_contact(
        &source,
        &registry,
        gateway,
        crate::dimension::Dimension::End,
        0,
        false,
        true,
    )
    .is_none());
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
fn item_key(path: &str) -> lodestone_model::ResourceKey {
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

#[test]
fn selected_placement_item_validates_the_held_stack_once() {
    let mut inventory = PlayerInventory::new();
    inventory.set_native(0, Some(stack("minecraft:redstone", 1)));

    assert_eq!(
        selected_placement_item(&inventory, 0),
        Some(Item::Redstone),
        "a built-in held item must enter placement as its registry type"
    );

    inventory.set_native(
        0,
        Some(ItemStack::new(
            ResourceKey::new("example", "custom_block").expect("valid custom key"),
            1,
        )),
    );
    assert_eq!(
        selected_placement_item(&inventory, 0),
        None,
        "an item outside the built-in registry cannot enter the typed placement path"
    );
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


// -- the composter interaction  --

/// A composter at `pos`, and a player inventory whose selected hotbar slot
/// (0) holds `held`. `MobHandle::default()` is an empty sim, so the first
/// `spawn_item` in a test is entity id 1 (its `next_id` starts at 1 — see
/// `MobSim::new`).
fn composter_scene(
    composter: Composter,
    held: Option<ItemStack>,
) -> (BlockEntityHandle, PlayerInventory, BlockPos, MobHandle) {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(4, 64, 4);
    block_entities.with(|reg| reg.insert(pos, BlockEntity::Composter(composter)));
    let mut inventory = PlayerInventory::new();
    inventory.set_native(0, held);
    (block_entities, inventory, pos, MobHandle::default())
}

/// The composter's fill level, read back through the registry.
fn composter_level(block_entities: &BlockEntityHandle, pos: BlockPos) -> u8 {
    block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Composter(composter)) => composter.level(),
        _ => u8::MAX,
    })
}

/// A right-click with a compostable item consumes one from the hand and
/// raises the fill level — the wiring that makes `Composter::insert`
/// reachable at all.
#[test]
fn right_click_consumes_one_compostable_and_raises_the_level() {
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(Composter::new(), Some(stack("minecraft:oak_leaves", 3)));

    // oak_leaves chance is 0.3; roll 0.0 always beats it (and level 0
    // always advances regardless of roll — the documented special case).
    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Consumed {
            remainder: Some(stack("minecraft:oak_leaves", 2)),
            block_state: Some(fixture_state("minecraft:composter[level=1]")),
        }
    );
    assert_eq!(composter_level(&block_entities, pos), 1);
}

/// A single compostable item in hand is fully consumed, emptying the slot.
#[test]
fn right_click_fully_consumes_a_single_item() {
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(Composter::new(), Some(stack("minecraft:wheat", 1)));

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Consumed {
            remainder: None,
            block_state: Some(fixture_state("minecraft:composter[level=1]")),
        }
    );
    assert_eq!(inventory.native(0), None, "the selected slot is empty after the click");
}

/// **Control**: a failed roll still consumes the item (vanilla consumes on
/// every accepted insert, per its own composter fill routine) but leaves the level —
/// and therefore the block state — unchanged.
#[test]
fn a_failed_roll_still_consumes_the_item_but_keeps_the_state() {
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(Composter::restore(1, None), Some(stack("minecraft:oak_leaves", 2)));

    // oak_leaves chance is 0.3; a roll of 0.9 fails away from level 0.
    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.9);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Consumed {
            remainder: Some(stack("minecraft:oak_leaves", 1)),
            block_state: None,
        }
    );
    assert_eq!(composter_level(&block_entities, pos), 1);
}

/// A non-compostable held item falls through without consuming anything or
/// touching the composter — the caller's cue to try ordinary placement
/// so the ordinary placement path can handle it.
#[test]
fn a_non_compostable_item_falls_through_without_touching_anything() {
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(Composter::new(), Some(stack("minecraft:diamond", 1)));

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(outcome, ComposterUseOutcome::NotComposter);
    assert_eq!(inventory.native(0), Some(&stack("minecraft:diamond", 1)));
    assert_eq!(composter_level(&block_entities, pos), 0);
}

/// An empty hand on a composter below level 8 returns `PASS`, so the
/// placement logic may place a block on top of the partially filled
/// composter.
#[test]
fn an_empty_hand_on_a_not_ready_composter_falls_through() {
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(Composter::restore(3, None), None);

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(outcome, ComposterUseOutcome::NotComposter);
    assert_eq!(composter_level(&block_entities, pos), 3);
}

/// A full (level 7, waiting on its scheduled tick) composter consumes the
/// click without touching the hand at `fillLevel == 7` with nothing to add.
#[test]
fn level_seven_consumes_the_click_without_touching_the_hand() {
    let mut composter = Composter::new();
    for _ in 0..MAX_FILL_LEVEL {
        assert!(matches!(
            composter.insert("minecraft:cake", 0.0),
            InsertOutcome::Consumed {
                level_increased: true
            }
        ));
    }
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(composter, Some(stack("minecraft:cake", 2)));

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(outcome, ComposterUseOutcome::Noop);
    assert_eq!(
        inventory.native(0),
        Some(&stack("minecraft:cake", 2)),
        "the hand must be untouched"
    );
    assert_eq!(composter_level(&block_entities, pos), MAX_FILL_LEVEL);
}

/// A ready composter (level 8) with an empty hand yields one bone-meal item
/// entity just above the block and resets to level 0 — the extraction half
/// of the interaction (`extractProduce`).
#[test]
fn extracting_a_ready_composter_spawns_bone_meal_and_resets() {
    let mut composter = Composter::new();
    for _ in 0..MAX_FILL_LEVEL {
        composter.insert("minecraft:cake", 0.0);
    }
    for _ in 0..READY_DELAY_TICKS {
        composter.tick();
    }
    assert!(composter.is_ready());
    let (block_entities, mut inventory, pos, mobs) = composter_scene(composter, None);
    let bone_meal_id = mobs.with(|sim| sim.next_id());

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Extracted {
            block_state: fixture_state("minecraft:composter[level=0]"),
        }
    );
    assert_eq!(composter_level(&block_entities, pos), 0);
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "exactly one bone-meal item entity must spawn"
    );
    // The spawn takes the sim's next id, and it must land at the block's
    // centre with the measured `1.01`-block vertical offset.
    assert_eq!(
        mobs.with(|sim| sim.item_position(bone_meal_id)),
        Some(Vec3::new(4.5, 65.01, 4.5)),
        "the bone meal must spawn just above the composter"
    );
}

/// **Control**: extraction reaches the player even with a compostable item
/// in hand — the item offer fails below level 8 (returns `NotAccepting`)
/// and the hand-use half extracts without consuming the hand.
#[test]
fn extracting_a_ready_composter_works_even_with_an_item_in_hand() {
    let mut composter = Composter::new();
    for _ in 0..MAX_FILL_LEVEL {
        composter.insert("minecraft:cake", 0.0);
    }
    for _ in 0..READY_DELAY_TICKS {
        composter.tick();
    }
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(composter, Some(stack("minecraft:cake", 2)));

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Extracted {
            block_state: fixture_state("minecraft:composter[level=0]"),
        }
    );
    assert_eq!(
        inventory.native(0),
        Some(&stack("minecraft:cake", 2)),
        "extraction must not consume the hand"
    );
    assert_eq!(mobs.with(|sim| sim.item_count()), 1);
}

/// **Control**: a non-compostable item on a *ready* composter also extracts
/// — the item offer fails the compostability check and the hand-use half
/// runs without consuming the hand.
#[test]
fn extracting_a_ready_composter_works_for_a_non_compostable_item_too() {
    let mut composter = Composter::new();
    for _ in 0..MAX_FILL_LEVEL {
        composter.insert("minecraft:cake", 0.0);
    }
    for _ in 0..READY_DELAY_TICKS {
        composter.tick();
    }
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(composter, Some(stack("minecraft:diamond", 1)));

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Extracted {
            block_state: fixture_state("minecraft:composter[level=0]"),
        }
    );
    assert_eq!(
        inventory.native(0),
        Some(&stack("minecraft:diamond", 1)),
        "the non-compostable item must stay in hand"
    );
    assert_eq!(mobs.with(|sim| sim.item_count()), 1);
}

/// A position holding no composter is not a composter interaction at all,
/// regardless of the held item.
#[test]
fn a_position_without_a_composter_is_not_a_composter_interaction() {
    let block_entities = BlockEntityHandle::new();
    let mut inventory = PlayerInventory::new();
    inventory.set_native(0, Some(stack("minecraft:oak_leaves", 1)));

    let outcome = apply_composter_use(
        &block_entities,
        &mut inventory,
        &MobHandle::default(),
        BlockPos::new(9, 9, 9),
        0.0,
    );

    assert_eq!(outcome, ComposterUseOutcome::NotComposter);
    assert_eq!(inventory.native(0), Some(&stack("minecraft:oak_leaves", 1)));
}

/// [`join_view_rings`]'s shape, at the three inputs that matter: the shell's
/// own radius, the degenerate 0, and a negative one.
///
/// Ring sizes are `1, 8, 16, …, 8r` and must sum to `(2r+1)²` with no
/// coordinate repeated — a ring walk that double-counted a corner or skipped
/// an edge would still be non-decreasing in distance, so the end-to-end gate
/// in `tests/serve_play.rs` checks set equality and this checks the counts.
#[test]
fn join_view_rings_partitions_the_square_exactly() {
    let rings = join_view_rings(9);
    assert_eq!(rings.len(), 10, "radius 9 has rings 0..=9");
    assert_eq!(rings[0], vec![(0, 0)], "ring 0 is the player's own column");
    for (r, ring) in rings.iter().enumerate() {
        let expected = if r == 0 { 1 } else { 8 * r };
        assert_eq!(ring.len(), expected, "ring {r} must hold {expected} columns");
        for &(dx, dz) in ring {
            assert_eq!(
                dx.abs().max(dz.abs()) as usize,
                r,
                "({dx}, {dz}) is not on ring {r}"
            );
        }
    }
    let flat: Vec<(i32, i32)> = rings.iter().flatten().copied().collect();
    let unique: HashSet<(i32, i32)> = flat.iter().copied().collect();
    assert_eq!(flat.len(), 361, "the rings must sum to (2*9+1)^2");
    assert_eq!(unique.len(), flat.len(), "no column may appear on two rings");
}

/// Radius 0 is one ring holding one column — the configuration several tests
/// in this crate join with.
#[test]
fn join_view_rings_at_radius_zero_is_a_single_column() {
    assert_eq!(join_view_rings(0), vec![vec![(0, 0)]]);
}

#[test]
fn delivered_columns_survive_only_while_they_remain_in_view() {
    let mut view = ViewTracker::new((0, 0), 1, 1);
    view.mark_delivered((0, 0));
    view.mark_delivered((1, 0));
    view.mark_delivered((9, 9));
    assert_eq!(view.delivered, HashSet::from([(0, 0), (1, 0)]));

    let _ = view.recenter(&RefusingChunkProtocol, 2, 0, None);
    assert_eq!(view.delivered, HashSet::from([(1, 0)]));
}

#[test]
fn served_full_stage_avoids_a_full_band_upgrade_with_shaped_control() {
    let coordinate = (2, 1);
    for (stage, expected_upgrades) in [
        (ChunkGenerationStage::Full, Vec::new()),
        (ChunkGenerationStage::Shaped, vec![coordinate]),
    ] {
        let mut view = ViewTracker::new_banded((0, 0), 3, 3, 0);
        let incarnation = view.incarnation(coordinate).unwrap();
        view.record_delivery(coordinate, incarnation, Some(stage));
        assert_eq!(view.columns[&coordinate].requested, ChunkGenerationStage::Shaped);
        assert_eq!(view.recenter(&RefusingChunkProtocol, 2, 1, None).upgrades, expected_upgrades);
    }
}

#[test]
fn shaped_receipt_keeps_a_higher_request_and_full_receipts_do_not_regress() {
    let coordinate = (2, 1);
    let mut view = ViewTracker::new_banded((0, 0), 3, 3, 0);
    let incarnation = view.incarnation(coordinate).unwrap();
    assert_eq!(view.recenter(&RefusingChunkProtocol, 2, 1, None).upgrades, vec![coordinate]);
    view.record_delivery(coordinate, incarnation, Some(ChunkGenerationStage::Shaped));
    assert_eq!(view.columns[&coordinate].requested, ChunkGenerationStage::Full);
    assert_eq!(view.columns[&coordinate].served, Some(ChunkGenerationStage::Shaped));
    assert_eq!(view.recenter(&RefusingChunkProtocol, 2, 2, None).upgrades, vec![(2, 2)]);
    assert!(view.recenter(&RefusingChunkProtocol, 2, 1, None).upgrades.is_empty());
    view.record_delivery(coordinate, incarnation, Some(ChunkGenerationStage::Full));
    view.record_delivery(coordinate, incarnation, Some(ChunkGenerationStage::Shaped));
    assert_eq!(view.columns[&coordinate].served, Some(ChunkGenerationStage::Full));
    assert!(!view.needs_delivery(coordinate, incarnation, Some(ChunkGenerationStage::Full)));
    assert!(!view.needs_delivery(coordinate, incarnation, Some(ChunkGenerationStage::Shaped)));
    assert!(view.needs_delivery(coordinate, incarnation, None));
}

#[test]
fn forgotten_and_reset_columns_reject_old_delivery_receipts() {
    let coordinate = (0, 0);
    let mut view = ViewTracker::new(coordinate, 0, 0);
    let old = view.incarnation(coordinate).unwrap();
    view.recenter(&RefusingChunkProtocol, 1, 0, None);
    view.recenter(&RefusingChunkProtocol, 0, 0, None);
    let reentered = view.incarnation(coordinate).unwrap();
    assert!(reentered.0 > old.0);
    view.record_delivery(coordinate, old, Some(ChunkGenerationStage::Full));
    assert!(view.delivered.is_empty());
    assert_eq!(view.columns[&coordinate].served, None);
    view.record_delivery(coordinate, reentered, Some(ChunkGenerationStage::Full));
    assert_eq!(view.delivered, HashSet::from([coordinate]));
    view.reset(coordinate);
    assert!(view.incarnation(coordinate).unwrap().0 > reentered.0);
    view.record_delivery(coordinate, reentered, Some(ChunkGenerationStage::Full));
    assert!(view.delivered.is_empty());
    assert_eq!(view.columns[&coordinate].served, None);
}

fn receipt_packet(payload: u8, stage: Option<ChunkGenerationStage>) -> EncodedColumn {
    EncodedColumn {
        directive: ServerDirective::Send { packet_id: 52, payload: vec![payload] },
        stage,
    }
}

struct ReceiptProtocol;

impl ServerProtocol for ReceiptProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        NetherPacketAdmissionProtocol.decode(state, packet_id, payload)
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
        receipt_packet(u8::from(column.generation_stage() == ChunkGenerationStage::Full), None)
            .directive
    }
    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
    fn retains_initial_column_light(&self) -> bool {
        true
    }
}

struct ReceiptSource { full: bool }

impl ChunkSource for ReceiptSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        unreachable!("receipt fixture requires no generation")
    }
    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
        crate::chunk::air_state()
    }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_owned()
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
    fn settle_resident_column_lights_with_neighbours(
        &self,
        _cx: i32,
        _cz: i32,
        _fallback: &ChunkColumn,
        _neighbour_offsets: &[(i32, i32)],
        _resident_only: bool,
        _replace_existing: bool,
        _exclusive: bool,
        _compute: &mut dyn FnMut(
            &ChunkColumn, &[(i32, i32, &ChunkColumn)],
        ) -> Option<crate::chunk::ColumnLightSettlement>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        if self.full { Ok(ChunkColumn::new(0, 16)) }
        else { Err(ColumnLightSettlementError::NoLight) }
    }
}

#[tokio::test]
async fn served_stage_follows_selected_centre_and_no_light_fallback() {
    let shaped = ChunkColumn::new(0, 16)
        .test_with_generation_stage(ChunkGenerationStage::Shaped);
    for (full, stage, payload) in [
        (true, ChunkGenerationStage::Full, vec![1]),
        (false, ChunkGenerationStage::Shaped, vec![0]),
    ] {
        let direct = encode_chunk_with_source_receipt(
            &ReceiptProtocol, &ReceiptSource { full }, 2, 1, &shaped,
        ).unwrap();
        assert_eq!(direct.stage, Some(stage));
        assert_eq!(direct.directive, ServerDirective::Send { packet_id: 52, payload });
        let owned = encode_column_owned(
            &ReceiptProtocol, Arc::new(ReceiptSource { full }), 2, 1, None,
            crate::join_scheduler::ColumnPayload::Column(shaped.clone()),
        ).await.unwrap();
        assert_eq!(owned.stage, Some(stage));
        assert_eq!(owned.directive, direct.directive);
    }
}

#[tokio::test]
async fn failed_chunk_write_does_not_promote_the_served_stage() {
    let coordinate = (2, 1);
    let mut view = ViewTracker::new_banded((0, 0), 3, 3, 0);
    let incarnation = view.incarnation(coordinate).unwrap();
    let (peer, endpoint) = lodestone_net::memory_pair();
    drop(peer);
    let error = send_encoded_column(
        &mut Connection::new(endpoint), &mut State::Play, &mut view,
        coordinate, incarnation, receipt_packet(1, Some(ChunkGenerationStage::Full)),
    ).await.unwrap_err();
    assert!(matches!(error, ServerError::Net(_)));
    assert_eq!(view.columns[&coordinate].served, None);
    assert!(view.delivered.is_empty());
    assert_eq!(view.recenter(&RefusingChunkProtocol, 2, 1, None).upgrades, vec![coordinate]);

    let (peer, endpoint) = lodestone_net::memory_pair();
    let mut connection = Connection::new(endpoint);
    assert!(send_encoded_column(
        &mut connection, &mut State::Play, &mut view,
        coordinate, incarnation, receipt_packet(2, Some(ChunkGenerationStage::Full)),
    ).await.unwrap());
    assert_eq!(view.columns[&coordinate].served, Some(ChunkGenerationStage::Full));
    assert_eq!(Connection::new(peer).read_packet().await.unwrap(), Some((52, vec![2])));
}

#[tokio::test]
async fn paused_encode_and_queued_batch_cannot_deliver_to_a_reentered_column() {
    let coordinate = (0, 0);
    let mut view = ViewTracker::new(coordinate, 0, 0);
    let old = view.incarnation(coordinate).unwrap();
    let (release, wait) = tokio::sync::oneshot::channel();
    let mut encodes = PendingJoinEncodes::new();
    encodes.push(true, Box::pin(async move {
        wait.await.unwrap();
        Ok((coordinate, (old, receipt_packet(1, Some(ChunkGenerationStage::Full)))))
    }));
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(encodes.poll_next(&mut context).is_pending());
    view.recenter(&RefusingChunkProtocol, 1, 0, None);
    view.recenter(&RefusingChunkProtocol, 0, 0, None);
    let current = view.incarnation(coordinate).unwrap();
    release.send(()).unwrap();
    let (_, (incarnation, encoded)) = std::future::poll_fn(|context| encodes.poll_next(context))
        .await.unwrap();
    let (peer, endpoint) = lodestone_net::memory_pair();
    let mut connection = Connection::new(endpoint);
    assert!(!send_encoded_column(
        &mut connection, &mut State::Play, &mut view, coordinate, incarnation, encoded,
    ).await.unwrap());
    send_pending_chunk_batch(
        &mut connection, &RefusingChunkProtocol, &mut State::Play, &mut view,
        PendingChunkBatch { columns: vec![
            (coordinate, old, receipt_packet(2, Some(ChunkGenerationStage::Full))),
            (coordinate, current, receipt_packet(3, Some(ChunkGenerationStage::Full))),
        ] },
    ).await.unwrap();
    assert_eq!(view.columns[&coordinate].served, Some(ChunkGenerationStage::Full));
    drop(connection);
    let mut peer = Connection::new(peer);
    assert_eq!(peer.read_packet().await.unwrap(), Some((40, Vec::new())));
    assert_eq!(peer.read_packet().await.unwrap(), Some((52, vec![3])));
    assert_eq!(peer.read_packet().await.unwrap(), Some((41, vec![1])));
    assert_eq!(peer.read_packet().await.unwrap(), None);
}

/// **The cross-arm invariant the off-centre join violated**, at a centre where
/// the two hypotheses actually differ.
///
/// Two independent constructions of one square: `join_view_rings` walks
/// Chebyshev rings and yields offsets, `ViewTracker::new` rasters a
/// `[-r, r]²` window around an absolute centre. The tracker's set is a *claim
/// about what the wire sent*, so if the two disagree the tracker suppresses
/// resends of columns the client never received. The expectation therefore
/// comes from neither implementation — it is the geometry both are supposed to
/// be describing.
///
/// `(25, -13)` deliberately: at a centre of `(0, 0)` the offset and absolute
/// readings coincide exactly, which is why every existing join gate — all of
/// which spawn at a position flooring to chunk `(0, 0)` — passed throughout.
#[test]
fn ring_offsets_plus_the_join_centre_are_the_square_the_view_tracker_seeds() {
    let radius = 9;
    let (cx, cz) = (25, -13);

    let emitted: HashSet<(i32, i32)> = join_view_rings(radius)
        .into_iter()
        .flatten()
        .map(|(dx, dz)| (cx + dx, cz + dz))
        .collect();
    let seeded = ViewTracker::new((cx, cz), radius, radius).loaded;

    assert_eq!(emitted.len(), 361, "radius 9 is 361 columns either way");
    assert_eq!(
        emitted, seeded,
        "the columns the join stream emits must be exactly the ones the tracker \
         records as sent; any difference is a column the client never gets and \
         never gets resent"
    );

    // The control, and it must fail the same assertion: the pre-fix code used
    // the raw offsets as absolute coordinates. Run and observed failing here
    // rather than described, because the *reason* this bug survived is that
    // the difference is invisible at the origin.
    let unshifted: HashSet<(i32, i32)> =
        join_view_rings(radius).into_iter().flatten().collect();
    assert_ne!(
        unshifted, seeded,
        "control failed: raw ring offsets must NOT equal the tracker's square at a \
         non-origin centre — if they do, this test cannot see the defect it exists for"
    );
    // And the reason the control has to be at a non-origin centre at all.
    assert_eq!(
        unshifted,
        ViewTracker::new((0, 0), radius, radius).loaded,
        "at the origin the two readings are identical, which is exactly why every \
         gate that spawns at chunk (0, 0) was blind to this"
    );
}

/// **A negative radius must yield no rings at all**, matching the raster walk
/// this replaced: `(-r..=r)` is an empty range for `r < 0`, so a negative
/// radius sent zero chunks. `view_radius.max(0)` would send one, and
/// `ViewTracker::new` would still record an empty loaded set for the same
/// input — the tracker and the wire disagreeing about a column the client
/// actually has. Nothing produces a negative radius today, which is why this
/// needs a test rather than a reading.
#[test]
fn join_view_rings_at_a_negative_radius_is_empty() {
    assert!(join_view_rings(-1).is_empty());
    assert!(join_view_rings(i32::MIN).is_empty());
}

/// The yaw → horizontal-facing map is vanilla's own yaw-to-direction conversion
/// (vanilla's own per-variant direction field table): yaw 0 = south, 90 = west, ±180 = north,
/// -90 = east, split at the 45° midpoints (the value at which
/// `floor(yaw / 90 + 0.5) & 3` rolls over). This is the facing a placed
/// diode then inverts so the block faces the player.
#[test]
fn horizontal_look_direction_matches_vanilla_from_y_rot() {
    assert_eq!(horizontal_look_direction(0.0), Direction::South);
    assert_eq!(horizontal_look_direction(90.0), Direction::West);
    assert_eq!(horizontal_look_direction(180.0), Direction::North);
    assert_eq!(horizontal_look_direction(-90.0), Direction::East);
    // The 45°/135°/225°/315° midpoints land exactly as the bit-mask's
    // `floor` does.
    assert_eq!(horizontal_look_direction(44.0), Direction::South);
    assert_eq!(horizontal_look_direction(45.0), Direction::West);
    assert_eq!(horizontal_look_direction(135.0), Direction::North);
    assert_eq!(horizontal_look_direction(225.0), Direction::East);
    assert_eq!(horizontal_look_direction(315.0), Direction::South);
    assert_eq!(horizontal_look_direction(-45.0), Direction::South);
    assert_eq!(horizontal_look_direction(-135.0), Direction::East);
    assert_eq!(horizontal_look_direction(-225.0), Direction::North);
    assert_eq!(horizontal_look_direction(-315.0), Direction::West);
    // Wraps around rather than clamping at ±180.
    assert_eq!(horizontal_look_direction(450.0), Direction::West);
    assert_eq!(horizontal_look_direction(-450.0), Direction::East);
}

// `gamemode_command_parses_names_aliases_and_ids` was here, testing
// `parse_gamemode_command` — a hand-rolled string split that has been
// deleted. `/gamemode` is now a real Brigadier command in
// `crate::commands::gamemode`, gated against the captured vanilla tree
// (`crates/protocol/v770/tests/builtin_command_parity.rs`) and driven
// end-to-end by `tests/builtin_commands.rs`.
//
// Worth recording rather than silently dropping: the deleted test asserted
// that `gamemode c` and `gamemode 1` parse as creative. **26.2 accepts
// neither.** vanilla's own game-type name lookup is an exact match against the four
// `getSerializedName` values, so the old parser — and the test that pinned
// it — were *more* permissive than vanilla. No test could have caught that,
// because the failure only ever made a command work that should have failed.

/// The three redstone families keep the full property set the signal model
/// reads, and everything else falls through to `crate::block_placement`
/// (whose own tests cover the per-block conventions). The observer is
/// deliberately **not** inverted: it watches in the player's look direction,
/// unlike the diodes' single inversion, which makes them face the player.
#[test]
fn placed_block_state_faces_diodes_at_the_player_and_observers_with_the_player() {
    let looking = |yaw: Option<f32>| crate::block_placement::PlaceContext {
        target: BlockPos::new(0, 64, 0),
        face: BlockFace::Up,
        cursor: Vec3f {
            x: 0.5,
            y: 0.0,
            z: 0.5,
        },
        yaw,
        pitch: Some(0.0),
        sneaking: false,
    };
    let air = |_: BlockPos| WorldState::from(StateId::AIR);
    let state = |block: &str, yaw: Option<f32>| {
        placed_block_state(
            Block::from_name(block.strip_prefix("minecraft:").unwrap_or(block))
                .expect("fixture block is built in"),
            &looking(yaw),
            air,
        )
        .map(|placed| placed.state)
    };
    // Looking north (yaw 180): a repeater and comparator face the player —
    // south — while an observer watches north.
    let repeater = state("minecraft:repeater", Some(180.0)).expect("placed repeater");
    assert_eq!(repeater.block(), Block::Repeater);
    assert_eq!(crate::redstone::get_str_property(repeater, PropertyKey::Facing), Some(BuiltinPropertyValue::South));
    let comparator = state("minecraft:comparator", Some(180.0)).expect("placed comparator");
    assert_eq!(comparator.block(), Block::Comparator);
    assert_eq!(crate::redstone::get_str_property(comparator, PropertyKey::Facing), Some(BuiltinPropertyValue::South));
    let observer = state("minecraft:observer", Some(180.0)).expect("placed observer");
    assert_eq!(observer.block(), Block::Observer);
    assert_eq!(crate::redstone::get_str_property(observer, PropertyKey::Facing), Some(BuiltinPropertyValue::North));
    // Looking east (yaw -90): a repeater faces west.
    let repeater = state("minecraft:repeater", Some(-90.0)).expect("placed repeater");
    assert_eq!(repeater.block(), Block::Repeater);
    assert_eq!(crate::redstone::get_str_property(repeater, PropertyKey::Facing), Some(BuiltinPropertyValue::West));
    // Blocks without any orientation keep the bare census name.
    assert_eq!(state("minecraft:dirt", Some(0.0)), None);
    // And no yaw reported yet keeps the bare name for the directional
    // families too.
    assert_eq!(state("minecraft:repeater", None), None);
}

#[test]
fn rejected_two_cell_placement_still_sends_the_partner_correction() {
    let clicked = BlockPos::new(4, 64, 4);
    let target = BlockPos::new(4, 65, 4);
    let upper = BlockPos::new(4, 66, 4);
    let changed = Vec::new();
    let updates = placement_update_positions(clicked, target, &[target, upper], &changed);
    assert_eq!(updates, vec![clicked, target, upper]);

    // The accepted path's fan-out may mention the same positions repeatedly,
    // but the wire still carries one update per cell.
    let changed = vec![(upper, StateId::AIR), (target, StateId::AIR)];
    let updates = placement_update_positions(clicked, target, &[target, upper], &changed);
    assert_eq!(updates, vec![clicked, target, upper]);
}

// -----------------------------------------------------------------
// `placement_obstructs_placer` — the server-side obstruction check used by
// `apply_use_item_on`. A full block cannot be placed through a standing
// player. These tests exercise the pure geometry directly rather
// than driving the whole `apply_use_item_on` pipeline, which needs a live
// `ChunkSource`/`BlockEntityHandle`/`MobHandle` fixture this predicate
// does not touch.
// -----------------------------------------------------------------

/// A full block at the target cell occupied by the player must be refused.
#[test]
fn placement_obstructs_placer_refuses_a_full_block_at_the_players_feet() {
    let target = BlockPos::new(0, 64, 0);
    let feet = Vec3::new(0.5, 64.0, 0.5);
    assert!(placement_obstructs_placer(target, Block::Stone.default_state(), feet));
}

/// The discriminating arm: a state with an **empty** collision shape must
/// never be refused, even at the player's own feet — otherwise this is a
/// blanket "nothing inside the player" rule rather than a real
/// obstruction test. Empty-shape blocks such as torches, rails, pressure
/// plates, and redstone dust remain placeable at those coordinates.
#[test]
fn placement_obstructs_placer_allows_an_empty_shape_at_the_players_feet() {
    let target = BlockPos::new(0, 64, 0);
    let feet = Vec3::new(0.5, 64.0, 0.5);
    assert!(
        lodestone_data::collision_shapes::collision_boxes(
            lodestone_data::block_states::StateId::new(
                lodestone_data::block_states::state_id("minecraft:torch").unwrap(),
            ).expect("torch validates")
        )
        .is_empty(),
        "this test's premise: a torch has no collision boxes"
    );
    assert!(!placement_obstructs_placer(target, Block::Torch.default_state(), feet));
}

/// Control: the same full block, far from the player, must not be
/// refused — proves the detector is a real geometric test and not an
/// unconditional `true`.
#[test]
fn placement_obstructs_placer_allows_a_full_block_far_from_the_player() {
    let target = BlockPos::new(50, 64, 50);
    let feet = Vec3::new(0.5, 64.0, 0.5);
    assert!(!placement_obstructs_placer(target, Block::Stone.default_state(), feet));
}

/// Boundary control: a full block exactly adjacent to the player (sharing
/// only a face) must not be refused — two boxes that only touch are not
/// intersecting, the same strict-inequality convention
/// `lodestone_shell::sim::placement::block_intersects_player` uses for
/// the client's own prediction of this rule.
#[test]
fn placement_obstructs_placer_allows_a_full_block_touching_but_not_overlapping() {
    let target = BlockPos::new(1, 64, 0);
    // Feet at x=0.5, half-width 0.3: the player's box is x in
    // [0.2, 0.8], which shares the x=1.0 boundary with `target` (x in
    // [1.0, 2.0]) without entering it.
    let feet = Vec3::new(0.5, 64.0, 0.5);
    assert!(!placement_obstructs_placer(target, Block::Stone.default_state(), feet));
}

/// The state-shaped case a full-cube approximation gets wrong: a top
/// slab occupies only the *upper* half of its cell, so a player whose own
/// box just clears that upper half is not obstructed by it — while an
/// (otherwise identically positioned) full block still would be. The
/// slab's real bottom edge is read from the live collision-shape table
/// rather than assumed, and the player's feet are derived from it
/// algebraically so the test holds regardless of the shape's exact
/// height.
#[test]
fn placement_obstructs_placer_lets_a_top_slab_clear_the_players_head_where_a_full_block_would_not()
{
    let target = BlockPos::new(0, 66, 0);
    let top_slab = "minecraft:oak_slab[type=top,waterlogged=false]";
    let id = lodestone_data::block_states::state_id(top_slab)
        .expect("minecraft:oak_slab[type=top,waterlogged=false] is a real 26.2 state");
    let state = lodestone_data::block_states::StateId::new(id).expect("top slab validates");
    let boxes = lodestone_data::collision_shapes::collision_boxes(state);
    assert!(!boxes.is_empty(), "a top slab has real collision geometry");
    let box_min_y = boxes
        .iter()
        .map(|b| b.min[1])
        .fold(f32::INFINITY, f32::min);
    assert!(
        box_min_y > 0.1,
        "a top slab must not fill the bottom half of its cell, got {box_min_y}"
    );
    // Stand so the player's own box top lands just under the slab's real
    // bottom edge (clears the slab) but still inside the target cell
    // (so an equivalently placed full block, whose bottom edge is the
    // cell floor, still hits the player).
    let feet_y = f64::from(target.y) + f64::from(box_min_y) - 1.8 - 0.05;
    let feet = Vec3::new(0.5, feet_y, 0.5);
    assert!(
        !placement_obstructs_placer(target, state, feet),
        "a top slab should clear the player's head here"
    );
    assert!(
        placement_obstructs_placer(target, Block::Stone.default_state(), feet),
        "a full block at the same position should still hit the player's head"
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
struct DimensionOnly(crate::dimension::Dimension);

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

struct NativeRestoreWorld;

impl ChunkSource for NativeRestoreWorld {
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
    fn dimension(&self) -> Option<crate::dimension::Dimension> {
        Some(crate::dimension::Dimension::Overworld)
    }
    fn sibling(
        &self,
        dimension: crate::dimension::Dimension,
    ) -> Option<Arc<dyn ChunkSource>> {
        (dimension == crate::dimension::Dimension::Nether)
            .then(|| Arc::new(DimensionOnly(dimension)) as Arc<dyn ChunkSource>)
    }
}

fn native_nether_session() -> (tempfile::TempDir, NativePlayerSession) {
    let directory = tempfile::tempdir().unwrap();
    let storage = Arc::new(
        crate::world_storage::WorldStorage::open(
            crate::world_storage::WorldStorageBackend::LodestoneNative {
                directory: directory.path().to_owned(),
            },
        )
        .unwrap(),
    );
    let loaded = crate::world_storage::NativePlayerData {
        locator: crate::world_storage::NativePlayerRecord {
            uuid: [0x71; 16],
            dimension: lodestone_storage_schema::BuiltinDimension::Nether,
            x_fixed: 12_500,
            y_fixed: 64_250,
            z_fixed: -3_750,
            yaw_millidegrees: 91_000,
            pitch_millidegrees: -12_500,
        },
        game_mode: Some(GameMode::Creative),
        runtime: Some(crate::world_storage::NativePlayerRuntimeState {
            health: 7.5,
            air_supply: 42,
            experience: crate::experience::PlayerExperience::restored(12, 0.5, 345),
        }),
        inventory: None,
    };
    (
        directory,
        NativePlayerSession {
            storage,
            uuid: loaded.locator.uuid,
            loaded: Some(loaded),
            save_blocked: false,
        },
    )
}

#[test]
fn native_nether_restore_selects_the_sibling_and_uses_its_pose() {
    let (_directory, mut session) = native_nether_session();
    let sibling = session
        .restored_source(&NativeRestoreWorld, true)
        .expect("a hosted saved dimension must become the join source");
    assert_eq!(sibling.dimension(), Some(crate::dimension::Dimension::Nether));
    assert_eq!(
        session.join_position(Vec3::new(8.0, 80.0, 8.0)),
        Vec3::new(12.5, 64.25, -3.75),
    );
    assert_eq!(session.initial_rotation(), Some(Rotation::new(91.0, -12.5)));
    let runtime = session.runtime().expect("typed runtime state restores");
    assert_eq!(runtime.health, 7.5);
    assert_eq!(runtime.air_supply, 42);
    assert_eq!(runtime.experience.level(), 12);
    assert_eq!(runtime.experience.progress(), 0.5);
    assert_eq!(runtime.experience.total(), 345);
}

#[test]
fn native_dimension_restore_failure_preserves_the_record() {
    let (_directory, mut session) = native_nether_session();
    assert!(session.restored_source(&NativeRestoreWorld, false).is_none());
    let fallback = Vec3::new(8.0, 80.0, 8.0);
    assert_eq!(session.join_position(fallback), fallback);
    assert!(
        session
            .snapshot(
                Some((1.0, 2.0, 3.0)),
                None,
                fallback,
                crate::dimension::Dimension::Overworld,
                GameMode::Survival,
                &PlayerVitals::default(),
                &crate::experience::PlayerExperience::default(),
                &PlayerInventory::default(),
            )
            .is_none(),
        "a failed cross-dimension restore must not overwrite its evidence on disconnect"
    );
}

/// No saved player at all falls back to the world spawn.
#[test]
fn join_position_for_saved_player_is_world_spawn_with_no_save() {
    let spawn = Vec3::new(8.0, 71.0, 8.0);
    assert_eq!(join_position_for_saved_player(None, spawn), spawn);
}

/// **The discriminating gate for the "buried in the ground"
/// report.** The same raw position, saved under two different dimension
/// tags: the overworld-tagged save is trusted verbatim (predicted to
/// equal the saved position exactly, not merely "differs from spawn"),
/// and the Nether-tagged one must fall back to the world spawn rather
/// than being joined into the overworld as a raw coordinate — which is
/// exactly the bug a player who died or disconnected in the Nether hit.
#[test]
fn join_position_for_saved_player_distrusts_a_non_overworld_position() {
    let spawn = Vec3::new(8.0, 71.0, 8.0);
    // Deliberately not equal to `spawn` on any axis, so a bug that
    // silently returned the wrong constant could not hide.
    let raw = Vec3::new(15.0, 64.0, -3.0);
    let inventory = PlayerInventory::default();

    let overworld_save = crate::player_data::PlayerData::capture(
        raw,
        Rotation::new(0.0, 0.0),
        20.0,
        300,
        GameMode::Survival,
        &inventory,
        crate::experience::PlayerExperience::default(),
        Vec::new(),
        crate::dimension::Dimension::Overworld,
    );
    assert_eq!(
        join_position_for_saved_player(Some(&overworld_save), spawn),
        raw,
        "an overworld-tagged save must be trusted verbatim"
    );

    let nether_save = crate::player_data::PlayerData::capture(
        raw,
        Rotation::new(0.0, 0.0),
        20.0,
        300,
        GameMode::Survival,
        &inventory,
        crate::experience::PlayerExperience::default(),
        Vec::new(),
        crate::dimension::Dimension::Nether,
    );
    assert_eq!(
        join_position_for_saved_player(Some(&nether_save), spawn),
        spawn,
        "a Nether-tagged save must fall back to the world spawn rather than be \
         joined as a raw overworld coordinate"
    );
}

/// An unparseable or unknown dimension tag falls back like any other
/// non-overworld tag instead of being treated as trustworthy.
#[test]
fn join_position_for_saved_player_distrusts_an_unparseable_dimension_tag() {
    let spawn = Vec3::new(8.0, 71.0, 8.0);
    let raw = Vec3::new(15.0, 64.0, -3.0);
    let inventory = PlayerInventory::default();
    let mut save = crate::player_data::PlayerData::capture(
        raw,
        Rotation::new(0.0, 0.0),
        20.0,
        300,
        GameMode::Survival,
        &inventory,
        crate::experience::PlayerExperience::default(),
        Vec::new(),
        crate::dimension::Dimension::Overworld,
    );
    save.dimension = "not a real dimension key".to_string();
    assert_eq!(
        join_position_for_saved_player(Some(&save), spawn),
        spawn,
        "an unparseable dimension tag must not be trusted as the overworld"
    );
}

mod respawn_tests;


/// `swing_action`'s two real inputs, against vanilla's own
/// `ClientboundAnimatePacket` constants (`SWING_MAIN_HAND = 0`,
/// `SWING_OFF_HAND = 3`) rather than the plausible-but-wrong `0`/`1`.
#[test]
fn swing_action_maps_hand_to_vanillas_animate_byte() {
    assert_eq!(swing_action(lodestone_model::Hand::Main), 0);
    assert_eq!(swing_action(lodestone_model::Hand::Off), 3);
}

/// The positive case: a spectator within range of a resolvable player
/// target gets the camera attached.
#[test]
fn spectator_action_resolves_a_nearby_player_target() {
    let registry = PlayerRegistry::new();
    let target = registry.join("Target", Uuid::from_u128(1), Vec3::new(10.0, 64.0, 10.0));
    let result = apply_spectator_action(
        GameMode::Spectator,
        Some(target.entity_id()),
        Some((10.0, 64.0, 11.0)),
        &MobHandle::default(),
        Some(&registry),
    );
    assert_eq!(result, Some(target.entity_id()));
}

/// **Control 1.** The identical setup, but not in spectator mode — the
/// feature is restricted to spectator mode.
#[test]
fn spectator_action_does_nothing_outside_spectator_mode() {
    let registry = PlayerRegistry::new();
    let target = registry.join("Target", Uuid::from_u128(1), Vec3::new(10.0, 64.0, 10.0));
    let result = apply_spectator_action(
        GameMode::Survival,
        Some(target.entity_id()),
        Some((10.0, 64.0, 11.0)),
        &MobHandle::default(),
        Some(&registry),
    );
    assert_eq!(result, None, "survival mode must never attach a camera");
}

/// **Control 2.** A target far outside the interaction range must not
/// resolve, proving the range check is load-bearing rather than
/// decorative (a wrong implementation that ignores distance entirely
/// would pass every other case here).
#[test]
fn spectator_action_rejects_a_target_out_of_range() {
    let registry = PlayerRegistry::new();
    let target = registry.join("Target", Uuid::from_u128(1), Vec3::new(500.0, 64.0, 500.0));
    let result = apply_spectator_action(
        GameMode::Spectator,
        Some(target.entity_id()),
        Some((10.0, 64.0, 11.0)),
        &MobHandle::default(),
        Some(&registry),
    );
    assert_eq!(result, None, "a target 500+ blocks away must not resolve");
}

/// **Control 3.** No target on the wire (`OptionalInt` absent) must do
/// nothing, matching vanilla's own handler, which has no branch for it at
/// all.
#[test]
fn spectator_action_does_nothing_with_no_target() {
    let registry = PlayerRegistry::new();
    let result = apply_spectator_action(
        GameMode::Spectator,
        None,
        Some((10.0, 64.0, 11.0)),
        &MobHandle::default(),
        Some(&registry),
    );
    assert_eq!(result, None);
}

/// **Control 4.** An unresolvable id (no mob, no player) must do nothing
/// rather than attach a camera to a fabricated position.
#[test]
fn spectator_action_rejects_an_unresolvable_target() {
    let registry = PlayerRegistry::new();
    let result = apply_spectator_action(
        GameMode::Spectator,
        Some(9999),
        Some((10.0, 64.0, 11.0)),
        &MobHandle::default(),
        Some(&registry),
    );
    assert_eq!(result, None);
}

/// Completing a `minecraft:ominous_bottle` use grants
/// `minecraft:bad_omen` for 120000 ticks at amplifier 0 and consumes the
/// bottle. The assertion covers both the effect result and the held-item
/// decrement.
#[test]
fn finish_drinking_ominous_bottle_grants_bad_omen_and_consumes_the_bottle() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(stack("minecraft:ominous_bottle", 1)));
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let started = ItemInUse {
        native: 0,
        item: "minecraft:ominous_bottle".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    assert!(effects.get("minecraft:bad_omen").is_none(), "precondition: no Bad Omen carried yet");
    let result = finish_drinking_ominous_bottle(&mut inv, &mut effects, &started, GameMode::Survival);
    let (native, remainder) = result.expect("a present ominous bottle must finish");
    assert_eq!(native, 0);
    assert!(remainder.is_none(), "the sole stack of 1 must be fully consumed, leaving the slot empty");
    assert_eq!(inv.native(0), None, "the bottle must actually leave the inventory");

    let instance = effects.get("minecraft:bad_omen").expect("Bad Omen must now be carried");
    assert_eq!(instance.amplifier(), 0);
    assert_eq!(instance.duration(), 120_000, "OminousBottleAmplifier.EFFECT_DURATION");
}

/// **Control**: any other item (even another drinkable, like a plain
/// potion) must not grant Bad Omen or be consumed through this path —
/// this function's whole reason to be separate from [`finish_consuming`]
/// is that it is a one-item special case, not a general drink handler.
#[test]
fn finish_drinking_ominous_bottle_ignores_every_other_item() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(stack("minecraft:potion", 1)));
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let started = ItemInUse {
        native: 0,
        item: "minecraft:potion".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let result = finish_drinking_ominous_bottle(&mut inv, &mut effects, &started, GameMode::Survival);
    assert!(result.is_none(), "a plain potion must not be handled by the ominous-bottle path");
    assert!(effects.get("minecraft:bad_omen").is_none(), "no effect must be granted");
    assert_eq!(inv.native(0), Some(&stack("minecraft:potion", 1)), "the potion must stay in hand, untouched");
}

fn potion_stack(potion: &str, count: u32) -> ItemStack {
    let mut s = stack("minecraft:potion", count);
    s.components.potion = Some(lodestone_data::potion::potion_id(potion).expect("real potion"));
    s
}

/// A real timed-effect potion (Strength II) must
/// land its full, **unscaled** duration and amplifier on the drinker and
/// consume the bottle — the whole reason this function exists, since before
/// it every potion in the game did nothing at all.
#[test]
fn finish_drinking_potion_grants_the_full_unscaled_effect() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(potion_stack("minecraft:strong_strength", 1)));
    let started = ItemInUse {
        native: 0,
        item: "minecraft:potion".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (native, remainder, effects) =
        finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("a real potion must finish");
    assert_eq!(native, 0);
    assert!(remainder.is_none(), "the sole stack of 1 must be fully consumed");
    assert_eq!(
        effects,
        vec![crate::mob_effects::SplashEffect::Timed {
            effect_id: lodestone_data::mob_effects::MobEffectId::STRENGTH,
            duration: 1800,
            amplifier: 1,
        }],
        "Strong Strength: amplifier II, 1:30 — unscaled, not the splash falloff"
    );
}

/// An instant potion (Harming) reaches the caller as a full-strength
/// [`SplashEffect::Instant`] — `6 << amplifier` unscaled, the same
/// `splash_instant_amount` computation at `scale = 1.0` a direct hit at
/// point-blank range would produce, proving drinking is not merely "a splash
/// with the thrower standing on the target".
#[test]
fn finish_drinking_potion_carries_an_instant_effect_at_full_strength() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(potion_stack("minecraft:harming", 1)));
    let started = ItemInUse {
        native: 0,
        item: "minecraft:potion".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (_, _, effects) =
        finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("harming must finish");
    assert_eq!(
        effects,
        vec![crate::mob_effects::SplashEffect::Instant {
            effect_id: lodestone_data::mob_effects::MobEffectId::INSTANT_DAMAGE,
            amount: 6.0,
        }]
    );
}

/// **Control**: a water bottle's `minecraft:potion` id resolves (it is a
/// real potion), but its built-in effect list is empty, so drinking it must
/// still fully consume the bottle and yield zero grants — not "not handled"
/// and not a panic on an empty list.
#[test]
fn finish_drinking_potion_water_bottle_control() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(potion_stack("minecraft:water", 1)));
    let started = ItemInUse {
        native: 0,
        item: "minecraft:potion".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (_, remainder, effects) =
        finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("water must still finish");
    assert!(remainder.is_none());
    assert!(effects.is_empty());
}

/// An out-of-census component value remains a wire-boundary failure, not
/// an empty entry in the built-in potion table. The stack still finishes
/// consuming, but it cannot grant an arbitrary built-in effect.
#[test]
fn finish_drinking_potion_rejects_an_unknown_component_id() {
    let mut invalid = stack("minecraft:potion", 1);
    invalid.components.potion = Some(-1);
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(invalid));
    let started = ItemInUse {
        native: 0,
        item: "minecraft:potion".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (_, remainder, effects) =
        finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("the stack must finish");
    assert!(remainder.is_none(), "the invalid component does not cancel consumption");
    assert!(effects.is_empty(), "an unknown raw id cannot become a built-in effect");
}

/// **Control**: any other item, including food, must not be handled by
/// this path.
#[test]
fn finish_drinking_potion_ignores_every_other_item() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(stack("minecraft:golden_apple", 1)));
    let started = ItemInUse {
        native: 0,
        item: "minecraft:golden_apple".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };
    assert!(finish_drinking_potion(&mut inv, &started, GameMode::Survival).is_none());
}

/// Drinking milk clears every active effect and
/// reports exactly the ids that were cleared, and consumes the bucket.
#[test]
fn finish_drinking_milk_clears_every_active_effect() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(stack("minecraft:milk_bucket", 1)));
    let mut effects = crate::mob_effects::ActiveEffects::new();
    effects.apply("minecraft:poison", 100, 0);
    effects.apply("minecraft:speed", 200, 1);
    let started = ItemInUse {
        native: 0,
        item: "minecraft:milk_bucket".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (native, remainder, mut cleared) =
        finish_drinking_milk(&mut inv, &mut effects, &started, GameMode::Survival).expect("milk must finish");
    assert_eq!(native, 0);
    assert!(remainder.is_none());
    cleared.sort();
    assert_eq!(cleared, vec!["minecraft:poison".to_owned(), "minecraft:speed".to_owned()]);
    assert!(effects.is_empty(), "every effect must actually be gone");
}

/// **Control**: milk drunk with nothing active clears nothing (an empty
/// `Vec`, not a sentinel) but still consumes the bucket — matching this
/// crate's own water-bottle-control convention for "ran, and had nothing to
/// do" versus "did not run".
#[test]
fn finish_drinking_milk_with_no_active_effects_control() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(stack("minecraft:milk_bucket", 1)));
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let started = ItemInUse {
        native: 0,
        item: "minecraft:milk_bucket".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (_, remainder, cleared) =
        finish_drinking_milk(&mut inv, &mut effects, &started, GameMode::Survival).expect("milk must finish");
    assert!(remainder.is_none(), "the bucket is still consumed");
    assert!(cleared.is_empty());
}

/// `player_overlaps_piston_sweep` verifies the overlap test used for
/// connection-side piston self-correction. A player standing in either the
/// source or destination cell must overlap; one standing a full block clear
/// of both must not.
#[test]
fn player_overlaps_piston_sweep_matches_source_and_dest_but_not_clear_ground() {
    let source = BlockPos::new(4, 0, 0);
    let dest = BlockPos::new(5, 0, 0);

    assert!(
        player_overlaps_piston_sweep(5.5, 0.0, 0.5, source, dest),
        "a player standing in the destination cell must overlap"
    );
    assert!(
        player_overlaps_piston_sweep(4.5, 0.0, 0.5, source, dest),
        "a player standing in the source cell must overlap too"
    );
    assert!(
        !player_overlaps_piston_sweep(10.5, 0.0, 0.5, source, dest),
        "control: a player well clear of both cells must not overlap"
    );
}

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
