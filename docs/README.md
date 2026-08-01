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
  silently picked the wrong mob. Also the sheep wool render layer (issue #53,
  now landed end to end): the mesh, the dye tint table, the
  `EntitySnapshot`/`EntityDraw` wiring, and `WoolMesh`/`prepare_wool` putting
  it on screen — gated on the wearer's resolved model name being exactly
  `"sheep"`, never its animation family, since pig/cow/wolf share that family
  and would otherwise grow wool too.
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
  keeps the resting pose swing-independent, the `MainHandSource` seam (now landed
  in `app.rs`), and the measured reason a square viewport draws zero pixels from a
  working build. Also issue #74: why the held item stayed lit as if it were noon
  after dark while the arm right next to it correctly dimmed — one `FogUniform`
  never carrying the world's sky-darken factor, not a missing light sample.
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
- [Combat](./combat.md) — issues #72 and #12: why the arm now swings
  unconditionally on every left-click (miss, entity, or the start of a dig),
  the entity-targeting ray that reuses the block pick's own geometry and a
  shorter vanilla reach, the serverbound `Attack` packet that had a fully
  built encoder and zero callers, why server-sent knockback used to be
  silently absorbed into a component nothing reads, the `HurtTime` countdown
  that closes two more decoded-but-unconsumed events, the attack-strength ticker
  and the crosshair cooldown indicator (issue #121, built as one unit because
  either half alone is an island), and why crit and sweep feedback specifically
  stays unbuilt until particles and sounds can consume it.
- [Autonomous navigation](./baritone-port.md) — the design for a Baritone-class
  pathfinding plugin: why movement costs are derived by simulating our own physics
  rather than by formula, how a 150 ms search reconciles with a one-threaded
  frame-driven ECS, the 0.25-block-per-packet agreement the server actually
  enforces, and its (now **superseded**) finding that the live `CollisionView` answered
  three questions out of twelve — all twelve are backed by real per-state data today.
- [**Roadmap index**](./roadmap/README.md) — the top of the plan to 1:1 parity, client and
  server: what "parity" means in falsifiable terms, the nine tracks and their epics, and the
  invariants every issue inherits. **Start here** rather than at any single area doc.
- [Benchmarks and performance roadmap](./roadmap/benchmarks.md) — what is measured and why,
  the harness design, and how a regression is caught without turning CI into a flake
  generator; includes the 2.08x per-section-uniform win as the worked example of what
  good evidence looks like, and why a wall-clock ceiling is the wrong shape for a gate.
- [Surface-stage generation performance](./worldgen-surface-perf.md) — two
  profile-driven, bit-identical memoisation fixes to `lodestone-worldgen`'s
  surface stage (a corner-cell value recomputed 256x more than needed per
  chunk, and a 98,304-entry `HashMap` pre-fill that only ever needed to hold
  the much smaller set of positions surface rules actually rewrote), taking
  measured per-chunk generation from ~24.5 ms to ~12.6 ms, plus the
  `Density::compute`-vs-`NoiseChunkSampler` caching asymmetry found along
  the way and left as the next lever.
- [Benchmark harness](./benchmark-harness.md) — the criterion-based implementation of
  that design for `lodestone-worldgen`, `lodestone-v770`, `lodestone-world` and
  `lodestone-entity`: the `support.rs` recording helper, the `bench-results/*.jsonl`
  format, why it is duplicated per-crate rather than a shared crate, and how to add a
  bench or a new site. `lodestone-world`'s and `lodestone-entity`'s benches are also the
  harness's worked examples of the "vacuous world" and "duration species" traps
  `CLAUDE.md` names — a light-propagation bench over flat terrain, a pathfinding bench
  over open ground, and a per-tick mob bench that lets a `b.iter` closure run a mob to
  completion would each look perfectly healthy while measuring nothing.
- [Client simulation, physics and input roadmap](./roadmap/client-simulation.md) — the
  ordered decomposition of movement modes not yet modelled, vitals, damage, prediction
  and reconciliation, and input, for epics #1–#4; what in `docs/baritone-port.md`'s own
  parity verdict is now stale (the live `CollisionView` answers all twelve questions,
  not three), and why the new-issue count came in under the ~35–60 guideline on
  purpose rather than by padding.
