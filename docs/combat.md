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

**Landed in the pass that added this paragraph**, closing issues #16/#27 and
the rest of #12's genuine remainder:

- **The `Q`/`Ctrl+Q` drop key** — two proven serverbound/click islands
  (`ClientAction::DropSelectedItem`/`DropSelectedItemStack`, and
  `Click::drop_one`/`drop_stack`/`do_throw`) closed by one binding. See "The
  drop key (`Q`)" below.
- **Crit particles**, as local-only prediction in `Sim::attack_entity`. See
  "Crit particles" below.
- **The sweep-attack particle is now built** in a later pass, split into its
  own issue rather than left as this issue's last hop. See "The sweep-attack
  particle" below.
- **`bobHurt` was re-confirmed still blocked** on `Camera` gaining a roll
  degree of freedom, a cross-cutting change out of this pass's scope. See
  "`bobHurt`, still blocked" below.

**Landed in a later pass, closing #12's real remainder** — everything above
this bullet is about the *client*: swinging, sending the attack packet,
taking knockback. None of it made attacking a mob **on our own server** do
anything, because `lodestone-server` never decoded the attack packet at all.
See "The integrated-server melee-damage gap" below for the full account:
`ServerBound::Attack`/`PlayerInput` decode, `MobHandle` (the mutation handle
issue #12's own census said was missing), `MobSim::attack` (damage +
knockback in one call), and a real client proving the exact predicted health
and knockback position land. Mob-on-player damage gets a real, tested
`PlayerVitals::apply_damage` entry point but **not** a live trigger — no AI
in this workspace gives a mob the player's position to attack, a separate,
larger feature; see that section's own scope note. Shield blocking remains
entirely unbuilt (unrelated to `damage.rs`'s pipeline — needs an item-data
model this workspace does not have).

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

**`deathTime` is now wired, and it was two islands rather than one.** Vanilla's
gate is the full disjunction `hurtTime > 0 || deathTime > 0`, and the second
operand had no component on this side, so the overlay ended ~10 ticks after the
killing blow — a mob went red, turned its normal colour again, and only *then*
fell over. Live player report: *"stuff dying doesnt have the death animation (the
one where they turn red and tilt on their side)"*. The chain now runs:

`ClientEvent::EntityStatus` (byte 3, `EntityEvent.DEATH`) → `route()`'s `ingest`
flag → `ingest::apply_entity_status` → `entity::DeathTime` →
`ingest::tick_death_time` (counting **up**, `LivingEntity.tickDeath`) →
`EntityDraw::death_time` → both the red overlay's second operand and the
fall-over rotation.

Three things about it that are not guessable from the shape:

* **`EntityStatus` was routed nowhere at all** — it sat in `event.rs`'s "claimed
  by nothing" list, so no byte it carried reached any system. Only byte 3 is
  claimed now; the other ~40 codes are dropped by `apply_entity_status` rather
  than by the routing table, because `route` answers "is anything *asked*".
* **`DeathTime` is inserted at zero, and absence means alive.** Vanilla's
  `deathTime` is still `0` when `die()` runs and only reaches `1` on the next
  `tickDeath()`, and both consumers test `deathTime > 0` — so the first tick of
  death draws upright and un-toppled, and the killing blow's own `HurtTime` is
  what keeps it red across that one frame. `extract_entity_draws` reproduces
  vanilla's ternary (`deathTime > 0 ? deathTime + partialTicks : 0.0F`) rather
  than a bare sum, or that first tick would start both effects mid-frame.
* **The rotation is not linear in `deathTime`.** It is
  `sqrt((deathTime - 1)/20 · 1.6)` clamped to 1, times `getFlipDegrees()`'s 90 —
  see `entity_anim::death_fall_over_degrees`. It saturates at `deathTime == 13.5`,
  not 20, so the mob lies flat for the last ~6.5 ticks before the server removes
  it; and "90° over 20 ticks" happens to agree at exactly `deathTime == 20`,
  which is the one tick a gate must not be written at alone.

**This now fires in singleplayer.** `ServerProtocol::encode_entity_event` exists,
`V770ServerProtocol` implements it against `ClientboundEntityEventPacket.write` (a
**fixed-width big-endian `i32`** id, not a VarInt, then the status byte), and
`crate::server::publish_health` sends byte 3 alongside the death notification while
the mob sim's `take_entity_animations` drain sends it for a mob. The camera damage
tilt fires from the same landing, through `encode_hurt_animation`.

**One remainder on the mob side, and it is a *retention* problem rather than a wire
one.** `MobSim::reap_dead` removes a corpse the same tick it dies, so a mob's
`REMOVE_ENTITIES` lands about one tick after its `entity_event`, and the tip-over is
cut short. Vanilla holds the body for 20 ticks (`LivingEntity.tickDeath` removes at
`deathTime == 20`), which is exactly the window the rotation curve above saturates
inside. Closing it means keeping dead mobs in `self.mobs` with AI off for 20 ticks —
which touches every gate that asserts a mob count drops on a kill — or holding the
removal back in `EntityStreamer`. The **player's** own death animation is unaffected:
a player entity is never removed. The mob hurt flash is unaffected too, since the mob
stays alive.

**Still not wired, and why.**
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

**Finding 2 had a third branch, and it took a second pass to see it.** The two
gaps above are the entity and no-target branches; the **block** branch still
`return`ed after its `UseItemOn` + `SwingArm` pair. So the shield and the bow
were fixed for a crosshair on a mob or on open air, and still dead the moment
the crosshair rested on **terrain behind** the mob — which in a real fight is
most of the screen. Eating, drinking and equip-on-use shared the gate for the
same reason, and so did boat placement (`docs/boat-placement.md`).

