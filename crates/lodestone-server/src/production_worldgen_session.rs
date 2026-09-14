//! Shared production execution for request-scoped world generation.

use std::marker::PhantomData;
use std::mem::size_of;

use sha2::{Digest, Sha256};

use crate::chunk::{ChunkColumn, ChunkGenerationStage};
use crate::worldgen_lifecycle::{
    ImmutableComputeExecutor, LifecycleCompletion, LifecycleMaterializer, LifecycleSpill,
    LifecycleWorldgenSource, PersistentWorldgenExecutor,
};
use crate::worldgen_session::{
    BlockCoordinate, GenerationRequest, GenerationSession, ImmutableProduct, ImmutableSidecar,
    PacketSnapshot, RequestStageDriver, SessionError,
};
use lodestone_worldgen::stage_schedule::{
    ChunkRequest, ColumnStage, Dimension, GenerationTarget, ResourceKey, SidecarKey, SourceSchedule,
    StageKey, END_SOURCES, NETHER_SOURCES, OVERWORLD_SOURCES,
};
use lodestone_worldgen::structure::StructureBlocks;

const EXECUTOR_VERSION: u32 = 1;

pub(crate) trait DimensionPolicy<S: LifecycleWorldgenSource> {
    const DIMENSION: Dimension;

    fn source_schedule() -> SourceSchedule;

    fn prefix_sidecars(
        source: &S,
        coordinate: (i32, i32),
    ) -> Vec<(StageKey, ImmutableSidecar)>;

    fn has_top_layer() -> bool;
}

struct OverworldPolicy;
struct NetherPolicy;
struct EndPolicy;

impl DimensionPolicy<crate::chunk::OverworldChunkSource> for OverworldPolicy {
    const DIMENSION: Dimension = Dimension::Overworld;

    fn source_schedule() -> SourceSchedule {
        OVERWORLD_SOURCES
    }

    fn prefix_sidecars(
        source: &crate::chunk::OverworldChunkSource,
        coordinate: (i32, i32),
    ) -> Vec<(StageKey, ImmutableSidecar)> {
        vec![
            (
                StageKey::new(Self::DIMENSION, ColumnStage::StructureReferences),
                ImmutableSidecar::new(
                    SidecarKey::StructureReferences,
                    source.generator().structure_references(coordinate.0, coordinate.1),
                ),
            ),
        ]
    }

    fn has_top_layer() -> bool {
        true
    }
}

impl DimensionPolicy<crate::chunk::NetherChunkSource> for NetherPolicy {
    const DIMENSION: Dimension = Dimension::Nether;

    fn source_schedule() -> SourceSchedule {
        NETHER_SOURCES
    }

    fn prefix_sidecars(
        source: &crate::chunk::NetherChunkSource,
        coordinate: (i32, i32),
    ) -> Vec<(StageKey, ImmutableSidecar)> {
        vec![
            (
                StageKey::new(Self::DIMENSION, ColumnStage::StructureReferences),
                ImmutableSidecar::new(
                    SidecarKey::StructureReferences,
                    source.generator().structure_references(coordinate.0, coordinate.1),
                ),
            ),
        ]
    }

    fn has_top_layer() -> bool {
        false
    }
}

impl DimensionPolicy<crate::chunk::EndChunkSource> for EndPolicy {
    const DIMENSION: Dimension = Dimension::End;

    fn source_schedule() -> SourceSchedule {
        END_SOURCES
    }

    fn prefix_sidecars(
        _source: &crate::chunk::EndChunkSource,
        _coordinate: (i32, i32),
    ) -> Vec<(StageKey, ImmutableSidecar)> {
        Vec::new()
    }

    fn has_top_layer() -> bool {
        false
    }
}

