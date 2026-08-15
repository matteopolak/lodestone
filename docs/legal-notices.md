# Legal notices and attribution

## What it is

The record behind this repository's `README.md` disclaimer, `NOTICE`, `LICENSE-MIT`, and
`LICENSE-APACHE` files: what an IP/attribution audit found, on what evidence, and which
questions are for counsel rather than for an agent working in this repo. This doc is the
"why" for those files; it is not itself a legal opinion, and nothing in it should be read as
one.

## How it works

Three separate questions, kept separate because they have different evidence and different
remediations:

**1. Is any third-party source code checked in?** No. `git ls-files .cache` returns zero
tracked files (`.cache/` — the decompiled reference used for behaviour verification — is
gitignored); there are zero tracked `*.jar` or `*.class` files; and the 37 tracked `*.java`
files (all under per-crate `oracle-java/` directories, one under a test fixture path) were
read in full for this audit. Every one *drives* the real, unmodified vanilla server jar
through its public API and reflection — bootstraps registries, walks a registry or block-state
table, calls a public or `setAccessible`d method, and prints the result — rather than
reproducing any method body. One file is a partial exception worth naming precisely:
`crates/lodestone-physics/oracle-java/MoveOracle.java` is a from-scratch Java re-implementation
of the player-movement algorithm (its own header says so), written to obtain ground-truth
`float`/`double` bit patterns from a real JVM; it is not decompiled or copied source, but it is
a full, independently authored reimplementation of proprietary game logic, and its method names
mirror vanilla's own (`restituteMovementAfterCollisions`, `travelFallFlying`) closely enough
that this is named explicitly rather than left implicit. `git log --diff-filter=A` over `*.java`,
`*.class`, and `*.jar` was also checked against the current tree; the counts match, so no file
of that class was ever added and later deleted.

**2. Are any third-party Rust Minecraft projects a dependency, or a source of copied code?**
No dependency: neither `azalea`, `ferrumc`, nor `Pumpkin` appears in any tracked `Cargo.toml`
or `Cargo.lock` in this workspace. All three are cited *by name*, as design references, in code
comments and in `docs/` — `azalea` (MIT) heavily in `crates/lodestone-ecs` and
`docs/bevy-migration.md`, `ferrumc` (MIT) once in `DESIGN.md`, and `Pumpkin` (**GPL-3.0**, the
one copyleft license among the three) in `docs/plans/worldgen-rewrite.md`, which records reading
Pumpkin's source at a pinned commit and recommends adopting several of its *engineering shapes*
for a not-yet-written worldgen rewrite. See `NOTICE` for the specifics and the licenses. No
source from any of the three is reproduced in this repository.

**3. Trademark and affiliation language.** `README.md` now states plainly that Lodestone is
not affiliated with, endorsed by, or associated with Mojang, Microsoft, or Minecraft. That
disclaimer does not, and cannot, address every occurrence of the word "Minecraft" in this
repository — most of the roughly one million tracked occurrences of the string `minecraft:`
are the wire-protocol namespace prefix (`minecraft:stone`, `minecraft:diamond_sword`, …),
which is part of the format this software interoperates with and cannot be renamed without
breaking compatibility. Prose usage (docs, code comments citing vanilla behaviour by symbol) is
a second, much smaller category, and is a deliberate, standing project convention — see
`CLAUDE.md`'s "Cite symbols, never line numbers" section — not something this doc changes. The
highest-attention category is the smallest: literal, user-visible UI strings in
`crates/lodestone-shell` that were not just descriptive but mirrored Mojang's own copy —
`"Copyright Mojang AB. Do not distribute!"` and `"Minecraft {version}"` on the title screen, a
`"Minecraft Realms"` button, a resource-pack description, and telemetry/sign-in screen text
close to vanilla's own wording. These are filed as an issue rather than edited here; see "How
to change it".

## How to change it

**Adding a new third-party reference.** If a future change makes `azalea`, `ferrumc`,
`Pumpkin`, or any other project an actual Cargo dependency (not just a cited design reference),
add it to `NOTICE`'s "Third-party design references" section with its license, and confirm the
license's own attribution requirements (MIT and Apache-2.0 both require the license text and a
copyright notice to travel with a distributed binary; GPL-3.0 additionally imposes copyleft
obligations on the combined work) are met before merging.

**The UI-string findings are not fixed by this doc.** They are user-visible product-copy
choices (do we call the "Realms" feature the same name Mojang does; do we reproduce Mojang's
own copyright line), not something an audit should silently edit — see the filed issue for the
specific file/line list and let the owner decide scope, in line with the file-ownership split
this audit worked under.

**The `.java`-oracle finding is a standing invariant, not a one-time result.** Any new
`oracle-java/*.java` file should keep calling into the real jar rather than reproducing a
method body, and its header comment should say so explicitly (every existing one does) — that
sentence is cheap and is exactly what a future audit will grep for first.

## Configuration

None — this is a documentation and licensing artifact, not code. The license each crate
declares lives in `Cargo.toml`'s `license.workspace = true` (root `Cargo.toml`'s
`[workspace.package]` sets `license = "MIT OR Apache-2.0"`); `web/Cargo.toml`,
`web/server/Cargo.toml`, and `xtask/Cargo.toml` do not currently set a `license` field and
were flagged in the filed issue rather than edited here, since editing source-tree
`Cargo.toml` files was out of scope for this pass.

## Dependencies

`NOTICE`, `LICENSE-MIT`, `LICENSE-APACHE`, and this doc are read by anyone auditing the
repository's IP posture; keep them consistent with each other rather than duplicating facts
that could drift. `cargo xtask docs-index` regenerates `docs/README.md`'s table of contents
from this file's H1 and this section; re-run it after any structural edit here.
