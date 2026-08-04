# The particle catalogue: what's wired, what isn't, and why

Issues #178 (ambient/environmental types) and #182 (combat/event types). Both start from the
same measured gap: `crates/protocol/v770/src/generated/particle_types.rs` (registry id →
name) and `crates/lodestone-data/src/generated/particle_types.rs`
(`PARTICLE_TYPE_COUNT: u32 = 125`) decode the full vanilla particle registry and network
dispatch resolves ids correctly, but `Particles::spawn_one`
(`crates/lodestone-shell/src/particles.rs`) only had a `match` arm for six of them
(`"flame"`, `"smoke"`, `"large_smoke"`, `"crit"`, `"splash"`, `"bubble"`) — every other name
fell into the `other => tracing::debug!(...)` catch-all and was silently dropped.

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

Checked directly against `.cache/mc/26.2/src/net/minecraft/world/level/Level.java:497-509`:
`Level.addParticle(...)`'s **default body is empty** — a genuine no-op. `ClientLevel`
overrides it to spawn a real local `Particle`; `ServerLevel` does **not** override it at all.
So any vanilla gameplay code that calls `this.level().addParticle(...)` (not
`serverLevel.sendParticles(...)`) does nothing when that code runs on the server — the
particle only appears because the *same* method also runs on `ClientLevel` for entities the
client itself ticks (breeding hearts, villager mood icons) or because a different, synced
mechanism (a block-action/`triggerEvent` broadcast, for note blocks) replays the call
client-side. This is the entity-state analogue of the block-state ambient prediction trap
#178's own issue body already named.

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
  have made witch particles sample the wrong PNGs the same way issue #45 did for the
  block/particle atlas mix-up.
- `ParticleTypes.TOTEM_OF_UNDYING` and `ParticleTypes.END_ROD` both name `glitter_0..7` —
  `totem_of_undying` (built here) reuses the pre-existing `Sheet::Glitter` variant directly.
  `end_rod` was **not** built this pass (see "What's still open" below) even though it shares
  the sheet, because its `move()` override (no collision at all) needs a `Behaviour` shape
  this pass didn't add.

### What was built, per issue

**#182 (combat/event), 6 of 8 checklist items reachable via the generic dispatch:**
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

**Explicitly blocked, not attempted:** `explosion_emitter`/`explosion`, `firework`, and
`dust` all carry a `ParticleOptions` payload (`DustParticleOptions`, `FireworkExplosion`, an
implicit colour/scale for explosion) that this workspace has no generic decoder for — the
same blocker #26 already named for the explosion particle. Building one of these without the
shared decoder would mean hand-rolling a second, narrower one; flagged rather than
special-cased, per the brief for this pass.

**#178 (ambient/environmental): not started this pass.** Every type on its checklist needs
either a bespoke `Behaviour` (`portal`, `soul`, `end_rod`, `gust`, `sonic_boom` each have a
`tick()`/`getQuadSize()`/`getLightCoords()` override with no existing analogue in this crate
— `PortalParticle` in particular recomputes position from a closed-form easing curve every
tick rather than integrating velocity at all) or is blocked on the same `ParticleOptions`
decoder (`dust`, `sculk_charge`). None of that is started. Separately, and regardless of the
render-side work: #178's own issue body is right that the real vanilla trigger for most of
these is a client-side per-block-state tick (torch flame, soul fire, portal), which is
`sim.rs` territory and was off-limits for this pass anyway.

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
- No protocol or ECS changes. The `ParticleOptions` decoder that `dust`/`sculk_charge`/
  `explosion`/`firework` are blocked on does not exist yet anywhere in the workspace.

## How to change it

- Adding another ambient/event particle: find its class in `.cache/mc/26.2/client-src/net/
  minecraft/client/particle/`, check its registration in `ParticleResources.java` for which
  class handles it, and its own `assets/minecraft/particles/<name>.json` for which physical
  sheet it samples (do **not** assume the sheet stem matches the registry name — `witch` and
  `instant_effect` both use `spell_N`, not `witch_N`; `angry_villager` uses `angry.png`, a
  single frame, not `angry_villager.png`). Add a `Sheet`/`Behaviour` variant only if the tick
  shape is genuinely new; several types share one class and can share a `Behaviour`.
- Before touching `dust`, `sculk_charge`, `explosion`, `explosion_emitter`, or `firework`:
  build the shared `ParticleOptions` decoder first (protocol-side, brokered — not
  `lodestone-particle`'s to build alone). Special-casing one of these without it just
  produces a second narrow decoder to reconcile later.
- Before touching `portal`, `soul`, `end_rod`, `gust`, or `sonic_boom`: each needs its own
  bespoke `Behaviour` (see "What's still open" above for why `PortalParticle` in particular
  is not a `move_by`-based particle at all). Read the Java class's `tick()`/`getQuadSize()`/
  `getLightCoords()` overrides before assuming any existing `Behaviour` fits.
