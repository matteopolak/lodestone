//! Block actions from a client: digging and breaking, support collapse, and the per-tick block-update batches sent back.

use super::*;

/// Applies one block-breaking phase for the three destroy-action ordinals.
///
/// This production path **validates** the break rather than trusting it: see
/// [`crate::block_breaking`] for the destroy-progress arithmetic and the
/// tolerance it deliberately carries. Creative mode has a separate start-only
/// path that bypasses timing while retaining target validity and proposal
/// protection. Two survival behaviours follow from the timing computation, and
/// they are opposite ends of the same missing computation:
///
/// * **`StartDestroy` can break the block by itself.** When destroy progress
///   reaches `1.0` on the first tick, a zero-hardness block needs no follow-up
///   action. The server therefore handles instant blocks at `StartDestroy`.
/// * **A `StopDestroy` that arrives too early is *deferred*, not refused.** It
///   records a deferred dig and keeps accruing progress on the server's clock,
///   breaking the block once it is fully earned — see
///   [`crate::block_breaking::PendingBreak::defer`] and `serve_play`'s
///   `vitals_tick` arm. Bedrock and obsidian are still not instant, because an
///   unbreakable block is not deferrable and obsidian's deferred dig is minutes
///   long; but hold-and-release on stone breaks stone, which an outright refusal
///   made impossible.
///
/// `pending_break` is this connection's tracked in-progress dig, including its
/// target and accumulated progress.
/// It is what makes `StartDestroy` + `StopDestroy` break a block while
/// `StartDestroy` + `AbortDestroy` does not, and what makes a `StopDestroy` for a
/// position nobody started is a no-op; only the tracked target may advance.
///
/// **Also removes a broken position's [`BlockEntity`], if any, from the
/// registry**. A screen can remain open at the broken position, so leaving the
/// record would let a later container click mutate a container whose block no
/// longer exists. If [`OpenContainer`] points at the broken position, it is
/// cleared as well; the client receives no synthetic close frame.
///
/// When an integrated world has a proposal owner, the earned break is first
/// submitted as a Paper-shaped `BlockBreak` proposal. A denied or unavailable
/// proposal sends the authoritative state back to this connection and leaves
/// the source, drops, block-entity registry, and block-tick feed untouched.
#[allow(clippy::too_many_arguments)]
pub(super) async fn apply_block_action<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    mut pending_relights: Option<&mut PendingRelights>,
    pending_break: &mut Option<PendingBreak>,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    // Shared mob handle where block-break loot becomes item entities. The
    // composter arm of `apply_use_item_on` uses the same handle for bone-meal
    // drops, so every connection's streaming pass sees the spawned entity.
    mobs: &MobHandle,
    drops_rng: &mut SpawnRng,
    // The breaker's main-hand stack, `None` for a bare hand. It supplies the
    // loot context and tool-eligibility check for the roll; a borrowed stack is
    // sufficient because the caller already owns the mutable inventory.
    held: Option<&ItemStack>,
    // The breaker's tracked feet position for the interaction-range
    // test, `None` until the client has sent a movement packet — see
    // `block_breaking::within_interaction_range` for why `None` permits the break
    // rather than refusing it.
    player_feet: Option<Vec3>,
    // The world's rules, for the `block_drops` gate below.
    world: &crate::world_state::WorldStateHandle,
    // The server tick this packet is being handled on, for the
    // destroy-progress accounting. `None` on `wasm32`, which has no timer to
    // count ticks with (see `serve_play`'s two definitions); the timing test is
    // then skipped, while the hardness and range tests still apply.
    game_tick: Option<u64>,
    // Where `destroy_block`'s break level event is published, and the player it
    // is published *except* for (this connection's own).
    block_ticks: &BlockTickFeed,
    breaker: uuid::Uuid,
    // Creative mode bypasses the hardness clock and produces no drops.
    creative: bool,
    action: BlockActionKind,
    // `minecraft:mined` counter — see `destroy_block`'s own parameter
    // comment for why it is awarded there rather than here.
    advancements: &mut AdvancementManager,
    // Hunger, for the per-block mining exhaustion `destroy_block` charges. Threaded
    // through rather than read from a wider scope so the creative guard stays at the
    // one place that knows the game mode.
    vitals: &mut PlayerVitals,
    pos: BlockPos,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    tracing::debug!(
        target: "lodestone_block_trace",
        ?action,
        x = pos.x,
        y = pos.y,
        z = pos.z,
        ?game_tick,
        pending = ?pending_break.as_ref().map(|dig| dig.pos),
        state = source.block_state_id(pos.x, pos.y, pos.z).raw(),
        "block action received"
    );
    // Vanilla's very first guard in `handleBlockBreakAction`, ahead of the
    // per-ordinal fork: a break out of reach is dropped whatever phase it is.
    if !crate::block_breaking::within_interaction_range(player_feet, pos) {
        return Ok(());
    }
    match action {
        BlockActionKind::StartDestroy => {
            let target = source.block_state_id(pos.x, pos.y, pos.z);
            let per_tick = (!creative)
                .then(|| crate::block_breaking::progress_per_tick(target, held))
                .flatten();
            if creative && target.block() == Block::Air {
                *pending_break = None;
                return Ok(());
            }
            // `None` is a state neither census knows — our gap, not a cheat, so
            // it is priced as an ordinary progressive dig that the `None`-clock
            // branch of `may_break_at` will accept on any `StopDestroy`.
            if creative || per_tick.is_some_and(|per| per >= 1.0) {
                // Vanilla's `"insta mine"` exit: the block is gone now, and no
                // `StopDestroy` is coming for it. This is the one-shot-block fix.
                // Creative takes the same exit for *every* block, which is what
                // makes a creative dig instant rather than merely fast.
                *pending_break = None;
                if !adjudicate_block_break(
                    conn,
                    proto,
                    source,
                    state,
                    world,
                    pos,
                    breaker,
                )
                .await?
                {
                    return Ok(());
                }
                destroy_block(
                    conn,
                    proto,
                    source,
                    state,
                    pending_relights.as_deref_mut(),
                    block_entities,
                    open_container,
                    container_sync,
                    mobs,
                    drops_rng,
                    held,
                    block_ticks,
                    breaker,
                    !creative && world.block_drops(),
                    world.block_drops(),
                    pos,
                    advancements,
                    (!creative).then_some(vitals),
                )
                .await?;
            } else {
                *pending_break = Some(PendingBreak {
                    pos,
                    progress_per_tick: per_tick.unwrap_or(f32::INFINITY),
                    start_tick: game_tick,
                    // Vanilla's `isDestroyingBlock`, not `hasDelayedDestroy`:
                    // this dig is waiting on a `StopDestroy` packet. A fresh
                    // `StartDestroy` replaces whatever was in the slot, including
                    // a deferred dig on another position — vanilla keeps the two
                    // states side by side and prefers the deferred one, a quirk
                    // not worth a second slot here (the client only ever has one
                    // dig in flight).
                    deferred: false,
                });
            }
        }
        BlockActionKind::AbortDestroy => {
            if pending_break.is_some_and(|dig| dig.pos == pos) {
                *pending_break = None;
            }
        }
        BlockActionKind::StopDestroy => {
            let Some(dig) = pending_break.filter(|dig| dig.pos == pos) else {
                return Ok(());
            };
            *pending_break = None;
            if !dig.may_break_at(game_tick) {
                // **Not a refusal.** A dig whose progress is below the threshold
                // enters the deferred state and continues through the per-player
                // tick until the block is fully mined. A `StopDestroy` arriving
                // on the same tick as `StartDestroy` therefore cannot clear 0.7.
                //
                // A `None` means the dig can never finish (bedrock, or no clock),
                // so the slot is simply left empty and nothing breaks. See
                // `block_breaking::PendingBreak::defer` and `serve_play`'s
                // `vitals_tick` arm, which is what finishes a deferred dig.
                *pending_break = dig.defer();
                return Ok(());
            }
            if !adjudicate_block_break(conn, proto, source, state, world, pos, breaker).await? {
                return Ok(());
            }
            destroy_block(
                conn,
                proto,
                source,
                state,
                pending_relights.as_deref_mut(),
                block_entities,
                open_container,
                container_sync,
                mobs,
                drops_rng,
                held,
                block_ticks,
                breaker,
                !creative && world.block_drops(),
                world.block_drops(),
                pos,
                advancements,
                (!creative).then_some(vitals),
            )
            .await?;
        }
    }
    Ok(())
}

