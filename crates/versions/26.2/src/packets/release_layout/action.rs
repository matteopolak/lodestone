use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};

/// Empty serverbound punch body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Punch;

impl Decode for Punch {
    fn decode(_: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        Ok(Self)
    }
}

impl Encode for Punch {
    fn encode(&self, _: &mut Writer, _: Ctx) -> Result<()> {
        Ok(())
    }
}

/// Teleport acknowledgement including the client's accepted pose.
#[derive(Clone, Debug, PartialEq)]
pub struct AcceptTeleportation {
    pub id: i32,
    pub pos: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
}

impl Decode for AcceptTeleportation {
    fn decode(reader: &mut Reader<'_>, _: Ctx) -> Result<Self> {
        Ok(Self {
            id: reader.var_i32()?,
            pos: [reader.f64()?, reader.f64()?, reader.f64()?],
            yaw: reader.f32()?,
            pitch: reader.f32()?,
        })
    }
}

impl Encode for AcceptTeleportation {
    fn encode(&self, writer: &mut Writer, _: Ctx) -> Result<()> {
        writer.var_i32(self.id);
        for value in self.pos {
            writer.f64(value);
        }
        writer.f32(self.yaw);
        writer.f32(self.pitch);
        Ok(())
    }
}
