use lodestone_server::worldgen_session::{
    BlockCoordinate, GenerationRequest, GenerationSession, ImmutableProduct, ImmutableSidecar,
    ImmutableStageCompletion, ProductKey, SessionBudget,
};
use lodestone_server::{
    end_chunk_source, nether_chunk_source, overworld_chunk_source, ChunkColumn, ChunkSource,
};
use lodestone_worldgen::stage_schedule::{
    BarrierPolicy, ColumnStage, Dimension, GenerationTarget, ResourceKey, StageKey,
};
#[cfg(not(target_arch = "wasm32"))]
use lodestone_worldgen::structure::StructureBlocks;

#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use lodestone_server::dimension::{Dimension as ServerDimension, DimensionalSource};
#[cfg(not(target_arch = "wasm32"))]
use lodestone_server::portal::PortalIndex;
#[cfg(not(target_arch = "wasm32"))]
use lodestone_server::region_source::RegionChunkSource;
#[cfg(not(target_arch = "wasm32"))]
use lodestone_server::worldgen_session::{
    PacketSnapshot, RequestStageDriver, SessionError,
};

#[cfg(not(target_arch = "wasm32"))]
struct ProbeDriver {
    calls: Arc<AtomicUsize>,
}

#[cfg(not(target_arch = "wasm32"))]
impl RequestStageDriver for ProbeDriver {
    fn generate(
        &self,
        session: &mut GenerationSession,
    ) -> Result<PacketSnapshot, SessionError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let target = session.request().target();
        for &stage in session
            .pipeline()
            .schedule()
            .stages_for(session.request().generation_target())
        {
            let key = StageKey::new(session.pipeline().dimension(), stage);
            if session
                .frontier(target)
                .is_some_and(|frontier| frontier.records().iter().any(|record| record.key() == key))
            {
                continue;
            }
            let descriptor = session
                .pipeline()
                .descriptor(stage)
                .expect("stage is in the selected pipeline");
            if descriptor.barrier() == BarrierPolicy::SourceOrdered {
                let sources = session.admission_order().to_vec();
                session.declare_mutable_sources(
                    key,
                    sources
                        .iter()
                        .copied()
                        .enumerate()
                        .map(|(order, source)| (order as u64, source)),
                )?;
                for (source_order, source) in sources.into_iter().enumerate() {
                    if session.source_order_committed(source_order as u64) {
                        continue;
                    }
                    let transaction = session.begin_mutable_source(
                        source,
                        key,
                        source_order as u64,
                    )?;
                    session.complete_mutable_source(transaction)?;
                }
                let products = descriptor
                    .outputs()
                    .iter()
                    .copied()
                    .map(|resource| ImmutableProduct::new(resource, stage as u8))
                    .collect();
                let sidecars = descriptor
                    .retained_sidecars()
                    .iter()
                    .copied()
                    .map(|sidecar| ImmutableSidecar::new(sidecar, stage as u8))
                    .collect();
                session.commit_mutable_stage(
                    key,
                    [stage as u8; 32],
                    [stage as u8; 32],
                    1,
                    products,
                    sidecars,
                )?;
                continue;
            }
            let products = descriptor
                .outputs()
                .iter()
                .copied()
                .map(|resource| ImmutableProduct::new(resource, stage as u8))
                .collect();
            let sidecars = descriptor
                .retained_sidecars()
                .iter()
                .copied()
                .map(|sidecar| ImmutableSidecar::new(sidecar, stage as u8))
                .collect();
            session.complete_immutable(ImmutableStageCompletion::new(
                target,
                key,
                [stage as u8; 32],
                [stage as u8; 32],
                1,
                products,
                sidecars,
            ))?;
            session.advance_ready_immutable()?;
        }
        session.complete_light_domain()?;
        session.finalize_packet(ChunkColumn::new(-64, 384))
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct DriverSource {
    driver: ProbeDriver,
}

#[cfg(not(target_arch = "wasm32"))]
impl ChunkSource for DriverSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(-64, 384)
    }

    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:air".to_owned()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:the_void".to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}

    fn request_stage_driver(&self) -> Option<&dyn RequestStageDriver> {
        Some(&self.driver)
    }
}

