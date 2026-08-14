//! `/setblock` and `/fill`, from `SetBlockCommand.java` and `FillCommand.java`.
//!
//! # Always self-targeted, and why
//!
//! [`crate::commands::Effect::SetBlock`]/[`Effect::Fill`](crate::commands::Effect::Fill)
//! carry no player identity because a block edit is not a per-player thing —
//! but *delivery* still rides the directed-effect channel, aimed at the caller's
//! own uuid, because applying it needs the chunk source (`SourceRef<'_, S>`)
//! that only the issuing connection's own `ChatCommand` arm has in scope; see
//! that arm's own handling of these two variants in `crate::server` for where
//! the actual [`ChunkSource::set_block`](crate::chunk::ChunkSource::set_block)
//! call and the [`crate::tick::BlockTickFeed`] broadcast happen. A command run
//! over RCON has no such connection and so cannot reach either one yet — see
//! `crate::rcon`'s module doc for the roster of what RCON can and cannot do.
//!
//! # `/fill`'s volume cap
//!
//! `FillCommand.MAX_FILL_SIZE` is a real limit (32768 in vanilla,
//! `commandModificationBlockLimit`'s default), refused with
//! `commands.fill.toobig` before a single position is enumerated — checked
//! against the *volume*, not after building the position list, so a
//! `1000000 1000000 1000000` corner pair costs one multiplication rather than
//! an allocation.

use lodestone_command_mc::{BlockArg, BlockPosArg, Coordinates};

use super::registrar::{Ctx, Registrar};

/// `Commands.LEVEL_GAMEMASTERS`.
const BLOCK_LEVEL: u8 = 2;

/// `FillCommand.MAX_FILL_SIZE`.
const MAX_FILL_SIZE: i64 = 32_768;

fn resolve_block_pos(ctx: &Ctx<'_>, coords: Coordinates) -> (i32, i32, i32) {
    let origin = (ctx.source.position.x, ctx.source.position.y, ctx.source.position.z);
    let rotation = (ctx.source.rotation.yaw, ctx.source.rotation.pitch);
    let (x, y, z) = coords.resolve(origin, rotation);
    (x.floor() as i32, y.floor() as i32, z.floor() as i32)
}

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();

    // ---- /setblock ------------------------------------------------------------
    let setblock = registrar.literal(root, "setblock");
    registrar.require_level(setblock, BLOCK_LEVEL);
    let (pos_node, pos_key) = registrar.arg(setblock, "pos", BlockPosArg);
    let (block_node, block_key) = registrar.arg(pos_node, "block", BlockArg);
    registrar.exec(block_node, move |ctx| {
        let Some(uuid) = ctx.source.uuid() else {
            return Err("That command can only be used by a player".to_string());
        };
        let pos = resolve_block_pos(ctx, *ctx.get(pos_key));
        let block = ctx.get(block_key).block.to_string();
        ctx.effect(uuid, crate::commands::Effect::SetBlock { pos, block: block.clone() });
        ctx.send_success(format!("Changed the block at {}, {}, {} to {block}", pos.0, pos.1, pos.2));
        Ok(1)
    });

    // ---- /fill ------------------------------------------------------------------
    let fill = registrar.literal(root, "fill");
    registrar.require_level(fill, BLOCK_LEVEL);
    let (from_node, from_key) = registrar.arg(fill, "from", BlockPosArg);
    let (to_node, to_key) = registrar.arg(from_node, "to", BlockPosArg);
    let (fill_block_node, fill_block_key) = registrar.arg(to_node, "block", BlockArg);
    registrar.exec(fill_block_node, move |ctx| {
        let Some(uuid) = ctx.source.uuid() else {
            return Err("That command can only be used by a player".to_string());
        };
        let from = resolve_block_pos(ctx, *ctx.get(from_key));
        let to = resolve_block_pos(ctx, *ctx.get(to_key));
        let block = ctx.get(fill_block_key).block.to_string();

        let (min_x, max_x) = (from.0.min(to.0), from.0.max(to.0));
        let (min_y, max_y) = (from.1.min(to.1), from.1.max(to.1));
        let (min_z, max_z) = (from.2.min(to.2), from.2.max(to.2));
        let dx = i64::from(max_x - min_x) + 1;
        let dy = i64::from(max_y - min_y) + 1;
        let dz = i64::from(max_z - min_z) + 1;
        let volume = dx.saturating_mul(dy).saturating_mul(dz);
        if volume > MAX_FILL_SIZE {
            return Err(format!(
                "Too many blocks in the specified area ({volume}, max {MAX_FILL_SIZE})"
            ));
        }

        let mut positions = Vec::with_capacity(volume as usize);
        for x in min_x..=max_x {
            for y in min_y..=max_y {
                for z in min_z..=max_z {
                    positions.push((x, y, z));
                }
            }
        }
        let count = positions.len();
        ctx.effect(uuid, crate::commands::Effect::Fill { positions, block: block.clone() });
        ctx.send_success(format!("Successfully filled {count} block(s) with {block}"));
        Ok(i32::try_from(count).unwrap_or(i32::MAX))
    });
}
