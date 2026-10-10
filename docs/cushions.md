# Cushions

## What it is

The 26.3 cushion: a dyed, block-attached entity (1 x 0.25 slab) placed on the top face of a block, seating one player. Server rules live in `lodestone_server::cushion` (pure) and `lodestone_server::mobs::MobSim` (state); drawing is a corpus model plus a per-dye texture sheet.

## How it works

* **Placement.** `UseItemOn` with a cushion stack reaches `cushion::apply_cushion_item`. Only an up-facing click places it; the position is the clicked cell's top (or the collision shape's top, re-aimed along the eye ray for partial shapes), the yaw is the player's snapped to a quarter turn, the colour comes from the item. An overlapping cushion, a solid cell or a suffocating position refuses it. One item is consumed outside creative.
* **State.** `MobSim` keeps cushions in a sidecar map (`TrackedCushion`), like minecarts. They stream from `push_cushion_snapshots` with `MetadataField::CushionColor` (index 8, dye serializer) so the 26.3 encoder writes the dump row.
* **Seating.** Interact seats the player unless sneaking or occupied and sends a passenger list; shift or a break dismounts. The client seat is the generic attachment fallback in `lodestone_ecs::riding`, 0.35 below the feet.
* **Breaking.** A survival or creative hit removes it; survival drops the coloured item. Every 101st tick an unseated cushion breaks if fire is in its box or its support is gone (`plan_cushion_checks`, applied in `tick.rs` only after a complete world read).
* **Drawing.** Corpus model `cushion` (`special::cushion_model`), pose `non_living_vehicle_placement("cushion")`, sheet chosen by `EntityVariant::Dyed` in `entity_appearance_sheet`. It is pickable and has no shadow.

## How to change it

* Rules: edit `cushion.rs` and its tests; the tick and interact wiring only call into it.
* New metadata: `session.rs` `MetadataField` plus the 26.2 family encoder constants, checked against the jar dump in `crates/versions/26.3/tests/support`.
* Not wired: persistence (cushions are lost on restart), sounds, break particles, destroy-on-leave straw-bed rules.

## Configuration

None.

## Dependencies

`lodestone-ecs` (riding), `lodestone-assets` (model corpus), `lodestone-render` (sheets), `lodestone-shell` (pick list, swing, shadow table), the 26.3 texture cache for `entity/cushion/*_cushion.png`.