/// Gives the tick-owned Paper event path the final say immediately before a
/// connection-side break writes its replacement. The proposal payload uses a
/// validated state id instead of a raw registry string; an unrecognised state
/// stays on the existing direct path because there is no safe event payload to
/// expose for it.
pub(super) async fn adjudicate_block_break<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    wire_state: &mut State,
    world: &crate::world_state::WorldStateHandle,
    pos: BlockPos,
    breaker: uuid::Uuid,
) -> Result<bool, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let current = source.block_state_id(pos.x, pos.y, pos.z);
    let block_state = current;
    let Some(proposals) = world.proposal_handle() else {
        tracing::debug!(target: "lodestone_block_trace", x = pos.x, y = pos.y, z = pos.z, "block break has no proposal owner");
        return Ok(true);
    };
    let decision = proposals.block_break(pos, block_state, breaker).await;
    tracing::debug!(target: "lodestone_block_trace", x = pos.x, y = pos.y, z = pos.z, ?decision, "block break proposal resolved");
    let allowed = matches!(
        decision,
        Ok(crate::ecs::ServerProposalAction::BlockBreak {
            pos: resolved_pos,
            state: resolved_state,
            breaker: resolved_breaker,
        }) if resolved_pos == pos && resolved_state == block_state && resolved_breaker == breaker
    );
    if !allowed {
        // A client may have predicted the break while the proposal waited for
        // the next tick. Correct it with the source's current authoritative
        // state, without publishing a world mutation or a synthetic break.
        apply(
            conn,
            wire_state,
            proto.encode_block_update(pos.x, pos.y, pos.z, current),
        )
        .await?;
    }
    Ok(allowed)
}

