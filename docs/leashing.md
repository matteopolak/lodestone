# Leashing (issue #236)

## What it is

Lead attach/detach, the fence-anchor re-parent, and the distance-based pull
and snap physics for a leashed mob — vanilla `Leashable`/`LeadItem`
(`.cache/mc/26.2/src/net/minecraft/world/entity/Leashable.java`,
`.../item/LeadItem.java`). Lives in `crates/lodestone-server/src/mobs/mod.rs`
(the `mobs.rs` file split moved several other domains to sibling files under
`mobs/`, but leashing stayed in `mod.rs` — it is core `MobSim` tick logic, not
a per-entity-kind slice):
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
- **`try_leash`'s `server.rs` hook is wired.** `ServerBound::InteractEntity`'s
  main-hand arm calls `try_leash` before the existing `MobSim::interact`
  taming dispatch, exactly mirroring vanilla `Mob.interact`'s own order
  (`Entity.interact`'s leash branches run, and short-circuit, before
  `mobInteract`). `LeashOutcome::Attached` consumes one `minecraft:lead` from
  the hotbar through the same `consume_one`/window-0 sync every other
  consuming interaction there uses; `Detached` needs no follow-up (`try_leash`
  already spawns the dropped lead itself); `Refused` falls through to the
  taming chain unchanged, matching vanilla's own `PASS` fallthrough.
- **`try_leash_to_fence`'s hook (right-click a fence with a lead) is still
  unwired** — it needs a call site in `apply_use_item_on`'s block-click path
  (`crates/lodestone-server/src/item_use.rs`, contended and mid-edit by
  another pass at the time this line was written), and even wired,
  `LeashHolder::Fence` still resolves to no wire entity id (see the bare-`BlockPos`
  gap above), so a fence-anchored lead would still draw no rope until a knot
  entity exists. Two separate gaps stacked on one interaction; neither
  closed here.
- **`SET_ENTITY_LINK` is now encoded and sent (issue #236, closed).**
  `ServerProtocol::encode_set_entity_link` (`crates/lodestone-server/src/protocol.rs`)
  and its `v770` implementation exist, ported from
  `ClientboundSetEntityLinkPacket.write` (two plain big-endian `i32`s, source
  then dest, `dest == 0` meaning "no holder" — the same order the constructor
  and the field declaration happen to agree on, checked against `write`
  anyway per this repo's port-from-`write` rule). `EntitySnapshot::leash_link`
  carries the resolved wire target — `MobSim::snapshots`'
  `resolve_leash_target` turns a `LeashHolder` into it: a `Player` resolves
  through the uuid-keyed player list (never the entity id directly, for the
  same reconnect reason `player_position` gives), a `Mob` is already a wire
  id, and a `Fence` resolves to `None` (no knot entity, as above).
  `EntityStreamer::sync` (`crates/lodestone-server/src/server.rs`) diffs it
  exactly like `metadata`: once on spawn when already `Some` (the join-late
  case — a client that joins or re-enters view range of an already-leashed
  mob still gets the rope, not just whoever witnessed the attach), and again
  on any update where it changed (covers both attach and detach).
- **The client-side chain is wired too, through the cheapest available
  channel.** `v770`'s adapter already decoded `SET_ENTITY_LINK` into
  `ClientEvent::EntityLeashed`, but nothing claimed it —
  `lodestone_model::event::route` had it in the "claimed by nothing" bucket.
  `lodestone_ecs::ingest::apply_entity_leash` now folds it into a `Leashed`
  component, and `lodestone_ecs::player::push_leash_lines`
  (`ExtractSet::Debug`) turns a leashed mob's `Position` and its resolved
  holder position into a `DebugLine` each frame. **This reuses the existing
  world-space debug-line channel and its already-installed render pass,
  deliberately** — `DebugLines` is a generic per-frame geometry channel whose
  pipeline already runs every frame regardless of any F3 toggle (gating, where
  it exists, lives in the *populating systems*, not the channel), so this adds
  zero new GPU pipeline, shader or `app.rs` wiring. It is **not** vanilla's
  rope: no texture, no catenary sag, a flat coloured line between raw
  (non-interpolated) tick positions. A future pass wanting real parity needs
  an actual pipeline in `lodestone-render`/`lodestone_shell::gpu`.
- **Adding a leashable exception**: a species where `!is_hostile_species`
  gives the wrong answer (a water creature, or the eventual hoglin/zoglin)
  needs a small exception table inside `is_leashable_species`, not a rewrite
  of the function — see its own doc comment for exactly which vanilla
  species diverge and in which direction.

## Configuration

No feature flags or env vars. `LEASH_TOO_FAR_DIST` (12.0) and
`LEASH_ELASTIC_DIST` (6.0) are `const`s in `mobs/mod.rs`, transcribed from
vanilla's own `Leashable` constants.

## Dependencies

- `is_hostile_species` — `is_leashable_species` is a thin wrapper over it.
- `SimMob::apply_knockback` / `NavigatingMob::apply_knockback` — the
  velocity-application seam the pull physics reuses rather than duplicating.
- `MobSim::spawn_item` — the dropped-lead item on detach and on snap.
- `.cache/mc/26.2/src/net/minecraft/world/entity/Leashable.java`,
  `.../decoration/LeashFenceKnotEntity.java`, `.../item/LeadItem.java`.
