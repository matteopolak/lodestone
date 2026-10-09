# Java plugin bridge: backing Paper's internal bytecode uses with Rust

## What it is

A design, a measurement and a foundation crate (`crates/plugins/lodestone-jvm-bridge`) for running unmodified Bukkit/Paper plugin jars against this server at zero cost when no Java plugin is loaded. The JVM runtime boundary is opt-in; the complete bridge is not built.

The approach: Paper bridges the Bukkit API onto vanilla's internal classes, so if Lodestone supplies classes with those internal names and signatures backed by JNI calls into Rust, Paper's own bytecode does the Bukkit translation, including the event bus, listener priorities and cancellation.

## How it works

### Census: sizing the surface

`crates/lodestone-nms-census` is a JVM-free class-file reader that walks method bytecode and counts static uses of vanilla-internal members from the Bukkit layer. Run against the patched Paper 26.2 build 121 server jar:

| quantity | value |
|---|---|
| classes parsed / parse failures | 10,353 / 0 |
| distinct static internal member operations from the Bukkit layer | 7,179 |
| external static instruction sites | 16,853 |
| all static internal sites including engine-internal | 379,852 |

The distinct-operation surface is about 15x smaller than the engine, which makes the job bounded and enumerable. The 7,179 figure is a lower bound: reflection and `MethodHandle` bootstraps are invisible to an instruction walk.

Findings that shape the design:

- **Entities and world/level are 54% of the symbolic surface**, exactly the subsystems the server already implements.
- **Field access is the biggest unresolved risk.** About 4,500 static field reads and 450 writes cannot be backed by a `native` shim. A shim must either declare a real Java field kept in sync, or a classload bytecode rewriter must turn field access into accessor calls.
- **143 members are Paper-injected**, with Bukkit-facing types in their own signatures. The shim set is therefore coupled to the Paper version, and Rust must construct Java objects defined by the user's Paper jar.

Two traps when measuring: the Paper download is a paperclip launcher holding a binary patch (scanning it reports zero internal references, a confident wrong answer), and the vanilla server jar nests the real classes one level down (the scanner recurses by default). Reproduce:

```bash
./scripts/fetch-paper.sh          # pinned version/build, sha256-verified, into .cache/paper/
container run --rm -v <work>:/work -w /work eclipse-temurin:25-jdk \
    java -Dpaperclip.patchonly=true -jar /work/paper.jar
cargo run --release -p lodestone-nms-census --bin nms-census -- \
    <work>/versions/26.2/paper-26.2.jar --prefix <target-package/> --top 40
```

### Licensing

Lodestone code is `GPL-3.0-or-later`. The design ships no Paper bytecode: the user supplies the Paper jar, the classloader redirects internal classes to Lodestone shims at load time, and the combination is assembled on the user's machine.

Never ship a modified Paper jar, a derivative patch set or a vendored Bukkit API, and never bundle or auto-download Paper. Two questions need counsel before any release containing shim classes: whether name/signature shims (especially of Paper-injected members) are a derivative work, and whether runtime combination in the user's JVM creates a combined work.

### Threading and reentrancy

`lodestone_ecs::EcsHandle` is a non-reentrant `Arc<RwLock<World>>`. Dispatching Java while holding a guard, then letting the handler call back, deadlocks silently one JNI frame deeper than any Rust stack trace shows. Three properties make that unrepresentable:

1. A Java handler runs on its own thread; the tick thread only services the port.
2. `lodestone_jvm_bridge::WorldPort` is all Java can hold. It has only a `SyncSender` and a `Duration`, so no `World`, `EcsHandle` or guard is reachable. A silent servicer is a reported timeout, not a hang.
3. `port::service_with_world` takes the guard itself, once per request, via `hold_write`. Calling it inside an existing guard panics naming both sites. Never hold one guard around a batch.

```
tick thread                              JVM thread
hold_write: collect events
drop guard                    -- event -> handler runs
                                          port.request(...)
service_with_world:  <--------------------+
  hold_write (short)  -- answer ------->  handler continues
  drop guard
                              <- result - handler returns
hold_write: apply result
```

`tests/reentrancy.rs` guards this, each gate paired with a control proving its detector fires. The grep gate lives in a different file from the one it greps. The wedge control hangs rather than panics, because a panic on a fresh thread aborts under the Cranelift debug backend.

Not closed: plugin-created async threads, ordering between concurrent handlers, and production wiring of `CallbackDepthGuard` at every native callback boundary (default budget `DEFAULT_CALLBACK_DEPTH_LIMIT`; over-limit becomes a bounded Java `RuntimeException`).

