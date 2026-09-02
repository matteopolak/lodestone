# Plan: runtime plugin loading — wasm components, and the same plugin compiled in

## What it is

The design for loading plugins at runtime (Java-style, drop a file in a folder) using WebAssembly
components, while keeping the existing compiled-in bevy-plugin path — including the milestone-zero
refactor that makes the library expose the composed `App` so a consumer can register plugins at all,
and how one plugin can be authored once and shipped either way. Read-only plan against the tree as
of 2026-08-04; nothing here is implemented, and every `file:line` is a sample taken that day, not a
durable coordinate — several cited files were being edited by other agents *while this was written*,
and one citation went stale between two greps minutes apart (noted inline where it happened).

**The owner's ask, verbatim:** *"some kind of way to include plugins at runtime (like java does)
using wasm or dynamic libraries or something … maybe wasm so theyre platform-independent, with a way
to still build them into the binary (like we do now) for users who know what theyre doing."* Plus a
second, upstream requirement added during this plan's drafting: *"the library should expose the app
— lodestone-shell should just be the full rendering thing on top of it and it should reuse the app"*,
and, on `lodestone-autopilot`: *"users can clone the repo and add it if they want it (or use the
library and register the plugin)."*

This plan builds on, rather than replaces, the four wasm-tier issues
(host scaffold, capability ABI, manifest, sandbox gates — see the table below) and the four
native-tier design issues (load order, panic isolation, hot reload, ABI versioning — same table).
Their claims were
re-verified against the tree: `grep -rli 'wasmtime\|wasmer\|wasmi' crates/` is still empty, so
the host-scaffold issue's "what we have today: nothing" is still true, which for this repo's issue tracker is worth
stating explicitly.

## The decided architecture this designs within

Settled by the owner, not reopened here: client and server are both bevy ECS
(`docs/server-ecs.md`); plugins are ordinary bevy plugins; core systems become plugins where it
makes sense (physics as a client plugin a headless bot omits); the Paper compat layer is itself an
external plugin on the same public API (`docs/plans/paper-nms-bridge.md`); and there is **no
separate internal API and plugin API** — one surface, with exactly two privileged internals
(the socket/driver task and the GPU device/queue, `docs/plugin-api.md` §"What stays privileged").

That last rule is what makes runtime loading genuinely hard, and the honest statement of the
problem is:

> A compiled-in bevy plugin gets `&mut World` — arbitrary, synchronous, zero-cost access. A wasm
> guest cannot: it has no shared address space, no Rust type identity with the host, and every call
> crosses a copy boundary. "The same plugin, either compiled in or loaded at runtime" is therefore
> **not free**, and any design that pretends otherwise is lying about one side or the other.

## Milestone zero: the library exposes the app

> **Status: landed.** `lodestone-app` exists, `client_app()` is the registration point, and `Sim`
> adopts a pre-built `App` — see [`docs/plugin-registration.md`](../plugin-api.md) for the
> shipped shape and the measured gates. Two predictions below were wrong and are corrected there
> rather than edited out of this plan: **the three shell plugins did not move down** (the "no `wgpu`
> type in the file" evidence is real but is about the wrong axis — `interact.rs` imports fourteen
> items from `crate::sim`, a cycle with the type that adopts the `App`), and the **wasm headroom is
> 47%, not 10–13%** (gzip 844,527 B against the 1,600,000 B ceiling, measured after repairing the
> two `web/` breaks; the 1.21–1.24 MiB baseline this plan cites is stale). The rest of the section
> stands as written and is left as the record of what was reasoned before it was built.

**This refactor is upstream of every other milestone in this plan, and it is not about wasm at
all.** A runtime-loaded wasm plugin and a compiled-in one must arrive at the same registration
point — `add_plugins` on the one `App` — and today that point is unreachable from outside the
shell. Until it is reachable, "use the library and register the plugin" is not a real option, so
nothing downstream of it is either.

**Verified current state (2026-08-04, working tree):**

- The `App` type already lives below the shell: `lodestone_ecs::app::App`. The *composition* does
  not — `Sim::build` (`crates/lodestone-shell/src/sim/build.rs`) calls `App::new()` and `add_plugins` on
  a fixed tuple (`CorePlugin`, `LocalPlayerPlugin`, `ControllerPlugin`, `SessionHudPlugin`,
  `IngestPlugin`, `SessionPlugin`, `EntityInterpPlugin`, `TerrainPlugin`, `InteractPlugin`), then
  **takes the `World` and drops the `App`** (`sim/build.rs`), storing only an `EcsHandle`. The
  crate's own comment there states the consequence outright: *"the shell's plugin set
  is closed at compile time: no downstream crate holding a `Sim` can add one afterwards"* —
  `Plugin::build` needs `&mut App`, and the `App` no longer exists.
