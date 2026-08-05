# The WASM plugin host — runtime plugin loading

## What it is

Lodestone's second plugin tier: `crates/lodestone-wasm-host` embeds `wasmtime`, loads a WebAssembly
component from a **file on disk at runtime**, and drives it through a capability-gated ABI defined in
WIT. It is what makes "install a plugin without rebuilding" true at all — the thing
[`docs/plugin-api.md`](plugin-api.md) is explicit the native tier categorically does not deliver,
where "install a plugin" means "add a dependency and rebuild". It closes issues
[#172](https://github.com/matteopolak/lodestone/issues/172) (scaffold),
[#173](https://github.com/matteopolak/lodestone/issues/173) (the queries-and-actions surface) and
[#175](https://github.com/matteopolak/lodestone/issues/175) (the manifest), and it is **additive** —
the native tier (`crates/plugins/lodestone-{autopilot,nav,event-logger}`) is untouched and stays the
right home for anything Baritone-shaped.

## How it works

```
  plugin.toml  ──parse──▶ Manifest ──requested capabilities──┐
  plugin.wasm  ──sniff──▶ component ────────────────────┐    │
                                                        ▼    ▼
                                             PluginHost::load_file
                                                        │
                                    Linker gets ONLY the granted imports
                                                        │
                                                        ▼
  Messages<GameEvent> ──lift──▶ list<event> ──▶ guest.on-tick ──▶ list<action>
                                                                       │
                                                                      lower
                                                                       ▼
                                                                  ActionQueue
```

| file | owns |
|---|---|
| `wit/lodestone-plugin.wit` | the ABI: the world, its types, and the capability interfaces |
| `src/host.rs` | the embedding — engine, one `Store` per guest, the gated `Linker`, fuel preemption |
| `src/capability.rs` | the capability vocabulary and the two enforcement mechanisms |
| `src/abi.rs` | lift `ClientEvent` → WIT `event`, lower WIT `action` → `ClientAction` |
| `src/conductor.rs` | `WasmHostPlugin` — the one native system that drives every guest |
| `src/manifest.rs` | `plugin.toml` |
| `crates/plugins/lodestone-chat-responder-wasm` | the worked example, built for `wasm32-unknown-unknown` |

### The ABI is the intent doctrine, not a new vocabulary

The load-bearing observation, and it belongs to
[`docs/plans/runtime-plugin-loading.md`](plans/runtime-plugin-loading.md) rather than to this crate:
**the intent doctrine is accidentally an ABI spec.** Every way a native plugin observes or acts is
already call-shaped or copy-shaped — `GameEvent(ClientEvent)` is a `Clone` value, an intent is a
small POD struct inserted and removed, an outcome is a small POD struct polled, an action is a value
pushed onto a `Vec`. None hands out a borrow into the `World`, and a surface that never hands out a
machine is exactly a surface that serialises.

So the WIT `event`/`action` variants are a **curated subset of the same vocabulary**, not a parallel
dialect. A plugin author graduates to the native tier by gaining APIs, not by rewriting against
different ones.

### Events push, actions return — and why that is not what the plan proposed

The plan specified `poll-events()` and `submit-actions()` as host imports. The shipped world instead
has the guest export `on-tick(events: list<event>) -> list<action>`. Two reasons, and the second
matters more:

1. **One boundary crossing per guest per tick instead of three.**
2. **A return value cannot be produced outside the guest's tick window.** A `submit-actions` import
   could be called from anywhere the guest reaches; a return value structurally cannot. That is what
   keeps the conductor the single writer of `ActionQueue`, which is
   [`docs/plugin-api.md`](plugin-api.md)'s clause 2.

### The conductor, and why it is one system

A guest cannot *be* a system — no Rust type identity with the host, so no `add_systems`, no ordering
against arbitrary sets. One native system drives every guest in sequence. That is not a workaround:
it means no guest can fork a sequence counter or race another guest's writes **even maliciously**,
because the worst a guest can do is return a list. Guests order among themselves by load order, which
`manifest::scan_directory` derives from their declared `priority`.

### Capabilities: two enforcement mechanisms, and which one you are relying on

This is the most important thing to understand before adding a capability, because the two halves have
very different security properties and look identical in a manifest.

| kind | example | enforced by | what a lying manifest gets |
|---|---|---|---|
| **import** | `fs:read` | the wasmtime `Linker` — the interface is simply absent | instantiation fails |
| **data-flow** | `observe:chat`, `act:chat` | the host's conductor, in Rust | events are never lifted; actions are counted and dropped |

An import capability is **structurally unforgeable**: the guest cannot call a function that was never
linked, and cannot even finish instantiating if it references one. Anything genuinely dangerous
(filesystem, network, subprocess) must be modelled as an import so it lands in that column.

**A manifest is a declaration. The `Linker` is the enforcement.** Nothing stops an author writing
`capabilities = []` and shipping a module that calls `filesystem.read-file` — and nothing needs to.
The manifest exists so an *honest* plugin is refused politely and early, with a message an operator can
act on.

### The capability probe

The claim above rests on a measured property of the component model, not on an assumption. Two
artifacts built from the **same source**, differing by one call:

```
[well-behaved] component imports: ["lodestone:plugin/logging@0.1.0"]
[misbehaving]  component imports: ["lodestone:plugin/filesystem@0.1.0", "lodestone:plugin/logging@0.1.0"]
```

A guest that never calls an import **does not import it** — the wasm linker drops the unreferenced
`extern` and `wit-component` encodes only what remains. So declaring `filesystem` in the world costs a
well-behaved plugin nothing, while a guest that actually calls it, without the grant, gets:

```
component imports instance `lodestone:plugin/filesystem@0.1.0`, but a matching implementation
was not found in the linker
```

`tests/capability_denial.rs` asserts three things, and no two are sufficient: the `Err` naming the
interface, an empty recording sink **and** an empty plugin list, and — the one that costs something to
build — the control in which the same module, granted the capability, records both of its reads. Without
that control, deleting the body of `filesystem::Host::read_file` would leave every other assertion
passing. It was additionally mutation-checked: granting `fs:read` makes the same module load, and the
test fails.

Defence in depth: even a *granted* plugin is confined to `with_filesystem_root`, so the fixture's
`/etc/passwd` read is refused while its in-root read returns bytes.

## Configuration

| knob | default | notes |
|---|---|---|
| `PluginHost::new(policy)` | — | use `CapabilitySet::default_policy()`: everything **except** `fs:read` |
| `with_fuel(n)` | `DEFAULT_FUEL_PER_TICK` = 10,000,000 | per-`on-tick` budget |
| `with_memory_limit(n)` | 32 MiB | per-guest linear memory |
| `with_filesystem_root(p)` | `None` | required in addition to `fs:read`; without it a granted plugin still reads nothing |
| `DEFAULT_PLUGIN_DIR` | `plugins` | one subdirectory per plugin, each with a `plugin.toml` |

`plugin.toml` is documented in full in `crates/plugins/lodestone-chat-responder-wasm/plugin.toml`,
which is a real file the host's own test suite parses so it cannot rot into invalidity.

### Installing the example plugin

```bash
cargo build --release --target wasm32-unknown-unknown \
  --manifest-path crates/plugins/lodestone-chat-responder-wasm/Cargo.toml
mkdir -p plugins/chat-responder
cp crates/plugins/lodestone-chat-responder-wasm/plugin.toml plugins/chat-responder/
cp crates/plugins/lodestone-chat-responder-wasm/target/wasm32-unknown-unknown/release/\
lodestone_chat_responder_wasm.wasm plugins/chat-responder/chat_responder.wasm
```

A plain `cargo build`, producing a plain core module — **no `cargo-component` needed**. The host sniffs
the wasm preamble and encodes it into a component itself, because requiring an extra tool on a plugin
author's PATH is friction with no security benefit.

## The size budget

Measured with `scripts/wasm-size.sh`, quoted from what it printed rather than from a doc:

| | gzip | ceiling | headroom |
|---|---|---|---|
| before this crate existed | 845,034 B | 1,600,000 B | 47.2% |
| after | 845,177 B | 1,600,000 B | 47.2% |

**The +143 B is not this crate.** `cargo tree --target wasm32-unknown-unknown` inside `web/` contains
zero references to `wasmtime`, `lodestone-wasm-host` or `wit-component`; the drift is other work landing
in the shared tree between the two runs. The structural check is the stronger evidence and is the one to
re-run.

Note also that an earlier "~10–13% headroom" figure is **wrong** and predates the repair of two `web/`
breaks; 47% is the measured number.

## How to change it, and the gotchas

**Adding an event or action** means editing three places, and the compiler catches only some of it:

1. `wit/lodestone-plugin.wit` — the variant.
2. `src/abi.rs` — the lift or the lower. `abi::capability_for` has **no wildcard arm** and `Action` is
   generated and not `#[non_exhaustive]`, so a new *action* that is not gated is a compile error. A new
   *event* is not: `ClientEvent` is `#[non_exhaustive]`, so `lift_event`'s wildcard is mandatory.
3. `src/capability.rs` — a new `Capability` if no existing one covers it. **Never grant an
   import-column capability in `default_policy`.**

Then bump the world version in both the `.wit` and `host::ABI_WORLD` if the meaning changed.

The gotchas, each of which cost something:

- **`instances(1)` on `StoreLimitsBuilder` is wrong.** A *component* is not one core instance —
  wasmtime instantiates the guest module plus the component model's adapter shims — so one plugin
  lands at two or more, and the failure reads *"resource limit exceeded: instance count too high at
  2"*, which looks like a runaway guest and is a host misconfiguration.
- **With `wasmtime`'s `default-features = false`, `wasmtime::Error` does not implement
  `std::error::Error`,** so `?` into an `anyhow::Error` does not compile. `HostError` captures messages
  with `{:?}`, which prints the whole causal chain — the part that names the missing import.
- **`TypedFunc::post_return` is a deprecated no-op in wasmtime 47.** Calling it warns and does nothing.
  A future version that reinstates the requirement would show up as a *trap*, not a warning.
- **A guest struct must not be named `Guest`** — `wit_bindgen::generate!` emits a trait by that name.
- **`CARGO_TARGET_TMPDIR` exists only for integration-test targets**, not for a lib's own unit tests,
  and `env!` on it there is a confusing compile error.
- **The guest crates are workspace-`exclude`d and are their own workspace roots.** Both halves are one
  decision: their `wit-bindgen` bindings declare `#[link(wasm_import_module = …)]` externs that do not
  link into a native `cdylib`, so a `--workspace --all-targets` check would go red on a crate that is
  not meant to be built that way.
- **Do not add this crate to `lodestone-app` or the shell unconditionally.** `wasmtime` is a JIT that
  maps executable pages and cannot be compiled *to* wasm32, so an unconditional edge would **break the
  browser build outright** rather than merely inflating it. A `cfg(not(target_arch = "wasm32"))` edge is
  the shape that works.

### Preemption: fuel, not epochs

The plan names epoch interruption. Epochs need a watchdog — something must call
`Engine::increment_epoch` on a timer — and a host that configures epoch deadlines *without* one has a
deadline that can never trip: an island, whose test would pass because the guest it was pointed at
never looped. Fuel needs only a budget, so fuel is what ships. `tests/preemption.rs` gates it against a
`--features spin` fixture that really does spin forever, with the well-behaved artifact under the *same*
budget as the control. Epochs and their watchdog are
[#176](https://github.com/matteopolak/lodestone/issues/176).

## Where the mapping is lossy

Written down rather than papered over. `src/abi.rs` carries the same table with more detail.

| native tier has | this tier has | why |
|---|---|---|
| `Text`, the styled component tree | a plain `String` | `Text` is recursive with translation keys, hover/click events and per-node style. Flattened with `to_plain_string`: a guest cannot see colour, cannot see a translation key, and cannot tell a translated message from a literal that renders the same. |
| `ChatAckInfo` on `ClientEvent::Chat` | dropped | signed-chat acknowledgement is the driver's bookkeeping; a guest echoing an `offset` would fork a sequence counter the driver owns. Deliberately unreachable. |
| ~110 `ClientEvent` variants | 3 | the curated subset — a full mirror is the staleness factory `lodestone_ecs::events`'s own module doc refuses. |
| ~55 `ClientAction` variants | 3 | same. |

### What is not in the world yet

**The intent half of the doctrine.** `BreakIntent`, `PlaceIntent`, `MovementIntent`, `LookIntent` and
their outcome components are *not* in `lodestone:plugin@0.1.0`. A guest can chat and swing; **it cannot
yet mine, place or steer.** That is a scope statement, not a bug: intents are install/remove-shaped
rather than value-shaped, so they need paired ABI calls the host mirrors into real component inserts and
removes, plus an outcome poll — a bigger surface than the one-crossing tick this world defines.
`PlaceOutcome::generation` is designed to cross unchanged when it does.

The residual staleness gap, stated honestly: nothing *automatic* stops the curated subset falling behind
`ClientEvent`, because `#[non_exhaustive]` forces a wildcard and no compiler error can fire. What
replaces that guard is the **subscription model** — a guest names the event kinds it wants in its
`plugin.toml`, and a kind this world does not define is a loud manifest rejection naming it. So the
plugin author finds out immediately, by name, at the point they ask. The host operator learns nothing,
and that is the gap.

## Pending on other work

- **Nothing in the shipped client registers `WasmHostPlugin` yet.** The tier is reachable and gated
  through the real `lodestone_app::client_app()` (`tests/reaches_the_real_action_queue.rs`), but no
  shell code calls it, so a `cargo run --release` client loads no wasm plugins. The wire is one
  `add_plugins` call plus a `load_directory`, and it must be target-gated per the browser note above.
- **The conductor sits in `TickSet::Predict`, not `TickSet::Send`,** because
  `lodestone_ecs::events::age_game_event_bus` is anchored in `Send` and is **private**, so a reader in
  the same set is unordered against the thing that trims the buffer it reads. If `lodestone-ecs` exposes
  a public ordering anchor for the ager, this moves to `Send` with `.before(…)` — a one-line change here
  and a patch the ECS owners would have to make.
- **Commands and permissions** ([#118](https://github.com/matteopolak/lodestone/issues/118)):
  command registration is a one-shot init-time declaration of plain data and command invocation is one
  more event variant, so both belong in this world — but `CommandRegistry` is in flight elsewhere. The
  world has no `declare-command` export yet, and adding one should not need a second ABI definition.
- **Scheduler / async** (`lodestone_ecs::scheduler`, `async_task`): a guest cannot hold a future across
  the boundary, so the shape is a `sleep(ticks)`-style request returning a token the host resolves into
  a later `on-tick` event. Not defined yet.
- **`Monitor`-tier read-only enforcement.** The native tier *enforces* that a `Monitor` system does not
  mutate the `World` (`lodestone_ecs::assert_monitor_system_is_read_only`). A wasm guest declaring
  `priority = "monitor"` is **not** yet held to the equivalent — it can still return actions.
- **Load order by declared dependency** ([#166](https://github.com/matteopolak/lodestone/issues/166)):
  there is deliberately no `depends` field. A field that is parsed and not enforced is worse than no
  field, because an author reads it as a guarantee.
- **Hot reload** ([#169](https://github.com/matteopolak/lodestone/issues/169)) is cheap here precisely
  because guest and host never share type identity, but is not implemented.
- **Browser plugin support is out of scope**, per the plan: `wasmtime` cannot run inside a wasm32 guest.
  The ABI being WIT means neither of the two structural outs (a `wasmi` backend, or `jco`
  transpilation) is foreclosed.

## Dependencies

- `wasmtime` 47, pinned minor, `default-features = false` with `runtime`/`cranelift`/`component-model`/
  `std` — and emphatically **not** `wasmtime-wasi`. A guest's only imports are the ones the `Linker`
  grants; there is no `fd_write`, no `path_open`, no clock and no socket for it to find. Worth stating
  precisely: "the sandbox denied it" and "the function does not exist" are different claims, and only
  the second is true here. It is the stronger one.
- `wit-component` 0.252 (kept aligned with wasmtime's own wasm-tools) for the core-module-to-component
  encode.
- `toml` + `serde` for the manifest; `thiserror`, `tracing`.
- `lodestone-model` (the vocabulary), `lodestone-ecs` (`GameEvent`, `ActionQueue`, `GameTick`,
  `TickSet`), `bevy_app`/`bevy_ecs`. The dependency arrow points host → ECS and never back, which is
  what keeps a wasm plugin invisible to every crate below.
- Guests need only `wit-bindgen` 0.57 and the vendored `.wit`.

## See also

- [`docs/plans/runtime-plugin-loading.md`](plans/runtime-plugin-loading.md) — the design this
  implements, including the runtime comparison and the per-tick cost predictions still to be measured.
- [`docs/plugin-api.md`](plugin-api.md) — the doctrine this ABI serialises.
- [`docs/plugin-registration.md`](plugin-registration.md) — `client_app()`, the seam
  `WasmHostPlugin` arrives through.
