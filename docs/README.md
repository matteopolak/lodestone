# Lodestone docs

Per-feature documentation. See also the root [`DESIGN.md`](../DESIGN.md)
(architecture and rationale) and [`HANDOFF.md`](../HANDOFF.md) (deferred work).

- [Fluid classification](./fluid-classification.md) — the one answer to "does this
  block state carry water?", shared by the mesher and by physics (swimming, fog,
  overlay, ambient sounds) — and why that is a different question from "can I break
  what is in this cell", which the pick ray answers on its own.
- [Swimming](./swimming.md) — the water-movement port: the missing `PlayerCommand`
  packet that meant the server never believed a sprint-swim, double-tap-to-sprint's
  fixed-tick timing, and the deliberate gaps (swimming hitbox, `WATER_MOVEMENT_
  EFFICIENCY`, bubble columns).
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
- [Container clicks](./container-clicks.md) — the click predictor: the seven
  `ContainerInput` modes, the `QUICK_CRAFT` drag machine and exactly what resets it,
  per-menu shift-click orders, the prediction-vs-authority rule, and three vanilla
  quirks transcribed on purpose because they read as bugs.
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
- [Pause menu](./pause-menu.md) — the in-game Escape stack, why `Screen::Paused`
  is deliberately kept out of `owns_frame` (that set drives a world-replacing
  `Clear` pass; pause overlays with `LoadOp::Load` instead), and `Sim::end_session`
  — what it resets, what it deliberately keeps, and why reconnecting is the actual
  acceptance test.
- [Tool mining speeds](./tool-mining.md) — how a held item's mining speed and
  correct-tool-for-drops verdict are resolved from the vanilla `minecraft:tool`
  census, the `correct_tool`/`requires_correct_tool` inversion trap, the
  `block_type_name` registry-id bug it fixed along the way, and how the shell
  resolves the held hotbar item through it.
- [Entity state as ECS components](./entity-components.md) — the one component set
  every entity's state lives in, the three-state `Reported<T>` encoding that keeps
  "never reported" distinct from "explicitly cleared" (and a dropped item visible),
  and the two folds that cannot be systems until the chunk world is a resource.
- [The local player as ECS components](./local-player-components.md) — the one entity
  the driver *is*, why the input half lives in `lodestone-controller`, the
  `CollisionSource` seam that lets a borrowed collision view reach a scheduled
  system, and what changed when movement intent stopped being computed per frame.
- [Bevy ECS migration](./bevy-migration.md) — the staged plan for moving state onto
  `bevy_ecs` so plugins have native-equivalent power: what azalea actually does,
  the two sources of truth that already exist, what must stay a plain library, and
  why a native plugin is not a substitute for the sandboxed WASM host.
- [Plugin API](./plugin-api.md) — the plugin surface as a specification: what a
  bevy plugin can read, write, schedule and intercept today versus what Stages
  2–6 must still deliver, why a compiled-in plugin has no sandbox, why a native
  plugin and the WASM host are not substitutes, and the gap list — four ABI
  pieces (`TickSet::Intent`, the `SendAction`/`RawPacket` messages, an `Extract`
  debug-geometry channel, and reachable block-physics constants) that no stage
  currently claims.
- [Autonomous navigation](./baritone-port.md) — the design for a Baritone-class
  pathfinding plugin: why movement costs are derived by simulating our own physics
  rather than by formula, how a 150 ms search reconciles with a one-threaded
  frame-driven ECS, the 0.25-block-per-packet agreement the server actually
  enforces, and the finding that the live `CollisionView` answers three questions
  out of twelve.