The correction that made the fix non-obvious: **`case BLOCK` is not a `break`
like `case ENTITY`'s.** Reading `Minecraft.startUseItem` again, `case BLOCK`
`return`s on `InteractionResult.Success` *and* on `InteractionResult.Fail`, and
falls out of the switch to `gameMode.useItem` only for a non-consuming result —
there is no `break` in that arm at all. So the fall-through is conditional:
`use_item_live` sends the generic use only when its `UseOnDecision` is `Nothing`
**and** the held item is not a placeable block, which is the shell's stand-in
for `MultiPlayerGameMode.performUseItemOn` answering `PASS` because the item has
no `useOn` of its own. An unconditional call would equip a **carved pumpkin**
(both a placeable block and `equippable`) onto the player's head whenever a
placement was refused. Both directions are gated in `sim::tests`, and each arm
fails under the other's neuter.

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

**A second false belief, this time in the live gate itself** —
`tests/live_use_item_release.rs`'s entity-target variant. Its first version
polled "does a `minecraft:arrow` entity still exist near the player" after
release, and that poll never once observed one, reading as a fix that still
did not reach the server. It was wrong about what to measure, not about the
fix: `AbstractArrow.onHitEntity`
(`.cache/mc/26.2/src/…/AbstractArrow.java:503-505`) `discard()`s a
non-piercing arrow the instant it damages something, and the test's pig sits
~2 blocks away — well under one tick of flight at a fully-drawn bow's
velocity, so the arrow is gone before the very first poll iteration runs. Two
things proved the fix itself was already correct while this was being
misread: the server granted the "Take Aim" advancement at the exact tick
`ReleaseUseItem` was sent, and the isolated no-entity variant (aim at open
sky, nothing for the arrow to hit) detected the arrow immediately, every
run. The gate now asserts the *persistent* effect of a landed hit — the
pig's health, read via `/data get entity <selector> Health` before and after
— which is the "measure by location/what actually changed, not by a proxy
that happens to correlate everywhere except the case under test" mistake
`CLAUDE.md`'s magnitude/world test-species entries already name, just with
"arrow existence" standing in for the wrong proxy this time.

### The drop key (`Q`)

Two islands, one binding, closing #16/#27. Both were fully built and tested
before this landed, with zero producers:

- **`ClientAction::DropSelectedItem`/`DropSelectedItemStack`** — encoded by
  all four protocol adapters (`PLAYER_ACTION` action ids `4`/`3`), round-trip
  tested, never constructed anywhere in `lodestone-shell`.