- The headless half of the seam **already exists**: `ClientBuilder::ecs(world: EcsHandle, session:
  Entity)` (`crates/lodestone-client/src/builder.rs`) lets a bot consumer build its own `App`
  on `lodestone-ecs`, add any plugins, and hand the `World` in. So "register a plugin from
  outside" is possible today *headless only*; the rendered client is the gap.
- `lodestone-autopilot`'s removal from the shell **landed while this plan was being written** — a
  first grep found `lodestone-autopilot = { workspace = true }` in
  `crates/lodestone-shell/Cargo.toml`; a second, minutes later, found the comment nearby
  saying the dependency is deliberately absent, and `sim/build.rs`'s note that `AutopilotPlugin`
  "used to be the last entry in that tuple and was removed on purpose." Not feature-gated —
  removed entirely, per the owner. The autopilot is now exactly the plugin a consumer would want
  to register from outside, and cannot, on the rendered path.
- The `#goto` chat command went with it (`crates/lodestone-shell/src/sim.rs`,
  `sim/session.rs`): the shell keeps the `#` command namespace reserved-but-empty, and both
  removal comments point at the plugin-command-registration issue (see the table below) as where a
  plugin will register commands properly. See the worked example below.

**Where composition belongs: a new crate, `lodestone-app`, between `lodestone-controller` and
`lodestone-shell`** — not growing `lodestone-controller`, whose charter is deliberately narrow
("the wasm-safe player-controller core," its own `Cargo.toml` package description). `lodestone-app` owns the plugin
tuple above plus the three plugins currently defined in the shell (`TerrainPlugin` in
`mesher.rs`, `InteractPlugin` in `interact.rs`, `EntityInterpPlugin` in
`entities.rs`), which move with it. The evidence they can: none of the three files contains a
`wgpu` type or a `crate::gpu` code reference (grep finds only two prose comments in `mesher.rs`) —
the mesher produces CPU-side data the GPU layer *pulls* (`sim/render_sources.rs` is precisely
"what the renderer pulls out of `Sim` each frame", its own module doc), so the dependency arrow
already points render→sim, never back. **The acceptance test for the answer, stated as the gate:
a headless consumer crate depending on `lodestone-app` must have no `wgpu` and no `winit`
anywhere in `cargo tree`** — run it, and run the negative control (the same check against
`lodestone-shell`, which must fail it).

**How a consumer registers a plugin: hand them the `App` before it is finalised.**
`lodestone-app` exposes roughly
`pub fn client_app(config: &AppConfig) -> App` — core plugin set installed, nothing render-shaped
— and the consumer calls `.add_plugins(TheirPlugin)` on the result, then hands it to a runner:

- headless: the existing `ClientBuilder::ecs` route (unchanged, just fed from `client_app()`), or
  `lodestone_ecs::runner`'s headless accumulator for offline simulation;
