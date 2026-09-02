# Java plugin bridge: backing Paper's own internal calls with Rust

## What it is

A design, a measurement, and a foundation crate for running **real, unmodified
Bukkit/Spigot/Paper plugin jars** against this server, with **zero cost when no Java plugin is
loaded**.

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

The bridge itself is not built. What is built is `crates/plugins/lodestone-jvm-bridge` (the
JVM-independent host machinery), `crates/lodestone-nms-census` (the scanner), and an executed
classload-interception spike.

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
| **distinct NMS members the Bukkit layer references** | **6,991** |
| distinct NMS classes it touches | 1,400 |
| external reference sites | 10,938 |
| *all* NMS references including engine-internal | 189,914 across 88,140 members |

**The ratio is the finding.** Paper's own bytecode makes 189,914 NMS references across 88,140
distinct members — but 88,140 is the size of *Minecraft*, not the size of this project. The members
reached from **outside** vanilla's own internal package (i.e. from CraftBukkit's and Paper's own
code) number **6,991**, a **12.6× reduction**. That number is what separates "reimplement the
game" from "implement a bounded, enumerable API".

Refining further by also treating Paper's rewritten chunk system and data converters
(`ca.spottedleaf.*`) as engine internals — a Rust-backed server replaces those outright rather than
shimming them — moves it only to **6,692 members / 1,352 classes**. So the headline is robust: the
bulk genuinely is the Bukkit bridge layer, not incidental callers.

### 1.2 Composition of the surface

| kind | count |
|---|---|
| methods | 4,318 |
| **fields** | **2,167** |
| interface methods | 506 |
| (of which constructors, `<init>`) | 421 |

**The 2,167 field references are the uncomfortable part** and are called out because they change the
shape of the work rather than its size. A method can be backed by a `native` shim; a `getfield` /
`putfield` cannot. A shim class must either *declare a real Java field* whose value is kept in sync
with Rust-owned state, or field access must be rewritten to accessor calls during classload
transformation. The first is cheap for immutable-after-construction data and wrong for live state;
the second turns "supply shim classes" into "supply shim classes *and* a bytecode rewriter", which
is a materially larger project. **This is unresolved and is the largest single design risk below.**

### 1.3 Where the work is, by subsystem

External member references by internal subsystem (top of 6,991):

| subsystem | members |
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

Entities and world/level are **54%** of the surface between them. That is the good news the
issue predicted: those are precisely the subsystems this repo already implements, so Java-plugin
compatibility really does reduce largely to server parity rather than being a separate project.

Most-referenced types, by rough category: the base entity type (274 refs), the item-data-component
registry (257), the server-side player type (223), the server-side level/world type (212), the
server singleton (207), the generic compound-tag/NBT container (191), the entity-type registry
(191), the world-generation-facing level view (170), the registry-of-registries type (165), and
the block-state type (152).

Most-referenced individual members, which give the natural implementation order — all simple
accessors or constructors, not behaviour: a server-singleton getter (55 refs), a block-state
property's bound (50, a field), a registry-key accessor (39), a holder unwrap (31), a
default-block-state constructor (30), a wrapper-object accessor that hands back the Bukkit-facing
object for a given internal one (28), a state-to-block accessor (24), and a chunk-source accessor
on the server-side level type (23).

### 1.4 The finding that most changes the design: Paper-injected members

**143 of the 6,991 members have Bukkit-facing types in their own signature.** Examples of the
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
    <work>/versions/26.2/paper-26.2.jar --internal <vanilla's internal package prefix> --top 40