- **`Click::drop_one`/`drop_stack`/`do_throw`** (`lodestone-game`, issue #27)
  — `ContainerInput::Throw` only ever reached `OUTSIDE_SLOT` in practice,
  where `doClick`'s own `slot_index >= 0` guard drops it, so the
  slot-drop branch could never execute. **`MenuInput::key_pressed`'s `Drop`
  arm already existed** (`crates/lodestone-shell/src/container.rs`, landed in
  `3ccbbb1` concurrently with the research that found this gap) — it was not
  the missing piece by the time this landed, only its one production caller
  was. Worth recording because the research this pass started from said
  otherwise; re-verify a "missing producer" claim against the current tree
  before assuming which hop is actually missing.

**`InputAction::Drop`** (`keybinds.rs`), default `Q` (`Options.java:664`,
GLFW keysym `81`), category `Inventory` — the twelfth vanilla inventory
mapping this table now implements. Vanilla's own gate
(`AbstractContainerScreen.java:495-501`) is `hoveredSlot != null &&
hoveredSlot.hasItem()`, **not** an empty cursor (`doClick` applies that
itself once the click reaches it, `AbstractContainerMenu.java:513`); `Ctrl`
selects drop-stack (button `1`) over drop-one (button `0`), and it is `else
if`, not two independent `if`s — the same three details `PickItem`'s own
handling already gets right in `key_pressed`, transcribed rather than
re-derived.

**Same two-mechanism shape as `key.swapOffhand`** (see that section above),
and the two contexts ask their checks in the vanilla order:

| context | mechanism | our route |
|---|---|---|
| container open, slot hovered | `ContainerInput::Throw`, button `0`/`1` (`AbstractContainerScreen.java:495-501`) | `KeyOutcome::ContainerDrop { ctrl }` → `MenuInput::key_pressed` → `Click::drop_one`/`drop_stack` |
| no screen, normal play | bare `PLAYER_ACTION`/`DROP_ITEM`\|`DROP_ALL_ITEMS` (`Minecraft.java:1907-1911`) | `KeyOutcome::Drop { ctrl }` → `ClientAction::DropSelectedItem`/`DropSelectedItemStack` |

`resolve_key` gained a fifth parameter, `ctrl: bool`, per this task's own
brief rather than reading the modifier at the driver's `match` arm: the
threading was **not** invasive — one new parameter, one new tracked field
(`WindowApp::ctrl_held`, mirroring the pre-existing `shift_held`), and every
existing call site took a mechanical `, false` (or `, true` for the two new
tests that need it). No call site needed restructuring.

`send_container_drop` goes through `MenuInput::key_pressed` rather than
building a `Click` directly the way `send_container_swap` does, because
`key_pressed` already carries the `hoveredSlot.hasItem()` guard —
duplicating it would be a second copy that can drift.
`send_drop_selected`/`drop_selected_action` mirror `send_offhand_swap`/
`offhand_swap_action`'s shape exactly, including the one guard
(`!player.isSpectator()`, `Minecraft.java:1908`).

**Live gate.** The item-count drop (5 → 4) this task's brief cites as already
proven is `crates/lodestone-game/tests/live_inventory.rs`, which sends
`ClientAction::DropSelectedItem` through `ClientHandle::send_action` directly
— it proves the wire format and the server's acceptance of the action, not
this pass's own contribution (the `app.rs` dispatch that reaches that call
from a real key press). This pass added the dispatch and its hermetic
`app.rs` tests (`q_drops_one_while_playing_and_ctrl_q_drops_the_stack`,
`q_issues_a_container_drop_while_a_container_is_open`, and the wire/spectator
pair mirroring the off-hand key's own tests); it did not re-run the live
oracle, since the wire format and server acceptance were already proven and
nothing about that half changed.

### Crit particles

Vanilla's `criticalAttack = fullStrengthAttack && canCriticalAttack(entity)`
(`Player.java:970-971,1032-1041`), whose visual half is
`attackVisualEffects`' `this.crit(entity)` call
(`Player.java:1063-1066` → `LocalPlayer.crit`, `LocalPlayer.java:664-665`).

**This is real vanilla dual simulation, not an invented approximation.**
`MultiPlayerGameMode.attack` runs the client's own copy of
`player.attack(entity)` (`MultiPlayerGameMode.java:428`) independently of,
and before, the server's authoritative copy of the same method — the server
computes the real damage, the client predicts only the cosmetic sound and
particle trigger so the swing has feedback without a round trip. The wire
`Attack` packet carries no damage or crit flag either way (see above); this
prediction cannot disagree with the server about anything that matters.

`Sim::maybe_spawn_crit_particles`, called from `Sim::attack_entity` **before**
the attack-strength ticker resets — vanilla's own order
(`MultiPlayerGameMode.attack`: send, then `player.attack(entity)`, then
`resetAttackStrengthTicker()`); reading the ticker after the reset would make
`fullStrengthAttack` false on every attack, including the one that just
landed at full charge, so this is a correctness-load-bearing ordering, not a
style choice.

**Condition, checked against the jar:**
`fallDistance > 0.0 && !onGround && !onClimbable && !isInWater &&
!isMobilityRestricted && !isPassenger && entity is LivingEntity &&
!isSprinting`, gated by the caller's own `fullStrengthAttack =
getAttackStrengthScale(0.5F) > 0.9F` — note the `0.5F` partial-tick argument,
**not** the crosshair indicator's `0.0F` (`Hud.java:448`). `Sim::
attack_strength_scale` was refactored into a private `attack_strength_scale_at(a)`
so both call sites share the ticker read and delay computation instead of
duplicating it; the public accessor is now a one-line call with `a = 0.0`,
observably unchanged.

Two clauses are not modelled, both disclosed rather than silent:

- **`!onClimbable`** is not read separately. This engine already resets
  `fall_distance` to `0.0` the instant `tick_air` finds a climbable
  (`LivingEntity.handleOnClimbable`, folded into `tick_air` — see
  `lodestone_physics::player::PlayerState::fall_distance`'s own "Climbable
  reset" bullet), so `fall_distance > 0.0` already implies not-on-climbable
  in this port's physics model. Derived from that source, not guessed.
- **`!isMobilityRestricted`/`!isPassenger`**, and the outer `baseDamage >
  0.0F || magicBoost > 0.0F` gate, are not modelled — this shell has no
  riding state and no local weapon-damage/enchantment computation to derive
  `baseDamage`/`magicBoost` from (the identical gap `attack_strength_delay`'s
  own doc names for `lodestone-data` carrying no per-item attack-speed
  census). The only divergence this can produce is a crit particle on an
  attack that deals zero base damage, which vanilla itself already treats as
  "nothing happens" one level up — cosmetic only, no damage number depends on
  it.

**The burst is one tick of `TrackingEmitter`, not three.** Vanilla's
`TrackingEmitter` (`TrackingEmitter.java:29-41`) runs for 3 ticks, spawning
up to 16 candidates per tick (filtered to a unit sphere, ~52% pass) that
track the entity's *current* position each tick. This shell's particle
system has no per-attack persistent emitter — every existing local spawn
(`Particles::destroy_block`/`breaking_block`) is a one-shot burst — so
`maybe_spawn_crit_particles` spawns one tick's worth (16 candidates, the same
unit-sphere filter and the same `Entity.getX(double)`-style position formula,
`Entity.java:3770-3811`) at the target's position at the moment of the
attack, rather than adding new multi-tick emitter machinery for a purely
cosmetic burst. The per-candidate physics
(`lodestone_particle::emit::crit`) is exact; only the tick count is a
disclosed simplification. Target-entity resolution goes through
`EntityIndex` (server id → ECS entity) and the `LivingEntity` check through
`lodestone_data::entity_types::entity_type_id_parts` +
`entity_census::is_living` — the same census `docs/backlog.md`/`CLAUDE.md`
already document for the metadata-index-8/15 collisions, reused here rather
than re-derived.

**The gate** is five hermetic `sim.rs` tests reached only through
`begin_attack_live` (the real production entry point, not the private
helper called directly): a positive case (full strength, airborne, not
sprinting, not grounded, living target) and four negative controls, each
run and watched to actually distinguish (grounded, sprinting, non-living
target, below full strength) — all against a particle-count delta from
`Particles::engine_mut().particles().len()`, the same instrument the file's
pre-existing particle tests already use.

### The sweep-attack particle

**Built.** Split out of #12 into its own issue (#409) rather
than left buried in a mostly-closed one, since — per the two prior passes
recorded above — it was the one genuine remainder and its whole rendering
path was unbuilt, not merely unwired. Landed in `lodestone-particle`
(`crates/lodestone-particle/src/{lib,emit}.rs`) and
`crates/lodestone-shell/src/particles.rs`, the two files this doc's own
"out of scope" note named as the blocker: a new `Sheet::SweepAttack` variant
(`particle/sweep_0`…`sweep_7`, confirmed against the real files under
`.cache/mc/26.2/client-src/assets/minecraft/textures/particle/`), a new
`Behaviour::SweepAttack` with its own full-tick override (`Particle::
tick_sweep_attack` — `AttackSweepParticle.tick()` never calls `move()` at
all; the quad is stationary for its whole 4-tick life), `emit::sweep_attack`,
and one `"sweep_attack"` arm in `Particles::spawn_one`.

No `sim.rs`/`app.rs`/`net.rs` change was needed, and the reason is worth
recording: vanilla's own trigger
(`.cache/mc/26.2/src/net/minecraft/world/entity/player/Player.java:1191`,
`serverLevel.sendParticles(ParticleTypes.SWEEP_ATTACK, ...)`) is an ordinary
`LEVEL_PARTICLES` broadcast, and that packet already forwards through
`ClientEvent::Particles` → `NetUpdate::Particles` → `Particles::
spawn_particles` → `spawn_one` generically (`crates/lodestone-shell/src/
net.rs:1466-1478`, `sim.rs::Sim::poll_net`) — the same path `"flame"`/`"crit"`/etc.
already used. Adding the dispatch arm was the entire wiring job; a
`/particle minecraft:sweep_attack` command or any server broadcasting the
type reaches pixels immediately.

One disclosed vanilla quirk, verified directly against the jar rather than
assumed: `AttackSweepParticle`'s constructor takes a `size` parameter
(`quadSize = 1.0F - (float) size * 0.5F`), but the one real call site above
sends `count == 0` with `maxSpeed == 0.0F`, and
`ClientPacketListener.handleParticleEvent`'s `count == 0` branch computes
`xAux = maxSpeed * xDist` — so the value that actually reaches the
constructor in real play is always `0.0`, regardless of swing direction,
making `quadSize` always exactly `1.0` in practice. `emit::sweep_attack`
still takes `size` as a real parameter (not hardcoded) so it stays a
faithful transcription for any future caller that passes something else.

**Verification.** `crates/lodestone-particle/src/emit.rs`'s
`sweep_attack_has_the_exact_vanilla_lifetime_and_colour_range` and
`sweep_attack_dies_on_exactly_the_fifth_tick` assert the exact lifetime (`4`,
hardcoded, not a range), the exact `quadSize` (`1.0`, derived from the
`size == 0.0` finding above, not merely "some size"), and the precise
post-increment removal tick (alive through tick 4, removed on tick 5 —
pinning the off-by-one a naive `age >= lifetime` check before incrementing
would get wrong). `crates/lodestone-shell/src/particles.rs`'s
`sheet_particle_resolves_against_the_real_particle_atlas` (an `#[ignore]`d
gate, run against the real `.cache/mc/26.2/client.jar`) confirms
`Sheet::SweepAttack`'s `"sweep"` stem actually resolves against the jar's
real `sweep_0.png`…`sweep_7.png`, not merely a plausible-looking guess; measured
`unresolved: 0` for the sweep instance alongside the rest of this same
pass's batch (see [`docs/particle-catalogue.md`](./particle-catalogue.md)).
No live-server oracle capture was taken for this one (the formula above was
derived from the decompiled source and the jar assets directly, which
CLAUDE.md ranks above a wiki or hand transcription); a future pass could add
one by swinging at a mob on the creative oracle and checking the packet
bytes.