fn completion(
    session: &GenerationSession,
    coordinate: (i32, i32),
    stage: ColumnStage,
) -> ImmutableStageCompletion {
    let key = StageKey::new(session.pipeline().dimension(), stage);
    let descriptor = session
        .pipeline()
        .descriptor(stage)
        .expect("fixture stage is in the selected pipeline");
    let products = descriptor
        .outputs()
        .iter()
        .copied()
        .map(|resource| ImmutableProduct::new(resource, stage as u8))
        .collect();
    let sidecars = descriptor
        .retained_sidecars()
        .iter()
        .copied()
        .map(|sidecar| ImmutableSidecar::new(sidecar, stage as u8))
        .collect();
    ImmutableStageCompletion::new(coordinate, key, [stage as u8; 32], [stage as u8; 32], 1, products, sidecars)
}

fn end_request() -> GenerationRequest {
    GenerationRequest::new(Dimension::End, (4, -9), GenerationTarget::Full, 1)
}

fn advance_end_prefix(session: &mut GenerationSession) {
    let target = session.request().target();
    for stage in [
        ColumnStage::Fill,
        ColumnStage::Biomes,
        ColumnStage::Surface,
        ColumnStage::Materialize,
        ColumnStage::StructureStarts,
    ] {
        let result = completion(session, target, stage);
        session
            .complete_immutable(result)
            .expect("pure prefix accepts the fixture product set");
        session
            .advance_ready_immutable()
            .expect("pure prefix commits in stage order");
    }
}

#[test]
fn immutable_workers_may_finish_out_of_order_but_frontier_commits_in_order() {
    let mut session = GenerationSession::new(end_request());
    let target = session.request().target();
    let later_result = completion(&session, target, ColumnStage::Biomes);
    session
        .complete_immutable(later_result)
        .expect("a later pure stage can be retained while its worker is early");
    assert!(session.frontier(target).unwrap().records().is_empty());

    let first_result = completion(&session, target, ColumnStage::Fill);
    session
        .complete_immutable(first_result)
        .expect("the first pure stage is accepted");
    let report = session
        .advance_ready_immutable()
        .expect("both ready prefix stages commit");
    assert_eq!(
        report.committed_stages(),
        &[
            StageKey::new(Dimension::End, ColumnStage::Fill),
            StageKey::new(Dimension::End, ColumnStage::Biomes),
        ]
    );
    assert_eq!(
        session.frontier(target).unwrap().records().len(),
        2,
        "worker completion order must not alter the frontier prefix"
    );
}

#[test]
fn cancellation_discards_pending_work_but_keeps_committed_products() {
    let mut session = GenerationSession::new(end_request());
    let target = session.request().target();
    let fill = completion(&session, target, ColumnStage::Fill);
    session
        .complete_immutable(fill)
        .unwrap();
    session.advance_ready_immutable().unwrap();
    let biomes = completion(&session, target, ColumnStage::Biomes);
    session
        .complete_immutable(biomes)
        .unwrap();
    let cancellation = session.cancellation();
    cancellation.cancel();

    let read = session.resident_read(target).unwrap();
    assert_eq!(session.usage().product_entries(), 1);
    assert_eq!(
        read.product::<u8>(ResourceKey::DensityField).as_deref(),
        Some(&(ColumnStage::Fill as u8))
    );
    assert!(read.product::<u8>(ResourceKey::BiomeQuarts).is_none());
    assert!(matches!(
        session.advance_ready_immutable(),
        Err(lodestone_server::worldgen_session::SessionError::Cancelled)
    ));
    assert!(cancellation.is_cancelled());
}

