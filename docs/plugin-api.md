# The plugin API

## What it is

The surface a third-party plugin uses to extend Lodestone: a native, compiled-in `bevy_ecs` plugin
tier with the same power as internal engine code, and a sandboxed WASM plugin host that loads a
capability-gated guest from disk at runtime. It covers reading and writing world/entity/player state,
expressing player-verb intent (break, place, move, look), commands, plugin messaging, persistent
per-plugin data, task scheduling, off-tick async work, and bulk world edits.

The driving requirement, from the project owner: plugins must be able to do everything native code can
do. Two consequences follow throughout this document: refusing a capability to plugins refuses it to
internal engine code too — there is no separate "internal API" — and the only genuinely privileged
internals are the network socket/driver task and the GPU device/queue/pipelines. Everything else is
single-writer state sitting behind an intent or a resource, reachable by any plugin that inserts the
right component or resource entry.

## How it works

### Two tiers

| | native `bevy_ecs` plugin | WASM host |
|---|---|---|
| power | everything native code can do | a curated capability ABI: events in, actions out |
| trust | fully trusted, no sandbox | untrusted-safe, capability-gated |
| loading | compiled into the binary, `App::add_plugins` | a `.wasm` file dropped in a directory, loaded at runtime |
| filesystem / network | unrestricted | denied unless a capability is granted |
| stability | pinned to the workspace's `bevy_ecs` version | Lodestone's own WIT-defined ABI, versioned independently |

A compiled-in bevy plugin is fully trusted, unsandboxed code — `add_plugins` is `dlopen`-equivalent
trust, and there is no capability check that could be added without contradicting the "everything
native can do" requirement itself. The WASM tier exists for automation that should not need that much
power; it is not a lighter substitute for the native tier or vice versa. A workload needing a resumable
off-thread search over an owned data snapshot (a pathfinder is the stress case) belongs on the native
tier, since every per-tick query across a WASM boundary is a host call with real overhead.

### Registration (native tier)