### `bobHurt`, still blocked

Re-confirmed, not re-derived: `camera_rig.rs`'s own `bobbed_camera` doc
comment already has the precise cost, and reading it against the current
tree shows it is not stale. `BobFrame::eye_transform`/`hurt_roll_degrees`
compute the correct roll matrix, but `bobbed_camera` folds the result back
into a `Camera` by decomposing a view matrix into `position`/`yaw`/`pitch` —
two rotational degrees of freedom recovered from three, because
`Camera::view_matrix` hardcodes `Vec3::Y` as up. The pure-roll component of
`bobHurt`'s tilt is the one thing that decomposition structurally cannot
carry (the table in `bobbed_camera`'s own doc: walk-bob roll and the
hurt-tilt roll both land in the "not carried" row).

Giving `Camera` a real roll field (or an equivalent `Mat4` hook on
`view_projection`) is not a local change: `bobbed_camera`'s doc counts 48
`Camera { .. }` struct literals across ~40 files, six inside
`crates/lodestone-shell/src/gpu.rs` and one in
`crates/lodestone-render/src/entity.rs`. Those are three other agents'
exclusive territory at once for this task
(`crates/lodestone-render/`/sky-cloud agent, `gpu.rs`/`gpu/*.rs`/sign agent,
`entity.rs`/creeper agent) — a change too large and too cross-cutting to land
inside this pass, and one that needs the orchestrator to sequence rather than
a single agent picking it up mid-flight. `ViewBob::hurt`/
`BobFrame::hurt_roll_degrees` stay exactly where they were: implemented,
unit-tested, called only by their own tests.

### The integrated-server melee-damage gap: player attacks now deal damage (issue #12)

