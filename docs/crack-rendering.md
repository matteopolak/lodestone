# Block-destruction crack rendering

## What it is

The block-destruction overlay draws the server-reported progress of every
breaking entity on top of the corresponding block face. It is a renderer-owned
visual effect, while the progress and its lifecycle are folded into the shared
session entity so headless clients and the shell observe the same state.

## How it works

The version adapter emits `ClientEvent::BlockDestruction` with the breaking
entity id, block position, and stage. `SessionPlugin` folds that event into
`SessionBlockDestruction`, whose `BlockDestructionOverlays` collection keeps one
entry per breaking entity. A new position replaces that entity's old entry and a
stage outside `0..=9` removes it.

Each frame, the shell's simulation render source enumerates the collection and
resolves the current block state from the shared world. `CrackResolver` turns
the state's model quads into crack geometry and `CrackPipeline` draws all
resolved targets in the crack pass. The pass has its own atlas sampler:
magnification uses nearest filtering so enlarged crack texels remain discrete;
minification and mip transitions use the existing linear policy.

Lifecycle cleanup is explicit at every boundary that can invalidate a target:

- entity removal and replacement clear the entry by wire entity id in the
  session fold while the ingest systems update the ECS entity index;
- chunk unload clears entries whose positions belong to that chunk, including
  negative coordinates using floor division; and
- login, respawn, disconnect, and session failure clear the remaining session
  collection.

Chunk unload and the two terminal events are shell-routed rather than
session-folded, so `SharedState::apply` performs their cleanup at the event
broker before forwarding them. This keeps the renderer state correct even when
a transport ends before a progress reset arrives.

## How to change it

Change the data fold in `lodestone-game::mining::BlockDestructionOverlays`, and
keep its entity, chunk, and session cleanup methods independent so each
lifecycle owner can remove only the state it invalidated. Change ECS wiring in
`lodestone-ecs::session` (the single overlay writer); change broker cleanup in
`lodestone-client::state::SharedState` when an event remains outside the session
route.

Change geometry or atlas binding in `lodestone-render::crack_resolver` and
`lodestone-render::crack_pipeline`. Keep the crack sampler separate from the
terrain sampler. The ignored GPU gates in
`crates/lodestone-render/tests/terrain/crack_gate.rs` check stage pixels at
specific locations, localization, cleanup, and blend behavior; run them with a
real adapter when changing shader, atlas, or pass state.

## Configuration

There is no runtime flag for crack rendering. The atlas dimensions and stage
regions come from the loaded resource pack; sampler filtering is defined by
`CrackPipeline` and is intentionally not inherited from terrain bindings.

## Dependencies

The fold depends on `lodestone-model::ClientEvent` and
`lodestone-game::mining::BlockDestructionOverlays`. ECS lifecycle consumers use
`SessionBlockDestruction`, `EntityIndex`, and the shared `World`. Rendering uses
`CrackResolver`, `CrackPipeline`, `GpuAtlas`, and the block model data supplied
by the active resource pack.
