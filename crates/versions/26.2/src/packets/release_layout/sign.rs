use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};
use lodestone_model::BlockPos;

/// Sign side encoded as a VarInt, with back=0 and front=1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignTextSlot {
    Back,
    Front,
}

impl Decode for SignTextSlot {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        Ok(if reader.var_i32()? == 1 { Self::Front } else { Self::Back })
    }
}

impl Encode for SignTextSlot {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        writer.var_i32(i32::from(*self == Self::Front));
        Ok(())
    }
}

fn read_pos(reader: &mut Reader<'_>) -> Result<BlockPos> {
    let value = reader.i64()?;
    Ok(BlockPos {
        x: (value >> 38) as i32, y: (value << 52 >> 52) as i32,
        z: (value << 26 >> 38) as i32,
    })
}

fn write_pos(writer: &mut Writer, pos: BlockPos) {
    writer.i64(((i64::from(pos.x) & 0x3ff_ffff) << 38)
        | ((i64::from(pos.z) & 0x3ff_ffff) << 12) | (i64::from(pos.y) & 0xfff));
}

/// Open the selected side of a sign for editing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenSignEditor {
    pub pos: BlockPos,
    pub slot: SignTextSlot,
}

impl Decode for OpenSignEditor {
    fn decode(reader: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        Ok(Self { pos: read_pos(reader)?, slot: SignTextSlot::decode(reader, ctx)? })
    }
}

impl Encode for OpenSignEditor {
    fn encode(&self, writer: &mut Writer, ctx: Ctx) -> Result<()> {
        write_pos(writer, self.pos);
        self.slot.encode(writer, ctx)
    }
}

/// Four sign lines followed by the selected sign side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignUpdate {
    pub pos: BlockPos,
    pub lines: [String; 4],
    pub slot: SignTextSlot,
}

impl Decode for SignUpdate {
    fn decode(reader: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        Ok(Self {
            pos: read_pos(reader)?,
            lines: [reader.string(384)?, reader.string(384)?, reader.string(384)?, reader.string(384)?],
            slot: SignTextSlot::decode(reader, ctx)?,
        })
    }
}

impl Encode for SignUpdate {
    fn encode(&self, writer: &mut Writer, ctx: Ctx) -> Result<()> {
        write_pos(writer, self.pos);
        for line in &self.lines { writer.string(line); }
        self.slot.encode(writer, ctx)
    }
}
