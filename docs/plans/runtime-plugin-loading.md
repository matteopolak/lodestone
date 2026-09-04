# Plan: runtime plugin loading — wasm components, and the same plugin compiled in

## What it is

This plan defines runtime plugin loading through WebAssembly components while retaining compiled-in
bevy plugins. It also requires the library to expose the composed `App`, so a consumer can register
a plugin before selecting a runner, and it defines how one portable plugin source produces either
a component or a compiled-in artifact. File paths and symbols identify durable integration points;
re-find a symbol rather than relying on a line number.

The runtime host, capability ABI, manifest, sandbox, native load-order, failure-isolation,
hot-reload, and ABI-versioning requirements are resolved below. `lodestone-wasm-host` implements
component loading, manifest parsing, capability gating, fuel preemption, and the conductor-to-
`ActionQueue` integration. Remaining work extends that shipped core without widening the default
capability surface.

## The decided architecture this designs within

Settled by the owner, not reopened here: client and server are both bevy ECS
([The integrated and dedicated server](../dedicated-server.md)); plugins are ordinary bevy plugins; core systems become plugins where it
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

> **Implemented baseline.** `lodestone-app::client_app()` provides the renderer-free composition;
> rendered consumers start from `Sim::client_app()`, which adds the shell-coupled plugins before
> `Sim::from_app`, `WindowApp::new_with_app`, `app::run_with_app`, or
> `lodestone_shell::run_with_app` takes ownership. The shell-coupled plugins stay there because
> they depend on shell-internal simulation types. [The plugin API](../plugin-api.md)
> records the integration shape and its measured gates. The browser-size guard currently fails;
> its script is the source of truth for the current measurement.

**This baseline is upstream of every other milestone and is not about wasm.** A runtime-loaded
component and a compiled-in plugin arrive at the same registration point: `add_plugins` on one
`App`. The public library seam must remain available before any runner finalizes that `App`.

**Current composition constraints:**

- `lodestone-app::client_app()` composes the reusable renderer-free plugin set. A headless consumer
  registers additional plugins before passing the resulting `App` to a runner.
- `ClientBuilder::ecs(world: EcsHandle, session: Entity)` remains the headless handoff. The rendered
  handoff starts from `Sim::client_app()` and continues through `Sim::from_app` or
  `WindowApp::new_with_app`; it includes the shell-coupled plugins required by a rendered client.
- `lodestone-autopilot` is not a shell dependency or feature. It is a consumer plugin that must
  register through the library seam on both headless and rendered routes.
- The former `#goto` chat command is absent with the shell dependency: the shell keeps the `#`
  command namespace reserved-but-empty, and command registration belongs to the plugin-owned
  registry described in the worked example below.

**Composition is split between `lodestone-app` and the rendering shell.** `lodestone-app` remains
below `lodestone-shell`; `lodestone-controller` stays the wasm-safe player-controller core.
`TerrainPlugin`, `InteractPlugin`, and `EntityInterpPlugin` remain shell plugins because
`interact.rs` depends on `crate::sim`. The mesher produces CPU-side data that the GPU layer pulls,
so the render dependency remains render→simulation. **The acceptance gate is:
a headless consumer crate depending on `lodestone-app` must have no `wgpu` and no `winit`
anywhere in `cargo tree`** — run it, and run the negative control (the same check against
`lodestone-shell`, which must fail it).

**How a consumer registers a plugin: take the `App` before it is finalised.**
`lodestone-app::client_app() -> App` installs the renderer-free core. `Sim::client_app() -> App`
adds the shell-coupled plugins required for rendering. The consumer calls `.add_plugins(TheirPlugin)`
on the composition for its runner, then hands it to that runner:

- headless: the existing `ClientBuilder::ecs` route (unchanged, just fed from `client_app()`), or
  `lodestone_ecs::runner`'s headless accumulator for offline simulation;