- [Client rendering and UI roadmap](./roadmap/client-rendering.md) — the ordered
  decomposition of block entity renderers, sky/weather, smooth lighting, particles, the
  remaining GUI screens, HUD, camera/post effects, item/entity visuals, audio and text
  breadth for epics #1–#4; the items a shallow grep would have re-flagged as missing but
  which already render (durability bars, animated textures, boss bar/scoreboard/tab
  list, third-person switching), and two claims in the drafting brief itself that were
  stale.
- [Plugin framework roadmap](./roadmap/plugin-framework.md) — the capability audit
  against the real Bukkit/Paper/Fabric surface for epic #77: events and cancellation,
  the scheduler, commands, permissions, world/entity/inventory access, persistence,
  packet interception, the escape hatch, and the client-only surface, each checked
  against the tree rather than the plan; a port-feasibility verdict for eight real
  plugin archetypes; a stale gap-list claim in `docs/plugin-api.md` found and fixed
  along the way (issue #180); and the one capability family — bidirectional low-latency
  packet mutation — where "port any Java plugin" needs an asterisk rather than a yes.
- [Server entities, AI and gameplay mechanics roadmap](./roadmap/server-entities.md) —
  the ordered decomposition of mob AI, spawning, breeding/taming/villagers, farming and
  processing blocks, and damage/health for epic #5; why most of Phase 0 is closing
  islands (originally `damage.rs`, `projectile.rs`, `explosion.rs`, the `brain` system
  and the dumped `path_types.rs` census all had zero consumers — `damage.rs` and
  `explosion.rs` are now wired into `MobSim`, `projectile.rs`/`brain`/`path_types.rs`
  are not) rather than new invention, and the correction that the goal-AI-plus-pathfinder
  composition the drafting brief called "groundwork" is in fact already ticking in
  production.
- [Server-side world simulation roadmap](./roadmap/server-simulation.md) — the ordered
  decomposition of chunk lifecycle, persistence, block behaviour and ticks, redstone,
  world state, and the rest of server plumbing for epic #5; two corrections to the
  drafting brief's own research (explosion block-destruction is genuinely absent but
  entity-exposure math for it already exists and is a separate island, and a real
  `WorldTime` resource exists but only client-side, never owned by `lodestone-server`);
  and why 9 of the 46 filed issues could not be attached to epic #5 directly — GitHub's
  100-sub-issue-per-parent cap, hit from concurrent filing across every Tier-4 audit.
- [Protocol, networking and multi-version roadmap](./roadmap/protocol.md) — the
  measured packet-coverage table for epics #4–#5: serverbound *decode* (being a
  server) is 0/69, a completely different axis from the 53/69 *encode* figure the
  connectedness tool reports; secure chat signing has real bookkeeping but zero
  cryptography in either role; `registry_data` is ingested by neither side, which is
  the root cause of two already-filed bugs; the `CHUNK_BATCH_START` "island" that
  turned out to be a benign side-effect-only packet; and the multi-version cost
  analysis, left as a design question rather than a recommendation.
- [Version table](./version-table.md) — the derived, provenance-tracked protocol
  number / `DataVersion` / release date for epic #343's sixteen target versions
  (1.7.10 through 26.2); the empirically-settled 1.13.2→1.14.4 boundary where the
  jar's own `version.json` starts existing; zero disagreements found between the
  jar and `vendor/minecraft-data` everywhere both were available; and the
  correction that 1.7.10 has *some* minecraft-data coverage, just not a
  per-version directory.
- [Protocol version crate naming](./protocol-crate-naming.md) — what the `vNNN`
  crate suffix actually denotes (two different rules already coexist between
  `v47`/`v340` and `v735`/`v770`, not just introduced by `v770`/protocol-776), a
  recommended single convention for the next fifteen crates with the exact
  mechanical rename steps costed out but not executed, and a factual survey of
  what `v47`/`v340`/`v735` already are: real client-direction translation layers
  into the canonical model, live-verified, incomplete in action-encode breadth,
  and with no server-direction (inbound) counterpart built for any of the three.
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
- [`/givedebug` client command](./givedebug-command.md) — the testing-only
  `/givedebug <item> <amount>` chat intercept that composes the server's real
  `/give @s <item> <amount>` rather than mutating local inventory state, why op
  is required, and why the oracle scripts now op the interactive player over
  RCON rather than pre-writing `ops.json`.