### Classload interception and JNI spikes

Both are executed, under Apple `container` (the host has no Java; see `docs/oracle-runtimes.md`).

- `crates/plugins/lodestone-jvm-bridge/spike/` compiles a caller once, then loads it through two loaders differing in one path element. The control answers `REAL`, the shim arm `SHIM`, and an intercepted class can carry a `native` method. Both loaders must take the **platform** loader as parent; with the system loader the shim is silently bypassed.
- `spike/invocation/` is a separate Cargo workspace: one JVM per process, `RegisterNatives`, a callback through `WorldPort` to a servicer on a different thread (predicted result 422 for `(11, 7, -3)`), plus dropped, timed-out, unregistered and panicking arms. Panics are caught and thrown as Java exceptions; no unwind crosses FFI.
- `spike/invocation/src/bin/intercepted.rs` composes the two. Run via `spike/invocation/run.sh`; `tests/invocation_spike.rs` is an ignored live gate.

These prove mechanism only, with stand-in classes, not that Paper's surface is shimmable.

### Production runtime boundary

All behind the default-off `jvm` feature (`paper-preflight` for archive validation without a JVM).

- `runtime::JvmRuntime::start` and `with_attached_thread` give a scoped JNI environment that receives no ECS handle. `load_isolated_class` uses a `URLClassLoader` with the platform loader as parent and `JvmConfig::with_classpath` order, so a shim directory placed first wins over operator jars.
- `paper::PaperBootstrapConfig::discover` opens the server jar and plugin directory in place (never extracting). Each top-level plugin jar needs exactly one of `paper-plugin.yml` or `plugin.yml`, with a deliberately narrow descriptor reader (scalar `name`, `version`, `main`; 64 KiB cap; at most 256 plugins). Entry classes are checked for class-file magic before the JVM starts.
- `PaperBootstrapPlan::lifecycle_load_requests` orders loads: bootstrap loader (shim paths plus server jar) first, then one fresh child loader per plugin containing only its jar. A plugin Load failure is isolated; a bootstrap failure is terminal.
- Lifecycle states are Load, Construct, Enable, Disable. `EntryConstructionOnly` permits a zero-argument constructor, then `onEnable` in discovery order and `onDisable` in reverse, all synchronously on the adapter worker with failures isolated per descriptor. This is an experiment on bridge-defined entry objects, not Bukkit lifecycle semantics.
- `native_surface` generates the shim's native declarations from one Rust `NativeMethodSpec` list, with typed `NativeSurfaceError`s. `lodestone.bridge.IsolatedPaperShim` is the shared definition every plugin child inherits.
- Field access is deliberately not exposed: reading a static field initializes its class, contradicting the non-initializing load contract.

### Adapter worker

`adapter::AdapterHost` loads an explicit adapter class on one dedicated worker, validates its static methods, registers natives and dispatches `onTick(long)` and `onBlockStateChanged(...)` through a single command slot (no backlog, no overlap). Natives get a thread-local `WorldPort`; a Java-created thread calling one gets a named error, and native queries from class initializers are unsupported.

- A missed deadline is terminal; arbitrary Java cannot be killed in-process, so the host drops the adapter and reports.
- There is no hot reload or JVM restart: JNI allows one JVM startup per process, including after a failed load.
- The shim surface is value-only and bounded: resident block-state read and write (resident columns only, never generating or reading disk, unavailable terrain is an error rather than air), server tick count, active plugin descriptor name/version/main class and lifecycle phase, and player handles (name, UUID, position, rotation, entity ID, game mode, experience, native inventory slot key/count, teleport) with several exact-match, prefix and enumeration resolvers that fail loudly on null, unknown or ambiguous input.
- One typed event seam exists: `ResidentBlockChangeListener.onResidentBlockStateChanged`, registered via `IsolatedPaperShim.subscribeResidentBlockStateChanges`. Listeners are activated after `onEnable` succeeds, removed on any failure or disable, capped at 64, and a listener exception is recorded without rolling back the change. It has no priorities, cancellation or event classes.
- Operator-selected control members (a static `()I`, `()J` and `(long): int` block-state member) are registered in the bootstrap loader before any plugin child exists, validated by exact descriptor.

Native declarations are optional per adapter: the worker registers only the exact-descriptor subset a class declares.

### Dedicated-host connection

`JavaAdapter::poll` in the dedicated server is the live producer, using `IntegratedServer::resident_block_state_id`, `set_resident_block_state_id` and `server_tick_count`. The admin loop services at most 64 block queries, 64 block writes, 64 player-position queries and 64 tick queries per 1 ms poll. Successful writes enter a FIFO of at most 64 `BlockStateWrite` callbacks, and no more writes are applied while it is full. Intervening ticks are coalesced rather than replayed.