Everything above this section is client-side: swinging, sending the
`Attack`/`Interact` packet, taking server-sent knockback. This repo also
hosts its own **server** (`lodestone-server`, singleplayer and open-to-LAN),
and until this pass, punching a mob there did nothing — `ServerBound` had no
`Attack` variant at all, so the packet was never decoded, `SimMob::apply_damage`
was reached only by AI-driven `MeleeAttackGoal` hits and explosions, and
`lodestone-physics/src/knockback.rs`'s `knockback_impulse`/`attack_direction`
had zero callers anywhere. Two prior passes (recorded on issue #12) found and
scoped this precisely, and named the real blocker: *"there is no way to reach
a live mob's health from a connection's own task"* — `MobSim` was ticked
entirely inside its own background task with no shared, lockable handle, the
way `BlockEntityHandle` already gives block entities.

**What this pass builds, in wire order:**

```text
ClientAction::InteractEntity { interaction: Attack, .. }   (already sent in production,
  -> Sim::attack_entity, lodestone-shell — see "Sending the attack" above)
  -> minecraft:attack (v770 ATTACK packet id 1)
  -> V770ServerProtocol::decode -> ServerBound::Attack { entity_id }
  -> dispatch_play_packet -> crate::server::apply_attack
  -> MobHandle::with(|sim| sim.attack(..))
     -> SimMob::apply_damage (pre-existing, real, jar-verified — HurtCooldown + apply_reductions)
     -> lodestone_physics::knockback::knockback_impulse (pre-existing, real, jar-verified)
     -> NavigatingMob::apply_knockback (new: one-tick position displacement)
  -> (no reply packet — the real wire shape) the *existing* EntityStreamer::sync,
     called on every inbound packet, carries the resulting position/health to
     every connection tracking the mob
```

**`MobHandle`** (`crates/lodestone-server/src/mobs/mod.rs`) is the missing
mutation handle, the exact `BlockEntityHandle` pattern
(`Arc<Mutex<_>>` + a single funnel method, `with`) reused rather than
reinvented. The one real wrinkle `BlockEntityRegistry` did not have:
`MobSim<'w>` *borrows* its `ChunkWorld`, but a handle shared with a
separately-`tokio::spawn`ed connection task must be `'static`.
`MobHandle::new` resolves this with `Box::leak` — the `ChunkWorld` a caller
hands in is leaked once, for the process's remaining lifetime, rather than
borrowed for one task's own stack frame the way the pre-handle
`run_mob_tick_loop` did. This is a **deliberate, bounded** leak: that
function's own doc comment already discloses its `ChunkWorld` snapshot is
static for the sim's whole lifetime (a fixed area around the mob-spawn
center, never widened) — leaking only changes *whose* lifetime "static" is
measured against, for the one `MobSim` a running `IntegratedServer`
constructs per call to `open_in_memory_with_mobs`. `MobHandle::seeded` is
the direct replacement for what `run_mob_tick_loop` used to do at its own
top (`set_next_id(1000)` + `seed_demo_mobs`); `run_mob_tick_loop` itself now
just ticks and republishes a handle it is handed, rather than owning the
sim outright. `IntegratedServer::open_in_memory_with_mobs` builds the
handle **synchronously**, before either task spawns, and clones it into
both — the connection task mutates it on an `Attack` packet, the tick-loop
task ticks and republishes it, both against the identical `MobSim`.
`MobHandle` also implements `EntitySource` directly (`self.with(MobSim::snapshots)`),
useful for a caller (or a test) that mutates the sim itself and does not
need a separate ticking population.