- [The sky pass and the air-bubble row](./sky-and-air-bubbles.md) — two features
  that landed as complete, tested, unreachable modules and then got wired: the
  sky's own pass before the terrain pass and the `Clear`-vs-`Load` handover that
  must key off whether the sky *actually drew*, the six-hop `airSupply` decode
  chain that did not exist at all, why every sky pipeline deliberately has no
  depth attachment, the deliberate omissions (flat clouds, no sunrise tint, an
  approximated sky colour), and the two pixel gates — both of which were wrong
  before they were right, in ways that generalise: a frame percentage cannot
  tell a uniform-but-wrong clear from a localised blob, and a control premise
  can be false before the feature under test ever existed.
- [Served session liveness](./served-session-liveness.md) — keep-alive (vanilla's
  own 15s interval and disconnect-on-timeout, not two different numbers), the
  day/night clock actually advancing over a live connection instead of a
  hermetic decode test, and view streaming (chunk-cache-center / forget / send)
  following the player between chunk columns — the three things a served
  session needed to survive, keep time, and follow the player, and why every
  piece the client needed already existed and drew nothing until this landed.
- [The `lodestone-data` crate](./lodestone-data-crate.md) — the nineteen game-data
  censuses extracted out of `crates/protocol/v770`, where exactly one table
  (`packet_ids.rs`) was ever wire format: why a server should not need a
  wire-format implementation to know how tall a zombie is, the two tables that
  deliberately did **not** move (`entity_variants.rs`, keyed by metadata
  serializer id rather than a registry, and `ShapeOracle.java`, whose output two
  independent tests cite as provenance), why `VersionAdapter::block_facts` and
  friends stay as trait methods with the smell recorded rather than acted on, and
  what #204 now takes.
- [Protocol 340 flattening table](./protocol-340-flattening-table.md) — the
  `id:meta` → block-state table for 1.12.2, derived from the real 1.13.2 jar's own
  `DataFixerUpper` rather than from `minecraft-data` or the 1.12.2 jar, with the
  full enumeration of ambiguous cases in both directions — including the 2400
  slots where vanilla itself silently falls back to air and this table refuses to
  — and every `minecraft-data` disagreement.
- [Block editing](./block-edit.md) — dig and place closing the loop on a served
  world: the decode→mutate→confirm path for `player_action`/`use_item_on`, why
  nothing retained a generated chunk column before this and why the retention that
  now does is edit-only rather than read-through, the break sequence's
  start/abort/stop semantics, why placement always writes plain stone (there is no
  inventory model to pick from), and the unrelated pre-existing gap it uncovered in
  the whole-column encoder.
- [Chunk column encoding](./chunk-column-encoding.md) — issue #363: why every
  served chunk was stone-and-air only, `build_world_column`'s real-per-cell fix
  with its memoized `resolve_state_id` lookup, and the **second, independent** bug
  the gate uncovered — the generator writing a bare `minecraft:water` with no
  `level` property, which resolved to *air* because water has no propertyless
  state, until `resolve_state_id` grew a same-name-default fallback tier. Includes
  the honest caveat that lowest-id is **not** a general default rule (661 of 797
  multi-state blocks disagree), so the tier is scoped to fluids on purpose.
- [Chunk memory pool footprint](./chunk-memory-pool-footprint.md) — issue #362's
  measured verdict on a size-classed buffer pool for chunk sections: real
  32-radius terrain populates exactly **one** of the 14 arithmetic size classes
  (and only 6 are structurally reachable at all), palettes never exceed 6 entries,
  the inflation hazard the proposal was designed around does not exist today and
  would be *introduced* by a naive pool — so the recommendation is not to build
  it. Also records an accounting gap worth more than the issue itself.
- [The `gpu/` module layout](./gpu-module-layout.md) — what went where when
  `gpu.rs` was split (#359), why `RenderState` stays in the module root, and the
  constraints that travel with the code rather than staying behind: the
  4-bind-group floor, and why the WGSL shaders were moved by line extraction
  instead of retyped.
