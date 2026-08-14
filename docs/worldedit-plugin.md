# Bulk-edit (WorldEdit-class) region API

## What it is

`crates/plugins/lodestone-worldedit` — a WorldEdit-class plugin: region fill/replace with a
per-session undo/redo stack, built entirely on top of `lodestone_world::World::fill_region_capturing` and
`lodestone_ecs::ChunkWorldWrite`, with no engine change of its own beyond the batched-write primitive
that backs it. "This is a *second* plugin, conceptually — WorldEdit itself is a plugin in the Java
ecosystem, not a server feature"; every region-selection/undo-shaped concern
lives here rather than in `lodestone-world` or `lodestone-ecs`.

## How it works

### The block read/write API this is built on

`lodestone_world::World` gained four methods for this pass:

- `block_state_at(x, y, z) -> Option<u32>` — the read half `set_block` never had a counterpart for.
- `set_block_with_physics(x, y, z, state, physics: bool) -> Option<u32>` — the packaged
  `set_block(pos, state, physics)` API, returning the previous state (what an undo
  history captures). `physics: false` is exactly `set_block`; `physics: true` additionally queues the six
  orthogonally-adjacent positions onto a `pending_physics_updates` queue (mirroring the existing
  `pending_relight` queue's "record rather than act on" shape) for a future block-tick/neighbour-update
  system — **there is no such system yet** (Tier 4, `docs/backlog.md`), so this ships the `physics: false`
  path now with a clearly marked queue as the TODO anchor for the `true` path, exactly as the issue's own
  scope note asks.
- `fill_region(min, max, state) -> usize` / `fill_region_capturing(min, max, state) -> Vec<(x, y, z, previous_state)>` —
  the batched primitive. Groups writes by chunk (one `HashMap::get_mut` per touched *column*, not per
  block) rather than the per-position lookup a loop over `set_block` would pay.

**Where the batching actually helps, re-derived rather than assumed.** The scoping question was whether the
existing write API supports batching efficiently, or whether a batched entry point is needed "to avoid
re-acquiring the chunk lock per block". Checked against the tree: there is exactly **one** lock for the
whole store (`ChunkWorldWrite`'s `std::sync::RwLock<World>`), taken once by whoever calls
`ChunkWorldWrite::write()` — so the lock itself was never the per-block cost, and a plugin holding the
guard for a whole edit already avoids re-acquiring it. The real cost `fill_region`/`fill_region_capturing`
remove is the repeated `HashMap::get_mut` + `section_index` bounds check a naive per-block loop pays.

**Measured** (release build, `cargo test -p lodestone-world --lib fill_region_lock_hold_time --release -- --ignored --nocapture`):
a 128×128×128 fill (2,097,152 blocks) across an 81-column, full-height loaded area completed in **43.8 ms**
(~20.9 ns/block) — a single-threaded, in-process `HashMap`/palette write, with the write guard held for
the whole call. The test is `#[ignore]`d per `CLAUDE.md`'s duration-measurement guidance (a timing on a
shared, possibly-loaded machine is not a reliable CI assertion); the committed assertion is a wide sanity
ceiling (10s), not a regression gate, and the number above is what should be quoted, not the assertion.

### `EditSession` (`src/session.rs`)

Holds a `ChunkWorldWrite` handle, an undo stack and a redo stack (each `Vec<Vec<(i32,i32,i32,u32)>>`,
capped at `MAX_UNDO_DEPTH = 64`, mirroring `PENDING_RELIGHT_CAP`'s "must not grow unbounded" reasoning).

- `fill(selection, state, physics)` — calls `fill_region_capturing`, pushes the returned record as one
  undo entry. `physics: true` additionally queues the six neighbours of every *written* position (not just
  the selection's edge), matching what a loop of single `set_block_with_physics(..., true)` calls would
  have queued — this is asserted directly (`fill_with_physics_true_queues_neighbour_updates`), because it
  is exactly the kind of detail a batched rewrite of a single-block operation can silently drop.
- `replace(selection, from, to)` — since there is no predicate-write primitive in `lodestone-world` today,
  this reads each position and rewrites only matches through the single-block API. A real, if modest,
  inefficiency relative to a hypothetical `World::replace_region`, named here rather than hidden.
- `undo()`/`redo()` — both call the same `replay_record` helper (write each recorded position, capturing
  what was there as the new opposite-direction record), so undo and redo are the *same function* applied
  twice rather than two write paths that could drift. A fresh edit after an undo clears the redo stack
  (`a_new_edit_after_undo_clears_the_redo_chain`), matching every real editor including WorldEdit itself.
- An empty write (e.g. a selection entirely outside loaded chunks) pushes **no** undo entry — otherwise a
  no-op "edit" would silently consume an undo slot a real edit could have used.

### `WorldEditPlugin` (`src/lib.rs`) — the real consumer

`EditSession` alone is a library any test can call directly; `WorldEditPlugin` is what proves it is wired
into a real schedule rather than only unit-tested as a function. It installs `EditSessions` (one
`EditSession` per `session_key`, created lazily) and `FillRequests` (a plain `Vec` resource, the same
shape `lodestone_ecs::player::ActionQueue` uses — see `docs/plugin-api.md`'s note on why a `Vec` resource
won over a bevy `Message` for a case needing synchronous drain-time application), and a `GameTick` system
draining the latter into the former.

`tests/drives_a_real_fill_through_the_schedule.rs` is the end-to-end gate: a real `App` with
`lodestone_ecs::CorePlugin` + `WorldEditPlugin`, a queued `FillRequest`, a real `run_schedule(GameTick)`
call, and the assertion reads through the **read** handle (`ChunkWorld`) — the same handle a real mesher
would hold — not the write handle the plugin itself used. A second test confirms two different
`session_key`s get independent undo histories.

## How to change it, and the gotchas

- **`replace` is not batched the way `fill` is** — see the note above. If profiling ever shows this
  matters, the fix is a predicate-write primitive in `lodestone-world` (`World::replace_region`), not a
  workaround here.
- **`undo`/`redo` must stay the same function.** Do not special-case one direction — the whole point of
  `replay_record` returning the opposite-direction record is that a bug in one direction shows up in both.
- **A `FillRequest`'s `session_key` is caller-defined** — this crate does not interpret it as a player id
  or anything else. A real chat-command plugin embedding this would key by the issuing player's
  `MinecraftEntityId.0`.
- **`WorldEditPlugin` requires a `ChunkWorldWrite` resource to already be installed** — it does not build
  a chunk store of its own, matching `drive_placement`/`lodestone-autopilot`'s own read side.

## Configuration

None. `MAX_UNDO_DEPTH` (64) is a compile-time constant; there is no runtime-configurable undo depth.

## Dependencies

`lodestone-world` (`World::fill_region_capturing`, `block_state_at`, `set_block_with_physics`),
`lodestone-ecs` (`ChunkWorldWrite`, `GameTick`), `bevy_ecs`/`bevy_app` (direct, for the derive macros —
see `docs/plugin-api.md`'s "how to change it").

## See also

- [`docs/plugin-api.md`](./plugin-api.md) — the plugin ABI, and the `ActionQueue` reasoning
  `FillRequests` mirrors.
- [`docs/plugin-data-and-config.md`](./plugin-data-and-config.md) — the sibling crate for the plugin
  config convention and the persistent data container.
- [`docs/world-unification.md`](./world-unification.md) — the chunk-store lock discipline this plugin's
  batching claim is checked against.