`ServerBound::Attack { entity_id }` decodes `minecraft:attack` (a 26.2-only
split of the old combined interact packet — wire body is just a VarInt
entity id, no hand/location/secondary-action bit, matching
`ServerboundAttackPacket`'s real record). `ServerBound::PlayerInput { sprint }`
decodes `minecraft:player_input`'s single flags byte, reading only bit
`0x40` — the other six movement flags are decoded off the wire (so a
malformed byte still fails cleanly) but dropped, the same "decode what the
loop needs" convention `PlayerMoved`'s own two fields already establish.

**`minecraft:interact` (a plain right-click, not `Attack`) deliberately gets
no `ServerBound` variant at all.** This crate has no interaction model for
anything it would carry — taming, feeding, mounting — so adding a decode-only
variant with nothing to consume it would be exactly the manufactured island
CLAUDE.md warns against (the same call the previous scoping pass on this
issue already made, for the same reason). It decodes to `Ignored` via the
wildcard arm, pinned by `decode_plain_interact_from_the_real_client_encoder_is_ignored`
in `server_protocol.rs` so a future agent adding taming/feeding changes that
test deliberately rather than discovering the gap by accident.

**Damage.** `PLAYER_BARE_HAND_ATTACK_DAMAGE = 1.0` — `Player.createAttributes()`'s
own `.add(Attributes.ATTACK_DAMAGE, 1.0)` (`Player.java:208`), **not**
`LivingEntity`'s generic `2.0` a player would otherwise inherit. This crate
has no item/weapon-attribute model for the player (`lodestone_entity::damage`'s
own module doc already names this gap for issue #261) and no server-tracked
attack-strength ticker (`Player.attack`'s `baseDamageScaleFactor()`,
cooldown-scaled damage, is client-cosmetic-prediction-only here — see the
crit-particle section above), so every hit is the flat constant, full
strength, no crit, no weapon bonus — all pre-existing, disclosed gaps, not
new ones this pass introduces.

**Knockback.** `lodestone_physics::knockback::knockback_impulse` needs a
horizontal push *direction* — real vanilla uses the **attacker's facing**
(`attack_direction(yaw)`), which nothing server-side tracked when this was
written. Issue #262 has since changed that (`PlayerRegistry::set_rotation`,
fed by all four movement packets), so the blocker is now only that
`apply_attack` has not been switched over. `apply_attack` still uses the
horizontal vector from the attacker's last known position to the target
instead. This is a smaller divergence than it sounds: a melee attack
requires the crosshair to already be on the target, so facing and
attacker→target are nearly always the same vector in practice — and it is
literally the only vector this crate has. `NavigatingMob` has no
persistent-velocity/drag model to blend an impulse into (it is "kinematic...
not the physics integrator" — every tick recomputes fresh from path
following), so `apply_knockback` applies the impulse as an immediate
one-tick position displacement rather than an ongoing velocity that would
need new decay machinery this composition was never built to carry — the
same "disclosed one-shot simplification" trade the crit-particle burst
above already makes (one tick's worth instead of vanilla's three-tick
`TrackingEmitter`).

**Knockback power, and why it is genuinely `0.0` for the common case.**
`Player.attack`'s real formula is `getKnockback(...) + (isSprinting() &&
fullStrengthAttack ? 0.5F : 0.0F)`. `getKnockback` resolves to the attacker's
own `minecraft:attack_knockback` attribute (registry default `0.0`,
`Attributes.java:18`) — zero for a bare hand, since this crate has no
weapon/enchantment model to add to it. So a **non-sprinting** attack's total
knockback power is exactly `0.0` — not a placeholder, the literal jar
formula for the one case this crate can model. `sprinting` *is* tracked
(`ServerBound::PlayerInput`, one new bool of state per connection), so a
**sprinting** attack correctly applies `SPRINT_ATTACK_KNOCKBACK_POWER = 0.5`
— `fullStrengthAttack` is assumed true throughout (no cooldown ticker
server-side, same disclosed gap as damage above). `combat_live.rs`'s live
gate (below) exercises the sprinting, nonzero-power case specifically so the
primitive's wiring is provably real, not just correctly inert.

**Mob-on-player damage: an entry point, not a live trigger.**
`PlayerVitals::apply_damage` (`crates/lodestone-server/src/vitals.rs`) is now
a real, unit-tested, jar-verified generic damage entry point — the
`HurtCooldown` + `apply_reductions` pipeline `SimMob::apply_damage` already
runs for a mob, given to the player for the first time (previously only
`tick`/drowning and `apply_fall_damage` existed, neither gated by an
i-frame). **Nothing calls it in production.** Making a mob actually attack
the connected player needs player-position-aware targeting AI that does not
exist anywhere in this workspace: `crate::mobs`'s own module doc already
scopes real player-targeting (`NearestAttackableTargetGoal`'s population
search) as a separate, larger feature, and `run_mob_tick_loop` has no
player-position feed into the sim at all — `MobSim::despawn_pass`'s own "no
despawn pass" scope note names the identical missing input. Disclosed here
rather than silently left unfindable, the same shape `ViewBob::hurt`/`bobHurt`
is tracked in elsewhere in this doc: real, tested, a documented reason
nothing calls it yet.

**Shield blocking: confirmed, again, out of scope.** `LivingEntity.applyItemBlocking`
(`:1308-1345`) is a separate angle-gated reduction keyed off the held item's
`BlocksAttacks` data component — this workspace has no item-data model for
it anywhere (the same prerequisite gap issue #261 already names for
per-item armour). Not started.

**The hurt flash for a damaged-but-alive mob now reaches the client** —
`ServerProtocol::encode_hurt_animation`, sent from the mob sim's
`take_entity_animations` drain for every hit that landed (the same `applied > 0.0`
guard vanilla's `tookFullDamage` is). It carries yaw `0.0`, which is vanilla's own
value for anything that is not a player: `LivingEntity.getHurtDir` is a constant and
only `ServerPlayer` overrides it.

**`encode_damage_event` is still absent, and the route is therefore a disclosed
substitution.** Vanilla broadcasts `ClientboundDamageEventPacket` for a mob and
reserves `hurt_animation` for the hurt player's own connection
(`ServerPlayer.indicateDamage`). We send `hurt_animation` in both cases because our
client folds either into the same `HurtTime`, and because `damage_event` additionally
needs a `minecraft:damage_type` registry id per source. The pixels are the same; the
packet is not the one a vanilla client would have received. The paragraph below is
kept as the record of what used to be missing (it is genuinely new server-side wire
work, not "wiring
existing pieces").

**Verification.**

- Hermetic: `crates/lodestone-server/tests/mob_attack.rs` (8 tests) drives
  `MobSim::attack` through the crate's public API — exact predicted health
  after the live-verified diamond-armour reduction (`3.0` from a raw `10.0`
  hit, the same RCON-verified number `damage.rs`'s own test cites), exact
  predicted knockback velocity (`(-0.5, 0.4, 0.0)`, hand-derived from the
  jar formula and cross-checked against calling `knockback_impulse`
  directly), a full-resistance no-op control, an i-frame-ignored-follow-up
  control, and immediate death removal. `crates/lodestone-server/src/vitals.rs`
  gains matching tests for `apply_damage`.
- Decode: `server_protocol.rs`'s `combat_decode_tests` module (6 tests) —
  `Attack`/`PlayerInput` round-tripped through the **real client encoder**
  (`crate::adapter().encode_action(..)`), not a hand-built wire body, plus
  the plain-`Interact`-is-`Ignored` pin and two truncated-payload controls.
- **Live, real client**: `crates/protocol/v770/tests/combat_live.rs`.
  `real_client_attacks_a_live_mob_and_the_server_applies_damage_and_knockback`
  drives a real `lodestone-client` through `serve_connection` directly
  (the same "hold my own `MobHandle` clone, `IntegratedServer` builds one
  internally with no accessor" reasoning `block_entities_live.rs` already
  established for `BlockEntityHandle`) against a zombie spawned 1 block from
  the attacker, sprinting. It asserts the server's own `MobHandle` reaches
  the exact predicted health (`max_health - 0.94`, the zombie-armour
  reduction hand-derived from the jar in the test's own doc comment) **and**
  that the real client's own read model (`ClientHandle::entity`)
  independently converges on the exact predicted post-knockback position
  (`(0.5, 64.4, 0.0)`) — proving the result reaches a real client over the
  real wire, not just server-internal state. `no_attack_means_no_movement`
  is the negative control: ordinary connection traffic (movement, chat)
  with no `Attack` packet must never move or damage the mob. Both run
  hermetically (in-memory transport, no real sockets or wall-clock waits)
  and are **not** `#[ignore]`d.