/// The [`crate::fluid::FluidEnv`] for the column `pos` falls in — the
/// dimension's real vertical extent rather than [`crate::fluid::FluidEnv::OVERWORLD`]'s
/// literal bounds, matching how [`crate::tick::run_tick_loop`] derives one for
/// its own fluid drain. [`crate::fluid::ticks_after_edit`] needs this at every
/// edit site so the seeding it schedules honours the same build-height guard
/// a scheduled fluid tick does.
pub(super) fn fluid_env_at<S: ChunkSource + ?Sized>(source: &S, pos: BlockPos) -> crate::fluid::FluidEnv {
    let column = source.column(pos.x.div_euclid(16), pos.z.div_euclid(16));
    crate::fluid::FluidEnv::overworld_in(column.min_y, column.height)
}

/// Breaks the block at `pos`: rolls and pops its loot, clears any block entity
/// and open container against it, and tells the client.
///
/// [`apply_block_action`] calls this helper for both instant `StartDestroy` and
/// validated `StopDestroy` requests. Both routes share loot rolling,
/// block-entity cleanup, and the client update.
#[allow(clippy::too_many_arguments)]
pub(super) async fn destroy_block<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    mut pending_relights: Option<&mut PendingRelights>,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    mobs: &MobHandle,
    drops_rng: &mut SpawnRng,
    held: Option<&ItemStack>,
    // Publish the break effect (sound and particles) to every viewer except
    // `breaker`; the acting client predicts its own effect locally. See
    // `BlockTickFeed::publish_effect_except`.
    block_ticks: &BlockTickFeed,
    breaker: uuid::Uuid,
    // `false` in creative — the direct destroy branch writes no drops, and a
    // creative break consumes no loot-roll RNG draws.
    drop_loot: bool,
    // The `block_drops` game rule **alone**, without the creative fork above —
    // for the support cascade only. The two gates differ because a support
    // cascade has no player context; passing `drop_loot` here would make a
    // creative player mining under a flower delete the flower.
    cascade_drops: bool,
    pos: BlockPos,
    // The statistics store, for the `minecraft:mined` counter. Keyed by the block
    // that was broken, and incremented on **every** break including a creative
    // one. Keep this independent of `drop_loot`, because creative breaks still
    // contribute to the mined count even though they produce no item entities.
    advancements: &mut AdvancementManager,
    // Hunger's mining cost (`0.005` per block).
    // `None` for a break by an invulnerable player, who mines for free. An
    // `Option` rather than a bool beside the vitals keeps the guard
    // cannot be forgotten at a new call site.
    exhaust: Option<&mut PlayerVitals>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let started = JoinStopwatch::now();
    // Read the block before replacement; once `set_block` runs, the original
    // state cannot be recovered. Capture its fluid state before writing the
    // replacement so a waterlogged block leaves its water source rather than
    // unconditional air (see `new_state` below).
    let broken = source.block_state_id(pos.x, pos.y, pos.z);
    // The removal write preserves a cell's *fluid* state. For a dry block
    // `fluid_state_of` is `None` and this is plain air, which is why every
    // existing break gate — all of them dry blocks — could not see the
    // difference. A waterlogged block's fluid state is a water source
    // (`fluid_state_of` reports `amount: 8, falling: false`), so its
    // `block_state()` is `minecraft:water[level=0]`, the source state left
    // behind.
    let new_state = crate::fluid::fluid_state_of_id(broken)
        .map(crate::fluid::FluidState::block_state_id)
        .unwrap_or_else(|| Block::Air.default_state());
    // The base name, not the state string: `minecraft:mined` is keyed by *block*,
    // so `minecraft:oak_log[axis=y]` and `minecraft:oak_log` must be one counter
    // rather than two. Every other per-block table in this crate strips the suffix
    // the same way.
    advancements.award_stat(
        breaker,
        crate::advancements::StatKey::new(
            crate::advancements::StatType::Mined,
            broken.block().name(),
        ),
        1,
    );
    if let Some(vitals) = exhaust {
        vitals.add_exhaustion(crate::food::EXHAUSTION_MINE);
    }
    if let Some(effect) = crate::effects::block_destroyed(pos, broken) {
        block_ticks.publish_effect_except(breaker, effect);
    }
    source.set_block(pos.x, pos.y, pos.z, new_state);
    let edit_elapsed = started.elapsed();
    // Roll the broken block's loot table and pop each resulting
    // stack as a real item entity. `MobSim` already ticks item
    // lifecycle and fall dynamics every server tick
    // (`crate::tick::run_tick_loop`) and already streams items to
    // every connection (`MobSim::snapshots`), so this one call is
    // what connects a 1,551-line loot module that had no production
    // caller to the wire path mobs already proved reaches a client.
    //
    // **Gated on `block_drops`**, the world-state drop rule. The resource-drop
    // path checks the rule before it rolls or emits any item entities.
    //
    // **Tool validation decides whether the table rolls and what context it receives.**
    // `drops_are_allowed` checks the required tool before
    // the loot table is rolled — so a bare hand on stone
    // breaks the block and drops nothing, and the roll's RNG draws
    // never happen either (folding the check into the table would
    // still consume them and shift the next break's stream). `held`
    // then rides into the roll as the tool loot-context parameter, which is
    // what makes `match_tool`, `apply_bonus` and `table_bonus`
    // evaluate against a real item instead of an absent one.
    let popped = if drop_loot && crate::block_drops::drops_are_allowed(broken, held) {
        crate::block_drops::drop_block_loot(
            crate::block_drops::bundled_tables(),
            broken,
            pos,
            held,
            drops_rng,
        )
    } else {
        Vec::new()
    };
    if !popped.is_empty() {
        mobs.with(|sim| {
            for drop in popped {
                // `ItemLifecycle::newly_dropped` already sets the
                // 10-tick delay used for freshly spawned drops, so the breaker
                // cannot re-absorb the drop on the spawning tick.
                let count = u8::try_from(drop.stack.count).unwrap_or(u8::MAX);
                sim.spawn_item(
                    &drop.stack,
                    drop.position,
                    drop.velocity,
                    ItemLifecycle::newly_dropped(count, DEFAULT_MAX_STACK_SIZE),
                );
            }
        });
    }
    // The break path awards experience orbs at the **centre** of the broken
    // cell, not at the jittered positions its item drops use.
    //
    // Gated on `drop_loot` for the same reason the loot above is: the world drop
    // rule controls the entire break reward path. It is deliberately **not** gated
    // on `drops_are_allowed` — tool validation controls item drops, while the
    // break reward is evaluated for every destroyed block,
    // so breaking coal ore with a bare hand yields no coal and still yields the XP.
    // This keeps item-drop validation separate from the experience reward.
    //
    // No enchantment is modelled here, so no tool-specific experience modifier
    // is applied.
    if drop_loot {
        let points = crate::experience::block_break_points(broken.block().name(), |bound| {
            drops_rng.next_int(bound)
        });
        if points > 0 {
            let centre = Vec3::new(
                f64::from(pos.x) + 0.5,
                f64::from(pos.y) + 0.5,
                f64::from(pos.z) + 0.5,
            );
            mobs.with(|sim| {
                sim.award_experience(centre, Vec3::new(0.0, 0.0, 0.0), points);
            });
        }
    }
    let drops_elapsed = started.elapsed();
    let removed = block_entities.with(|reg| reg.remove(pos));
    // A broken hive lets every bee out at once, at the hive's own cell.
    if let Some(crate::block_entities::BlockEntity::Beehive(mut hive)) = removed {
        let flower = hive.flower();
        let facing = crate::mobs::bees::hive_facing(broken);
        let released = hive.release_all();
        mobs.with(|sim| {
            for bee in &released {
                sim.release_bee(pos, facing, true, bee, flower, false);
            }
        });
    }
    if open_container.as_ref().is_some_and(|open| open.pos == pos) {
        *open_container = None;
        *container_sync = ContainerSync::default();
    }
    // Fluid spread's seeding hook (`crate::fluid`). Breaking a block is the
    // single most common way a player starts a fluid moving — mine the floor of
    // an ocean, or the block beside a spring — and it is exactly the
    // neighbor-changed case: the *water* did not change, so only a notification
    // can wake it. `ticks_after_edit` reads this cell and its six neighbours to
    // decide which of them already hold a fluid, and schedules only those.
    //
    // Deliberately **not** folded into `propagate_placement`, whose return value
    // several gates assert on exactly. This is its own request against the same
    // feed, and `run_tick_loop`'s rebase loop routes it to the fluid queue.
    block_ticks.request_fluid_scheduled_ticks(crate::fluid::ticks_after_edit(
        source,
        fluid_env_at(source, pos),
        pos,
    ));
    let directive = proto.encode_block_update(pos.x, pos.y, pos.z, new_state);
    apply(conn, state, directive).await?;
    // Breaking a light source has to darken the column, and the `BLOCK_UPDATE`
    // above carries no light. See `crate::light` for why this is a column resend
    // rather than a `LIGHT_UPDATE`. `new_state` rather than a hardcoded `AIR`
    // for the same reason as the write above: a broken waterlogged block keeps
    // a light-dampening fluid in the cell, not empty air.
    resend_column_for_light(
        conn,
        proto,
        source,
        state,
        pending_relights.as_deref_mut(),
        broken,
        new_state,
        pos,
    ).await?;
    let light_elapsed = started.elapsed();

    // A break runs two neighbour passes: shape recomputation (a torch or rail
    // that loses support turns to air) followed by redstone and gravity
    // reactions. The shape pass precedes the neighbour-notification pass.
    let mut collapsed = collapse_unsupported(source, pos);
    // Portal validation is a *second* shape pass, alongside
    // `block_support`'s survives table
    // `collapse_unsupported` already runs above — a broken frame block must
    // extinguish the portal cells it was holding up, which
    // `collapse_unsupported` cannot see (a portal is not "supported by one
    // specific neighbour"; it is re-validated against its whole frame). See
    // `crate::portal::extinguish_broken_frames`'s own doc comment. Extends
    // `collapsed` (same `(pos, state_before)` shape) rather than a second
    // list, so the `block_update`/relight/fan-out code below needs no new
    // branch to reach it.
    if let Some(dimension) = source.dimension() {
        collapsed.extend(crate::portal::extinguish_broken_frames(source, dimension, pos));
    }
    // The update-or-destroy → destroy-block → drop-resources chain.
    //
    // **Gated on `cascade_drops`, not on `drop_loot`.** The creative no-drop
    // applies only to the block *the player broke*, while a cell that
    // self-destructs has no player context. A creative player mining the dirt
    // under a flower therefore gets the flower; reusing `drop_loot` here would
    // silently eat it.
    //
    // The tool is not consulted either: the update-or-destroy routine reaches the
    // three-argument drop-resources call, which carries no
    // tool loot-context parameter — hence `None` rather than `held`, and no
    // `drops_are_allowed` call.
    if cascade_drops {
        for (cell, was) in &collapsed {
            let popped = crate::block_drops::drop_block_loot(
                crate::block_drops::bundled_tables(),
                *was,
                *cell,
                None,
                drops_rng,
            );
            if popped.is_empty() {
                continue;
            }
            mobs.with(|sim| {
                for drop in popped {
                    let count = u8::try_from(drop.stack.count).unwrap_or(u8::MAX);
                    sim.spawn_item(
                        &drop.stack,
                        drop.position,
                        drop.velocity,
                        ItemLifecycle::newly_dropped(count, DEFAULT_MAX_STACK_SIZE),
                    );
                }
            });
        }
    }
    let mut fanned: Vec<(BlockPos, StateId)> = Vec::new();
    // Vanilla's own tripwire-block affect-neighbors-after-removal routine — the "the string just broke"
    // instant pulse. `broken` is `pos`'s own state from *before* this function
    // overwrote it, exactly what `propagate_removal_with_entities` needs; a
    // no-op for every block that is not a tripwire.
    {
        let (mut changed, scheduled) = propagate_removal_with_entities(source, pos, broken);
        block_ticks.request_scheduled_ticks(scheduled);
        fanned.append(&mut changed);
    }
    let mut fan_origins: Vec<BlockPos> = vec![pos];
    fan_origins.extend(collapsed.iter().map(|(cell, _)| *cell));
    for origin in fan_origins {
        let (mut changed, scheduled) = propagate_placement_with_entities(source, origin, Some(block_entities));
        block_ticks.request_scheduled_ticks(scheduled);
        fanned.append(&mut changed);
    }
    // The collapsed cells and then whatever the fan-out rewrote, deduped and with
    // `pos` excluded (it already had its own `block_update` above).
    let mut notify: Vec<BlockPos> = Vec::new();
    for cell in collapsed
        .iter()
        .map(|(cell, _)| *cell)
        .chain(fanned.iter().map(|(cell, _)| *cell))
    {
        if cell != pos && !notify.contains(&cell) {
            notify.push(cell);
        }
    }
    for cell in notify {
        let current = source.block_state_id(cell.x, cell.y, cell.z);
        let directive = proto.encode_block_update(cell.x, cell.y, cell.z, current);
        apply(conn, state, directive).await?;
        block_ticks.request_fluid_scheduled_ticks(crate::fluid::ticks_after_edit(
            source,
            fluid_env_at(source, cell),
            cell,
        ));
    }
    // A popped torch or lantern has to darken its column too. `should_relight`
    // compares the two states' emission and dampening, so a collapsed flower
    // costs nothing here. Re-read rather than assume `AIR`: `collapse_unsupported`
    // may have left a fluid's legacy block behind, which dampens light
    // differently than empty air.
    for (cell, was) in &collapsed {
        let now = source.block_state_id(cell.x, cell.y, cell.z);
        resend_column_for_light(
            conn,
            proto,
            source,
            state,
            pending_relights.as_deref_mut(),
            *was,
            now,
            *cell,
        ).await?;
    }
    let total = started.elapsed();
    if total >= Duration::from_millis(50) {
        tracing::warn!(
            target: "lodestone_server::stall",
            x = pos.x,
            y = pos.y,
            z = pos.z,
            total_ms = total.as_millis(),
            edit_ms = edit_elapsed.as_millis(),
            drops_ms = drops_elapsed.saturating_sub(edit_elapsed).as_millis(),
            light_ms = light_elapsed.saturating_sub(drops_elapsed).as_millis(),
            fanout_ms = total.saturating_sub(light_elapsed).as_millis(),
            "block break delayed the connection loop",
        );
    }
    Ok(())
}

