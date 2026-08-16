# The ender dragon fight

## What it is

The server-side state for the ender dragon boss fight: the eleven-phase
flight/combat state machine, end-crystal beam healing, and the
`EndDragonFight` controller (persisted "already defeated" flag, scan-on-load,
boss-bar progress value, the exit-portal block geometry, and the four-crystal
respawn sequence). Lives at `crates/lodestone-server/src/dragon/`, ported from
26.2's decompiled `EnderDragon`/`EnderDragonPhaseManager`/`EndCrystal`/
`EnderDragonFight`/`DragonRespawnStage` under
`.cache/mc/26.2/src/net/minecraft/world/entity/boss/enderdragon/` and
`.../world/level/dimension/end/`.

Every function in the module is **pure** — no world, no entity, no packet.
Given inputs, `dragon::phase::PhaseManager::tick` returns a phase transition;
given inputs, the `dragon::fight` free functions return a new
`FightState` plus a list of effects (`ScanOutcome`, `DeathOutcome`,
`Vec<RespawnEvent>`) for a caller to perform against the real world. This is
deliberate: the module has no dependency on `crates/lodestone-server/src/mobs/mod.rs`'s
`SimMob`/`MobSim` machinery, `ChunkWorld`, or `ServerProtocol`, so it is
testable with nothing but a scripted sequence of inputs and is safe to land
without a concurrent production-wiring pass touching those larger, actively-
edited files.

## How it works

Three submodules:

- **`dragon::phase`** — `PhaseManager` owns the current `Phase` (an
  eleven-variant enum whose discriminants match `EnderDragonPhase`'s own
  static-initializer order, because that order **is** the wire value
  `EnderDragon.DATA_PHASE` carries) plus the per-phase timer state each
  vanilla `DragonPhaseInstance` implementor keeps as instance fields
  (`fireball_charge`, `scanning_time`, `flame_ticks`/`flame_count`,
  `attacking_ticks`, `time_since_charge`, `sitting_damage_received`).
  `PhaseManager::tick` takes a `DragonInputs` struct and a `&mut dyn
  DragonRng` and returns an optional `PhaseEffect` (currently only
  `FireFireball`); the phase transition itself is applied internally and
  observable via `PhaseManager::current`.

  **The one deliberate substitution:** vanilla drives most transitions off a
  `Path`/`Node` search across a fixed 12-node ring above the arena
  (`EnderDragon.findClosestNode`/`findPath`) — full aerial pathfinding this
  codebase's flying-mob AI does not have (`lodestone-entity`'s goal/pathfinder
  stack is built for ground navigation, not node graphs). Every phase that
  needs "has the current flight leg finished" takes it as a per-tick input
  (`DragonInputs::leg_complete`) instead of computing it from a path. Every
  other condition — health thresholds, crystal counts, timers, RNG rolls,
  hurt amounts — is ported with vanilla's own numbers. See `phase`'s own
  module doc for the full accounting, and its `tests` module for the
  transition-table gate (a scripted sequence of inputs driving an exact phase
  *sequence* through eight of the eleven phases, plus isolated gates for the
  other three).

- **`dragon::crystal`** — `crystal_heal_tick` is the exact port of
  `EnderDragon.checkCrystals`'s heal clause: **1.0 HP once every 10 ticks**
  (a proc, not a smeared rate) while a live nearest crystal exists and health
  is below max, clamped to max health. `NearestCrystal` tracks the
  `nearestCrystal` field's own lifecycle (cleared when the tracked crystal is
  removed; reassigned on a rescan roll, `should_rescan_crystals`, matching
  `random.nextInt(10) == 0`).

- **`dragon::fight`** — `FightState` (`needs_state_scanning`, `dragon_killed`,
  `has_previously_killed_dragon` — the persisted flags `EnderDragonFight`'s
  own codec round-trips) plus free functions: `scan_state` (the world-load
  scan for an existing dragon/portal), `set_dragon_killed` (what to do the
  tick the dragon actually dies — egg placement gated on first-kill-ever,
  portal activation, and a `spawn_gateway: true` flag for the caller to act
  on — **no gateway position formula or shuffled-pool bookkeeping is
  implemented anywhere**, a real disclosed gap rather than a simplification;
  see `fight::FightState`'s own doc comment), `boss_bar_value` (the `progress`/
  `visible` pair a `BOSS_EVENT` packet needs), `exit_portal_blocks` (a
  clause-for-clause port of `EndPodiumFeature.place`'s block geometry — the
  bedrock/end-stone foundation, the bedrock-ring-and-portal-or-air disc, the
  domed air clearing above it, the four-block bedrock pole, and its four wall
  torches), `respawn_crystal_positions`/`try_respawn` (the four N/S/E/W cells
  three blocks out from the portal where live end crystals must stand), and
  `tick_respawn` (the five-stage `DragonRespawnStage` spectacle:
  `Start → PreparingToSummonPillars → SummoningPillars → SummoningDragon →
  End`).

### What this does not attempt, and why

