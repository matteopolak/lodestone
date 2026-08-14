# Leashing (issue #236)

## What it is

Lead attach/detach, the fence-anchor re-parent, and the distance-based pull
and snap physics for a leashed mob — vanilla `Leashable`/`LeadItem`
(`.cache/mc/26.2/src/net/minecraft/world/entity/Leashable.java`,
`.../item/LeadItem.java`). Lives in `crates/lodestone-server/src/mobs.rs`:
`MobSim::try_leash`, `MobSim::try_leash_to_fence`, `MobSim::tick_leashes`,
plus the `LeashHolder`/`LeashOutcome` types and a `leash_holder` field on
`SimMob`.

## How it works

`Mob.canBeLeashed()`'s real default is `!(this instanceof Enemy)` — every
species that is not one of vanilla's `Enemy`-tagged hostiles accepts a lead,
not a curated allowlist. `is_leashable_species` is a one-line wrapper around
the existing `is_hostile_species` predicate, checked per-species against the
real class hierarchy rather than assumed from the name overlap (see
`is_leashable_species`'s own doc comment for the exceptions vanilla layers on
both sides, none of which apply to any species this sim spawns today).

`MobSim::try_leash(mob_id, holder, holding_lead, creative)` mirrors vanilla
`Entity.interact`'s two leash branches:

1. Already leashed to `holder` → detach, spawning a `minecraft:lead` item
   unless `creative`.
2. Not already held by a *different* player, holding a lead, leashable, and
   within `LEASH_TOO_FAR_DIST` (12 blocks) → attach.
3. Otherwise refused.

`MobSim::try_leash_to_fence(holder, fence_pos)` mirrors `LeadItem.bindPlayerMobs`:
every mob currently leashed to `holder` moves to `LeashHolder::Fence(fence_pos)`.

`MobSim::tick_leashes` (called every tick from `tick_with_terrain`, no
production hook needed) applies vanilla's two distance thresholds:
`LEASH_ELASTIC_DIST` (6 blocks) starts a pull toward the holder, and
`LEASH_TOO_FAR_DIST` (12 blocks) snaps the lead and drops it as an item.

## How to change it, and the gotchas

- **`LeashHolder::Fence` is a bare `BlockPos`, not a spawned entity.**
  Vanilla ties a mob to a real `LeashFenceKnotEntity` — a decoration entity
  with no health, no `AttributeMap`, no goals. `SimMob` assumes all three
  (that is what makes it a *mob* sim), so modelling a real knot would need a
  second, lighter entity kind this crate does not have yet. The cost: no
  client-visible knot to render or right-click, and a knot with several mobs
  tied to it cannot be right-clicked once to free them all (vanilla's own
  behaviour there). If a "decoration entity" concept lands for other reasons
  (item frames, paintings), route the knot through it rather than growing a
  parallel path here.
- **The pull/snap physics is a disclosed simplification of vanilla's real
  spring/torque model.** Real vanilla (`Leashable.checkElasticInteractions`)
  computes a spring interaction across up to four attachment-point pairs and
  applies angular momentum to yaw. `tick_leashes` applies one straight-line
  impulse toward the holder's position through `SimMob::apply_knockback` —
  the same physics-owner handoff `explosion.rs`/`damage.rs` already use for
  combat knockback, not a second model grown here. Three specific gaps,
  named in `tick_leashes`'s own doc comment: no yaw torque, no per-entity
  bounding-box subtraction from the elastic threshold (flat `6.0` instead of
  `6.0 - both widths`), and an unresolvable holder (disconnected player,
  removed mob) silently drops the leash with no item.
- **The server.rs hooks are not wired yet.** Both `try_leash` (right-click a
  leashed/leashable mob) and `try_leash_to_fence` (right-click a fence with a
  lead in hand) need call sites in `server.rs`, which is outside this pass's
  ownership. See the broker note this session left (leashing server hooks)
  for the exact anchors — `ServerBound::InteractEntity`'s existing taming
  dispatch, and `apply_use_item_on`'s block-click path — and the proposed
  patches.
- **No `SET_ENTITY_LINK`-equivalent wire packet was checked for.** Whether
  `v770` can even encode a leash link is unverified — that lives in
  `crates/protocol/v770/src/server_protocol.rs`, out of ownership this pass.
  Until it exists (or is confirmed to already exist), a client would see the
  leashed mob's position update from the pull physics but never draw a lead
  line to it.
- **Adding a leashable exception**: a species where `!is_hostile_species`
  gives the wrong answer (a water creature, or the eventual hoglin/zoglin)
  needs a small exception table inside `is_leashable_species`, not a rewrite
  of the function — see its own doc comment for exactly which vanilla
  species diverge and in which direction.

## Configuration

No feature flags or env vars. `LEASH_TOO_FAR_DIST` (12.0) and
`LEASH_ELASTIC_DIST` (6.0) are `const`s in `mobs.rs`, transcribed from
vanilla's own `Leashable` constants.

## Dependencies

- `is_hostile_species` — `is_leashable_species` is a thin wrapper over it.
- `SimMob::apply_knockback` / `NavigatingMob::apply_knockback` — the
  velocity-application seam the pull physics reuses rather than duplicating.
- `MobSim::spawn_item` — the dropped-lead item on detach and on snap.
- `.cache/mc/26.2/src/net/minecraft/world/entity/Leashable.java`,
  `.../decoration/LeashFenceKnotEntity.java`, `.../item/LeadItem.java`.