/// Vanilla's own `maxChainedNeighborUpdates` for the support cascade specifically.
///
/// The tallest real chain is a bamboo or sugar-cane column (16 at the very most)
/// or a two-cell door, so this is a runaway guard rather than a behavioural
/// limit — but it has to exist, because [`collapse_unsupported`] re-queues the
/// neighbours of every cell it removes and a data error in
/// [`crate::block_support`] would otherwise walk the world.
pub(super) const MAX_SUPPORT_COLLAPSE: usize = 64;

/// Runs vanilla's `updateNeighbourShapes` self-destruct pass around `origin`,
/// transitively: every cell whose support [`crate::block_support`] models and
/// whose support cell is now gone becomes air, drops its loot, and has its own
/// neighbours re-examined.
///
/// Returns `(pos, state_before)` for each removed cell, already written to air in
/// `source`, so the caller can send the `block_update`s, roll the loot and
/// relight. The drops are deliberately **not** rolled here: this function needs no
/// `MobHandle` and no RNG, which is what lets `crate::support_collapse_gate` drive
/// the production cascade against a rig world rather than a copy of it.
pub(crate) fn collapse_unsupported<S>(source: &S, origin: BlockPos) -> Vec<(BlockPos, StateId)>
where
    S: ChunkSource + ?Sized,
{
    let mut removed: Vec<(BlockPos, StateId)> = Vec::new();
    let mut queue: VecDeque<BlockPos> = crate::neighbor_update::ALL_DIRECTIONS
        .iter()
        .map(|d| d.relative(origin))
        .collect();
    while let Some(cell) = queue.pop_front() {
        if removed.len() >= MAX_SUPPORT_COLLAPSE {
            tracing::warn!(
                "support collapse from {origin:?} hit its {MAX_SUPPORT_COLLAPSE}-cell bound"
            );
            break;
        }
        if removed.iter().any(|(seen, _)| *seen == cell) {
            continue;
        }
        let was = source.block_state_id(cell.x, cell.y, cell.z);
        if crate::chunk::is_air_or_fluid_id(was) {
            continue;
        }
        if crate::block_support::survives(cell, was, |probe| {
            source.block_state_id(probe.x, probe.y, probe.z)
        }) {
            continue;
        }
        // Removing a block with a fluid state writes that fluid's block state
        // rather than literal air. A waterlogged sign therefore leaves its
        // water source behind when the support block collapses; see
        // `destroy_block`'s `new_state` for the same rule.
        let new_state = crate::fluid::fluid_state_of_id(was)
            .map(crate::fluid::FluidState::block_state_id)
            .unwrap_or_else(|| Block::Air.default_state());
        source.set_block(cell.x, cell.y, cell.z, new_state);
        removed.push((cell, was));
        // The removed cell's own neighbours: this is what makes a stack of sugar
        // cane collapse all the way up, and a door's upper half follow its lower.
        for direction in crate::neighbor_update::ALL_DIRECTIONS {
            queue.push_back(direction.relative(cell));
        }
    }
    removed
}