fn column_fingerprint(
    coordinate: (i32, i32),
    boundary: ColumnStage,
    column: &ChunkColumn,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"lodestone-shaped-prefix-v1");
    digest.update(coordinate.0.to_le_bytes());
    digest.update(coordinate.1.to_le_bytes());
    digest.update([boundary as u8]);
    digest.update(column.min_y.to_le_bytes());
    digest.update(column.height.to_le_bytes());
    digest.update([column.generation_stage() as u8]);
    for id in column.palette_state_ids() {
        digest.update(id.raw().to_le_bytes());
    }
    for section in 0..column.section_count() {
        column.for_each_section_palette_index(section, |cell, palette_id| {
            digest.update((cell as u16).to_le_bytes());
            digest.update(palette_id.to_le_bytes());
        });
    }
    for biome in column.biome_quarts() {
        digest.update(biome.as_bytes());
        digest.update([0]);
    }
    digest.finalize().into()
}

fn feature_sidecars(
    dimension: Dimension,
    spills: &[LifecycleSpill],
    column: &ChunkColumn,
) -> Vec<ImmutableSidecar> {
    let mut sidecars = vec![
        ImmutableSidecar::new(SidecarKey::DecorationSpills, spills.to_vec()),
        ImmutableSidecar::new(
            SidecarKey::ClientHeightmaps,
            column
                .client_heightmaps_raw()
                .expect("FEATURES must retain client heightmaps"),
        ),
    ];
    if dimension == Dimension::End {
        let entities = column.block_entities().to_vec();
        let gateways = entities
            .iter()
            .filter(|(_, entity)| entity.type_id() == "minecraft:end_gateway")
            .cloned()
            .collect::<Vec<_>>();
        sidecars.push(ImmutableSidecar::new(SidecarKey::Gateways, gateways));
        sidecars.push(ImmutableSidecar::new(
            SidecarKey::BlockEntityEvents,
            entities,
        ));
    }
    sidecars
}

fn structure_blocks_retained_bytes(value: &StructureBlocks) -> usize {
    size_of::<StructureBlocks>()
        + value.mutations().len()
            * size_of::<lodestone_worldgen::structure::StructureBlockMutation>()
        + value
            .mutations()
            .iter()
            .map(|mutation| mutation.state.capacity())
            .sum::<usize>()
        + value.loot().len() * size_of::<lodestone_worldgen::structure::StructureLoot>()
}

fn source_order<S, P>(request: GenerationRequest) -> Vec<(i32, i32)>
where
    S: LifecycleWorldgenSource + Sync,
    P: DimensionPolicy<S>,
{
    let target = request.target();
    let request = ChunkRequest::single(
        target.0,
        target.1,
        i32::from(request.dependency_radius()),
    );
    P::source_schedule()
        .order_for(request, target)
        .into_iter()
        .map(|(dx, dz)| (target.0 + dx, target.1 + dz))
        .collect()
}

fn import_shaped_prefixes<S, P>(
    source: &S,
    session: &mut GenerationSession,
    materializer: &LifecycleMaterializer<&S>,
) -> Result<(), SessionError>
where
    S: LifecycleWorldgenSource + Sync,
    P: DimensionPolicy<S>,
{
    let boundary = session
        .pipeline()
        .schedule()
        .target_stage(GenerationTarget::Shaped);
    let admissions = session.admission_order().to_vec();
    for coordinate in admissions {
        if shaped_column_from_session(session, coordinate).is_some() {
            continue;
        }
        let column = materializer
            .resident_column(coordinate)
            .cloned()
            .expect("parallel shaped admission must retain every resident");
        let fingerprint = column_fingerprint(coordinate, boundary, &column);
        let retained_bytes = column.memory_census().logical_total();
        let product = ImmutableProduct::new_with_retained_bytes(
            ResourceKey::MaterializedWorld,
            column,
            retained_bytes,
        );
        session.import_aggregate_prefix(
            coordinate,
            boundary,
            product,
            P::prefix_sidecars(source, coordinate),
            fingerprint,
            fingerprint,
            EXECUTOR_VERSION,
        )?;
    }
    Ok(())
}