## What is deliberately not built here

**`bobHurt`'s production wiring** — see "`bobHurt`, still blocked" above.
Blocked on `Camera` gaining a roll degree of freedom, a change spanning three
other agents' exclusive territory for this task.

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

**Mob-on-player autonomous damage** — see "Mob-on-player damage: an entry
point, not a live trigger" above. `PlayerVitals::apply_damage` exists,
tested, jar-verified; nothing calls it, because no AI in this workspace
gives a mob the player's position to target. Needs player-position-aware
mob AI, a materially larger feature than this pass's "reach a live mob's
health" scope.

**Shield blocking** — confirmed out of scope again this pass; see that
section above. Needs an item-data model (`BlocksAttacks`) this workspace
does not have anywhere, the same prerequisite class as issue #261's armour
feed.

**A server-side hurt-flash cue for a damaged-but-alive mob** — **landed.**
`ServerProtocol::encode_hurt_animation` plus `MobSim::take_entity_animations`. What
remains is `encode_damage_event` (the *route* vanilla uses for a mob, which needs a
`minecraft:damage_type` registry id) and the 20-tick corpse retention that makes the
death tip-over visible rather than one tick long.

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
- `crates/lodestone-shell/src/keybinds.rs`'s `InputAction::Drop` default —
  `KeyCode::KeyQ`, vanilla's `key.drop` (`Options.java:664`, GLFW `81`).
- `Sim::maybe_spawn_crit_particles`'s per-tick candidate count — `16`,
  vanilla's `TrackingEmitter.tick()` loop bound (`TrackingEmitter.java:29`).
  Not configurable; see "Crit particles" above for why this is one tick's
  worth rather than `TrackingEmitter`'s real three.
- `crates/lodestone-server/src/server.rs::PLAYER_BARE_HAND_ATTACK_DAMAGE` —
  `1.0`, `Player.createAttributes()`'s bare-hand `ATTACK_DAMAGE`
  (`Player.java:208`). Not weapon-aware; see "Damage" above.
