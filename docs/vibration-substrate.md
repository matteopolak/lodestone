# Vibration substrate (issue #459, steps 2–3)

## What it is

A world-event type (`VibrationEvent`) and a host-side "nearest audible event"
resolution (step 2), plus a first real consumer: warden anger, pursuit, a
real melee-or-sonic-boom hit, and a real invulnerable emerging spawn window
(step 3, `crates/lodestone-server/src/mobs/warden.rs`). Step 1 of that issue
(the Brain driver reaching production) is separate, tracked elsewhere. Step 3
here is **almost complete** — anger accumulation, pursuit, emerging and both
attacks (melee and a real ranged sonic boom) are all built and reach a real
health change through production ticks; only `Digging` (the warden's
give-up-and-despawn retreat) remains, deliberately left open rather than
guessed at — see `warden.rs`'s own module doc for exactly why.

## How it works

`crates/lodestone-entity/src/vibration.rs` is pure data plus one pure
function, with no `std::fs` or platform dependency (it compiles for
`wasm32-unknown-unknown` the same as every other data module in that crate):

- `VibrationEvent` — vanilla's `minecraft:game_event` registry, modelled for
  now as exactly `#minecraft:warden_can_listen`'s own members (plus the one
  member its nested `#minecraft:shrieker_can_listen` reference adds),
  transcribed from
  `.cache/mc/26.2/src/data/minecraft/tags/game_event/{warden_can_listen,shrieker_can_listen}.json`.
  `resonate_1..15`/`shriek` (sculk-catalyst-internal signal amplification,
  never posted by an ordinary producer) are deliberately excluded — see the
  type's own doc for why an unmodelled subset is the honest shape here
  rather than invented completeness.
- `nearest_listenable(origin, radius, vibrations)` — the nearest posted
  vibration within `radius` of `origin`. No travel delay and no
  line-of-sight occlusion: vanilla's own `VibrationSystem.Ticker` walks a
  signal toward its listener over several ticks and can be blocked by
  intervening blocks, and this substrate's first pass answers "audible this
  instant, unobstructed" instead — a disclosed simplification, not a silent
  one, matching the tractability assessment issue #459 itself recorded.
- `is_vibration_listener(species)` — which species this substrate currently
  resolves an answer for. `"warden"` only, today; table-driven so a second
  listener (a calibrated sculk sensor block-entity, which would need its own
  host-side wiring since it is not a mob) has one place to be added.
- `WARDEN_LISTENER_RADIUS` (16.0) — `Warden.VibrationUser.getListenerRadius`.

### The host-side event log, mirroring persistent anger

