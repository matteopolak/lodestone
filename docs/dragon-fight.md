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
  portal activation, gateway spawn), `boss_bar_value` (the `progress`/
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

**No obsidian pillars anywhere in this repo.** `docs/worldgen-end.md` already
says the pillars (`EndSpikeFeature`) and the exit portal
(`EndPodiumFeature`) are "structure/entity work" with "a gameplay placer"
rather than terrain generation — and that gameplay placer for the pillars has
never been written, by this change or any other. Concretely:

- Crystal healing and the fight's crystal count do not require pillars — a
  crystal in this world is wherever a caller places one, not standing atop a
  40-80-block spike.
- The "caged vs. uncaged crystal" distinction issue #276 names
  (`EndSpikeFeature`'s `guarded` flag wraps a short pillar's crystal in iron
  bars) has no pillars to attach cages to and is not modelled. There is
  nothing to cage.
- `tick_respawn`'s `SummoningPillars` stage is ported as a state machine
  **parameterized by a spike count**, so it stays correct if pillar placement
  lands later. Called with the honest count in a world with none
  (`spike_count = 0`), the formula **correctly degenerates**: `index <
  spike_count` is `0 < 0`, always false, so the stage advances to
  `SummoningDragon` on its first tick rather than stalling — this is vanilla's
  own formula evaluated at the true input, not a stub. See
  `fight::tests::respawn_stage_summoning_pillars_degenerates_instantly_with_zero_spikes`.

**No `BOSS_EVENT` wire packet.** Issue #276 draws this line itself ("this
crate's job is the phase/health state, not the bar widget"): `boss_bar_value`
computes the value; nothing here sends it. `ServerProtocol` (in
`crates/lodestone-server/src/protocol.rs`) has no boss-event encoder today —
see "What a production wiring pass still needs" below.

**No live entity, no ticking, no streaming.** This change adds no `SimMob`,
no `MobSim` field, and no `EntitySnapshot` producer. See "What consumes this
today" below — the honest answer is "nothing in production yet".

## What consumes this today

**Nothing in production.** This module is self-contained and unit-tested but
not wired into `crate::mobs::MobSim` — there is no dragon or end-crystal
entity a real server spawns, and `MobSim::tick`/`MobSim::snapshots` do not
call into `dragon::phase`/`dragon::crystal`/`dragon::fight` anywhere. This is
named explicitly rather than left implicit (`CLAUDE.md`'s island rule): the
phase state machine, crystal healing and fight controller are real,
individually tested, and reach zero pixels until a follow-up wires them into
`MobSim` as a new tracked-entity kind (the `TrackedTnt`/`TrackedMinecart`
pattern — a plain struct in a `HashMap<i32, _>`, ticked in `MobSim::tick`,
appended in `MobSim::snapshots`, **not** a full goal-driven `SimMob`, since a
dragon's flight is not ground pathfinding). That wiring was not attempted in
this change because `crates/lodestone-server/src/mobs/mod.rs` is a large,
concurrently-edited shared file and the pure-module split above is real,
substantial, independently valuable work that does not need to wait for it —
see `HANDOFF.md`/the issue tracker for the follow-up.

## What a production wiring pass still needs from `protocol.rs`

`crates/lodestone-server/src/protocol.rs` and the v770 adapter were held for
a concurrent edit while this change was written, so nothing here touches
them. A future pass needs:

- **`MetadataField::DragonPhase(i32)`** — `EnderDragon.DATA_PHASE`, wire
  index **16**, `INT` serializer (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`).
  Index 16 is shared by many other `INT`/non-`INT` fields across species
  (`Creeper.DATA_SWELL_DIR`, `WitherBoss.DATA_TARGET_A`, ...), all resolved
  the same way every other crowded index in this enum already is: the
  *producer* (a species switch in whatever ticks the dragon) only ever emits
  this variant for a `minecraft:ender_dragon` entity, so there is no true
  collision at the wire.
- **`MetadataField::CrystalBeamTarget(Option<BlockPos>)`** —
  `EndCrystal.DATA_BEAM_TARGET`, index **8**, `OPTIONAL_BLOCK_POS`. Distinct
  serializer from every other index-8 claimant, so no dispatch ambiguity.
- **`MetadataField::CrystalShowBottom(bool)`** — `EndCrystal.DATA_SHOW_BOTTOM`,
  index **9**, `BOOLEAN`. Defaults to `true` in vanilla
  (`DEFAULT_SHOW_BOTTOM`); needed only if a crystal is ever rendered floating
  (a respawn-summoned one has no bottom slab).
- **A `BOSS_EVENT` encoder** — `ServerProtocol` has no `encode_boss_event` (or
  equivalent) method today; `BOSS_EVENT` (packet id 9 in
  `crates/protocol/v770/src/generated/packet_ids.rs`) is currently
  **decode-only** (client-side parsing of a real server's boss bar), with no
  server-side encode path anywhere in this crate. `dragon::fight::boss_bar_value`
  produces exactly the `progress`/`visible` pair such an encoder would need;
  color (`PINK`) and overlay (`PROGRESS`) never change in vanilla
  (`EnderDragonFight.init`) and can be hardcoded at the call site.

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
