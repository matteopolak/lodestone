//! End-to-end: the **real** `lodestone-client`, running the real
//! [`V770Adapter`], digging and placing against the real
//! [`V770ServerProtocol`] over the in-memory transport — proving a served
//! world can actually be changed, not just walked around in
//! (`docs/block-edit.md`).
//!
//! Terrain here is the **real** [`OverworldChunkSource`] (unlike
//! `server_liveness.rs`'s cheap gradient stand-in): the whole point of this
//! file is `OverworldChunkSource`'s new edit-retention cache, which
//! `WorldgenChunkSource` does not have (its `ChunkSource::set_block` is a
//! `todo!()` — a solidity-only source with no retention — see
//! `crates/lodestone-server/src/chunk.rs`'s module docs). Seed `1234`, chunk
//! `(0, 0)` — the coordinates and their
//! pre-edit content are pinned by `set_up`'s own doc comment below and cross
//! -checked against `lodestone-server`'s hermetic
//! `set_block_persists_across_repeated_column_calls` test, which asserts the
//! same fixture without the network/client machinery.

use std::time::Duration;

use lodestone_client::{BlockPos, ChunkPos, ClientAction, ClientBuilder, Hand, LoginProfile, ServerAddress};
use lodestone_model::{BlockActionKind, BlockFace, GameMode, ItemStack, Rotation, Vec3f};
use lodestone_server::{IntegratedServer, overworld_chunk_source};
use lodestone_data::block::Block;
use lodestone_data::block_states::block_name;
use lodestone_v26_2::{V770ServerProtocol, adapter};

fn profile(name: &str) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid: uuid::Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// The base block name (properties stripped) `lodestone-client`'s `block_at`
/// reports at `pos`, or a message noting the block/chunk was not loaded —
/// used only for assertion failure messages, not the assertions themselves.
fn base_name_at(handle: &lodestone_client::ClientHandle, pos: BlockPos) -> String {
    match handle.block_at(pos) {
        Some(id) => block_name(id)
            .unwrap_or("<unknown id>")
            .split('[')
            .next()
            .unwrap_or("<unknown id>")
            .to_string(),
        None => "<not loaded>".to_string(),
    }
}

