# Lodestone docs

Per-feature documentation. See also the root [`DESIGN.md`](../DESIGN.md)
(architecture and rationale) and [`HANDOFF.md`](../HANDOFF.md) (deferred work).

- [Block physics constants](./block-physics-constants.md) — friction, speed and jump
  factor, bounce, stuck multiplier, climbable and `blocksMotion`: the block facts that
  are *not* geometry, where they live, why they sit outside the version seam, and the
  measured 2,618 states the old shape-derived `blocksMotion` got wrong.
- [Collision shapes](./collision-shapes.md) — the per-block-state collision census
  reaching the physics engine, why `blocks_motion` moved from a geometry
  approximation to a dumped census of its own, and `fluid_at` reporting a real
  per-state level now. (`is_solid_face`'s approximation is **fixed** — #216.)
- [Climbing and freezing](./climbing-and-freezing.md) — scaffolding's sneak-hold
  exception versus a ladder, powder-snow freezing, and the swept-segment sweep both
  use (#210, #212, #216). Also why the sneak-to-fall-through collision toggle is a
  deliberate gap: it needs a descending/approach context `collision_boxes` has no
  way to express.
- [Riptide and firework boost](./riptide-and-firework-boost.md) — the trident launch
  and elytra glide-boost impulses, landed physics-only pending their item and entity
  triggers (#208, #206), each verified against a trig identity rather than a
  recorded trace.
- [Entity tick drivers](./entity-tick-drivers.md) — `ProjectileRegistry` and
  `ItemEntityRegistry`, the per-tick seam that turned `projectile.rs` and
  `item_entity.rs`'s lifecycle half from islands into driven code (#211, #215), plus
  why `MobSim` having **no production instantiation** makes #217 the prerequisite for
  wiring any of them into the integrated server rather than a beneficiary of them.
- [Live mob simulation](./live-mob-sim.md) — issue #217: `MobSim` now has a real
  production tick loop (`IntegratedServer::open_in_memory_with_mobs`), and the actual
  gap was one level up from what the module doc said — the encoders and the
  spawn/update/remove wire pipeline already existed and were already proven live;
  nothing ever constructed or ticked a `MobSim`. Also the collision this wiring found
  live: `MobSim`'s default starting entity id (`1`) is `V770ServerProtocol`'s own
  `LOCAL_PLAYER_ENTITY_ID`, so the first mob a fresh sim ever spawned silently never
  reached the client (a real client never `ADD_ENTITY`s itself) until mob ids moved
  off that range; and why `Goal: Send` landing separately is what made a real `MobSim`
  usable as a `tokio::spawn`ed `EntitySource` at all.
- [Entity-versus-entity interaction](./entity-push.md) — the soft crowd push
  (`Entity.push`) and the hard-collision half of `noCollision`: why `isPushable` and
  `canBeCollidedWith` are different predicates, why players passing through each
  other is vanilla, why the push has no distance falloff despite looking like it
  does, and the shell interface it is waiting on.
- [Block outline and interaction shapes](./block-outline-shapes.md) — the third
  shape census (selection/pick, distinct from collision): why cobweb outlines to a
  full cube while colliding with nothing, and its three consumers — pickability,
  the drawn selection box, and the pick ray, which kept treating every block as a
  unit cube for a whole release *after* the drawn box was fixed (#375).
- [Item prototype components](./item-prototypes.md) — `max_stack_size`,
  `max_damage` and `equippable`, the three item facts a clientbound stack never
  carries because vanilla keeps them in the item's prototype rather than the wire
  patch, and the seam that folds them in at decode time (the fix that made armour
  equippable).
- [Fluid classification](./fluid-classification.md) — the one answer to "does this
  block state carry water?", shared by the mesher and by physics (swimming, fog,
  overlay, ambient sounds) — and why that is a different question from "can I break
  what is in this cell", which the pick ray answers on its own.
- [Bubble columns](./bubble-columns.md) — the soul-sand lift and magma drain
  (issue #199): four constants, one impulse **per occupied cell** rather than per
  tick, and why the impulse belongs beside `update_stuck_multiplier` rather than in
  `tick_water` — `applyEffectsFromBlocks` runs *after* `travel()`, so it lands on the
  next tick. Also the "doubled for magma" term that the issue described and vanilla
  does not have.
- [View bobbing, the damage tilt and view lag](./view-bobbing.md) — issue #58: the
  walk bob landed (state, transform, user option, pixel gate), and the **one
  blocker** that stopped the other two. `Camera` has three degrees of freedom where
  a bob matrix has four, so folding the bob into it **drops roll** — measured at
  2.52 px worst case for the walk bob, which is why that shipped, and fatal for the
  damage tilt, which is *pure* roll for a frontal hit and is therefore implemented,
  tested against vanilla, and deliberately left unwired. Also: why the bob goes on
  `render_camera` and never `camera` (which is the pick ray and the audio
  listener), that a bounding-box centre is **not** a projected centroid (8.50 px vs
  6.53, close enough to a nod-free bob's 8.31 to hide it), and that the island
  control fires exactly one test out of 26.
- [Creative flight](./creative-flight.md) — issue #191: the server-granted flight the
  client had **no** consumer for (`AbilitiesChanged` was decoded, tested and wired to
  nothing, so the client would free-cam on a server that never granted flight), why
  flight is a *wrapper* around the existing travel dispatch rather than a fourth
  `tick_*` arm, the `0.6` that is Y-only and *overwrites* rather than damps, the
  thirteen `!flying` conjuncts (one of which nothing had listed), and the
  `getFlyingSpeed` sprint arm whose absence made **every sprint-jump** 30% short —
  in the Rust *and* in the Python oracle, which is why they agreed. Spectator is
  explicitly deferred rather than half-modelled.
- [Riding](./riding.md) — Tier 1 item 8: `EntityPassengersChanged` was a **complete**
  island (four grep hits: the decode, its two tests, the variant), and so were all
  five other riding-shaped wire items, three of them serverbound with zero
  producers. What landed is mount / seat / camera / dismount; what is deferred is
  the vehicle *moving*, and for one reason — **every vehicle is client-authoritative
  while a player rides it**, horses included, so the server zeroes its delta and
  waits for `ServerboundMoveVehiclePacket`. Also: 26.2's data-driven attachment rule
  and its two easy-to-invert constants (the `PASSENGER` fallback is `height × 1.0`,
  *not* the eye height's `× 0.85`; the player's own `VEHICLE` attachment is `0.6`
  and is **subtracted**), why the camera needed no code at all (vanilla's own
  `Camera` has no `isPassenger()` branch), why dismount needed none either (it is
  the sneak bit of a packet we already send, and vanilla does not predict it), and
  the measured correction that the server *cannot* kick a passenger over
  `on_ground` — its float check is explicitly `&& !isPassenger()`.
- [Fluid rendering](./fluid-rendering.md) — given that a cell carries water, what
  gets drawn: vanilla `FluidRenderer`'s three different face predicates, which
  sprite each face takes, and the shoreline bug that proved occlusion is a
  **per-face** question and not a whole-block one derived from the render layer
  (`grass_block`'s transparent overlay decal made every lake edge draw a
  waterfall).
- [Swimming](./swimming.md) — the water-movement port: the missing `PlayerCommand`
  packet that meant the server never believed a sprint-swim, double-tap-to-sprint's
  fixed-tick timing, Depth Strider's attribute now reaching physics for real, and
  the deliberate gaps that remain (`movement_speed`). Also lava's
  shallow-vs-deep `travelInLava` branch (issue #214) — a structurally different
  arm rather than water with different numbers, and one that no coarse
  presence-cell scenario could have distinguished, since coarse presence reads as
  a full `1.0` height and is always on the deep side of the `0.4` threshold.
- [Pose-dependent dimensions](./pose-dimensions.md) — why the player box is
  `0.6 × 0.6` swimming and `0.6 × 1.5` crouching, and why that is a **fit-gated
  state machine** rather than a pose lookup: vanilla has no recovery for a player
  whose box grows into a ceiling, so the veto is the only thing preventing a
  surfacing swimmer from being clipped into one.
- [Item GUI geometry](./item-gui-geometry.md) — baking block items into 3-D
  inventory-slot geometry, and the pose/projection matrices that place them.
- [Item variants](./item-variants.md) — one item, several baked geometries: why
  resolving `items/<id>.json` **once at load against a static GUI context**
  flattened all 84 branching items to their inventory form (a spyglass in the hand
  drew the flat sprite, and took `item/generated`'s `firstperson_righthand` rather
  than the in-hand model's), why the fix has to run in the **pre-stitch** discovery
  pass (`bow_pulling_*` geometry is walked out of the alpha outline of a texture
  that was in no atlas at all), which properties are honestly sourced and which
  fall back, and why `use_duration` counts **up** while `use_cycle` counts down.
- [Section camera uniform](./section-camera-uniform.md) — the group-0 camera
  binding split into a shared per-frame half (view-projection + fog) and a
  per-section origin arena addressed by a dynamic offset, fixing a profiled
  ~4000 `queue.write_buffer` calls/frame (issue #75), and why the crack,
  entity and demo-world paths were deliberately left alone.
- [Distance fog](./fog.md) — where the ramp starts and how wide it is: vanilla's
  `clamp(view/10, 4, 64)` fade band (issue #388, replacing a 0.75 fraction that
  was twice as hazy as vanilla in the outer fifth), the two terms vanilla
  combines with `max`, and the spherical-versus-cylindrical metric this client
  does not model yet. Colours live in the next entry.
- [Dimension visuals](./dimension-visuals.md) — what already renders differently in
  the Nether/End (the sky-light default, now registry-driven), what is still a
  hardcoded fog and sky colour because the dimension type's `attributes` map is not
  decoded, and the stale-`player.dimension`-after-a-portal bug that undermined both
  (fixed; the diagnosis is kept as the record of how it hid).
- [Registry data ingest](./registry-data-ingest.md) — decoding the Configuration
  `registry_data` packet (issue #288): typed dimension types and world clocks, the
  three hardcoded guesses they replaced (column height, `has_skylight`, which clock
  is the day clock), why `the_nether` is holder **3** and the End has its own clock,
  and the `handles_event` arm without which the whole chain reaches zero pixels.
- [Time-of-day lighting](./time-of-day-lighting.md) — the day clock and
  `sky_darken`, the one number that darkens terrain *and* mobs at night. Why 26.2's
  `set_time` clock map is empty in 19 packets out of 20, how reading the world age
  for those pinned the factor to a **session constant** (permanent noon: the
  reported "fullbright world" and "daytime mobs", one root cause), why breaking a
  block appeared to fix it, and why two green shader gates could not see any of it.
- [The light ramp](./light-ramp.md) — vanilla's lightmap curve (`level / (4 - 3·level)`
  plus `notGamma` at the default gamma), which replaced a linear `0.2 + 0.8·level`
  ramp with a 20% floor (issues #383, #386). Includes the two wrong numbers the
  record carried about it — the curve composed with `sky_darken` in the wrong order,
  and `notGamma` omitted entirely, which together turned "14% too dark at midnight"
  into "5.36× too bright" — why every full-bright gate in the tree was byte-identical
  afterwards, and why a ratio measured against light 0 is now degenerate.
- [Model smooth lighting / AO](./model-smooth-lighting.md) — vanilla's
  `AmbientOcclusionFace` four-corner blend on the *model* path (issue #22), the
  `ambientocclusion` model-flag gate, and why AO rides the shader's existing gamma
  round-trip. Read its "which mesher this is" section first: `--headless` drives
  `mesh_simple` and structurally cannot exercise any of this. Also lists four
  measured divergences still open, the visible one being that our AO occluder test
  is `occludes` where vanilla's is `isCollisionShapeFullBlock` — so leaves, slime
  and spawners darken nothing and a tree canopy's underside stays bright.
- [Block entity renderers](./block-entity-renderers.md) — the cuboid rigs whose
  block model does not describe them (issue #23; chest landed, eleven types not).
  A 26.2 chest has **no block model at all** — `block/chest.json` declares only a
  particle texture, zero elements — so before this it was a hole in the world that
  no terrain metric could see. Covers the placement matrix (block space, **not**
  the entity path's Y-flip and 1.501 lift), the three separate lid transforms,
  why sheets are keyed by stem rather than model, and the note that the GUI
  item-icon path (#369) is a **second consumer** of the same geometry. Now also
  the **four** creation routes, not the two the chain diagram used to imply
  (issue #374): in vanilla *writing a block state is what creates the block
  entity*, no packet involved, so a freshly placed chest was invisible while
  still opening — and why the removal half of `World::sync_block_entity` matters
  as much as the creation half.
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
- [Item pickup animation](./item-pickup-animation.md) — the 3-tick fly-to-collector
  flight (issue #365): why the item entity is **removed immediately** and what flies
  is a frozen copy, the quadratic ease that spends half the flight on a quarter of
  the distance, `getEyeY()` being absolute rather than an offset (read as an offset
  it aims 32 blocks underground), the missing arm in `net.rs`'s `forward` that left a
  correct decode and a tested fold reaching zero pixels, and why the local player
  needs a second collector lookup that a mob does not.
- [Thrown projectiles](./thrown-projectiles.md) — the nine `ThrownItemRenderer`
  entities (snowball, egg, pearl, potions, fireballs, eye of ender) as camera-facing
  billboards of their own item model, why the billboard rotation is *derived* from
  the view matrix instead of written out (three stacked conventions, wrong three
  times by hand), the three non-uniform columns of the registration table, and why
  wind charges and arrows are deliberately not here.
- [Projectile renderers](./projectile-renderers.md) — arrow, spectral arrow and
  trident (issue #380): the `ArrowRenderer`/`ThrownTridentRenderer` rigs, and why
  they need a placement of their own. `ArrowRenderer extends EntityRenderer`, so it
  gets **neither** the mob path's Y flip nor its 1.501 lift — and that lift is
  applied *before* the flip, so reusing the mob matrix draws an arrow 1.5 blocks
  **high**, not low, which is the direction both the issue and the first draft of
  the test got wrong. Also: the orientation needed no velocity plumbing after all,
  because the *server* runs the same `atan2` and broadcasts the result as ordinary
  rotation; why pitch is about `Z` and the trident's `+90°` unifies the two rigs;
  the only corpus use of `texScale`; and the proof that a wrong **Y flip** changes
  no pixel on this rig at all, so #380's prescribed texel gate or live oracle was
  not needed — with the trident rig and the arrowhead texture patch as the two
  controls.
- [First-person held item](./first-person-held-item.md) — vanilla's arm-**or**-item
  fork: the `applyItemArmTransform` chain and how its constants differ from the bare
  arm's by just enough to look like rounding, the `Ry(45)/Ry(-45)` cancellation that
  keeps the resting pose swing-independent, the `MainHandSource` seam (now landed
  in `app.rs`), and the measured reason a square viewport draws zero pixels from a
  working build. Also issue #74: why the held item stayed lit as if it were noon
  after dark while the arm right next to it correctly dimmed — one `FogUniform`
  never carrying the world's sky-darken factor, not a missing light sample.
- [Held-item equip animation](./held-item-equip-animation.md) — the dip-and-raise on a
  hotbar change (issue #366): the real 26.2 field names (`mainHandHeight`, *not* the
  `equippedProgress` every reference including the issue cites), the ±0.4-per-tick
  ramp and the 300 ms swap, why the visible item is exchanged at the **bottom** of the
  dip and why branching the arm/item fork on the *selected* item instead produces a
  recognisably wrong animation, the count-and-components half of vanilla's retrigger
  predicate that a bare item id cannot see, and the `#[derive(Default)]` that would
  have drawn every test's bare arm 0.6 blocks off the bottom of frame.
- [Entity metadata: the item field](./entity-metadata-item.md) — decoding the
  `ITEM_STACK` serializer so a dropped item knows what it is, why the item codec
  is shared rather than duplicated, and the one place the decoder deliberately
  abandons alignment instead of misreading it.
- [Arm poses](./item-use-arm-poses.md) — the bow draw and crossbow wind, from
  either of the two metadata bits that drive them (using-item for players,
  aggressive for mobs)
  (issue #57): why metadata index 8 is ambiguous between `LivingEntity`'s
  using-item bitfield and an arrow's **crit** flag (both plain bytes, so a naive
  decoder reports every critical arrow as drawing a bow), why the draw fraction has
  to be counted client-side because `useItemRemaining` is never synced, why a
  repeated metadata byte must not restart the draw, and why a bow-holding zombie
  correctly keeps its arms forward.
- [Block-break timing](./block-break-timing.md) — how long a block takes to mine
  and how fast its crack fills: the per-state hardness seam, the two ways to wire
  it that break blocks *too fast*, and the server branch the real numbers change.
- [Block placement prediction](./block-placement-prediction.md) — writing a placed
  block locally instead of waiting a round trip for `BLOCK_UPDATE` (issue #381, the
  prediction half of #374). Covers why the server's *unconditional double*
  `ClientboundBlockUpdatePacket` after every `use_item_on` means a refused placement
  needs no rollback mechanism of its own, the `state_id → default state` census that
  does **not** exist and why "the lowest state id for this block" is a waterlogged
  chest rather than a chest, the 60 measured non-geometric property defaults plus the
  16 unambiguous ones deliberately excluded, and why interactability is
  over-approximated on purpose.
- [Break particles](./break-particles.md) — the debris a breaking block throws: the
  `#particle` sprite (which is not the texture of any face), the per-state tint that
  vanilla applies with a *different* virtual method from the in-world one, and the
  hardcoded `[1.0; 3]` that rendered every greyscale-sprite block's debris white —
  plus the two hypotheses about that bug that captured server bytes refuted. Also the
  wrong-atlas bug (issue #45): why a flame particle drew *block* texels for months while
  `unresolved` sat at zero, and the pixel gate that discriminates by colour.
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
  `MenuKind`, why the result slot is never computed locally, and (issue #370)
  vanilla's **two** labels: their per-screen anchors, why `inventoryLabelY` must be
  derived from `imageHeight` rather than restated, and why the player inventory
  screen is titled "Crafting" and is the only screen that omits the second label.
- [Container clicks](./container-clicks.md) — the click predictor: the seven
  `ContainerInput` modes, the `QUICK_CRAFT` drag machine and exactly what resets it,
  per-menu shift-click orders, the prediction-vs-authority rule, three vanilla
  quirks transcribed on purpose because they read as bugs, and (issue #27) a
  verb-by-verb audit against 26.2's `doClick` that closed three test-coverage
  gaps and fixed a real swap-overflow ordering bug.
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
- [Menu UI framework](./ui-framework.md) — the plan of record for vanilla-faithful
  menu screens (epic #392): the vanilla→us mapping, why `AbstractContainerScreen` was
  re-derived by four separate agents in one day, and the measured reason the port is
  closer than it looks — 26.2's `Renderable.extractRenderState`/`GuiRenderState` split
  is structurally our `ExtractSet`/`FrameSet`. Also the settings census (93
  `OptionInstance` accessors, ~198 controls, 4 persisted), the disabled path
  (`active = false`, grey `-6250336`, and the three widgets with no disabled sprite),
  why sprite/nine-slice reachability was **deleted** from the plan rather than
  scheduled, and the HUD/container boundary this deliberately does not cross.
- [Menu widgets](./menu-widgets.md) — the first child of that epic (#393):
  `Widget`, `WidgetSprites` and the disabled render path, converted title and pause
  rows with pixels unmoved, why `Checkbox`/`EditBox`/`AbstractSliderButton` must
  **not** get disabled art, and three things the written record had wrong that the
  jar settles — the sprite argument is `isHoveredOrFocused()` and not
  `isFocused()`, the two `WidgetSprites::get` arguments are different predicates,
  and `EditBox` does route through the record.
- [Menu layout containers](./menu-layout.md) — the second child (#394):
  `GridLayout`, `LinearLayout`, `FrameLayout`, `HeaderAndFooterLayout` and
  `Divisor`, with the title column and the pause grid now **arranged** instead of
  tabulated and their hand-derived rects kept as the no-move gate. Also why
  `setX` truncates where `setY` rounds, why the alignment model is padding-aware
  rather than a naive centre, which of vanilla's two two-phase timings this
  follows and why (`PauseScreen`'s — nothing here survives a frame to be
  repositioned), and the measured reason the title screen's hand arithmetic is
  numerically a `LinearLayout` even though vanilla uses no layout class for it.
- [Menu focus and `EditBox`](./menu-focus.md) — the third child (#395): focus,
  Tab/arrow traversal, `ContainerEventHandler` dispatch and the three child
  registries, plus `EditBox` wired into `Screen::ServerEdit`'s address fields. Also
  why a focused text field swallows the arrow keys purely from
  `Screen.keyPressed`'s **ordering** rather than any rule, that Tab's wrap is a
  `clearFocus()`-then-retry in `Screen` and **not** in `handleTabNavigation` (so
  arrows do not wrap at all), that arrow navigation has a *second* "vaguely in
  direction" pass without which focus dies at the end of a column, the first menu
  widgets in this shell that **outlive a frame** and what that cost, and the
  correction #393's own correction needs: `EditBox` passes `isFocused()` where
  `AbstractButton` passes `isHoveredOrFocused()`, so hovering a text field must
  *not* highlight it.
- [World select, with creation disabled](./world-select.md) — the fifth child
  (#397) and `HeaderAndFooterLayout`'s first consumer: vanilla's
  `SelectWorldScreen` with four of its six footer buttons present and greyed,
  **Create New World among them** (#190 is the screen it would open) — Play Selected
  World went live with #287, see [`singleplayer.md`](./singleplayer.md). Also the
  hand-derived footer arithmetic and why *Play*'s 150 px is what makes all four
  columns 71, why the search box lands at y 21 and not the 22 its own constructor
  says, the gate that a canvas-*dependent* container arranged once is still right
  at 320×240 and 1920×1080, and the jar finding that contradicts the obvious
  guess: **vanilla has no empty-list state for this screen** — `NoWorldsEntry` is
  Realms-only and `SINGLEPLAYER` with no worlds *leaves* for `CreateWorldScreen`,
  which is why the one row we do have is drawn with `NoWorldsEntry`'s geometry
  rather than `WorldListEntry`'s.
- [Singleplayer](./singleplayer.md) — #287: **Play Selected World now starts a real
  integrated server in-process** and the client joins it over an in-memory duplex,
  so singleplayer and multiplayer differ in exactly one thing, the `Transport`.
  Covers the serverbound half of the version seam
  (`lodestone_registry::server_protocol_for_protocol`, the twin of
  `adapter_for_protocol`) and why its `lodestone-server` dependency must be
  **required rather than feature-gated** — a `#[cfg]`'d function would turn
  `--no-default-features` from the `None` the shell reports into a compile failure.
  Also: why `Box<dyn ServerProtocol>` needed a forwarding impl before it could be
  served, and why forgetting one of its eighteen forwards is **not** a compile error
  but a silently-defaulted method that only misbehaves in singleplayer; why the
  server lives on the existing net thread rather than a new one; and that
  `MenuAction::Singleplayer` sat with **no producer at all** from #397 until this
  landed, which is what an island looks like from the inside.
- [The multiplayer server list](./server-list.md) — the fourth child (#396) and
  `HeaderAndFooterLayout`'s other consumer: vanilla's `JoinMultiplayerScreen` and
  `ServerSelectionList` at vanilla's geometry, with the seven footer buttons and
  three of them inactive when nothing is selected. Also the latency buckets (which
  run *downward* — five bars for a fast server) and the pinging animation's
  ping-pong fold, why `getRowLeft()`'s **two separate integer divisions** mean a
  list row cannot be a `Slot`, how a mouse *position* reaches a frame at all so the
  favicon's join / move-up / move-down quadrants can be both drawn and clicked from
  one definition, and why Refresh needed a verb of its own (`refresh` skips every
  row that already has a result, so the button would have done nothing).
- [The accounts screen](./accounts-screen.md) — the same idiom applied to
  `Screen::Accounts`, which has **no vanilla original** (Minecraft picks an account
  in the launcher), so the server list *is* the reference and every constant cites
  it rather than a jar line. Mostly the record of a reported bug: the sign-in error
  was one unwrapped `TEXT_SCALE` line, and shortening the message is not a fix
  because the screen does not own the string — `AuthError` embeds up to 400
  characters of raw response body and a loopback URL is a few hundred characters of
  query string, both of which are **whitespace-free**, so the multiplayer screen's
  greedy `wrap_measured` does nothing for them. Hence `MenuNotice`: text carried
  unwrapped and wrapped in the draw's own font, a line *count* the layout derives
  from the band rather than a constant, and a `wrap_bounded` that breaks inside a
  word. Also why `Enter` had to start cancelling a sign-in (a button that draws,
  highlights and does nothing is #391's shape), and which half of #402's
  scroll-window gap is now closed and which is still open.
- [The settings tree](./settings-screen.md) — the sixth child (#55) and
  `HeaderAndFooterLayout`'s **first production** consumer: vanilla's whole
  `OptionsScreen` tree over eight pages, **135 controls of which 18 work and 117
  are present and greyed out**, which is the deliverable rather than a shortfall.
  Also `OptionsList`'s geometry transcribed (the 310/150/160/25 metrics, and the
  header whose `paddingTop` is 0 for the first entry and 18 after), the five vanilla
  screens deliberately not built because each needs a *different* list widget, the
  four departures each written down with what the alternative would have cost — no
  value on a row we do not honour, no handle on a slider we hold no value for, a
  scroll window derived from `MIN_SCALED_HEIGHT` because this pipeline has no
  scissor, and a cursor that deliberately stops on inactive rows — plus two jar
  findings that each nearly shipped something wrong: `guiScale` is a **cycle
  button**, not a slider (`ClampingLazyMaxIntRange.createCycleButton()` is `true`),
  and `AbstractSliderButton`'s sprite predicate is a *conjunction*, so
  `SLIDER_SPRITES` needs the 3-argument `WidgetSprites` collapse and the obvious
  2-argument one lights a greyed-out slider up under the cursor. It also corrects
  this repo's own census: we persist **2 of 93** options, not 4 — `render_distance`
  and `sensitivity` are argv, not `options.json`.
- [Main menu](./main-menu.md) — the screen state machine, the persisted
  multiplayer server list, per-server status pings (MOTD, players, favicon), the
  dependency edge that gave `lodestone-net`'s ping its first consumer, the
  account list screen (issue #66) — a scrollable list mixed with real nine-slice
  action buttons, why that mixing needed a `row_rect` fix, and the non-vanilla
  `Accounts` title-screen row — and issue #68: why `Screen::Error` rendered a raw
  `multiplayer.disconnect.*` key, the `NetUpdate::Disconnected` fix (now an
  unresolved `Text` resolved at `Sim::poll_net`, same shape as #52's container
  title), and the `NetUpdate` sweep that found `Death`'s message has the same
  bug and left it as the already-named separate follow-up.
- [Pause menu](./pause-menu.md) — the in-game Escape stack, why `Screen::Paused`
  is deliberately kept out of `owns_frame` (that set drives a world-replacing
  `Clear` pass; pause overlays with `LoadOp::Load` instead), and `Sim::end_session`
  — what it resets, what it deliberately keeps, and why reconnecting is the actual
  acceptance test.
- [Menu background and title panorama](./menu-panorama.md) — the spinning cubemap
  behind every out-of-world screen: vanilla's `_1,_3,_5,_4,_0,_2` face order and
  per-face vertical flip, the Linear sampler that must not be "fixed" to Nearest,
  why the pipeline runs with no depth and no culling rather than betting on a
  winding polarity, and why the `menu_background.png` wash is a shader uniform
  instead of a second quad. Also **the asset-object store**, and the mistake that
  makes it worth reading: `client.jar` ships deliberate 69-byte 1×1 grey **stubs**
  for all six panorama faces, and the real 1024×1024 art is delivered through
  `asset-index-*.json`. Reading the jar gave a flat sky that bound, drew and passed
  every "it reached pixels" gate. Exactly **8** of 5057 index objects share a name
  with a jar entry (the 6 faces, `panorama_overlay`, `unifont.json`), so the rule
  is narrow — *for a name in both, prefer the object store* — but the same index
  holds all 4871 `.ogg` sounds, so `audio.rs`'s private copy of this lookup was
  **extracted into `crate::asset_objects`** and audio now runs through it. Also
  measures what actually blocks sound: `sounds.json` is present and parses (1968
  events), but only **11 of 4871** samples are on disk, so audio comes up and plays
  nothing — the "connected but silent" state, not a startup failure. Closed by
  [Sound playback](./sound-playback.md).
- [Sound playback](./sound-playback.md) — the packet-to-speakers chain (all of which
  already existed) and the two things that kept it quiet: no samples on disk, and
  **two env vars for one directory** — `audio.rs` demanded `LODESTONE_ASSET_ROOT`
  while everything else resolved the same folder from `LODESTONE_ASSETS` or an
  ancestor walk, so a plain `cargo run` rendered vanilla textures with audio
  switched *off*. Both fixed. Also `xtask fetch-sounds`, whose corpus is **derived
  from `sounds.json`** rather than a rotting file list: 4751 objects / 80 MB is every
  sample a non-music event can select (`--all` adds 92 music and record tracks,
  +293 MB), and the excluded-only-if-*every*-referencing-event-is-music rule is what
  keeps `records/cat` — shared with `jukebox.play` — from being dropped. Ends with
  what is still silent and why. Now also carries the **correction** that the
  `LEVEL_EVENT` 2001 fix does *not* make every break audible: a player's own dig
  never emits `2001` at all (`ServerPlayerGameMode.destroyBlock` calls `removeBlock`
  with no `levelEvent` in the method), so vanilla predicts its own break locally
  through `ClientLevel.levelEvent`, and the live predicted break is the one producer
  still missing — its emit sits in an ECS system with no audio handle.
- [Block sound types](./block-sound-types.md) — the per-block-state `SoundType`
  census (break / step / place / hit / fall + volume + pitch) that block sounds were
  waiting on, dumped from the real 26.2 server by `SoundTypeOracle.java`. Measured
  **126** distinct sound types across 32,366 states, so a 126-entry table plus a
  per-state `u8` index is 34,634 bytes against 647,320 for a per-state record. Also
  the three ways a hand-transcription of `SoundType.java` would have been wrong:
  `TWISTING_VINES` is declared with pitch `0.5` and assigned to **no block**,
  `IRON` and `METAL` are not the pairing the names suggest (iron blocks are `IRON`;
  `METAL`'s 1.5 pitch is gold, rails and hoppers), and `HARD_CROP`/`GLOW_LICHEN`
  mix sounds from two families.
- [Keybindings](./keybindings.md) — the rebindable action → input table behind every
  gameplay key, why a `Binding` must hold a mouse button (vanilla's attack and use
  are mouse-bound by default), the `resolve_key` precedence chain that lets chat and
  container screens swallow keys before gameplay sees them, and the persisted format
  in `options.json` — plus the check that F3 *is* a real `KeyMapping` in 26.2 while
  Escape genuinely is not, and `key.swapOffhand`'s **two** mechanisms (a container
  `SWAP` with a screen open, a bare `ServerboundPlayerAction` without one).
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
- [Section mesh invalidation](./section-mesh-invalidation.md) — "air, and air is the
  truth" vs. "air, and air is a guess" as a type rather than a convention (issue #389):
  why a seam meshed before its neighbour arrived draws water twice, why the frontier
  ring had no mechanism to heal it, and the two things vanilla does that we now both do.
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
  stays unbuilt until particles and sounds can consume it. In the same file: the
  shield/bow island **pair** — `ReleaseUseItem` had no producer and the input layer
  had no release edge for `Use` at all, while `use_item_live` returned early on any
  entity under the crosshair instead of falling through to a generic use, so aiming
  at a mob sent nothing. Food hid both, because `useOnRelease() == false` items
  auto-complete on tick count. Plus a live gate firing a real server arrow, and the
  two false beliefs its first draft was built on.
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
  cryptography in either role; `registry_data`, which was ingested by neither side and
  is the root cause of two already-filed bugs (the client half has since landed — see
  [Registry data ingest](./registry-data-ingest.md)); the `CHUNK_BATCH_START` "island" that
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
- [The sky pass and the air-bubble row](./sky-and-air-bubbles.md) — two features
  that landed as complete, tested, unreachable modules and then got wired: the
  sky's own pass before the terrain pass and the `Clear`-vs-`Load` handover that
  must key off whether the sky *actually drew*, the six-hop `airSupply` decode
  chain that did not exist at all, why every sky pipeline deliberately has no
  depth attachment, and the pixel gates — several of which were wrong before they
  were right, in ways that generalise: a frame percentage cannot tell a
  uniform-but-wrong clear from a localised blob, and a control premise can be
  false before the feature under test ever existed. Also (issue #96) where
  vanilla's horizon-to-zenith gradient actually comes from — `sky.fsh` fogging a
  flat disc, per *vertex*, which is the banding — the 26.2 `day.json` timeline
  colour tracks and their gamma-space, floor-rounding byte arithmetic, why
  `sunrise_sunset_color` is ARGB and not RGBA, why the sunrise fan's ring is
  centred on the **eye** rather than on its own apex, void fog's quadratic
  world-bottom falloff, a stale module doc that had rotted from "a duplicate of a
  validated formula" into "a divergent second opinion", and — re-verified against
  #288 rather than taken on faith — why per-biome sky tint is *not* blocked on a
  protocol hop after all: both ends are built, `entry_names` has no caller outside
  its own crate, and the 66 biome files hold only 16 distinct sky colours, with
  `plains` and `swamp` byte-identical so the obvious gate discriminator is vacuous.
- [Weather](./weather.md) — rain, the storm darkening of sky/fog/lightmap, and the
  lightning flash. `ClientEvent::WeatherChanged` was a **fourth** island in `net.rs`'s
  `forward`: decoded, hermetically tested since it was written, and consumed by
  nothing in any of the three routers. Also why the levels ride a shared cell rather
  than `NetUpdate` (~20 superseded messages a second otherwise), why the arm still
  lives in the router anyway, three traps that each look like a bug on this side and
  are not — vanilla's own `START_RAINING` → `0.0` inversion, thunder always being
  multiplied by rain (a stale non-zero thunder level arrives on **every** join), and
  the `SKY_LIGHT_FACTOR` layer split that stops a full storm being darkened twice —
  and exactly what blocks snow: biome ids reach the client but biome *climate* does
  not, so vanilla's temperature predicate is built and tested with no caller that has
  real input.
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
- [Drowning: air-supply countdown and damage](./drowning.md) — issue #267's
  server half: `PlayerVitals` ticking air down while submerged and dealing damage
  on vanilla's exact 320-then-every-20-tick cadence; the discovery that the server
  tracked **no player `y` and no health at all** before this, so this is really
  *server-side player vitals* with drowning as its first consumer; what is
  modelled versus deliberately deferred (Respiration, water-breathing, i-frames,
  non-player entities, death/respawn); and the undrained-`EventStream` trap that
  silently stalled a real-client test with no error anywhere.
- [Underwater and fire screen overlays](./screen-overlays.md) — issues #108 and
  #112: one shared one-bind-group pass, why the underwater tint is **grayscale**
  and independent of `fog.rs` (the blue comes from the texture's own pixels), and
  the on-fire flag's missing session-scoped fold — it decodes, but the local player
  is deliberately excluded from the generic entity-view path that would carry it.
- [Entity and player nametags](./entity-nametags.md) — issue #100: billboarded text
  above named entities and other players, why the two are resolved by genuinely
  different vanilla predicates at `net::entity_snapshot` rather than in the draw
  pass, the jar-verified normal/see-through depth settings including the
  wgpu-forced `Always`/`write:false` substitute for vanilla's "no depth attachment
  at all" (found only by a validation error), and why the occlusion gate asserts
  **pixel-identity** with the occluder baseline rather than mere absence.
- [Death screen](./death-screen.md) — issue #103: why no death screen could ever
  have appeared before this (the client was built with `RespawnPolicy::Automatic`,
  which answered every death packet with an unconditional respawn before the shell
  could react), vanilla's `DeathScreen` layout including the "You Died!" title's
  easy-to-miss `width/4` centering, Escape deliberately doing nothing
  (`shouldCloseOnEsc() == false`), and the live-oracle evidence that chunk
  streaming actually resumes after a manual respawn — the failure mode `CLAUDE.md`
  records as a silent total chunk blackout.
- [Clientbound packet coverage](./clientbound-packet-coverage.md) — the measured
  triage behind #26: `xtask connectedness` at 109/141 decoded, and why the other 32
  gap packets are deliberately **not** decoded — a full A/B/C tier table with the
  decompiled record cited per packet, splitting "blocked on a consumer in another
  crate" from "dormant `ClientAction` but no UI" from "genuinely irrelevant".
  Landing them now would manufacture exactly the island pattern the issue warns
  against. Also explains why `CHUNK_BATCH_START` reads as stranded to the tool and
  is not.
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
- [Shaders](./shaders.md) — every WGSL shader now lives in `src/shaders/*.wgsl` and
  is pulled in with `include_str!`, which deletes the double-quote trap rather than
  documenting it; the `wgsl_valid` test that runs all 22 through naga with no GPU
  (a broken shader used to reach `main` with every `cargo check` green), the
  measured truth that a `"` in a WGSL *comment* is now simply inert, and the five
  byte-identical duplicates left unmerged on purpose.

---

## Plans and research

Longer-form artifacts that are not per-subsystem docs: a phased plan, and read-only
diagnoses produced before the corresponding fix was written. They live here because a
diagnosis is worth keeping *after* the fix lands — CLAUDE.md's standing claim is that
the record of confidently-held false beliefs is the most valuable thing in this repo,
and several of these caught the *brief* being wrong rather than the code.

- [World generation plan](./worldgen-plan.md) — the phased plan behind epic
  [#404](https://github.com/matteopolak/lodestone/issues/404). Opens with a correction
  worth reading first: worldgen is **not** absent, `crates/lodestone-worldgen` is
  ~9.4k lines and its shape stage is 98304/98304 bit-exact against the JVM. The
  highest-value finding is in §2 — the overworld's ~700-entry multi-noise biome table
  looks like a 1,124-line Java port and is actually a **data dump**, because the JSON
  is a two-line preset pointer and the real table is a runtime registry object. §5
  argues for stopping after vegetation rather than building structures.
- [Combat scope](./research/combat-scope.md) — the mechanic-by-mechanic diff against
  the jar that found `ReleaseUseItem` had no producer, so bow and shield could not
  fire at all.
- [Cross-model plant lighting](./research/cross-model-light.md) — why a plant beside a
  solid block went dark, including the falsifiable prediction (all four cross quads
  bake north/south, so east/west neighbours cannot matter) that held.
- [Fog and the night shadow](./research/fog-and-night-shadow.md) — both reports turned
  out to be *hue*, not brightness or distance. Note its midnight keyframe
  transcription (`#0D0D16`) was itself wrong; `as8BitChannel` floors, so it is
  `#0C0C16`, caught by the implementer's own gate.
- [View bobbing and distant water](./research/bobbing-and-water.md) — neither was the
  reported cause: bobbing was a menu-dispatch bug, and the blocky water was the
  singleplayer chunk-stream ring missing its buffer.
- [Container titles](./research/container-titles.md) — three of four reported claims
  were already fixed and pixel-gated; only nine per-screen title anchors were real.
