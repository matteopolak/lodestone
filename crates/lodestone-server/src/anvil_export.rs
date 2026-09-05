//! One typed native chunk record to an Anvil-compatible NBT chunk tree.
//!
//! The native record has one ordering datum that the Anvil chunk schema cannot
//! carry: the scheduler's global insertion sequence. Callers must explicitly
//! acknowledge that loss before exporting pending ticks; every other field in
//! [`crate::world_storage::NativeChunkRecord`] has a direct representation in
//! the emitted tree.

use lodestone_core::Nbt;
use lodestone_world::{Heightmap, LightData};

use crate::{
    chunk_nbt::{ChunkExtras, SavedTick},
    scheduled_tick::PersistedScheduledTick,
    world_storage::NativeChunkRecord,
};

const LIGHT_ARRAY_BYTES: usize = 2048;

/// Which native tick queue owns an unrepresentable insertion-order value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TickQueue {
    /// The block-tick queue.
    Block,
    /// The fluid-tick queue.
    Fluid,
}

/// One native record feature that an Anvil chunk tree cannot retain.
///
/// This is metadata only: it deliberately does not retain a tick's position,
/// kind, or timing payload outside the typed input record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnsupportedNativeFeature {
    /// Anvil retains a tick's list order but has no field for the native
    /// scheduler's world-wide insertion sequence.
    TickInsertionOrder {
        /// Queue containing the affected ticks.
        queue: TickQueue,
        /// Number of sequence values that will be discarded.
        ticks: usize,
    },
}

/// Payload-free inventory of native values an Anvil chunk cannot represent.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChunkExportReport {
    unsupported: Vec<UnsupportedNativeFeature>,
}

impl ChunkExportReport {
    /// Values that require a caller's explicit loss acknowledgement.
    #[must_use]
    pub fn unsupported(&self) -> &[UnsupportedNativeFeature] {
        &self.unsupported
    }

    /// Applies the caller's export decision to this report.
    #[must_use]
    pub fn decide(&self, decision: ExportLossDecision) -> ExportAuthorization {
        match decision {
            ExportLossDecision::Abort => ExportAuthorization::Aborted,
            ExportLossDecision::ProceedAndDiscardUnsupported if self.unsupported.is_empty() => {
                ExportAuthorization::Lossless
            }
            ExportLossDecision::ProceedAndDiscardUnsupported => ExportAuthorization::LossAccepted {
                discarded_features: self.unsupported.len(),
            },
        }
    }
}

/// A caller's decision after inspecting [`ChunkExportReport`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportLossDecision {
    /// Do not emit a chunk.
    Abort,
    /// Emit the chunk while discarding every reported native-only value.
    ProceedAndDiscardUnsupported,
}

/// An authorization bound to one particular native chunk export report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "pass this result to export_chunk; native-only fields must not be discarded implicitly"]
pub enum ExportAuthorization {
    /// The caller declined the export.
    Aborted,
    /// The record is fully representable by the Anvil chunk schema.
    Lossless,
    /// The caller accepted the stated count of discarded native-only features.
    LossAccepted {
        /// Number of report entries the caller accepted.
        discarded_features: usize,
    },
}

impl ExportAuthorization {
    fn permits_export(self) -> bool {
        matches!(self, Self::Lossless | Self::LossAccepted { .. })
    }
}

/// One successfully exported Anvil chunk tree and its reviewed loss report.
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkExportResult {
    /// The complete, unnamed chunk NBT root for the selected Anvil region slot.
    pub chunk: Nbt,
    /// The report whose matching authorization permitted this export.
    pub report: ChunkExportReport,
}

