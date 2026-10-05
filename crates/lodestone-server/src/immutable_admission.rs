use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lodestone_worldgen::end::EndGenerationIdentity;
use lodestone_worldgen::nether::NetherGenerationIdentity;
use lodestone_worldgen::overworld::{GeneratedColumn, OverworldGenerator};
use lodestone_worldgen::stage_schedule::{
    GeneratedProducerIdentity, GeneratedStageIdentity, GenerationTarget, PipelineOptions,
    StageIdentity, END_PIPELINE, NETHER_PIPELINE,
};
use lodestone_worldgen::structure::StructureStart;

use crate::worldgen_progress::{PhaseTimer, WorldgenTimingPhase};

type Coordinate = (i32, i32);

pub struct OwnedAdmissionWork(Box<dyn FnOnce() -> OwnedAdmissionProducts + Send>);

impl OwnedAdmissionWork {
    pub(crate) fn new(work: impl FnOnce() -> OwnedAdmissionProducts + Send + 'static) -> Self {
        Self(Box::new(work))
    }

    pub(crate) fn run(self) -> OwnedAdmissionProducts { (self.0)() }
}

impl std::fmt::Debug for OwnedAdmissionWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedAdmissionWork(..)")
    }
}

pub enum AdmissionColumn {
    Generated(GeneratedColumn),
    Materialized(crate::chunk::ChunkColumn),
}

pub(crate) struct AdmissionMetadata {
    pub(crate) boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    pub(crate) content: Option<AdmissionContentMetadata>,
    pub(crate) client_heightmaps: Option<crate::worldgen_lifecycle::LifecycleClientHeightmaps>,
    pub(crate) references: Option<BTreeMap<String, Vec<i64>>>,
}

pub(crate) struct AdmissionContentMetadata {
    pub(crate) fingerprint: StageIdentity,
    pub(crate) retained_bytes: usize,
}

pub struct OwnedAdmissionProducts {
    pub(crate) columns: Vec<(Coordinate, AdmissionColumn, AdmissionMetadata)>,
    pub(crate) context: Option<AdmissionProducts>,
}

impl std::fmt::Debug for OwnedAdmissionProducts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedAdmissionProducts")
            .field("columns", &self.columns.iter().map(|(coordinate, ..)| coordinate).collect::<Vec<_>>())
            .field("context", &self.context.is_some())
            .finish()
    }
}

pub(crate) fn materialized_product(
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: crate::chunk::ChunkColumn,
    client_heightmaps: Option<crate::worldgen_lifecycle::LifecycleClientHeightmaps>,
    references: Option<BTreeMap<String, Vec<i64>>>,
) -> (Coordinate, AdmissionColumn, AdmissionMetadata) {
    materialized_product_with_fingerprint(coordinate, boundary, column, client_heightmaps, references, None)
}

pub(crate) fn pristine_end_product(
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: crate::chunk::ChunkColumn,
    client_heightmaps: Option<crate::worldgen_lifecycle::LifecycleClientHeightmaps>,
    identity: &EndGenerationIdentity,
) -> (Coordinate, AdmissionColumn, AdmissionMetadata) {
    let fingerprint = pristine_end_identity(
        identity, coordinate, boundary, &column,
        EndGenerationIdentity::SHAPED_VERSION,
        crate::production_worldgen_session::EXECUTOR_VERSION,
    );
    materialized_product_with_fingerprint(
        coordinate, boundary, column, client_heightmaps, None, Some(fingerprint),
    )
}

fn pristine_end_identity(
    identity: &EndGenerationIdentity,
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: &crate::chunk::ChunkColumn,
    shaped_version: u32,
    executor_version: u32,
) -> StageIdentity {
    pristine_product_identity(
        GeneratedProducerIdentity::End(identity.clone()), coordinate, boundary, column,
        shaped_version, executor_version,
    )
}

