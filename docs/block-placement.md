# Player block placement

## What it is

This document describes how a right-click block placement chooses its target cell and validates the proposed state. It covers the shared server path for air, fluids, replaceable blocks, and blocks with placement-specific state rules.

## How it works

The server reads the clicked state and held block item, then chooses between the clicked cell and the adjacent cell. Air and fluids retain their existing direct-target rule; otherwise the built-in `minecraft:replaceable` block tag lets partial and plant blocks such as snow layers or short grass be replaced in place. Matching slabs keep their separate doubling rule.

The client predictor uses the same air, fluid, and replaceable-tag classification before it writes an optimistic state, so a snow layer or plant is not shown at the cell above while the server processes the click. After target selection, `block_placement::placement` derives directional and multi-cell state details. Waterlogging, support, build-height, player obstruction, and occupied-cell checks run before any world mutation. A successful placement writes all owned cells and sends their block updates; a rejected attempt writes none.

Both `Sim::use_item_live` and `interact::drive_placement` test player obstruction through `placement_intersects_player`. It resolves the proposed placement state from the click geometry and intersects the player's bounding box with that state's generated collision boxes, translated into the destination cell. Empty-collision blocks such as torches can therefore be placed where the player stands and follow the existing successful-placement swing path. Full cubes still obstruct; a partial block is tested against its actual occupied volume. Unknown placement states retain a conservative full-cell check.

## How to change it

Change target selection in the `UseItemOn` consumer in `lodestone-server::server`, and keep the clicked-cell replaceability predicate shared with `block_placement::validate_placement`. Add state-property conventions to `block_placement::placement`; its census-based dispatch handles property families, with explicit cases only when the state census cannot distinguish behaviors. Keep multi-cell placement validation atomic.

The `minecraft:replaceable` tag is the current generic membership source. Air and fluids are handled separately. If item-specific replacement rules become necessary, thread the held item and clicked face through the predicate rather than broadening tag membership into an approximation.

Keep placement-state resolution and obstruction checking aligned when extending the client predictor. Change `placement_intersects_player` rather than adding an item-specific swing exception; the swing follows the placement decision. Geometry regression fixtures should distinguish empty shapes, full cubes, and partial shapes where the player's box overlaps only the empty part of a cell.

## Configuration

There are no placement-specific environment variables or flags. The block tag lookup uses the built-in 26.2 tag table and may use the server's synchronized block-tag table when one has been installed.

## Dependencies

The path uses `lodestone-data` for block states, collision shapes and tag membership, `lodestone-game::placement` for the client decision and placement geometry, `lodestone-server::block_placement` for state derivation and validation, the integrated world's block-state source for reads and writes, and the protocol adapter for block-update packets.
