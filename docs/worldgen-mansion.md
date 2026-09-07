# Woodland mansion assembly

## What it is

`structure::mansion` builds the template-piece list for the currently supported exterior of a woodland mansion. It is intentionally partial: the seeded plan, entrance, exterior walls, corridor floors, and roof layers place blocks; room dividers, doors, carpets, stairs, secret rooms, furnishings, and entity markers do not yet place.

## How it works

`mansion::generate` consumes the structure start's already-selected rotation and random stream. It builds an 11 by 11 seeded lower-floor topology, derives a shorter third-floor topology from it, and turns exposed cell edges into wall and roof template placements. Every returned `StructurePiece` has a `PiecePlacement`, so the normal structure-placement stage clips and writes the bundled template blocks into chunk grids.

The module returns `MansionAssembly` rather than a bare vector. Its `coverage()` is `ExteriorAndCorridors`; integrations must retain the `mansion:room_templates` ledger entry until the omitted room path is implemented. A missing required template returns `MissingTemplate` before any piece is emitted, preventing an incomplete asset bundle from looking like a valid empty or reduced mansion.

## How to change it

Extend the same grid-derived planner with the room-classification pass before adding room templates. Add every newly reachable template to `TEMPLATE_IDS`; the registry's eager template loading must call `mansion::template_ids()` or `generate` will return `MissingTemplate`.

Keep all output as `template_piece` records. Do not write blocks in the planner: the per-chunk structure placement stage owns clipping, block processors, and the block-entity handoff. When interiors land, remove or narrow the registry ledger entry in the same change and add a fixture that distinguishes an interior block from a shell-only control.

## Configuration

There are no mansion-specific flags. The world seed, start chunk, selected rotation, terrain-adjusted start height, and the normal structure placement settings determine output. Templates are loaded from `assets/structure/woodland_mansion/` through the resolver.

## Dependencies

The assembler depends on `TemplateStore`, `template_piece`, `Rotation`, `PlaceSettings`, and the existing template processor and placement path. It does not use jigsaw pools or a separate block writer.
