# Combat: swinging, attacking entities, knockback, and the attack-cooldown indicator

## What it is

Issues #72 (left-click only swung when a dig started), #12 (attacking
entities, taking knockback), and #121 (the crosshair cooldown reticle).
Covers three things from the original change, plus the ticker/indicator pair
added afterward:

1. **The arm now swings on every left-click**, not only the ones that start a
   dig — including a miss (empty air) and attacking an entity.
2. **Left-clicking a living entity sends the real attack packet** — the
   serverbound `Interact`/`Attack` action was fully built and encoded
   (`crates/protocol/v770/src/adapter.rs`'s `InteractEntity` arm) but had zero
   callers anywhere in `lodestone-shell`; this is the first one.
3. **Server-sent knockback now actually moves the local player.** A
   `ClientboundSetEntityMotionPacket` naming our own entity id was silently
   absorbed into a component nothing reads; it now overwrites the physics
   velocity that `player_physics` integrates.

Also landed: the `HurtTime` countdown component (both `EntityDamaged` and
`EntityHurtAnimation` were decoded-but-unconsumed islands before this), the
ECS-side half of the hurt-flash window a future render pass can key off.

Also landed, still under issue #12: the `ReleaseUseItem`/`use_item_live`
fallthrough fix that makes the shield and the bow reachable at all in combat —
see "The shield/bow island pair" below. Two of #12's own named gaps
(attack-strength cooldown bar, hurt tint) already shipped under #121/#98 by
the time of that pass; its "camera shake" line was never a real vanilla
mechanic (confirmed by grepping `client-src` for `[Ss]hake`, one unrelated
hit in `ItemInHandRenderer.java`) — noted here so this doc does not repeat a
stale claim #12 itself still carries.

## How it works

### Swing dispatch (`Sim::begin_attack`, `crates/lodestone-shell/src/sim.rs`)

Vanilla's `Minecraft.startAttack` (`Minecraft.java:1603-1672`) switches on
`hitResult.getType()` and swings the arm **unconditionally after the switch**,
on every arm including a miss. Before this fix, only the "a dig actually
started" arm ever reached `Sim::swing_hand` (through `drive_mining`'s queued
`SwingArm`) — punching air, an entity, or empty space produced no animation.

`begin_attack` now splits into `begin_attack_live`/`begin_attack_demo`, each
implementing the same three-way switch:

- **`ENTITY`** — `Self::attack_entity` sends the packet, then swings.
- **`BLOCK`** (not air) — arms the hold-to-mine loop exactly as before; that
  loop's own `SwingArm` still fires when a dig actually starts.
- **`MISS`** (nothing targeted) — swings and does nothing else.