```

Pre-seed `<work>/cache/mojang_26.2.jar` from `.cache/mc/26.2/server.jar` to save paperclip a
download. **The Paper jar is a local measurement input: it is not committed, not redistributed, and
`.cache/` is outside git deliberately** — see §2.

### 1.7 How far to trust the scanner

`crates/lodestone-nms-census` is a constant-pool reader (JVMS chapter 4), no JVM involved. Evidence
it is correct, in the order it was gathered:

- **Hermetic**, against a class file hand-expanded from the specification —
  `tests/classfile_fixture.rs`. The fixture deliberately contains a `CONSTANT_Long`, because a
  `Long` occupies **two** pool slots and a parser that advances one slot per entry silently resolves
  every later index to its neighbour. A fixture without one passes under both the correct and the
  incorrect reading.
- **At scale**, against the real vanilla 26.2 server: **30,563 classes, 0 parse failures**, and the
  most-referenced members come out as exactly the kind of thing dense server code calls
  constantly: a random-number roll, a block position's coordinate accessors, a default-block-state
  constructor, a block-state get/set pair, and a level's client-side check. Those being
  *semantically* what server code does is stronger evidence than the counts.
- **End to end**, a jar built in-test containing the fixture, censused back to exactly one external
  reference attributed to the right referrer.

What it does **not** answer: it counts *symbolic references*, so an NMS member reached purely
through reflection or a `MethodHandle` bootstrap is invisible to it. Paper uses reflection sparingly
in the Bukkit-facing wrapper layer, but "6,991" is a lower bound rather than a total.

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
- **This repository is `MIT OR Apache-2.0`**, and `crates/plugins/*` carry per-crate licences
  precisely because the copyleft boundary is per-plugin. `lodestone-nav` is the precedent: LGPL-3.0
  in its own crate, a clean-room design, with the reasoning recorded in `docs/baritone-port.md` §1.

### 2.2 What we ship, and what we do not

The design's licensing property is that **we distribute no Paper bytecode, modified or otherwise.**

| artefact | who supplies it | licence consequence |
|---|---|---|
| the Paper jar | **the user**, obtained from PaperMC themselves | we never distribute it |
| shim classes with NMS names/signatures | us | see §2.3 |
| the Rust bridge crate | us | MIT OR Apache-2.0 |
| the classloader that redirects vanilla's own internal classes | us | our own code |
| the Paper jar used for the census | downloaded locally, **never committed** | a measurement input |

Paper is never patched, never repackaged, and never redistributed. Classload interception — proven
executable in §4 — is what makes that possible: Paper's already-compiled bytecode binds to our
classes at load time with **no modification to Paper at all**.

### 2.3 The engineering conclusion

**The bridge crate is `MIT OR Apache-2.0`**, and this is a decision rather than an inherited default:

1. **No derivation.** The crate contains no Paper source, no Paper bytecode, and no transcription of
   either. Its NMS-facing surface is derived from **Mojang's** de-obfuscated names — the same source
   the rest of this repo already ports from — not from Paper's expression of anything.
2. **No distribution of a derivative work.** GPL-3.0's copyleft obligations attach to *conveying* a
   work based on the Program. We convey no part of Paper. The user assembles the combination on
   their own machine, at their own runtime, from parts they obtained separately.
3. **The precedent is already in the tree.** `crates/plugins/*` exist as separately-licensed crates
   for exactly this reason, and `lodestone-nav` already carries a different licence from the
   workspace for a copyleft-boundary reason.

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
  the port's semantics are already thread-safe. What is unproven is the *JVM-side* lifecycle — thread
  attach/detach around a port, and what happens to an outstanding request when a plugin thread dies
  mid-call. This needs the JNI spike.
- **Reentrancy in the other direction** — Rust calling Java calling Rust calling Java. The port
  bounds Rust-side lock acquisition, but nothing yet bounds *stack depth* across the boundary.
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

- **no crate in the workspace names the bridge** (a manifest scan across every `Cargo.toml` under
  `crates/` and `xtask/`), with a control that finds `lodestone-ecs`'s many dependents using the same
  parser — because a parser that read nothing would certify the bridge as unreferenced forever;
- **the bridge names no unconditional JVM-linking crate**, permitting `optional = true` only, which
  is the shape the JNI layer must land in.

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

Still open: JNI **global refs** for the Java-side objects, and the `DeleteGlobalRef` discipline that
pairs with `release()`. That needs a JVM to develop against.

---

## 7. How to change this, and the gotchas

- **Do not add a handle field to `WorldPort`.** One field re-opens the deadlock and still compiles;
  `tests/reentrancy.rs` is what catches it. Same for removing the request deadline.
- **Do not wire the bridge into any crate's default dependencies.** When it is wired, it goes behind
  an optional dependency and a default-off feature, and `zero_cost_graph.rs` should be *updated to
  assert that shape* rather than deleted.
- **When re-running the census, re-pin.** Paper publishes several builds a week and the counts move.
  `scripts/fetch-paper.sh` carries the pin (version, build, sha256) so a number in this document can
  be traced to an exact input; update the pin and the numbers together.
- **A census of the paperclip download is not a census.** See §1.5. If the numbers come back tiny,
  that is the bug.
- **The 6,991 is a lower bound**, not a total: reflection and `MethodHandle` bootstraps are outside
  what a constant-pool walk can see.

## 8. Configuration

- `scripts/fetch-paper.sh` — the pinned Paper build (26.2 / 121 / sha256
  `0de30efb024bc8b83c9c7d507d11802897ad8056b6110ec09fe1a91d126ccb54`). Writes to
  `.cache/paper/26.2/paper.jar`, which is outside git.
- `lodestone_jvm_bridge::port::DEFAULT_REQUEST_DEADLINE` — how long a Java-side call waits for the
  tick thread.
- `nms-census --prefix / --internal / --no-recurse / --top / --all` — see `--help`.

## 9. Dependencies

- `crates/lodestone-nms-census` — `zip`, `anyhow`. **No JVM**, deliberately: it must run on a machine
  with no Java, which is this one.
- `crates/plugins/lodestone-jvm-bridge` — `lodestone-ecs` only, plus `lodestone-plugin-support` as a
  dev-dependency for the reentrancy harness. **No `jni`, no `libjvm`.**
- The spike and the paperclip step need Apple `container` and an `eclipse-temurin` image — see
  `docs/oracle-runtimes.md`. The host needs no `java`.
