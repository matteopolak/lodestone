use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParticleDistribution {
    Default,
    Alternative,
    AlternativeWithSpeed,
}

impl Decode for ParticleDistribution {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        Ok(match reader.var_i32()? {
            1 => Self::Alternative,
            2 => Self::AlternativeWithSpeed,
            _ => Self::Default,
        })
    }
}

impl Encode for ParticleDistribution {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        writer.var_i32(match self {
            Self::Default => 0,
            Self::Alternative => 1,
            Self::AlternativeWithSpeed => 2,
        });
        Ok(())
    }
}

/// The fixed fields following the type-specific particle options.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleSpawn {
    pub override_limiter: bool,
    pub always_show: bool,
    pub pos: [f64; 3],
    pub offset: [f32; 3],
    pub speed: [f32; 3],
    pub count: i32,
    pub distribution: ParticleDistribution,
}

impl Decode for ParticleSpawn {
    fn decode(reader: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        Ok(Self {
            override_limiter: reader.bool()?, always_show: reader.bool()?,
            pos: [reader.f64()?, reader.f64()?, reader.f64()?],
            offset: [reader.f32()?, reader.f32()?, reader.f32()?],
            speed: [reader.f32()?, reader.f32()?, reader.f32()?],
            count: reader.var_i32()?, distribution: ParticleDistribution::decode(reader, ctx)?,
        })
    }
}

impl Encode for ParticleSpawn {
    fn encode(&self, writer: &mut Writer, ctx: Ctx) -> Result<()> {
        writer.bool(self.override_limiter);
        writer.bool(self.always_show);
        for value in self.pos { writer.f64(value); }
        for value in self.offset { writer.f32(value); }
        for value in self.speed { writer.f32(value); }
        writer.var_i32(self.count);
        self.distribution.encode(writer, ctx)
    }
}