fn shaped_column_from_session(
    session: &GenerationSession,
    coordinate: (i32, i32),
) -> Option<std::sync::Arc<ChunkColumn>> {
    session
        .aggregate_prefix(coordinate)
        .or_else(|| {
            session
                .resident_read(coordinate)
                .ok()
                .and_then(|resident| resident.product(ResourceKey::MaterializedWorld))
        })
}

fn commit_features<S>(
    session: &mut GenerationSession,
    materializer: &mut LifecycleMaterializer<&S>,
    sources: &[(i32, i32)],
    spills: &[LifecycleSpill],
    structure_blocks: StructureBlocks,
) -> Result<(), SessionError>
where
    S: LifecycleWorldgenSource + Sync,
{
    let target = session.request().target();
    let key = StageKey::new(session.pipeline().dimension(), ColumnStage::Features);
    let already_committed = session
        .frontier(target)
        .is_some_and(|frontier| frontier.records().iter().any(|record| record.key() == key));
    if already_committed {
        return Ok(());
    }
    session.declare_mutable_sources(
        key,
        sources
            .iter()
            .copied()
            .enumerate()
            .map(|(order, source)| (order as u64, source)),
    )?;
    for (source_order, &source) in sources.iter().enumerate() {
        if session.source_order_committed(source_order as u64) {
            continue;
        }
        let mut transaction = session.begin_mutable_source(source, key, source_order as u64)?;
        for (ordinal, spill) in spills
            .iter()
            .filter(|spill| spill.source == source)
            .enumerate()
        {
            transaction.push(
                ordinal as u32,
                BlockCoordinate::new(
                    spill.position.0,
                    spill.position.1,
                    spill.position.2,
                ),
                spill.state.clone(),
            )?;
        }
        session.complete_mutable_source(transaction)?;
    }
    let column = materializer
        .resident_column(target)
        .cloned()
        .expect("target was admitted before FEATURES");
    let fingerprint = column_fingerprint(target, ColumnStage::Features, &column);
    let retained_bytes = column.memory_census().logical_total();
    let mut products = vec![ImmutableProduct::new_with_retained_bytes(
        ResourceKey::ResidentOverlay,
        column.clone(),
        retained_bytes,
    )];
    if session
        .pipeline()
        .descriptor(ColumnStage::Features)
        .is_some_and(|descriptor| descriptor.outputs().contains(&ResourceKey::StructureBlocks))
    {
        let retained_bytes = structure_blocks_retained_bytes(&structure_blocks);
        products.push(ImmutableProduct::new_with_retained_bytes(
            ResourceKey::StructureBlocks,
            structure_blocks,
            retained_bytes,
        ));
    }
    session.commit_mutable_stage(
        key,
        fingerprint,
        fingerprint,
        EXECUTOR_VERSION,
        products,
        feature_sidecars(session.pipeline().dimension(), spills, &column),
    )
}