Player join and disconnect reach the worker through a bounded value-only queue; a disconnect advances the handle generation, so a reconnect gets a different `long`. The host never hands Java a server object, connection, ECS entity or world guard. An asynchronous adapter failure disables the adapter while the server keeps running; shutdown drops the adapter before saving.

### ABI decision

A compile-time optional feature on an `rlib`, not `cdylib` plus `libloading`. A runtime-loadable bridge would force a stable `repr(C)` ABI on every crossing type forever, while the default-off feature already gives zero cost. Moving to a C ABI later is cheap; unwinding one is not. Revisit only if one-download binary distribution with a runtime Java toggle becomes the main way people run this.

`tests/zero_cost_graph.rs` makes zero cost checkable: production edges to the bridge must be optional and default-off, and the bridge names no unconditional JVM-linking crate. It scans manifests rather than `Cargo.lock` or `cargo tree`, because `jni` is already in the lockfile via target-gated dependencies and a nested cargo call contends on the package-cache lock.

### Object identity

`identity::ObjectRegistry` is a generational slot map for Bukkit-held `Player`/`World`/`Block` references. A stale reference must fail distinguishably, and a reused slot must never answer for the old object.

- `ObjectRef::to_bits` does not pack the kind; the kind is read from the slot, so a plugin cannot relabel a `Block` handle as a `Player`.
- Generations saturate rather than wrap; a saturated slot is retired.
- One object has exactly one live handle, so plugin identity comparison works.
- `try_handle_for` is fallible, so a full registry reports instead of growing.

## How to change it

- Never add a handle field to `WorldPort` or remove the request deadline; it still compiles and re-opens the deadlock. `tests/reentrancy.rs` catches it.
- Never put the bridge in a crate's default dependencies. Wire it behind an optional dependency and default-off feature, and update `zero_cost_graph.rs` to assert that shape.
- Extending the native surface: change the declaration in `native_surface`, the JNI registration and worker dispatch in `adapter`, and lifecycle cleanup in `paper`, together, then add an independently predicted end-to-end fixture. Add concrete host queries on demand; do not enumerate a speculative compatibility surface.
- Keep the two spikes separate so a failure stays unambiguous.
- Re-pin the census input (`scripts/fetch-paper.sh` carries version, build and sha256) and update the numbers together. A suspiciously tiny census means the paperclip jar was scanned.
- Ignored JVM gates need `JAVA_HOME` and a fresh process each, for example `cargo test -p lodestone-jvm-bridge --features jvm --test adapter_host -- --ignored`.

## Configuration

Dedicated host (needs the `jvm` feature; configuration given to a build without it is a reported error then clean shutdown):

| variable | meaning |
|---|---|
| `LODESTONE_JAVA_ADAPTER_CLASS` | dotted adapter class name |
| `LODESTONE_JAVA_CLASSPATH` | platform path list of directories/jars, in loader resolution order |
| `LODESTONE_JAVA_DEADLINE_MS` | startup/callback deadline, default 5000 |
| `LODESTONE_PAPER_JAR`, `LODESTONE_PAPER_PLUGIN_DIRECTORY` | Paper bootstrap intake |
| `LODESTONE_PAPER_SHIM_PATH` | shim directory or jar with the `IsolatedPaperShim` declarations; without it no facade exists |
| `LODESTONE_PAPER_OPERATOR_BLOCK_STATE_CLASS` / `_METHOD` | one operator `(long): int` member; both or neither, requires the shim path |

Constants: `port::DEFAULT_REQUEST_DEADLINE`, `callback::DEFAULT_CALLBACK_DEPTH_LIMIT`, `adapter::MAX_RESIDENT_OBJECT_HANDLES` (shared by block and player handles), and `MAX_PENDING_PLAYER_LIFECYCLE_EVENTS` in the dedicated host. `nms-census` flags: `--prefix`, `--internal`, `--no-recurse`, `--top`, `--all`.

## Dependencies

- `lodestone-nms-census`: `zip`, `anyhow`; no JVM, so it runs on a machine without Java.
- `lodestone-jvm-bridge`: `lodestone-ecs`, plus `lodestone-plugin-support` as a dev-dependency; `jni` only under `jvm`.
- The spikes and the paperclip step need Apple `container` and an `eclipse-temurin` image; the invocation spike's `Containerfile` pins its base image, toolchain and Temurin checksums.