#[test]
fn mutable_sources_commit_as_one_canonical_prefix_and_rollback_is_private() {
    let mut session = GenerationSession::new(end_request());
    advance_end_prefix(&mut session);
    let stage = StageKey::new(Dimension::End, ColumnStage::Features);
    session
        .declare_mutable_sources(stage, [(0, (4, -8)), (1, (5, -9)), (2, (4, -9))])
        .unwrap();
    let mut later = session
        .begin_mutable_source((5, -9), stage, 1)
        .expect("later source may complete before the canonical predecessor");
    later
        .push(0, BlockCoordinate::new(81, 70, -140), 7_u8)
        .unwrap();
    let report = session.complete_mutable_source(later).unwrap();
    assert!(report.committed_source_orders().is_empty());
    assert_eq!(session.committed_mutation_order().len(), 0);

    let mut first = session
        .begin_mutable_source((4, -8), stage, 0)
        .expect("the first source can now be submitted");
    first
        .push(0, BlockCoordinate::new(64, 70, -140), 5_u8)
        .unwrap();
    let report = session.complete_mutable_source(first).unwrap();
    assert_eq!(report.committed_source_orders(), &[0, 1]);
    let committed = session.committed_mutation_order();
    assert_eq!(committed.len(), 2);
    assert_eq!(committed[0].source(), (4, -8));
    assert_eq!(committed[1].source(), (5, -9));

    let mut rolled_back = session
        .begin_mutable_source((4, -9), stage, 2)
        .unwrap();
    rolled_back
        .push(0, BlockCoordinate::new(65, 70, -140), 9_u8)
        .unwrap();
    session.rollback_transaction(rolled_back).unwrap();
    assert_eq!(session.committed_mutation_order().len(), 2);
}

#[test]
fn packet_snapshot_is_detached_after_generation_and_light_settlement() {
    let mut session = GenerationSession::new(end_request());
    advance_end_prefix(&mut session);
    let stage = StageKey::new(Dimension::End, ColumnStage::Features);
    session
        .declare_mutable_sources(stage, [(0, (4, -9)), (1, (5, -9))])
        .unwrap();
    for (source_order, source) in [(0, (4, -9)), (1, (5, -9))] {
        let transaction = session
            .begin_mutable_source(source, stage, source_order)
            .unwrap();
        session.complete_mutable_source(transaction).unwrap();
    }
    let descriptor = session.pipeline().descriptor(ColumnStage::Features).unwrap();
    let products = descriptor
        .outputs()
        .iter()
        .copied()
        .map(|resource| ImmutableProduct::new(resource, 1_u8))
        .collect();
    let sidecars = descriptor
        .retained_sidecars()
        .iter()
        .copied()
        .map(|sidecar| ImmutableSidecar::new(sidecar, 1_u8))
        .collect();
    session
        .commit_mutable_stage(stage, [1; 32], [2; 32], 1, products, sidecars)
        .unwrap();
    let output = completion(&session, session.request().target(), ColumnStage::Output);
    session.complete_immutable(output)
        .unwrap();
    session.advance_ready_immutable().unwrap();
    assert!(matches!(
        session.finalize_packet(ChunkColumn::new(0, 16)),
        Err(_)
    ));
    session.complete_light_domain().unwrap();
    let packet = session.finalize_packet(ChunkColumn::new(0, 16)).unwrap();
    assert_eq!(packet.column().height, 16);
    assert!(packet.neighbours().is_empty());
}

