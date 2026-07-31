//! End-to-end: the **real** `lodestone-client`, running the real
//! [`V770Adapter`], digging and placing against the real
//! [`V770ServerProtocol`] over the in-memory transport — proving a served
//! world can actually be changed, not just walked around in
//! (`docs/block-edit.md`).
//!
//! Terrain here is the **real** [`OverworldChunkSource`] (unlike
//! `server_liveness.rs`'s cheap gradient stand-in): the whole point of this
//! file is `OverworldChunkSource`'s new edit-retention cache, which
//! `WorldgenChunkSource` does not have (its `ChunkSource::set_block` is the
//! trait's no-op default — see `crates/lodestone-server/src/chunk.rs`'s
//! module docs). Seed `1234`, chunk `(0, 0)` — the coordinates and their
//! pre-edit content are pinned by `set_up`'s own doc comment below and cross
//! -checked against `lodestone-server`'s hermetic
//! `set_block_persists_across_repeated_column_calls` test, which asserts the
//! same fixture without the network/client machinery.

use std::time::Duration;

use lodestone_client::{BlockPos, ChunkPos, ClientAction, ClientBuilder, Hand, LoginProfile, ServerAddress};
use lodestone_model::{BlockActionKind, BlockFace, Rotation, Vec3f};
use lodestone_server::{IntegratedServer, overworld_chunk_source};
use lodestone_data::block_states::block_name;
use lodestone_v770::{V770ServerProtocol, adapter};

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

/// The registry id for block-state name `name` (propertyless states only —
/// exactly the two this test cares about), found the same way
/// `server_protocol.rs`'s own `stone_id`/`air_id` do: a linear scan by name
/// over the generated table, rather than a literal id that could silently
/// desync from a regenerated table.
fn state_id(name: &str) -> u32 {
    (0..)
        .find(|&id| block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("generated block-state table has no `{name}` entry"))
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
///   used for the break half.
/// * `(0, 37, 0)` — **`minecraft:gravel`**, the single-block ocean floor at
///   this column.
/// * `(0, 38, 0)` — **`minecraft:water`**, directly above the gravel (this
///   column is a submerged one, matching `worldgen_data`'s own test fixture
///   notes about chunk `(0, 0)` at nearby seeds) — the placement target once
///   the gravel's `Up` face is clicked, since water is replaceable.
///
/// # A discovered wire-fidelity gap this test works around, not fixes
///
/// `V770ServerProtocol::encode_chunk`'s `build_world_column` (pre-existing,
/// not touched by this change) collapses every *solid* block in a whole
/// -column send to a single `minecraft:stone`, and everything non-solid
/// (air **and every fluid**) to air — it only ever writes `ChunkSection
/// ::set_block(…, stone)` under an `is_solid` check, never any other state.
/// So a real client's chunk store, via `handle.block_at`, cannot see
/// `deepslate`/`gravel`/`water` at all — only "solid" (stone) or "not"
/// (air), for *any* full-column send, edited or not. This is unrelated to
/// block editing (`ServerProtocol::encode_block_update` resolves the real
/// state string correctly, via `resolve_state_id` — that path has full
/// fidelity), and is not fixed here: it is a pre-existing limitation of the
/// bulk terrain encoder, orthogonal to this task's scope and risky to change
/// inside this change (it touches the whole-column path every client join
/// and every view-tracker resend uses). See this crate's task report.
///
/// So the *client*-observable pre-edit state at all three cells is only
/// ever `stone` (solid) or `air` (not) — asserted below — while the
/// *real* per-block content above is asserted independently, straight off
/// the generator, so "what terrain was actually there" is still on record
/// even though the wire cannot show it. Both a stone→air (break) and an
/// air→stone (place) transition are still real, observable, non-vacuous
/// edits: neither cell already showed the post-edit value before it was
/// touched.
#[tokio::test]
async fn dig_and_place_persist_through_forget_and_reload() {
    let seed: i64 = 1234;
    let view_radius = 0; // one column only — keeps the real generator's cost down.

    // The real per-block content, independent of the client-visible wire
    // encoding discussed above — this is "what terrain was actually there".
    let generator = lodestone_server::overworld_generator(seed);
    let real_column = generator.column(0, 0);
    assert_eq!(
        real_column.block_state(0, -50, 0).split('[').next(),
        Some("minecraft:deepslate")
    );
    assert_eq!(real_column.block_state(0, 37, 0), "minecraft:gravel");
    assert_eq!(
        real_column.block_state(0, 38, 0).split('[').next(),
        Some("minecraft:water")
    );

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

    let break_pos = BlockPos::new(0, -50, 0);
    let clicked_pos = BlockPos::new(0, 37, 0);
    let target_pos = BlockPos::new(0, 38, 0); // clicked_pos.relative(Up)

    let stone_id = state_id("minecraft:stone");
    let air_id = state_id("minecraft:air");

    // --- World-species control, at the fidelity the wire actually offers
    // (see the doc comment above): the two solid cells (deepslate, gravel)
    // read as `stone`; the fluid cell (water) reads as `air`. None of the
    // three already matches its post-edit value.
    let break_pre = handle.block_at(break_pos).expect("break column loaded");
    assert_eq!(
        break_pre, stone_id,
        "expected the (real: deepslate) break cell to read as solid/stone pre-edit, got {}",
        base_name_at(&handle, break_pos)
    );
    let clicked_pre = handle.block_at(clicked_pos).expect("clicked column loaded");
    assert_eq!(
        clicked_pre, stone_id,
        "expected the (real: gravel) clicked cell to read as solid/stone pre-edit, got {}",
        base_name_at(&handle, clicked_pos)
    );
    let target_pre = handle.block_at(target_pos).expect("target column loaded");
    assert_eq!(
        target_pre, air_id,
        "expected the (real: water) target cell to read as non-solid/air pre-edit, got {}",
        base_name_at(&handle, target_pos)
    );

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
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: clicked_pos,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 1.0, 0.5),
            inside_block: false,
            sequence: 3,
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
    // rather than always writing to the clicked position. Note this is not
    // `Some(clicked_pre)`: vanilla's own `handleUseItemOn` unconditionally
    // sends a `block_update` for the clicked cell too (this module's own
    // `apply_use_item_on` mirrors that), and unlike the whole-column send,
    // `encode_block_update` carries the block's *real* resolved id — so the
    // clicked cell's client-visible state upgrades here from the wire
    // -collapsed `stone` to the real `gravel` id, even though the server
    // never wrote a new value there. That is the correct, intended effect of
    // sending the confirmation, not a bug.
    let gravel_id = state_id("minecraft:gravel");
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
    // the original generator output (real: deepslate, so still `stone` at
    // the wire's solid/air fidelity — see this test's own doc comment) —
    // the edit is scoped to what was actually touched, not a side effect
    // that corrupted the whole column on regeneration. The *exact* string
    // -level claim ("still literally deepslate, not just still solid") is
    // what `lodestone-server`'s hermetic
    // `set_block_persists_across_repeated_column_calls` test (same fixture,
    // no client/wire involved) checks instead.
    let untouched_pos = BlockPos::new(2, -50, 0);
    assert_eq!(
        real_column.block_state(2, -50, 0).split('[').next(),
        Some("minecraft:deepslate"),
        "fixture assumption broke: expected deepslate at {untouched_pos:?}"
    );
    assert_eq!(
        handle.block_at(untouched_pos),
        Some(stone_id),
        "an untouched cell in the edited column changed too after the reload — got {}",
        base_name_at(&handle, untouched_pos)
    );

    server.shutdown().await;
}