- rendered: `Sim` gains an entry point that **adopts** a pre-built `App` instead of building its
  own, and `Sim::new` becomes a thin wrapper: `client_app()` + adopt. The shell inserts its
  render-scoped resources (`ParticleSim`, `AudioEngine`, the session's `ChunkWorld`/`TerrainMesh`)
  *after* adoption, which needs only the `World`, not the `App` — resources need no
  `Plugin::build`.

This satisfies the no-two-APIs principle by construction: the shell registers `CorePlugin` and
friends through the identical `client_app()` a consumer calls, so there is no private composition
path left to drift. A `Vec<Box<dyn Plugin>>` constructor argument (shape (b)) was considered and
rejected: it is a second registration vocabulary with less power (no plugin groups, no
`is_plugin_added` interrogation between additions), existing only to avoid exposing a type that is
already public one crate down.

**The conformance test for milestone zero is `lodestone-autopilot`, built both native routes:**
in-repo (a consumer workspace member adding it to `client_app()`'s result) and out-of-repo (a
scratch crate depending on the published-path library, doing the same). The gate is behavioural,
not compile-only — `crates/plugins/lodestone-autopilot/tests/drives_to_goal.rs` already drives a
real `GameTick` schedule to a commanded block; the M0 gate is that same walk succeeding through a
`client_app()`-composed `App` on both routes, with a negative control: the identical harness
*without* `AutopilotPlugin` added must fail to arrive. Note this is the conformance test for
**milestone zero's native dual route**, not for the wasm/native dual path — the distinction
matters and is argued in the compiled-in section below.

**The worked example of the boundary: `#goto`, and what a plugin needs to contribute a command.**
The shell used to parse `#goto x z` itself and write `lodestone_autopilot::AutopilotGoal` — a chat
command in the shell reaching into a plugin's resource, backwards for a plugin architecture, and
now deleted. For the plugin to own it, the plugin-command-registration issue's shape is what is needed: a `CommandRegistry`
resource a plugin populates in `Plugin::build` (root literal, argument tree via the existing
`lodestone-command` crate, permission node), with chat input routed registry-first before the `#`
namespace falls through. **Is that inside the portable capability surface? Yes, deliberately:**
command registration is a one-shot init-time declaration (name + argument tree — plain data,
WIT-expressible) and command invocation is one more event variant in the stream a guest already
polls. A wasm `#goto` plugin is therefore fully expressible: declare the command in the manifest
or at init, receive `command-invoked` events, submit movement intents. The *native* autopilot
registers against the same `CommandRegistry` resource directly. One vocabulary, two power levels —
the same shape-3 relationship as everything else in this plan, which is the evidence the shape
generalises.

## The dual-path tension: three candidate shapes, one verdict

| shape | what it is | what it costs |
|---|---|---|
| 1. One capability API, two backends | every plugin written against a narrow capability surface; native backend implements it with direct access, wasm backend over the ABI | compiled-in plugins lose `&mut World` — contradicts "plugins are ordinary bevy plugins" and would force `lodestone-autopilot` through a keyhole it demonstrably does not fit (`docs/plugin-api.md` §"Native versus WASM" cost analysis) |
| 2. Two tiers, honestly separate | native gets full bevy; wasm gets a capability subset; different authoring experiences | two APIs is the exact thing the no-two-APIs principle exists to prevent, *if* the subset is a second vocabulary. It is not automatically that — see below |
| 3. Wasm hosts a *restricted* plugin; native is a superset | the wasm surface is the **intersection**; a plugin that stays inside it is portable (buildable both ways); a plugin that needs more is native-only, honestly | only works if the intersection is genuinely useful |

**Verdict: shape 3, with one refinement that dissolves most of shape 2's objection.** The
intersection is not a new vocabulary invented for wasm — it is the **intent doctrine's existing
vocabulary**, which the native tier already speaks:

- observe: the `GameEvent(ClientEvent)` bus (`crates/lodestone-ecs/src/events.rs`, one
  no-`match` write site) and snapshot-shaped reads of components/resources
- act: intent components (`BreakIntent`/`PlaceIntent`/`MovementIntent`/`LookIntent`,
  `crates/lodestone-ecs/src/player.rs`) and `ActionQueue` (`player.rs`)
- hear back: always-present outcome components (`BreakOutcome`, `PlaceOutcome { generation }`)

Every one of those is **already call-shaped or copy-shaped**: an event is a `Clone` value, an
intent is a small POD struct inserted/removed, an outcome is a small POD struct polled, an action
is a value pushed onto a `Vec`. None requires holding a borrow across a tick. The five clauses
(`docs/plugin-api.md` §doctrine) were designed to keep plugins out of other systems' machines —
and a surface that never hands out a machine is exactly a surface that serializes. **The doctrine
is accidentally an ABI spec.** That is the load-bearing observation of this plan.

So the relationship between the tiers is:

- **Portable plugin**: written against a new `lodestone-plugin-api` crate — a trait plus plain data
  types mirroring the intent/observe/outcome vocabulary, with **no bevy dependency**. Buildable two
  ways: (a) wrapped by a generated shim into an ordinary bevy plugin and compiled in behind a cargo
  feature; (b) compiled to a wasm component and loaded from disk at runtime. Same source, byte-for-
  byte same logic, two artifacts.
- **Native plugin**: everything `crates/plugins/` is today — full bevy API, `&mut World`-capable
  systems, real schedule ordering. `lodestone-autopilot` stays here and is the proof this tier must
  survive: `docs/plugin-api.md`'s own analysis (owned `Arc<ChunkSection>` snapshots, ~15k `Arc`
  clones per snapshot, thousands of collider queries per search step) concludes "Baritone targets
  the native tier," and nothing about runtime loading changes that arithmetic.