#[test]
fn immutable_completion_rejects_mutable_stage() {
    let session = GenerationSession::new(end_request());
    let completion = completion(&session, session.request().target(), ColumnStage::Features);
    let mut rejected = GenerationSession::new(end_request());
    assert!(matches!(
        rejected.complete_immutable(completion),
        Err(lodestone_server::worldgen_session::SessionError::MutableStage { .. })
    ));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn request_driver_survives_dimension_persistence_arc_and_borrowed_wrappers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let driver = ProbeDriver {
        calls: Arc::clone(&calls),
    };
    let primary = DriverSource { driver };
    let dimension = DimensionalSource::alone(
        primary,
        ServerDimension::End,
        PortalIndex::new(),
    );
    assert!(dimension.request_stage_driver().is_some());

    let directory = tempfile::tempdir().expect("temporary world directory");
    let region = RegionChunkSource::new(
        dimension,
        directory.path(),
        ServerDimension::End,
        -64,
        384,
    )
    .expect("open temporary world");
    assert!(region.request_stage_driver().is_none());

    let handle = Arc::new(region);
    assert!(handle.request_stage_driver().is_none());
    let borrowed = &handle;
    assert!(borrowed.request_stage_driver().is_none());

    let mut session = GenerationSession::new(GenerationRequest::new(
        Dimension::End,
        (4, -9),
        GenerationTarget::Full,
        1,
    ));
    let request = session.request();
    let packet = match handle
        .request_generation(request, Some(&mut session))
        .expect("capability forwarded through every wrapper")
        .expect("probe driver completes the full stage sequence")
    {
        lodestone_server::worldgen_session::GenerationRequestResult::Generated(packet) => packet,
        lodestone_server::worldgen_session::GenerationRequestResult::Existing(_) => {
            panic!("probe source has no persisted column")
        }
    };
    assert_eq!(packet.coordinate(), (4, -9));
    assert!(packet.neighbours().is_empty());
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

struct LegacySource;

impl ChunkSource for LegacySource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:air".to_owned()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:the_void".to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

#[test]
fn legacy_source_without_capability_keeps_scalar_generation() {
    let source = LegacySource;
    assert!(source.request_stage_driver().is_none());
    assert_eq!(source.column(0, 0).height, 16);
}

#[cfg(not(target_arch = "wasm32"))]
fn run_real_source_request<S: ChunkSource>(source: &S, dimension: Dimension) {
    let target = (0, 0);
    let request = GenerationRequest::new(dimension, target, GenerationTarget::Full, 1);
    let mut session = GenerationSession::new(request);
    let packet = source
        .request_stage_driver()
        .expect("bundled dimension exposes the production driver")
        .generate(&mut session)
        .expect("production driver completes a full request");
    assert_eq!(packet.coordinate(), target);
    assert_eq!(packet.column().generation_stage(), lodestone_server::ChunkGenerationStage::Full);
    assert!(session
        .frontier(target)
        .expect("target admitted")
        .records()
        .iter()
        .any(|record| record.key() == StageKey::new(dimension, ColumnStage::Output)));
    if dimension == Dimension::Overworld {
        let scalar = source.column(target.0, target.1);
        let start_signature = |column: &ChunkColumn| {
            column
                .structure_starts()
                .iter()
                .map(|start| {
                    (
                        start.structure.clone(),
                        start.chunk_x,
                        start.chunk_z,
                        start.references,
                        start.bounding_box,
                        start.pieces.len(),
                        start.terrain_adaptation,
                        start.pieces_complete,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(start_signature(packet.column()), start_signature(&scalar));
        assert_eq!(
            packet.column().structure_references(),
            scalar.structure_references(),
        );
        assert_eq!(packet.column().block_entities(), scalar.block_entities());
    } else {
        session
            .resident_read(target)
            .expect("target resident is readable")
            .product_at::<StructureBlocks>(ProductKey::new(
                target,
                StageKey::new(dimension, ColumnStage::Features),
                ResourceKey::StructureBlocks,
            ))
            .expect("dimension Features retains typed structure blocks");
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn production_driver_completes_real_requests_in_each_dimension() {
    run_real_source_request(&overworld_chunk_source(42), Dimension::Overworld);
    run_real_source_request(&nether_chunk_source(42), Dimension::Nether);
    run_real_source_request(&end_chunk_source(42), Dimension::End);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn cancelled_real_request_does_not_admit_work() {
    let source = end_chunk_source(42);
    let request = GenerationRequest::new(Dimension::End, (0, 0), GenerationTarget::Full, 1);
    let mut session = GenerationSession::with_cancellation(
        request,
        lodestone_server::worldgen_session::RequestCancellation::new(),
    );
    session.cancel();
    assert!(matches!(
        source
            .request_stage_driver()
            .expect("bundled End exposes the production driver")
            .generate(&mut session),
        Err(SessionError::Cancelled)
    ));
    assert_eq!(session.usage().product_entries(), 0);
}

#[test]
fn mutable_stage_requires_the_complete_declared_source_set() {
    let mut session = GenerationSession::new(end_request());
    advance_end_prefix(&mut session);
    let stage = StageKey::new(Dimension::End, ColumnStage::Features);
    session
        .declare_mutable_sources(stage, [(0, (4, -9)), (1, (5, -9))])
        .unwrap();
    let transaction = session
        .begin_mutable_source((4, -9), stage, 0)
        .unwrap();
    session.complete_mutable_source(transaction).unwrap();
    let descriptor = session.pipeline().descriptor(ColumnStage::Features).unwrap();
    let products = descriptor
        .outputs()
        .iter()
        .copied()
        .map(|resource| ImmutableProduct::new(resource, 1_u8))
        .collect();
    let sidecars = descriptor
        .retained_sidecars()
        .iter()
        .copied()
        .map(|sidecar| ImmutableSidecar::new(sidecar, 1_u8))
        .collect();
    assert!(matches!(
        session.commit_mutable_stage(stage, [1; 32], [2; 32], 1, products, sidecars),
        Err(lodestone_server::worldgen_session::SessionError::MutableSourcesIncomplete {
            expected: 2,
            committed: 1,
            ..
        })
    ));
}

#[test]
fn session_budgets_reject_oversized_product_sidecar_and_mutation() {
    let request = end_request();
    let mut product_session = GenerationSession::with_budget(
        request,
        SessionBudget::new(0, 2048, 131_072, 64 * 1024 * 1024),
    );
    assert!(matches!(
        product_session.complete_immutable(completion(
            &product_session,
            request.target(),
            ColumnStage::Fill,
        )),
        Err(lodestone_server::worldgen_session::SessionError::BudgetExceeded {
            kind: lodestone_server::worldgen_session::BudgetKind::Products,
            ..
        })
    ));

    let nether_request = GenerationRequest::new(
        Dimension::Nether,
        request.target(),
        GenerationTarget::Full,
        request.dependency_radius(),
    );
    let mut sidecar_session = GenerationSession::with_budget(
        nether_request,
        SessionBudget::new(4096, 0, 131_072, 64 * 1024 * 1024),
    );
    assert!(matches!(
        sidecar_session.complete_immutable(completion(
            &sidecar_session,
            nether_request.target(),
            ColumnStage::StructureReferences,
        )),
        Err(lodestone_server::worldgen_session::SessionError::BudgetExceeded {
            kind: lodestone_server::worldgen_session::BudgetKind::Sidecars,
            ..
        })
    ));

    let mut mutation_session = GenerationSession::with_budget(
        request,
        SessionBudget::new(4096, 2048, 0, 64 * 1024 * 1024),
    );
    advance_end_prefix(&mut mutation_session);
    let stage = StageKey::new(Dimension::End, ColumnStage::Features);
    mutation_session
        .declare_mutable_sources(stage, [(0, request.target())])
        .unwrap();
    let mut transaction = mutation_session
        .begin_mutable_source(request.target(), stage, 0)
        .unwrap();
    transaction
        .push_with_retained_bytes(0, BlockCoordinate::new(64, 70, -140), 5_u8, 1)
        .unwrap();
    assert!(matches!(
        mutation_session.complete_mutable_source(transaction),
        Err(lodestone_server::worldgen_session::SessionError::BudgetExceeded {
            kind: lodestone_server::worldgen_session::BudgetKind::Mutations,
            ..
        })
    ));
}

#[test]
fn mutable_writes_cannot_escape_the_declared_chunk_domain() {
    let mut session = GenerationSession::new(end_request());
    advance_end_prefix(&mut session);
    let target = session.request().target();
    let stage = StageKey::new(Dimension::End, ColumnStage::Features);
    session.declare_mutable_sources(stage, [(0, target)]).unwrap();
    let mut transaction = session.begin_mutable_source(target, stage, 0).unwrap();
    transaction
        .push(0, BlockCoordinate::new(0, 70, -140), 5_u8)
        .unwrap();
    assert!(matches!(
        session.complete_mutable_source(transaction),
        Err(lodestone_server::worldgen_session::SessionError::MutationOutsideWriteDomain { .. })
    ));
}

#[test]
fn packet_finalization_requires_exact_ready_neighbour_domain_and_cancellation() {
    let mut session = GenerationSession::new(end_request());
    advance_end_prefix(&mut session);
    let stage = StageKey::new(Dimension::End, ColumnStage::Features);
    let target = session.request().target();
    session
        .declare_mutable_sources(stage, [(0, target)])
        .unwrap();
    let transaction = session
        .begin_mutable_source(target, stage, 0)
        .unwrap();
    session.complete_mutable_source(transaction).unwrap();
    let descriptor = session.pipeline().descriptor(ColumnStage::Features).unwrap();
    let products = descriptor
        .outputs()
        .iter()
        .copied()
        .map(|resource| ImmutableProduct::new(resource, 1_u8))
        .collect();
    let sidecars = descriptor
        .retained_sidecars()
        .iter()
        .copied()
        .map(|sidecar| ImmutableSidecar::new(sidecar, 1_u8))
        .collect();
    session
        .commit_mutable_stage(stage, [1; 32], [2; 32], 1, products, sidecars)
        .unwrap();
    let output = completion(&session, session.request().target(), ColumnStage::Output);
    session.complete_immutable(output).unwrap();
    session.advance_ready_immutable().unwrap();
    session.complete_light_domain().unwrap();
    session
        .declare_packet_neighbours([(5, -9)])
        .unwrap();
    assert!(matches!(
        session.finalize_packet(ChunkColumn::new(0, 16)),
        Err(lodestone_server::worldgen_session::SessionError::PacketNeighbourDomainMismatch)
    ));
    assert!(matches!(
        session.finalize_packet_snapshot(
            ChunkColumn::new(0, 16),
            [((5, -9), ChunkColumn::new(0, 16))],
        ),
        Err(lodestone_server::worldgen_session::SessionError::PacketNeighbourNotReady {
            coordinate: (5, -9),
        })
    ));
    session.cancel();
    assert!(matches!(
        session.finalize_packet(ChunkColumn::new(0, 16)),
        Err(lodestone_server::worldgen_session::SessionError::Cancelled)
    ));
}

#[test]
fn checkpoint_round_trip_restores_committed_products_and_rejects_missing_sidecars() {
    let mut session = GenerationSession::new(end_request());
    let target = session.request().target();
    let fill = completion(&session, target, ColumnStage::Fill);
    session.complete_immutable(fill).unwrap();
    session.advance_ready_immutable().unwrap();
    let checkpoint = session.export_checkpoint();
    let restored = GenerationSession::from_checkpoint(checkpoint.clone()).unwrap();
    assert_eq!(
        restored
            .resident_read(target)
            .unwrap()
            .product::<u8>(ResourceKey::DensityField)
            .as_deref(),
        Some(&(ColumnStage::Fill as u8))
    );

    let missing = ImmutableStageCompletion::new(
        target,
        StageKey::new(Dimension::End, ColumnStage::Output),
        [0; 32],
        [0; 32],
        1,
        vec![ImmutableProduct::new(
            ResourceKey::OutputColumn,
            1_u8,
        )],
        Vec::new(),
    );
    let mut fresh = GenerationSession::new(end_request());
    assert!(matches!(
        fresh.complete_immutable(missing),
        Err(lodestone_server::worldgen_session::SessionError::MissingSidecar { .. })
    ));
}

#[test]
fn checkpoint_round_trip_restores_an_ordered_partial_mutable_prefix() {
    let mut session = GenerationSession::new(end_request());
    advance_end_prefix(&mut session);
    let target = session.request().target();
    let stage = StageKey::new(Dimension::End, ColumnStage::Features);
    session
        .declare_mutable_sources(stage, [(0, target), (1, (5, -9))])
        .unwrap();
    let mut transaction = session.begin_mutable_source(target, stage, 0).unwrap();
    transaction
        .push(0, BlockCoordinate::new(64, 70, -140), 3_u8)
        .unwrap();
    session.complete_mutable_source(transaction).unwrap();

    let checkpoint = session.export_checkpoint();
    let mut restored = GenerationSession::from_checkpoint(checkpoint).unwrap();
    restored
        .declare_mutable_sources(stage, [(0, target), (1, (5, -9))])
        .unwrap();
    assert!(restored.source_order_committed(0));
    assert!(!restored.source_order_committed(1));
    assert_eq!(restored.committed_mutation_order().len(), 1);
}