`crates/lodestone-server/src/mobs/mod.rs` is the host: `MobSim` carries a
per-tick `posted_vibrations: Vec<PostedVibration>` log, `pub fn
post_vibration(position, event)` for producers to call, and `resolve_vibrations`
(private, called once per tick) that resolves each listener species' nearest
answer into `SimMob::nearest_vibration` and drains the log back to empty —
the same "resolved once by the host, read as a plain value by everything
else" shape `feed_perception`'s own persistent-anger block already uses
(`docs/DESIGN.md`'s own evidence log calls this pattern out explicitly).

`resolve_vibrations` runs at the **end** of `MobSim::tick_with_terrain`,
deliberately after `reap_dead` (which runs much earlier in the same tick) —
not inside `feed_perception` (which runs before `reap_dead`), so a death
this same tick is audible this same tick rather than one tick late.

### The one producer this crate owns today: `reap_dead`

`MobSim::reap_dead` posts `VibrationEvent::EntityDie` at a dying mob's own
position, **with that mob's own entity id as `source`** — `LivingEntity.die`
posting `GameEvent.ENTITY_DIE` with `GameEvent.Context.of(this)`. Every other
vanilla producer (`block_destroy`, `block_place`, `container_open`, `step`,
`swim`, ...) lives outside `crates/lodestone-server/src/mobs/**`'s owned
files (`server.rs`, `block_placement.rs`, `block_entities.rs`) and is real,
disclosed follow-up work — `post_vibration` is `pub` specifically so a
caller there can post one without this module changing.

### The step-3 consumer: warden anger, pursuit and two attacks (`mobs/warden.rs`)

`MobSim::resolve_warden_anger`, run right after `resolve_vibrations` each
tick: counts down a freshly-spawned warden's `EMERGE_DURATION_TICKS` (134,
`WardenAi.EMERGE_DURATION`) window, during which it is invulnerable
(`SimMob::apply_damage`'s own warden arm, matching `Warden.isInvulnerableTo`'s
`isDiggingOrEmerging` gate) and struck by nothing (the strike loop below
skips it outright — `EMERGE` outranks `FIGHT` in `WardenAi::updateActivity`'s
own priority list); decays every warden's anger by 1/tick regardless of the
emerge window (`onReceiveVibration` is a plain listener callback in vanilla,
not a `Brain` behaviour, so it keeps running through `EMERGE`); absorbs this
tick's `nearest_vibration` answer at its `source` by 35
(`Warden.increaseAngerAt`'s own default); and — once a warden's anger crosses
80 (`AngerLevel.ANGRY.getMinimumAnger()`), its emerge window has ended, and a
target exists — lands a real hit: a ranged, true-damage sonic boom
(`SonicBoom`'s own 15-block-XZ/20-block-Y range and 40-tick cooldown, 10.0
damage) when the target is in that range and the boom is off cooldown,
falling back to melee inside a 3-block reach otherwise — both through the
same `SimMob::apply_damage` pipeline every other hit in this crate uses.
**Single-suspect**, not vanilla's multi-suspect `AngerManagement`: a
vibration from a different source replaces the tracked target outright
rather than being tracked alongside it.

**Pursuit is real**, on a separate seam from `resolve_warden_anger`: an angry
warden's own `Brain` runs `lodestone_entity::brain::roster::warden_brain`'s
`FIGHT` activity, walking toward `MobController::angry_target` — fed, once
per tick, by `MobSim::feed_perception` resolving `SimMob::warden_anger_target`
to a live position, gated on `AngerLevel::Angry` and (like the strike loop)
on the emerge window having ended. That behaviour only ever walks; it never
calls `BrainMob::attack`, so `resolve_warden_anger` stays the single place a
hit is actually resolved.

**Still open: `Digging`** — vanilla's give-up-and-retreat behaviour, which
does not just play an animation: `Digging.stop` calls
`body.remove(Entity.RemovalReason.DISCARDED)`, i.e. a digging warden
despawns outright. Its entry condition depends on `MemoryModuleType.DIG_COOLDOWN`'s
*initial* state, which reading the decompile alone could not pin down —
getting it wrong risks either every idle warden vanishing within seconds of
spawning, or the behaviour never triggering at all (today's state). Left
open on purpose rather than guessed; `warden::POSE_DIGGING`/
`DIGGING_DURATION_TICKS`/`DIGGING_COOLDOWN_TICKS` are reserved constants for
whoever resolves it.

All of this is disclosed in `warden.rs`'s own module doc, along with why a
stale (already-reaped) target's anger is not proactively pruned — doing so
would erase the anger a corpse-sourced vibration just granted on the same
tick it was granted, and why sonic-boom knockback is not yet delivered to a
player target (this crate has no mechanism at all to deliver a velocity
impulse to a player from the server — a pre-existing, wider gap, not
introduced here).

**Also still open, and wider than the warden**: nothing in this crate posts
a vibration whose `source` is ever a *player* — `reap_dead`'s `EntityDie` is
still the only wired producer, and it only ever names another mob. So today
a warden's anger target, sonic-boom target and melee target are always
another `SimMob`, never a player, however loudly a nearby player mines —
see the producer section below.

## How to change it, and the gotchas

- **A new producer** calls `MobSim::post_vibration(position, event)` from
  wherever the real vanilla action happens. It does not need to live in
  `mobs/mod.rs` — any code holding a `&mut MobSim` can call it.
- **A new listenable event** is a new `VibrationEvent` variant, kept honest
  by `vibration_events_match_the_real_tag`'s two-way check against the
  transcribed tag list (every modelled variant is a real tag member, and
  every real tag member has a modelled variant) — extend both together.
- **A second listener species** needs its own entry in
  `is_vibration_listener` and, if its radius or listenable set differ from
  the warden's, its own constant/predicate — `WARDEN_LISTENER_RADIUS` and
  `VibrationEvent::is_warden_listenable` are both named for the one listener
  this substrate currently serves, not generic.
- **`SimMob::nearest_vibration` now has one real consumer** (`resolve_warden_anger`,
  feeding anger, pursuit and both attacks). Only `Digging` remains —
  `mobs/warden.rs`'s own module doc names exactly what is uncertain about
  it (the `DIG_COOLDOWN` initial-state ambiguity) and why that made it a
  deliberate stopping point rather than a guess.
- **Travel delay and occlusion are not modelled.** Adding them means
  reproducing `VibrationSystem.Ticker`'s multi-tick walk and a block-based
  line check — real follow-up work, now with a consumer that would actually
  benefit from the distinction.
- **A second producer** would immediately make the warden more than a
  one-trick reactor to nearby deaths — `block_destroy`/`step`/`container_open`
  are the natural next ones, and each lives outside this crate's `mobs/**`
  ownership (see the producer section above).

## Configuration

None — no flags or env vars gate this.

## Dependencies

`lodestone_model::Vec3` (position). `MobSim::reap_dead` (the one producer),
`MobSim::resolve_vibrations` (the resolution pass),
`SimMob::nearest_vibration` (the read seam). Deliberately independent of
`crates/lodestone-entity/src/brain/**` (the `Sensor`/`Memory` system) and of
`lodestone_ecs::GameEvent` (the client-side plugin event bus) — see
`vibration.rs`'s own module doc for the three-way name collision this type
was chosen to avoid.

## Verification

```bash
cargo test -p lodestone-entity --lib --no-fail-fast -- vibration::
cargo test -p lodestone-server --lib --no-fail-fast -- vibration_substrate_tests::
cargo test -p lodestone-server --lib --no-fail-fast -- mobs::warden::
cargo test -p lodestone-entity --lib --no-fail-fast -- brain::roster::
```
