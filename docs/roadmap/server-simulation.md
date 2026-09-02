# Server simulation — the roadmap

**Scope:** everything about *simulating the world* server-side — chunk lifecycle, persistence, block
behaviour, redstone, world state, the tick loop, and the rest of server plumbing. Command execution
(Brigadier, selectors, `/execute`, functions/datapacks) is deliberately **not** re-decomposed here: it
already has its own issue, [#48](https://github.com/matteopolak/lodestone/issues/48), and a comment on
that issue lists the natural sub-scopes for whoever picks it up. Mob AI, pathfinding, breeding,
villagers, and raids belong to a sibling doc (`server-entities.md`) and a different agent's audit —
several of that audit's findings are cited below only where they correct a claim this doc's own research
first got wrong (see [Corrections](#corrections-mid-audit)).

This file is the *why this order*; the 46 issues below are the units. See
[epic #5](https://github.com/matteopolak/lodestone/issues/5) for the tracker itself, and the note under
[Epic capacity](#epic-capacity-a-real-constraint) for why not all 46 are attached to it directly.

## Foundations already in place

Worth internalising before estimating any of this: worldgen (noise router, density, carvers, surface,
aquifer, ore features) is bit-exact against JVM oracles; so are collision shapes (32,366 states),
hardness, entity dimensions, and block physics constants. A generated `path_types.rs` dumped from
vanilla's own pathfinding-node evaluator exists as groundwork for pathfinding (not this doc's concern, but it means the
mob-AI side isn't starting from zero either). `lodestone-server` exists as a real crate with a working
tokio target-split, an in-memory *and* TCP transport behind the same connection loop
(`crates/lodestone-server/src/integrated.rs`), and a real (if currently unwired) v770 server protocol
implementation (`V770ServerProtocol` in `crates/protocol/v770/src/server_protocol.rs`). NBT has a complete, tested
reader/writer in `crates/lodestone-core/src/lib.rs`. None of this is a green-field project.

What is *not* in place, confirmed by whole-tree search rather than assumed: Anvil region-file
persistence, any tick loop independent of client traffic, redstone (any component), fluid/growth/fire/
gravity/explosion block simulation, and every item in the World State phase below except difficulty
(which the client already decodes and displays, just doesn't own).

## Phase ordering and dependency edges

```
Phase 0 (server plumbing core)
  │
  ├──> Phase 1 (chunk lifecycle) ──────────────┐
  │                                             │
  ├──> Phase 2 (persistence) <──────────────────┘  (unloading needs somewhere to save to)
  │
  ├──> Phase 3 (block behaviour ticks) ──> Phase 4 (redstone family)
  │         │
  │         └──> gravity blocks, fire, fluid flow all consume Phase 3's
  │              scheduled-tick queue directly
  │
  ├──> Phase 5 (world state simulation)
  │         │  (time needs Phase 0's tick loop; sleep needs time + weather + gamerules;
  │         │   spawn-chunk-keep-loaded in Phase 1 needs world-spawn-point from here)
  │
  └──> Phase 6 (server plumbing, the rest)
            (autosave needs Phase 0 + Phase 2; RCON/query/ping/resource-pack-push/
             plugin-messaging are independent leaves — parallelizable with everything)
```

**Phase 0 is the one true prerequisite.** Three issues — a real tick loop, MSPT/TPS accounting, and
wiring the already-built `V770ServerProtocol` into the shell's singleplayer path — block nearly
everything downstream, either because there is no clock to schedule against yet, or because every other
feature in this epic needs a real client observing a real server to be verified against anything beyond
a closed-loop unit test. **File and land these first**, ahead of anything else in this document, even
though they read as unglamorous.

**Phase 1 and Phase 2 are mutually entangled at one edge, not fully ordered.** Chunk unloading (Phase 1)
is inert without persistence (Phase 2) to hand data to; conversely persistence's autosave (Phase 2) only
matters once there's a tick loop and a reason to save proactively rather than only on unload. Build the
region-file container and the ticket/status pipeline roughly together; sequence unloading and autosave
after both exist.

**Phase 3 before Phase 4 is not optional.** Every redstone component reads a power level that Phase 3's
neighbour-update-propagation issue establishes, and reacts to notifications that same issue defines the
shape of. Building any redstone component against ad-hoc, component-specific update logic (rather than
the shared propagation primitive) is exactly the kind of thing that "looks done" in isolation and then
needs a rewrite once the real primitive lands — see the piston sub-issue's own trap note, which is the
sharpest version of this risk in the whole epic.

**Phase 5 has the weakest internal ordering of any phase** — most of its eight issues are close to
independent of each other (time, weather, world border, gamerules, difficulty), with two real edges:
sleeping depends on time + weather + gamerules all landing first (a sleep skip is a coordinated jump in
all three), and the chunk-lifecycle spawn-chunk-keep-loaded issue (Phase 1) depends on world-spawn-point
(Phase 5) to know *where* to keep loaded. Multi-dimension support is the one issue in this phase that may
be much larger than it looks — see its own issue body for why (Nether/End worldgen may not exist yet at
all, which is a second large project hiding behind what reads like a plumbing issue).

**Phase 6 is the most parallelizable phase in the epic.** RCON, query, server-list-ping, resource-pack
push, and plugin-messaging channels share no state with each other or with anything upstream except
Phase 0's protocol wiring (they all need a real server connection to test against, nothing more).
Autosave and permission-storage are the two exceptions with real upstream dependencies (Phase 0+2, and
issue #48's command dispatcher, respectively).

## The issues, by phase

Each issue's number links to its GitHub page; the summary here is deliberately shorter than the issue
body, which carries the file:line evidence, the vanilla class/method citation, the traps, and the
verification method.

### Phase 0 — server plumbing core (build first)

| # | Issue | One-line reason it's first |
|---|---|---|
| [#284](https://github.com/matteopolak/lodestone/issues/284) | Real server tick loop (20 Hz) | Nothing else in this epic has a clock to run on without it |
| [#285](https://github.com/matteopolak/lodestone/issues/285) | MSPT/TPS accounting | Needs #284's loop to measure |
| [#287](https://github.com/matteopolak/lodestone/issues/287) | Wire `V770ServerProtocol` into the shell's singleplayer path — **island** | Every feature below needs a real client to verify against; `V770ServerProtocol` is built, tested, and has zero consumers today |

### Phase 1 — chunk lifecycle

| # | Issue |
|---|---|
| [#289](https://github.com/matteopolak/lodestone/issues/289) | Ticket/loading-priority system and the empty-to-full status pipeline |
| [#290](https://github.com/matteopolak/lodestone/issues/290) | View/simulation distance and re-streaming as a player moves |
| [#292](https://github.com/matteopolak/lodestone/issues/292) | Unloading and the save-on-unload hook |
| [#293](https://github.com/matteopolak/lodestone/issues/293) | Async, non-blocking chunk generation on the server connection loop |
| [#295](https://github.com/matteopolak/lodestone/issues/295) | Wire carver and ore-feature placement into the served chunk pipeline — **island**, and the cheapest high-visual-impact win in the whole epic (the math is already JVM-parity-tested; the whole gap is a wiring seam) |
| [#297](https://github.com/matteopolak/lodestone/issues/297) | Spawn-chunk keep-loaded ticket |

### Phase 2 — persistence

| # | Issue |
|---|---|
| [#298](https://github.com/matteopolak/lodestone/issues/298) | Anvil region file (.mca) reader/writer — reuses the existing NBT codec in `lodestone-core`, not starting from zero |
| [#300](https://github.com/matteopolak/lodestone/issues/300) | level.dat world metadata read/write |
| [#302](https://github.com/matteopolak/lodestone/issues/302) | Player data (.dat) read/write |
| [#303](https://github.com/matteopolak/lodestone/issues/303) | Per-chunk entity and point-of-interest (POI) storage |
| [#305](https://github.com/matteopolak/lodestone/issues/305) | Autosave scheduling and world upgrade / DataVersion handling |

### Phase 3 — block behaviour simulation

| # | Issue |
|---|---|
| [#307](https://github.com/matteopolak/lodestone/issues/307) | Random tick scheduler |
| [#308](https://github.com/matteopolak/lodestone/issues/308) | Scheduled-tick queue and neighbour-update propagation — **the load-bearing issue of Phases 3–4** |
| [#309](https://github.com/matteopolak/lodestone/issues/309) | Fluid flow simulation (water and lava spread) |
| [#310](https://github.com/matteopolak/lodestone/issues/310) | Crop growth, sapling growth, and leaf decay |
| [#311](https://github.com/matteopolak/lodestone/issues/311) | Gravity blocks (sand, gravel, anvils, concrete powder) |
| [#312](https://github.com/matteopolak/lodestone/issues/312) | Fire spread and burnout |
| [#313](https://github.com/matteopolak/lodestone/issues/313) | Explosion block-destruction and blast resistance — complements [#213](https://github.com/matteopolak/lodestone/issues/213) (entity-exposure/damage, a different crate, already built) rather than duplicating it |

### Phase 4 — redstone family (8 sub-issues, nested under one parent)

| # | Issue |
|---|---|
| [#314](https://github.com/matteopolak/lodestone/issues/314) | **Parent.** Signal propagation for dust and torches |
| [#315](https://github.com/matteopolak/lodestone/issues/315) | Repeaters and comparators |
| [#316](https://github.com/matteopolak/lodestone/issues/316) | Pistons, including vanilla's update-order quirks — the highest-risk single issue in this phase |
| [#317](https://github.com/matteopolak/lodestone/issues/317) | Observers |
| [#318](https://github.com/matteopolak/lodestone/issues/318) | Powered and detector rails |
| [#319](https://github.com/matteopolak/lodestone/issues/319) | Redstone-openable blocks: doors, trapdoors, fence gates |
| [#320](https://github.com/matteopolak/lodestone/issues/320) | Dispensers and droppers |
| [#321](https://github.com/matteopolak/lodestone/issues/321) | Hoppers |
| [#322](https://github.com/matteopolak/lodestone/issues/322) | Note blocks, tripwire hooks, and target blocks |

### Phase 5 — world state simulation

| # | Issue |
|---|---|
| [#323](https://github.com/matteopolak/lodestone/issues/323) | Time simulation and the daylight cycle |
| [#324](https://github.com/matteopolak/lodestone/issues/324) | Weather simulation (rain and thunder state machine) |
| [#325](https://github.com/matteopolak/lodestone/issues/325) | Sleeping and the night-skip vote |
| [#326](https://github.com/matteopolak/lodestone/issues/326) | World border: server-authoritative state and enforcement |
| [#327](https://github.com/matteopolak/lodestone/issues/327) | Game rule storage and enforcement — **island**: `GameRulesChanged` already decodes client-side and has zero consumers |
| [#328](https://github.com/matteopolak/lodestone/issues/328) | Difficulty storage and enforcement |
| [#329](https://github.com/matteopolak/lodestone/issues/329) | World spawn point and per-player respawn points |
| [#330](https://github.com/matteopolak/lodestone/issues/330) | Multi-dimension support and server-driven portal travel — possibly much larger than it reads; check Nether/End worldgen exists before estimating |

### Phase 6 — server plumbing (the rest)

| # | Issue |
|---|---|
| [#331](https://github.com/matteopolak/lodestone/issues/331) | RCON listener (our server hosting one; the existing `RconClient` is the opposite direction — a test tool that drives *vanilla* oracles) |
| [#332](https://github.com/matteopolak/lodestone/issues/332) | Query protocol (GameSpy4/UT3) — lowest player-facing value in the epic; fine to defer |
| [#333](https://github.com/matteopolak/lodestone/issues/333) | Server list ping responder (existing `ping.rs` only pings *other* servers) |
| [#334](https://github.com/matteopolak/lodestone/issues/334) | Server-initiated resource pack push |
| [#335](https://github.com/matteopolak/lodestone/issues/335) | Plugin messaging channel registry and dispatch |
| [#336](https://github.com/matteopolak/lodestone/issues/336) | Ops, whitelist, bans, and permission levels |
| [#337](https://github.com/matteopolak/lodestone/issues/337) | Loot table loading and rolling |
| [#338](https://github.com/matteopolak/lodestone/issues/338) | Advancements and statistics: server-side tracking |

## Islands found

Confirmed built-and-tested-but-zero-consumer code, labelled `island` on the relevant issue:

- **`V770ServerProtocol`** (`crates/protocol/v770/src/server_protocol.rs`) — a real protocol-776
  server implementation, exercised only by its own crate's tests. [#287](https://github.com/matteopolak/lodestone/issues/287).
- **Carvers and ore-feature placement** (`crates/lodestone-worldgen/src/carver/`, `src/feature/mod.rs`) —
  JVM-parity-tested, never composed into `OverworldGenerator`. [#295](https://github.com/matteopolak/lodestone/issues/295).
- **`GameRulesChanged`** (decoded from `GAME_RULE_VALUES` in the v770 adapter) — decoded, lowered, and
  dropped with no consumer; the serverbound `SET_GAME_RULE` is unhandled entirely.
  [#327](https://github.com/matteopolak/lodestone/issues/327).

Two more were found but belong to the mob-AI/entity domain, not this one, and are filed by that audit
rather than here — noted for completeness since this doc's own research tripped over them:

- `crates/lodestone-entity/src/explosion.rs` (entity-exposure/knockback math for a blast, zero
  consumers) — [#213](https://github.com/matteopolak/lodestone/issues/213).
- `MobSim`'s `!Send` `Goal` trait blocking it from the real entity-streaming path
  (`SharedSnapshotSource`'s own doc comment in
  `crates/protocol/v770/tests/entity_streaming_live.rs` documents this explicitly) —
  [#217](https://github.com/matteopolak/lodestone/issues/217) covers the consequence (mob positions
  never reach a client); the root cause is a `Send` bound on `lodestone_entity::ai::Goal`, flagged
  separately as a background task rather than filed here since fixing it means touching entity-AI code
  this epic does not own.

## Corrections mid-audit

Two claims in this doc's own first-draft research turned out to be wrong, caught by cross-referencing a
concurrent mob-AI/entity audit that happened to touch the same files from a different angle — worth
recording per this repo's own standing rule about stale claims being the most expensive defect class
here:

1. **"Explosions are entirely absent" was wrong.** A scoped grep for `Explosion\b` missed
   `crates/lodestone-entity/src/explosion.rs` because it lives in a crate outside the paths that grep
   covered. The entity-exposure/damage half of explosions is built and tested (see
   [#213](https://github.com/matteopolak/lodestone/issues/213) above); what is actually absent is the
   *block-destruction* half — which blocks a blast destroys, and the blast-resistance data that decides
   it. [#313](https://github.com/matteopolak/lodestone/issues/313) is scoped to that corrected, narrower
   gap.
2. **"No time concept exists anywhere" was wrong.** A `WorldTime` bevy resource is real, tested, and
   actively used throughout `lodestone-client` and `lodestone-ecs`. What is actually absent is any
   *server-side* ownership of it — `lodestone-server` has zero dependency on `lodestone-ecs` at all, so
   it cannot be advancing, reading, or broadcasting that resource. [#323](https://github.com/matteopolak/lodestone/issues/323)
   is scoped to that corrected claim, and should reuse `WorldTime`'s shape as its wire-facing model
   rather than invent a second one.

The general lesson, consistent with this repo's own recorded experience elsewhere: a grep scoped to the
crates you *expect* an answer to live in is not evidence of absence — the producer can be one crate over
from where you looked. Both corrections above were made before filing, not after; had they shipped as
written, they would have duplicated already-built work in one direction (explosions) and asserted a
false absence in the other (time).

## Epic capacity — a real constraint

GitHub caps a parent issue at **100 sub-issues**. [Epic #5](https://github.com/matteopolak/lodestone/issues/5)
is shared across every Tier-4 audit running concurrently in this repo (mob AI/pathfinding, entities,
this doc's own scope, and others), and it hit that cap partway through filing this doc's own 46 issues.
The redstone family's 8 sub-issues were nested under their own parent
([#314](https://github.com/matteopolak/lodestone/issues/314)) rather than flatly under #5, which both
matches what each sub-issue's body already claimed and freed 7 slots — used to attach 7 more issues
before the cap closed again. **Nine issues from this doc remain without a GitHub parent link**: #330
(multi-dimension) and all of Phase 6 except none — specifically #331–#338. They are still fully labelled
(`feature`/`tier-4`/`area/server`/`roadmap`), auto-added to the project board, and referenced correctly
by number in this document and in each other's bodies; they are simply not in the sub-issue graph under
#5. If the epic's sub-issue count needs to come down further, the natural fix is what was done for
redstone here: promote one issue per remaining phase (persistence, world state, server plumbing) to a
phase-level parent and nest its siblings under that instead of under #5 directly — but that is a
repo-organisation decision, not one this doc makes unilaterally.