fn commit_top_layer<S>(
    session: &mut GenerationSession,
    materializer: &mut LifecycleMaterializer<&S>,
    spills: &[LifecycleSpill],
) -> Result<(), SessionError>
where
    S: LifecycleWorldgenSource + Sync,
{
    let target = session.request().target();
    let key = StageKey::new(session.pipeline().dimension(), ColumnStage::TopLayer);
    if session
        .frontier(target)
        .is_some_and(|frontier| frontier.records().iter().any(|record| record.key() == key))
    {
        return Ok(());
    }
    session.declare_mutable_sources(key, [(0, target)])?;
    if !session.source_order_committed(0) {
        let mut transaction = session.begin_mutable_source(target, key, 0)?;
        for (ordinal, spill) in spills.iter().enumerate() {
            transaction.push(
                ordinal as u32,
                BlockCoordinate::new(spill.position.0, spill.position.1, spill.position.2),
                spill.state.clone(),
            )?;
        }
        session.complete_mutable_source(transaction)?;
    }
    let column = materializer
        .resident_column(target)
        .cloned()
        .expect("target was admitted before top layer");
    let fingerprint = column_fingerprint(target, ColumnStage::TopLayer, &column);
    let retained_bytes = column.memory_census().logical_total();
    session.commit_mutable_stage(
        key,
        fingerprint,
        fingerprint,
        EXECUTOR_VERSION,
        vec![ImmutableProduct::new_with_retained_bytes(
            ResourceKey::ResidentRegion,
            column,
            retained_bytes,
        )],
        Vec::new(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenerationPhase {
    Admission,
    ImportShaped,
    ResumeOutput,
    FeatureSource(usize),
    FinishFeatures,
    CommitFeatures,
    TopLayer,
    CommitTopLayer,
    Output,
    PacketNeighbours,
    SettleLight,
    Finalize,
}

struct GenerationStateMachine<'a, S, P>
where
    S: LifecycleWorldgenSource + Sync,
    P: DimensionPolicy<S>,
{
    source: &'a S,
    session: &'a mut GenerationSession,
    materializer: LifecycleMaterializer<&'a S>,
    admissions: Vec<(i32, i32)>,
    missing_admissions: Vec<(i32, i32)>,
    sources: Vec<(i32, i32)>,
    feature_spills: Vec<LifecycleSpill>,
    top_spills: Vec<LifecycleSpill>,
    output: Option<ChunkColumn>,
    packet_neighbours: Vec<((i32, i32), ChunkColumn)>,
    phase: GenerationPhase,
    policy: PhantomData<P>,
}

impl<'a, S, P> GenerationStateMachine<'a, S, P>
where
    S: LifecycleWorldgenSource + Sync,
    P: DimensionPolicy<S>,
{
    fn new(
        source: &'a S,
        session: &'a mut GenerationSession,
    ) -> Result<Self, SessionError> {
        if session.cancellation().is_cancelled() {
            return Err(SessionError::Cancelled);
        }
        if session.request().dimension() != P::DIMENSION
            || session.request().generation_target() != GenerationTarget::Full
        {
            return Err(SessionError::CheckpointPipelineMismatch);
        }
        let admissions = session.admission_order().to_vec();
        let missing_admissions = admissions
            .iter()
            .copied()
            .filter(|coordinate| shaped_column_from_session(session, *coordinate).is_none())
            .collect();
        let sources = source_order::<S, P>(session.request());
        Ok(Self {
            source,
            session,
            materializer: LifecycleMaterializer::new(source),
            admissions,
            missing_admissions,
            sources,
            feature_spills: Vec::new(),
            top_spills: Vec::new(),
            output: None,
            packet_neighbours: Vec::new(),
            phase: GenerationPhase::Admission,
            policy: PhantomData,
        })
    }

    fn step(
        &mut self,
        executor: &dyn ImmutableComputeExecutor,
    ) -> Result<Option<PacketSnapshot>, SessionError> {
        match self.phase {
            GenerationPhase::Admission => {
                for &coordinate in &self.admissions {
                    if let Some(column) = shaped_column_from_session(self.session, coordinate) {
                        self.materializer.admit_existing(coordinate, (*column).clone());
                    }
                }
                self.materializer
                    .admit_many_parallel_with(&self.missing_admissions, executor);
                self.phase = GenerationPhase::ImportShaped;
            }
            GenerationPhase::ImportShaped => {
                import_shaped_prefixes::<S, P>(self.source, self.session, &self.materializer)?;
                crate::worldgen_progress::emit(crate::worldgen_progress::WorldgenProgress {
                    session: self.session.id().value(),
                    admitted: self.admissions.len() as u32,
                    completed: 0,
                    committed: 0,
                    queued: 0,
                    retained_bytes: self.session.usage().retained_bytes(),
                    stage: "admission-complete",
                });
                self.phase = GenerationPhase::ResumeOutput;
            }
            GenerationPhase::ResumeOutput => {
                if self.session.cancellation().is_cancelled() {
                    return Err(SessionError::Cancelled);
                }
                self.materializer
                    .restore_committed_mutations(self.session.committed_mutations());
                let output = self
                    .session
                    .resident_read(self.session.request().target())?
                    .product::<ChunkColumn>(ResourceKey::OutputColumn);
                if let Some(output) = output {
                    self.output = Some((*output).clone());
                    self.phase = GenerationPhase::PacketNeighbours;
                } else {
                    self.phase = GenerationPhase::FeatureSource(0);
                }
            }
            GenerationPhase::FeatureSource(sequence) => {
                if self.session.cancellation().is_cancelled() {
                    return Err(SessionError::Cancelled);
                }
                if sequence == self.sources.len() {
                    self.phase = GenerationPhase::FinishFeatures;
                } else {
                    let source = self.sources[sequence];
                    let mut spills = Vec::new();
                    self.materializer.complete_for_target_observing(
                        self.session.request().target(),
                        source,
                        LifecycleCompletion::Features,
                        sequence as u64,
                        |spill| spills.push(spill.clone()),
                    );
                    self.feature_spills.extend(spills);
                    crate::worldgen_progress::emit(crate::worldgen_progress::WorldgenProgress {
                        session: self.session.id().value(),
                        admitted: self.admissions.len() as u32,
                        completed: (sequence + 1) as u32,
                        committed: 0,
                        queued: (self.sources.len() - sequence - 1) as u32,
                        retained_bytes: self.session.usage().retained_bytes(),
                        stage: "mutable-source-complete",
                    });
                    self.phase = GenerationPhase::FeatureSource(sequence + 1);
                }
            }
            GenerationPhase::FinishFeatures => {
                self.materializer.finish_target(self.session.request().target());
                if self.session.cancellation().is_cancelled() {
                    return Err(SessionError::Cancelled);
                }
                self.phase = GenerationPhase::CommitFeatures;
            }
            GenerationPhase::CommitFeatures => {
                let structure_blocks = self.materializer.take_feature_structure_blocks();
                commit_features(
                    self.session,
                    &mut self.materializer,
                    &self.sources,
                    &self.feature_spills,
                    structure_blocks,
                )?;
                crate::worldgen_progress::emit(crate::worldgen_progress::WorldgenProgress {
                    session: self.session.id().value(),
                    admitted: self.admissions.len() as u32,
                    completed: self.sources.len() as u32,
                    committed: 1,
                    queued: 0,
                    retained_bytes: self.session.usage().retained_bytes(),
                    stage: "features-committed",
                });
                self.phase = if P::has_top_layer() {
                    GenerationPhase::TopLayer
                } else {
                    GenerationPhase::Output
                };
            }
            GenerationPhase::TopLayer => {
                if self.session.cancellation().is_cancelled() {
                    return Err(SessionError::Cancelled);
                }
                let mut spills = Vec::new();
                self.materializer.complete_target_post_features(
                    self.session.request().target(),
                    |spill| spills.push(spill.clone()),
                );
                self.top_spills = spills;
                self.phase = GenerationPhase::CommitTopLayer;
            }
            GenerationPhase::CommitTopLayer => {
                commit_top_layer(self.session, &mut self.materializer, &self.top_spills)?;
                self.phase = GenerationPhase::Output;
            }
            GenerationPhase::Output => {
                if self.session.cancellation().is_cancelled() {
                    return Err(SessionError::Cancelled);
                }
                let target = self.session.request().target();
                let mut output = self.materializer.snapshot_for_packet(target);
                output.mark_generation_stage(ChunkGenerationStage::Full);
                let output_key = StageKey::new(self.session.pipeline().dimension(), ColumnStage::Output);
                let fingerprint = column_fingerprint(target, ColumnStage::Output, &output);
                let maps = output
                    .client_heightmaps_raw()
                    .expect("packet output must retain client heightmaps");
                let retained_bytes = output.memory_census().logical_total();
                let mut sidecars = vec![ImmutableSidecar::new(SidecarKey::ClientHeightmaps, maps)];
                if self.session.pipeline().dimension() == Dimension::End {
                    let entities = output.block_entities().to_vec();
                    let gateways = entities
                        .iter()
                        .filter(|(_, entity)| entity.type_id() == "minecraft:end_gateway")
                        .cloned()
                        .collect::<Vec<_>>();
                    sidecars.push(ImmutableSidecar::new(SidecarKey::BlockEntityEvents, entities));
                    sidecars.push(ImmutableSidecar::new(SidecarKey::Gateways, gateways));
                }
                self.session.complete_immutable(crate::worldgen_session::ImmutableStageCompletion::new(
                    target,
                    output_key,
                    fingerprint,
                    fingerprint,
                    EXECUTOR_VERSION,
                    vec![ImmutableProduct::new_with_retained_bytes(
                        ResourceKey::OutputColumn,
                        output.clone(),
                        retained_bytes,
                    )],
                    sidecars,
                ))?;
                self.session.advance_ready_immutable()?;
                self.output = Some(output);
                self.phase = GenerationPhase::PacketNeighbours;
            }
            GenerationPhase::PacketNeighbours => {
                if self.session.cancellation().is_cancelled() {
                    return Err(SessionError::Cancelled);
                }
                let target = self.session.request().target();
                let neighbours = self
                    .session
                    .admission_order()
                    .iter()
                    .copied()
                    .filter(|coordinate| {
                        *coordinate != target
                            && (coordinate.0 - target.0).abs() <= 1
                            && (coordinate.1 - target.1).abs() <= 1
                    })
                    .collect::<Vec<_>>();
                self.session.declare_packet_neighbours(neighbours.iter().copied())?;
                self.packet_neighbours = neighbours
                    .into_iter()
                    .map(|coordinate| {
                        self.materializer
                            .resident_column(coordinate)
                            .cloned()
                            .map(|column| (coordinate, column))
                            .ok_or(SessionError::PacketNeighbourNotReady { coordinate })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.phase = GenerationPhase::SettleLight;
            }
            GenerationPhase::SettleLight => {
                self.session.complete_light_domain()?;
                self.phase = GenerationPhase::Finalize;
            }
            GenerationPhase::Finalize => {
                if self.session.cancellation().is_cancelled() {
                    return Err(SessionError::Cancelled);
                }
                let output = self.output.take().expect("output phase retains packet column");
                let neighbours = std::mem::take(&mut self.packet_neighbours);
                let snapshot = self
                    .session
                    .finalize_packet_snapshot_with_dependencies(output, neighbours)?;
                crate::worldgen_progress::emit(crate::worldgen_progress::WorldgenProgress {
                    session: self.session.id().value(),
                    admitted: self.admissions.len() as u32,
                    completed: self.sources.len() as u32,
                    committed: 2,
                    queued: 0,
                    retained_bytes: self.session.usage().retained_bytes(),
                    stage: "packet-snapshot",
                });
                return Ok(Some(snapshot));
            }
        }
        Ok(None)
    }
}

pub(crate) fn generate_request_with_executor<S, P>(
    source: &S,
    session: &mut GenerationSession,
    executor: &dyn ImmutableComputeExecutor,
) -> Result<PacketSnapshot, SessionError>
where
    S: LifecycleWorldgenSource + Sync,
    P: DimensionPolicy<S>,
{
    let mut machine = GenerationStateMachine::<S, P>::new(source, session)?;
    loop {
        if let Some(snapshot) = machine.step(executor)? {
            return Ok(snapshot);
        }
    }
}

#[cfg(target_arch = "wasm32")]
struct SerialImmutableExecutor;

#[cfg(target_arch = "wasm32")]
impl ImmutableComputeExecutor for SerialImmutableExecutor {
    fn execute_shaped(
        &self,
        jobs: Vec<(i32, i32)>,
        work: &(dyn Fn((i32, i32)) -> ChunkColumn + Send + Sync),
    ) -> Vec<ChunkColumn> {
        jobs.into_iter().map(work).collect()
    }
}

#[cfg(target_arch = "wasm32")]
async fn generate_request_yielding<S, P>(
    source: &S,
    session: &mut GenerationSession,
) -> Result<PacketSnapshot, SessionError>
where
    S: LifecycleWorldgenSource + Sync,
    P: DimensionPolicy<S>,
{
    let mut machine = GenerationStateMachine::<S, P>::new(source, session)?;
    let executor = SerialImmutableExecutor;
    loop {
        if let Some(snapshot) = machine.step(&executor)? {
            return Ok(snapshot);
        }
        crate::chunk::yield_to_browser().await;
    }
}

fn generate_request<S, P>(
    source: &S,
    session: &mut GenerationSession,
) -> Result<PacketSnapshot, SessionError>
where
    S: LifecycleWorldgenSource + Sync,
    P: DimensionPolicy<S>,
{
    generate_request_with_executor::<S, P>(source, session, &PersistentWorldgenExecutor)
}

impl RequestStageDriver for crate::chunk::OverworldChunkSource {
    fn generate(&self, session: &mut GenerationSession) -> Result<PacketSnapshot, SessionError> {
        generate_request::<Self, OverworldPolicy>(self, session)
    }

    #[cfg(target_arch = "wasm32")]
    fn generate_yielding<'a>(
        &'a self,
        session: &'a mut GenerationSession,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<PacketSnapshot, SessionError>> + 'a>,
    > {
        Box::pin(generate_request_yielding::<Self, OverworldPolicy>(self, session))
    }

    fn generate_with_executor(
        &self,
        session: &mut GenerationSession,
        executor: &dyn ImmutableComputeExecutor,
    ) -> Result<PacketSnapshot, SessionError> {
        generate_request_with_executor::<Self, OverworldPolicy>(self, session, executor)
    }
}

impl RequestStageDriver for crate::chunk::NetherChunkSource {
    fn generate(&self, session: &mut GenerationSession) -> Result<PacketSnapshot, SessionError> {
        generate_request::<Self, NetherPolicy>(self, session)
    }

    #[cfg(target_arch = "wasm32")]
    fn generate_yielding<'a>(
        &'a self,
        session: &'a mut GenerationSession,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<PacketSnapshot, SessionError>> + 'a>,
    > {
        Box::pin(generate_request_yielding::<Self, NetherPolicy>(self, session))
    }

    fn generate_with_executor(
        &self,
        session: &mut GenerationSession,
        executor: &dyn ImmutableComputeExecutor,
    ) -> Result<PacketSnapshot, SessionError> {
        generate_request_with_executor::<Self, NetherPolicy>(self, session, executor)
    }
}

impl RequestStageDriver for crate::chunk::EndChunkSource {
    fn generate(&self, session: &mut GenerationSession) -> Result<PacketSnapshot, SessionError> {
        generate_request::<Self, EndPolicy>(self, session)
    }

    #[cfg(target_arch = "wasm32")]
    fn generate_yielding<'a>(
        &'a self,
        session: &'a mut GenerationSession,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<PacketSnapshot, SessionError>> + 'a>,
    > {
        Box::pin(generate_request_yielding::<Self, EndPolicy>(self, session))
    }

    fn generate_with_executor(
        &self,
        session: &mut GenerationSession,
        executor: &dyn ImmutableComputeExecutor,
    ) -> Result<PacketSnapshot, SessionError> {
        generate_request_with_executor::<Self, EndPolicy>(self, session, executor)
    }
}