**No obsidian pillars anywhere in this repo — no longer true; see below.**
This section originally said the pillars (`EndSpikeFeature`) and the exit
portal (`EndPodiumFeature`) were "structure/entity work" with "a gameplay
placer" that had never been written. `crates/lodestone-worldgen/src/end/
spikes.rs` (`end_spikes_for_seed`, `end_spike_blocks`) and `.../end/
podium.rs` (`end_podium`) now port both, and `MobSim::init_end_dragon_fight`
(`mobs/dragon.rs`) wires them to real crystal/dragon spawns — see "The End's
own furniture, and how it gets placed on first arrival" below for exactly
what landed and how it is called. (An earlier version of this doc
said flatly "there is nothing to cage"; re-verify a "not modelled" claim
against the tree before repeating it — this file already names three other
docs that shipped exactly that kind of drift.)

- Crystal healing and the fight's crystal count still do not *require*
  pillars — a crystal in this world is wherever a caller places one — but
  `init_end_dragon_fight` now places all ten atop real spikes by default.
- The "caged vs. uncaged crystal" distinction issue #276 names is real now:
  `EndSpike::guarded` (exactly two of ten, for any seed — see
  `end::spikes::tests::exactly_two_spikes_are_guarded_for_any_seed`) drives
  `end_spike_blocks`' iron-bars cage.
- `tick_respawn`'s `SummoningPillars` stage, parameterized by spike count,
  is unchanged by this — it is a *respawn*-sequence concern (Bring back a
  dragon), not initial-spawn, and initial spawn is the gap this update
  closes.

**No `BOSS_EVENT` wire packet — no longer true.** `ServerProtocol::
encode_boss_event_add`/`encode_boss_event_update_progress`/
`encode_boss_event_remove` all exist in `protocol.rs`, and `crate::server`'s
`sync_boss_bars` (`stream_pass`'s own call, once per connection per pass)
diffs `EntitySource::boss_bars()` — whose one real producer is
`MobSim::boss_bars`, which already appends the dragon's own bar — onto the
wire. Zero remaining work here; see "What consumes this today" below for the
full chain.

**No live entity, no ticking, no streaming — no longer true; see below.**

## What consumes this today

**The whole chain is real, from spawn to the wire, once something calls
`init_end_dragon_fight`.** `mobs/dragon.rs` and `mobs/end_crystal.rs` give
`MobSim::spawn_dragon`/`spawn_end_crystal`, `tick_dragons` (drives
`dragon::phase`/`dragon::crystal` with real inputs: crystal count and
positions from `MobSim`'s own crystal map, nearest-player distance from
`MobSim::players`), `damage_dragon`, `dragon_boss_bar`, and
`destroy_end_crystal`. Both kinds are plain `HashMap<i32, _>` entries — the
same `TrackedTnt`/`TrackedMinecart` shape, not a goal-driven `SimMob` — and
both are appended in `MobSim::snapshots()`.

**`tick_dragons` is called from `crate::tick::run_tick_loop`** — the
one-line addition this doc's earlier version said `tick_tnt`/
`tick_vehicles`/`tick_minecarts` already modelled the shape for
(`mobs.with(super::mobs::MobSim::tick_dragons);`) has landed.

**`PhaseEffect::FireFireball` reaches a real `minecraft:dragon_fireball`
projectile** through the same `spawn_projectile_from` funnel every other
projectile in this crate uses — see `tick_one_dragon`'s own `FireFireball`
arm and `mobs::dragon::tests::a_strafing_dragon_now_actually_fires_a_real_fireball_projectile`.

**`phase::PhaseManager::dying_health_this_tick` has a real production
caller** — a killing blow on a flying dragon now redirects into the dying
phase at `1.0` health instead of leaving health frozen at `1.0` forever; see
`mobs::dragon::tests::a_killing_blow_while_flying_now_actually_finishes_the_dragon_off`.

**The boss bar reaches the wire with no new call site**, exactly as
`mobs::wither`'s own doc describes for the identical shape: `MobSim::
boss_bars` (in `mobs/dragon.rs`) is the dragon's own producer, and
`crate::tick::run_tick_loop` already calls it once per tick and publishes
through `LiveMobSource` — the path `crate::server::sync_boss_bars` diffs
against a connection's last-sent set.

**No longer true — see below.** `crate::server`'s `travel_through_end_portal`
now calls `MobSim::init_end_dragon_fight` itself, the first time any
connection reaches a fresh End sibling.

## The End's own furniture, and how it gets placed on first arrival

`MobSim::init_end_dragon_fight(seed, origin, min_y) -> EndDragonFightInit`
(`mobs/dragon.rs`) is everything a join-path owner needs in one call: it
spawns all ten end crystals atop real, seed-derived spike positions
(`lodestone_worldgen::end::end_spikes_for_seed`), spawns the dragon itself
(`spawn_dragon`), and returns `EndDragonFightInit::block_writes` — every
obsidian/bedrock/iron-bars/podium block the arena needs, computed as pure
data by `lodestone_worldgen::end::{end_spike_blocks, end_podium}` (`crates/
lodestone-worldgen/src/end/{spikes,podium}.rs`). It places **zero** blocks
itself, matching this crate's existing "no block-write authority" contract
for `try_construct_wither`/`try_construct_golem`.

Landed in `travel_through_end_portal` (`crates/lodestone-server/src/server.rs`):

```rust
if destination.claim_dragon_fight_start() {
    let seed = crate::worldgen_data::active_world_seed();
    let init = mobs.with(|sim| {
        sim.init_end_dragon_fight(seed, Vec3::new(0.0, 64.0, 0.0), to.min_y())
    });
    for write in &init.block_writes {
        destination.set_block(write.x, write.y, write.z, &write.state);
    }
}
```

**`ChunkSource::claim_dragon_fight_start`** (`crates/lodestone-server/src/chunk.rs`)
is the "fresh" gate this doc used to say did not exist: an atomic
compare-exchange on a new `EndChunkSource` field, defaulted to `true`
("already claimed", the correct degradation) for every other source and
forwarded through `Arc<S>`, `DimensionalSource<S>`, `ChunkStore<S>` and
`RegionChunkSource<S>` so a real End sibling — always wrapped in at least the
first three — reaches the real override no matter how many layers deep it is
wrapped. Exactly one caller among any connections racing to the fresh End on
the same tick sees `true` and performs the init; the rest see `false` and do
nothing.

**This is a process-lifetime gate, not a persisted one** — `EndChunkSource`
carries no NBT-backed "already fought" flag, so a server restart re-arms it
and the pillars/podium/dragon/crystals are placed again on the next arrival.
This crate still has no `FightState`/`EnderDragonFight`-equivalent world
state to round-trip through a save (`dragon::fight::FightState` is ready to
receive one whenever that lands); a disclosed gap, not a silent one.

`origin` at `(0.0, 64.0, 0.0)` reproduces vanilla's own fixed
`BlockPos.ZERO` fight origin (`ServerLevel`'s `dragonFight.init(this, seed,
BlockPos.ZERO)`) at a plausible End-island y; a future pass should resolve
the true surface y the way vanilla's own `getHeightmapPos` does rather than
hardcoding `64`, since the main island's terrain height varies by seed.

## What a production wiring pass still needs from `protocol.rs`

Two items remain, both narrow:

- **`MetadataField::CrystalBeamTarget(Option<BlockPos>)`** —
  `EndCrystal.DATA_BEAM_TARGET`, index **8**, `OPTIONAL_BLOCK_POS`. The wire
  field exists and every crystal streams `CrystalBeamTarget(None)`
  (`mobs/end_crystal.rs`'s own doc); nothing yet computes a real target (the
  respawn sequence's summoning beam). Distinct serializer from every other
  index-8 claimant, so no dispatch ambiguity when a real producer lands.
- **No darken-screen bit on `BossBarSnapshot`**, the same gap
  `docs/wither-fight.md` names for the wither's own bar —
  `EnderDragonFight.init`'s `setCreateWorldFog(true)`/music flag have no
  carrier either.

## How to change it

- Each function cites the vanilla symbol it ports by class and method name,
  never a line number — the decompile under `.cache/mc/26.2/` gets
  re-extracted and lines move; re-verify against the current tree rather than
  trusting a comment's paraphrase.
- A divergence from vanilla is named at the point it happens, in a doc
  comment, not left implicit — `phase`'s pathfinding substitution and
  `fight`'s "no obsidian pillars" note are the two big ones; smaller ones
  (e.g. the sitting-scan aim cone only gating movement, not the
  attack-phase transition) are called out inline where a naive transcription
  would have gotten them wrong.
- If you add a phase transition or a new `dragon::fight` effect, add it to
  the relevant `tests` module as a **transition-table** assertion (a scripted
  input sequence asserting the resulting phase/stage *sequence*), not a
  single "some phase changed" assertion — see `phase::tests::full_landing_and_sitting_sequence`
  for the shape, including the `NeverZeroRng` control proving an RNG-gated
  transition is load-bearing rather than decorative.
- The exit-portal geometry (`exit_portal_blocks`) is order-sensitive: a
  caller applying the returned `Vec<(BlockPos, &str)>` in order gets the
  correct final state (the central bedrock pole is emitted *after* the main
  loop specifically so it overwrites the portal disc at that one column,
  matching vanilla's separate, later `setBlock` calls).

## Configuration

None — every constant (`DRAGON_SPAWN_Y = 128`, the 10-tick heal interval, the
100/40/100-tick respawn-stage timers, the `0.25 * max_health` sitting-damage
threshold) is a vanilla constant transcribed as a named `const` or inline
literal with its own doc comment citing the field it came from. There is no
config surface to extend without also changing the vanilla behaviour being
ported.

## Dependencies

- `lodestone_model::BlockPos` — the only external type this module uses,
  for exit-portal and respawn-crystal positions.
- Nothing else: no dependency on `lodestone-entity`'s goal/pathfinder stack,
  `lodestone-world`, or any `crates/protocol/*` crate. This is what makes the
  module buildable and testable independent of the concurrently-edited files
  named above.
