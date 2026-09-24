# Worldgen coral features

## What it is

The coral feature module places the three configured warm-ocean geometries: a branching tree, a claw-shaped set of branches, and a hollow shell-like mushroom. They share the registry-selected coral-block state and water survival rules.

## How it works

`place_coral` consumes the caller-owned random stream, selecting one coral block before dispatching to the requested geometry. Each successful block requires water at the target's top face, writes the coral block, then performs the plant-or-sea-pickle decoration roll and four ordered wall-fan rolls. The tree chooses a 1–3 block trunk and 2–4 shuffled branches; the claw chooses 2–3 shuffled side branches; the mushroom samples dimensions 3–5 and a 1–3 block sink, then walks its shell in x/y/z order. `VegGrid::set_if_in_bounds` clips spill at the caller's footprint boundary.

The module-local compiled-server fixture test covers exact geometry, state properties, direction order, and random draw order for seed 11. The survival controls demonstrate that water at both the origin and the block above is required; the clipping control demonstrates that cross-boundary writes are dropped without changing the caller's random stream.

## How to change it

Keep the shared helper's draw order stable because later configured features consume the same stream. Add geometry in a dedicated body and extend `CoralKind`; the parent vegetation parser and dispatcher own the configured-feature seam. Keep state strings canonical (`facing` before `waterlogged` on wall fans, and `waterlogged=true` on living coral plants/fans).

## Configuration

The configured-feature identifiers are `coral_tree`, `coral_claw`, and `coral_mushroom`. They have no JSON parameters. The five coral blocks, ten top decorations, and five wall fans come from the bundled protocol-776 block tags.

## Dependencies

The implementation depends on `VegGrid`, `BlockPos`, and the shared `RandomSource` trait. It is consumed by the vegetation configured-feature parser and dispatcher and does not seed, fork, or own the caller's random source.
