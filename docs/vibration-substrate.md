# Vibration substrate (issue #459, steps 2–3)

## What it is

A world-event type (`VibrationEvent`) and a host-side "nearest audible event"
resolution (step 2), plus a first real consumer: warden anger and a real
melee consequence (step 3, `crates/lodestone-server/src/mobs/warden.rs`).
Step 1 of that issue (the Brain driver reaching production) is separate,
tracked elsewhere. Step 3 here is **partial** — anger accumulation and a
genuine in-range hit are built; pursuit, dig/emerge and the sonic boom are
not (see `warden.rs`'s own module doc for exactly what and why).

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

### The step-3 consumer: warden anger (`mobs/warden.rs`)

`MobSim::resolve_warden_anger`, run right after `resolve_vibrations` each
tick, decays every warden's anger by 1/tick, absorbs this tick's
`nearest_vibration` answer at its `source` by 35 (`Warden.increaseAngerAt`'s
own default), and — once a warden's anger crosses 80
(`AngerLevel.ANGRY.getMinimumAnger()`) and its target is within a 3-block
melee reach — lands a real hit through the same `SimMob::apply_damage`
pipeline every other hit in this crate uses. **Single-suspect**, not
vanilla's multi-suspect `AngerManagement`: a vibration from a different
source replaces the tracked target outright rather than being tracked
alongside it. **No pursuit**: without `ai::roster` coverage or a Brain melee
behaviour (neither exists for the warden), nothing moves it toward a target
— a hit only lands on a target already in range. **No dig/emerge, no sonic
boom.** All of this is disclosed in `warden.rs`'s own module doc, along with
why a stale (already-reaped) target's anger is not proactively pruned — doing
so would erase the anger a corpse-sourced vibration just granted on the same
tick it was granted.

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
- **`SimMob::nearest_vibration` now has one real consumer** (`resolve_warden_anger`),
  but pursuit, dig/emerge and the sonic boom remain — `mobs/warden.rs`'s own
  module doc names exactly what each would need (a movement seam the warden
  has none of today; a pose/animation state; a ranged burst-damage attack).
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
```