What shape 2's objection reduces to under this framing: there are still two *power levels*, but
only one *vocabulary* — the portable surface is a strict subset of what native plugins already do
through the same components and resources, not a parallel dialect. A plugin author graduates from
portable to native by gaining APIs, not by rewriting against different ones.

## What a wasm plugin structurally cannot do, and why

Stated plainly, checked against the five clauses, so nobody discovers these at implementation time:

1. **Hold any borrow into the host `World`.** Everything is a copy across linear memory. A
   guest never sees `&mut World`, a `Query`, or an `Arc<ChunkSection>`. Consequence: nav-class
   workloads (resumable search over an owned snapshot) pay a full snapshot copy per rebuild —
   possible, but the native tier exists so they never have to.
2. **Be a system.** A guest cannot register a `bevy_ecs` system or order against arbitrary sets.
   Instead the host runs one **conductor system per schedule slot** (a native system inside
   `WasmHostPlugin`) that drives every loaded guest's exported hook in sequence. Guests order
   *among themselves* by declaring an `EventPriority` tier (`Lowest..Monitor`,
   `crates/lodestone-ecs/src/sets.rs`) in their manifest; the conductor sorts by it. Clause 2
   ("exactly one system owns each machine") survives *because* of this: the conductor is the single
   writer that applies guest-submitted intents, so a guest cannot fork a sequence counter or race
   another writer even maliciously.
3. **Define component types visible to other plugins.** No shared Rust type identity. Cross-plugin
   state is host-defined vocabulary only (plus, if ever needed, an opaque per-plugin key-value
   store — deliberately left open below).
