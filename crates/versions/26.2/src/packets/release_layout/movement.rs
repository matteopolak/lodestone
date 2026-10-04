use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer};

/// One timed relative position step, in 1/4096 block units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeltaStep {
    pub ticks: i32,
    pub delta: [i16; 3],
}

/// Relative movement can carry a single delta or a timed sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeltaPath {
    Linear([i16; 3]),
    Stepped(Vec<DeltaStep>),
}

impl DeltaPath {
    fn decode(reader: &mut Reader<'_>, count: usize) -> Result<Self> {
        if count == 0 {
            return Ok(Self::Linear([reader.i16()?, reader.i16()?, reader.i16()?]));
        }
        if count > reader.remaining() / 7 {
            return Err(Error::LimitExceeded { limit: reader.remaining() / 7, actual: count });
        }
        let mut steps = Vec::with_capacity(count);
        for _ in 0..count {
            steps.push(DeltaStep {
                ticks: reader.var_i32()?,
                delta: [reader.i16()?, reader.i16()?, reader.i16()?],
            });
        }
        Ok(Self::Stepped(steps))
    }

    fn encode(&self, writer: &mut Writer) {
        match self {
            Self::Linear(delta) => {
                for value in delta { writer.i16(*value); }
            }
            Self::Stepped(steps) => {
                for step in steps {
                    writer.var_i32(step.ticks);
                    for value in step.delta { writer.i16(value); }
                }
            }
        }
    }

    fn properties(&self, on_ground: bool) -> Result<i32> {
        let count = match self { Self::Linear(_) => 0, Self::Stepped(steps) => steps.len() };
        if matches!(self, Self::Stepped(_)) && count == 0 {
            return Err(Error::Custom("a stepped delta path needs at least one step".to_owned()));
        }
        if count > (i32::MAX as usize) >> 1 {
            return Err(Error::LimitExceeded { limit: (i32::MAX as usize) >> 1, actual: count });
        }
        Ok(((count as i32) << 1) | i32::from(on_ground))
    }
}

fn read_relative(reader: &mut Reader<'_>) -> Result<(i32, bool, DeltaPath)> {
    let entity_id = reader.var_i32()?;
    let properties = reader.var_i32()? as u32;
    let path = DeltaPath::decode(reader, (properties >> 1) as usize)?;
    Ok((entity_id, properties & 1 != 0, path))
}

/// Relative position update with a packed ground flag and step count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveEntityPos {
    pub entity_id: i32,
    pub on_ground: bool,
    pub path: DeltaPath,
}

impl Decode for MoveEntityPos {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        let (entity_id, on_ground, path) = read_relative(reader)?;
        Ok(Self { entity_id, on_ground, path })
    }
}

impl Encode for MoveEntityPos {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        writer.var_i32(self.entity_id);
        writer.var_i32(self.path.properties(self.on_ground)?);
        self.path.encode(writer);
        Ok(())
    }
}

/// Relative position and byte-angle rotation update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveEntityPosRot {
    pub entity_id: i32,
    pub on_ground: bool,
    pub path: DeltaPath,
    pub yaw: u8,
    pub pitch: u8,
}

impl Decode for MoveEntityPosRot {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        let (entity_id, on_ground, path) = read_relative(reader)?;
        Ok(Self { entity_id, on_ground, path, yaw: reader.u8()?, pitch: reader.u8()? })
    }
}

impl Encode for MoveEntityPosRot {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        writer.var_i32(self.entity_id);
        writer.var_i32(self.path.properties(self.on_ground)?);
        self.path.encode(writer);
        writer.u8(self.yaw);
        writer.u8(self.pitch);
        Ok(())
    }
}

/// Rotation-only update, with the ground flag preceding both byte angles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveEntityRot {
    pub entity_id: i32,
    pub on_ground: bool,
    pub yaw: u8,
    pub pitch: u8,
}

impl Decode for MoveEntityRot {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        Ok(Self {
            entity_id: reader.var_i32()?, on_ground: reader.bool()?,
            yaw: reader.u8()?, pitch: reader.u8()?,
        })
    }
}

impl Encode for MoveEntityRot {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        writer.var_i32(self.entity_id);
        writer.bool(self.on_ground);
        writer.u8(self.yaw);
        writer.u8(self.pitch);
        Ok(())
    }
}

/// One absolute position with its interpolation duration.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionStep {
    pub pos: [f64; 3],
    pub ticks: i32,
}

/// Absolute position path carried by entity synchronization.
#[derive(Clone, Debug, PartialEq)]
pub enum PositionPath {
    Linear([f64; 3]),
    Stepped(Vec<PositionStep>),
}

impl Decode for PositionPath {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        if reader.var_i32()? != 1 {
            return Ok(Self::Linear([reader.f64()?, reader.f64()?, reader.f64()?]));
        }
        let count = reader.var_i32()?;
        let count = usize::try_from(count).map_err(|_| Error::NegativeLength(count))?;
        if count == 0 {
            return Err(Error::Custom("an absolute stepped path needs at least one step".to_owned()));
        }
        if count > reader.remaining() / 25 {
            return Err(Error::LimitExceeded { limit: reader.remaining() / 25, actual: count });
        }
        let mut steps = Vec::with_capacity(count);
        for _ in 0..count {
            steps.push(PositionStep {
                pos: [reader.f64()?, reader.f64()?, reader.f64()?], ticks: reader.var_i32()?,
            });
        }
        Ok(Self::Stepped(steps))
    }
}

impl Encode for PositionPath {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        match self {
            Self::Linear(pos) => {
                writer.var_i32(0);
                for value in pos { writer.f64(*value); }
            }
            Self::Stepped(steps) => {
                if steps.is_empty() {
                    return Err(Error::Custom("a stepped position path needs at least one step".to_owned()));
                }
                let count = i32::try_from(steps.len()).map_err(|_| Error::LimitExceeded {
                    limit: i32::MAX as usize, actual: steps.len(),
                })?;
                writer.var_i32(1);
                writer.var_i32(count);
                for step in steps {
                    for value in step.pos { writer.f64(value); }
                    writer.var_i32(step.ticks);
                }
            }
        }
        Ok(())
    }
}

/// Absolute entity synchronization with a position path and float angles.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityPositionSync {
    pub entity_id: i32,
    pub path: PositionPath,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

impl Decode for EntityPositionSync {
    fn decode(reader: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        Ok(Self {
            entity_id: reader.var_i32()?, path: PositionPath::decode(reader, ctx)?,
            yaw: reader.f32()?, pitch: reader.f32()?, on_ground: reader.bool()?,
        })
    }
}

impl Encode for EntityPositionSync {
    fn encode(&self, writer: &mut Writer, ctx: Ctx) -> Result<()> {
        writer.var_i32(self.entity_id);
        self.path.encode(writer, ctx)?;
        writer.f32(self.yaw);
        writer.f32(self.pitch);
        writer.bool(self.on_ground);
        Ok(())
    }
}