pub(crate) fn pristine_nether_product(
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: crate::chunk::ChunkColumn,
    client_heightmaps: Option<crate::worldgen_lifecycle::LifecycleClientHeightmaps>,
    references: Option<BTreeMap<String, Vec<i64>>>,
    identity: &NetherGenerationIdentity,
) -> (Coordinate, AdmissionColumn, AdmissionMetadata) {
    let fingerprint = pristine_product_identity(
        GeneratedProducerIdentity::Nether(identity.clone()), coordinate, boundary, &column,
        NetherGenerationIdentity::SHAPED_VERSION,
        crate::production_worldgen_session::EXECUTOR_VERSION,
    );
    materialized_product_with_fingerprint(
        coordinate, boundary, column, client_heightmaps, references, Some(fingerprint),
    )
}

fn pristine_product_identity(
    producer: GeneratedProducerIdentity,
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: &crate::chunk::ChunkColumn,
    product_version: u32,
    executor_version: u32,
) -> StageIdentity {
    let pipeline = match &producer {
        GeneratedProducerIdentity::End(_) => END_PIPELINE,
        GeneratedProducerIdentity::Nether(_) => NETHER_PIPELINE,
    };
    StageIdentity::Generated(Arc::new(GeneratedStageIdentity {
        producer,
        coordinate,
        pipeline: pipeline.identity(PipelineOptions::ALL),
        boundary,
        carrier_target: match column.generation_stage() {
            crate::chunk::ChunkGenerationStage::Shaped => GenerationTarget::Shaped,
            crate::chunk::ChunkGenerationStage::Full => GenerationTarget::Full,
        },
        min_y: column.min_y,
        height: column.height,
        executor_version,
        product_version,
    }))
}

fn materialized_product_with_fingerprint(
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: crate::chunk::ChunkColumn,
    client_heightmaps: Option<crate::worldgen_lifecycle::LifecycleClientHeightmaps>,
    references: Option<BTreeMap<String, Vec<i64>>>,
    fingerprint: Option<StageIdentity>,
) -> (Coordinate, AdmissionColumn, AdmissionMetadata) {
    let metadata = AdmissionMetadata {
        boundary,
        content: Some(AdmissionContentMetadata {
            fingerprint: fingerprint.unwrap_or_else(|| {
                crate::production_worldgen_session::column_fingerprint(coordinate, boundary, &column).into()
            }),
            retained_bytes: column.memory_census().logical_total(),
        }),
        client_heightmaps,
        references,
    };
    (coordinate, AdmissionColumn::Materialized(column), metadata)
}

pub(crate) struct AdmissionJob {
    generator: Arc<OverworldGenerator>,
    coordinates: Vec<Coordinate>,
    admitted: Vec<Coordinate>,
    prefix_targets: Vec<Coordinate>,
    prefix_radius: i32,
}

pub(crate) struct AdmissionProducts {
    pub(crate) columns: Vec<GeneratedColumn>,
    pub(crate) references: Vec<(Coordinate, BTreeMap<String, Vec<i64>>)>,
    pub(crate) starts: Vec<(Coordinate, Vec<Arc<StructureStart>>)>,
}

impl AdmissionJob {
    pub(crate) fn new(
        generator: Arc<OverworldGenerator>,
        coordinates: &[Coordinate],
        lease_coordinates: &[Coordinate],
        prefix_targets: &[Coordinate],
        prefix_radius: i32,
    ) -> Self {
        let admitted = lease_coordinates.iter().chain(prefix_targets).copied()
            .collect::<BTreeSet<_>>().into_iter().collect();
        Self {
            generator,
            coordinates: coordinates.to_vec(),
            admitted,
            prefix_targets: prefix_targets.to_vec(),
            prefix_radius,
        }
    }