`case ENTITY` takes priority: `EntityRayTarget` (below) is already the nearer
of an entity-or-block pick, so a `Some` there means mining must not start even
when a block is also targeted (`begin_attack_live_prefers_an_entity_target_over_mining`
in `sim.rs`'s test module pins this).

The demo world (no live connection, no networked entities) only ever sees
`BLOCK` vs `MISS` — there is nothing to populate an `ENTITY` case with.

### Entity targeting (`Sim::update_entity_target`, same file)

A new resource, `EntityRayTarget(pub Option<i32>)` (`crates/lodestone-shell/src/interact.rs`),
holds the server entity id (not a `bevy_ecs::Entity`) the view ray currently
points at. Recomputed every frame alongside the existing block `RayTarget`, by
`Sim::update_target`, against a **shorter** range:

- `ENTITY_REACH = 3.0` — vanilla's `DEFAULT_ENTITY_INTERACTION_RANGE`
  (`Player.java:134`), distinct from and shorter than block `REACH` (`4.5`,
  `Player.java:133`). Creative's `+2.0` modifier (`Player.java:150`) is **not**
  tracked — this shell has no attribute-modifier pipeline for it yet, so every
  session uses the unmodified survival default.
- Further shortened to the block hit's own entry distance when a block sits
  closer than that, so a wall between the eye and an entity is never picked
  through it (vanilla's `blockDistance` clamp in `GameRenderer.pick`). The
  block is treated as a unit cube for this cutoff rather than its real outline
  shape (the shell does not carry outline geometry at all — see
  `Sim::outline_shape_source`'s docs on the same gap); the only effect of that
  approximation is a very slightly conservative cutoff, never a pick through
  solid terrain.

Candidates come from the same `(Position, EntityKind)` query
`Sim::tick_nearby_entities` already uses for the entity-push crowd pass,
resolved to a hitbox through the identical `VersionData::entity_facts` seam —
an unknown entity type is excluded, never approximated. The local player is
never a candidate, with no special-case code needed: `apply_entity_spawn`/
`apply_local_player_login` (`lodestone-ecs/src/ingest.rs`) never give the local
player's own `Entity` a `Position`/`EntityKind` component, so the query
structurally cannot return it.

The actual ray-vs-box test is `crate::raycast::ray_aabb` — a plain slab test
taking raw `min`/`max` triples rather than `lodestone_physics::Aabb`, so
`raycast.rs` keeps the "no `lodestone-world`, no GPU" independence its module
docs already promise for the block DDA raycast next to it.

### Sending the attack (`Sim::attack_entity`)

Lowers straight to `ClientAction::InteractEntity { entity_id, interaction:
EntityInteraction::Attack, sneaking }`, sent directly (like
`Sim::use_item_live`'s sends) rather than queued — an attack is a discrete
click event, not a per-tick one. The v770 adapter already encodes this variant
as the dedicated 26.2 `Attack` packet (`crates/protocol/v770/src/adapter.rs`);
before this change nothing in the shell ever constructed
`ClientAction::InteractEntity` at all, so the encoder was dead, unused code.

The `Attack` packet's wire shape carries only the target entity id — no
damage, no strength scalar. Damage is fully server-authoritative.

### Knockback (`apply_entity_velocity`, `crates/lodestone-ecs/src/ingest.rs`)

Vanilla's `Entity.lerpMotion` (`Entity.java:2649-2651`) is
`this.setDeltaMovement(movement)` — an unconditional **replace** despite the
"lerp" name — and `LocalPlayer` declares no override
(`ClientPacketListener.handleSetEntityMotion`, `:623-629`). So a
`ClientboundSetEntityMotionPacket` naming our own id means "overwrite your own
velocity", not "nudge it".

`apply_entity_velocity` now checks whether the event's `entity_id` resolves
(through `EntityIndex`) to the `LocalPlayer` entity. If so, it writes directly
into `PhysicsState.0.velocity` — the exact field `player_physics` integrates
every `TickSet::Physics` — instead of inserting the generic `Velocity`
component, which nothing reads for the local player (motion comes from
`PhysicsState`, never that component). Before this fix every such packet fell
into that dead branch: **the client took a hit and never moved.**

No staging/pending component is needed. `NetIngest` runs synchronously on the
net thread as each packet decodes, strictly before the driver's next
`GameTick` (see `ingest.rs`'s module docs, "How events get in"), so a plain
overwrite here is picked up by that tick's `player_physics` exactly once —
matching vanilla's one-shot `setDeltaMovement`.

Remote entities are unaffected: they still get the generic `Velocity`
component exactly as before.

### The hurt-flash countdown (`HurtTime`, `crates/lodestone-ecs/src/entity.rs`)

`ClientEvent::EntityDamaged` and `ClientEvent::EntityHurtAnimation` were both
**decoded, unconsumed islands** before this change — real events, correctly
produced by the v770 adapter, with zero `match` arms anywhere in
`lodestone-ecs`/`lodestone-shell`/`lodestone-client`. Worse: neither was even
in `ingest::handles_event`'s routing `matches!`, so `SharedState::apply`
(`crates/lodestone-client/src/state.rs`) never sent them to `NetIngest` at
all — they fell into the legacy `Inner::apply` fallback and vanished
regardless of what a hermetic test showed (a test that pushes straight onto
`IngestQueue` and runs the schedule bypasses that routing gate entirely; see
`handles_event_covers_exactly_the_variants_with_a_system` in `ingest.rs`'s
test module for the control that catches this class of bug).

Both events now reset a `HurtTime(pub u32)` component to `10` ticks — vanilla's
`LivingEntity.handleDamageEvent` (`:2044-2049`) and `LivingEntity.animateHurt`
(`:1873-1876`) write the identical `hurtDuration = 10; hurtTime = hurtDuration;`
pair, so one countdown covers both reports. `tick_hurt_time`
(`TickSet::Animate`) ages it toward zero, one per `GameTick`, matching
vanilla's per-tick decrement. `EntityHurtAnimation`'s `yaw` field is not
carried into the component — vanilla's own override accepts the parameter and
never stores it either.

### The per-entity hurt/death red overlay (issue #98, entity half)

Issue #98 asked for a "hurt flash and screen shake." Before writing any code,
both premises were checked against `.cache/mc/26.2/client-src` directly, and
**neither is a full-screen effect in vanilla**:

- `ScreenEffectRenderer.java` — the class this port's underwater/fire overlay
  pass (`crate::screen_effects`, `docs/screen-overlays.md`) already
  transliterates — has zero `hurt` references. `Gui.java`, `LevelRenderer.java`
  and `GameRenderer.java` were also grepped clean for any screen-space quad tied
  to `hurtTime`. The only two things vanilla ties to the **local player's own**
  `hurtTime` are `bobHurt` (a camera *roll*, `GameRenderer.java:297-313`) and
  the per-entity overlay below — there is no third, separate "screen effect."
- **`bobHurt` is issue #58's, not #98's**, by #58's own checklist text: "`bobHurt`
  — the damage tilt, from `hurtTime`/`hurtDuration` and `hurtDir`. This is the
  'screen tilt thing': a roll about the damage direction, not a shake." Issue
  #98's own body agrees — "Not the same as bobHurt (#58)". This repo's
  `camera_rig.rs` was deliberately left untouched by the #98 work below to
  avoid duplicating or conflicting with whoever picks up #58.
- **No camera-shake mechanism exists anywhere in `client-src`**, for
  explosions or anything else: grepping `client/` for `[Ss]hake` turns up only
  the bow-draw item wobble in `ItemInHandRenderer.java` (a held-item pose
  animation, unrelated to the camera). `ClientExplosionTracker.java` — the
  class that *does* react to a nearby explosion client-side — only ever spawns
  particles; it holds no camera reference at all. Issue #98's "vanilla shakes
  the camera on nearby explosions" does not hold up, and nothing was built for
  it here. If this is still wanted, it is a **new game-feel mechanic**, not a
  vanilla port, and should be scoped as one explicitly rather than silently
  invented under a "port" issue.

What vanilla **does** have, and what this pass actually builds, is a per-entity
model overlay: `LivingEntityRenderer.java:281` sets `state.hasRedOverlay =
entity.hurtTime > 0 || entity.deathTime > 0`, sampled from the baked
`OverlayTexture` lookup (`OverlayTexture.java`) — the `y < 8` row is a flat
ARGB `-1291911168` for every `x`, i.e. `(178, 255, 0, 0)`: pure red at alpha
`178/255`. This is a **blend toward red, not a multiply** — vanilla's own
overlay is composited over the shaded texel, not folded into the tint's
gamma-multiply, and treating it as a multiply would crush the mob toward black
instead of washing it red. It applies to *any* drawn living entity — including
the local player's own third-person body — never the local player's own
screen; a first-person player cannot see their own overlay, matching vanilla
(there is no first-person hurt feedback beyond `bobHurt`, #58's territory, and
the HUD heart flash, `hud.rs`'s territory).

**The render mechanism**, in `crates/lodestone-render/src/entity_pipeline.rs`:

- `EntityInstanceRaw::tint`'s previously-unused top byte (bits 24–31) now
  carries the overlay's alpha, `HURT_OVERLAY_ALPHA_BYTE = 178` when active, `0`
  when not — the tint word already rides the instance buffer at wgpu's
  4-bind-group floor (see the field's own doc on why a fifth bind group is the
  one thing this shader cannot afford), so this reuses that byte instead of
  adding a new vertex attribute or a new group.
- `EntityInstanceRaw::with_hurt_overlay(bool)` is the builder, chainable with
  `with_tint` (dyed leather plus a hurt wearer both read back correctly — see
  `hurt_overlay_shares_the_tint_word_without_colliding`).
- `ENTITY_WGSL`'s `fs_main` blends `mix(shaded, vec3(1.0, 0.0, 0.0), in.overlay)`
  in the same gamma-space stage the tint/shade multiply already uses, then
  round-trips back to linear — one round-trip, matching the shader's existing
  convention rather than adding a second one.
- Vanilla's gate is boolean (`hurtTime > 0`), not a fade by how much of
  `hurtTime` remains, so `with_hurt_overlay` takes a `bool`, not a
  `0.0..=1.0` strength.

**The production wiring (the part that was an island for one session).** When
`with_hurt_overlay` landed, production called it **zero times anywhere in
`lodestone-shell`** — a real, pixel-gated mechanism with no data feeding it,
which is `CLAUDE.md`'s dominant defect class and the twelfth confirmed instance
of it. The chain that closes it, every hop shipped:

```text
ClientboundHurtAnimationPacket / ClientboundDamageEventPacket
  -> ClientEvent::EntityHurtAnimation / ::EntityDamaged
  -> ingest::handles_event                       (the routing switch; already listed
                                                  both arms as of 24943a3)
  -> ingest::apply_entity_hurt_animation
     / ::apply_entity_damaged                    -> HurtTime(10)
  -> ingest::tick_hurt_time                      (TickSet::Animate, one per GameTick)
  -> entities::extract_entity_draws              -> EntityDraw::hurt
  -> gpu::prepare_entities / ::prepare_armour / ::prepare_wool
  -> InstanceTint { rgb, hurt } -> upload_instances_tinted
  -> EntityInstanceRaw::with_hurt_overlay        -> ENTITY_WGSL fs_main -> pixels
```

Three decisions in there are worth knowing before changing any of it.

**1. `hurt` rides `EntityDraw`, not `EntitySnapshot`.** The spec this section
used to carry called for an `EntitySnapshot::hurt` field. That was the wrong
hop: `HurtTime` lives on the *ingest* entity, not the render entity, so a
snapshot field would need the value copied ingest → snapshot → draw, and
`EntitySnapshot` is the second of three pose copies that `docs/bevy-migration.md`
Stage 1 deletes outright. `extract_entity_draws` already bridges the two entity
families through `EntityIndex` for `AttackSwing`; `HurtTime` reads the same way,
one hop shorter, and ~15 `EntitySnapshot` literals across the test suite needed
no edit.

**2. `hurt` is `HurtTime(n).0 > 0`, never `HurtTime` being present.**
`tick_hurt_time` saturates at zero and **leaves the component attached**, so a
presence check leaves every mob that was ever hit permanently red. There is a
test for exactly this third state
(`a_ticking_hurt_time_reaches_the_extracted_draw_and_expires`).

**3. `upload_instances_tinted` takes `&[InstanceTint]`, not `&[[u8; 3]]` plus a
parallel `&[bool]`.** The obvious patch was a second slice beside the tints;
`InstanceTint { rgb, hurt }` bundles them so a tint cannot travel without its
overlay flag. Same move as `sprite_rect` returning its atlas alongside its rect.

**Why `prepare_entities` plans twice.** `plan_entities` groups by model and
drops the input order, and `EntityInstance` (in `lodestone-render`'s
`entity.rs`) carries only the light byte — so the flag cannot be zipped back
onto a batch afterwards. The instances are split by `EntityDraw::hurt` *before*
planning and each half's flag stays attached to the plan it produced, as a
`(bool, EntityFrame)` pair. Effectively grouping by `(model, hurt)`: one extra
batch per hurt model while its 10 ticks run, and nothing at all otherwise
(`plan_entities` on an empty slice returns no batches). **If you widen
`EntityInstance` to carry the overlay directly, delete the split rather than
keeping both** — two mechanisms for one flag is how they drift.

**Armour and wool redden too**, because vanilla's overlay is sampled by every
layer of a `LivingEntityRenderer`'s model, not just the body. A hurt mob whose
breastplate stayed its own colour would read as a rendering fault.

**Still not wired, and why.**

- **`deathTime`.** Vanilla's gate is `hurtTime > 0 || deathTime > 0`; nothing
  decodes the death animation on this side of the wire, so the overlay ends
  ~10 ticks after the killing blow instead of persisting through the fall-over.
  That is the only known divergence.
- **The local player's own body.** `gpu/sources.rs`'s `into_draw` passes
  `hurt: false` by construction: the local player has no ingest entity carrying
  `HurtTime` (`apply_local_player_login` gives it no `EntityKind`/`Position`),
  and with no third-person camera there is nothing to see either. The identical
  gap blocks `on_fire` — see `docs/screen-overlays.md`. Both unblock together
  with the same `entity_view()` reachability fix.

**The gate is `crates/lodestone-shell/tests/hurt_overlay_pixels.rs`**
(`-- --ignored --nocapture`). It pushes a `ClientEvent` into the real
`IngestQueue` and reads texels out the other end, touching no render-crate type
directly — the render crate's own gate
(`entity_hurt_overlay_pixels.rs`) was green throughout the session in which
nothing called the code it tested, which is precisely why a second gate through
production was needed rather than a stronger version of the first. It measures
by location against a run-time-derived silhouette mask, and asserts **zero**
changed pixels outside it — the mechanical proof this is still a per-model
blend and not a screen-space tint.

What it printed on the run that landed this:

```text
=== HURT OVERLAY PRODUCTION-PATH PIXEL GATE (#98) ===
overlay alpha byte: 178 (vanilla OverlayTexture red row)
EntityDraw::hurt  subject: false -> true | control: false -> false
zombie silhouette (rest vs no entities): x[132..187] y[65..173], 3440 px
sky bytes: [62, 118, 211]; already-red pixels in the entity-less frame: empty
reddened by the overlay: x[132..187] y[65..173], 3440 px
control (never damaged): moved empty, reddened empty
flag forced off vs rest: empty (must be empty)
control with the flag forced on: reddened x[132..187] y[65..173], 3440 px
```

All 3440 silhouette pixels redden and **zero** pixels outside it move. The
`sky bytes` and `already-red … empty` lines are premise 2 doing its job: this is
a red-tint gate, so the entity-less frame is checked to contain nothing
red-dominant before any redness is attributed to the overlay.

**Both negative controls were run and watched failing**, not described — each by
breaking one hop and re-running:

- *`extract_entity_draws` ignores `HurtTime`* (the island as it actually
  shipped) → `EntityHurtAnimation reached ingest but EntityDraw::hurt is still
  false`, exit 101.
- *`prepare_entities` drops the flag* → `EntityDraw::hurt  subject: false ->
  true` still prints, the data half is entirely green, and then
  `reddened by the overlay: empty` →
  `only 0 of the zombie's 3440 silhouette pixels moved toward vanilla's overlay
  red`. **This is the one that matters**: it reproduces the exact defect shape
  where every unit test passes and the screen is wrong, and shows the gate
  catches it. A gate that only checked `EntityDraw::hurt` would have been green
  through it.

### The attack-strength ticker and the crosshair indicator (issue #121)

Built as one unit deliberately: the ticker (state) and the crosshair reticle
(the thing that displays it) were left unbuilt together in the original
combat change specifically so they would not become two separate unconsumed
islands — see the git history around `24943a3` and the previous revision of
this doc's "deliberately not built" section.

**The ticker** is `AttackStrengthTicker(pub u32)`
(`crates/lodestone-ecs/src/player.rs`), a component on the `LocalPlayer`
entity mirroring vanilla's `attackStrengthTicker` field
(`Player.java:210`/`268`). `tick_attack_strength`, registered in
`LocalPlayerPlugin::build` under `TickSet::Animate` (already chained after
`Physics`/`Predict` by `CorePlugin`, so no new ordering edge was needed),
increments it by exactly `1` every `GameTick` — vanilla's unconditional
`this.attackStrengthTicker++` in `Player.tick()`. `spawn_local_player`/
`reset_local_player` start it at `0`, matching Java's bare-`int` default.
`Sim::attack_entity` (`crates/lodestone-shell/src/sim.rs`) resets it to `0`
synchronously on every entity attack — vanilla's
`MultiPlayerGameMode.attack` calling `player.resetAttackStrengthTicker()`
right after the client-side `player.attack(entity)`
(`MultiPlayerGameMode.java:425-430`). Unconditional, because this shell has
no client-side `cannotAttack` gate (damage is fully server-authoritative, see
above): every left-click on an entity restarts the cooldown, matching what
the real client does before any server response is known.

**The delay is not a constant.** `Sim::attack_strength_delay` implements
vanilla's `getCurrentItemAttackStrengthDelay`, `(1.0 /
getAttributeValue(Attributes.ATTACK_SPEED)) * 20.0` (`Player.java:1816-1818`).
It reads `minecraft:attack_speed` off the local player's own `Attributes`
component (`crates/lodestone-ecs/src/entity.rs`) through
`lodestone_entity::attribute::attribute_value` — the same server-fed,
three-stage-`calculateValue` fold `player_physics` already uses for
`WATER_MOVEMENT_EFFICIENCY` (Depth Strider). This was checked rather than
assumed, per two things worth re-verifying named in this task's brief:

- `lodestone-entity`'s attribute census (`crates/lodestone-entity/src/
  attribute.rs::default_def`) already carries `"attack_speed" => d(4.0, 0.0,
  1024.0)` — the correct vanilla default and clamp range — so no new default
  table was needed.
- `lodestone-data`'s `item_prototypes` census (`crates/lodestone-data/src/
  item_prototypes.rs`) does **not** carry attack speed at all — it covers
  only `max_stack_size`/`max_damage`/`equip_slot`. That is not a gap here,
  though: a weapon's `-2.4` (sword)/`-3.0` (axe) attack-speed modifier
  arrives the same way any other equipment-driven attribute change does — a
  server `update_attributes` packet the instant the held item changes
  (`AttributeMap`'s dirty-tracking on `LivingEntity.setItemSlot`), already
  folded into `Attributes` by `apply_entity_attributes`
  (`crates/lodestone-ecs/src/ingest.rs`). Nothing per-item needed adding.

Before the first `update_attributes` for the local player (a fresh demo-world
player, or a live session before login's fold lands), `attribute_value` falls
back to the registry default `4.0` (unarmed), giving the correct 5-tick delay
rather than a guess. `Sim::attack_strength_scale` combines ticker and delay
into vanilla's `getAttackStrengthScale(0.0F)` — the exact call
`Hud.extractCrosshair` makes for the crosshair-style indicator
(`Hud.java:448`) — clamped to `0.0..=1.0`.

**The indicator** is `HudFrame::attack_cooldown: Option<f32>`
(`crates/lodestone-shell/src/hud.rs`), populated unconditionally in `app.rs`
(`Some(self.sim.attack_strength_scale())` — unlike `health`/`food`/`xp`, both
the ticker and the attribute default exist before any server connection, so
this is never `None` on a real run) and drawn inside `HudGeometry::
build_inner`'s existing crosshair block, nested under `if frame.crosshair`
right alongside the white-plus reticle. That nesting is deliberate: it reuses
the same visibility gate the crosshair itself already has (issue #51's
container-screen suppression), rather than inventing a second one.

The two sprites (`hud/crosshair_attack_indicator_background`,
`hud/crosshair_attack_indicator_progress`, both 16x4 native) were already
present in the GUI atlas — `GuiAtlas` globs `gui/sprites/**`, and the
air-bubble/hotbar work had already established that pattern holds — so no
asset plumbing was needed, only the draw call. `Builder::sprite`/
`gui_geometry` are no-op-safe with no atlas attached (see `sprite_vitals`'s
own doc), so a jar-less/headless run draws nothing here instead of needing a
second procedural implementation — the same choice already made for the
underwater bubble row. The progress bar is cropped by shrinking both the
destination width and the sampled UV span to the cooldown fraction, the exact
idiom `sprite_vitals` already uses for the XP-bar progress fill.

**Scope cuts, both deliberate:**

- **Only `AttackIndicatorStatus::CROSSHAIR`.** Vanilla's real enum
  (`AttackIndicatorStatus.java`) has three variants — `OFF`, `CROSSHAIR`,
  `HOTBAR` — and issue #121 explicitly scoped this shell to ship the default
  (crosshair) only, noting the options-menu toggle as future work (#32/#55
  own that menu). There is no `Options::attack_indicator` read anywhere; the
  indicator always draws whenever the crosshair does.
- **No full-charge "ready" icon.** Vanilla replaces the fill bar with a
  distinct `CROSSHAIR_ATTACK_INDICATOR_FULL_SPRITE` circle when the scale
  reaches `1.0` *and* the crosshair is over a living, in-range target *and*
  the held weapon's delay exceeds 5 ticks (`Hud.java:450-465`) — a slow-weapon
  "you're ready" nicety. That needs the crosshair's entity target plus its
  liveness/range, none of which `HudFrame` carries today. At full charge this
  shell simply draws nothing (matching vanilla's non-"ready" default case),
  which is also the correct control for the pixel gate below: the indicator
  must produce **zero** pixels at `attack_cooldown = Some(1.0)`, not the full
  circle.

### The shield/bow island pair: `ReleaseUseItem` and `use_item_live`'s fallthrough

A combat-scoping pass (docs/backlog.md item 7, re-verified against the jar
rather than assumed) found the shield and the bow **functionally dead**, for
two independent, zero-ambiguity reasons — the highest-value gap this doc
tracks, because both items are trivial to obtain in survival and a stranger
hits the dead control within the first hour.

**Finding 1 — `ClientAction::ReleaseUseItem` was a serverbound island.**
Encoded by all four protocol adapters (v47/v340/v735/v770, `PLAYER_ACTION`
action id `5`) since whenever each adapter landed, with **zero producers**
anywhere in `lodestone-shell` — the outbound mirror of the inbound-island
class this doc's own history already has two instances of
(`EntityDamaged`/`EntityHurtAnimation`, the ticker/indicator pair above).
`app.rs`'s mouse-input match had both edges of `Attack` (`begin_attack`/
`end_attack`) but only `(Use, Pressed)` — no release arm existed at all, on
mouse or keyboard.

Vanilla's `LivingEntity.updateUsingItem` (`LivingEntity.java:3471-3475`)
auto-completes a use only when `!useItem.useOnRelease()`. Food and potions
are `useOnRelease() == false`, so they auto-complete on the server's own tick
count and *appear* to work fine with no release packet at all — exactly why
this stayed invisible. Bow, crossbow and shield are all
`useOnRelease() == true` (`:3602-3616`, `releaseUsingItem`/`stopUsingItem`)
and structurally cannot fire or lower without the explicit packet.

Fix, mirroring vanilla's own gate (`Minecraft.java:1914-1917`,
`this.player.isUsingItem()` guarding `gameMode.releaseUsingItem`):

- `KeyOutcome::Use` became `Use(bool)` (`app.rs`); both the mouse match and
  the keyboard `resolve_key` chain now route a release edge to a new
  `Sim::end_use`.
- A new resource, `UsingItem(pub bool)` (`crates/lodestone-shell/src/
  interact.rs`), is the client-side mirror of `isUsingItem()`. Vanilla's own
  flag is a side effect of the held item's `use()` running identically
  client- and server-side (a bow's `use()` calls `LivingEntity.
  startUsingItem` on both) — this client has no per-item `use()` simulation
  to drive an equivalent flag from, so `UsingItem` is an **input-state**
  mirror instead: set `true` at the top of `Sim::use_item_live` (every live
  press), cleared by `Sim::end_use_live`. That is a superset of vanilla's
  real gate — this client cannot yet tell whether a use is *actually* in
  progress server-side, only that the button is down — but the gap is inert,
  not a wrong transition: `LivingEntity.releaseUsingItem`
  (`.cache/mc/26.2/src/…/LivingEntity.java:3602-3613`) already no-ops
  whenever the server has nothing in progress, so an extra release is a
  harmless duplicate.
- `Sim::end_use`/`end_use_live` split the same way `begin_attack`/
  `begin_attack_live` do, purely so the send logic is reachable from a
  hermetic test with no `vanilla_atlas`.

**Finding 2 — `use_item_live` could not even *start* a use in the situations
combat happens in**, independent of Finding 1. Two structural gaps in
`Sim::use_item_live` (`sim.rs`):

- Whenever the crosshair was over **any** entity — hostile or not, which is
  the overwhelmingly common combat case — the method called
  `Self::interact_entity` and returned **unconditionally**, never falling
  through to the generic use-item send. Vanilla's own `case ENTITY`
  (`Minecraft.java:1693-1708`) only returns early on a **successful**
  interact (`instanceof InteractionResult.Success`); anything else hits an
  explicit `break;` at `:1708` and falls through to the unconditional
  generic-use call at `:1730` (`gameMode.useItem`) — the call that actually
  raises a shield or starts a bow draw. Most hostile mobs have no special
  right-click behaviour, so this is the common path, not an edge case.
- With **no** target at all — open air, or a mob standing just past block
  reach with nothing behind it — the method `return`ed with **nothing sent**.
  Vanilla's own `hitResult == null` path skips the whole
  `if (this.hitResult != null)` switch (`Minecraft.java:1681,1691`) and still
  reaches the same unconditional fallback at `:1730`.

Fix: `use_item_live`'s entity branch now calls the new
`Self::use_item_generic` after `interact_entity` instead of returning, and
the no-target branch calls it instead of returning empty-handed.
`use_item_generic` lowers to `ClientAction::UseItem` — a **second**
serverbound island this pass found alongside `ReleaseUseItem`, encoded by all
four adapters with zero producers before this fix — guarded on the main hand
actually holding something (vanilla's own `!heldItem.isEmpty()` check at the
same call site), and borrows its block-prediction `sequence` from
`Placement::take_use_sequence` rather than a second independent counter,
matching vanilla's single shared `BlockStatePredictionHandler` sequence
(`MultiPlayerGameMode.startPrediction`,
`.cache/mc/26.2/client-src/…/MultiPlayerGameMode.java:293-299`).

**Deliberate divergence from vanilla, and why:** vanilla's `case ENTITY` only
skips the fallback on a *locally classified* successful interact
(`player.interactOn`, run client-side for prediction). This client has no
such classification — there is no per-item interact simulation, only the wire
send (the same gap `Self::interact_entity`'s own docs cover for why
`InteractAt`'s hit position is not fabricated here) — so every entity
interact is now treated as non-consuming and *always* falls through. The one
place this can disagree with vanilla is a genuinely successful mount (an
empty boat, a saddled rideable): vanilla's local prediction would skip the
fallback there, and this shell does not, so a held item can also start its
use while boarding a vehicle. That is judged the smaller error next to a
shield/bow that could never fire at all, and it is the same "err toward
sending" trade `Self::use_item_live`'s block-placement half already makes in
the opposite direction (never predict something wrong, but here there is no
prediction to get wrong — only a decision between never sending and
sometimes sending one redundant packet).

**A false belief worth recording.** The first pass at `UsingItem` only
registered it in `Sim::end_session`'s explicit `insert_resource` block — the
same place `Attacking`/`MiningPredictor`/`PlacementPredictor` are reset on
reconnect — on the assumption that mirroring their reset call was sufficient
for a resource genuinely new to this session's state. It was not: those three
are *also* registered via `InteractPlugin::build`'s `app.init_resource::<…>()`
calls, which is what actually creates them on a **first** session (`end_session`
only runs when an existing session ends, never before the first connect). A
hermetic test calling `Sim::end_use_live()` on a fresh `Sim::new(…)` panicked
immediately with "Requested resource … does not exist in the World" —
caught by the test, not by either `cargo check` (resource lookups are a
runtime `bevy_ecs` panic, invisible to the type checker) or by hand-reading
the diff, which looked complete. Fixed by adding
`app.init_resource::<UsingItem>()` alongside its siblings.

## What is deliberately not built here

**Vanilla's crit condition and the sweep-attack condition.** These are real
per-hit vanilla mechanics (`Player.java:951-1053`'s `attack()`), but both
exist only to trigger **local** sound/particle feedback — the damage number
itself is server-authoritative, and the wire `Attack` packet carries none of
it. Their consumer is `entities.rs`/asset work (particles, sounds), out of
`lodestone-shell/src/{sim.rs,interact.rs,hud.rs}`'s scope for this pass, and
they need real particle-emitter and sound-cue plumbing this shell does not
have wired to combat yet. Building either now — with the ticker/indicator
already landed as the natural place a crit read would plug into
(`Sim::attack_entity`'s call site) — would still orphan the sound/particle
half, so they stay unbuilt rather than half-started. Whoever adds the sweep
sound or the crit particle burst is the natural owner.

**`HurtTime` now reaches pixels** (issue #98, the entity half) — the chain and
its three design decisions are in "The per-entity hurt/death red overlay"
above. It did not for one session: the render mechanism landed with **zero**
callers in `lodestone-shell`, and the sentence that used to stand here said so.
Note what that sentence got *wrong* even while its headline was right — it
named `EntitySnapshot` and two `gpu.rs` line numbers as the patch site, and the
real fix went through `extract_entity_draws` instead and touched neither. A
patch spec written from the outside ages faster than the claim it wraps; verify
the shape before following one. Local-player-specific
camera feedback (`bobHurt`, the "screen tilt thing") is issue #58's, not #98's
or this component's — confirmed against #58's own checklist, which already
names `bobHurt`/`hurtTime`/`hurtDir` as its scope. There is no vanilla
full-screen colour overlay or camera shake for taking damage at all — see
issue #98's section above for the jar evidence.

## Configuration

- `crates/lodestone-shell/src/sim.rs::ENTITY_REACH` — `3.0`, vanilla's
  unmodified `DEFAULT_ENTITY_INTERACTION_RANGE`. No creative/attribute
  modifier tracked.
- `crates/lodestone-ecs/src/ingest.rs::HURT_DURATION_TICKS` — `10`, vanilla's
  `hurtDuration` constant.
- `crates/lodestone-render/src/entity_pipeline.rs::HURT_OVERLAY_ALPHA_BYTE` —
  `178`, vanilla's hurt/death overlay alpha (`OverlayTexture`'s red row,
  `-1291911168`'s alpha channel). Not configurable; matches
  `LivingEntityRenderer.java:281`'s boolean gate exactly, with no fade.
- The attack-strength delay has no standalone constant: it is
  `20.0 / attack_speed_attribute`, computed fresh in
  `Sim::attack_strength_delay` every call. The only literal is the registry
  default `4.0` (unarmed attack speed), which lives in
  `lodestone-entity/src/attribute.rs::default_def` — not duplicated in
  `lodestone-shell`.
- `crates/lodestone-shell/src/hud.rs`'s crosshair block hardcodes the
  indicator's native size (`16x4`) and offset (`cx - 8, cy + 9`) — vanilla's
  own constants (`Hud.java:457-458`), not configurable.

## Dependencies

- `lodestone_model::{ClientAction::InteractEntity, EntityInteraction}` — the
  outbound action shape (`crates/lodestone-model/src/action.rs`).
- `crates/protocol/v770/src/adapter.rs` — the existing `InteractEntity` encode
  arm this change is the first caller of.
- `VersionData::entity_facts` (`lodestone-model`/`lodestone-v770`'s entity
  census) — entity hitbox dimensions for the attack ray, the same seam
  `tick_nearby_entities` already depends on for the push crowd pass.
- `lodestone_ecs::entity::{MinecraftEntityId, Position, EntityKind, HurtTime,
  Attributes}`, `lodestone_ecs::player::{PhysicsState, AttackStrengthTicker}`
  — the ECS components this change reads and writes.
- `lodestone_entity::attribute::attribute_value` and
  `crates/lodestone-entity/src/attribute.rs::default_def`'s `"attack_speed"`
  entry — the attribute fold and its registry default, both pre-existing and
  reused rather than duplicated.
- The GUI atlas's `hud/crosshair_attack_indicator_background`/
  `hud/crosshair_attack_indicator_progress` sprites (already stitched from
  `client.jar` — no new asset plumbing).
- `lodestone_render::entity_pipeline::{EntityInstanceRaw, HURT_OVERLAY_ALPHA_BYTE}`
  — the per-entity hurt/death overlay's render-side mechanism (issue #98).
  Currently reached only by `crates/lodestone-render/tests/
  entity_hurt_overlay_pixels.rs`; not yet called from `lodestone-shell`.

## How to change it

- Adding crit/sweep sound-and-particle feedback: start from
  `Sim::attack_entity` (`sim.rs`) — that is where the outbound send already
  happens and where `AttackStrengthTicker`/`attack_strength_scale` are
  already read, so a `criticalAttack`/`fullStrengthAttack`/`sweepAttack`
  decision has everything it needs except the particle-emitter/sound-cue
  plumbing. See "What is deliberately not built here" above.
- Adding the hotbar-style attack indicator or the `AttackIndicatorStatus`
  options toggle: `HudFrame::attack_cooldown` already carries the fraction;
  the missing half is an `Options`-driven read gating which of
  `hud.rs`'s crosshair-block draw (already built) vs. a new hotbar-adjacent
  draw (not built) runs, mirroring vanilla's `extractItemHotbar`
  (`Hud.java:606-621`).
- Adding the full-charge "ready" icon: needs the crosshair's live entity
  target's liveness/range plus the held weapon's delay compared against `5`
  ticks (`Hud.java:450-455`) threaded into `HudFrame`, then a third sprite
  branch (`hud/crosshair_attack_indicator_full`, already in the atlas)
  alongside the existing background/progress branch in `hud.rs`.
- Changing the delay's item-attribute-modifier source: it is **not** a
  per-item census read (`lodestone-data`'s `item_prototypes` deliberately has
  none) — it is whatever the server's `update_attributes` packets put in the
  local player's `Attributes` component. A demo-world (offline) weapon swap
  will *not* change the delay, because nothing generates that packet without
  a server; that is a real, current gap for offline testing, not a bug in the
  live path.
- Adding the render-side hurt tint: the mechanism (`EntityInstanceRaw::
  with_hurt_overlay`, `entity_pipeline.rs`) already exists and is pixel-gated;
  `HurtTime` already exists and already counts down correctly. The missing
  half is entirely `entities.rs` (build a per-entity `hurt: bool`) plus
  `gpu.rs` (thread it into the `upload_instances*` call sites) — see the exact
  spec in "The per-entity hurt/death red overlay (issue #98, entity half)"
  above.
- Changing entity reach/attribute modifiers: `ENTITY_REACH` is a `const`
  today, not attribute-driven. Creative's `+2.0` needs an attribute-modifier
  pipeline this shell does not have; do not hardcode `5.0` for creative
  without one, since gamemode isn't read at that call site today either.
- `EntityRayTarget`/`RayTarget` must stay computed from the *same* ray
  (`Sim::update_target` calls `update_entity_target` with the exact `origin`/
  `dir`/`block_hit` it just used) — computing them independently would let the
  two disagree about, e.g., a diagonal look direction.

## The hurt-overlay gate, and what it printed (issue #98)

`#[ignore]`d GPU gate, needs no `client.jar` (a synthetic flat sheet, like
`entity_variant_pixels.rs`'s): `cargo test -p lodestone-render --test
entity_hurt_overlay_pixels -- --ignored --nocapture`. Actually run on this
machine, not predicted:

```text
=== HURT OVERLAY PIXEL GATE ===
mob bbox: x[96..159] y[0..229], area 9498 px
control A (no with_hurt_overlay call) vs control B (with_hurt_overlay(false)): 0 px differ (must be 0)
determinism (hurt x2): 0 px differ (must be 0)
reddened mob pixels: 9498 / 9498
background pixels changed by the overlay: 0 (must be 0)
overlay alpha byte: 178 (vanilla OverlayTexture red row, LivingEntityRenderer.java:281)
```

Both controls are executed, not described: control A/B proves `false` (the
`HurtTime == 0` case) is bit-identical to the code path that existed before
this change, and the background count proves the effect never leaks outside
the entity's own silhouette — it cannot become a de-facto full-screen tint by
accident. 100% of the silhouette reddened rather than a fraction, because the
comparison is per-pixel against that same pixel's own pre-overlay colour
(cancelling out per-face diffuse shading), not a global average — see the
gate's own module doc for why a bounding box is printed on every run, not just
on failure.