/// An error preventing a native chunk from becoming an Anvil chunk tree.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Native-only data must be reviewed before an export can begin.
    #[error("Anvil export requires an explicit ExportAuthorization")]
    MissingAuthorization,
    /// The caller explicitly declined the export.
    #[error("Anvil export authorization does not permit conversion: {authorization:?}")]
    AuthorizationDenied {
        /// The decision supplied by the caller.
        authorization: ExportAuthorization,
    },
    /// The supplied authorization belongs to a different report or no longer
    /// matches the native record's present features.
    #[error(
        "Anvil export authorization does not match this native record: supplied {supplied:?}, required {required:?}"
    )]
    AuthorizationMismatch {
        /// The decision supplied by the caller.
        supplied: ExportAuthorization,
        /// The decision required by the record's current report.
        required: ExportAuthorization,
    },
    /// The typed column does not describe a positive Anvil height window.
    #[error("native chunk height {height} is not positive")]
    InvalidHeight {
        /// Native column height.
        height: i32,
    },
    /// A native motion-blocking height does not fit the chunk height's packed
    /// Anvil heightmap representation.
    #[error("native motion-blocking height {value} at index {index} exceeds chunk height {height}")]
    HeightmapValue {
        /// Flat local XZ index.
        index: usize,
        /// Native stored height.
        value: u16,
        /// Chunk's height window.
        height: u32,
    },
    /// A manually assembled light record contains a value outside the four-bit
    /// Anvil light range.
    #[error("native {kind} light section {section} has invalid uniform value {value}")]
    InvalidUniformLight {
        /// Light-section index, including the two boundary sections.
        section: usize,
        /// Light layer name.
        kind: &'static str,
        /// Invalid value.
        value: u8,
    },
    /// A tick's absolute native trigger cannot become Anvil's signed relative
    /// delay at the selected export time.
    #[error(
        "native {queue:?} tick {index} at trigger {trigger_tick} cannot be represented as an i32 delay from game time {game_time}"
    )]
    TickDelayOutOfRange {
        /// Queue containing the rejected tick.
        queue: TickQueue,
        /// Position in that queue's stored order.
        index: usize,
        /// Native absolute trigger tick.
        trigger_tick: u64,
        /// Game time supplied for this export.
        game_time: u64,
    },
}

/// Inventories the native-only values in one complete typed chunk record.
#[must_use]
pub fn preflight_chunk(record: &NativeChunkRecord) -> ChunkExportReport {
    let mut unsupported = Vec::new();
    for (queue, ticks) in [
        (TickQueue::Block, &record.block_scheduled_ticks),
        (TickQueue::Fluid, &record.fluid_scheduled_ticks),
    ] {
        if !ticks.is_empty() {
            unsupported.push(UnsupportedNativeFeature::TickInsertionOrder {
                queue,
                ticks: ticks.len(),
            });
        }
    }
    ChunkExportReport { unsupported }
}

/// Converts one complete native chunk record into the Anvil chunk NBT tree.
///
/// `game_time` is the destination world's current game time. Native ticks
/// store absolute trigger ticks, while the Anvil tree stores a signed delay;
/// an out-of-range difference is rejected before an NBT tree is returned.
/// Block-state properties, three-dimensional biome cells, `MOTION_BLOCKING`,
/// block entities, and every present light section are emitted directly.
///
/// Native scheduled-tick insertion sequences have no destination field. A
/// non-empty queue therefore appears in [`preflight_chunk`] and needs a
/// matching [`ExportAuthorization`] before its tick list can be emitted.
pub fn export_chunk(
    column_x: i32,
    column_z: i32,
    record: &NativeChunkRecord,
    game_time: u64,
    authorization: Option<ExportAuthorization>,
) -> Result<ChunkExportResult, Error> {
    let Some(authorization) = authorization else {
        return Err(Error::MissingAuthorization);
    };
    if !authorization.permits_export() {
        return Err(Error::AuthorizationDenied { authorization });
    }
    let report = preflight_chunk(record);
    let required = report.decide(ExportLossDecision::ProceedAndDiscardUnsupported);
    if authorization != required {
        return Err(Error::AuthorizationMismatch {
            supplied: authorization,
            required,
        });
    }

    let extras = ChunkExtras {
        block_entities: record.column.block_entities().to_vec(),
        block_ticks: export_ticks(&record.block_scheduled_ticks, TickQueue::Block, game_time)?,
        fluid_ticks: export_ticks(&record.fluid_scheduled_ticks, TickQueue::Fluid, game_time)?,
    };
    let mut chunk =
        crate::chunk_nbt::column_to_nbt_with(column_x, column_z, &record.column, &extras);
    write_motion_blocking(&mut chunk, record)?;
    write_light(&mut chunk, record)?;
    Ok(ChunkExportResult { chunk, report })
}

fn export_ticks(
    ticks: &[PersistedScheduledTick],
    queue: TickQueue,
    game_time: u64,
) -> Result<Vec<SavedTick>, Error> {
    ticks
        .iter()
        .enumerate()
        .map(|(index, tick)| {
            let delay = i128::from(tick.trigger_tick) - i128::from(game_time);
            let delay = i32::try_from(delay).map_err(|_| Error::TickDelayOutOfRange {
                queue,
                index,
                trigger_tick: tick.trigger_tick,
                game_time,
            })?;
            Ok(SavedTick {
                pos: tick.pos,
                kind: tick.kind.clone(),
                delay,
                priority: tick.priority,
            })
        })
        .collect()
}