    pub(crate) fn run(self) -> AdmissionProducts {
        let lease = {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::Lease, self.admitted.len() as u32);
            self.generator.lease_batch(&self.admitted)
        };
        {
            let _timing = PhaseTimer::start(
                WorldgenTimingPhase::PreOre, self.prefix_targets.len() as u32,
            );
            #[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
            let tile_side = (crate::chunk::browser_worldgen_parallelism() > 1).then_some(4);
            #[cfg(not(all(target_arch = "wasm32", feature = "wasm-threads")))]
            let tile_side = None;
            let jobs = lease.pre_ore_region_work(&self.prefix_targets, self.prefix_radius, tile_side);
            let prepared = crate::run_worldgen_jobs(jobs, |job| {
                lease.prepare_pre_ore_region_detailed(job)
            });
            let mut totals = lodestone_worldgen::overworld::PreOreRegionPreparation::default();
            for work in prepared {
                totals.requested_slots += work.requested_slots;
                totals.initialized_slots += work.initialized_slots;
                totals.reused_slots += work.reused_slots;
                totals.batch_executions += work.batch_executions;
                totals.evaluated_prefixes += work.evaluated_prefixes;
            }
            for (phase, count) in [
                (WorldgenTimingPhase::PreOreSlots, totals.requested_slots),
                (WorldgenTimingPhase::PreOreInitializations, totals.initialized_slots),
                (WorldgenTimingPhase::PreOreReuses, totals.reused_slots),
                (WorldgenTimingPhase::PreOreBatches, totals.batch_executions),
                (WorldgenTimingPhase::PreOreEvaluatedPrefixes, totals.evaluated_prefixes),
            ] {
                crate::worldgen_progress::record_work(phase, count);
            }
        }
        let mut references = Vec::with_capacity(self.prefix_targets.len());
        let mut starts = Vec::with_capacity(self.prefix_targets.len());
        {
            let _timing = PhaseTimer::start(
                WorldgenTimingPhase::StructureContext, self.prefix_targets.len() as u32,
            );
            let mut origins = BTreeSet::new();
            for &(cx, cz) in &self.prefix_targets {
                let chunk_references = lease.structure_references(cx, cz);
                origins.extend(chunk_references.values().flatten().map(|packed| {
                    (*packed as u32 as i32, (*packed >> 32) as u32 as i32)
                }));
                references.push(((cx, cz), chunk_references));
                starts.push(((cx, cz), lease.structure_starts(cx, cz)));
            }
            for origin in origins {
                starts.push((origin, lease.structure_starts(origin.0, origin.1)));
            }
        }
        let columns = {
            let _timing = PhaseTimer::start(
                WorldgenTimingPhase::ShapedProducts, self.coordinates.len() as u32,
            );
            crate::run_worldgen_jobs(self.coordinates, |(cx, cz)| lease.column_shaped(cx, cz))
        };
        {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::Lease, self.admitted.len() as u32);
            drop(lease);
        }
        AdmissionProducts { columns, references, starts }
    }
}

pub(crate) use crate::owned_compute::check_cancellations;

#[cfg(test)]
mod identity_tests {
    use super::*;
    use crate::chunk::{ChunkColumn, ChunkGenerationStage, ChunkSource, EndChunkSource};
    use crate::worldgen_lifecycle::{LifecycleCompletion, LifecycleMaterializer, LifecycleWorldgenSource};
    use lodestone_data::block::Block;
    use lodestone_worldgen::stage_schedule::{ColumnStage, Dimension, GenerationTarget, StageKey, END};

    fn fingerprint(metadata: &AdmissionMetadata) -> StageIdentity {
        metadata.content.as_ref().expect("unchanged admission content").fingerprint.clone()
    }

    fn content_identity(coordinate: Coordinate, boundary: ColumnStage, column: &ChunkColumn) -> StageIdentity {
        crate::production_worldgen_session::column_fingerprint(coordinate, boundary, column).into()
    }