/// Sends every world-tick block mutation promptly and queues lighting once per
/// affected delivered column.
///
/// This deliberately has no join-stream gate. A column snapshot that has not
/// been sent yet will supersede an earlier block update, but a snapshot that
/// was already sent will not. Dropping the shared feed while a later join
/// column remains would therefore leave an already-visible column stale.
pub(super) async fn send_tick_block_updates<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    delivered: &HashSet<(i32, i32)>,
    pending_relights: &mut PendingRelights,
    changes: Vec<crate::tick::TickBlockChange>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    if changes.is_empty() {
        return Ok(());
    }
    let started = lodestone_time::Instant::now();
    let change_count = changes.len();
    let radius = i32::from(proto.uses_cross_column_light());
    for change in &changes {
        let (x, y, z, block_state) = (change.x, change.y, change.z, change.state);
        let column = (x.div_euclid(16), z.div_euclid(16));
        if delivered.contains(&column) {
            apply(conn, state, proto.encode_block_update(x, y, z, block_state)).await?;
        }
    }
    let queued_relight_count = pending_relights.enqueue_batch(tick_relight_targets(
        changes.iter().filter(|change| change.needs_relight).map(|change| {
            (change.x.div_euclid(16), change.z.div_euclid(16))
        }),
        delivered,
        radius,
    ));
    let total = started.elapsed();
    if total >= STALL_REPORT {
        tracing::warn!(
            target: "lodestone_server::stall",
            change_count,
            queued_relight_count,
            pending_relight_count = pending_relights.len(),
            total_millis = total.as_millis() as u64,
            "tick block updates stalled the connection loop",
        );
    }
    Ok(())
}

pub(super) const TICK_BLOCK_UPDATE_BATCH: usize = 64;

pub(super) fn queue_tick_block_updates(
    pending: &mut VecDeque<crate::tick::TickBlockChange>,
    delivered: &HashSet<(i32, i32)>,
    changes: Vec<crate::tick::TickBlockChange>,
) {
    pending.extend(changes.into_iter().filter(|change| {
        delivered.contains(&(change.x.div_euclid(16), change.z.div_euclid(16)))
    }));
}

pub(super) async fn send_pending_tick_block_updates<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    delivered: &HashSet<(i32, i32)>,
    pending_relights: &mut PendingRelights,
    pending: &mut VecDeque<crate::tick::TickBlockChange>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    let count = pending.len().min(TICK_BLOCK_UPDATE_BATCH);
    let changes = pending.drain(..count).collect();
    send_tick_block_updates(conn, proto, state, delivered, pending_relights, changes).await
}