- rendered: start from `Sim::client_app()`, add plugins, then call `Sim::from_app` or
  `lodestone_shell::run_with_app`. `Sim::new` is the thin `Sim::client_app()` + adopt wrapper. The
  shell inserts its
  render-scoped resources (`ParticleSim`, `AudioEngine`, the session's `ChunkWorld`/`TerrainMesh`)
  *after* adoption, which needs only the `World`, not the `App` — resources need no
  `Plugin::build`.

This satisfies the no-two-APIs principle by construction: `Sim::client_app()` begins with
`lodestone_app::client_app()` and adds only the shell-coupled plugins, so there is no private
composition path left to drift. A `Vec<Box<dyn Plugin>>` constructor argument (shape (b)) was considered and
rejected: it is a second registration vocabulary with less power (no plugin groups, no
`is_plugin_added` interrogation between additions), existing only to avoid exposing a type that is
already public one crate down.

**The conformance test for milestone zero is `lodestone-autopilot`, built both native routes:**
in-repo (a consumer workspace member adding it to `Sim::client_app()`'s result) and out-of-repo (a
scratch crate depending on the published-path library, doing the same). The gate is behavioural,
not compile-only — `crates/plugins/lodestone-autopilot/tests/drives_to_goal.rs` already drives a
real `GameTick` schedule to a commanded block; the M0 gate is that same walk succeeding through a
runner-appropriate composed `App` on both routes, with a negative control: the identical harness
*without* `AutopilotPlugin` added must fail to arrive. Note this is the conformance test for
**milestone zero's native dual route**, not for the wasm/native dual path — the distinction
matters and is argued in the compiled-in section below.

**The worked example of the boundary: `#goto`, and what a plugin needs to contribute a command.**
`#goto` illustrates the boundary: a native plugin owns the command through a `CommandRegistry`
resource that it populates in `Plugin::build` (root literal, argument tree via the existing
`lodestone-command` crate, permission node), with chat input routed registry-first before the `#`
namespace falls through. The portable WASM surface does **not** yet expose command declaration or
invocation. Its WIT world currently exports only `init` (plugin metadata) and `on-tick` (subscribed
events in, allowed actions out). Adding portable commands requires a versioned WIT declaration
shape, a command-invoked event, and host-side routing; movement intents also remain outside the
shipped action vocabulary. The native autopilot registers directly against `CommandRegistry` today.

## The dual-path tension: three candidate shapes, one verdict

| shape | what it is | what it costs |
|---|---|---|
| 1. One capability API, two backends | every plugin written against a narrow capability surface; native backend implements it with direct access, wasm backend over the ABI | compiled-in plugins lose `&mut World` — contradicts "plugins are ordinary bevy plugins" and would force `lodestone-autopilot` through a keyhole it demonstrably does not fit ([The plugin API: Two tiers](../plugin-api.md#two-tiers)) |
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
  survive: `docs/plugin-api.md`'s own analysis (owned `Arc<ChunkSection>` snapshots, substantial
  `Arc` cloning, and many collider queries per search step) concludes "Baritone targets
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
4. **Block the tick.** The host enforces fuel-based preemption, which is
   a capability the native tier structurally cannot offer — a native plugin that loops forever
   hangs the game, full stop. Failure isolation
   *belongs* to the wasm tier: a trapping, panicking, or runaway guest is unloaded and reported,
   process intact. This is the strongest positive argument for wasm beyond portability.
5. **Touch the two privileged internals** — which native plugins cannot touch either, so this row
   costs nothing.

Clause check: the shipped ABI is the current, narrow vocabulary: `init`, `on-tick`, and three
event variants plus three action variants. It preserves one conductor as the `ActionQueue` writer but does
not yet carry intent installs/removals, outcomes such as `PlaceOutcome`, or component mirroring.
Those operations require a future, versioned ABI extension with paired install/remove calls,
outcome delivery, and host-side component mapping. That extension must preserve the existing
host-side priority and corrective-action rules rather than letting a guest write them directly.

## Runtime choice: wasmtime, with the component model and WIT

**Recommendation: wasmtime + the component model, WIT-defined ABI, `wit-bindgen` for Rust guests.**

- **Wasmtime 47** is pinned with `default-features = false` and the `runtime`, `cranelift`,
  `component-model`, and `std` features. `wasmtime-wasi` is intentionally absent: guests receive
  only the imports the host's `Linker` grants. The host uses the component model and WIT-generated
  bindings, and fuel bounds each guest callback; epoch interruption remains a future option because
  it requires a watchdog that increments the engine epoch.
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
ergonomic Rust guest bindings for. Hand-rolling means inventing our own lifting/lowering for the
large `ClientEvent` variant (`crates/lodestone-model/src/event.rs`) and maintaining it by hand
forever; that is a staleness factory. The WIT world is also
the natural unit of ABI versioning: a world is named and
versioned; a guest built against `lodestone:plugin@0.2.0` is rejected loudly by a host that only
speaks `0.1.x`.

One honest cost: WIT/component tooling is younger than core wasm. `cargo-component`/`wit-bindgen`
are actively maintained and used in production hosts, but expect toolchain churn; pin versions in
the host crate and vendor the `.wit` files in-repo as the single source of truth.

## Per-tick cost: measure the implemented boundary

The shipped ABI calls each guest once per tick with a `list<event>` and receives a `list<action>`.
That batching avoids a per-event crossing in the hot path; it does not establish a timing budget.
Guests retain their own linear-memory state across callbacks, so resumable work stays guest-owned
rather than becoming host request/response state.

**What must be measured before M2 ships, on an idle machine** (per the standing rule: a figure
taken while other agents build is a sample, and a sequential-duration ratio is not protected either
— prefer counts, and record the machine state alongside the number):

1. Round-trip cost of one representative `on-tick` WIT call with a `ClientEvent`-shaped list and
   returned action list, using the pinned Wasmtime 47 host on an M-series machine.
2. The batched entity-snapshot call at 1k / 5k / 20k entities — confirm it is memcpy-bound.
3. Fixed per-guest per-tick overhead (store access, fuel accounting, conductor dispatch) at 1 / 5 / 20
   loaded guests.
4. Per-`Store` memory overhead and whether the pooling allocator matters at our guest counts.

## wasm-in-wasm: the browser answer

The client already targets `wasm32-unknown-unknown` for the browser (`web/`,
`crates/lodestone-server/Cargo.toml`'s wasm32 target block). `scripts/wasm-size.sh` enforces a
**1,600,000 B gzip** ceiling. The current `just wasm-size` snapshot is **17,732,637 B raw**,
**6,005,332 B gzip**, and **4,875,099 B brotli**. The overage is generated data and static tables
in the shipped client, not the WASM host: the browser dependency graph does not reach
`lodestone-wasm-host`. Do not treat older bundle measurements as a current headroom budget.

**Wasmtime cannot run inside a wasm32 guest** — it is a JIT that maps executable pages; there is no
wasmtime-as-guest story. So the desktop design does not carry to the browser as-is. Two structural
outs exist, neither cheap:

- **wasmi compiled to wasm** (proven pattern: Substrate runs wasmi inside its own wasm runtime —
  [wasmi README](https://github.com/wasmi-labs/wasmi)). Costs: an interpreter, a second runtime
  backend to maintain, no component model (hand-lowering of the WIT
  world), and additional bundle bytes whose cost must be measured against the current browser
  bundle rather than assumed from an older baseline.
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
reloading a `.so` from a different build gets different `TypeId`s for
the "same" component, silently breaking every `Query` — the worst failure shape, wrong rather than
loud. `abi_stable`-style C-shaped boundaries exist, but a C ABI boundary *is* a capability ABI with
none of wasm's sandbox, portability, or preemption — all of the restriction, none of the payoff.

**When dylibs are the right answer: essentially never, for this project.** The one candidate niche
— trusted, local, same-machine dev iteration — is served better by (a) the compiled-in path with
incremental builds, and potentially (b) WASM component replacement, which can avoid shared type
identity but is not implemented. This plan closes the dylib option rather than leaving it half-open.

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
  snapshots, substantial `Arc` cloning, and many collider queries per search step), and
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

## Requirements resolved by this plan

| concern | durable decision |
|---|---|
| Runtime host | `lodestone-wasm-host` uses Wasmtime 47 and the component model to load a component or encode a core module, then invokes it through a WIT world vendored in-repo. |
| Capability ABI | The host supplies capability-gated events to each guest's `on-tick` callback and accepts its returned actions through one conductor, preserving `ActionQueue` as the single writer. |
| Manifest | TOML expresses capability policy, the targeted WIT world version, and the `EventPriority` tier used by the conductor. |
| Sandbox and preemption | The `Linker` omits ungranted imports; fuel interrupts an endlessly looping guest and marks it permanently failed. Epoch preemption requires a watchdog and is not enabled. |
| Native load order | Native plugins retain their existing ordering rules. Runtime plugins load in manifest priority-then-name order; dependency declarations remain a future extension. |
| Failure isolation | Trusted native-plugin panics remain fatal. The wasm tier traps, unloads, and reports failing guests while keeping the process alive. |
| Hot reload | Native hot reload is rejected because Rust has no stable ABI and `TypeId` identity breaks across builds. WASM component replacement and state carry-over are not implemented. |
| ABI versioning | The WIT world version is machine-checked alongside the ordering-anchor enum policy. |
| Plugin commands | Native plugins use `CommandRegistry`. Portable command declaration and invocation require a future WIT extension and host routing; the current `init`/`on-tick` ABI does not expose them. |

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

- **M0 — library and rendered composition: implemented.** `lodestone-app::client_app()` supplies the
  renderer-free baseline; `Sim::client_app()` extends it with shell-coupled plugins; `Sim::from_app`
  and `run_with_app` preserve a caller's plugin through real window construction. The headless graph
  remains free of `wgpu` and `winit`; use `scripts/wasm-size.sh` for the current browser-size result.
- **M1 — runtime host: implemented, with application wiring remaining.** `lodestone-wasm-host` scans
  plugin directories, parses manifests, loads a component or core module, gates capabilities, drives
  `on-tick`, and routes returned actions through the real `ActionQueue`. Its integration tests use
  the real `lodestone_app::client_app()` and include absent-plugin and denied-capability controls.
  No shipped client currently calls `load_directory`, so selecting and loading a plugin directory is
  the remaining on-screen integration step.
- **M2 — the batched query surface + the cost measurements** (the four measurements above). A
  measurement that materially conflicts with the intended batching model is a design signal, not a
  tuning problem.
- **M3 — sandbox + preemption gates: implemented.** Import capabilities are denied by an absent
  `Linker` interface; event/action capabilities are enforced by the conductor; a spinning guest is
  fuel-preempted and permanently failed by the tested negative control.
- **M4 — the dual path.** `lodestone-plugin-api` + `NativeHost` shim; the conformance plugin built
  both ways; the equivalence gate above. This is the milestone that discharges the owner's dual
  requirement, and it is deliberately *after* the runtime path is real — the shim without a working
  host would be an island with extra steps.
- **M5 — manifest, dependency ordering, versioning, and hot reload.** Manifest parsing, priority-
  then-name ordering, and ABI-world validation are implemented. Dependency declarations, component
  replacement, and any serialize/restore policy remain future work.
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

- [The plugin API: Two tiers](../plugin-api.md#two-tiers) — the doctrine this ABI serializes and
  the ancestor of this plan's shape 3.
- [The integrated and dedicated server](../dedicated-server.md) and
  [`server-ecs-migration.md`](./server-ecs-migration.md) — the server `World` and the Phase 2
  registration point M6 waits for.
- `docs/plans/paper-nms-bridge.md` — the census proving the server has no reachable seam yet; the
  JVM tier is a future *native* plugin and is unaffected by the wasm tier except that both consume
  the same public surface.
- The requirements table above is the complete record of decisions this plan establishes.