    #[test]
    fn pristine_end_product_identity_is_independent_and_domain_complete() {
        let first = crate::worldgen_data::end_chunk_source(42);
        let independent = crate::worldgen_data::end_chunk_source(42);
        let coordinates = [(0, 0), (1, -2)];
        let first = first.owned_admission_work(&coordinates).run();
        let independent = independent.owned_admission_work(&coordinates).run();
        let mut ledger = crate::chunk_store::GenerationLedger::new();
        ledger.admit(END_PIPELINE, &coordinates).unwrap();
        for ((coordinate, column, metadata), (_, other, other_metadata)) in first.columns.iter().zip(&independent.columns) {
            let AdmissionColumn::Materialized(column) = column else { panic!("End carrier") };
            let AdmissionColumn::Materialized(other) = other else { panic!("End carrier") };
            assert_eq!(fingerprint(metadata), fingerprint(other_metadata));
            assert_eq!(metadata.client_heightmaps, other_metadata.client_heightmaps);
            assert_eq!(metadata.content.as_ref().unwrap().retained_bytes, column.memory_census().logical_total());
            assert_eq!(
                crate::production_worldgen_session::column_fingerprint(*coordinate, metadata.boundary, column),
                crate::production_worldgen_session::column_fingerprint(*coordinate, metadata.boundary, other),
            );
            assert!(matches!(fingerprint(metadata), StageIdentity::Generated(_)));
            assert_ne!(fingerprint(metadata), content_identity(
                *coordinate, metadata.boundary, column,
            ));
            let make_session = |column: &ChunkColumn, identity: StageIdentity| {
                let mut session = crate::worldgen_session::GenerationSession::new(
                    crate::worldgen_session::GenerationRequest::new(
                        Dimension::End, *coordinate, GenerationTarget::Full, 0,
                    ),
                );
                session.import_aggregate_prefix(
                    *coordinate, metadata.boundary,
                    crate::worldgen_session::ImmutableProduct::new(
                        lodestone_worldgen::stage_schedule::ResourceKey::MaterializedWorld,
                        column.clone(),
                    ),
                    [], &identity, &identity,
                    crate::production_worldgen_session::EXECUTOR_VERSION,
                ).unwrap();
                session
            };
            let session = make_session(column, fingerprint(metadata));
            ledger.publish_session(END_PIPELINE, &session).unwrap();
            let same = make_session(other, fingerprint(other_metadata));
            ledger.publish_session(END_PIPELINE, &same).unwrap();
            let checkpoint = ledger.checkpoint(END_PIPELINE, session.request()).unwrap();
            let restored = crate::worldgen_session::GenerationSession::from_checkpoint(checkpoint).unwrap();
            ledger.publish_session(END_PIPELINE, &restored).unwrap();
            let StageIdentity::Generated(mut changed) = fingerprint(metadata) else { unreachable!() };
            Arc::make_mut(&mut changed).product_version += 1;
            let divergent = make_session(column, StageIdentity::Generated(changed));
            assert_eq!(ledger.publish_session(END_PIPELINE, &divergent),
                Err(crate::chunk_store::GenerationLedgerError::CheckpointMismatch));
        }
        assert_ne!(fingerprint(&first.columns[0].2), fingerprint(&first.columns[1].2));

        let generator = crate::worldgen_data::end_generator(42);
        let boundary = END.target_stage(GenerationTarget::Shaped);
        let column = EndChunkSource::new(generator).shaped_column(0, 0)
            .test_with_generation_stage(ChunkGenerationStage::Shaped);
        let generator = crate::worldgen_data::end_generator(42);
        let identity = generator.generation_identity().unwrap();
        let shaped_version = EndGenerationIdentity::SHAPED_VERSION;
        let executor_version = crate::production_worldgen_session::EXECUTOR_VERSION;
        let base = pristine_end_product((0, 0), boundary, column.clone(), None, identity);
        for (shaped, executor) in [
            (shaped_version + 1, executor_version),
            (shaped_version, executor_version + 1),
        ] {
            assert_ne!(fingerprint(&base.2), pristine_end_identity(
                identity, (0, 0), boundary, &column, shaped, executor,
            ));
        }
        let shifted = ChunkColumn::new(-16, 256)
            .test_with_generation_stage(ChunkGenerationStage::Shaped);
        let shorter = ChunkColumn::new(0, 128)
            .test_with_generation_stage(ChunkGenerationStage::Shaped);
        for (coordinate, boundary, column) in [
            ((-1, 0), boundary, column.clone()),
            ((0, 0), ColumnStage::Output, column.clone()),
            ((0, 0), boundary, shifted),
            ((0, 0), boundary, shorter),
            ((0, 0), boundary, column.test_with_generation_stage(ChunkGenerationStage::Full)),
        ] {
            assert_ne!(fingerprint(&base.2), pristine_end_identity(
                identity, coordinate, boundary, &column, shaped_version, executor_version,
            ));
        }
    }

