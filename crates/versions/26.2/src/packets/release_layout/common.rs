use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer};
use lodestone_model::BlockPos;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireStateId(u32);

impl WireStateId {
    pub const fn new(raw: u32) -> Option<Self> {
        if raw <= i32::MAX as u32 { Some(Self(raw)) } else { None }
    }

    pub const fn raw(self) -> u32 { self.0 }
}

/// Ordered resource identifiers of active screen post effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostEffects {
    pub effects: Vec<String>,
}

impl Decode for PostEffects {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        let count = reader.var_i32()?;
        let count = usize::try_from(count).map_err(|_| Error::NegativeLength(count))?;
        if count > reader.remaining() {
            return Err(Error::LimitExceeded { limit: reader.remaining(), actual: count });
        }
        let mut effects = Vec::with_capacity(count);
        for _ in 0..count {
            effects.push(reader.string(32767)?);
        }
        Ok(Self { effects })
    }
}

impl Encode for PostEffects {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        let count = i32::try_from(self.effects.len()).map_err(|_| Error::LimitExceeded {
            limit: i32::MAX as usize, actual: self.effects.len(),
        })?;
        writer.var_i32(count);
        for effect in &self.effects {
            writer.string(effect);
        }
        Ok(())
    }
}

/// Temporary block visual, retaining the selected release's wire state ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddTransientBlock {
    pub pos: BlockPos,
    pub state: WireStateId,
}

impl Decode for AddTransientBlock {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        let packed = reader.i64()?;
        let pos = BlockPos {
            x: (packed >> 38) as i32,
            y: (packed << 52 >> 52) as i32,
            z: (packed << 26 >> 38) as i32,
        };
        let raw = reader.var_i32()?;
        let state = u32::try_from(raw).ok().and_then(WireStateId::new)
            .ok_or_else(|| Error::Custom(format!("invalid 26.3 block state {raw}")))?;
        Ok(Self { pos, state })
    }
}

impl Encode for AddTransientBlock {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        let packed = ((i64::from(self.pos.x) & 0x3ff_ffff) << 38)
            | ((i64::from(self.pos.z) & 0x3ff_ffff) << 12)
            | (i64::from(self.pos.y) & 0xfff);
        writer.i64(packed);
        writer.var_i32(self.state.raw() as i32);
        Ok(())
    }
}

/// Hand discriminant on the protocol 777 wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireHand {
    Main,
    Off,
}

impl Decode for WireHand {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        match reader.var_i32()? {
            1 => Ok(Self::Off),
            _ => Ok(Self::Main),
        }
    }
}

impl Encode for WireHand {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        writer.var_i32(match self { Self::Main => 0, Self::Off => 1 });
        Ok(())
    }
}

/// The explicit motion carried by a remote arm animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwingKind {
    None,
    Whack,
    Stab,
}

impl Decode for SwingKind {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        match reader.var_i32()? {
            1 => Ok(Self::Whack),
            2 => Ok(Self::Stab),
            _ => Ok(Self::None),
        }
    }
}

impl Encode for SwingKind {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        writer.var_i32(match self { Self::None => 0, Self::Whack => 1, Self::Stab => 2 });
        Ok(())
    }
}

/// Remote entity arm animation with an explicit type and duration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwingAnimation {
    pub entity_id: i32,
    pub hand: WireHand,
    pub kind: SwingKind,
    pub duration_ticks: i32,
}

impl Decode for SwingAnimation {
    fn decode(reader: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        Ok(Self {
            entity_id: reader.var_i32()?,
            hand: WireHand::decode(reader, ctx)?,
            kind: SwingKind::decode(reader, ctx)?,
            duration_ticks: reader.var_i32()?,
        })
    }
}

impl Encode for SwingAnimation {
    fn encode(&self, writer: &mut Writer, ctx: Ctx) -> Result<()> {
        writer.var_i32(self.entity_id);
        self.hand.encode(writer, ctx)?;
        self.kind.encode(writer, ctx)?;
        writer.var_i32(self.duration_ticks);
        Ok(())
    }
}
