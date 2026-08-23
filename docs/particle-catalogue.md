# The particle catalogue: what's wired, what isn't, and why

This covers ambient/environmental particle types and combat/event particle types. Both start from the
same measured gap: `crates/protocol/v770/src/generated/particle_types.rs` (registry id →
name) and `crates/lodestone-data/src/generated/particle_types.rs`
(`PARTICLE_TYPE_COUNT: u32 = 125`) decode the full vanilla particle registry and network
dispatch resolves ids correctly, while `Particles::spawn_one`
(`crates/lodestone-shell/src/particles.rs`) matches a growing subset and every other name falls
into the `other => tracing::debug!(...)` catch-all and is silently dropped. It started at six
(`"flame"`, `"smoke"`, `"large_smoke"`, `"crit"`, `"splash"`, `"bubble"`); the "What was built,
per issue" section below is the live inventory — **read that rather than any count in this
paragraph.**

Not every type arrives over the network at all: a large group is spawned client-side from
`Block.animateTick`, and those come from `Particles::ambient_tick` instead. A type can
legitimately have both.

## What it is

`lodestone-particle`'s `Sheet` enum names a physical texture sheet under
`textures/particle/*.png`; `Behaviour` names a per-type tick/quad-size/layer override;
`emit` is one function per vanilla particle *class* (not per registry type — several
registry types share a Java class, see "One class, several registry names" below).
`Particles::spawn_one` (`crates/lodestone-shell/src/particles.rs`) is the one place that maps
a decoded registry name to an emitter call. Adding a type end-to-end means: a `Sheet`
variant naming the real sheet (if one doesn't already exist for it), a `Behaviour` variant
if the tick shape is new, an `emit::` function transcribed from the matching
`net.minecraft.client.particle.*` class, and one `match` arm in `spawn_one`.

## How it works

### The dispatch is reachable independently of any specific gameplay trigger

`spawn_one` is reached from exactly one place: `Particles::spawn_particles`, which is called
generically from `NetUpdate::Particles` in `sim.rs`, which in turn is populated from
`ClientEvent::Particles` in `net.rs` — itself a decode of vanilla's ordinary
`ClientboundLevelParticlesPacket` (`LEVEL_PARTICLES`). That packet is what a `/particle`
command, a datapack, or a plugin sends for **any** registered particle type, so wiring a
type's dispatch arm makes it render correctly the moment such a packet names it —
independently of whether *this specific codebase* also predicts the type's usual in-game
trigger locally. This matters because several of the types below have a real vanilla trigger
that is **not** a network packet at all (see the next section); the dispatch fix still reaches
the screen for the generic path even when the specific-trigger path is unbuilt.

### The trap: several of these particles are not server-sent at all

Checked directly against `.cache/mc/26.2/src/net/minecraft/world/level/Level.java`:
`Level.addParticle(...)`'s **default body is empty** — a genuine no-op. `ClientLevel`
overrides it to spawn a real local `Particle`; `ServerLevel` does **not** override it at all.
So any vanilla gameplay code that calls `this.level().addParticle(...)` (not
`serverLevel.sendParticles(...)`) does nothing when that code runs on the server — the
particle only appears because the *same* method also runs on `ClientLevel` for entities the
client itself ticks (breeding hearts, villager mood icons) or because a different, synced
mechanism (a block-action/`triggerEvent` broadcast, for note blocks) replays the call
client-side. This is the entity-state analogue of the block-state ambient prediction trap
the tracking issue's own body already named.

Confirmed per-type:

| type | real vanilla trigger | network path? |
|---|---|---|
| `sweep_attack` | `Player.doSweepAttack` → `serverLevel.sendParticles(...)` | yes — ordinary packet |
| `note` | `NoteBlock.triggerEvent` → `level.addParticle` (client-replayed via block action) | no — needs a `BLOCK_EVENT` consumer per instrument |
| `heart` | `Animal.aiStep` → `level().addParticle` (runs on both sides; a no-op server-side) | no — needs a per-entity tick predictor keyed off synced `inLove` |
| `angry_villager` / `happy_villager` | villager AI ticks, same shape as `heart` | no |
| `witch` | `Witch`'s drink-tick, same shape as `heart` | no |
| `totem_of_undying` | an entity-status broadcast (`LivingEntity`'s totem consumption), replayed client-side in a loop | no — needs an entity-event consumer |

So this pass built the **dispatch-reachable** half (real, and enough for any server or
plugin's generic particle spawn, or `/particle`), and explicitly did **not** attempt the
per-type production trigger for `note`/`heart`/the villager pair/`witch`/`totem_of_undying` —
each of those needs a consumer in `sim.rs`'s block-action handling or a new per-entity ECS
tick driver (`lodestone-ecs`/`ingest.rs`), both outside `lodestone-particle`'s and
`particles.rs`'s scope for this pass, and one of them (`lodestone-ecs`) was off-limits
entirely (an event route table landing there concurrently). Flagged rather than guessed at.

### One class, several registry names

Vanilla's registration table (`.cache/mc/26.2/client-src/net/minecraft/client/particle/
ParticleResources.java`) reuses Java classes across differently-named, differently-textured
registry types — the sheet a type samples is decided by its own
`assets/minecraft/particles/<name>.json` (a texture list), independent of which class
renders it. Two instances that mattered here:

- `ParticleTypes.EFFECT`/`ENTITY_EFFECT` (`effect_0..7`) and `ParticleTypes.WITCH`/
  `INSTANT_EFFECT` (`spell_0..7`) are **all four** `SpellParticle`-family classes, but name
  two different physical sheets. `Sheet::Effect` already existed (unused); this pass added
  `Sheet::Spell` as a separate variant rather than reusing `Effect`, because reusing it would
  have made witch particles sample the wrong PNGs the same way an earlier issue did for the
  block/particle atlas mix-up.
- `ParticleTypes.TOTEM_OF_UNDYING` and `ParticleTypes.END_ROD` both name `glitter_0..7` —
  `totem_of_undying` (built here) reuses the pre-existing `Sheet::Glitter` variant directly.
  `end_rod` shares the sheet and is now built too — its `move()` override turned out to be
  `has_physics = false`, which `move_by` already honours, not a new `Behaviour`.
  **Both `end_rod.json` and `totem_of_undying.json` list `glitter_7 … glitter_0`, descending**,
  which is why `Sheet::Glitter`'s frame list runs that way.

### What was built, per issue

**Combat/event particles, 6 of 8 checklist items reachable via the generic dispatch:**
`note`, `heart`, `angry_villager`, `happy_villager`, `witch`, `totem_of_undying`.

Each is a direct transcription of its vanilla class (see `crates/lodestone-particle/src/
emit.rs` doc comments on `note`/`heart`/`angry_villager`/`happy_villager`/`witch`/
`totem_of_undying` for the exact Java source lines and constants) plus one `match` arm in
`Particles::spawn_one`. Two new full-tick `Behaviour` variants were needed:

- `Behaviour::SweepAttack` (see `docs/combat.md`) and `Behaviour::Suspended`
  (`happy_villager`, from `SuspendedTownParticle`) both bypass `Particle::tick_base` entirely,
  joining `WaterDrop`/`Bubble` as the crate's fourth and fifth full-tick overrides.
  `Suspended`'s `lifetime`-countdown (rather than `age`-increment) and its no-collision
  `move()` are the two things a literal port would get subtly wrong — see
  `Particle::tick_suspended`'s doc comment for the exact post-decrement semantics, and
  `crates/lodestone-particle/src/emit.rs`'s
  `happy_villager_survives_exactly_lifetime_ticks_and_ignores_collision` test for the
  off-by-one this pins (alive through tick `lifetime`, removed on tick `lifetime + 1`).
- `Behaviour::Note`/`Behaviour::Heart` reuse the existing fast-fade-in `quad_size()` formula
  (`Crit`/`AshSmoke`'s `clamp((age+a)/lifetime*32, 0, 1)`) since `NoteParticle`/
  `HeartParticle` both override `getQuadSize` with the identical expression.
- `Behaviour::Spell` (`witch`) reuses `AshSmoke`'s per-tick `set_sprite_from_age()` call but
  needs its own layer (`Translucent`, not `Opaque`).

**Built.** `dust` and `dust_color_transition` both carry a real `ParticleOptions` payload
(`DustParticleOptions`/`DustColorTransitionOptions`) — issue #683's `DEBUG no emitter wired
for particle type "dust"; dropped`, on a live server, is what this closed. The shared decoder
this section used to say did not exist anywhere in the workspace is now
`decode_particle_options` (`crates/protocol/v770/src/adapter/chunk.rs`), called from the
`LEVEL_PARTICLES` handler right after it resolves the registry name: it reads a packed RGB24
big-endian `i32` (`ARGB.red/green/blue`'s own `>> 16`/`>> 8`/plain `& 0xFF`, unpacked to
`[0, 1]` the same way `ARGB.redFloat` does) plus a big-endian `f32` scale for `dust`, and two
such colours for `dust_color_transition`, landing in
`lodestone_model::event::ParticleOptions::Dust`/`DustColorTransition`. Every name this
decoder does not recognise still resolves to `ParticleOptions::None` — correct for the
overwhelming majority of registry entries (a bare `SimpleParticleType`), not a placeholder.

Both share `dust.json`/`dust_color_transition.json`'s sheet — the same eight
`generic_0..generic_7` textures as `Sheet::Generic` itself, confirmed against the real pack
rather than assumed from the registry name, so no new `Sheet` variant was needed.
`Behaviour::Dust`/`Behaviour::DustColorTransition { from, to }`
(`crates/lodestone-particle/src/lib.rs`) transcribe `DustParticleBase`'s shared physics
(`friction = 0.96`, `speedUpWhenYMotionIsBlocked`, `xd/yd/zd *= 0.1`, `quadSize *= 0.75 *
scale`, the `(int)(8.0 / (nextDouble()*0.8+0.2))` lifetime redraw, and the same fast-fade-in
`getQuadSize` formula `Crit`/`AshSmoke`/`Note`/`Heart` already share) plus
`DustParticle`/`DustColorTransitionParticle`'s own per-channel `randomizeColor` draws
(`crates/lodestone-particle/src/emit.rs`'s `dust`/`dust_color_transition`). The colour
transition itself lerps once per game tick rather than vanilla's per-*frame* partial-tick
recompute (`DustColorTransitionParticle.lerpColors`) — the same tick-granularity
simplification `Behaviour::Crit`'s desaturation already makes, documented on the `Behaviour`
variant itself.

**Caught by the decoder's own test, not by inspection:** the first version matched the
namespace-*stripped* path (`"dust"`) against `particle_type_name`'s fully-qualified output
(`"minecraft:dust"`), so every dust particle silently fell back to `ParticleOptions::None` —
exactly the "assertion" species of vacuous fix this repo's evidence standards warn about, a
correct-looking decoder function fed the wrong name by its own caller. A test whose expected
`ParticleOptions::Dust { .. }` value is derived from the RGB bytes rather than from re-running
the decoder (`level_particles_decodes_a_dust_payload`,
`crates/protocol/v770/tests/sound_particle_screen.rs`) is what caught it — `None != Dust {..}`,
not a green pass.

**Correction: `firework` was never in this bucket, and the claim above that it needed a
`ParticleOptions` decoder was wrong.** `ParticleTypes.FIREWORK`
(`.cache/mc/26.2/client-src/net/minecraft/core/particles/ParticleTypes.java`) is a
`SimpleParticleType`, argument-less like `explosion_emitter`/`explosion` — its stream codec
reads no further bytes, so there was never anything to decode. `FireworkExplosion` (the
component this doc's first draft was actually thinking of) is a *data component* on a firework
rocket item, not the particle payload, and is unrelated. **Built now**: `emit::firework`
transcribes `FireworkParticles.SparkParticle` via its `SparkProvider` (the plain wire particle,
not the rocket-explosion burst a `Starter`/`NoRenderParticle` spawns client-side and this
client never receives over the wire at all) — `SimpleAnimatedParticle`'s constructor sets
`friction = 0.91`/`gravity = 0.1` (its third-from-last constructor parameter is gravity, not a
size scale), velocity is taken directly with no jitter, `quadSize *= 0.75`,
`lifetime = 48 + nextInt(12)`, and `SparkProvider.createParticle` sets `alpha = 0.99`. A new
`Sheet::Spark` variant (`spark_7` … `spark_0`, descending per `firework.json`) was needed —
distinct from `Sheet::Glow`, since `firework`'s and `electric_spark`/`glow`'s sheets are
visually similar sparks over physically different textures.

**Correction (creeper explosion sound fix, `7025d90`): `explosion_emitter`/`explosion` are
*not* in this bucket.** Both are `SimpleParticleType`
(`.cache/mc/26.2/client-src/net/minecraft/core/particles/ParticleTypes.java`), whose own
stream codec reads no further bytes — there is no payload to decode, so nothing here is
blocked on the shared `ParticleOptions` codec. `crates/protocol/v770/src/adapter/chunk.rs`'s
`decode_explode` already distinguishes the two registry ids (29/30) for exactly this reason.
(The real blocker in the `explode` packet is `blockParticles`, a
`WeightedList<ExplosionParticleInfo>` whose *entries* do each carry a real particle-options
payload — typically a block state for the flying debris — which is not decoded at all and is
the accurate target for that blocker.)

**Second correction: distinguishing exactly those two registry ids was never the right fix,
and it dropped a live packet.** `SimpleParticleType` is 103 of the registry's 125 entries, not
2 — the 29/30 allowlist just happened to cover the two ids `Level.explode`'s own convenience
helpers pass, and missed every other producer. A real server sent id 34
(`minecraft:gust_emitter_small`, `WindCharge.explode`'s own explosion — see
`world/entity/projectile/hurtingprojectile/windcharge/WindCharge.java`), which is exactly as
argument-less as `explosion_emitter`/`explosion`, and `decode_explode` rejected the whole
packet on it. `lodestone_data::particle_types::is_simple_particle_type` (added alongside the
item-component-decode sweep this doc's sibling, `docs/item-data-component-decode.md`,
describes) is the general census this should have been from the start — derived from
`ParticleTypes.java`'s two `register()` overloads rather than from which two ids one call site
happened to use. Wiring it into `decode_explode` in place of the 29/30 check is tracked
separately; until then the allowlist is known-too-narrow, not merely historical.

**Built.** `explosion_emitter` (`ParticleTypes.EXPLOSION_EMITTER`, the id
`decode_explode` actually sees on the wire — see below) and `explosion` (`EXPLOSION`) both now
have a `Behaviour`, an `emit::` function and a `spawn_one` dispatch arm:

- `Behaviour::HugeExplosionSeed` — `HugeExplosionSeedParticle`, a `NoRenderParticle`: it draws
  no quad at all (excluded explicitly in `ParticleEngine::extract`, since `Behaviour::layer()`
  has no "not drawn" value), and its `tick()` is a full override with no `super.tick()` call —
  over its hardcoded 8-tick life it spawns **six** `Behaviour::HugeExplosion` follow-ups per
  tick, jittered `±4` blocks per axis, at a `size` that grows `0/8 → 7/8` across those ticks.
  Because a particle's own `tick()` cannot call `ParticleEngine::add` (the engine's tick loop
  already holds `self.particles` borrowed), `Particle::tick_huge_explosion_seed` *returns* its
  spawn requests as `(x, y, z, size)` tuples, and `ParticleEngine::tick` turns them into real
  particles only after the loop (and the borrow) ends.
- `Behaviour::HugeExplosion` — `HugeExplosionParticle`: ordinary physics (vanilla's constructor
  touches none of gravity/friction/collision), `lifetime = 6 + nextInt(4)`, a grey tint
  (`nextFloat()*0.6+0.4`, one draw for all three channels), full-bright
  (`getLightCoords` hardcodes `15728880` = `FULL_BRIGHT`), opaque, animating through the new
  `Sheet::Explosion` (`explosion_0`..`explosion_15`, **16** frames — the one sheet in this
  crate that isn't 8, confirmed against the jar's own `particles/explosion.json` texture list
  rather than assumed). `quadSize = 2.0 * (1.0 - size*0.5)`.
- `spawn_one`'s two new arms live in `crates/lodestone-shell/src/particles.rs`. `explosion`
  reuses `xa` as the constructor's `size` parameter, the same repurposing `sweep_attack`'s own
  arm already does for its `size`; `explosion_emitter` ignores every positional argument, since
  `HugeExplosionSeedParticle`'s constructor reads none.
- Verification follows this doc's own convention: `lodestone-particle/src/emit.rs` has exact
  predicted-value tests transcribed from the two Java classes (lifetime ranges, the
  `2.0 - size` quad-size formula at its two extremes, the jitter box, full-bright), plus a
  `NoRenderParticle` exclusion test with a positive control (a sibling `crit` particle in the
  same engine *does* extract a quad) proving the exclusion is deliberate, not a broken
  extractor. `lodestone-shell/src/particles.rs` has the same dispatch-reachability shape
  the combat/event and ambient/environmental passes established, **except** `explosion_emitter` is deliberately *not* in the shared
  loop that asserts `drawn == 1` for every newly-wired kind — it is dispatch-reachable but
  produces zero quads on its own by design, so it gets its own test
  (`explosion_emitter_reaches_pixels_only_after_a_tick`) that dispatches, asserts `drawn == 0`,
  ticks once, and *then* asserts `drawn == 6`. `sheet_particle_resolves_against_the_real_particle_atlas`
  now also emits a `huge_explosion` particle and passed with `unresolved: 0` against the real
  26.2 `client.jar`, and `report.missing_textures.is_empty()` in that same test independently
  confirms all sixteen real `explosion_N.png` files exist (the atlas builder loads every sprite
  the jar's own `explosion.json` declares, not merely the one frame any single particle here
  happens to sample).
- **Still not wired to the real wire path.** `decode_explode`
  (`crates/protocol/v770/src/adapter/chunk.rs`) recognises `explosionParticle`'s registry id only to
  stay byte-aligned and returns a single `Directive::Emit(ClientEvent::Sound { .. })` — it
  never also emits a `ClientEvent::Particles` directive the way `decode_full::<LevelParticles>`'s
  `LEVEL_PARTICLES` handler does. That decode is outside `lodestone-particle`'s and
  `lodestone-shell/src/particles.rs`'s ownership (it lives in the protocol crate); the render
  Behaviour this section describes is the half that was actually missing per this issue's own
  framing, and it is what's built. The exact follow-up: `decode_explode` should push a second
  `Directive::Emit(ClientEvent::Particles { particle: parse_key("explosion_emitter", "particle")?,
  long_distance: false, pos: Vec3 { x, y, z }, offset: Vec3f::ZERO, max_speed: 0.0, count: 1 })`
  alongside its existing `Sound` directive — `net.rs`/`sim.rs` need no new arm at all, since
  `ClientEvent::Particles` already forwards generically into `Particles::spawn_particles` (see
  "The dispatch is reachable independently of any specific gameplay trigger" above).

**`options.particles` is live.** `ClientLevel.doAddParticle` has two filters and this client
had transcribed only one: the 32-block cutoff with its `overrideLimiter` bypass (`long_distance`
on the wire) sat in `sim::net_apply`'s `NetUpdate::Particles` arm, and the particle-**level** test
next to it did not. `config::ParticleLevel` (`All`/`Decreased`/`Minimal`, vanilla's own
`ParticleStatus` order) now reaches that arm through `Sim::set_particle_level`, polled per
presented frame in `app/redraw.rs`.

The fold itself is `Particles::particle_level_permits`, a transcription of
`calculateParticleLevel` — and it is *probabilistic*, which is the part worth knowing before
changing it: `DECREASED` is folded down to `MINIMAL` one draw in three, so it keeps roughly two
thirds of eligible spawns rather than a fixed budget. It draws from the particle engine's own
`JavaRandom`, which is `java.util.Random`-compatible, so `nextInt` matches vanilla exactly (the
*stream* does not, and does not need to — nothing observes particle randomness across the wire).

The nesting matters: `overrideLimiter` bypasses **both** filters, in one branch. Collapsing that
into a single `&&` would let a `Minimal` setting suppress exactly the particles the server marked
un-suppressible.

**The always-show flag now reaches this fold.** It used to be the "read off the wire and
discarded at the decode site" shape in its purest form: `v770` decoded
`LevelParticles::always_show`, `ClientEvent::Particles` did not carry it, `net_apply.rs` passed a
literal `false`, and `particle_level_permits` had taken the parameter all along. So on `Minimal`
every non-override packet particle was deleted, where vanilla's `calculateParticleLevel` gives an
always-show one a one-in-ten lift to `DECREASED`.

Two things about it. It is a **reprieve, not an exemption** — the lift is followed by
`DECREASED`'s own one-in-three fold back down, so the real survival rate on `Minimal` is
`1/10 x 2/3`, one in fifteen. And it is `false` on every legacy family for a reason that is not a
gap: the field does not exist on the pre-26.2 particle packets at all (1.12's `WORLD_PARTICLES`
carries `longDistance` and nothing else), so `false` is what vanilla's own three-argument
`addParticle` overload passes there too. A gate on this cannot be a single send — see
`always_show_gives_a_minimal_setting_particle_a_reprieve_and_not_an_exemption`, which counts over
900 bursts precisely because zero, `SENDS`, and `SENDS/15` are the three hypotheses it has to
tell apart.

**Ambient/environmental: landed.** Fourteen new sheets, two new behaviours, seventeen
new `spawn_one` arms and — the half this section previously called out as the real gap — a
client-predicted per-block-state emitter.

* **The `Sheet` frame list.** `Sheet::frames()` now returns the pack's own texture list rather
  than synthesising `<stem>_<n>`. That was a **shipped bug**, not a refactor: `smoke.json`,
  `cloud.json`, `large_smoke.json`, `snowflake.json`, `effect.json`, `witch.json`,
  `instant_effect.json`, `end_rod.json` and `totem_of_undying.json` all list their frames
  **descending**, so every smoke plume, potion mote, witch mote and totem sparkle animated
  backwards. A sprite lookup still resolved, which is why nothing caught it. It is also the only
  way `enchant.json` is expressible at all — its frames are `sga_a` … `sga_z`.
  `Sheet::Generic` (descending) and `Sheet::PortalGeneric` (ascending) are two variants over the
  *same eight textures*, because a sheet's identity here is its **sequence**.
* **Two new behaviours, both full `tick()` overrides.** `Behaviour::Portal` recomputes position
  from `Particle::spawn` every tick — `xd/yd/zd` are an **amplitude**, never a speed, and neither
  `gravity` nor `friction` is read. `Behaviour::CampfireSmoke` applies a `3.0e-6` gravity straight
  to `yd`, has no friction at all, and fades over the **last 60 ticks** rather than the back half
  of life; a signal fire lives ~300 ticks, so a back-half fade makes it transparent halfway up.
* **`end_rod` needed no new behaviour after all.** It is a `SimpleAnimatedParticle` and the
  no-collision override is `has_physics = false`, which `move_by` already honours — the note above
  claiming it needed a new `Behaviour` shape was wrong.
* **`Particles::ambient_tick`** is the client-predicted emitter: a bounded random scan of nearby
  block states, at vanilla's own sample density, for torches, soul torches, nether portals, end
  gateways, end rods and lit campfires. **None of these is on the wire** — vanilla spawns them from
  `Block.animateTick` — so no dispatch table however complete could have produced them. It rides
  the collision snapshot `Sim::tick_particles` already holds, so it costs no extra lock.
* **`enchant` is wired** — see "The `CritParticle`, `SpellParticle` and
  `FlyTowardsPositionParticle` families" below. The bullet that stood here said it was blocked
  because "its motes travel toward a target the enchanting-table block entity supplies, a
  different wiring shape from everything here", and that was wrong in the way this repo's rules
  warn a blocker usually is: `enchant` arrives on an ordinary `LEVEL_PARTICLES` packet like
  every other type, and the "target" is simply the packet's own position, with the three
  velocity words carrying the *offset* the mote flies in from. No block-entity wiring was
  involved.
* **Still not wired:** `dust`/`dust_color_transition` are built, `tinted_leaves`/`flash`
  now share `entity_effect`'s `ColorParticleOption` arm (see "The everyday environment pass"
  below), and the whole `BlockParticleOption` family is built (see "The `BlockParticleOption`
  family" below). The one option-carrying shape left is `ItemParticleOption` (`item`), which
  wants a new `ParticleOptions` variant as well as an arm in `decode_particle_options`. The
  decoder itself is not the blocker.

### The `CritParticle`, `SpellParticle` and `FlyTowardsPositionParticle` families

Eleven types, and the pass that closed the one **stranded** particle
`cargo xtask world-coverage` reports.

`enchanted_hit` was that finding: `Sheet::EnchantedHit` was declared, listed in `Sheet::all()`
and therefore *stitched into the particle atlas*, with no emitter anywhere. `Sheet::Effect` and
`Sheet::Enchant` were the same shape one layer removed — atlas-resident and constructed by
nothing outside a test — but the census could not see them, because it reports the **subject**
side and only `enchanted_hit` happened to share a name with a sheet frame stem.

All three are now live, and the reverse query that would have found them is a gate:
`no_sheet_is_atlas_resident_and_unreachable_from_the_dispatch` drives the **whole 125-entry
particle registry** through `spawn_particles` and requires every sheet in `Sheet::all()` to come
back. Observed failing (with `Effect` named) before the fix.

| type | vanilla provider | sheet, per its own `particles/<name>.json` |
|---|---|---|
| `enchanted_hit` | `CritParticle.MagicProvider` | `enchanted_hit` |
| `damage_indicator` | `CritParticle.DamageIndicatorProvider` | `damage` |
| `effect`, `entity_effect` | `SpellParticle.InstantProvider`/`MobEffectProvider` | `effect_7…0` |
| `instant_effect` | `SpellParticle.InstantProvider` | `spell_7…0` |
| `infested`, `raid_omen`, `trial_omen` | `SpellParticle.Provider` | one texture each |
| `enchant` | `FlyTowardsPositionParticle.EnchantProvider` | `sga_a…sga_z` |
| `nautilus` | `FlyTowardsPositionParticle.NautilusProvider` | `nautilus` |

Three things this cost, worth carrying:

* **The class does not decide the sheet; the type's own JSON does.** Six registry types share
  `SpellParticle` across *four* sheets, and the damage indicator is a `CritParticle` that does
  not share the crit sprite. Every wrong answer here still resolves to a real sprite, so nothing
  is red — `each_spell_and_crit_type_samples_the_sheet_its_own_definition_names` exists for that,
  with its expectations read out of the pack rather than out of our `Sheet` enum, and it collects
  mismatches instead of asserting inside its loop so a neuter reports every wrong arm.
* **`FlyTowardsPositionParticle`'s `xd/yd/zd` are an offset, not a velocity**, and its vertical
  term is a **quartic** sag (`(1 - pos)^4 * 1.2`) rather than the linear rise its closed-form
  sibling `PortalParticle` uses. Both misreadings produce a live, plausibly-moving particle. The
  flight also *ends* at its deepest point, 1.2 blocks below the target — a glyph dives into the
  table rather than landing on it, and "it lands on the target" was this gate's first predicted
  value and was wrong by exactly that 1.2.
* **`effect`, `entity_effect` and `instant_effect` drew white, and no longer do.** Their tint
  rides a `SpellParticleOption` (an RGB24 word plus an f32 power) or a `ColorParticleOption` (one
  ARGB word), and `decode_particle_options` had no arm for either — the bytes were captured into
  `LEVEL_PARTICLES`'s `#[mc(remaining)]` field and dropped, so every potion mote reached
  `spawn_one` as `ParticleOptions::None` and took the module's `WHITE` constant. `v770` now
  decodes all three into `ParticleOptions::Spell`/`Color`, and `emit::spell_instant`/
  `spell_mob_effect` apply the provider's `setColor`/`setPower`/`setAlpha` calls.

  Three things this cost. **The three types are three different option classes** — eight bytes
  for the two `SpellParticleOption` ones against four for `ColorParticleOption` — so they cannot
  share a decode arm, and the alpha byte only exists on the `entity_effect` one
  (`MobEffectProvider` is the only `SpellParticle` provider that calls `setAlpha`). **The
  fallback still draws white rather than dropping**, because the legacy families do not carry
  the payload in this shape at all — 1.12's `WORLD_PARTICLES` puts a mob-spell tint in the offset
  words — and dropping would be a visible regression there; the fallback arms log on the
  `particles` target instead, since an untinted mote looks exactly like a working one and that is
  why this survived so long. **`setPower` is not a velocity multiply**: it is
  `yd = (yd - 0.1) * power + 0.1`, rescaling about the upward bias the base constructor added.

### The ambient and biome family

Thirteen types, chosen by how often a player actually sees them rather than by
registry order. `poof` — the puff every mob death, every breeding and every spawner
spawn produces — is the headline: it had no arm at all and hit the catch-all.

| class | types |
|---|---|
| `ExplodeParticle` | `poof`, `spit` |
| `BaseAshSmokeParticle` | `ash`, `white_ash`, `white_smoke` (joining `smoke`/`large_smoke`) |
| `SuspendedParticle` | `underwater`, `crimson_spore`, `warped_spore`, `spore_blossom_air` |
| `SuspendedTownParticle` | `mycelium`, `composter`, `egg_crack`, `dolphin` (joining `happy_villager`) |

Four things this found or fixed:

* **`spore_blossom_air` was wired as a `DripParticle` and is a `SuspendedParticle`.**
  It shares `drip_fall`'s *texture* with `falling_spore_blossom` and nothing else,
  and the sheet stem is exactly why the misreading is plausible. The lifetimes do not
  overlap: `SporeBlossomAirProvider` draws a flat `500..=1000` ticks while a drip's is
  `(int)(64 / (nextFloat() * 0.8 + 0.2))`, whose **maximum** is 320 — so a single
  sample separates the hypotheses, which is what
  `spore_blossom_air_outlives_every_possible_drip` asserts. As a drip it vanished
  roughly twenty times too fast.
* **`ExplodeParticle` needed a new `Behaviour`, and reusing `AshSmoke` would have been
  invisible.** Both animate their sheet by age, but `BaseAshSmokeParticle` *also*
  overrides `getQuadSize` with `clamp(age / lifetime * 32, 0, 1)` and `ExplodeParticle`
  does not — so borrowing it makes every poof start at **exactly zero size** and swell
  in over its first thirty-second. Nothing about the particle count, its sprite or its
  physics would show that. `Behaviour::Animated` is the split;
  `a_poof_is_full_size_on_its_first_frame_and_a_smoke_puff_is_not` computes both
  hypotheses and was observed landing on the wrong one's predicted `0.0` under a neuter.
* **A one-frame sheet is not a frame of an eight-frame one.** Eight types name
  `generic_0` alone in their own JSON, and reusing `Sheet::Generic` for them would
  animate a still particle. `Sheet::Generic0` exists for the same reason
  `Sheet::PortalGeneric` does: a sheet's identity is its frame *sequence*.
* **`BaseAshSmokeParticle`'s eight positional parameters became `AshSmokeParams`.**
  `smoke` and `ash` differ by the sign of `dirY`, the sign and magnitude of `gravity`,
  one `colorRandom` and one `maxLifetime` — four lone numbers in a row of bare floats,
  which is the transposition shape this repo's evidence rules warn about. Naming them
  makes the swap unspellable rather than merely unlikely.

Four of these (`underwater`, `crimson_spore`, `warped_spore`, `ash`) deliberately ignore
the packet's three velocity words: their vanilla providers *draw* the velocity rather
than taking it from the caller, so that is the class's shape and not a dropped field.

### The `GlowParticle` family, and three sheet/provider bugs

Eight types, three of which were **already wired and wired wrong**. This is the
group where the sheet-comes-from-the-JSON rule paid for itself repeatedly.

| type | class | sheet |
|---|---|---|
| `electric_spark`, `glow`, `scrape`, `wax_on`, `wax_off` | `GlowParticle` | `glow` |
| `copper_fire_flame`, `small_flame` | `FlameParticle` | `copper_fire_flame`, `flame` |
| `sculk_soul` | `SoulParticle.EmissiveProvider` | `sculk_soul_0…10` |

The three fixes:

* **`electric_spark` and `glow` shared one approximation of the wrong class.** They
  were emitted by an `emit::spark` that took `FireworkParticles.SparkParticle`'s
  shape: `friction 0.9` against `GlowParticle`'s `0.96`, an `8 + nextInt(4)` lifetime
  against `nextInt(2) + 2`, no tint, no `speedUpWhenYMotionIsBlocked`, and collision
  left on where `hasPhysics` is false. The sheet was right, which is why it looked
  fine. `glow` is additionally its *own* provider — a glow squid's two-population
  green shimmer, drawn from a `nextBoolean()` — and had been collapsed into
  `electric_spark`'s.
* **`small_gust` pointed at `Sheet::Gust`.** `GustParticle.SmallProvider` shares the
  class, and `small_gust.json` names `small_gust_0…6` — **seven** frames of its own,
  against `gust_N`'s twelve. It sampled the wrong texture and indexed past the end of
  a sequence it does not have.
* **`sculk_soul` would have taken `Sheet::Soul`.** `sculk_soul.json` names
  `sculk_soul_N`; only the eleven-frame count coincides. `emit::soul` hard-coded its
  sheet, so the sheet became a parameter — a producer feeding a constant, one layer in.

`emit::spark` is deleted rather than left beside its replacement: two plausible-looking
emitters for one family is worse than none.

**The jar-backed gate is now registry-driven.** `sheet_particle_resolves_against_the_real_particle_atlas`
used to call a hand-listed set of ~20 `emit::` functions, extended by hand per sheet —
the fixture corpus that certifies "the sheets I remembered". It now walks all 125
registry entries through `spawn_particles`. Measured against the real 26.2 jar:
**112 definitions, 285 sprites, a 512×512 atlas, 65 wired types, 0 unresolved.** A
hermetic `(Sheet, frame) -> UV` fixture resolves *any* frame name, so this is the only
place a wrong frame-naming convention can show. It also names the one legitimate
undrawn particle (`explosion_emitter`, a `NoRenderParticle`) rather than relaxing the
`drawn == alive` equality, so a second undrawn type cannot hide behind a `>=`.

### The drip family, and the first particle that spawns another

Seventeen registry types — the largest single family in the registry — over one
Java class, one `(kind, phase)` table in `emit::drip`, and **a chain that runs
inside the particle's own tick**.

Five of the seventeen were already wired, as unchained one-shots with a
hardcoded `64 / nextFloat()` lifetime. What that meant in play: a cave ceiling
grew drips that hung for the wrong length of time and then **blinked out without
ever falling**. The chain is `DripParticle.tick`'s business, not any spawn
site's, so no amount of dispatch-table work upstream could have supplied it — a
`dripping_water` particle spawns a `falling_water` one when its 40 ticks are up,
and that spawns a `splash` or a `landing_*` where it hits.

`Particle::tick`'s follow-up channel was `Vec<(f64, f64, f64, f32)>`, serving
exactly one caller (`HugeExplosionSeed`). It is now `Vec<Spawn>`, a three-variant
enum, because a drip's successor needs a kind and a phase and a water drip's
successor is not a drip at all.

Things the transcription turned on:

* **`DripParticle` applies gravity raw** (`yd -= gravity`), not through the base
  tick's `0.04` scale. A drip therefore falls twenty-five times harder than the
  same `gravity` number means anywhere else, which is why a hanging phase's value
  is `1.2e-3` — and honey's `1.2e-5`, because `HoneyHangProvider` multiplies the
  already-scaled `0.06 * 0.02` by a further `0.01`.
* **`lifetime--` is a post-decrement tested against zero**, so a drip lives
  `lifetime + 1` ticks and `lifetime` counts *down*. `CoolingDripHangParticle`
  reads that counter directly, so modelling it as an incrementing `age` inverts
  the colour ramp.
* **A lava drip cools.** `g = 16 / (elapsed + 16)` and `b = 4 / (elapsed + 8)`,
  recomputed every tick from white-hot `(1, 1, 0.5)`. The check that both
  constants are right is that at `elapsed == 40` the ramp lands on
  `LavaFallProvider`'s independently specified `setColor(1.0F, 0.2857143F,
  0.083333336F)` — two vanilla methods that never mention each other, which have
  to meet. That is an outside expectation rather than a restatement of the code.
  The colour is computed from the pre-decrement lifetime, so the k-th tick sees
  `elapsed == k - 1`; this gate's first prediction was off by exactly that one.
* **`dripping_dripstone_lava` lands as `landing_lava`.** It borrows plain lava's
  landing type rather than having one of its own, and `falling_water` /
  `falling_dripstone_water` land as `splash` — a `SplashParticle`, not a drip
  phase, which is why `Spawn` needs a third variant.

`a_hanging_water_drip_falls_and_the_falling_drip_splashes` is the gate, observed
failing under a neuter that removes the hand-off (`left: []`).

### Clouds, lava and ink

Six more, taken in the order a player meets them: `cloud`, `sneeze`, `lava`,
`squid_ink`, `glow_squid_ink`, `sculk_charge_pop`.

* **`Behaviour::Animated` gained a `layer` field** rather than a second
  near-identical variant. `ExplodeParticle` is `OPAQUE` and
  `SculkChargePopParticle` is `TRANSLUCENT`, and there is genuinely nothing else
  to tell them apart — the layer *is* the difference.
* **`LavaParticle` is the second particle whose own tick spawns another**, and
  unlike a drip's hand-off it is probabilistic: `nextFloat() > age / lifetime`,
  rolled every tick, so a fresh pop trails smoke almost continuously and an old
  one almost never does. `a_lava_pop_trails_smoke_and_stops_as_it_ages` measures
  early-half against late-half rather than a single tick — one tick's roll is a
  coin flip and proves nothing — and the neuter reported `0` trails in the first
  25 ticks. It also reads **none** of the packet's velocity words: the
  constructor damps them to `0.8` and then overwrites `yd` outright with
  `nextFloat() * 0.4 + 0.05`, so every pop launches upward.
* **`PlayerCloudParticle`'s numbers run opposite to the smoke family it
  resembles.** The quad is *grown* `1.875×` rather than shrunk `0.75×`, the
  lifetime is the usual draw *multiplied by 2.5*, and the colour draw is
  `1 - nextFloat() * 0.3` (near-white) against smoke's `nextFloat() * 0.3`
  (near-black). One thing is not ported and is named at the behaviour: vanilla's
  tick also drags the puff toward a player within two blocks, which needs a
  player position this crate has no access to.
* **`ARGB.colorFromFloat` takes `(alpha, red, green, blue)`.** A glow squid's ink
  is `colorFromFloat(1.0F, 0.2F, 0.8F, 0.6F)` — alpha **1.0** over a
  `(0.2, 0.8, 0.6)` teal, not the alpha-0.6 reading the leading `1.0F` invites.
  Four same-typed arguments in a row, which is the transposition shape again.

### `dragon_breath`, and a payload that carries no colour at all

Every lingering potion leaves a `dragon_breath` cloud on the ground, and the type had no
dispatch arm at all — it fell into `spawn_one`'s catch-all and drew nothing. It is the fourth
option-carrying type this decoder handles, and it is the one that shows why an option class
cannot be inferred from what a particle looks like.

* **`PowerParticleOption` is a bare `ByteBufCodecs.FLOAT` and nothing else.** Four bytes, no
  colour, against `SpellParticleOption`'s eight — so `dragon_breath` cannot share `effect`'s arm
  even though both end in a power. `DragonBreathParticle` draws its purple per particle out of
  two narrow bands (`Mth.nextFloat(random, 0.7176471, 0.8745098)` for red,
  `0.8235294..0.9764706` for blue) with a **third draw for green whose bounds are both `0.0`** —
  a real draw, not a constant, so omitting it shifts every later number in the RNG stream.
* **`Sheet::DragonBreath` is `generic_5`, `generic_6`, `generic_7` — ascending, three frames.**
  A *subsequence* of `Sheet::Generic`'s eight, in the opposite direction, which is the same
  "identity is the frame sequence, not the pixels" case `PortalGeneric` and `Generic0` already
  document. Pointed at `Generic` it would still resolve to a real sprite.
* **The tick is a full override with no `super.tick()`**, so no gravity, no vertical friction
  and no ground drag. The tell is horizontal and it is the whole visual: `if (y == yo) { xd *=
  1.1; zd *= 1.1; }` fires on every tick a `hasPhysics = false` cloud with no vertical velocity
  takes, so horizontal speed *grows* by `1.1 × 0.96 = 1.056` per tick where a `tick_base` port
  would damp it by `0.96`. That is what makes a lingering cloud creep across a floor instead of
  hanging where it landed, and the two hypotheses move the number in opposite directions, so one
  tick separates them.
* **`hasHitGround` is transcribed even though it can never become `true`** here: `hasPhysics =
  false` means `move` never sets `onGround`, so the `yd += 0.002` lift and the vertical friction
  are both unreachable — in vanilla too. Dropping a clause because it happens to be unreachable
  is how a later change to one field silently breaks another.

## Verification

- `crates/lodestone-particle/src/emit.rs`: one test per new emitter asserting an **exact**
  predicted value from the Java source, not merely "a particle appeared" — e.g.
  `note_colour_matches_the_three_phase_shifted_sine_formula` recomputes the expected RGB
  independently from `NoteParticle.java`'s own sine formula rather than checking a range, and
  `totem_of_undying_has_two_disjoint_colour_populations` draws 200 seeded samples and asserts
  both the ~75%/~25% branches actually fire *and* that their colour ranges never overlap
  (the magnitude check, not just the sign).
- `crates/lodestone-shell/src/particles.rs`:
  `every_newly_wired_kind_reaches_its_emitter_through_the_generic_dispatch` proves each new
  `kind` string is reachable through `spawn_particles` → `spawn_one`, the same path a real
  packet uses, not just that calling `emit::foo` directly works. Its negative control,
  `a_near_miss_kind_still_falls_into_the_catch_all`, checks a near-miss string
  (`"sweep"`, `"totem"`, …) still falls through, so the new arms are exact-match, not prefix
  matches that would also fire on garbage.
- `sheet_particle_resolves_against_the_real_particle_atlas` (`#[ignore]`d, run against the
  real `.cache/mc/26.2/client.jar`) is the one test that proves every new `Sheet::stem()`
  (`"sweep"`, `"spell"`, `"angry"`, `"glint"`) names a texture that actually exists in the
  jar, not a plausible-looking guess — measured `unresolved: 0` across all ten emitted
  instances (the pre-existing three plus the seven new ones) the last time this was run.

### The everyday environment pass

Fifteen types a player sees constantly, taking `cargo xtask world-coverage`'s particle bucket
from **86 drawn / 39 absent** to **101 drawn / 24 absent**. All fifteen were genuinely absent
— no struct, no codec, no sheet, no dispatch arm — which was checked type by type before any
code was written, because this repo has repeatedly found work already done and merely
undispatched.

| type | vanilla class | reuses | new |
|---|---|---|---|
| `rain` | `WaterDropParticle` | `Sheet::Splash`, `Behaviour::WaterDrop` | — |
| `fishing` | `WakeParticle` | `Sheet::Splash` | `Behaviour::Wake` |
| `bubble_column_up` | `BubbleColumnUpParticle` | `Sheet::Bubble` | `Behaviour::BubbleColumnUp` |
| `current_down` | `WaterCurrentDownParticle` | `Sheet::Bubble` | `Behaviour::WaterCurrentDown` |
| `bubble_pop` | `BubblePopParticle` | — | `Sheet::BubblePop`, `Behaviour::BubblePop` |
| `snowflake` | `SnowflakeParticle` | `Sheet::Generic` | `Behaviour::Snowflake` |
| `dust_plume` | `DustPlumeParticle` | `Sheet::Generic`, `emit::base_ash_smoke` | `Behaviour::DustPlume` |
| `cherry_leaves` | `FallingLeavesParticle.CherryProvider` | — | `Sheet::CherryLeaves`, `Behaviour::FallingLeaves` |
| `pale_oak_leaves` | `…PaleOakProvider` | `Behaviour::FallingLeaves` | `Sheet::PaleOakLeaves` |
| `tinted_leaves` | `…TintedLeavesProvider` | `Behaviour::FallingLeaves`, `ParticleOptions::Color` | `Sheet::TintedLeaves` |
| `firefly` | `FireflyParticle` | — | `Sheet::Firefly`, `Behaviour::Firefly` |
| `flash` | `FireworkParticles.FlashProvider` | `ParticleOptions::Color` | `Sheet::Flash`, `Behaviour::FireworkFlash` |
| `item_slime` | `BreakingItemParticle.SlimeProvider` | `SpriteSource::Item`, `Behaviour::Terrain` | `emit::item_burst_particle` |
| `item_cobweb` | `…CobwebProvider` | the same | — |
| `item_snowball` | `…SnowballProvider` | the same | — |

Seven things this pass paid for, in rough order of how much time each would cost the next
person:

* **`rain` is not the falling rain.** `WeatherEffectRenderer`'s textured columns
  (`crates/lodestone-render/src/weather.rs`, and `docs/weather.md`) are the streaks; the
  `minecraft:rain` registry type is `WaterDropParticle`, the splash on impact. They are two
  independent subsystems and wiring one says nothing about the other. The same split holds
  for `snowflake`, which is a real particle and not the snow columns.
* **`rain` and `splash` differ in exactly one number, and the natural way to write `rain`
  gets it wrong.** `SplashParticle extends WaterDropParticle` and overrides `gravity` from
  `0.06` to `0.04` — so copying the already-written `emit::splash` silently keeps the
  splash's value and leaves raindrops hanging in the air. Nothing downstream can see it:
  the count, the sheet, the layer and the behaviour are identical.
  `the_water_types_carry_their_own_gravity_and_not_a_sibling_s` predicts both hypotheses and
  requires the measurement to land on one; observed failing with `0.04` planted.
* **The three `item_*` types take `BreakingItemParticle`'s *four*-argument constructor**, not
  the seven-argument one `emit::item_particle` already implements. The seven-argument sibling
  damps the constructor's jitter to a tenth before adding the caller's velocity, and these
  three have no caller velocity at all — routing them through it leaves the crumbs
  motionless. `emit::item_burst_particle` is the four-argument form.
* **Those same three are the sharpest transposition risk in `spawn_one`**: three adjacent
  arms differing in one item name each, all reaching one helper, and a swap changes nothing
  observable except the texture.
  `the_three_item_burst_types_carry_their_own_registry_item` asserts each id against a
  registry lookup made in the test *and* that the three are pairwise distinct.
* **`FallingLeavesParticle` is one class and three providers that differ in five constants at
  once**, and the tinted variant takes the **pale oak** set rather than a third one of its
  own. `emit::LeafParams` carries them as a named set for exactly that reason; a loose
  argument list is five chances to transpose a pair.
* **`tinted_leaves` and `flash` share `entity_effect`'s decode arm.** All three carry a
  `ColorParticleOption` — one packed ARGB word — and all three use the alpha byte for real
  (`FlashProvider` calls `setAlpha` with it). The arm must stay four-component; narrowing it
  to RGB24 for the leaf's sake would make every firework flash opaque. `decode_particle_options`
  previously carried a comment saying these two were deliberately absent "until the emitter
  lands"; that comment is now the decode arm, which is the point — a comment asserting an
  absence is evidence about the moment it was written.
* **`FireflyParticle` overrides `getLightCoords` and the override is not a light value.** It
  returns the fade fraction scaled by 255. `ParticleEngine::extract`'s own doc already named
  this as the trap; it is not ported, and the firefly samples world light like everything
  else. Its death test (`!getBlockState(pos).isAir()`) is approximated as
  `CollisionView::blocks_motion`, because this view cannot answer "is this air" — the
  approximation errs only toward keeping a mote alive inside grass, never toward deleting a
  visible one.

Two of these needed the quantized trig rather than the library's: `WaterCurrentDownParticle`'s
spiral and `OverlayParticle`'s size curve both call `Mth.cos`/`Mth.sin`, and both sweep through
the axis crossings where the table and `f32::cos` disagree. `FallingLeavesParticle`'s swirl
calls `Math.cos`/`Math.sin` — the library trig — and is transcribed that way; the two are not
interchangeable and vanilla itself uses both, in the same file.

Still absent from this bucket after the pass, and why:

* **`elder_guardian`** — the only particle in the registry that is not a quad. It is a full
  `GuardianParticleModel` on its own `ParticleRenderType.ELDER_GUARDIANS` pass, which means a
  rig, a texture and a pipeline rather than an emitter. Still a separate piece of work; four
  things about it were checked against the 26.2 decompile while the `BlockParticleOption` family
  was being built, and each removes a guess:

  * **The rig is already correct and needs no change.** `ElderGuardianParticle` bakes
    `ModelLayers.ELDER_GUARDIAN`, and that layer *is* `GuardianModel.createBodyLayer().apply(
    ELDER_GUARDIAN_SCALE)` with `ELDER_GUARDIAN_SCALE = MeshTransformer.scaling(2.35F)` — a
    **mesh** transform, not the renderer's. `ElderGuardianRenderer`'s own `2.35`-looking
    constructor argument is `1.2F`, a shadow radius. So `lodestone_assets::entity_models::
    elder_guardian_model`, which is `scaled(guardian_model(), 2.35)`, is exactly the layer the
    particle bakes. `GuardianParticleModel` itself adds nothing: it is a bare `Model<Unit>` over
    the baked root, with no `setupAnim`, so the rig is drawn in its bind pose.
  * **It is drawn camera-relative and never reads its own position.**
    `ElderGuardianParticleGroup.ElderGuardianParticleRenderState.fromParticle` builds a fresh
    `PoseStack` — camera rotation, then `Axis.XP.rotationDegrees(60 - 150 * ageScale)`, then
    `scale(0.42553192, -0.42553192, -0.42553192)`, then `translate(0, -0.56, 3.5)` — and the
    particle's `x`/`y`/`z` appear nowhere in it. Level render space is camera-relative, so the
    guardian hangs at a fixed offset in front of the eye regardless of where the packet put the
    particle. That is the effect as played: the curse shows a guardian in your face, not one out
    in the water. A pass that placed it at the particle's world position would be wrong in a way
    no structural check could see.
  * **`0.42553192` is `1 / 2.35`, exactly**, so the scale step *undoes* the layer's own mesh
    scale and the guardian is drawn at plain guardian size. It is not a free constant: if the
    baked rig's scale ever changes, this must change with it, and transcribing the number without
    that premise is how the two silently stop cancelling.
  * **The alpha lane is the actual blocker, not the rig or the pose.** The render type is
    `RenderTypes.entityTranslucent` and the per-instance colour is
    `ARGB.colorFromFloat(0.05 + 0.5 * sin(ageScale * PI), 1, 1, 1)` — a real blend alpha that
    fades in and out over the 30-tick life. `EntityPipeline::banner_layer_pipeline` already
    supplies the right *state* (`ALPHA_BLENDING`, no cutout, depth write off) over the same
    bind-group layouts, but `EntityInstanceRaw` has nowhere to put the alpha: its `tint` word's
    top byte is the hurt-overlay alpha and `white_overlay`'s low byte is the creeper flash. So
    the pass needs either a spare lane in that shared struct or an instance format of its own.
  * The simulation half is the small half: `gravity = 0`, `lifetime = 30`, no tick override at
    all. It wants a `Behaviour` that is **excluded from `ParticleEngine::extract`**, the way
    `Behaviour::HugeExplosionSeed` already is, so it never emits a quad — and building that half
    alone would be an island, since nothing would draw it.
* **`vibration`, `trail`, `shriek`, `vault_connection`, `trial_spawner_detection`** — each
  carries a position or a target on the wire, the same shape.

`falling_dust`, `block_crumble`, `dust_pillar` and `block_marker` **were** on this list and are
now built — see the section below.

### The `BlockParticleOption` family

Five registry types — `block`, `block_marker`, `block_crumble`, `dust_pillar`, `falling_dust` —
sharing **one** wire payload and agreeing on nothing else. `BlockParticleOption`'s stream codec
is `ByteBufCodecs.idMapper(Block.BLOCK_STATE_REGISTRY)`: a single **VarInt** block-state id, and
the only VarInt in `decode_particle_options`, where every other arm reads a fixed-width `INT` or
`FLOAT`. It reaches the shell as `ParticleOptions::BlockState { state }`.

The five providers are registered in `ParticleResources`, and reading the shared payload as a
shared *behaviour* is the mistake to avoid — a `block_marker` would fall and a `falling_dust`
would wear the block's own texture:

| type | vanilla provider | what it is |
|---|---|---|
| `block` | `TerrainParticle.Provider` | the packet's position and velocity, nothing overridden |
| `block_crumble` | `TerrainParticle.CrumblingProvider` | velocity discarded outright, lifetime re-rolled to `nextInt(10) + 1` |
| `dust_pillar` | `TerrainParticle.DustPillarProvider` | velocity `(gaussian/30, ya + gaussian/2, gaussian/30)`, lifetime `nextInt(20) + 20` |
| `block_marker` | `BlockMarker.Provider` | no gravity, no physics, no tint, a flat `0.5` quad size, 80 ticks |
| `falling_dust` | `FallingDustParticle.Provider` | a **generic sheet mote** tinted from the block, with its own tick |

The predecessor's reading that this was "a wire-payload job, the geometry already exists" was
right for the first four: `SpriteSource::BlockState` and `Behaviour::Terrain` were already what
`destroy_block` uses, so those arms are the payload plus a provider's overrides. `falling_dust`
is the exception — it needed a new `Behaviour::FallingDust`, because its tick is a full override
whose fall is a **raw** `yd -= 0.003` applied after the move and clamped at a terminal `-0.14`,
and neither number goes through `gravity`. Reading `0.003` as a gravity multiplier gives a
thirteenth of the speed and loses the clamp.

`dust_pillar`'s vertical term is the packet's own `ya` **plus** a gaussian, not a gaussian alone;
that additive base is why a mace smash throws a column upward rather than a puff sideways, and it
is invisible in a single sample because the gaussian swamps it — the gate that pins it averages
200 draws and compares the mean against both hypotheses.

Both refusals in `Particles::block_state_payload` are deliberate. A missing payload is a caller
fault that production cannot produce, since the adapter decodes the state alongside the type. Air
is vanilla's own — `TerrainParticle.createTerrainParticle` returns `null` for air and for
`moving_piston` — and it matters because a producer that reads a block *after* removing it sends
the air state. The provider's third clause, refusing an invisible render shape, is not ported:
there is no per-state render-shape table here, and the states it would catch have no particle
sprite either, so they are already refused one layer down.

**One gap, and it is visible.** `FallingDustParticle.Provider` resolves its colour through a
three-step chain: `FallingBlock.getDustColor`, else the block's tint source, else
`state.getMapColor(level, pos).col`. This client has data for the middle step only — there is no
per-state map-colour table anywhere in `lodestone-data`, and neither Mojang's `blocks.json` nor
`registries.json` carries one — so a block with no tint source arrives white. A sand mote is
therefore pale rather than sand-coloured. Closing it means dumping `BlockState.getMapColor` off
the real server the way `crates/lodestone-data/tests/{collision_shapes,hardness}.rs` dump their
tables; nothing else will do, because a map colour is not derivable from the model or the atlas.

## Configuration

No new env vars, flags or constants. Every numeric literal in the new `emit::` functions is a
transcribed vanilla constant, documented inline with its Java source line.

## Dependencies

- `lodestone_particle::{Sheet, Behaviour, emit}` — see `crates/lodestone-particle/src/lib.rs`
  and `emit.rs` doc comments for the per-type Java source.
- `Particles::spawn_one`/`spawn_particles` (`crates/lodestone-shell/src/particles.rs`) — the
  dispatch this pass extended.
- `decode_particle_options` (`crates/protocol/v770/src/adapter/chunk.rs`) and
  `lodestone_model::event::ParticleOptions` — the shared decoder `dust`/`dust_color_transition`
  needed, now built (see "Built" above). `explosion`/`explosion_emitter` and `firework` are
  **not** in that list (see "Built"/"Correction" above); none of the three needed the decoder,
  only the render `Behaviour`/`emit::` function this pass and the firework fix added.
  `sculk_charge` **is** now decoded: `ParticleTypes.SCULK_CHARGE` is
  `SculkChargeParticleOptions(float roll)`, a one-field payload
  (`.cache/mc/26.2/client-src/net/minecraft/core/particles/SculkChargeParticleOptions.java`),
  and it has an arm in `decode_particle_options` plus its own `emit::sculk_charge` — the roll is
  what makes a spreading charge's motes lie along the direction it is travelling instead of all
  sharing one orientation. Writing that emitter surfaced two things the shared
  `emit::animated_ambient` call it replaced had wrong: the lifetime is a per-particle
  `random.nextInt(12) + 8` draw rather than a constant, and borrowing `Behaviour::AshSmoke` added
  a `* 32` quad-size fade-in that `SculkChargeParticle` does not have (it overrides neither
  `getQuadSize` nor the default layer — `Behaviour::Animated { layer: Translucent }` is the
  matching pair). The provider also calls `setParticleSpeed` with the packet's own words, so the
  base constructor's velocity jitter is discarded for this type.

## How to change it

- Adding another ambient/event particle: find its class in `.cache/mc/26.2/client-src/net/
  minecraft/client/particle/`, check its registration in `ParticleResources.java` for which
  class handles it, and its own `assets/minecraft/particles/<name>.json` for which physical
  sheet it samples (do **not** assume the sheet stem matches the registry name — `witch` and
  `instant_effect` both use `spell_N`, not `witch_N`; `angry_villager` uses `angry.png`, a
  single frame, not `angry_villager.png`). Add a `Sheet`/`Behaviour` variant only if the tick
  shape is genuinely new; several types share one class and can share a `Behaviour`.
- The shared `ParticleOptions` decoder now exists (`decode_particle_options`,
  `crates/protocol/v770/src/adapter/chunk.rs`) — adding another option-carrying type (`item`,
  `shriek`, `trail`, `vibration`, …) means a new arm there plus, if the
  payload's own colour/scale/whatever needs to reach the emitter, a new
  `lodestone_model::event::ParticleOptions` variant threaded through `NetUpdate::Particles` and
  `Particles::spawn_particles`/`spawn_one` (`crates/lodestone-shell/src/particles.rs`) the same
  way `Dust`/`DustColorTransition` are. **Match on the fully-namespaced registry name**
  (`"minecraft:dust"`, not `"dust"`) inside the decoder — `particle_type_name` returns the
  namespaced form, and matching the stripped path silently decodes nothing for every particle;
  this pass's own first draft made exactly that mistake and only a test whose expected value
  came from re-deriving the RGB bytes independently caught it. `explosion`/`explosion_emitter`
  and `firework` looked like they belonged on this list too, and did not — see
  "Built"/"Correction" above for how that was checked before being ruled out, not assumed.
- **Read the type's `particles/<type>.json` frame list out of the jar, never infer it.** Half of
  vanilla's multi-frame sheets are listed descending and one (`enchant`) is alphabetic. A wrong
  order animates backwards and every test still passes, because the sprite resolves either way.
- Read the Java class's `tick()`/`getQuadSize()`/`getLightCoords()` overrides before assuming an
  existing `Behaviour` fits — but also before adding a new one: `Behaviour::AshSmoke` already
  means "ordinary physics, advance the sheet by age" and covers `SoulParticle`, `SculkCharge`,
  `Gust` and `SonicBoom`, and `has_physics = false` already covers a `move()` override that only
  skips collision.
