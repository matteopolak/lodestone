# Lodestone docs

Per-feature documentation. See also the root [`DESIGN.md`](../DESIGN.md)
(architecture and rationale) and [`HANDOFF.md`](../HANDOFF.md) (deferred work).

- [Block physics constants](./block-physics-constants.md) — friction, speed and jump
  factor, bounce, stuck multiplier, climbable and `blocksMotion`: the block facts that
  are *not* geometry, where they live, why they sit outside the version seam, and the
  measured 2,618 states the old shape-derived `blocksMotion` got wrong.
- [Collision shapes](./collision-shapes.md) — the per-block-state collision census
  reaching the physics engine, why `blocks_motion` moved from a geometry
  approximation to a dumped census of its own, `fluid_at` reporting a real
  per-state level now, and the `is_solid_face` approximation that remains.
- [Entity-versus-entity interaction](./entity-push.md) — the soft crowd push
  (`Entity.push`) and the hard-collision half of `noCollision`: why `isPushable` and
  `canBeCollidedWith` are different predicates, why players passing through each
  other is vanilla, why the push has no distance falloff despite looking like it
  does, and the shell interface it is waiting on.
- [Block outline and interaction shapes](./block-outline-shapes.md) — the third
  shape census (selection/pick, distinct from collision): why cobweb outlines to a
  full cube while colliding with nothing, `is_pickable` reading it for real now,
  and the selection-box render hook that exists but nothing installs yet.
- [Item prototype components](./item-prototypes.md) — `max_stack_size`,
  `max_damage` and `equippable`, the three item facts a clientbound stack never
  carries because vanilla keeps them in the item's prototype rather than the wire
  patch, and the seam that folds them in at decode time (the fix that made armour
  equippable).
- [Fluid classification](./fluid-classification.md) — the one answer to "does this
  block state carry water?", shared by the mesher and by physics (swimming, fog,
  overlay, ambient sounds) — and why that is a different question from "can I break
  what is in this cell", which the pick ray answers on its own.
- [Fluid rendering](./fluid-rendering.md) — given that a cell carries water, what
  gets drawn: vanilla `FluidRenderer`'s three different face predicates, which
  sprite each face takes, and the shoreline bug that proved occlusion is a
  **per-face** question and not a whole-block one derived from the render layer
  (`grass_block`'s transparent overlay decal made every lake edge draw a
  waterfall).
- [Swimming](./swimming.md) — the water-movement port: the missing `PlayerCommand`
  packet that meant the server never believed a sprint-swim, double-tap-to-sprint's
  fixed-tick timing, Depth Strider's attribute now reaching physics for real, and
  the deliberate gaps that remain (`movement_speed`, bubble columns).
- [Pose-dependent dimensions](./pose-dimensions.md) — why the player box is
  `0.6 × 0.6` swimming and `0.6 × 1.5` crouching, and why that is a **fit-gated
  state machine** rather than a pose lookup: vanilla has no recovery for a player
  whose box grows into a ceiling, so the veto is the only thing preventing a
  surfacing swimmer from being clipped into one.
- [Item GUI geometry](./item-gui-geometry.md) — baking block items into 3-D
  inventory-slot geometry, and the pose/projection matrices that place them.
