# Lodestone docs

Per-feature documentation. See also the root [`DESIGN.md`](../DESIGN.md)
(architecture and rationale) and [`HANDOFF.md`](../HANDOFF.md) (deferred work).

- [Fluid classification](./fluid-classification.md) — the one answer to "does this
  block state carry water?", shared by the mesher and by physics (swimming, fog,
  overlay, ambient sounds).
- [Item GUI geometry](./item-gui-geometry.md) — baking block items into 3-D
  inventory-slot geometry, and the pose/projection matrices that place them.
- [Entity rendering](./entity-rendering.md) — how an entity type resolves to a
  mesh, a texture and a `setupAnim`, and the two places that resolution has
  silently picked the wrong mob.
- [Dropped items](./dropped-items.md) — rendering `minecraft:item` entities in the
  world (bob, spin, `display.ground`), the winding rule that inverts between the
  GUI and world paths, and the metadata decode that still keeps them invisible.
- [Entity metadata: the item field](./entity-metadata-item.md) — decoding the
  `ITEM_STACK` serializer so a dropped item knows what it is, why the item codec
  is shared rather than duplicated, and the one place the decoder deliberately
  abandons alignment instead of misreading it.
- [Block-break timing](./block-break-timing.md) — how long a block takes to mine
  and how fast its crack fills: the per-state hardness seam, the two ways to wire
  it that break blocks *too fast*, and the server branch the real numbers change.
- [GUI item icons](./gui-item-icons.md) — the draw half of putting an item in a
  slot: which of the two icon streams a part reaches, the pass order, and the
  four GPU resources borrowed from the world renderer rather than uploaded twice.
  Shared by the hotbar and the container screen.
- [Vanilla HUD text](./vanilla-hud-text.md) — drawing real `ascii.png` glyphs with
  per-glyph proportional advances and the gamma-space drop shadow, why the font is
  fail-open rather than required, and why the gate measures distances between lit
  columns instead of asserting on the source string.
- [Container screen](./container-screen.md) — laying out an open `Menu` (chest,
  inventory, crafting table), why the crafting branch hangs off `Menu` instead of
  `MenuKind`, and why the result slot is never computed locally.
- [Crafting](./crafting.md) — the recipe data model and matching rules, loading
  the vanilla corpus from the client jar's datapack JSON, and the crafting-table
  menu layout (including who actually computes the result slot).
- [Frame pacing](./frame-pacing.md) — vanilla's ten-tick catch-up cap, why
  presentation must never gate simulation (a stalled client is sent no chunks), and
  the measured reason the unfocused frame schedule is absolute rather than
  elapsed-based: the obvious gate delivers 26 fps at a 30 fps target.
- [Main menu](./main-menu.md) — the screen state machine, the persisted
  multiplayer server list, per-server status pings (MOTD, players, favicon), and
  the dependency edge that gave `lodestone-net`'s ping its first consumer.
- [Tool mining speeds](./tool-mining.md) — how a held item's mining speed and
  correct-tool-for-drops verdict are resolved from the vanilla `minecraft:tool`
  census, the `correct_tool`/`requires_correct_tool` inversion trap, the
  `block_type_name` registry-id bug it fixed along the way, and how the shell
  resolves the held hotbar item through it.
- [Bevy ECS migration](./bevy-migration.md) — the staged plan for moving state onto
  `bevy_ecs` so plugins have native-equivalent power: what azalea actually does,
  the two sources of truth that already exist, what must stay a plain library, and
  why a native plugin is not a substitute for the sandboxed WASM host.
