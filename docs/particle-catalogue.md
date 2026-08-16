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
* **Still not wired:** `enchant` (its motes travel toward a target the enchanting-table block
  entity supplies, a different wiring shape from everything here). `dust`/`dust_color_transition`
  are now built — see "Built" above — but the other option-carrying types (`block`,
  `block_marker`, `falling_dust`, `item`, …) still want their own arm in
  `decode_particle_options`; the decoder itself is no longer the blocker, only the per-type
  payload shape.

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
  `sculk_charge` is **not** fixed by this pass despite the name grouping it with `dust` above in
  an earlier draft of this doc: `ParticleTypes.SCULK_CHARGE` is
  `SculkChargeParticleOptions(float roll)`, a real one-field payload
  (`.cache/mc/26.2/client-src/net/minecraft/core/particles/SculkChargeParticleOptions.java`),
  and `decode_particle_options` does not have an arm for it — its existing dispatch arm
  (`Particles::spawn_one`'s `"sculk_charge"` case) spawns and animates correctly but reads no
  roll, matching this doc's own "explicitly not attempted" convention rather than a discovered
  bug.

## How to change it

- Adding another ambient/event particle: find its class in `.cache/mc/26.2/client-src/net/
  minecraft/client/particle/`, check its registration in `ParticleResources.java` for which
  class handles it, and its own `assets/minecraft/particles/<name>.json` for which physical
  sheet it samples (do **not** assume the sheet stem matches the registry name — `witch` and
  `instant_effect` both use `spell_N`, not `witch_N`; `angry_villager` uses `angry.png`, a
  single frame, not `angry_villager.png`). Add a `Sheet`/`Behaviour` variant only if the tick
  shape is genuinely new; several types share one class and can share a `Behaviour`.
- The shared `ParticleOptions` decoder now exists (`decode_particle_options`,
  `crates/protocol/v770/src/adapter/chunk.rs`) — adding another option-carrying type (`block`,
  `block_marker`, `falling_dust`, `item`, `sculk_charge`, …) means a new arm there plus, if the
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