- [Section camera uniform](./section-camera-uniform.md) — the group-0 camera
  binding split into a shared per-frame half (view-projection + fog) and a
  per-section origin arena addressed by a dynamic offset, fixing a profiled
  ~4000 `queue.write_buffer` calls/frame (issue #75), and why the crack,
  entity and demo-world paths were deliberately left alone.
- [Dimension visuals](./dimension-visuals.md) — what already renders differently in
  the Nether/End (the sky-light default, now End-correct), what is still a hardcoded
  overworld sky and fog colour, the fog presets built and waiting to wire, and the
  stale-`player.dimension`-after-a-portal bug that undermined both.
- [Time-of-day lighting](./time-of-day-lighting.md) — the day clock and
  `sky_darken`, the one number that darkens terrain *and* mobs at night. Why 26.2's
  `set_time` clock map is empty in 19 packets out of 20, how reading the world age
  for those pinned the factor to a **session constant** (permanent noon: the
  reported "fullbright world" and "daytime mobs", one root cause), why breaking a
  block appeared to fix it, and why two green shader gates could not see any of it.
- [Entity rendering](./entity-rendering.md) — how an entity type resolves to a
  mesh, a texture and a `setupAnim`, and the two places that resolution has
  silently picked the wrong mob. Also the sheep wool render layer (issue #53):
  the mesh, the dye tint table, the `EntitySnapshot`/`EntityDraw` wiring that
  now carries the decoded variant all the way to `EntityDraw::wool`, and the
  `WoolMesh`/`prepare_wool` mesh-and-draw work still needed in the held render
  files to put a pixel on screen.
- [Humanoid armour rendering](./armour-rendering.md) — the four slot meshes and
  the **two inflations** they are baked at (the detail that makes leggings clip
  when a port loses it), why every piece is posed off the wearer's own part
  matrix rather than a second skeleton, the gamma-space leather dye, and why
  trims are designed but deliberately not landed.
- [Dropped items](./dropped-items.md) — rendering `minecraft:item` entities in the
  world (bob, spin, `display.ground`), the winding rule that inverts between the
  GUI and world paths, and the two claims in here that went stale after the fixes
  landed — one of which was then cited as the root cause of four separate issues.
  The stack count now reaches `EntityDraw::count` with no model dependency; the
  multi-copy jittered draw itself is still a specified, unlanded patch.
- [Thrown projectiles](./thrown-projectiles.md) — the nine `ThrownItemRenderer`
  entities (snowball, egg, pearl, potions, fireballs, eye of ender) as camera-facing
  billboards of their own item model, why the billboard rotation is *derived* from
  the view matrix instead of written out (three stacked conventions, wrong three
  times by hand), the three non-uniform columns of the registration table, and why
  wind charges and arrows are deliberately not here.
- [First-person held item](./first-person-held-item.md) — vanilla's arm-**or**-item
  fork: the `applyItemArmTransform` chain and how its constants differ from the bare
  arm's by just enough to look like rounding, the `Ry(45)/Ry(-45)` cancellation that
  keeps the resting pose swing-independent, the `MainHandSource` seam that was the
  *only* missing link, and the measured reason a square viewport draws zero pixels
  from a working build.
- [Entity metadata: the item field](./entity-metadata-item.md) — decoding the
  `ITEM_STACK` serializer so a dropped item knows what it is, why the item codec
  is shared rather than duplicated, and the one place the decoder deliberately
  abandons alignment instead of misreading it.
- [Block-break timing](./block-break-timing.md) — how long a block takes to mine
  and how fast its crack fills: the per-state hardness seam, the two ways to wire
  it that break blocks *too fast*, and the server branch the real numbers change.
- [Break particles](./break-particles.md) — the debris a breaking block throws: the
  `#particle` sprite (which is not the texture of any face), the per-state tint that
  vanilla applies with a *different* virtual method from the in-world one, and the
  hardcoded `[1.0; 3]` that rendered every greyscale-sprite block's debris white —
  plus the two hypotheses about that bug that captured server bytes refuted.
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
- [Edge back-off](./edge-back-off.md) — `maybeBackOffFromEdge`: the sneak-at-a-ledge
  rule the server replays through `MoverType.PLAYER`, its three stepping loops, why
  the creative oracle structurally cannot observe it, and why it was a live desync
  source rather than a theoretical one.
- [Frame pacing](./frame-pacing.md) — vanilla's ten-tick catch-up cap, why
  presentation must never gate simulation (a stalled client is sent no chunks), and
  the measured reason the unfocused frame schedule is absolute rather than
  elapsed-based: the obvious gate delivers 26 fps at a 30 fps target.
- [Main menu](./main-menu.md) — the screen state machine, the persisted
  multiplayer server list, per-server status pings (MOTD, players, favicon), the
  dependency edge that gave `lodestone-net`'s ping its first consumer, and the
  account list screen (issue #66) — a scrollable list mixed with real nine-slice
  action buttons, why that mixing needed a `row_rect` fix, and the non-vanilla
  `Accounts` title-screen row.
- [Pause menu](./pause-menu.md) — the in-game Escape stack, why `Screen::Paused`
  is deliberately kept out of `owns_frame` (that set drives a world-replacing
  `Clear` pass; pause overlays with `LoadOp::Load` instead), and `Sim::end_session`
  — what it resets, what it deliberately keeps, and why reconnecting is the actual
  acceptance test.
- [Keybindings](./keybindings.md) — the rebindable action → input table behind every
  gameplay key, why a `Binding` must hold a mouse button (vanilla's attack and use
  are mouse-bound by default), the `resolve_key` precedence chain that lets chat and
  container screens swallow keys before gameplay sees them, and the persisted format
  in `options.json` — plus the check that F3 *is* a real `KeyMapping` in 26.2 while
  Escape genuinely is not.
- [Tool mining speeds](./tool-mining.md) — how a held item's mining speed and
  correct-tool-for-drops verdict are resolved from the vanilla `minecraft:tool`
  census, the `correct_tool`/`requires_correct_tool` inversion trap, the
  `block_type_name` registry-id bug it fixed along the way, and how the shell
  resolves the held hotbar item through it.
- [Entity state as ECS components](./entity-components.md) — the one component set
  every entity's state lives in, the three-state `Reported<T>` encoding that keeps
  "never reported" distinct from "explicitly cleared" (and a dropped item visible),
  and the two folds that cannot be systems until the chunk world is a resource.
- [One bevy World (§4.1(c))](./world-unification.md) — three `World`s become one, the
  single 20 Hz accumulator and why its catch-up cap is ten ticks rather than five, the
  lock discipline that costs, and the honest answer to whether ingest can stall a frame.
- [Dissolving `Sim`](./sim-dissolution.md) — what Stage 5 moved off the shell's god
  object, the fifteen fields still on it and precisely why each stays, and the two
  20 Hz clocks whose divergence is cumulative rather than a rounding artefact.
- [Chunk world resource](./chunk-world-resource.md) — the one `lodestone_world::World`
  behind an ECS resource, terrain meshing as `Update` state, and why §4.1's two clauses
  are independent: unifying the *chunk* store did not unify the *bevy* worlds.
- [Session and HUD components](./session-components.md) — the scoreboard, tab list,
  boss bars and menus as ECS components: the *three* implementations Stage 3 collapsed,
  the two that disagreed, and why `PlayerSnapshot`'s vitals are still a real duplicate
  until the worlds unify.
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
  4–6 must still deliver, why a compiled-in plugin has no sandbox, why a native
  plugin and the WASM host are not substitutes, and the remaining gap list — the
  `TickSet::Intent` anchor, the `SendAction`/`RawPacket` messages (partly closed
  by the `ActionQueue` resource) and an `Extract` debug-geometry channel, now
  that reachable block-physics constants closed in `24af787`.
- [The third-person player body](./third-person-player-body.md) — the render
  path that folds the local player's own state into an ordinary `EntityDraw` so
  it draws through the same resolve/cull/pose/upload pipeline every mob does,
  why it must never share a pose function with the first-person arm (and why
  sharing the arm-swing *scalar* is not the same thing), and how the
  camera-mode toggle is a single `Option` rather than an enum.
- [Arm swing animation](./arm-swing-animation.md) — vanilla's `attackAnim` clock
  and the `sin(sqrt(a)·π)` shaping the first-person arm is posed by, why the
  interpolation wraps forward across the sawtooth instead of lerping (a plain
  lerp rewinds the whole arc every time a held mine re-swings), the four sites
  that start a swing, and the exact remaining wiring for remote players' and
  mobs' swings — `ClientboundAnimatePacket` is decoded but consumed by nothing.
- [Autonomous navigation](./baritone-port.md) — the design for a Baritone-class
  pathfinding plugin: why movement costs are derived by simulating our own physics
  rather than by formula, how a 150 ms search reconciles with a one-threaded
  frame-driven ECS, the 0.25-block-per-packet agreement the server actually
  enforces, and the finding that the live `CollisionView` answers three questions
  out of twelve.
- [Microsoft account storage and online-mode join](./accounts.md) — multi-account
  credentials, the keychain/plain-file split (issue #64), wiring it into the
  join flow (issue #65), and the account list screen that draws it (issue #66):
  why the refresh token and everything else live in different places, the
  one-shot keychain-availability probe and why it never retries, the pre-#64
  plaintext-cache migration, the encrypted-at-rest fallback that was
  deliberately not built, the `lodestone-client` driver arm that turns a
  `Directive::BeginEncryption` into an actual RSA/AES handshake plus session-server
  `join`, the typed XSTS/refresh-token error taxonomy, why the account screen
  hand-rolls `login::finish_interactive`'s equivalent instead of calling it, and
  exactly which steps are verified versus unverifiable without a real Microsoft
  account.