4. **Block the tick.** The host enforces preemption (wasmtime epoch interruption or fuel), which is
   a capability the native tier structurally cannot offer — a native plugin that loops forever
   hangs the game, full stop (the panic-isolation issue's honest answer for the native tier). Failure isolation
   *belongs* to the wasm tier: a trapping, panicking, or runaway guest is unloaded and reported,
   process intact. This is the strongest positive argument for wasm beyond portability.
5. **Touch the two privileged internals** — which native plugins cannot touch either, so this row
   costs nothing.

Clause check: (1) wishes-in-observation-vocabulary survives verbatim — the ABI *is* that
vocabulary; (2) survives via the conductor, above; (3) refusal-observable requires the ABI to carry
outcomes back — every intent-install call is paired with an outcome poll (or an outcome push in the
guest's next tick callback), and `PlaceOutcome::generation` crosses the boundary unchanged so a
late-polling guest keeps the same race-free read the native tier has; (4) human-outranks is
enforced host-side in `drive_mining`/`drive_placement` and needs nothing from the guest; (5)
lifecycle-encodes-verb-shape maps to paired install/remove ABI calls the host mirrors into real
component insert/remove.

## Runtime choice: wasmtime, with the component model and WIT

**Recommendation: wasmtime + the component model, WIT-defined ABI, `wit-bindgen` for Rust guests.**

- **Wasmtime** is actively maintained (v1-8.0.3, released 2026-07-31 —
  [releases](https://github.com/bytecodealliance/wasmtime/releases)), is the reference
  implementation of the component model
  ([component-model.bytecodealliance.org](https://component-model.bytecodealliance.org/running-components/wasmtime.html)),
  and already ships WASI 0.3 / preview-3 support (Wasmtime 43+,
  [eunomia status survey](https://eunomia.dev/blog/2025/02/16/wasi-and-the-webassembly-component-model-current-status/)).
  Host-call overhead is on the order of **10 ns** for a trivial call after the 2023 trampoline
  overhaul ([Bytecode Alliance, "Wasmtime and Cranelift in 2023"](https://bytecodealliance.org/articles/wasmtime-and-cranelift-in-2023)),
  which sets the floor for the cost model below. Epoch interruption and fuel give the preemption
  the panic-isolation issue wants. Security posture is serious (the same releases page shows coordinated point releases
  across three supported branches for a single CVE).
- **Wasmer** is optimized for a different bet — WASIX, a POSIX-flavored fork of WASI preview 1 for
  running existing applications ([docs.wasmer.io](https://docs.wasmer.io/runtime/runners/wasix));
  component-model alignment is planned, not shipped
  ([2026 comparison](https://wasmruntime.com/en/blog/wasmtime-vs-wasmer-2026)). Our guests are
  purpose-written plugins, not ported POSIX apps; WASIX buys us nothing and the component model is
  the part we actually need.
- **wasmi** (1.0 stable —
  [Wasmi Labs announcement](https://wasmi-labs.github.io/blog/posts/wasmi-v1.0/)) is an
  interpreter: slower by design, but `no_std` and itself compilable to wasm
  ([README](https://github.com/wasmi-labs/wasmi)) — which makes it the one candidate relevant to
  the browser question below. It does **not** implement the component model, only core wasm. Wrong
  choice for the desktop host; keep it in mind as a possible second backend, and note that choosing
  WIT as the ABI *description* does not foreclose it (a WIT world can be hand-lowered onto core-wasm
  imports/exports if a wasmi backend is ever built — jco does exactly this transpilation for
  browsers).

**Why the component model rather than hand-rolled core-wasm imports:** the ABI is a typed surface
of records, variants, lists and resources — exactly what WIT expresses and `wit-bindgen` generates
ergonomic Rust guest bindings for. Hand-rolling means inventing our own lifting/lowering for a
~110-variant `ClientEvent` (`crates/lodestone-model/src/event.rs`) and maintaining it by hand
forever; that is the staleness factory this repo's §2 exists to warn about. The WIT world is also
the natural unit of ABI versioning, which gives the ABI-versioning issue its second axis for free (a world is named and
versioned; a guest built against `lodestone:plugin@0.2.0` is rejected loudly by a host that only
speaks `0.1.x`).

One honest cost: WIT/component tooling is younger than core wasm. `cargo-component`/`wit-bindgen`
are actively maintained and used in production hosts, but expect toolchain churn; pin versions in
the host crate and vendor the `.wit` files in-repo as the single source of truth.

## Per-tick cost: prediction first, then what must be measured

The risk case named in the brief: a plugin observing block/entity events at 20 Hz over thousands of
entities. Predictions below are derived from the 10 ns call floor plus copy costs at memory
bandwidth (~10 GB/s conservative for `memcpy` into linear memory); they are *predictions to be
measured against*, not results.

| pattern | arithmetic | per-second cost | verdict |
|---|---|---|---|
| chatty: per-event call, 1,000 events/tick × 5 guests | 100k crossings/s × ~300 ns (call + lift/lower of one record with a string) | ~30 ms/s ≈ 3% of a core | tolerable but wasteful — and it scales linearly with event rate |
| chatty: per-entity poll, 5,000 entities, per-entity call returning a record | 100k calls/s/guest × 0.3–1 µs | 30–100 ms/s **per guest** | the failure mode; forbidden by ABI design, not by advice |
| batched: one `events-since-last-tick` call per guest per tick returning `list<event>` | 20 crossings/s/guest + 1,000 events × ~64 B = 64 KB/tick ≈ 1.3 MB/s memcpy | ~0.1 ms/s | fine |
| batched: one `entity-snapshot` call, 5,000 entities × ~64 B | 320 KB/tick ≈ 6.4 MB/s memcpy | ~1 ms/s | fine |

So the design rule, which is the capability-ABI issue's open decision (1) resolved: **the ABI exposes batched,
tick-granular calls only** — `poll-events() -> list<event>`, `snapshot-entities(filter) ->
list<entity-snapshot>`, `submit-actions(list<action>)` — never per-item calls in the hot vocabulary.
This mirrors the native tier's own owned-snapshot pattern, and it is also the capability-ABI issue's decision (2)
resolved: **the guest owns its own state across calls** (it is a live instance with linear memory,
not stateless request/response), which is what makes a resumable computation inside a guest viable
without the host running it.

**What must be measured before M2 ships, on an idle machine** (per the standing rule: a figure
taken while other agents build is a sample, and a sequential-duration ratio is not protected either
— prefer counts, and record the machine state alongside the number):

1. Round-trip cost of one representative WIT call with a `ClientEvent`-shaped variant argument,
   host→guest and guest→host, wasmtime pinned version, M-series host.
2. The batched entity-snapshot call at 1k / 5k / 20k entities — confirm it is memcpy-bound.
3. Fixed per-guest per-tick overhead (store access, epoch check, conductor dispatch) at 1 / 5 / 20
   loaded guests.
4. Per-`Store` memory overhead and whether the pooling allocator matters at our guest counts.

## wasm-in-wasm: the browser answer

The client already targets `wasm32-unknown-unknown` for the browser (`web/`,
`crates/lodestone-server/Cargo.toml`'s wasm32 target block), with an enforced bundle ceiling of
**1,600,000 B gzip** and a recorded baseline of 1.21–1.24 MiB — roughly 10–12% headroom after the
bevy adoption (`scripts/wasm-size.sh`; `docs/plans/server-ecs-migration.md` §"Binary size,
measured").

**Wasmtime cannot run inside a wasm32 guest** — it is a JIT that maps executable pages; there is no
wasmtime-as-guest story. So the desktop design does not carry to the browser as-is. Two structural
outs exist, neither cheap:

- **wasmi compiled to wasm** (proven pattern: Substrate runs wasmi inside its own wasm runtime —
  [wasmi README](https://github.com/wasmi-labs/wasmi)). Costs: an interpreter (order 10–100×
  slower), a second runtime backend to maintain, no component model (hand-lowering of the WIT
  world), and bundle bytes we mostly do not have — against ~170–380 KiB of remaining gzip headroom,
  an embedded interpreter plus lowered bindings is plausibly most of it.
- **jco transpilation**: the Bytecode Alliance's component-to-JS+core-wasm transpiler runs
  components in browsers ([component model docs](https://component-model.bytecodealliance.org/running-components/wasmtime.html)
  ecosystem; jco is the browser-side runner). The plugin would run as a sibling JS+wasm module
  with host imports bridged through JS into our exported functions — real engineering across the
  wasm-bindgen boundary, not a checkbox.

**Verdict: browser plugin support is out of scope for v1, and that is fine — say so rather than
imply it.** The deliverable here is that the ABI is defined in WIT, which both outs above can
consume, so v1 forecloses neither. Desktop client and (eventually) desktop-hosted server get
runtime loading; the browser build gets compiled-in plugins only, exactly as it gets everything
else today.

## Dynamic libraries, costed honestly

Rust has no stable ABI: `repr(Rust)` layout is explicitly unspecified and may change between
compiler releases ([Rust Reference, type layout](https://doc.rust-lang.org/reference/type-layout.html#the-rust-representation)).
A `cdylib` plugin therefore only works when built by the **exact same rustc, same std, same
dependency versions** as the host — which as a distribution story is "we mail you a toolchain," not
"drop a file in a folder." Worse, and specific to us: bevy's storage is `TypeId`-keyed, and the
hot-reload issue's analysis already records that a reloaded `.so` from a different build gets different `TypeId`s for
the "same" component, silently breaking every `Query` — the worst failure shape, wrong rather than
loud. `abi_stable`-style C-shaped boundaries exist, but a C ABI boundary *is* a capability ABI with
none of wasm's sandbox, portability, or preemption — all of the restriction, none of the payoff.

**When dylibs are the right answer: essentially never, for this project.** The one candidate niche
— trusted, local, same-machine dev iteration — is served better by (a) the compiled-in path with
incremental builds, and (b) wasm hot reload, which is *cheap and safe* precisely because guest and
host never share type identity (the hot-reload issue's own conclusion: hot reload is an argument **for** the wasm
host). This plan recommends closing the dylib option explicitly in that issue's decision record rather
than leaving it half-open.

## The compiled-in path: how it generalises, and the conformance plugin

The compiled-in mechanism after milestone zero is: a consumer (the shell included) calls
`client_app()`, adds plugins, hands the `App` to a runner. `lodestone-autopilot` is the worked
case — removed from the shell entirely (verified above; the owner's framing is "users can clone
the repo and add it if they want it, or use the library and register the plugin"), so "compiled
in" now means *compiled into the consumer's build via registration*, not "listed in the shell's
tuple." What this plan adds on top:

- **Portable plugins get their compiled-in artifact via a shim**, not by hand: `lodestone-plugin-api`
  ships a `NativeHost<P: LodestonePlugin>` adapter that wraps any portable plugin into an
  `impl bevy_app::Plugin` (its systems are thin calls into the same hook functions the wasm
  conductor would call). One source, two artifacts, no forked logic.
- The wasm host itself is just another native plugin — `WasmHostPlugin`, added through the same
  `client_app()` seam. It has no privileged position, which is exactly the no-two-APIs principle
  applied to the loader.

**Two dual paths, two conformance tests — the distinction matters.** A natural proposal is
autopilot, built compiled-in and runtime-loaded, as the dual-path proof. Half right:

- **M0's native dual route** (registered in-repo vs registered by an external library consumer):
  autopilot **is** the right conformance test, as specified in the milestone-zero section — it is
  the plugin the owner named, and its existing goal-arrival gate is behavioural.
- **M4's wasm/native dual path**: autopilot is the **wrong** first conformance test, and the
  record already says why — it is the native tier's stress case (owned `Arc<ChunkSection>`
  snapshots, ~15k `Arc` clones per snapshot, thousands of collider queries per search step), and
  `docs/plugin-api.md`'s own cost analysis concludes it does not fit through a call-shaped ABI.
  Forcing it through one would prove the wrong point at maximum cost. The right first conformance
  plugin for M4 is small and lives entirely inside the portable vocabulary:
  `crates/plugins/lodestone-event-logger` (a `Monitor`-tier `GameEvent` reader) is nearly it
  already, plus one *acting* plugin (a chat auto-responder: observe chat events, push a chat
  `ClientAction`) so the suite covers both directions of the boundary. The M4 gate: **the same
  plugin source, built as a wasm component and as a compiled-in shim, run against the same
  recorded event stream, must produce identical action sequences** — an equivalence assertion
  whose expected value originates outside both backends. If, later, batched snapshot queries prove
  cheap enough that a *simplified* navigator fits the portable surface, porting autopilot becomes
  a stretch goal — but it is not the gate.

## Impact on the existing issues

| issue | verdict under this plan |
|---|---|
| #172 (host scaffold) | proceed; record the runtime decision (wasmtime + component model) in it; the scaffold's "load a module, call an export, prove fs/socket denial" stays exactly as scoped |
| #173 (capability ABI) | its three open decisions resolve here: (1) batched/snapshot calls only in the hot path; (2) guests are stateful instances; (3) actions submitted batched, mirroring `ActionQueue`. The ABI *content* is the intent-doctrine vocabulary, defined as a versioned WIT world vendored in-repo |
| #175 (manifest) | still needed — the component model types the surface but does not carry capability *policy*. Keep the TOML manifest as scoped there, adding: the WIT world version targeted, and the declared `EventPriority` tier for conductor ordering |
| #176 (sandbox gates) | unchanged and load-bearing; add a preemption gate (a guest that loops forever must be interrupted, with a negative control where epochs are disabled and the loop is observed to hang a sacrificial thread) |
| #166 (native load order) | unchanged for native; the wasm host adds runtime load order = manifest-declared dependencies, topologically sorted, which is Bukkit's shape and should be specified in #175 rather than a new issue |
| #168 (panic isolation) | resolves as: native panics stay fatal by design (trusted code); the wasm tier is where isolation lives — trap/unload/report, plus preemption. Its "documentation paragraph, not a mechanism" instinct for the native tier is confirmed |
| #169 (hot reload) | resolves as: native hot reload rejected (no stable ABI, `TypeId` breakage); wasm hot reload accepted as a real deliverable — drop-in replace of a component file, guest state lost unless the plugin opts into a serialize/restore hook (left open below) |
| #170 (ABI versioning) | gains the WIT world version as a second, machine-checked axis alongside the ordering-anchor enum policy |
| [#118](https://github.com/matteopolak/lodestone/issues/118) (plugin command registration) | promoted in priority by the `#goto` removal (the milestone-zero section's worked example): the `CommandRegistry` it specifies is the thing that lets a plugin own the command the shell just gave up, and command registration/invocation belongs in the portable capability surface from day one |

Server-side: the wasm host lands **client-first**. The client `World` has the full surface today
(bus, intents, outcomes, `ActionQueue`); the server has no plugin registration point at all —
`docs/plans/paper-nms-bridge.md`'s census found **bucket (a) empty: zero of 14 seam categories
reachable**, because nothing exists to register against until `docs/plans/server-ecs-migration.md`
Phase 2 (proposal queue + `Adjudicate`) lands. The same conductor design then applies server-side
with guests slotted into the `Adjudicate` window — which is precisely the registration point whose
absence the paper-nms census names as the thing to fix. Nothing in this plan needs redesigning for
that; it needs Phase 2 to exist.

## Milestones

Ordered so that the first thing that exists is a real plugin doing something observable — the
dominant defect class here is the island (built, tested, reaches nothing), and "a plugin host with
no plugin loaded through it" is its textbook form.

- **M0 — the library exposes the app** (the milestone-zero section above, in full): `lodestone-app`
  with `client_app()`, the three shell plugins moved down, `Sim` adopting a pre-built `App`, the
  no-`wgpu`/`winit` `cargo tree` gate with its negative control, and autopilot registered from
  outside on both native routes as the behavioural gate. Everything below arrives through the seam
  this creates. Bundle-size caveat: this moves code down the stack and must add **no** new
  dependency edges to the headless graph; re-run `scripts/wasm-size.sh` before and after — noting
  the report, received during this plan's drafting and not independently verified here, that
  `just wasm-size` is **currently unrunnable on `main`** from two pre-existing committed `web/`
  breaks. If true, the 1.6 MB gzip ceiling is presently *unenforced*, and a layering refactor is
  exactly the kind of change that would blow it silently. Repairing the gate is part of M0's
  definition of done, not optional.
- **M1 — one real plugin, loaded at runtime, observably acting.** A `plugins/` directory scanned at
  startup; the chat auto-responder above compiled to a wasm component; loaded by a minimal
  `lodestone-wasm-host` embedding wasmtime; its observed→acted round trip lands in the client's
  chat, on screen. Gate: an integration test over the real client `World` asserting the action
  reaches `ActionQueue`; **negative control**: with the `.wasm` file absent, the same test must
  observe zero actions — run it and watch it fail the positive assertion. The WIT world at M1 is
  deliberately tiny: `poll-events`, `submit-actions`, nothing else.
- **M2 — the batched query surface + the cost measurements** (the four measurements above), sized
  against the predictions table; any pattern that misses its prediction by more than ~3× is a
  design signal, not a tuning problem.
- **M3 — sandbox + preemption gates** (the sandbox-gates issue as scoped, plus the epoch gate), each with its working
  negative control.
- **M4 — the dual path.** `lodestone-plugin-api` + `NativeHost` shim; the conformance plugin built
  both ways; the equivalence gate above. This is the milestone that discharges the owner's dual
  requirement, and it is deliberately *after* the runtime path is real — the shim without a working
  host would be an island with extra steps.
- **M5 — manifest, load order, versioning, hot reload** (resolutions above, per the table).
- **M6 — server-side conductor**, blocked on server-ecs Phase 2, not before.

## Deliberately left open for the owner

- **Distribution and trust presentation**: whether loaded plugins are listed in-game, whether
  capability grants are prompted per-plugin or configured in a file, and any signing story.
- **A cross-plugin opaque channel** (key-value or message) for wasm guests that want to talk to
  each other without host vocabulary — cheap to add, easy to get wrong, not needed for M1–M4.
- **Hot-reload state carry-over**: whether a reloaded guest gets a serialize/restore hook or always
  restarts cold.
- **Whether `ActionQueue` becomes a bevy `Message`** first (the open design question
  `docs/plugin-api.md` records) — the ABI mirrors whichever shape wins; deciding before M2 avoids
  mirroring the losing one.
- **Whether the shell's own binary bundles any plugins at all.** The autopilot call — removal, not
  feature-gating — suggests the default answer is none, with `cargo run` shipping a plugin-free
  client and every plugin arriving via a consumer build or the wasm loader. But "none" is a real
  product decision (it changes what a plain `cargo run --release` can do), so it is the owner's.

## Dependencies

- `wasmtime` (+ `wasmtime-wasi` only if a capability ever grants WASI interfaces; default guests
  get none), pinned; `.wit` files vendored in-repo; `wit-bindgen`/`cargo-component` for guests.
- `lodestone-ecs` (the vocabulary), `lodestone-model` (`ClientEvent`/`ClientAction`); the host
  crate must remain version-free — it speaks the same version-free vocabulary plugins do.
- No new dependency for the compiled-in path.

## See also

- `docs/plugin-api.md` — the doctrine this ABI serializes; its "Native versus WASM" table is the
  ancestor of this plan's shape 3.
- `docs/server-ecs.md`, `docs/plans/server-ecs-migration.md` — the server `World` and the Phase 2
  registration point M6 waits for.
- `docs/plans/paper-nms-bridge.md` — the census proving the server has no reachable seam yet; the
  JVM tier is a future *native* plugin and is unaffected by the wasm tier except that both consume
  the same public surface.
- See the "Impact on the existing issues" table above for the full set of issues this plan touches.