/// A served world you can actually mine and build in: dig a real block,
/// place a real block, and — the persistence proof — see both survive the
/// column being forgotten and re-sent from the server's own retained state,
/// not merely see the confirming packet arrive once.
///
/// # Fixture: seed `1234`, chunk `(0, 0)`
///
/// Sampled directly from `OverworldGenerator::column(0, 0)` before writing
/// this test (this generator runs no carvers — `worldgen_data`'s own "no
/// caves/ores/trees" scope note — so a deep column like this is
/// deterministic and always solid), and re-asserted below straight off a
/// second, independent generator instance so this doc comment cannot drift
/// from what the test actually checks:
///
/// * `(0, -50, 0)` — **`minecraft:deepslate[axis=y]`**, deep underground,
///   used as an untouched reload sentinel.
/// * `(0, 37, 0)` — **`minecraft:gravel`**, the single-block ocean floor at
///   this column, used for the break half.
/// * `(0, 38, 0)` — **`minecraft:water`**, directly above the gravel (this
///   column is a submerged one, matching `worldgen_data`'s own test fixture
///   notes about chunk `(0, 0)` at nearby seeds) — the placement target once
///   the gravel's `Up` face is clicked, since water is replaceable.
///
/// # A formerly-collapsed wire-fidelity gap, now fixed
///
/// `V770ServerProtocol::encode_chunk`'s
/// `build_world_column` collapsed every *solid* block in a whole-column send
/// to a single `minecraft:stone`, and everything non-solid (air **and every
/// fluid**) to air — it only ever wrote `ChunkSection::set_block(…, stone)`
/// under an `is_solid` check, never any other state. That meant a real
/// client's chunk store, via `handle.block_at`, could not see
/// `deepslate`/`gravel`/`water` at all — only "solid" (stone) or "not"
/// (air), for *any* full-column send, edited or not.
///
/// `build_world_column` now copies each cell's real typed state via
/// `ServerChunkColumn::block_state_id` (the same
/// [`ServerProtocol::encode_block_update`] already used for a single cell),
/// so the pre-edit assertions below check the **real** per-block content
/// directly, at full wire fidelity — no separate "what terrain was actually
/// there" shadow-check is needed anymore; the client now sees it.
///
/// [`ServerProtocol::encode_block_update`]: lodestone_server::ServerProtocol::encode_block_update
#[tokio::test]
async fn dig_and_place_persist_through_forget_and_reload() {
    let seed: i64 = 1234;
    let view_radius = 0; // one column only — keeps the real generator's cost down.

    // The real per-block content, from an independent generator instance —
    // now also exactly what the wire and `handle.block_at` are expected to
    // show, per this test's doc comment above.
    let generator = lodestone_server::overworld_generator(seed);
    let real_column = generator.column(0, 0);
    let deepslate = real_column.block_state_id(0, -50, 0);
    let gravel = real_column.block_state_id(0, 37, 0);
    let water = real_column.block_state_id(0, 38, 0);
    assert_eq!(deepslate.block(), Block::Deepslate);
    assert_eq!(gravel, Block::Gravel.default_state());
    assert_eq!(water.block(), Block::Water);
    let gravel_id = gravel.raw();
    let water_id = water.raw();

    let source = overworld_chunk_source(seed);
    let (server, client_io) = IntegratedServer::open_in_memory(V770ServerProtocol, source, view_radius);
    let (handle, _events) =
        ClientBuilder::new(address(), profile("Digger"), Box::new(adapter())).connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("initial column never arrived");

    // The server validates interaction distance before applying either break
    // phase. Move to the ocean floor before issuing the edits; the original
    // deep-deepslate coordinate was a useful state sample but was over 100
    // blocks from the fallback spawn and could therefore never be broken.
    let break_pos = BlockPos::new(0, 37, 0);
    let clicked_pos = break_pos;
    let target_pos = BlockPos::new(0, 38, 0); // clicked_pos.relative(Up)

    let stone_id = Block::Stone.default_state().raw();
    let air_id = Block::Air.default_state().raw();

    // --- World-species control: the whole-column send now carries real
    // per-block fidelity, so each cell reads as its own
    // distinct real state — deepslate, gravel, and (the fluid, the case
    // most likely to be missed by a fix that only thinks about solids)
    // water — not a collapsed stone/air pair. None of the three already
    // matches its post-edit value (stone for the placement, air for the
    // break), so a fix that accidentally left the collapse in place, or
    // one that fixed solids but still mapped fluids to air, would fail this
    // loudly rather than by coincidence.
    let break_pre = handle.block_at(break_pos).expect("break column loaded");
    assert_eq!(
        break_pre, gravel_id,
        "expected the break cell to read as real gravel pre-edit, got {}",
        base_name_at(&handle, break_pos)
    );
    let clicked_pre = handle.block_at(clicked_pos).expect("clicked column loaded");
    assert_eq!(
        clicked_pre,
        gravel_id,
        "expected the clicked cell to read as real gravel pre-edit, got {}",
        base_name_at(&handle, clicked_pos)
    );
    let target_pre = handle.block_at(target_pos).expect("target column loaded");
    assert_eq!(
        target_pre, water_id,
        "expected the target cell to read as real water pre-edit (not air — water is a \
         fluid, the case the old collapse mapped to air rather than stone), got {}",
        base_name_at(&handle, target_pos)
    );

    // The fallback spawn is intentionally above the ocean. Move down to the
    // floor before exercising the interaction wire path; the server's reach
    // check is authoritative and would correctly ignore an edit from y=64.
    handle
        .move_to(
            lodestone_model::Vec3::new(0.5, 39.0, 0.5),
            Rotation::new(0.0, 0.0),
            false,
            false,
        )
        .expect("move to the edit fixture");

    // --- Sequence control: Start then Abort must NOT break the block. There
    // is no event to wait on for "nothing happened", so a later, observable
    // action on the same ordered connection (the placement below) is used as
    // the synchronization point: once its effect is observed, everything
    // sent before it — including this Abort — is guaranteed processed.
    handle
        .send_action(ClientAction::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: break_pos,
            face: BlockFace::Up,
            sequence: 1,
        })
        .expect("send start destroy");
    handle
        .send_action(ClientAction::BlockAction {
            action: BlockActionKind::AbortDestroy,
            pos: break_pos,
            face: BlockFace::Up,
            sequence: 2,
        })
        .expect("send abort destroy");

    // --- Placement: click the gravel's Up face. Gravel is not replaceable,
    // so the target cell is the *neighbour* (0, 38, 0), not (0, 37, 0)
    // itself.
    //
    // The hotbar load is required. This test used to click with an
    // **empty hand** and still get stone, because placement wrote
    // `minecraft:stone` for anything it could not resolve — which turned out
    // to be every ordinary block, and was the bug. An item that places no
    // block now places nothing, so the subject of this test (a placed block
    // surviving a forget/reload) needs a real item in hand. Stone is kept as
    // that item precisely so nothing else about this test changes.
    handle
        .send_action(ClientAction::ChangeGameMode {
            mode: GameMode::Creative,
        })
        .expect("client still connected");
    handle
        .send_action(ClientAction::SetCreativeModeSlot {
            slot: 36,
            item: Some(ItemStack::new(
                "minecraft:stone".parse().expect("valid resource key"),
                1,
            )),
        })
        .expect("client still connected");
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: clicked_pos,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 1.0, 0.5),
            inside_block: false,
            sequence: lodestone_model::PredictionSequence::new(3),
        })
        .expect("send use item on");

    handle
        .wait_for(Duration::from_secs(30), |h| {
            h.block_at(target_pos) == Some(stone_id)
        })
        .await
        .expect("placement never landed on the target cell");

    // Now that the placement (sent after the abort) has taken effect, the
    // abort is guaranteed to have already been processed too.
    assert_eq!(
        handle.block_at(break_pos),
        Some(break_pre),
        "Start+Abort must not break the block — the sequence is not being \
         collapsed into a single event"
    );
    // The clicked cell's *block* is untouched — only its neighbour changed,
    // proving the replace-vs-relative placement-cell choice actually ran
    // rather than always writing to the clicked position. This is exactly
    // `Some(clicked_pre)`: vanilla's own `handleUseItemOn` unconditionally
    // sends a `block_update` for the clicked cell too (this module's own
    // `apply_use_item_on` mirrors that), but now that the whole-column send
    // already carries full fidelity, that confirmation
    // reconfirms the same real `gravel` id the column already showed rather
    // than upgrading it from a wire-collapsed `stone`.
    assert_eq!(
        handle.block_at(clicked_pos),
        Some(gravel_id),
        "placement must not overwrite the clicked (non-replaceable) gravel cell"
    );

    // --- Now actually break it: Start then Stop.
    handle
        .send_action(ClientAction::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: break_pos,
            face: BlockFace::Up,
            sequence: 4,
        })
        .expect("send start destroy");
    handle
        .send_action(ClientAction::BlockAction {
            action: BlockActionKind::StopDestroy,
            pos: break_pos,
            face: BlockFace::Up,
            sequence: 5,
        })
        .expect("send stop destroy");

    handle
        .wait_for(Duration::from_secs(30), |h| {
            h.block_at(break_pos) == Some(air_id)
        })
        .await
        .expect("break never landed");

    // --- The persistence proof: leave far enough that the view drops this
    // column entirely, confirm it was actually forgotten (not just still
    // cached client-side by coincidence), then come back and require the
    // column to be freshly re-sent from the server's own retained state.
    let spawn = handle.position().expect("spawned");
    handle
        .move_to(
            lodestone_model::Vec3::new(spawn.x + 160.0, spawn.y, spawn.z),
            Rotation::new(0.0, 0.0),
            true,
            false,
        )
        .expect("send move away");
    handle
        .wait_for(Duration::from_secs(30), |h| {
            !h.is_chunk_loaded(ChunkPos::new(0, 0))
        })
        .await
        .expect("edited chunk (0, 0) was never forgotten — the leave half of the round trip didn't happen");

    handle
        .move_to(spawn, Rotation::new(0.0, 0.0), true, false)
        .expect("send move back");
    handle
        .wait_for(Duration::from_secs(30), |h| {
            h.is_chunk_loaded(ChunkPos::new(0, 0))
        })
        .await
        .expect("chunk (0, 0) was never re-sent after returning");

    // The edits survived a real forget + fresh `column()` regeneration on
    // the server, not merely a client-side cache that never actually
    // dropped the column.
    assert_eq!(
        handle.block_at(break_pos),
        Some(air_id),
        "broken block reappeared after the column was re-sent from server state — \
         got {}",
        base_name_at(&handle, break_pos)
    );
    assert_eq!(
        handle.block_at(target_pos),
        Some(stone_id),
        "placed block vanished after the column was re-sent from server state — \
         got {}",
        base_name_at(&handle, target_pos)
    );
    // And an untouched cell in the very same edited column still reflects
    // the original generator output at *full* fidelity — real
    // `deepslate[axis=y]`, not merely "still solid" — the edit is scoped to
    // what was actually touched, not a side effect that corrupted the whole
    // column on regeneration. `lodestone-server`'s hermetic
    // `set_block_persists_across_repeated_column_calls` test (same fixture,
    // no client/wire involved) checks the same claim without the network
    // machinery.
    let untouched_pos = BlockPos::new(2, -50, 0);
    assert_eq!(
        real_column.block_state_id(2, -50, 0).block(),
        Block::Deepslate,
        "fixture assumption broke: expected deepslate at {untouched_pos:?}"
    );
    let untouched_deepslate_id = real_column.block_state_id(2, -50, 0).raw();
    assert_eq!(
        handle.block_at(untouched_pos),
        Some(untouched_deepslate_id),
        "an untouched cell in the edited column changed too after the reload — got {}",
        base_name_at(&handle, untouched_pos)
    );

    server.shutdown().await;
}
