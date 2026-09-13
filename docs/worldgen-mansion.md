# Woodland mansion assembly

## What it is

`structure::mansion` builds the complete template-piece list for a woodland mansion. The seeded plan, exterior shell, corridors, room dividers, doors, carpets, stairs, secret rooms, furnishings, and roof layers all reach the normal template placement stage; entity data markers remain a server-side consumer concern.

## How it works

`mansion::generate` consumes the structure start's already-selected rotation and random stream. It builds an 11 by 11 seeded lower-floor topology, classifies each floor's one-cell, one-by-two-cell, and two-by-two-cell rooms, derives the shorter third-floor topology from the second-floor stairs room, and turns exposed cell edges into wall, carpet, divider, door, room, and roof template placements. Room variants and orientations are selected from the same stream as the topology. Every returned `StructurePiece` has a `PiecePlacement`, so the normal structure-placement stage clips and writes the bundled template blocks into chunk grids.

The module returns `MansionAssembly` rather than a bare vector. Its `coverage()` is `CompleteTemplates`, so the registry treats the mansion structure as supported. A missing required template returns `MissingTemplate` before any piece is emitted, preventing an incomplete asset bundle from looking like a valid empty or reduced mansion.

## How to change it

Keep the room classifier and piece-placement loops in lockstep with the grid flags: room ids identify shared walls, the origin and door flags choose the entrance side, and the stairs flag selects the third-floor connection. Add every newly reachable template to `TEMPLATE_IDS`; the registry's eager template loading must call `mansion::template_ids()` or `generate` will return `MissingTemplate`.

Keep all output as `template_piece` records. Do not write blocks in the planner: the per-chunk structure placement stage owns clipping, block processors, and the block-entity handoff. When changing room selection or traversal, update the seeded piece-count and clipped-write fixture; those values are independent controls for RNG/order drift and template placement drift.

## Configuration

There are no mansion-specific flags. The world seed, start chunk, selected rotation, terrain-adjusted start height, and the normal structure placement settings determine output. Templates are loaded from `assets/structure/woodland_mansion/` through the resolver.

## Dependencies

The assembler depends on `TemplateStore`, `template_piece`, `Rotation`, `PlaceSettings`, and the existing template processor and placement path. It does not use jigsaw pools or a separate block writer.
