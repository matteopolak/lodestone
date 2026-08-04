# Plugin framework: the capability audit

## What this is

The decomposition behind epic [#77](https://github.com/matteopolak/lodestone/issues/77):
a capability-by-capability audit of what a real Bukkit/Paper/Fabric plugin does, checked
against what a native `bevy_app::Plugin` (and, where it exists only on paper today, the
WASM host) can do in this codebase *right now*, with a gap and an issue number attached
to every row that is not fully closed. Epic [#7](https://github.com/matteopolak/lodestone/issues/7)
owns the ECS substrate that makes any of this possible
([`../bevy-migration.md`](../bevy-migration.md), [`../world-unification.md`](../world-unification.md));
this doc and its 49 sub-issues own whether that substrate adds up to **capability parity**
with the Java ecosystem, which is a different and harder question than "does the ECS
exist."

**The claim under test, stated so it can be falsified:** *any Bukkit/Paper/Fabric plugin
should be portable to this framework — not approximately.* That is not a wish list, it is
a test with a pass/fail per capability, and the honest answer (§"Verdict" below) is that
it does not pass today, in one specific and non-negotiable way.

## Method

Every capability below was checked against the actual tree, not against what a design doc
says should exist — the two disagree in one important place (§"A stale claim found and
fixed" below). Sources read in full: [`../plugin-api.md`](../plugin-api.md),
[`../bevy-migration.md`](../bevy-migration.md), [`../world-unification.md`](../world-unification.md),
[`../entity-components.md`](../entity-components.md), [`../local-player-components.md`](../local-player-components.md),
[`../session-components.md`](../session-components.md), `crates/lodestone-ecs/src/{sets,schedules,player,session}.rs`,
`crates/lodestone-model/src/{adapter,action}.rs`, `crates/plugins/lodestone-nav` (the one
real, 75-test clean-room plugin), and the existing issue tracker (`gh issue list --state
all --limit 200`, to avoid duplicating #20, #35, #36, #37, #38, #46, #48, #67, all of
which this doc references rather than re-files).

## A stale claim found and fixed

`docs/plugin-api.md`'s own "four concrete gaps" section, at the time this audit started,
stated that the `TickSet::Intent` ordering anchor and a `LookIntent` component distinct
from the camera were **still missing**, each backed by a `grep` that returned empty.
Re-running those same greps against the current tree returns real hits: `TickSet` has six
variants today (`Input, Intent, Physics, Predict, Animate, Send`), and
`crates/lodestone-ecs/src/player.rs` defines `LookIntent` with the
insert-to-take-control/remove-to-release idiom already established for other intent
components. Both landed in commit `0d82ab4` ("feat: close the ingest seam, three plugin
ABI pieces, and Tier-1 wiring"), whose own message names closing exactly these two items —
the commit that fixed the gap knew it was fixing a documented gap, and the doc was never
updated. Filed as [#180](https://github.com/matteopolak/lodestone/issues/180)
(`stale-record`), and this table below reflects the *current* tree, not the stale doc.
This is exactly the failure class `CLAUDE.md` names as the most expensive in this repo —
true when written, false since, and not wrong-looking on inspection — and it directly
changed this audit's verdict on ordering-anchor coverage from "gap" to "closed."

## The capability audit

Status legend: **done** (real, verified against the tree) · **partial** (some of the
capability exists, concretely stated what's missing) · **gap** (nothing exists) ·
**ceiling** (will not exist by design; stated why).

### Events

| Java capability | our status | gap | issue |
|---|---|---|---|
| `@EventHandler` subscription to a typed event | gap | no `Message`/observer a plugin can read; only component *effects* are observable via `Query` | [#104](https://github.com/matteopolak/lodestone/issues/104) |
| Raw packet visibility (ProtocolLib-class, receive side) | partial | `RawPacket` message specified (`bevy-migration.md` §5.1), never built | [#104](https://github.com/matteopolak/lodestone/issues/104) |
| Cancellation (`setCancelled`) | gap | **the hardest design question in this epic** — no mechanism to veto an in-flight effect in a schedule that already ran to completion | [#101](https://github.com/matteopolak/lodestone/issues/101) (design) |
| Cancellation of the concrete high-value verbs (break/place/damage/click/move) | gap | depends on #101 | [#109](https://github.com/matteopolak/lodestone/issues/109) |
| `EventPriority` (LOWEST..MONITOR) | gap | ordering anchors today are internal `SystemSet`s, which don't let two *unrelated* plugins agree on order | [#105](https://github.com/matteopolak/lodestone/issues/105) (design) |
| MONITOR (guaranteed-last, read-only) | gap | depends on #105 | [#110](https://github.com/matteopolak/lodestone/issues/110) |
| Custom/plugin-defined events | partial | any bevy `Message` type already works for two plugins compiled into one binary; no documented convention or worked example | [#107](https://github.com/matteopolak/lodestone/issues/107) |
| ~400 Paper event types | gap (by extension) | not enumerable as one issue; each is an instance of the event-bus + cancellation primitives above once those exist | tracked via #101/#104/#109 |

### Scheduler

| Java capability | our status | gap | issue |
|---|---|---|---|
| `runTaskLater` / `runTaskTimer` | gap | only the fixed 20 Hz `GameTick`; no delayed/repeating primitive | [#113](https://github.com/matteopolak/lodestone/issues/113) |
| `runTaskAsynchronously` + main-thread hand-back | partial | the pattern exists once, hand-built, for `lodestone-nav`'s search (owned snapshot, dedicated thread, never touches the `World` lock); no general API | [#114](https://github.com/matteopolak/lodestone/issues/114) |
| Folia-style region threading | **ceiling, permanently** | one `World`, one thread, one clock, by design (`world-unification.md`) — not a gap, a decided permanent contract; see "Decision records" below | [#116](https://github.com/matteopolak/lodestone/issues/116) (closed) |

### Commands

| Java capability | our status | gap | issue |
|---|---|---|---|
| Register a plugin command | gap | client can send arbitrary command *text* (`ClientAction::SendCommand`); no registry a plugin adds a node to, on either tier | [#118](https://github.com/matteopolak/lodestone/issues/118) |
| Argument types + tab completion | gap | no parser, no suggestion-protocol consumer | [#119](https://github.com/matteopolak/lodestone/issues/119) |
| Permission per command node | gap | depends on both #118 and the permission system below | [#122](https://github.com/matteopolak/lodestone/issues/122) |
| `/execute` interop for plugin commands | blocked | depends on #48 (server-side Brigadier dispatcher, Tier 4, not this epic's to build) | [#123](https://github.com/matteopolak/lodestone/issues/123) |

Note: vanilla command UX (#46) and the vanilla dispatcher (#48) are **not** duplicated
here — they are the non-plugin surface. #118–123 are the plugin *extension point* into
whatever #46/#48 eventually build, and were scoped explicitly to share an argument-type
library with #48 rather than diverge.

### Permissions

| Java capability | our status | gap | issue |
|---|---|---|---|
| Permission nodes, wildcards, defaults | gap | `grep -rli permission crates/` finds only OS-keychain strings — **no Minecraft-shaped permission concept exists anywhere**, client or server | [#125](https://github.com/matteopolak/lodestone/issues/125) |
| Op-level / per-player / group resolution | gap | depends on #125 | [#127](https://github.com/matteopolak/lodestone/issues/127) |
| Delegating to a permissions plugin (the real-world default) | gap | no resolver-trait seam exists to delegate to | folded into #125's design |

This is the single largest *pure* gap in the whole audit: not partial, not "exists but
unwired" — genuinely absent, and load-bearing for four other issues in this epic (#12,
#118's node-gating, and any protection/economy-style archetype in the port-feasibility
section below).

### World and block access

| Java capability | our status | gap | issue |
|---|---|---|---|
| Get block (with/without version lock) | **done** | `VersionAdapter::{block_collision, block_name, block_outline, block_interaction}`, `lodestone_model::block_physics` — real, closed in `24af787` | — |
| Set block, with/without physics | gap | no write API at all; only the shell's own non-plugin-reachable `Sim::set_block_world` for the offline demo world, and no physics/neighbor-update pipeline exists to feed the `physics: true` path into (Tier 4) | [#129](https://github.com/matteopolak/lodestone/issues/129) |
| Bulk edits (WorldEdit-class) | gap | depends on #129; scoped to a batched-write primitive only — undo/redo is the *plugin's* problem on real Paper too | [#131](https://github.com/matteopolak/lodestone/issues/131) |
| Custom world generator / biome provider | gap | `lodestone-worldgen` is deliberately not a system (bit-exact oracle, `bevy-migration.md` §8); whether a plugin seam is even compatible with that guarantee is an open design question | [#132](https://github.com/matteopolak/lodestone/issues/132) (design) |
| Custom dimension registration | gap | blocked on a dimension-type registry that doesn't exist yet (also the root cause of a known-broken sky-light bug in `docs/backlog.md`) | [#134](https://github.com/matteopolak/lodestone/issues/134) |
| Structure placement | gap | `lodestone-worldgen` has no structure concept at all yet — Tier-4 scope, not plugin-API scope | [#136](https://github.com/matteopolak/lodestone/issues/136) (parked) |

### Entity manipulation

| Java capability | our status | gap | issue |
|---|---|---|---|
| Modify an existing entity (position, health, equipment, ...) | **done** | real, plugin-writable components, reach the screen next `Extract` | — |
| Spawn / despawn | gap | every ECS entity today arrives via an ingest fold of a server packet; no `spawn_entity`/`despawn_entity` a plugin can call | [#138](https://github.com/matteopolak/lodestone/issues/138) |
| Custom entity types | gap | the wire protocol has no room for a novel entity type either (same ceiling vanilla itself has); scoped to a disguise-as-vanilla-type registry | [#140](https://github.com/matteopolak/lodestone/issues/140) |
| Attribute modification | partial (needs verification) | `Attributes` is plugin-writable; whether a write reaches the wire the way `Position` does is unverified | [#141](https://github.com/matteopolak/lodestone/issues/141) |
| AI-goal manipulation | gap | **no AI exists at all** — Tier 4, "plausibly larger than Tiers 1-3 combined" per `docs/backlog.md`; there is no goal selector to add a goal to | [#141](https://github.com/matteopolak/lodestone/issues/141) (design) |
| NBT / DataComponent read-write | partial | item component patches exist and are read; plugin write-path unaudited | [#143](https://github.com/matteopolak/lodestone/issues/143) |
| Disguises (packet-level) | gap | depends on the packet-interception design (#156) resolving in the plugin's favour — see Packets below | tracked via #156 |

### Inventories and items

| Java capability | our status | gap | issue |
|---|---|---|---|
| Custom inventory/menu (`createInventory`) | gap | `SessionMenus` is read-only, folded from real server packets; no synthetic-open path. Client-side-only menus (no server round trip) are the cheap 80% here | [#145](https://github.com/matteopolak/lodestone/issues/145) |
| Custom items / item components | partial | same wire-id ceiling as custom entities; scoped to a vanilla-item-plus-tag pattern | [#147](https://github.com/matteopolak/lodestone/issues/147) |
| Runtime recipe registration | gap | recipe set is loaded once at startup from the vanilla corpus (`docs/crafting.md`); no `addRecipe` | [#148](https://github.com/matteopolak/lodestone/issues/148) |
| Anvil/loom/smithing hooks | blocked | the stations themselves are unbuilt Tier-2 container screens; nothing to hook | [#150](https://github.com/matteopolak/lodestone/issues/150) (parked) |

### Persistence

| Java capability | our status | gap | issue |
|---|---|---|---|
| `PersistentDataContainer` / metadata | gap | no per-entity/per-chunk key-value store of any kind | [#152](https://github.com/matteopolak/lodestone/issues/152) |
| Surviving a restart | blocked | world persistence (Anvil format) is itself unbuilt Tier-4 work; the in-memory half (parity with Bukkit's `Metadatable`) is unblocked and should ship now | [#152](https://github.com/matteopolak/lodestone/issues/152) |
| Plugin config files / data directory | gap | no convention; should reuse whatever #67 (existing issue, data-dir de-duplication) settles rather than adding a third implementation | [#153](https://github.com/matteopolak/lodestone/issues/153) |
| Database access from a plugin | **done, trivially** | native tier is unrestricted `std`; a plugin can already open a SQLite file or a Postgres connection like any Rust program. No issue needed. | — |

### Packet-level access

| Java capability | our status | gap | issue |
|---|---|---|---|
| ProtocolLib-class read/modify/cancel/inject, inbound | gap | `RawPacket` (read-only, off by default) is specified, unbuilt; mutation/cancellation raises a genuine reentrancy hazard (the net thread applies events **inline**, under `ecs.write()`) | [#156](https://github.com/matteopolak/lodestone/issues/156) (design) |
| Outbound mutation/cancellation | gap | `ActionQueue` is append-only; no interception point before the wire | [#157](https://github.com/matteopolak/lodestone/issues/157) |

### The escape hatch

| Java capability | our status | gap | issue |
|---|---|---|---|
| NMS/internals for whatever the plugin API doesn't cover | **done, and better than Java's** | a plugin may depend on a version crate directly (it's a leaf crate) — version-locks it exactly like NMS reflection version-locks a Paper plugin, except ours is a compile-time `Cargo.toml` fact, not a runtime `ClassNotFoundException` | [#159](https://github.com/matteopolak/lodestone/issues/159) (docs only — no code needed) |

### Client-side plugin surface

| Java capability | our status | gap | issue |
|---|---|---|---|
| World-space custom rendering (waypoints, overlays) | partial | exactly one real instance: `ExtractSet::Debug` + `DebugLines`, landed in `0d82ab4` — a debug line pipeline, not general-purpose | [#161](https://github.com/matteopolak/lodestone/issues/161) |
| Input interception | partial | the precedent (chat/container screens swallowing keys before gameplay) already exists in `resolve_key`'s precedence chain; no plugin-facing slot in it | [#162](https://github.com/matteopolak/lodestone/issues/162) |
| Camera control | partial (needs verification) | third-person toggle is real and shipped; whether a plugin can *drive* the pose (spectator/cinematic) rather than only observe it is unaudited | [#164](https://github.com/matteopolak/lodestone/issues/164) |
| Custom shaders / replace the render pipeline | **ceiling, by design** | `lodestone-render` carries no bevy dependency and never will (4-bind-group floor, winding-sign invariant); a plugin never gets a `wgpu::Device` | [#165](https://github.com/matteopolak/lodestone/issues/165) (docs the ceiling) |

### Lifecycle and tooling

| Java capability | our status | gap | issue |
|---|---|---|---|
| Manifest + load order + dependencies | gap | "install a plugin" means "add a `Cargo.toml` dependency and rebuild"; Cargo gives crate-level resolution for free, `add_plugins` ordering and soft-deps do not exist as a convention yet | [#166](https://github.com/matteopolak/lodestone/issues/166) (design) |
| Error isolation (one bad plugin ≠ dead server) | gap, and possibly a **ceiling** | `catch_unwind` around a system risks leaving the `World` in a half-mutated state, arguably worse than crashing; may be a documentation answer ("a plugin panic is exactly as fatal as an internal one, by design — the trust model already says fully trusted") rather than a mechanism | [#168](https://github.com/matteopolak/lodestone/issues/168) (design) |
| Hot reload | **ceiling** | Rust has no stable ABI across compiler versions; a reloaded `.so` gets different `TypeId`s for "the same" component, silently breaking every `Query`. Not achievable for the native tier as designed — an argument *for* prioritizing WASM if this is a real requirement | [#169](https://github.com/matteopolak/lodestone/issues/169) (docs the ceiling) |
| Versioned ABI / what breaks across versions | partial | the policy is written down in prose (`plugin-api.md`); nothing enforces it | [#170](https://github.com/matteopolak/lodestone/issues/170) |

### Native vs. WASM (both tiers must express the same features)

| Java capability | our status | gap | issue |
|---|---|---|---|
| WASM host existing at all | gap | `grep -rli wasmtime crates/` — nothing; `DESIGN.md` calls it "deferred to Phase 8"; `bevy-migration.md` §6.1: "not started — no crate, no design doc yet" | [#172](https://github.com/matteopolak/lodestone/issues/172) |
| Capability ABI (queries + actions) | gap | described in one sentence in two docs, never designed; the pathfinder cost analysis in `plugin-api.md` already shows the naive shape (every query = a host call) fails badly | [#173](https://github.com/matteopolak/lodestone/issues/173) |
| Manifest / capability declaration | gap | depends on #172/#173 existing first | [#175](https://github.com/matteopolak/lodestone/issues/175) |
| Verified sandbox (the actual selling point of this tier) | gap | no gate, and per this repo's own evidence standard, an untested "untrusted-safe" claim needs a negative control that proves the sandbox would be seen if broken | [#176](https://github.com/matteopolak/lodestone/issues/176) |

### The correctness constraint underneath all of it

| Concern | our status | gap | issue |
|---|---|---|---|
| `EcsHandle` reentrancy — detected | **done, partially** | `hold_read`/`hold_write` panic instead of hanging; ledger only sees guards taken through those two functions ([#20](https://github.com/matteopolak/lodestone/issues/20), tracked separately for `lodestone-client`'s own ~12 direct call sites) | — |
| `EcsHandle` reentrancy — **unrepresentable** for a plugin author who never read the docs | gap | this is the brief's own top-priority ask, and it is a real design question with no clean answer yet (a plugin can always `Arc::clone` around any wrapper) | [#177](https://github.com/matteopolak/lodestone/issues/177) (design) |
| A test harness a third-party plugin author can run against their own plugin | gap | one bespoke test (`mining_deadlock.rs`) pins one historical bug; nothing reusable | [#179](https://github.com/matteopolak/lodestone/issues/179) |
| `ActionQueue` (shipped) vs. `MessageWriter<SendAction>` (specified) | open question | both work; the shape decision affects every downstream outbound-action issue (#157, #109) | [#181](https://github.com/matteopolak/lodestone/issues/181) (design) |

## Decision records

Closed design questions, written down once so they are not re-derived. Each quotes the owner's own
words rather than paraphrasing them, per this repo's own standard for what counts as a decision on
record.

### #116 — Folia-style region threading is not a goal; closed

[#116](https://github.com/matteopolak/lodestone/issues/116) asked, as a decision record rather
than code: is region-based parallelism ever a goal for this project, and if not, say so on the
record so nobody reopens it. The answer already existed, split across two sibling issues in the
same epic, and this section is that answer collected in one place.

**[#341](https://github.com/matteopolak/lodestone/issues/341)'s scope decision** (Java plugin
compatibility, targeting Paper) states it directly, as the owner's own issue comment:

> **Do not target Folia.** It is a separate fork with regionised multi-threading, and its
> threading model conflicts directly with our single `RwLock<World>` — see the reentrancy
> constraint in the issue body, which is already the hardest part of this design. Folia would
> multiply it rather than help.

That settles the *plugin-compatibility* question: a Folia-authored plugin that assumes
region-sharded scheduling is out of scope, the same way a plugin too old for modern Paper is out of
scope by the same issue's own reasoning.

**[#342](https://github.com/matteopolak/lodestone/issues/342) (regionised server ticking, filed as
a later performance track) records the internal counterpart**, and is explicit that the two
statements are not in tension:

> #341 says do **not** target Folia as a *plugin-compatibility* target. That is a different
> statement, and both hold:
>
> - **Supporting Folia's plugin threading contract** — plugins written to assume regionised
>   scheduling. Out of scope; it multiplies #341's hardest problem.
> - **Our own server adopting regionised ticking** — an internal performance architecture. This
>   issue, and legitimate.
>
> They interact in a useful direction: our single `RwLock<World>` is what blocks both today. If
> this lands, Folia-style plugin threading becomes *possible* to reconsider, where today it is
> structurally excluded.

So: two separate axes, correctly not conflated. **This project may, later and for measured
performance reasons (§342's own sequencing: tick loop → single-threaded parity → benchmarks →
profile → only then decide), regionise its own server tick loop internally.** That is unrelated to
whether this project ever backs Folia's plugin-facing threading contract, which it will not.

**Reaffirmed as permanent for the plugin ABI, independent of whether #342 ever lands:** one
`bevy_ecs::World`, one `GameTick` schedule, one 20 Hz accumulator
(`docs/world-unification.md`'s §4.1(c): "It now holds **one** [`World`]... and that one `World`
carries **one** `GameTick` schedule driven by **one** 20 Hz accumulator"). Every clause in this
doc's doctrine — and every clause in [`../plugin-api.md`](../plugin-api.md)'s intent doctrine —
is written assuming a single writer, a single schedule, and a single ordered tick. §342's own
"what it costs" section is explicit that regionisation would need either one `World` per region or
a provably-partitioned single `World`, and that "global ordering disappears" is one of the costs,
not a side effect it avoids. If #342 ever lands, it changes the **server's own** internal ticking
architecture; it does not retroactively change what a plugin can assume about the client-side
`GameTick` this framework is built on, and it does not reopen #341's Folia answer. A contributor
reading #342 as a reason to revisit either should read this record first.

**Closed:** [#116](https://github.com/matteopolak/lodestone/issues/116), pointing here.

## Port-feasibility analysis

Eight real, well-known Paper/Fabric plugin archetypes, scored against the audit above —
this is the test that catches gaps an API-shaped enumeration misses, because a real
plugin needs several capabilities *at once*, in a specific combination.

| archetype | needs | verdict today | verdict once this epic's issues land |
|---|---|---|---|
| **Protection plugin** (WorldGuard-class: claim a region, veto breaks/places/PvP inside it) | block-break/place cancellation, permission nodes, persistent per-chunk region data | **not portable** — no cancellation mechanism (#101/#109), no permission system (#125), no persistence (#152) | portable, and this is the one archetype whose gaps are *entirely* inside this epic's own scope — no Tier-4 dependency |
| **Economy plugin** (Vault-class: per-player balance, transaction events) | persistent per-player data, custom events other plugins subscribe to, commands with permissions | **not portable** — no persistence, no custom-event convention (#107), no command API (#118) | portable for the in-memory half immediately; balance *surviving a restart* is blocked on world/player persistence existing at all (Tier 4), same ceiling as PDC generally |
| **Minigame plugin** (a lobby/countdown/arena manager) | scheduler (delayed/repeating tasks), custom inventories (kit-select GUI), command registration, cancellation of movement/damage during specific phases | **not portable** — scheduler (#113), custom menus (#145), commands (#118), cancellation (#101) are all missing | portable — this archetype touches no ceiling capability at all, making it (with the protection plugin) the strongest candidate for a first real ported example once the P0 items below land |
| **World editor** (WorldEdit-class) | bulk block read/write with undo, region selection, its own command tree | **not portable** — no block-write API at all yet (#129), let alone batched (#131) | portable for the batched-write primitive; undo/redo and region selection are (correctly) the plugin's own problem, exactly as on real Paper |
| **Anti-cheat plugin** (movement/combat legitimacy checking) | raw movement/combat packet visibility both directions, high-priority cancellation before the action resolves, per-player flagging with persistence | **not portable** — no packet interception (#156/#157), no cancellation, no permission-gated punishment commands | **partially portable at best even after every issue in this epic lands** — real anti-cheat plugins depend on outbound packet mutation (faking a rubber-band) and on seeing the raw wire bytes in both directions with low latency; #156 explicitly names this as a genuine reentrancy hazard with no clean resolution proposed yet, only a design question. This is the archetype most likely to still fail the "any Java plugin" test after full completion of this roadmap. |
| **Holograms / disguise plugin** (fake entities and floating text via packets no real entity backs) | outbound packet injection (spawn/metadata packets the server never sent), or the client-only-cosmetic-entity path | **not portable** — depends on #156 resolving in favour of injection, which its own design issue is honest is not guaranteed; the client-only-entity alternative is a narrower, real path via #138/#140 once those land | **partially portable** — a *client-side-only* hologram (visible to the local player only, driven entirely by extract-time draw + a local fake entity) is achievable via #138/#140/#161 without needing #156 at all; a *server-broadcast* disguise visible to other real players needs outbound injection, which is the same open question as anti-cheat |
| **Client-side HUD mod** (a Fabric-class minimap/waypoint/info overlay) | custom draw buffer, input interception, no server involvement at all | **not portable** — `DebugLines` is the only precedent and is debug-shaped, not general-purpose (#161); no input hook (#162) | **portable**, and the cheapest archetype in this table — every capability it needs is additive to the existing `Extract`/`FrameSet` seams and touches no ceiling |
| **Pathfinding bot** (a Baritone-class navigator) | analog movement intent, a debug-geometry channel, per-tick collision queries against an owned snapshot, resumable multi-tick search state | **done today**, and it is not hypothetical — `lodestone-nav` (75 tests) plus the missing `lodestone-autopilot` shell (tracked separately, [#38](https://github.com/matteopolak/lodestone/issues/38)) is the one archetype this codebase has already built for real. `TickSet::Intent`, `LookIntent` and `ExtractSet::Debug` — the three gaps `docs/plugin-api.md` named as prerequisites — all closed in `0d82ab4` (see the stale-record note above) | already portable on the **native** tier; confirmed **not** portable on the WASM tier as a matter of architecture, not missing work — `docs/plugin-api.md`'s own cost analysis shows a stateless per-query capability call cannot host a resumable 20,000-node search cheaply, so this archetype is the concrete proof that native and WASM are not substitutes for each other |

**What this table adds beyond the capability list:** the anti-cheat and disguise rows are
the honest finding. Every other archetype's gaps are individually closeable by an issue
already filed in this epic. Those two depend on a *design decision* (#156) that this audit
does not resolve and flags as possibly unresolvable cleanly — outbound packet injection
and low-latency bidirectional raw-packet mutation sit directly on top of the net thread's
inline-apply-under-write-lock behavior (`world-unification.md`'s "can ingest stall the
frame" section), and every option considered in #156 either reintroduces the reentrancy
hazard this epic's other top-priority issue (#177) exists to eliminate, or leaks version
types across the plugin boundary in a way `bevy-migration.md` §5 forecloses for shared
crates. This is not a gap with an obvious size; it is the one place in the whole audit
where "just file the issue" was not honest, because the issue itself is "decide if this is
possible at all, and at what cost to the reentrancy guarantee."

## The ordered plan

Not a schedule — a dependency order. Four waves, each unblocking the next:

1. **Foundational primitives with no prerequisites, several already flagged P0 by the
   brief.** The event bus (#104), the reentrancy-unrepresentable design (#177) and its
   test harness (#179), the permission-node system (#125), the block write API (#129), the
   entity spawn/despawn API (#138), the scheduler (#113), and the stale-doc fix (#180).
   Nothing else in the epic can start in earnest without at least the event bus and the
   permission system, and #177 should land *before* any issue that adds a new
   plugin-facing entry point into the `World`, per the brief's own framing that ergonomics
   here is a correctness property, not a nice-to-have.
2. **The event-cancellation design decision (#101), and everything that names it as a
   dependency** — the concrete cancelable verbs (#109), monitor priority (#110), priority
   ordering (#105). This is the single highest-leverage decision in the epic: it gates the
   protection-plugin and minigame archetypes, which are otherwise the cheapest real wins
   available.
3. **Commands, world/entity write APIs, inventories, persistence** — mechanical once the
   foundations exist, each independently shippable, most with no cross-dependency on each
   other (#118→#119→#122, #131 on #129, #145, #147, #152, #153).
4. **The two open-ended tracks, which can run in parallel with everything above but should
   not block a v1:** the packet-interception design (#156, and its honest possibility that
   the answer is "partially, at a cost") and the WASM host (#172→#173→#175→#176, an
   epic-sized effort in its own right). Lifecycle/tooling design issues (#166, #168, #169,
   #170) are cheap to resolve early since most of them are documentation-shaped decisions,
   not implementation, and resolving them early avoids a contributor rediscovering the
   same question mid-implementation.

## Verdict: is "port any Java plugin" achievable as stated?

**No, not as stated, and the qualification is narrow and specific rather than a broad
hedge.** Of the roughly 15 Bukkit/Paper/Fabric capability families audited above, 13 are
either already real, cheaply closeable by an issue filed in this epic, or a *documented
ceiling* this project is explicitly right to accept (no `wgpu::Device` for a plugin, no
hot reload for the native tier, no novel wire-protocol types). Those ceilings do not
violate the spirit of the claim — Java plugins hit equivalent ceilings too (no plugin
replaces the JVM's renderer either; there isn't one to replace).

**The one qualification that is real:** packet-level interception in the direction that
matters for anti-cheat and true disguise plugins (outbound mutation/injection, low
latency, both directions) is not just unbuilt, it is **not currently known to be buildable
without either reopening the reentrancy hazard this epic's own top-priority issue exists to
close, or leaking version-specific wire types across a boundary the architecture treats as
inviolable.** That is a genuine, structural tension between two things the brief asks for
simultaneously: "make the reentrant deadlock unrepresentable" and "give plugins
ProtocolLib-class packet control." I do not think this is a flaw in the brief — it is the
correct thing for an audit to surface, and #156 is filed as a design issue precisely
because it may resolve to "achievable, at a documented latency/complexity cost" rather than
"not achievable," and that resolution is not mine to pre-empt.

**Restated as the honest version of the claim:** *any Bukkit/Paper/Fabric plugin whose
capability needs are drawn from the other fourteen families is portable, once the 49
sub-issues here land.* The fifteenth family — direct, bidirectional, low-latency packet
manipulation — is real, load-bearing for two named archetypes (anti-cheat, server-visible
disguises), and is the one place "not approximately" needs an asterisk until #156 either
lands a workable design or documents why it cannot.

## See also

- [`../plugin-api.md`](../plugin-api.md) — the surface as a specification, including the
  now-corrected gap list (see [#180](https://github.com/matteopolak/lodestone/issues/180)).
- [`../bevy-migration.md`](../bevy-migration.md) — the staged ECS plan; §6/§6.1 are the
  plugin-API and two-tier sections this doc's audit checks against the real tree.
- [`../world-unification.md`](../world-unification.md) — the lock-discipline section every
  reentrancy-adjacent issue in this epic (`#156`, `#157`, `#177`, `#179`) must be read
  against before implementation starts.
- [`../baritone-port.md`](../baritone-port.md) — the one archetype in the port-feasibility
  table that is already real, and the source of the WASM-cost analysis this doc leans on
  for the native-vs-WASM verdict.
- [`./README.md`](./README.md) — the roadmap index; epic #7 (substrate) vs. epic #77
  (capability parity) is explained there and repeated here because conflating the two is
  the most likely misreading of this doc.
