# Java plugin bridge: backing Paper's static internal bytecode uses with Rust

## What it is

A design, a measurement, and a foundation crate for running **real, unmodified
Bukkit/Spigot/Paper plugin jars** against this server, with **zero cost when no Java plugin is
loaded**. The crate's JVM runtime boundary is opt-in; the complete plugin bridge remains future
work.

The approach is not "reimplement the Bukkit API in Rust". Bukkit is only the *API*;
Paper/CraftBukkit is the *implementation* that bridges Bukkit interfaces onto vanilla's own
internal classes (NMS — the internal package tree Bukkit's implementation layer sits on top of).
Each Bukkit-facing wrapper type is a thin shell around one internal counterpart: a player wrapper
around the internal player object, a world wrapper around the internal level object, a block
wrapper reading directly from internal block state. So if we supply classes with **NMS names and
signatures** backed by JNI calls into Rust, Paper's own bytecode does all the Bukkit translation
for us — including the event bus,
listener priorities and cancellation semantics, which the plugin-API audit identified as among the
hardest things to express natively in an ECS.

This document covers what has been **settled and built**: the licensing decision, the threading and
reentrancy design, the ABI decision, the object-identity design, and — the measurement the whole
estimate turns on — the **NMS reference census**, run against a real Paper jar.

The complete bridge itself is not built. What is built is `crates/plugins/lodestone-jvm-bridge`
(the JVM-independent host machinery plus an opt-in `jvm` runtime boundary),
`crates/lodestone-nms-census` (the scanner), an executed classload-interception spike, and a
separate JNI-invocation spike that drives native methods through the real
`WorldPort`/`PortServicer` pair.

---

## 1. The census — the measurement that sizes everything else

### 1.1 The headline

Scanned: **Paper 26.2 build 121** (STABLE, published 2026-08-29), the patched server jar
`versions/26.2/paper-26.2.jar` produced by running paperclip. 26.2 is the Minecraft version this
repo targets, so the census lines up with the decompiled source already under `.cache/mc/26.2/src`.

| quantity | value |
|---|---|
| classes parsed | 10,353 |
| **parse failures** | **0** |
| vanilla-internal classes defined in the jar | 7,492 |
| **distinct static NMS member operations encoded by the Bukkit layer** | **7,179** |
| distinct NMS classes carrying those operations | 1,395 |
| external static instruction sites | 16,853 |
| symbolic pool members from the Bukkit layer | 6,991 |
| *all* static NMS instruction sites including engine-internal | 379,852 |

**The ratio is the finding.** Paper's own bytecode encodes 379,852 NMS member instruction sites,
but that is the size of the game engine, not the bridge. The external layer encodes 16,853 static
sites across **7,179 distinct member-operation keys**. The all-package table has 106,095 distinct
keys, so the distinct-operation surface is a **14.8× reduction**. That number is what separates
"reimplement the game" from "implement a bounded, enumerable API".

The scanner retains the former **6,991 symbolic pool members** separately. A pool entry can be
needed by a descriptor or bootstrap path without a `get*`, `put*`, or `invoke*` instruction in that
class, so it is useful context but not a static-site work count.

### 1.2 Composition of the surface

| static instruction kind | distinct operations | external sites |
|---|---:|---:|
| `invokevirtual` | 3,466 | 8,619 |
| `invokespecial` | 419 | 944 |
| `invokestatic` | 427 | 1,218 |
| `invokeinterface` | 474 | 1,091 |
| `getfield` | 533 | 1,653 |
| `getstatic` | 1,552 | 2,882 |
| `putfield` | 305 | 441 |
| `putstatic` | 3 | 5 |

**The 4,535 static field reads and 446 writes are the uncomfortable part.** A method can be
backed by a `native` shim; a field operation cannot. A shim class must either *declare a real Java
field* whose value is kept in sync with Rust-owned state, or field access must be rewritten to
accessor calls during classload transformation. The first is cheap for immutable-after-construction
data and wrong for live state; the second turns "supply shim classes" into "supply shim classes
*and* a bytecode rewriter", which is a materially larger project. **This is unresolved and is the
largest single design risk below.**

### 1.3 Symbolic context retained from the former pool census

The following grouped ranking is the **former symbolic-pool census** (6,991 members), retained as
context only. It does not rank the 7,179 static instruction operations above: one symbolic member
can have no instruction site in a class, while one member can occur in several instruction forms.
Do not use this table to choose implementation order until the static sites are grouped by
subsystem too.

| subsystem | symbolic members |
|---|---|
| entities | 1,924 |
| world/level | 1,829 |
| items | 616 |
| server-side level state | 350 |
| data components | 160 |
| inventories | 129 |
| dedicated-server config | 111 |
| damage sources | 97 |
| network protocol | 97 |
| chat networking | 86 |

Entities and world/level are **54%** of that symbolic surface. That is the good news the
issue predicted: those are precisely the subsystems this repo already implements, so Java-plugin
compatibility really does reduce largely to server parity rather than being a separate project.

Most-referenced symbolic types, by rough category: the base entity type (274 refs), the item-data-component
registry (257), the server-side player type (223), the server-side level/world type (212), the
server singleton (207), the generic compound-tag/NBT container (191), the entity-type registry
(191), the world-generation-facing level view (170), the registry-of-registries type (165), and
the block-state type (152).

Most-referenced symbolic members are historical context, not a static-site ordering: a
server-singleton getter (55 refs), a block-state
property's bound (50, a field), a registry-key accessor (39), a holder unwrap (31), a
default-block-state constructor (30), a wrapper-object accessor that hands back the Bukkit-facing
object for a given internal one (28), a state-to-block accessor (24), and a chunk-source accessor
on the server-side level type (23).

### 1.4 The finding that most changes the design: Paper-injected members

**143 of the 6,991 symbolic members have Bukkit-facing types in their own signature.** Examples of the
shape: an entity's internal type gaining an accessor that hands back its Bukkit-facing wrapper
object; a block-state type gaining an accessor that hands back Bukkit-facing block data; a
server-side level type gaining an accessor that hands back its Bukkit-facing world object; the
server singleton gaining a field that holds its Bukkit-facing server object; a generic
inventory-holder type gaining an accessor that hands back a Bukkit inventory-holder interface.

**These members do not exist in vanilla's own internals.** They are Paper's own patches *into*
those internal classes, and they are how the Bukkit-facing wrapper layer finds its counterpart
object. So the shim layer is not "supply vanilla's internal API"; it is "supply vanilla's internal
API **plus Paper's injected hooks**, and construct real Paper-defined Bukkit-facing objects from
Rust". That is a genuine bidirectional seam — Rust must instantiate
Java objects belonging to the user's Paper jar — and it means the shim set is **Paper-version-coupled
in a way the vanilla NMS surface is not**. It does not invalidate the approach; it does mean "adapts
across Paper versions for free" is too strong a claim.

### 1.5 Two traps in obtaining these numbers, both measured

**Trap 1: the download contains no server.** The Paper distribution is a *paperclip* launcher: its
`META-INF/versions/26.2/` holds `server-26.2.jar.patch`, a binary patch, not classes. Scanning the
downloaded jar directly gives **10,500 classes and zero references into vanilla's own internals** —
a confident, well-formed, completely wrong census that reads as good news. The real jar only exists
after paperclip applies its patch to a vanilla jar, which needs a JVM.

**Trap 2: jars nest, and so does the vanilla one.** `.cache/mc/26.2/server.jar` is Mojang's bundler:
**4 classes** at the top level (the bundler's own entry class and its helpers), with the real server
one level down.
Scanning it without recursion finds 4 classes; with recursion, **30,563**. The scanner recurses by
default for this reason and `--no-recurse` exists mainly to keep the difference measurable.

Both traps have the same shape and it is one this repo already records: *an instrument that reports
a small confident number for input it could not actually read*. The scanner therefore reports
`parse failures` on every run and never silently skips a class.

### 1.6 Reproducing it

```bash
./scripts/fetch-paper.sh          # pinned to 26.2 build 121, sha256-verified
# then materialise the real server jar (paperclip needs a JVM; see docs/oracle-runtimes.md)
container run --rm -v <work>:/work -w /work eclipse-temurin:25-jdk \
    java -Dpaperclip.patchonly=true -jar /work/paper.jar
cargo run --release -p lodestone-nms-census --bin nms-census -- \
    <work>/versions/26.2/paper-26.2.jar --prefix <target-package/> --top 40
```

Pre-seed `<work>/cache/mojang_26.2.jar` from `.cache/mc/26.2/server.jar` to save paperclip a
download. **The Paper jar is a local measurement input: it is not committed, not redistributed, and
`.cache/` is outside git deliberately** — see §2.

### 1.7 How far to trust the scanner

`crates/lodestone-nms-census` is a class-file reader (JVMS chapter 4), no JVM involved. It walks
method `Code` bytecode instruction by instruction; it never searches raw bytes, so opcode-valued
operands and variable-width switch payloads cannot become false uses. Evidence it is correct, in
the order it was gathered:

- **Hermetic**, against a class file hand-expanded from the specification —
  `tests/classfile_fixture.rs`. The fixture contains a two-slot `Long`, a repeated member use, all
  four field directions, an opcode-valued operand, both variable-width switch forms, and `wide`.
  Each case makes a common parser shortcut produce a different count.
- **At scale**, against the local pinned Paper 26.2 server: **10,353 classes, 0 parse failures,
  7,179 external static member operations, and 16,853 external instruction sites**. The ignored
  `tests/vanilla_jar.rs` gate records those exact values for deliberate reruns.
- **End to end**, a jar built in-test containing the fixture preserves three symbolic pool entries,
  counts ten static uses, and exposes eight distinct operations attributed to the right referrer.

What it does **not** answer: a member reached purely through reflection or a `MethodHandle`
bootstrap has no member instruction, so it is invisible to the static-site table. The symbolic table
retains descriptor and bootstrap context, but neither table can discover a reflective target. The
7,179 static-operation baseline is therefore a lower bound rather than a total.

---

## 2. Licensing

**Nothing in this section is legal advice, and two questions below are explicitly flagged as
requiring counsel rather than an engineering call.** What follows is the reasoning as engineering
sees it, written down so counsel has something concrete to react to.

### 2.1 The facts

- **Paper is GPL-3.0.** CraftBukkit's patches and Paper's own code carry it.
- **The Bukkit API is GPL-3.0** as well (Spigot's API repackaging notwithstanding).
- **Vanilla Minecraft is proprietary** and neither we nor Paper may redistribute it — which is
  exactly why Paper ships a *patch* plus a launcher that downloads Mojang's jar at first run, rather
  than shipping the server (§1.5's trap 1 is that mechanism observed from outside).
- **Lodestone-owned code is `GPL-3.0-or-later`**, including the bridge and the crates under
  `crates/plugins/*`.

### 2.2 What we ship, and what we do not

The design's licensing property is that **we distribute no Paper bytecode, modified or otherwise.**

| artefact | who supplies it | licence consequence |
|---|---|---|
| the Paper jar | **the user**, obtained from PaperMC themselves | we never distribute it |
| shim classes with NMS names/signatures | us | see §2.3 |
| the Rust bridge crate | us | GPL-3.0-or-later |
| the classloader that redirects vanilla's own internal classes | us | our own code |
| the Paper jar used for the census | downloaded locally, **never committed** | a measurement input |

Paper is never patched, never repackaged, and never redistributed. Classload interception — proven
executable in §4 — is what makes that possible: Paper's already-compiled bytecode binds to our
classes at load time with **no modification to Paper at all**.

### 2.3 The engineering conclusion

**The bridge crate is `GPL-3.0-or-later`.** That project-wide license choice does not change the
bridge's clean-room and distribution boundaries:

1. **No derivation.** The crate contains no Paper source, no Paper bytecode, and no transcription of
   either. Its NMS-facing surface is derived from **Mojang's** de-obfuscated names — the same source
   the rest of this repo already ports from — not from Paper's expression of anything.
2. **No distribution of a derivative work.** GPL-3.0's copyleft obligations attach to *conveying* a
   work based on the Program. We convey no part of Paper. The user assembles the combination on
   their own machine, at their own runtime, from parts they obtained separately.

**What may not be shipped, under any framing:**

- a modified Paper jar, or any Paper class file;
- a patch set that is a derivative of Paper's patches;
- a vendored copy of the Bukkit API;
- anything that would make our distribution and Paper's a **single combined work** — in particular,
  **do not statically link or bundle Paper, and do not auto-download it as part of our installation**
  without counsel signing off on that specific flow.

### 2.4 The two questions for counsel

**These are the parts engineering should not decide.**

1. **Does an NMS-signature shim set constitute a derivative work of Paper — or of Minecraft?** Our
   shims replicate *interfaces* (names, signatures, descriptors) that Mojang authored and Paper
   depends on. Interface replication for interoperability has well-known treatment in some
   jurisdictions and is not settled everywhere. The §1.4 finding sharpens this considerably: the
   143 Paper-*injected* members (`getBukkitEntity`, `asBlockData`, `getWorld`, …) are **Paper's own
   API additions, not Mojang's**, so shimming those is replicating Paper's interface specifically.
   That is the narrowest and most pointed version of the question, and it should be put to counsel
   in exactly those terms.
2. **Does running our shims inside the user's JVM alongside Paper create a combined work at
   runtime, and if so does that obligate anything given we distribute no part of it?** The
   at-runtime-combination question is the classic GPL linking debate; the FSF's position and the
   case law are not the same thing. The relevant facts to hand counsel: the combination is created
   by the user, on the user's machine, from separately-obtained parts, and our side never links
   against Paper at build time.

A third, lower-stakes question worth asking at the same time: **may we distribute the *census
scanner*?** Engineering's view is plainly yes — it reads a jar's byte format and reproduces none of
its content, the same relationship `lodestone-anvil` has to a region file — but it is cheap to ask
in the same conversation.

**Until (1) and (2) are answered, do not publish a release containing shim classes.** Building them,
testing them locally, and measuring against a locally-downloaded Paper jar are all unaffected.

---

## 3. Threading and reentrancy — the hardest part

### 3.1 The hazard, stated precisely

`lodestone_ecs::EcsHandle` is `Arc<parking_lot::RwLock<World>>` and is **not reentrant**. Of the four
guard combinations, three deadlock always and the fourth deadlocks whenever a writer is queued. This
is not hypothetical: it froze this client outright on the first tick of the first block dig — **no
panic, no error, no log line** — because a system called a read-model accessor from inside the
schedule's write guard.

A Bukkit plugin is the easiest possible way to reintroduce it. Plugins assume one main thread and
call `world.getBlockAt()` freely from inside an event handler. A design that dispatches to Java
*while holding the guard* and lets the handler call back reproduces the exact shape — one JNI frame
deeper, where no Rust stack trace will show it.

`hold_read`/`hold_write` keep a thread-local ledger and **panic instead of hanging**, which is a
real backstop but not a total one: `EcsHandle` is a type alias for `Arc<RwLock<World>>`, so
`.read()`/`.write()` are `parking_lot`'s own inherent methods and cannot be intercepted.

### 3.2 The design: three properties, each with a named enforcer

The requirement is that the deadlock be **unrepresentable, not documented**. It is closed three
ways, because the three fail differently.

**Property 1 — a Java handler runs on its own thread, never on the tick thread.** During dispatch
the tick thread's job is to *service* the port, not to run the handler. Everything else follows: the
guard is only ever taken by the servicing thread, and that thread is by construction not inside one
when it dispatches.

**Property 2 — the Java side has no route to the lock, enforced by the type.**
`lodestone_jvm_bridge::WorldPort` is the only thing a JNI callback holds. It contains a
`SyncSender` and a `Duration`. There is **no field** from which a `World`, an `EcsHandle` or a guard
can be reached, and `channel()` — the only constructor — takes neither. The worst a misbehaving
handler can do is send a request nobody answers, which is a *reported timeout*, not a hang.

This is the same reasoning `docs/plugin-api.md` already records for the sanctioned plugin surface
("never place `EcsHandle` on the surface a plugin depends on") and for `AsyncTaskPool::spawn`'s
parameterless closure: give the callee no argument capable of reaching the lock, and reentrancy
stops being a discipline problem.

**Property 3 — wiring the servicer inside a guard is a loud panic, not a hang.**
`port::service_with_world` takes the guard **itself**, once per request, through
`lodestone_ecs::hold_write`. A host that ever calls it from inside an existing guard trips
`hold_write`'s thread-local ledger and gets a panic **naming both call sites**. The rule "dispatch
to Java outside any guard" therefore stops being a comment and becomes something the process checks
every time it runs. This deliberately reuses the existing ledger rather than inventing a second
mechanism for one invariant — the repo has a recorded case of the same hazard being independently
discovered and fixed twice because nothing connected the two call sites.

One guard **per request**, never one around the batch: `EcsHandle`'s whole safety argument is that no
guard spans a frame, and a guard held across a JNI round trip would span an unbounded one.

### 3.3 The dispatch sequence

```
tick thread                              JVM thread
-----------                              ----------
hold_write: collect events
drop guard                    ── event ─▶ handler runs
                                          port.request(...)  ─┐
service_with_world:  ◀────────────────────────────────────────┘
  hold_write (short)  ── answer ────────▶ handler continues
  drop guard
  ... repeat while handler runs
                              ◀─ result ─ handler returns
hold_write: apply result
```

The tick thread is never inside a guard while Java code runs. The JVM thread never holds one at all.

### 3.4 What guards this

`crates/plugins/lodestone-jvm-bridge/tests/reentrancy.rs`, and each gate is paired with a control
because an assertion of an absence is worth only the evidence that its detector fires:

| gate | control |
|---|---|
| this crate's manifest names neither `lodestone-shell` nor `lodestone-client` (so it has no route to an `EcsHandle` at all) — reuses `lodestone-plugin-support`'s `assert_ecs_only_dependency_graph` | that function's own `#[should_panic]` test |
| `WorldPort`'s **fields** name no lock type (source grep) | the same predicate over `lodestone-plugin-support/src/reentrancy.rs`, which genuinely does name `EcsHandle` and must be found |
| a Java-style callback completes under a real tick guard | a deliberate wedge — raw `handle.read()` inside a `hold_write` — which must time out |

Two details of those gates are deliberate and worth not undoing. The grep gate **lives in a different
file from the one it greps**: a source-grep gate placed inside its own subject matches its own
assertion string and passes with the defended line deleted. And the wedge control **hangs rather than
panics**, because a panic raised on a freshly-spawned thread aborts the process under this
workspace's Cranelift debug backend — a hang is backend-independent, is the shape the historical bug
actually had, and needs no carve-out in a shared manifest.

### 3.5 What is *not* closed

- **Async plugin threads.** Bukkit's scheduler has async tasks and JNI requires
  `AttachCurrentThread`. `WorldPort` is `Clone + Send` so each attached thread can hold its own, and
  the port's semantics are already thread-safe. The invocation spike proves that a Rust-created
  thread can attach, call Java, and receive a Java-to-Rust callback. Its composed intercepted-shim
  arm uses a scoped attachment and checks the JNI attachment count returns to its pre-call value;
  arbitrary plugin-created async threads and the fate of an outstanding request when one dies
  mid-call remain unproven.
- **Reentrancy in the other direction** — Rust calling Java calling Rust calling Java. The bridge
  now exposes a thread-local `CallbackDepthGuard` with a finite default budget; the composed spike
  uses it and throws a bounded Java exception when the budget is exceeded. A production host must
  enter the guard at every native callback boundary.
- **Ordering guarantees.** Bukkit promises handlers run in listener-priority order on the main
  thread. Servicing a port from the tick thread preserves ordering per handler; concurrent handlers
  on separate threads do not have a defined order between them, and Bukkit's own semantics for that
  are thinner than plugin authors assume.

---

## 4. Classload interception — the mechanism, executed

The design rests entirely on one claim: vanilla's own internal classes can be redirected to
signature-identical shims **at classload time**, so Paper's already-compiled bytecode calls into us
with no bytecode
modification and no redistribution. That had never been executed here, so it was an assumption.

`crates/plugins/lodestone-jvm-bridge/spike/` executes it, under Apple `container` (this host has no
Java runtime; see `docs/oracle-runtimes.md`). A stand-in caller class is compiled **once** against a
stand-in whose name and signature mirror one of vanilla's own world-level types, then loaded through
two loaders differing in exactly one path element:

```
control arm [real, app]: REAL:11,1,4
test arm    [shim, app]: SHIM:11,1,4
native seam (shim)     : UnsatisfiedLinkError (the JNI seam is reachable)

control shows NO interception : PASS
test    shows interception    : PASS
native seam reachable         : PASS
```

Three things this establishes and one it does not:

- interception works on **already-compiled third-party bytecode**, with no recompilation — if the
  caller had to be recompiled, the design would be jar patching, which is the thing licensing
  forbids;
- the **control arm** answers `REAL`, which is what makes the test arm meaningful; without it a shim
  that was never reached and one that was would look identical;
- an intercepted class can carry a `native` method, and reaching it with no library loaded raises
  `UnsatisfiedLinkError` — evidence the JNI attach point genuinely exists on an intercepted class
  rather than a claim that it could.

**It does not prove Paper's ~7,000-member surface is shimmable.** It isolates the mechanism, with
stand-in classes, so it stays runnable by anyone without a Paper jar. Sizing is §1's job.

One implementation detail is load-bearing and easy to get wrong: both loaders take the **platform**
class loader as parent, not the system one. With the system loader as parent, ordinary parent-first
delegation finds whichever `Level` is on the application classpath and the test arm silently answers
`REAL` — an interception that appears to fail for a reason unrelated to interception.

### 4.1 JNI invocation and port round trip — the mechanism, executed

The classloader spike remains isolated at `crates/plugins/lodestone-jvm-bridge/spike/`. The next
mechanism is a standalone Cargo workspace under `spike/invocation/`; it remains intentionally
separate from the production host. The production crate also names `jni`, but only as an optional
dependency compiled by its default-off `jvm` feature; the spike keeps the end-to-end gate isolated
from ordinary Rust tests.

The Rust process creates one JVM, starts a named Rust invocation thread, attaches that thread, and
registers the Java plugin's static `nativeScore(int, int, int)` method with `RegisterNatives`. The
Java method calls back into Rust. That callback holds the existing `WorldPort`, sends the three
integers plus its Rust thread identity, and blocks only up to the port deadline. A different named
Rust thread owns `PortServicer`, evaluates `x * 31 + y * 7 - z * 5 + 17`, and includes its own thread
identity in the response. Inputs `(11, 7, -3)` therefore have the independently predicted result
**422**. The callback rejects a same-thread response, and the successful run prints both unequal
thread identities.

Every arm starts the executable afresh because JNI permits one live JVM per process. The runner
executes these observations under an outer 15-second process timeout:

```text
scenario=Success RESULT:422
callback_thread=ThreadId(3) service_thread=ThreadId(2) distinct=PASS
scenario=Unregistered ERROR:UnsatisfiedLinkError:...
scenario=Dropped ERROR:RuntimeException:Rust error: world port failure: the world servicer is no longer running
scenario=TimedOut ERROR:RuntimeException:Rust error: world port failure: the world servicer did not answer within 150ms
scenario=Panicked ERROR:RuntimeException:Rust panic: deliberate callback panic after service response
callback_thread=ThreadId(3) service_thread=ThreadId(2) distinct=PASS
INVOCATION SPIKE PASSED
```

The unregistered arm is the control proving the JVM truly needs native registration. The dropped and
silent-servicer arms exercise the `Closed` and `TimedOut` `PortError` variants and return to Java
without a hang; `Saturated` is not exercised here. The panic arm first completes a real port round
trip, then panics inside the callback; `EnvUnowned::with_env` catches the unwind and
`ThrowRuntimeExAndDefault` translates it to a Java `RuntimeException`, so no Rust unwind crosses the
FFI boundary.

This is a mechanism test using a stand-in plugin class. It does not start Paper, touch
`lodestone-server`, exercise global references, prove nested Java/Rust reentrancy bounds, or measure
production marshalling cost. Its result is narrower: invocation, registration, thread attachment,
the port hand-off, and loud error translation all work together in one process.

### 4.2 Composed intercepted-shim invocation — hermetic fixture

`spike/invocation/src/bin/intercepted.rs` composes the two mechanisms without importing any third-party
bytecode. It compiles the existing stand-in class pair and the existing user-supplied-style `Caller`
once, then loads `Caller` through a platform-parented loader first with the real directory and then with
the shim directory. The harness invokes `describe` before native registration and requires the independent
`SHIM:11,1,4` result, so a native callback cannot mask a wrong class selection. The control must answer
`REAL:11,1,4`; the shim's registered
`nativeBlockName(int,int,int)` must answer `RUST:345` after its request crosses `WorldPort` to a separate
servicer thread. The same shim registers `nativeBlockStateId(int,int,int)` with the primitive `(III)I`
descriptor; the plugin calls it independently and requires `NATIVE-ID:345`, proving multi-method
registration and integer return marshalling through the same port rather than only string conversion.

The composed runner repeats the dropped-servicer, silent-servicer, unregistered-native, and callback-panic
arms, each with a 150 ms port deadline and a 15 second process bound. It uses scoped JNI attachment and
checks the JNI attachment counter returns to its pre-call value, proving detach as well as attach. The
fixture is exercised by `spike/invocation/run.sh`; `tests/invocation_spike.rs` is an ignored live gate
because it requires the checksum-pinned container runtime rather than the ordinary Rust test environment.

### 4.3 Opt-in production runtime boundary

With `lodestone-jvm-bridge --features jvm`, a host can construct `runtime::JvmConfig`, explicitly
call `runtime::JvmRuntime::start`, and use `with_attached_thread` for a scoped JNI environment.
The default feature set is empty: depending on the crate or constructing a config does not load a
JVM. The attachment callback receives no ECS handle or
world guard; world access remains the bounded `WorldPort` request/response path serviced by the
tick side. Callback errors map to `JvmError`, while the existing port retains its timeout and
panic/error mapping. This is the host-callable runtime seam, not broad event compatibility or a
Paper redistribution.

`JvmRuntime::load_isolated_class` is the production operator-jar loading primitive. It creates a
fresh `URLClassLoader` whose parent is the platform loader and whose URLs preserve
`JvmConfig::with_classpath` order. The platform parent is crucial: the system loader could win a
parent-first lookup before an operator path is considered. A host can therefore put a locally built
shim directory or jar before the user-supplied Paper and plugin jars; already-compiled bytecode
then resolves that shim without changing, copying, or redistributing either operator jar. The
primitive loads one named class only; it does not yet select Paper's bootstrap class, discover
plugin descriptors, or implement its lifecycle.

### 4.4 Paper bootstrap plan and plugin discovery

`paper::PaperBootstrapConfig` is the pre-JVM, host-callable intake boundary for an
operator-supplied Paper server jar and a plugin directory. It is available through the default-off
`paper-preflight` feature, which adds archive validation without linking or starting a JVM; `jvm`
includes that feature for the dedicated host. `discover` opens archives in place; it
never extracts an entry or permits an archive path to become a filesystem path. The server jar must
carry both the expected Paper bootstrap class entry and the Paper manifest marker. Each top-level
`.jar` in the plugin directory is sorted by path, subject to a default limit of 256, and must contain
exactly one descriptor: either `paper-plugin.yml` or `plugin.yml`. A jar carrying both is rejected
instead of guessing which lifecycle it wanted.

The descriptor reader is deliberately narrow and loud: it accepts top-level scalar `name`,
`version`, and `main` fields, rejects duplicate fields, invalid Java binary names, control text, and
duplicate plugin names (case-insensitively). It then maps each valid binary name to its exact
archive class entry, requires that entry exactly once, and checks its Java class-file magic before
JVM startup. Descriptor reads are capped at 64 KiB; entry-point checking reads only the four-byte
header, so discovery cannot decompress a plugin body merely to prove the future loader's input is
unambiguous. This admits the ordinary Paper and Bukkit descriptor shapes while making unsupported
YAML constructs, missing entry points, duplicate entries, and non-class payloads named input errors
rather than accidental partial plugin loads.

The resulting `PaperBootstrapPlan` records the validated plugin metadata and exact entry surfaces
but keeps plugin jars out of the server bootstrap classpath. Its `start_runtime` starts an empty-system-classpath JVM.
`lifecycle_load_requests` is an injectable, JVM-independent loading seam: it requests the bootstrap
first, then every plugin entry in sorted discovery order. The bootstrap loader owns the shim paths
and server jar. Each plugin request has only its own jar and is a fresh child of that retained
bootstrap loader; no plugin jar appears in another plugin's request, but every plugin observes the
same server-API and shim definitions. The dedicated host uses
`load_lifecycle_entries_in_runtime`, which turns that topology into loaders and calls `loadClass`
without initialization. Its returned
`PaperLifecycleLoad` keeps the bootstrap loader, then every successfully loaded plugin's descriptor,
loader, and non-initialized entry class together for the Java adapter worker's lifetime. Its
`PaperPluginLifecycleStatusSet` records each descriptor's Load result separately. The lifecycle
order is explicit: a discovered descriptor may Load, a loaded descriptor may Construct, a constructed
descriptor may Enable, and an enabled descriptor may Disable. Loading records only Load; the separate entry-only construction seam below
owns its experimental callbacks and is not a compatible server-owned API state. A bootstrap Load
failure remains terminal; a plugin Load failure is isolated, reported against that descriptor, and
does not stop the host from checking later isolated entries. When the operator
also requests the isolated native shim, the shared bootstrap loader first resolves
`lodestone.bridge.IsolatedPaperShim`, verifies its exact static native
`blockStateId(int, int, int): int`, `serverTickCount(): long`, and
`setBlockStateId(int, int, int, int): int`, `currentPluginName(): String`, and
`currentPluginVersion(): String`, `currentPluginMainClass(): String`, and
`currentPluginDescriptor(): IsolatedPluginDescriptor` declarations, then registers the full
generated callback list before it loads the bootstrap or plugin entry. Each plugin child inherits
that one registration and definition, preventing the same Java API type from being separately
defined for each plugin loader.

The native declaration list is generated from one Rust `NativeMethodSpec`, and its class, name,
descriptor, and order have hermetic tests that need neither a JDK nor operator jars. Registration
validation produces a typed `NativeSurfaceError`; the lifecycle host preserves its exact context in
the terminal startup error. The ignored JDK gate compiles only a repository-owned stand-in declaration
and proves JNI accepts that generated registration. This is a native callback seam, not a supplied
Bukkit/Paper API or a claim that any plugin can run.

One additional interception control accepts a single operator-selected static `()I` member. Its
binary class name, member name, and returned value are validated input rather than a committed
upstream inventory. The bridge loads and registers that member in the retained bootstrap loader
before it creates any plugin child loader; an already-compiled child entry therefore consumes the
same native definition through its normal parent relationship. A missing class, a wrong member
shape, or failed native registration names the requested member and stops startup. The JDK-gated
lifecycle fixture checks that a real retained plugin entry receives the expected `341` value from
this path. This is a narrow class-loader/native-registration control, not a parallel Bukkit facade
or a claim of broad internal-surface coverage.

A second, independently selected control accepts one static `(long): int` block-state member.
The `long` is an opaque resident block handle, not a server object: the worker generation-checks it,
copies its coordinates, and only then sends the already-bounded state query to the host. The ignored
JDK fixture packages the caller solely in a plugin child archive while the selected declaration
lives only in the bootstrap paths; its successful read therefore proves the production parent/child
loader route as well as the handle-to-host-query sequence. A stale or wrong-kind handle fails before
the host receives a query. This is deliberately one operator-provided value shape, not a class of
world wrappers.

Field access is deliberately not part of this surface. Reading a static field would initialize its
declaring shim class, contradicting the non-initializing load contract; instance fields would need a
separate object-lifetime design. The field-operation risk in §1.2 therefore remains open rather than
being hidden behind a constant or an unvalidated reflection path.

The retained `Load` lifecycle step keeps a loader and non-initialized entry-class reference per
successful descriptor. It is immediately wrapped in `PaperPluginConstructionPlan`, whose readiness
snapshot keeps each plugin's validated name, version, kind, and main class beside the same
worker-lifetime loader that resolved it. On the isolated worker, preflight reads each loaded class's
public constructor metadata without initializing the class, instantiating it, or invoking a callback.
A missing or uninspectable constructor is reported before any facade decision; `EntryLoadFailed`
remains the result for an entry with no retained class.

`PaperServerFacadeInput::entry_construction_only` is the intentionally tiny exception: it consumes
the adapter worker's capability token and permits one zero-argument Java-language constructor attempt
for each preflighted entry on that same worker, then retains the resulting Java object with its
defining loader. It supplies **no** server object, plugin metadata object, callback surface, event
system, or compatibility contract. It exists to prove the retained-object and failure-isolation
mechanics, not to make an ordinary plugin usable.
The native-surface input permits that same bounded construction path after its shim is installed.
Its name, version, and main-class queries, plus `currentPluginDescriptor()`, are available only
while the matching entry's constructor, `onEnable`, or `onDisable` is executing on the adapter
worker. The descriptor is a shim-defined `lodestone.bridge.IsolatedPluginDescriptor` value whose
checked constructor and three accessors carry the validated name, version, and main-class binary
name. It is not a Bukkit or Paper metadata class and the bridge requires no server, event,
lifecycle, or mutating method on it. Each query reads a worker-local stack, not a JVM property,
field, world port, or server object; outside one of those calls it fails with a Java error. This
gives a retained entry truthful identity metadata without implying a Bukkit/Paper metadata or
server facade.
`construct_entries` attempts eligible entries in discovery order; a constructor exception changes only
that entry to `Failed` with a `Construct` diagnostic and later eligible entries still run. The status
sequence can continue through the equally narrow retained-object callbacks: `enable_entries` calls
the zero-argument instance method named `onEnable` once for every constructed entry in discovery
order, then `disable_entries` calls `onDisable` once for every successfully enabled entry in reverse
order. These ownership-consuming state transitions are synchronous on the adapter worker and cannot
be sent to, retried from, or overlapped by another thread. A missing method or Java exception changes
only that descriptor to `Failed` with an `Enable` or `Disable` diagnostic; later enable attempts and
earlier reverse-order disable attempts still run. The final retained state keeps every enabled
instance alive until the worker exits, including one whose disable callback failed. These are
explicit method names on the experimental entry object, not Bukkit/Paper lifecycle semantics, a
server facade, plugin metadata, or an event system. The ordinary `Unavailable` input remains a
construction blocker. A future server-facing construction path must add a real facade through the
retained loader rather than treating the identity query or entry-only experiment as a partial API.

The unignored unit tests inject loader outcomes and constructor shapes to prove ordering, phase
rules, bootstrap-terminal failure, isolated plugin failures, callback-failure isolation, and blocking.
The runtime gate stays ignored because it needs a locally installed JDK; it builds stand-in archives
only, constructs four retained entries on the one adapter worker, and records the enable and disable
callback sequence. One enable failure leaves later entries eligible; one disable failure leaves the
earlier reverse-order callback eligible. It checks the final per-descriptor `Enable`/`Disable` status
and a callback log, but does not use an operator Paper jar or establish plugin compatibility.

```bash
cargo test -p lodestone-jvm-bridge --features jvm --test paper_lifecycle \
    lifecycle_entries_run_callbacks_on_the_adapter_worker -- --ignored --exact
```

The ignored local-jar gate uses a locally materialized server jar and does not download or extract
it:

```bash
LODESTONE_PAPER_JAR=<local-paper-server.jar> \
    cargo test -p lodestone-jvm-bridge --features paper-preflight --lib \
    paper::tests::local_paper_jar_is_discovered_without_extracting_it -- --ignored --exact
```

### 4.5 Experimental adapter worker

`adapter::AdapterHost` is the production loading and invocation boundary for an explicitly supplied
adapter class. It is not Paper plugin discovery or a Fabric mod loader. The host starts it with a
`JvmConfig`, a dotted Java class name, and a positive operation deadline. Startup happens on one
dedicated worker; `poll` reports `Ready` only after class loading, static-method validation, and
native registration succeed. The worker keeps its class reference inside one scoped attachment,
and each callback uses a local JNI frame so repeated dispatches do not accumulate temporary references.

The adapter bootstrap contract is deliberately small:

```java
package example;
public final class Adapter {
    private static native int blockStateId(int x, int y, int z);
    public static void onTick(long tick) {
        int state = blockStateId(11, 7, -3);
        System.out.println("tick=" + tick + " blockState=" + state);
    }
    public static void onBlockStateChanged(int x, int y, int z, int stateId) {
        System.out.println("changed=" + x + "," + y + "," + z + ":" + stateId);
    }
}
```

The host calls `dispatch_tick` only while `is_idle` is true, drains `service_pending` through its
public block-state accessor, and polls `TickCompleted` before dispatching again. After it has
successfully applied a resident block-state replacement, it may call
`dispatch_block_state_changed`; completion retains the exact `BlockStateWrite` payload as
`BlockStateChangedCompleted`. There is one shared command slot for both callback shapes, so a slow
callback cannot silently build a backlog or overlap another callback. In particular, the host must
not send a block-change callback while a native world request is unresolved: it waits for the
native write response, then queues the separate host-to-JVM callback. `BlockStateQuery` uses
absolute primary-world block coordinates. The host returns either a real `u32` state ID or a named
error; an unavailable chunk must not be reported as air. Values outside Java's nonnegative `int`
range fail explicitly. The isolated shim additionally exposes `setBlockStateId(x, y, z, stateId)`:
the host validates the non-negative integer against its generated built-in state table, then replaces
the block only when its primary-world column is already resident and `y` is in that column's stored
extent. It returns the accepted state id; an unavailable column, invalid state, or out-of-height
coordinate is a named Java error. The call neither loads nor generates terrain, and it has no event
cancellation, physics propagation, block-entity, packet-broadcast, or plugin lifecycle semantics.

The registered native function receives a thread-local `WorldPort`, never an ECS world. It enters
the shared callback-depth guard, applies the port deadline, and contains panics/errors at the JNI
boundary. The port is installed only on the worker: a Java-created thread attempting the native
query fails with a named worker-thread error. Native queries from class initializers are unsupported
because initialization can run before registration. Adapters must defer world queries until
`onTick`.

Loading and callback errors retain the class/method and Java exception description. A missed
deadline is terminal even if a late completion is queued; the host must drop the adapter and report
the error. Drop disconnects channels without joining untrusted Java code. Arbitrary Java execution
cannot be safely killed in-process, so a timed-out callback may keep running until process exit.
There is no hot reload or JVM restart: JNI allows only one JVM startup in a process, including after
a failed adapter load. The adapter class is loaded through the isolated ordered operator loader,
not the JVM system loader. This makes shim-first interception a production capability, but Paper
bootstrapping and the plugin lifecycle remain separate work.

Hermetic tests exercise worker/host thread separation, exact block-query arithmetic, sequence
preservation, overlap rejection, error propagation and terminal deadlines. The live test
`java_adapter_registration_world_query_and_exception_are_connected` compiles only the repository's
`tests/java/BridgeAdapter.java` and starts a real JVM. Its success callback expects state `422` for
`(11,7,-3)`; its failure callback queries `(-19,5,23)` and requires the host's unavailable-chunk error
to return through Java as a `RuntimeException` naming `onTick` and the native query.
The fixture also requires an unregistered native to throw `UnsatisfiedLinkError` and a Java-created
thread to receive the named worker-thread error, with the registered main-worker query as control.
Run this gate
in a fresh process with `JAVA_HOME` pointing to a JDK:

```bash
cargo test -p lodestone-jvm-bridge --features jvm --test adapter_host \
    java_adapter_registration_world_query_and_exception_are_connected -- --ignored --exact
```

`tests/isolated_loader.rs` independently compiles one probe against an original jar definition,
then proves the production loader returns `REAL` with the original jar first and `SHIM` when a
same-name shim jar is first. This control is deliberately separate from the adapter test because
the process may start only one JVM:

```bash
cargo test -p lodestone-jvm-bridge --features jvm --test isolated_loader \
    ordered_operator_jars_select_the_intercepting_definition -- --ignored --exact
```

To extend the boundary, change the adapter's native declaration and the corresponding Rust
registration together, then add an independently predicted end-to-end fixture. Add concrete public
host queries on demand rather than enumerating a speculative compatibility surface.

#### Narrow resident block-change event seam

The isolated shim also has one strongly typed observation contract:
`ResidentBlockChangeListener.onResidentBlockStateChanged(int x, int y, int z, int stateId)`. A
plugin registers an object with `IsolatedPaperShim.subscribeResidentBlockStateChanges(listener)`.
This is deliberately a host-confirmed resident block-state replacement callback, not a Paper event
bus: there are no event classes, priorities, cancellation, physics, or server objects in this
slice. The shim and listener declarations are validated in the same isolated loader before any
native function is registered, so a missing or mismatched callback fails that loader's setup.

Registrations are retained only on the dedicated adapter worker and are dispatched in insertion
order. Each registration captures the active descriptor identity and starts inactive; the
lifecycle owner activates that entry's registrations only after `onEnable` succeeds. A failed
constructor or enable callback, and every attempted `onDisable` callback (including one that
throws), removes that entry's registrations and drops their JNI global references on the attached
worker. The worker retains at most 64 registrations at once and reports an explicit registration
error when the bound is reached.

After a host-confirmed change, the adapter invokes its existing callback and then walks the active
listeners. A listener exception is cleared and recorded in the completion's
`listener_failures` list with its stable registration number, descriptor name, and bounded detail;
later listeners still run, the applied change is not rolled back, and the adapter remains usable.
During each listener call, `IsolatedPaperShim.currentBlockHandle()` returns the opaque `jlong`
for that listener's changed block. The value is a generation-checked slot reference whose payload
is only the owner identity and block coordinates; no ECS pointer or world guard can cross the JNI
boundary. The call fails outside a resident block-change callback. Handles are interned per owner
and position, the registry has a 1,024-live-handle bound, and disabling or failing an entry releases
all of that entry's handles before the slot can be reused. The callback still uses the existing
one-slot worker command queue, and any world read or write from a listener must use the bounded
`WorldPort` request/response seam rather than reaching an ECS guard. The
`IsolatedPaperShim.blockHandlePosition(long)` resolver returns a copied coordinate string while the
handle is live; stale, forged, out-of-range, and worker-off-thread calls fail as named Java errors.
The typed `blockHandleX(long)`, `blockHandleY(long)`, and `blockHandleZ(long)` accessors return the
same copied coordinates as `(J)I` values. They resolve the handle and its generation first, then
read only the worker-owned payload: they deliberately make no host query, so they cannot turn a
coordinate lookup into chunk generation or a server-world read. Hermetic controls use coordinates
whose components differ and then release the handle to prove both the selected component and the
stale-generation failure path.
`blockHandleStateId(long)` first generation-checks that same handle, then asks the host's bounded
resident-block query port for the state at its copied coordinate. This preserves the distinction
between air and an unavailable column, and a stale handle fails before a host query is sent.
The generic registry separately reports wrong-kind use. A live player handle may
also resolve through `playerHandleName(long)` and `playerHandleUuid(long)`. The
latter returns a canonical lowercase UUID string from the fixed sixteen profile
bytes the dedicated roster already copied into `PlayerIdentity`; it does not
read a world, retain a connection, or cross a server-world guard. Both resolvers
generation-check before returning their bounded copied value, so a disconnect
or replacement fails loudly instead of resolving a later player. The inverse
`playerHandleForName(String)` searches the same copied worker roster and returns
that generation-checked handle only when exactly one active copied display name
matches. A null, unknown, or ambiguous name fails loudly rather than selecting
a hash-map iteration winner. `playerHandleForNameIgnoringCase(String)` is the
separate ASCII-only case-insensitive variant: null or non-ASCII input and any
case-fold collision fail loudly, so it cannot turn a malformed roster into an
arbitrary player choice. Both return the same worker-owned generation-checked
handle; neither reads a server registry. There is no additional environment
variable or runtime toggle: the `jvm` feature and an operator-built shim
containing the exact isolated declarations are the only prerequisites.

`playerHandleForNamePrefix(String)` is a separate, case-sensitive convenience
resolver over that same copied roster. Its exact JNI descriptor is
`(Ljava/lang/String;)J`. Null and empty prefixes fail before the map is read,
and a prefix that matches more than one active copied name fails rather than
returning a hash-map winner. A successful result is the existing
generation-checked handle, so disconnect and slot reuse invalidate the old
value just as they do for complete-name lookup.

`playerHandleForProfile(String, String)` is the explicit disambiguator. Its
exact JNI descriptor is `(Ljava/lang/String;Ljava/lang/String;)J`: the first
argument is the copied display name and the second is a 36-character hexadecimal UUID form.
It resolves only their complete value pair in the worker-local lifecycle map.
That means a transient roster with either duplicate names or duplicate UUIDs
still has one deterministic answer when the pair is unique, while null or
malformed inputs and an absent pair fail before any server lookup. The returned
`long` remains the same generation-checked handle as the other resolvers, so a
disconnect removes the inverse mapping and an old handle fails rather than
identifying a later reconnect. The generated shim fixtures pin this exact
declaration and registration; hermetic controls prove both unqualified lookup
ambiguities before proving the qualified pair selects the intended handle.

`activePlayerHandleAt(int)` completes the worker-local roster boundary for
callers that need to enumerate active profiles. It uses the exact `(I)J` JNI
descriptor and returns the same generation-checked opaque handle as the
profile resolvers. Because the backing map has no stable iteration order, the
worker sorts copied profiles by UUID and then display name before selecting an
index. Negative and out-of-range indices fail explicitly; a disconnected
profile therefore cannot remain enumerable or be replaced by a later
generation. The selection is bounded by the worker's existing object-handle
and lifecycle limits and never reads a server registry or world state.

`activePlayerCount()` returns the count of live player handles in that same
worker-owned lifecycle map. Its producer is the dedicated host's existing
value-only roster reconciliation: each queued join adds a handle before its
Java callback, and each disconnect removes and generation-invalidates it after
its callback. It is consequently a reconciled worker snapshot, not a fresh
JNI read of the server registry; transitions still waiting in the bounded host
queue are not counted. The count is a copied integer and has no path to an ECS
value, connection, world, or guard.

`playerHandleIsActive(long)` resolves the supplied player handle before it
looks up that handle's copied profile in the same reconciled map. It returns a
copied boolean: `true` means the profile has reached the worker through a
queued join and has not yet reached its queued disconnect; `false` means a
live callback-only profile has no matching lifecycle entry. It is intentionally
not a fresh server-registry read, so a transition waiting in the bounded queue
is not visible yet. A stale, forged, wrong-kind, or off-worker handle raises a
named error before the map lookup; no ECS value, connection, world, or guard
crosses JNI.

To extend this event subset, update the source-of-truth declarations and validation in
`native_surface`, the JNI registration and worker dispatch in `adapter`, and the lifecycle cleanup
calls in `paper`. Keep the listener list ordered and bounded; do not turn it into a general event
hierarchy. Hermetic tests cover registration order, activation and cleanup, the registration bound,
and exception isolation without starting a JVM or server. The ignored JVM fixtures remain separate
controls for JNI registration and should be run only in a fresh process with an explicit JDK.

### 4.6 Dedicated-host connection

The dedicated binary enables the bridge only with its default-off `jvm` feature. Set both
`LODESTONE_JAVA_ADAPTER_CLASS` (dotted class name) and `LODESTONE_JAVA_CLASSPATH` (a platform path
list of adapter class directories or jars in isolated-loader resolution order); `LODESTONE_JAVA_DEADLINE_MS` optionally overrides the
5000 ms startup/callback deadline. Without configuration, no JVM worker or poll timer starts.
Configuration supplied to a build without `jvm` is a reported error followed by clean shutdown.

To add Paper bootstrap intake to that same worker, set `LODESTONE_PAPER_JAR` and
`LODESTONE_PAPER_PLUGIN_DIRECTORY`; `LODESTONE_PAPER_SHIM_PATH` optionally names one shim directory
or jar. When set, it must contain the operator-built `lodestone.bridge.IsolatedPaperShim` native
declarations; the host registers its block-state query and server-tick callbacks in the shared
bootstrap loader before loading any entry. The host discovers and retains a `PaperBootstrapPlan` before starting
a JVM, then loads the Paper bootstrap followed by each plugin entry class through fresh plugin-child
loaders before the adapter can report ready. The dedicated adapter owns the resulting
worker-lifetime lifecycle state and snapshots each descriptor's Load status before readiness. Each
bootstrap loader sees shim paths and the server jar; each plugin child sees only its own jar and
inherits those shared definitions. The worker also snapshots each
descriptor's construction prerequisite. Without `LODESTONE_PAPER_SHIM_PATH`, no facade is available.
With it, the dedicated host supplies one Java-facing, loader-local native state surface: resident
block-state reads, validated resident block-state replacement, the current server tick, and the
active retained entry's validated descriptor name and version. It also reconciles the server's
value-only player roster into generation-checked handles: `playerHandleName(long)`,
`playerHandleUuid(long)`, `playerHandleForUuid(String)`, `playerHandleForName(String)`,
`playerHandleForNameIgnoringCase(String)`, `playerHandleForNamePrefix(String)`,
`activePlayerHandleAt(int)`, `activePlayerCount()`, and
`playerHandleIsActive(long)` read that worker snapshot only. The dedicated host
never hands a server object, connection, ECS entity, or world guard to Java.
`AdapterHost::start_with_setup` mints the corresponding capability token only
from its worker's request ports; consuming it keeps that token with the retained loader state, and the
dedicated `JavaAdapter::poll` call is the matching live producer through
`IntegratedServer::resident_block_state_id`, `IntegratedServer::set_resident_block_state_id`, and
`IntegratedServer::server_tick_count`. A lifecycle caller
cannot claim that seam with an enum value while no request producer exists. It is not a Bukkit `Server`
or a plugin loader. It permits only the isolated retained-entry construction and callback experiment
described above; it is not a general construction permit or plugin compatibility claim. These
are non-initializing class loads: the host does **not**
initialize Paper, dispatch Paper events, or promise plugin compatibility. A failed plugin entry is
logged with its descriptor and stays disabled;
a failed Paper bootstrap remains terminal for the configured run.
A malformed Paper configuration or bootstrap Load failure saves the world before the configured run
can continue. A plugin-entry Load failure is instead retained as disabled descriptor status, rather
than silently removing the operator input or preventing the later isolated entries from being checked.

A host operator can additionally select one static native `(long): int` block-state member from that
same shim path by setting both `LODESTONE_PAPER_OPERATOR_BLOCK_STATE_CLASS` and
`LODESTONE_PAPER_OPERATOR_BLOCK_STATE_METHOD`. The dedicated host rejects a partial selection, an
invalid binary/member name, or a selection without `LODESTONE_PAPER_SHIM_PATH` before it starts the
worker. During bootstrap loading it registers the chosen declaration before any plugin child loader
exists; its callback accepts only an opaque resident block handle, validates the generation on the
worker, copies the block coordinates, and sends the existing bounded state query to the host. It
does not expose a world object, retain an ECS lock across the JVM boundary, or define a second plugin
API.

The admin loop services at most 64 block queries, 64 block writes, and 64 server-tick queries per 1 ms poll. Block
queries use `IntegratedServer::resident_block_state_id`, which reaches the live primary `ChunkStore`,
checks presence, and reads the cell under one cache lock. It never invokes generation, reads disk,
clones a whole column, or returns air for unavailable terrain. `Arc`, borrowed-source and dimension
wrappers forward this capability. A source with no retained read capability returns `None`; the host
converts this into a named Java error. Extreme and out-of-height Y coordinates are also unavailable.
The Rust API returns validated `StateId`; conversion to a raw integer happens only at the Java wire
boundary. Writes validate that raw value back into `StateId`, render its canonical state string, and
write through the same live source only after its resident-column preflight succeeds. Each successful
write enters a dedicated-host FIFO of at most 64 `BlockStateWrite` callbacks. The host does not apply
more writes when that FIFO is full, so it never acknowledges a native write then silently loses its
matching callback. When the adapter worker becomes idle, it receives the oldest callback through the
same one-slot worker command queue before the next tick is dispatched. A failed write emits no
block-change callback. Tick queries use
`IntegratedServer::server_tick_count`; an inactive server is likewise a
named error rather than a fabricated zero, because zero may be a valid future tick count.

The adapter observes the latest completed server tick whenever it becomes idle. Intervening server
ticks are **coalesced**, not replayed with historical state; the same tick is never dispatched twice.
This is a narrow native state contract. Its host-confirmed block-change callback is an adapter
method, not a Bukkit/Paper event: it has no listener registry, priority ordering, cancellation,
plugin instance, or event object. Readiness and failures are logged; an asynchronous failure
disables the adapter while the server continues.
Shutdown drops the adapter before saving. Closed stdin suspends only the console-input future, so
adapter polling and termination signals continue under a supervisor.

The dedicated test `java_adapter_reads_the_running_persistent_world` compiles a repository-owned
adapter, opens a temporary persistent server through the production constructor, and drives the
production `JavaAdapter::poll` consumer. Its first callback requires the real resident air state
`0` and prints `LIVE-WORLD`; its next callback requires a far-away unavailable-cell exception. It
does not bind TCP or load Paper. Run it separately with `JAVA_HOME` set:

```bash
cargo test -p lodestone-dedicated-server --features jvm \
    java_adapter_reads_the_running_persistent_world -- --ignored --nocapture
```

---

## 5. The ABI decision

**Decision: compile-time-optional feature on a `rlib`, not `cdylib` + `libloading` — for now, with a
named condition under which that flips.**

The issue asked for this to be decided early because it affects the plugin ABI. The options:

| option | JVM optional at… | cost |
|---|---|---|
| **(a) compile-time feature** | build time | a user wanting Java plugins builds with `--features jvm`, or takes a build that has it |
| (b) `cdylib` + `libloading` | run time | forces a **stable C ABI** on every type crossing the boundary, forever |

Reasoning for (a):

1. **(b) prices in a permanent cost for a benefit nobody has asked for yet.** A runtime-loadable
   bridge means every type crossing the boundary needs a stable, `repr(C)`, version-tolerant
   representation. That constraint would propagate into the ECS-facing types, and it is paid on
   every change forever, for the benefit of not rebuilding.
2. **The zero-cost requirement is fully satisfied by (a).** The constraint is *"a user who loads no
   Java plugin pays nothing"* — no `libjvm` linkage, no JVM startup, no per-tick cost. A default-off
   feature delivers that exactly.
3. **The decision is reversible in one direction only, cheaply.** Going (a) → (b) later means adding
   a C-ABI shim over an existing Rust API. Going (b) → (a) means unwinding an ABI other people
   already build against.
4. **A JVM bridge cannot be WASM-sandboxed regardless**, so this tier is opt-in and
   trust-the-operator either way; dynamic loading buys no isolation here.

**The condition that flips it:** if binary *distribution* becomes the primary way people run this —
one download, Java support toggled without a rebuild — (b) becomes the right answer, and it should
be revisited then rather than assumed now.

### 5.1 What makes "zero cost" checkable rather than asserted

`crates/plugins/lodestone-jvm-bridge/tests/zero_cost_graph.rs`:

- **production edges must be default-off** (a manifest scan across workspace-member `Cargo.toml`
  files under `crates/` and `xtask/`). The dedicated host is the permitted consumer, with an optional
  dependency enabled only by its `jvm` feature and an empty default feature list. Negative controls
  reject unconditional and default-enabled edges. Additional controls find `lodestone-ecs`'s real
  dependents and exclude the standalone invocation workspace from the production graph;
- **the bridge names no unconditional JVM-linking crate**, permitting `optional = true` only, and
  asserts that the `jvm` feature is absent from the default feature set.

**One trap here was measured and is worth not rediscovering: a `Cargo.lock` grep is the wrong
instrument.** `jni` 0.22.4 is *already* in this workspace's lockfile, via `android-activity`,
`cpal`, `hickory-*` and `rustls-platform-verifier` — all reaching it only on targets we do not
build. `cargo tree -p lodestone-shell -e normal -i jni` prints *"nothing to print"* for the host. A
lockfile grep would therefore report a violation that does not exist, which is the same class of
error as grepping for a field name and finding every struct that has one.

Following `lodestone-app`'s `renderer_free_graph.rs`, the gate does **not** shell out to `cargo
tree`: a nested cargo invocation inside a test contends on the package-cache lock in a shared
checkout. `cargo tree` is the measurement; the manifest scan is the guard.

---

## 6. Object identity and lifetime

Bukkit plugins keep `Player`, `World` and `Block` references in fields, in maps and in scheduled
tasks, and dereference them arbitrarily later. The entity behind one can despawn, disconnect or
unload in between. `lodestone_jvm_bridge::identity` is a generational slot map, and three properties
are non-negotiable:

- a stale reference must **fail**, not dangle;
- it must fail **distinguishably**, so the bridge can raise the exception Bukkit semantics call for
  rather than returning a plausible wrong answer;
- a slot reused by a later object must **never** answer for the old one.

The third is the one a plain index gets wrong, and it gets it wrong in the worst possible way: a
plugin holding a ref to a logged-out player would start operating on whoever next occupied that
slot — a correctness bug that presents as a permissions bug. `release()` bumps the slot's
generation, so every handle minted before compares unequal to every handle minted after, at the cost
of a `u32` compare.

Three details that are decisions rather than implementation:

- **`ObjectRef::to_bits` does not pack the kind in.** The kind is recovered from the registry's slot
  on resolution. Packing it would make the kind claim caller-controlled, defeating the check it
  exists for — a plugin could relabel a `Block` handle as a `Player` by editing the `jlong`.
- **Generations saturate, they do not wrap.** A wrapped generation starts matching ancient handles
  again — reintroducing exactly the bug the counter exists to prevent, after four billion reuses of
  one slot, which is precisely the kind of defect that ships. A saturated slot is retired instead:
  one slot lost, correctness kept.
- **One object has exactly one live handle.** Bukkit plugins compare entity references for identity;
  two live handles to one entity would break `equals` in a way that presents as a plugin bug.

`ObjectRegistry` is generic over a host-owned, value-like payload and carries the expected
`ObjectKind` (`World`, `Player`, `Block`, and the remaining object categories) beside each opaque
reference. `try_handle_for` is the fallible path used at native boundaries, so a full registry is
reported rather than growing or returning a default. The resident block-change listener described
in §4.5 is the first callback consumer: the same worker-local registry now carries both its block
handle and an optional value-only `PlayerIdentity` supplied by the host. A listener obtains the
player through `currentPlayerHandle()` only while that callback is active and can resolve its
name through `playerHandleName(long)`; both operations return an opaque generation-checked value,
never an ECS or connection pointer. Handles are scoped to the retained entry that observed the
callback, so disabling that entry releases its block and player payloads together. The worker-wide
handle bound is shared by both kinds, and worker teardown clears the registry.

The dedicated host now reconciles the existing shared player registry with the adapter worker.
Each value-only join produces `onPlayerJoined(long)` on that worker; the matching disconnect calls
`onPlayerDisconnected(long)` while the old handle is still resolvable, then advances its generation
and releases it before reporting completion. The reconciliation queue is bounded, and a disconnect
for a player the worker never observed does not mint a temporary object. Registry reads finish
before dispatch, so neither callback runs under a server-world guard. A reconnect therefore gets a
different `long`, while a plugin never receives an ECS pointer or a connection object.

The same worker-local lifecycle map supports `playerHandleForUuid(String)`. It accepts a 36-character
UUID string (hexadecimal case is ignored), searches only the roster snapshot already reconciled by
the dedicated host, and returns the matching generation-checked handle. Unknown, malformed, or
ambiguous UUIDs fail with a named Java error; the resolver never creates a handle just to satisfy a
lookup. A disconnect removes the reverse mapping before the slot can be reused, so a lookup after
departure cannot recover an old `long` or accidentally select a replacement player.

`playerHandleForName(String)` has the same worker-local lifecycle boundary but
uses copied display names rather than profile bytes. Because display names need
not be unique in a malformed or transitional roster snapshot, it refuses an
ambiguous name rather than choosing an arbitrary handle. A disconnected name
has no reverse entry, and a later reconnect resolves only to the new generation.

Hermetic registry tests force slot reuse after release, exercise kind mismatch and capacity
exhaustion, and verify that `clear` advances every live generation before reuse. The adapter's
worker-local callback tests obtain both block and player handles, check that the player is available
only while the listener is running, release the retained entry, and prove the old bits resolve as
stale. The dedicated-host source tests drive a value-only join/disconnect tracker and its bounded
burst control. The ignored JVM fixtures validate the declaration and callback ABI separately; they
do not claim complete Paper object compatibility. Typed world and remaining object resolvers still
wait on corresponding server capabilities.

`activePlayerCount()` reads the same active lifecycle map as the retained
player handles. Hermetic bridge tests cover its `0 → 1 → 0 → 1 → 0` transition
around a release and slot reuse, so the count's consumer chain cannot quietly
stop following the generation-checked lifecycle path. The count is paired with
`activePlayerHandleAt(int)` for callers that need the bounded handle sequence.

`activePlayerHandleAt(int)` supplies that missing enumeration step without
publishing the map itself. Its controls insert profiles in reverse order,
including a duplicate UUID with distinct names, and predict the UUID/name
sort order independently of hash-map layout. Negative and out-of-range
indices are rejected, and releasing a profile removes it from the indexed
snapshot before a slot can be reused. The result remains a worker-local,
generation-checked handle; no connection, ECS value, or guard crosses JNI.

`playerHandleIsActive(long)` has hermetic controls for both sides of that
worker snapshot: an active lifecycle handle reports `true`, a live
callback-only handle reports `false`, and a released handle fails its
generation check before the snapshot is read. The isolated shim fixture pins
its `(J)Z` declaration and registration after every declaration validation.

`playerHandleForUuid(String)` has a matching `(Ljava/lang/String;)J` declaration and registration
fixture. Its controls cover malformed input before map lookup, an unknown UUID after disconnect,
and slot reuse: the old handle is not returned for the departed profile. This is a value resolver
over the copied roster, not a server or world object lookup.

`playerHandleForName(String)` uses that same `(Ljava/lang/String;)J` ABI, pinned
by the generated shim fixture. Hermetic controls reject null, unknown, and
ambiguous inputs, then force a disconnect and slot reuse to prove a lookup
cannot return the departed generation.

`playerHandleForNameIgnoringCase(String)` uses the same exact
`(Ljava/lang/String;)J` ABI but intentionally limits matching to ASCII copied
profile names. Its controls reject null and non-ASCII input, reject case-fold
collisions, and prove a disconnect removes the reverse mapping before a slot
can be reused. It is a worker-local resolver, not a server-registry read.

`playerHandleForNamePrefix(String)` also uses `(Ljava/lang/String;)J`, pinned
by the generated shim and lifecycle fixtures. Its hermetic control rejects a
null or empty prefix, proves a shared prefix fails instead of picking hash-map
order, and then releases and reuses a matching player's slot. The returned
handle is therefore only a bounded value from the reconciled worker roster,
never a reference to a connection, ECS value, or world guard.

The same composed caller exercises recursive Java-to-Rust-to-Java callbacks below and above the
budget. Depth `2` returns `REENTRANT:OK:3`; depth `4` attempts one more callback and receives
`RuntimeException:Rust error: reentrant callback depth limit 4 exceeded`. A thread-local guard restores its
counter while unwinding, so the over-limit control cannot poison later callbacks.

---

## 7. How to change this, and the gotchas

- **Do not add a handle field to `WorldPort`.** One field re-opens the deadlock and still compiles;
  `tests/reentrancy.rs` is what catches it. Same for removing the request deadline.
- **Do not wire the bridge into any crate's default dependencies.** When it is wired, it goes behind
  an optional dependency and a default-off feature, and `zero_cost_graph.rs` should be *updated to
  assert that shape* rather than deleted.
- **Keep the two spikes separate.** `spike/run.sh` proves classload interception without a native
  library. `spike/invocation/run.sh` proves JVM invocation and native callbacks without involving
  classloader interception or production server state. Combining them would make a failure
  ambiguous again.
- **When re-running the census, re-pin.** Paper publishes several builds a week and the counts move.
  `scripts/fetch-paper.sh` carries the pin (version, build, sha256) so a number in this document can
  be traced to an exact input; update the pin and the numbers together.
- **A census of the paperclip download is not a census.** See §1.5. If the numbers come back tiny,
  that is the bug.
- **The 7,179 static-operation count is a lower bound**, not a total: reflection and
  `MethodHandle` bootstraps are outside what an instruction walk can see.

## 8. Configuration

- `scripts/fetch-paper.sh` — the pinned Paper build (26.2 / 121 / sha256
  `0de30efb024bc8b83c9c7d507d11802897ad8056b6110ec09fe1a91d126ccb54`). Writes to
  `.cache/paper/26.2/paper.jar`, which is outside git.
- `lodestone_jvm_bridge::port::DEFAULT_REQUEST_DEADLINE` — how long a Java-side call waits for the
  tick thread.
- `lodestone_jvm_bridge::callback::DEFAULT_CALLBACK_DEPTH_LIMIT` — the maximum nested
  Java/Rust callback depth before the boundary reports an error; a host may use
  `CallbackDepthGuard::enter_with_limit` when its policy needs a different bound.
- `lodestone_jvm_bridge::adapter::MAX_RESIDENT_OBJECT_HANDLES` — the worker-wide bound for opaque
  block and player handles. The old `MAX_RESIDENT_BLOCK_HANDLES` name remains as a compatibility
  alias. `MAX_PENDING_PLAYER_LIFECYCLE_EVENTS` in the dedicated host bounds roster transitions
  waiting for the worker.
- `lodestone_jni_invocation_spike::REQUEST_DEADLINE` — the prototype's 150 ms deadline, chosen to
  make the silent servicer control fast while the runner's outer 15-second timeout remains an
  independent hang gate.
- `nms-census --prefix <target-package/> --internal <replaced-layer-package/> --no-recurse
  --top <N> --all` — see `--help`.

## 9. Dependencies

- `crates/lodestone-nms-census` — `zip`, `anyhow`. **No JVM**, deliberately: it must run on a machine
  with no Java, which is this one.
- `crates/plugins/lodestone-jvm-bridge` — `lodestone-ecs` by default, plus
  `lodestone-plugin-support` as a dev-dependency for the reentrancy harness. The optional `jni`
  invocation dependency is compiled only with the default-off `jvm` feature; ordinary builds have
  no `libjvm` linkage.
- The classloader spike and the paperclip step need Apple `container` and an `eclipse-temurin` image
  — see `docs/oracle-runtimes.md`. The host needs no `java`.
- The invocation spike is its own Cargo workspace and uses `jni` 0.22.4 with `invocation`. Its
  `Containerfile` pins the Rust/`cc` base image by digest, installs the repository's dated nightly
  and matching Cranelift component, and checksum-locks Temurin 25.0.3+9 for both supported container
  architectures. The checkout is mounted read-only; Java classes, Cargo caches and target artefacts
  stay inside the ephemeral container.