- `crates/lodestone-server/src/server.rs::SPRINT_ATTACK_KNOCKBACK_POWER` —
  `0.5`, `Player.attack`'s `knockbackAttack` bonus (`Player.java:963-966,
  987-988`). A non-sprinting attack's power is exactly `0.0` (the attacker's
  `attack_knockback` attribute default, no placeholder) — see "Knockback
  power" above.

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
- `lodestone_ecs::entity::EntityIndex` — server entity id → ECS `Entity`,
  used by `maybe_spawn_crit_particles` to resolve the attack target's
  `Position`/`EntityKind`.
- `lodestone_data::entity_types::entity_type_id_parts` +
  `lodestone_data::entity_census::is_living` — the `LivingEntity` check for
  the crit condition's target clause; the same per-type census
  `docs/backlog.md`'s metadata-index-8/15 collision notes and
  `crates/protocol/v770/src/adapter.rs`'s `TrackedEntity` already depend on,
  reused rather than re-derived.
- `lodestone_particle::emit::crit` and `Particles::engine_mut`/
  `ParticleEngine::rng` — the crit particle's own physics and the RNG draws
  `maybe_spawn_crit_particles` uses for its 16-candidate scatter. Both
  pre-existing; this is their first local-prediction caller from combat.
- `crate::container::{MenuInput, MenuKey, MenuContext}` (imported as
  `ContainerMenuKey` in `app.rs` to avoid colliding with `menu::nav::MenuKey`)
  — the drop key's container-open route. `MenuKey::Drop`/`key_pressed`'s
  handling of it were already built in `container.rs`; this pass added the
  one caller.
- `lodestone_model::ClientAction::{DropSelectedItem, DropSelectedItemStack}`
  — the drop key's gameplay route, encoded by all four protocol adapters
  already.
- `lodestone_server::{MobHandle, AttackOutcome, ServerBound::{Attack,
  PlayerInput}}` — the server-side wiring this pass adds (issue #12).
  `MobHandle` is the shared mutation handle onto the live `MobSim`; see "The
  integrated-server melee-damage gap" above for the full design (why it
  leaks its `ChunkWorld`, why it also implements `EntitySource`).
- `lodestone_physics::knockback::{knockback_impulse, attack_direction}` — now
  has a real caller (`MobSim::attack`). `lodestone-server`'s `Cargo.toml`
  gained a `lodestone-physics` dependency for this; zero cycle risk, since
  that crate depends on nothing.
- `lodestone_entity::ai::navigating_mob::NavigatingMob::apply_knockback` —
  new: the one-tick position-displacement mechanic knockback needed, absent
  before this pass because nothing had ever applied an external impulse to a
  `NavigatingMob`.
- `lodestone_entity::{apply_reductions, HurtCooldown, Defenses, DamageFlags}`
  — now also used by `PlayerVitals::apply_damage`
  (`crates/lodestone-server/src/vitals.rs`), the identical pipeline
  `SimMob::apply_damage` already ran, given to the player.

## How to change it

- The sweep-attack particle is built — see "The sweep-attack particle" above.
  Correcting one thing this bullet used to claim: `AttackSweepParticle`
  overrides neither `getFacingCameraMode()` nor `roll`, so — verified directly
  against `.cache/mc/26.2/client-src/net/minecraft/client/particle/
  AttackSweepParticle.java` rather than assumed — it is an ordinary
  camera-facing billboard like every other particle in this crate, *not*
  oriented by swing direction. To extend it (a bigger sweep for a bigger
  weapon, say): `emit::sweep_attack`'s `size` parameter already threads
  through to `quadSize`; only the caller — wherever swing detection lands in
  `sim.rs`, outside this crate's scope — would need building.
- Adding sweep/crit *sound*: both are ordinary server-broadcast sounds
  (`Player.java:965,1064`, `playServerSideSound`) — already covered by the
  generic sound pipeline (`docs/sound-playback.md`), no client work needed,
  confirmed under "Already checked and confirmed correct" in the scoping
  pass this section descends from.
- Wiring `bobHurt`: give `Camera` a roll degree of freedom (or an equivalent
  `Mat4` hook on `view_projection`) first — see "`bobHurt`, still blocked"
  above for the exact cost and why it spans three other agents' files. Once
  that lands, `Sim::render_camera`'s hardcoded `damage_tilt_strength = 0.0`
  becomes a real value driven by the local player's own `HurtTime`/
  `EntityHurtAnimation` yaw, and `ViewBob::hurt` already has everything else
  it needs.
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
- **Getting a real attacker-facing knockback direction** (instead of the
  attacker→target stand-in `apply_attack` uses): **the yaw is now available.**
  Issue #262 added exactly the `player_rot: Option<Rotation>` this entry used
  to propose, tracked alongside `player_pos` in `serve_play` and fed by all
  four movement packets, so the decode-side work this entry warned about is
  done. What remains is the swap itself: pass that yaw into
  `lodestone_physics::knockback::attack_direction` in `apply_attack` in place
  of the position-delta vector. Note `apply_attack` currently takes
  `player_pos` by value and would need the rotation threaded the same way.
- **Wiring the cooldown-scaled damage/crit-bonus formula server-side**:
  needs an attack-strength ticker tracked *server*-side (today it is
  client-only, for the cosmetic crosshair indicator) plus a weapon/item
  model to derive `baseDamage`/critical eligibility from. Both are named,
  disclosed gaps in `lodestone_entity::damage`'s own module doc (issue
  #261); `PLAYER_BARE_HAND_ATTACK_DAMAGE`'s own doc comment names the exact
  same blocker.
- **The sweep arc's own multi-entity damage loop is a distinct, still-fully
  unbuilt mechanic — not merely "sweep does bonus damage to the one target
  already hit".** Re-checked directly against the jar rather than assumed,
  because every existing mention of "sweep" in this doc (and in issue #12's
  own history) is about the *particle*: vanilla's `Player.doSweepAttack`
  (`.cache/mc/26.2/src/net/minecraft/world/entity/player/Player.java:1163-1189`)
  is called only when `isSweepAttack` holds (`:1043-1052` — full-strength,
  not a crit, not a knockback-bonus hit, attacker on ground, attacker's
  recent horizontal speed under `getSpeed() * 2.5`, main-hand item tagged
  `#minecraft:swords`) and then loops
  `level().getEntitiesOfClass(LivingEntity.class,
  entity.getBoundingBox().inflate(1.0, 0.25, 1.0))` — i.e. every living
  entity in a box around the *original target*, not the attacker — and
  damages **each one** (excluding the attacker, the original target, allies,
  and marker armour stands, plus a `distanceToSqr(nearby) < 9.0` clamp) with
  `1.0 + sweeping_damage_ratio_attribute * baseDamage` (`Attributes.
  SWEEPING_DAMAGE_RATIO`) scaled by `attackStrengthScale`, applying its own
  separate `0.4F`-power knockback to
  every entity it hits. This is a real, structurally separate hit-multiple-
  targets-in-one-swing mechanic, and `crates/lodestone-server/src/mobs/`
  has no code resembling it at all (confirmed by grep: zero hits for
  `sweep`/`Sweep` anywhere combat-related in that crate, across the
  directory the `mobs.rs` file split turned it into). It needs the same
  attack-strength-ticker-server-side and sword-item-tag prerequisites the
  bullet above already names, so it belongs under #261 rather than as new
  scope here — recorded explicitly because the existing #261 body's
  "critical-hit/sweep-attack bonus damage" phrase reads as a damage-number
  tweak and could understate that the actual missing piece is an
  entity-query loop with its own knockback, not a multiplier.
- **Wiring mob-on-player damage for real** (not just the entry point):
  give `run_mob_tick_loop`/`MobSim` a live feed of the connected player's
  position (the same missing input `MobSim::despawn_pass`'s own "no despawn
  pass" scope note names), then a target-acquisition goal
  (`NearestAttackableTargetGoal`-equivalent) for hostile species so a
  `MeleeAttackGoal` connecting against the player calls
  `PlayerVitals::apply_damage` instead of (or alongside) another `SimMob`.
  `crate::mobs`'s own module doc already flags this as future, separate
  work — not a quick follow-up to this pass.
- **Adding `encode_damage_event`** (the one encoder of this family still missing;
  `encode_hurt_animation` and `encode_entity_event` are landed): a new trait method
  defaulted to `ServerDirective::None` like every other optional encoder — see
  `protocol.rs`'s own doc comment on why a boxed protocol must forward every one —
  implemented in `V770ServerProtocol` against `ClientboundDamageEventPacket.write`,
  and sent where `MobAnimation::Hurt` is drained today. Its extra input over
  `hurt_animation` is a `minecraft:damage_type` registry id per source, which is why
  it was not folded into the same pass. The
  client-side consumer (`ClientEvent::EntityDamaged`/`EntityHurtAnimation`
  → `HurtTime` → the render overlay) already exists end to end — see "The
  per-entity hurt/death red overlay" above — so this is purely a new
  server-side encoder plus one new call site, not a new mechanic.

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