fn write_motion_blocking(chunk: &mut Nbt, record: &NativeChunkRecord) -> Result<(), Error> {
    let Some(heights) = record.column.motion_blocking() else {
        return Ok(());
    };
    let height = u32::try_from(record.column.height).map_err(|_| Error::InvalidHeight {
        height: record.column.height,
    })?;
    if height == 0 {
        return Err(Error::InvalidHeight {
            height: record.column.height,
        });
    }
    let mut map = Heightmap::new(height);
    for z in 0..16 {
        for x in 0..16 {
            let index = Heightmap::index(x, z);
            let value = heights[index];
            if u32::from(value) > height {
                return Err(Error::HeightmapValue {
                    index,
                    value,
                    height,
                });
            }
            map.set(x, z, u32::from(value));
        }
    }
    root_fields_mut(chunk).push((
        "Heightmaps".to_owned(),
        Nbt::Compound(vec![(
            "MOTION_BLOCKING".to_owned(),
            Nbt::LongArray(map.longs().iter().map(|value| *value as i64).collect()),
        )]),
    ));
    Ok(())
}

fn write_light(chunk: &mut Nbt, record: &NativeChunkRecord) -> Result<(), Error> {
    let first_section = record.column.min_y.div_euclid(16) - 1;
    let mut wrote_light = false;
    for index in 0..record.light.light_section_count() {
        let sky = light_bytes(record.light.sky(index), index, "sky")?;
        let block = light_bytes(record.light.block(index), index, "block")?;
        if sky.is_none() && block.is_none() {
            continue;
        }
        wrote_light = true;
        let section_y = first_section + index as i32;
        let fields = section_fields_mut(chunk, section_y);
        if let Some(bytes) = sky {
            fields.push(("SkyLight".to_owned(), Nbt::ByteArray(bytes)));
        }
        if let Some(bytes) = block {
            fields.push(("BlockLight".to_owned(), Nbt::ByteArray(bytes)));
        }
    }
    if wrote_light {
        let field = root_fields_mut(chunk)
            .iter_mut()
            .find(|(name, _)| name == "isLightOn")
            .expect("chunk_nbt always writes isLightOn");
        field.1 = Nbt::Byte(1);
    }
    Ok(())
}

fn light_bytes(
    data: &LightData,
    section: usize,
    kind: &'static str,
) -> Result<Option<Vec<i8>>, Error> {
    match data {
        LightData::Missing => Ok(None),
        LightData::Uniform(value) if *value <= 15 => {
            Ok(Some(vec![(value | (value << 4)) as i8; LIGHT_ARRAY_BYTES]))
        }
        LightData::Uniform(value) => Err(Error::InvalidUniformLight {
            section,
            kind,
            value: *value,
        }),
        LightData::Values(values) => Ok(Some(
            values.as_bytes().iter().map(|byte| *byte as i8).collect(),
        )),
    }
}

fn root_fields_mut(chunk: &mut Nbt) -> &mut Vec<(String, Nbt)> {
    let Nbt::Compound(fields) = chunk else {
        unreachable!("chunk_nbt always returns a compound root")
    };
    fields
}

fn section_fields_mut(chunk: &mut Nbt, section_y: i32) -> &mut Vec<(String, Nbt)> {
    let sections = root_fields_mut(chunk)
        .iter_mut()
        .find(|(name, _)| name == "sections")
        .expect("chunk_nbt always writes sections");
    let Nbt::List { elements, .. } = &mut sections.1 else {
        unreachable!("chunk_nbt sections are a list")
    };
    let found = elements.iter().position(|section| {
        let Nbt::Compound(fields) = section else {
            return false;
        };
        fields.iter().any(|(name, value)| {
            name == "Y" && matches!(value, Nbt::Byte(y) if i32::from(*y) == section_y)
        })
    });
    let index = found.unwrap_or_else(|| {
        elements.push(Nbt::Compound(vec![(
            "Y".to_owned(),
            Nbt::Byte(section_y as i8),
        )]));
        elements.len() - 1
    });
    let Nbt::Compound(fields) = &mut elements[index] else {
        unreachable!("new and chunk_nbt sections are compounds")
    };
    fields
}