    #[test]
    fn dynamic_end_admission_hashes_its_actual_content() {
        use lodestone_worldgen::density::{NoiseParams, Resolver};

        struct DynamicResolver;
        impl Resolver for DynamicResolver {
            fn density_function(&self, _id: &str) -> serde_json::Value { unreachable!("constant density") }
            fn noise(&self, _id: &str) -> NoiseParams {
                NoiseParams { first_octave: -6, amplitudes: vec![1.0] }
            }
        }
        let settings = serde_json::json!({
            "legacy_random_source": true,
            "noise": { "min_y": 0, "height": 128, "size_horizontal": 2, "size_vertical": 1 },
            "sea_level": 0,
            "default_block": { "Name": "minecraft:end_stone" },
            "default_fluid": { "Name": "minecraft:air" },
            "noise_router": { "final_density": 0.5, "preliminary_surface_level": 0.0 },
            "surface_rule": { "type": "minecraft:sequence", "sequence": [] }
        });
        let generator = lodestone_worldgen::end::EndGenerator::new(42, &settings, &DynamicResolver);
        assert!(generator.generation_identity().is_none());
        let products = EndChunkSource::new(generator).owned_admission_work(&[(0, 0)]).run();
        let (_, column, metadata) = &products.columns[0];
        let AdmissionColumn::Materialized(column) = column else { panic!("End carrier") };
        assert_eq!(fingerprint(metadata), content_identity(
            (0, 0), metadata.boundary, column,
        ));
        let mut changed = column.clone();
        changed.set_block_id(3, 201, 5, Block::GoldBlock.default_state());
        assert_ne!(fingerprint(metadata), content_identity(
            (0, 0), metadata.boundary, &changed,
        ));
    }

    #[test]
    fn authoritative_admission_keeps_exact_content_identity() {
        let source = crate::worldgen_data::end_chunk_source(42);
        let coordinate = (100, -100);
        let mut input = ChunkColumn::new(0, EndChunkSource::WINDOW_HEIGHT)
            .test_with_generation_stage(ChunkGenerationStage::Shaped);
        input.set_block_id(3, 201, 5, Block::GoldBlock.default_state());
        source.retain_generation_input(coordinate.0, coordinate.1, &input);
        let retained = source.owned_admission_work(&[coordinate]).run();
        assert_eq!(fingerprint(&retained.columns[0].2), content_identity(
            coordinate, retained.columns[0].2.boundary, &input,
        ));
        let previous = fingerprint(&retained.columns[0].2);
        source.set_block(coordinate.0 * 16 + 3, 201, coordinate.1 * 16 + 5, Block::DiamondBlock.default_state());
        let edited = source.owned_admission_work(&[coordinate]).run();
        let AdmissionColumn::Materialized(column) = &edited.columns[0].1 else { panic!("End carrier") };
        assert_eq!(fingerprint(&edited.columns[0].2), content_identity(
            coordinate, edited.columns[0].2.boundary, column,
        ));
        assert_ne!(previous, fingerprint(&edited.columns[0].2));

        let nether = crate::worldgen_data::nether_chunk_source(42);
        let pristine = nether.owned_admission_work(&[coordinate]).run();
        assert!(matches!(fingerprint(&pristine.columns[0].2), StageIdentity::Generated(_)));
        nether.retain_generation_input(coordinate.0, coordinate.1, &input);
        let retained = nether.owned_admission_work(&[coordinate]).run();
        assert_eq!(fingerprint(&retained.columns[0].2), content_identity(
            coordinate, retained.columns[0].2.boundary, &input,
        ));
        let independent = crate::worldgen_data::nether_chunk_source(42);
        independent.retain_generation_input(coordinate.0, coordinate.1, &input);
        let independent = independent.owned_admission_work(&[coordinate]).run();
        assert_eq!(fingerprint(&retained.columns[0].2), fingerprint(&independent.columns[0].2));
    }

