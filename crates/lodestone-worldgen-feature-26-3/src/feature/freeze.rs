//! The top-layer freeze (`freeze_top_layer`): ice over cold water and snow over cold ground.

use crate::climate;
use crate::env::Heightmap;
use crate::level::Level;
use crate::pos::Pos;

pub fn place_freeze_top_layer(level: &mut Level<'_>, origin: Pos) -> bool {
    let env = level.env;
    for dx in 0..16 {
        for dz in 0..16 {
            let (x, z) = (origin.x + dx, origin.z + dz);
            let y = level.height(Heightmap::MotionBlocking, x, z);
            if climate::should_freeze_in(level, y, x, y - 1, z, false) {
                level.set(x, y - 1, z, env.known.ice);
            }
            if climate::should_snow(level, x, y, z) {
                level.set(x, y, z, env.known.snow);
                let below = level.get(x, y - 1, z);
                if env.blocks.has_property(below, "snowy") {
                    if let Some(snowy) = env.blocks.with(below, "snowy", "true") {
                        level.set(x, y - 1, z, snowy);
                    }
                }
            }
        }
    }
    true
}