`lodestone-app`'s `client_app()` returns a composed, unfinalised `App` built from a small,
renderer-free, version-free set of plugins; a consumer calls `app.add_plugins(MyPlugin)` on it before
handing the `App` to a runner — headless, via `spawn_session` and `lodestone_client::ClientBuilder::ecs`,
or rendered, via `Sim::client_app()` (= `client_app()` plus the shell's own render-coupled plugins) and
`Sim::from_app`. `Sim::new` **is** those two calls in sequence, so the shell registers its own plugins
through the identical function a consumer calls — no private composition path can drift from the public
one.

`Sim::from_app` accepting a plugin was necessary but not sufficient: every real entry point into the
*windowed* client (`lodestone_shell::run`, `app::run`, `WindowApp::new`) still built its own `Sim::new`
under the hood, so a plugin could reach a headless consumer but never the shipped, on-screen game. The
entry point for that is `WindowApp::new_with_app` (and, above it, `run_with_app` at both `app::` and the
crate root) — a downstream crate composes an `App` from `Sim::client_app()`, adds its plugin, and calls
`lodestone_shell::run_with_app(app, config)` in place of `run(config)` to get the real winit-driven,
GPU-rendered client with the plugin's systems running, rather than a bare ticked `Sim`. Only
`Mode::Window` accepts an `App` this way — `Headless`/`Connect` are ownership-gated CLI diagnostics with
their own composition, and a headless session or bot consumer already has `ClientBuilder::ecs`.
`crates/lodestone-shell/src/app/tests.rs`'s `window_app_new_with_app_wires_a_callers_plugin_into_the_real_constructor`
is the gate, with a negative control on plain `WindowApp::new`.

Not every internal plugin can live in the renderer-free `client_app()`: the terrain mesher and the
pick-target/interaction plugin are coupled to shell-internal types, so a headless consumer gets
entity/local-player/session state and the `ActionQueue` egress, but no mesher, pick target, or
render-side interpolation — correct, since those exist only to feed a renderer. A new plugin belongs in
`client_app()` only if it is version-free and renderer-free; anything needing shell-internal block/net/
gpu types goes into `Sim::client_app()` instead, one crate up. `client_app()` installs plugins, never
session-scoped resources (the chunk store, mesh pool, and version adapter are built against a specific
world/protocol, so a runner inserts them after adoption). The session entity is spawned once, through
`lodestone_app::spawn_session`; a reconnect re-inserts the whole component set rather than resetting
field by field, so a component missed on reset would leak the old session's state into the new one.

### Schedules and ordering anchors

| schedule | cadence | public sets, in order |
|---|---|---|
| `NetIngest` | once per driver iteration | `IngestSet::{Drain, Apply, Index}` |
| `GameTick` | 20 Hz, ≤10 catch-up | `TickSet::{Input, Intent, Physics, Predict, Animate, Send}` |
| `Update` (bevy's own) | per frame | `FrameSet::{Input, Interpolate, Camera}` |
| `Extract` | per frame, last | `ExtractSet::{Terrain, Entities, Debug, Hud}` |

These are the anchors a plugin orders against — **sets, not system functions**, so internal systems can
be renamed or split freely while the set stays the ABI. `FrameSet::Camera` has no systems in it today;
a plugin instead overrides the drawn frame with `CameraOverride { position, yaw, pitch }` (insert to
take the camera, remove to hand it back — no near/far/FOV field, so it cannot open the wrong clip plane,
and it touches only this frame's pixels, not collision or audio). `lodestone-key-toggle`'s
`CameraTogglePlugin` is the compiled-in reference consumer: its claimed key inserts/removes one fixed
pose through `GameTick`, and its rendered-client control proves the pose reaches `Sim::render_camera`.
A fifth anchor type, `EventPriority`,
orders two third-party plugins against each other rather than against our systems (see "Events" below).

### The intent doctrine: five clauses

Every player-verb seam (`MovementIntent`, `LookIntent`, `BreakIntent`, `PlaceIntent`) follows this
shape, and a plugin author should copy it for anything similar:

1. **Observation vocabulary, never wire vocabulary.** `BreakIntent { pos, face }` and
   `PlaceIntent { pos, face }` are exactly the two facts a mouse-ray hit carries — no sequence number,
   no dig-state id, no raw `ClientAction`.
2. **Exactly one system owns each machine.** The dig/placement state machines, the prediction sequence
   counter, and the post-break cooldown are private state of one consumer system (the shell's
   `drive_mining`/`drive_placement`). A plugin depends on `lodestone-ecs`, never on the shell, so there
   is structurally one writer.
3. **Refusal is always observable.** `BreakOutcome`/`PlaceOutcome` are always-present components, so a
   plugin can poll from the first tick; rejections are typed, never a silent no-op.
   `PlaceOutcome::generation` bumps by one per resolved attempt, so a late poller can tell "the result of
   the attempt I just made" from "one I never read."
4. **Human input outranks installed intent, per verb, with no handshake.** A real player's own
   attack/use always wins; a plugin's intent left behind simply loses every tick the human is active.
5. **Lifecycle encodes verb shape.** A dig is continuous (`BreakIntent` stays until the plugin removes
   it); a placement is one-shot (the shell removes `PlaceIntent` the instant an attempt resolves,
   whatever the result — that removal is the acknowledgement).

`MovementIntent(MovementInput { forward, strafe, jump, sneak, sprint })` is genuinely analog, never
clamped between the component and the physics integrator. `LookIntent { yaw, pitch }` is distinct from
the camera, applied before physics reads yaw, and absent by default.

**Known gap, narrower than it looks:** the *human* break/place path does not go through the intent
seam's observation half — a human's dig never populates `BreakIntent`/`BreakOutcome`, so a plugin
cannot *watch* a human dig the way it watches its own intent. It can still **veto** one:
`drive_mining` asks `ActionVetoes` before advancing the dig state machine on both the human-driven
hit and the `BreakIntent`-driven one, in the same call, at the same site — `via_intent` only decides
whether `BreakOutcome` gets a written verdict afterward, never whether the veto itself is asked. A
protection plugin's `Deny` stops a real player's dig exactly as it stops another plugin's. The actual
gap is narrower: the human path can be *vetoed* but not *observed* through `BreakIntent`/`BreakOutcome`.

`crates/plugins/lodestone-block-jobs` is a reference producer: a plain queue of `BlockJob`s
(`Break`/`Place`, each a `pos`/`face` pair) that a `TickSet::Intent` system installs one at a time,
polling `BreakOutcome`/`PlaceOutcome` for completion before starting the next. It is the first
production call site for either component — see that crate's own doc for the state machine.

### What stays privileged

Two things are off-limits by construction, not by a permission check a plugin could route around:
**version types** (wire codecs and protocol-version types never cross into the engine crates a plugin
depends on — reachable only by depending on a version crate directly, at the cost of version-locking),
and **the GPU device, queue, and pipelines** (the renderer has no `bevy_ecs` dependency and is never in
the ECS; a plugin that wants to draw gets an `Extract`-time channel, never a `wgpu::Device` — hardware
constraints like the 4-bind-group floor are not something a plugin author can be expected to respect, so
this is a permanent ceiling, not a gap). Texture/model *substitution* is separate and already solved: the
resource-pack override stack and server-push resource packs swap textures with no rendering work at all,
the same way a real Bukkit/Paper plugin does it too.

### Reading and writing state

| kind | examples | notes |
|---|---|---|
| non-player entity components | `Position`, `Rotation`, `Velocity`, `Health`, `EntityFlags`, `Equipment`, … | mobs, dropped items, projectiles; see [Player simulation](./player-simulation.md) |
| local player components | `PhysicsState`, `MovementIntent`, `LookIntent`, `SelectedSlot`, `Flying`, `Dead` | see [Player simulation](./player-simulation.md) |
| session/HUD components | scoreboard, tab list, boss bar, health/food/experience, menu/phase | see [Player simulation](./player-simulation.md) |
| chunk world | `ChunkWorld` (read) / `ChunkWorldWrite` (the only write route) | a `Clone`-able handle over one shared `lodestone_world::World`; the read/write split means `Res<ChunkWorld>` compiles nowhere that mutates the store |
| block read/write | `block_state_at`, `set_block_with_physics`, `fill_region`/`fill_region_capturing` | see "Bulk world edits" below |
| outbound intent | `ActionQueue(pub Vec<ClientAction>)`, drained every tick | never push `BlockAction`/`UseItemOn` with a hand-fabricated prediction sequence — that number belongs to the mining/placement predictors, and forking it desyncs them |
| resources | `WorldTime { age, time_of_day }` | plus the local player/session/chunk-world state above |

Not yet reachable from a client plugin: per-entity attribute writes and AI-goal manipulation. Neither
has a client-side write path — vanilla has no client→server attribute-set packet, and AI-goal state is
server-only simulation state with no plugin-reachable component — so both belong on a future
server-side plugin surface.

### Extract-time draw channels and input interception

A plugin draws world-space geometry without touching the GPU device: `DebugLines`/`PluginBillboards`
are resources a system ordered `.in_set(ExtractSet::Debug)` appends to each frame, cleared before the
next with no writer. `PluginBillboard` is always camera-facing (position, size, colour, and either a
flat tint or a name the renderer's block atlas already resolves, falling back to the tint if
unresolved). A plugin can also claim a physical key via `PluginKeybinds`, in `Consume` mode (nothing
below it in the input chain sees the key) or `Observe` (gameplay still resolves it normally, the plugin
is just also told) — an open chat/menu/container always outranks a claim, while a claim outranks an
ordinary gameplay binding on the same key. `PhysicalKey` is a plain string (winit's `Debug` output)
rather than a mirrored enum, since `lodestone-ecs` ships to wasm and never depends on winit.

### Events: the game-event bus and cross-plugin priority

`GameEvent(pub ClientEvent)` is a bevy `Message` read with `MessageReader<GameEvent>` — the same
version-free, already-decoded vocabulary every ingest system folds into components. The one write site
pushes every event with no `match` on it, so a new `ClientEvent` variant cannot silently miss the bus.
It is gated off by default behind a marker resource, checked once at construction, so an unused client
pays nothing beyond one cached boolean per event.

Two plugins that have never heard of each other still need a shared order — the schedule anchors above
only order a plugin against *our* systems. `EventPriority::{Lowest, Low, Normal, High, Highest, Monitor}`
mirrors Bukkit's tiers, `.chain()`ed into all four public schedules. `Monitor` is enforced structurally:
a system with any mutable `World` access fails to register in that tier, checked against bevy's
per-system access metadata before scheduling (a `Monitor` system queuing a deferred `Commands` mutation
is the one known gap this check cannot see).

Raw wire-level packet observation does not exist yet; a plugin needing an undecoded packet type still
has no route but a direct version-crate dependency, at the cost of version-locking. **Decided:** the
packet-interception shape is observation-only, permanently, in both directions — a mutate/cancel/
inject-at-the-wire trait was considered and rejected, since inbound events apply inline under the
world's write guard (an interceptor needing `&mut World` there reintroduces a reentrancy hazard this
architecture makes unrepresentable elsewhere) and outbound mutation only exists inside a version-typed
adapter. Real-time anti-cheat and a disguise visible to *other* players both need outbound byte mutation
and stay out of reach; everything else (protection, economy, minigames, HUD mods, a client-side-only
disguise) is served by `ActionVetoes`/`EgressFilters` instead — see [`packet-wiring.md`](./packet-wiring.md).
This ceiling is scoped to the shared, version-free crates a native plugin ordinarily depends on. A
plugin willing to depend on a version crate directly and pay for version-locking has a real escape
hatch outside this ceiling — see [`plugin-packet-decorators.md`](./plugin-packet-decorators.md).

### Cross-plugin custom messages and channels

Any plugin can `#[derive(Message)]` its own type and an unrelated plugin can read it with
`MessageReader<T>` — `bevy_ecs` does not restrict this. What was missing was the pattern and one real
blocker: `bevy_app` panics on a duplicate `add_plugins`, and neither side can know if the other is
installed. The convention is a three-crate shape — `my-thing-api` (the message type and a registration
plugin, nothing else), `my-thing` (publisher), a subscriber depending on **`-api`**, never the publisher
— with `add_plugin_message::<T>()` checking `is_plugin_added` first so whichever side registers first
wins, and the `-api` plugin returning `is_unique() == false` so two crates adding it is not a
duplicate-plugin panic. Aging (`Messages<T>::update()`, or it grows unbounded) runs automatically at
`TickSet::Send` for every registered type.

`add_plugin_channel::<T>()` specialises this to Minecraft's `custom_payload` packet: implement
`PluginChannel` on a message type with a `CHANNEL` string (e.g. `"minecraft:brand"`), call
`add_plugin_channel::<T>()`, read with `MessageReader<T>`. Inbound dispatch runs
`.before(EventPriority::Lowest)` so every tier sees the same tick's payloads; outbound runs
`.after(EventPriority::Monitor)` so anything written this tick reaches the wire this tick.
`add_plugin_channel` also installs the game-event bus on the plugin's behalf (without it a channel
decodes nothing, forever, with no error) and panics at build time on a malformed channel string rather
than silently never matching. `decode` returning `None` is not an error and never disconnects — vanilla
reads and discards an unparseable payload too. A built-in `minecraft:brand` channel plugin ships as the
worked example.

Server-side, `lodestone_server::plugin_channels::{PluginChannelRegistry, PluginChannelHandler}` are the
plugin-facing API over `custom_payload` — not just wire-level decode. `PluginChannelHandler` is the
trait a plugin implements per channel; `PluginChannelRegistry::register` installs one, `dispatch` feeds
it an inbound payload, and `broadcast` sends a payload to every connection sharing the registry.
`LanConfig::plugin_channels` carries one `PluginChannelRegistry` into every accepted connection (cloned
per connection, so registrations made before `open_to_lan`/`bind` reach every player who joins after),
which is what makes this a real cross-connection channel rather than a per-connection stub.

### Commands

A plugin populates a `CommandRegistry` resource in `Plugin::build` with an arena-shaped command tree
(`PluginCommand::new(name)`; literals/arguments hang off `NodeId`s the builder returns, since
Brigadier's fluent style does not translate to Rust without heavy `Box<dyn>` use). Registration refuses
a duplicate root literal/alias and a tree with no handler anywhere, up front. Dispatch strips a leading
`/`, rewrites an alias to its canonical literal, parses against the tree through a permission filter,
then walks the parsed path *backwards* for the nearest handler. Any node can carry a permission, and a
denied node is invisible **together with its whole subtree** — vanilla's actual semantics. The two
halves of that gate deliberately differ: a denied node fails `dispatch` loudly, naming the node (a
player needs to know the command exists and is not theirs), but is silently absent from tab-completion
(vanilla never sent it, so a suggestion would leak its existence). A missing `Permissions` resource is a
hard dispatch error, never an ungated fallback.

Argument primitives (`IntegerArgument`, `StringArgument`, …) come from `lodestone-command`; two
Minecraft-flavoured helpers need live state: `player_argument` is lenient (an offline name still
parses), `choice_argument` is strict (an unlisted value fails at parse). The client's own Brigadier-tree
decode type stays a separate type from `lodestone-command`'s construction API on purpose — one must
tolerate unknown wire ids, the other holds `dyn ArgumentType` trait objects.

### Task scheduler and off-tick async work

`TaskScheduler` is Bukkit's `runTaskLater`/`runTaskTimer`: `schedule_once(delay_ticks, f)` and
`schedule_repeating(delay_ticks, period_ticks, f)` return a `TaskId`; `cancel(id)` removes it. The
closure takes `&mut World`, which is sound because `run_due_tasks` is an *exclusive* system — the
`&mut World` a task receives is the driver's own guard one stack frame deeper, never a second lock.
Firing is an exact schedule: `schedule_once(0, f)` and `schedule_once(1, f)` both fire next tick; a
repeating task fires at `delay`, `delay + period`, … `run_due_tasks` anchors at `TickSet::Input`, the
earliest anchor, so a task can write `MovementIntent` and have the same tick's later sets act on it. A
task may schedule or cancel another from inside its own callback — anything scheduled is considered
starting next tick, never re-entered this one. Task closures must be `Send + Sync` (`Arc`, not `Rc`).

`AsyncTaskPool` is Bukkit's `runTaskAsynchronously` plus `runTask`: `spawn_with_handback(work, |result,
world: &mut World| {..})` runs the hand-back on the tick thread at a schedule point (recommended —
nothing has to remember to poll); `spawn(work) -> PendingTask<T>` is a `Component` a system polls with
`try_take()`. The off-tick closure takes **no parameters at all** — no `&World`, no `EcsHandle` — so
there is no argument through which it could violate the rule against blocking while a world guard is
held. A plugin can still defeat this by *capturing* an `EcsHandle` clone and blocking on it from the
worker thread; since Rust cannot forbid that at compile time, every pool worker is marked and an
internal reentrancy ledger panics loudly instead of hanging — real but partial protection, since a raw
`handle.read()`/`.write()` call (bypassing the sanctioned wrapper) still hangs, and a thread the plugin
spawns itself outside the pool is unmarked. On `wasm32` (no threads) both functions run the closure
inline instead — same API, just not off-tick; `AsyncTaskPool::runs_work_inline()` reports this.

### Persistent data and plugin config

`crates/plugins/lodestone-plugin-support` gives every plugin author the conveniences they would
otherwise reimplement: a per-plugin data directory and typed JSON config (mirroring
`JavaPlugin.getDataFolder()`/`getConfig()` — a missing or corrupt config loads as `T::default()`, never
a load error), and a namespaced (`"<plugin>:<key>"`) in-memory key-value store attached to an entity or
chunk (`EntityDataStore`/`ChunkDataStore`, mirroring `PersistentDataContainer`). Values are opaque
`serde_json::Value`, deliberately never decoded into named fields — a schema that decides which fields
to carry through by consulting a static name list is exactly the shape that silently drops data
elsewhere in this codebase whenever a field is present but unlisted; a future persistent (survives-
restart) tier must keep that property, carrying each entry through as one opaque blob per key. There is
no automatic eviction on entity despawn (a despawned id can be reused, so a stale entry could be read
back by a new occupant) — wiring that would require this crate to depend on the engine's ingest fold,
inverting the plugin→engine dependency direction every plugin crate is checked against; a plugin that
cares removes its own entries on observing a despawn via `GameEvent`.

### Bulk world edits

`lodestone_world::World` has the block read/write pair a WorldEdit-class plugin needs:
`block_state_at` (read), `set_block_with_physics(x, y, z, state, physics)` (write, returning the
previous state an undo history needs — `physics: true` additionally queues the six adjacent positions
for a neighbour-update pass that does not exist yet), and a batched pair,
`fill_region`/`fill_region_capturing`, that groups writes by chunk column instead of a hashmap lookup
per block. `crates/plugins/lodestone-worldedit` is the worked example, and is genuinely a *second*
plugin layered entirely on that API — conceptually the same as WorldEdit being a Java plugin rather than
a server feature, so no region-selection/undo logic lives in the engine crates. `EditSession` holds a
`ChunkWorldWrite` handle plus a capped undo/redo stack; `undo`/`redo` both call the same replay helper
(write each recorded position, capturing what was there as the opposite-direction record) so the two
directions cannot drift apart. `WorldEditPlugin` queues fill requests into a plain `Vec` resource and
drains it once per `GameTick`, the same synchronous-drain shape `ActionQueue` uses, for the same reason:
an edit must land before anything else that tick reads the world.

### The WASM plugin host

The second tier: `crates/lodestone-wasm-host` embeds `wasmtime`, loads a WebAssembly component from a
file on disk at runtime, and drives it through a capability-gated ABI defined in WIT — the one thing
that makes "install a plugin without rebuilding" literally true. It is additive: the native tier is
untouched and stays the right home for anything needing a resumable off-thread search over an owned
snapshot.

```
plugin.toml ──parse──▶ Manifest ──requested capabilities──┐
plugin.wasm ──sniff──▶ component ─────────────────────┐   │
                                                       ▼   ▼
                                            PluginHost::load_file
                                                       │
                                Linker gets ONLY the granted imports
                                                       │
Messages<GameEvent> ──lift──▶ list<event> ──▶ guest.on-tick ──┐
host tick ──▶ due guest tasks ──▶ guest.on-task ─────────────┴─▶ list<action> ──lower──▶ ActionQueue
```

The ABI is the intent doctrine, curated, not a parallel vocabulary — every way a native plugin observes
or acts is already call-shaped or copy-shaped, so the WIT `event`/`action` variants are a curated subset
of the same thing; none hands out a `World` borrow. The guest returns actions from host-invoked
`on-tick(events)` and `on-task(id, token)` callbacks rather than through a submit import. A return
value structurally cannot be produced outside one of those callback windows, which keeps one native
conductor the single writer of `ActionQueue` even against a malicious guest. A guest cannot itself be
a bevy system, so one conductor drives every guest in sequence, ordered by a `priority` field in its
manifest.

Two independent enforcement mechanisms have very different guarantees. An **import** capability
(e.g. `fs:read` or `schedule:tasks`) is enforced by the wasmtime `Linker` itself — the interface is
simply absent, so a guest
referencing it without the grant fails to instantiate at all, structurally unforgeable. A **data-flow**
capability (e.g. `observe:chat`, `act:chat`) is enforced by the host's own conductor code — events are
never lifted to an ungranted guest and its actions are refused, counted, and logged, which means the manifest is a
*declaration*, not the enforcement, and anything genuinely dangerous (filesystem, network, subprocess)
must be modelled as an import rather than trusted as data-flow.

`on-verdict(context)` is the synchronous cancellation half. It receives one copy-only typed context
for each existing action veto and returns only `allow` or `deny`. The conductor brokers it into
`ActionVetoes`, so it runs under the tick owner rather than re-entering the world. Eligible guests run
in deterministic load order (directory loading is manifest priority then name); the first denial
stops dispatch. A trap, fuel exhaustion, or future native context this ABI cannot represent denies the
current action, unloads the failing guest where applicable, and lets later actions be considered by
the remaining guests. The `veto:actions` data-flow capability gates delivery to the export.

The ABI includes copied look and movement intent plus one-shot placement: the guest cannot provide a
world handle, block state, held item, prediction sequence, or raw packet. Placement returns a finite,
generation-bounded result only to a guest granted `observe:place`; a multi-tick break claim remains
outside the ABI because it needs a separate cancellation and ownership contract. Command
registration/invocation, async equivalents, `Monitor`-tier enforcement for a guest, and declared
load-order dependencies are all named gaps. The native windowed client installs the WASM conductor
before `WindowApp` adopts its `App` and scans the cwd-relative `plugins/` directory through
`PluginHost::load_directory`. Browser plugin support is out of scope: `wasmtime` cannot itself run
inside a wasm32 guest.

## How to change it, and the gotchas

- **Ordering anchors are ABI.** `TickSet`/`IngestSet`/`FrameSet`/`ExtractSet`/`EventPriority` variants
  are consumed only as `SystemSet` labels, never pattern-matched, so internal systems stay renameable.
  Adding a variant is safe; renaming or removing one breaks every plugin ordered against it. Never add a
  system-*function* anchor instead of a set — a renamed function breaks every plugin naming it. When you
  add a variant, also add it to `CorePlugin`'s `configure_sets(...).chain(...)` call — an unchained
  variant carries no ordering guarantee at all, silently.
- **A `Resource` a plugin orders against must be truly `'static`-owned.** A subsystem still built around
  a borrow cannot be smuggled in as a resource, and workspace-wide `unsafe_code = "deny"` forecloses
  transmuting the lifetime away — for this repo's own crates only; an external plugin crate sets its
  own lints and is not sandboxed by it.
- **Name-keyed data belongs in a plain version-free function; state-keyed data belongs behind
  `VersionAdapter`.** A state id is renumbered every protocol version; a name-keyed constant (e.g.
  `lodestone_model::block_physics`) is stable across versions and needs no version crate at all.
- **A plugin deriving `Resource`/`Component`/`Message`/`Plugin` needs `bevy_ecs`/`bevy_app` as *direct*
  dependencies, not only `lodestone-ecs`.** The re-export lets a plugin *name* those types freely, but a
  derive macro expands to an absolute `bevy_ecs::…` path that only resolves if the compiling crate has
  it directly — pinned to the same `[workspace.dependencies]` entry, or the graph gets two incompatible
  copies.
- **A cross-plugin `-api` crate's dependency direction is easy to get backwards.** A subscriber depends
  on `-api`, never the publisher; adding the publisher "just for one helper" silently defeats the point
  while every behavioural test still passes.
- **`ActionQueue` still accepts a raw `ClientAction` with a hand-fabricated prediction sequence.**
  Prefer `BreakIntent`/`PlaceIntent`/`MovementIntent`/`LookIntent` for anything those seams cover.
- **WASM ABI changes touch three places, and the compiler only catches one cleanly:** the `.wit` world,
  the lift/lower in the host's ABI module (an ungated new *action* is a compile error since the
  generated enum is exhaustive; a new *event* is not, since `ClientEvent` is `#[non_exhaustive]`), and a
  new `Capability` if none covers it — never grant an import-column capability in the default policy.
  Never make the WASM host an unconditional dependency of the shell or `lodestone-app`: `wasmtime`
  cannot itself compile to wasm32, so an unconditional edge breaks the browser build outright; gate it
  `cfg(not(target_arch = "wasm32"))`.

## Configuration

**Native tier:** none. A plugin is a `Cargo.toml` dependency added with `App::add_plugins` — there is no
loading mechanism, feature flag, or manifest format yet. `GameEventBusPlugin`, `SchedulerPlugin`,
`AsyncTaskPoolPlugin`, and `PersistentDataPlugin` are each opt-in and install `CorePlugin` themselves if
absent.

**WASM tier:** `PluginHost::new(policy)` takes a `CapabilitySet` (`default_policy()` withholds
`fs:read`, `schedule:tasks`, `commands:register`, `act:look`, `act:movement`, `act:place`, and
`observe:place`); `with_fuel(n)` bounds each guest's per-host-tick instruction budget —
fuel rather than
epoch-based preemption, since an epoch deadline needs a watchdog and a host without one has a deadline
that never trips; `with_memory_limit(n)` bounds linear memory; `with_filesystem_root(p)` is required in
addition to `fs:read`, or a granted plugin still reads nothing. Each plugin is one subdirectory with its
own `plugin.toml` declaring capabilities, subscribed event kinds, and load-order priority. A plain
`cargo build` producing a core wasm module is enough — the host sniffs the preamble and encodes it into
a component itself, so no extra tool is required on a plugin author's `PATH`.

The shipped native windowed client uses `DEFAULT_PLUGIN_DIR`, the cwd-relative `plugins/` directory.
It does not create that directory: absence means an empty plugin set. Each invalid, ABI-mismatched, or
capability-denied child is logged and excluded without blocking valid siblings. Embedders and tests can
call `lodestone_shell::wasm_plugins::install_from_directory` with an explicit path before handing the
`App` to `Sim::from_app` or `run_with_app`. If the caller already installed `WasmHostPlugin`, that host
and its policy remain authoritative; the shell does not add a second loader.

## Dependencies

- `lodestone-ecs` → `bevy_app`, `bevy_ecs` (`default-features = false`, `features = ["std"]`, never
  `multi_threaded`), `parking_lot`, `lodestone-model`, `uuid`. Never a version crate — a workspace-wide
  isolation check enforces this.
- A native plugin crate depends on `lodestone-ecs`, and — if it needs version data unavailable through
  the seam — may additionally depend on a version crate directly, version-locking itself. Deriving its
  own `Resource`/`Component`/`Plugin`-adjacent types needs `bevy_ecs`/`bevy_app` as direct dependencies.
- `lodestone-plugin-support` → `lodestone-auth`, `lodestone-ecs`, `lodestone-world`, `serde`/`serde_json`.
- `lodestone-wasm-host` → `wasmtime` (pinned minor, `default-features = false`, emphatically **not**
  `wasmtime-wasi` — a guest's only imports are whatever the `Linker` grants), `wit-component`,
  `toml`/`serde`, `lodestone-model`, `lodestone-ecs`. The dependency arrow points host → ECS and never
  back, keeping a WASM plugin invisible to every crate below it. Guest crates need only `wit-bindgen`
  and the vendored `.wit` file, and are workspace-`exclude`d as their own workspace roots.
- An island detector (`cargo xtask check-connected`) treats `lodestone-ecs` as non-allowlisted, so a
  component or resource set landing with no consumer shows up red rather than shipping silently.

## See also

- [Player simulation](./player-simulation.md) — the full entity, local-player, and session/HUD
  component sets, including the three-state encoding that governs when a component should be absent
  versus present-with-`None`.
- [`docs/packet-wiring.md`](./packet-wiring.md) — `ActionVetoes` and `EgressFilters`, the pre-check veto
  and outbound-filter layers that answer "how does a plugin stop an action" without wire access.
- [`docs/autonomous-navigation.md`](./autonomous-navigation.md) — `lodestone-autopilot`, the native
  plugin that exercises the intent seam end to end.
- [`docs/architecture.md`](./architecture.md) — where this surface sits in the wider crate graph.
- [`docs/roadmap/plugin-framework.md`](./roadmap/plugin-framework.md) — the capability audit this
  document's gaps are tracked against.
- [`docs/plans/runtime-plugin-loading.md`](./plans/runtime-plugin-loading.md) — the design the WASM host
  implements.