    #[test]
    fn pristine_end_identity_revokes_for_foreign_and_restored_writes() {
        use crate::worldgen_session::{BlockCoordinate, ProvenanceMutation};
        use std::cell::Cell;

        let source = crate::worldgen_data::end_chunk_source(42);
        let source_chunk = (6, 0);
        let destination = (6, -1);
        let boundary = END.target_stage(GenerationTarget::Shaped);
        for restored in [false, true] {
            let mut materializer = LifecycleMaterializer::new(&source);
            materializer.admit_many_parallel(&[source_chunk, destination]);
            let original = materializer.shared_resident_prefix(destination, boundary, |_| {
                panic!("pristine End admission must carry its producer identity")
            }).unwrap();
            if restored {
                let mutation = ProvenanceMutation::test_block_state(
                    source_chunk, source_chunk, StageKey::new(Dimension::End, ColumnStage::Features),
                    0, BlockCoordinate::new(100, 201, -1), 1, Block::GoldBlock.default_state(),
                );
                materializer.restore_committed_mutations([&mutation]);
            } else {
                materializer.complete(source_chunk, LifecycleCompletion::Features, 0);
            }
            let scanned = Cell::new(false);
            let changed = materializer.shared_resident_prefix(destination, boundary, |column| {
                scanned.set(true);
                crate::production_worldgen_session::column_fingerprint(destination, boundary, column)
            }).unwrap();
            assert!(scanned.get(), "foreign/restored writes must revoke pristine content identity");
            assert_ne!(original.1, changed.1);
            assert_eq!(changed.0.block_state_id(4, if restored { 201 } else { 48 }, 15),
                if restored { Block::GoldBlock.default_state() } else { Block::Obsidian.default_state() });
            assert_eq!(changed.1, content_identity(destination, boundary, &changed.0));
        }
    }

    #[test]
    fn end_pristine_admission_retries_after_input_replacement() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ReplacingSource {
            source: Arc<EndChunkSource>,
            attempts: Arc<AtomicUsize>,
        }

        impl LifecycleWorldgenSource for ReplacingSource {
            type ReplayContext = ();
            fn lifecycle_replay_context(&self, _target: Coordinate) -> Arc<()> { Arc::new(()) }
            fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn { self.source.shaped_column(cx, cz) }
            fn immutable_admission_versions(&self, chunks: &[Coordinate]) -> Vec<Option<u64>> {
                self.source.immutable_admission_versions(chunks)
            }
            fn owned_admission_work(
                &self, chunks: &[Coordinate], _lease: &[Coordinate], _targets: &[Coordinate], _radius: i32,
            ) -> Option<OwnedAdmissionWork> {
                let work = self.source.owned_admission_work(chunks);
                let source = Arc::clone(&self.source);
                let attempts = Arc::clone(&self.attempts);
                let coordinate = chunks[0];
                Some(OwnedAdmissionWork::new(move || {
                    let products = work.run();
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        let mut replacement = ChunkColumn::new(0, EndChunkSource::WINDOW_HEIGHT)
                            .test_with_generation_stage(ChunkGenerationStage::Shaped);
                        replacement.set_block_id(3, 201, 5, Block::GoldBlock.default_state());
                        source.retain_generation_input(coordinate.0, coordinate.1, &replacement);
                    }
                    products
                }))
            }
            fn feature_result(
                &self, _source: Coordinate,
                _overrides: &BTreeMap<(i32, i32, i32), lodestone_data::block_states::StateId>,
                _resident: &BTreeMap<Coordinate, ChunkColumn>,
            ) -> crate::worldgen_lifecycle::LifecycleFeatureResult { unreachable!("admission-only control") }
        }

        let source = ReplacingSource {
            source: Arc::new(crate::worldgen_data::end_chunk_source(42)),
            attempts: Arc::new(AtomicUsize::new(0)),
        };
        let mut materializer = LifecycleMaterializer::new(&source);
        materializer.admit_many_parallel(&[(0, 0)]);
        assert_eq!(source.attempts.load(Ordering::SeqCst), 2);
        let column = materializer.resident_column((0, 0)).unwrap();
        assert_eq!(column.block_state_id(3, 201, 5), Block::GoldBlock.default_state());
        let boundary = END.target_stage(GenerationTarget::Shaped);
        let prefix = materializer.shared_resident_prefix((0, 0), boundary, |_| {
            panic!("replacement metadata must be accepted after retry")
        }).unwrap();
        assert_eq!(prefix.1, content_identity((0, 0), boundary, &prefix.0));
    }
}
