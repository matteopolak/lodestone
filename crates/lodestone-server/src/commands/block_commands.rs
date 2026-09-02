//! `/setblock` and `/fill`.
//!
//! # Always self-targeted, and why
//!
//! [`crate::commands::Effect::SetBlock`]/[`Effect::Fill`](crate::commands::Effect::Fill)
//! carry no player identity because a block edit is not a per-player thing —
//! but *delivery* still rides the directed-effect channel, aimed at the
//! caller's own uuid, because applying it needs a chunk source: a live
//! connection's own `ChatCommand` arm (`crate::server::dispatch_play_packet`)
//! has one (`SourceRef<'_, S>`) and applies the effect inline, and RCON's own
//! dispatcher (`crate::rcon::run_command_as`) does the same over its stored
//! `RconConfig::world_source`. Either way the actual
//! [`ChunkSource::set_block`](crate::chunk::ChunkSource::set_block) call and
//! the [`crate::tick::BlockTickFeed`] broadcast happen at the one place that
//! *has* both, never inside this module.
//!
//! # The console has no uuid, so it gets the crate's own "no player" sentinel
//!
//! [`CommandSource::uuid`] is `None` for the console
//! (`SourceEntity`'s own doc: "`None` on `CommandSource` is the console"),
//! and `Effect` delivery needs a concrete uuid to address. Rather than
//! refusing outright — which is what this module did before RCON had
//! anywhere to apply the effect — a console source uses `Uuid::nil()`, the
//! same "not a real player, cannot collide with one" sentinel `crate::rcon`'s
//! own console identity already uses. A live connection never reaches this
//! fallback: [`CommandSource::player`] always carries a real uuid, so
//! `Uuid::nil()` is unambiguous console-only.
//!
//! # `/fill`'s volume cap
//!
//! The real limit is 32768 (the default command block-modification limit),
//! refused with `commands.fill.toobig` before a single position is
//! enumerated — checked against the *volume*, not after building the
//! position list, so a `1000000 1000000 1000000` corner pair costs one
//! multiplication rather than an allocation.

use lodestone_command_mc::{BlockArg, BlockPosArg, Coordinates};

use super::registrar::{Ctx, Registrar};

/// The game-masters permission level.
const BLOCK_LEVEL: u8 = 2;

/// The real fill-size limit.
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
        // See this module's own doc for why `None` (the console) falls back
        // to `Uuid::nil()` rather than refusing outright.
        let uuid = ctx.source.uuid().unwrap_or(uuid::Uuid::nil());
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
        // See this module's own doc for why `None` (the console) falls back
        // to `Uuid::nil()` rather than refusing outright.
        let uuid = ctx.source.uuid().unwrap_or(uuid::Uuid::nil());
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
