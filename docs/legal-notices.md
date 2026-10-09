# Legal notices and attribution

## What it is

The record behind the repository's `README.md` disclaimer, `NOTICE` and `LICENSE`: what an IP and attribution audit found, on what evidence, and which questions are for counsel. It is the "why" for those files and not a legal opinion.

## How it works

Three questions, kept apart because evidence and remediation differ.

**1. Is third-party source checked in? No.** `.cache/` (the decompiled behavioural reference) is gitignored with zero tracked files; no tracked `*.jar` or `*.class` exists, and none was ever added and deleted. The 37 tracked `*.java` files (all under per-crate `oracle-java/`) each drive the real, unmodified server jar through its public API and reflection, printing results rather than reproducing method bodies. The one partial exception is `crates/lodestone-physics/oracle-java/MoveOracle.java`, a from-scratch re-implementation of player movement written to obtain ground-truth float and double bit patterns from a JVM. It is independently authored, not copied, but is a full re-implementation of proprietary logic with method names close to two of the original's, so it is named here.

**2. Are third-party Rust Minecraft projects a dependency or a source of copied code? No.** `azalea`, `ferrumc` and `Pumpkin` appear in no tracked `Cargo.toml` or `Cargo.lock`. They are cited by name as design references: `azalea` (MIT) in `crates/lodestone-ecs` and its design docs, `ferrumc` (MIT) in `docs/architecture.md`, and `Pumpkin` (GPL-3.0, the only copyleft one) in `docs/plans/worldgen-rewrite.md`, which records reading its source at a pinned commit and borrowing engineering shapes. No source from any is reproduced. `NOTICE` has the licenses.

**3. Trademark and affiliation language.** `README.md` states that Lodestone is not affiliated with, endorsed by or associated with Mojang, Microsoft or Minecraft. It cannot cover every use of the word: most of the roughly one million tracked `minecraft:` strings are wire-protocol namespace prefixes that cannot be renamed, and prose citing vanilla behaviour is a standing convention. The highest-attention category was user-visible UI strings in `crates/lodestone-shell` mirroring Mojang's copy. Owner decisions:

- Kept: the `"Minecraft Realms"` button label (names a real feature) and the advancement titles (game data, so changing them is a fidelity bug); `"Sign in with Microsoft"`, `"Microsoft account"` and `"Contacting Microsoft..."` (they describe the real OAuth flow).
- Reworded to Lodestone's own copy: the title screen (draws "LODESTONE" in the menu font, a two-line "Not an official Minecraft product. Not approved by or associated with Mojang or Microsoft." notice, and `"Lodestone <mc-version>"` bottom left), the "Add Server" hint, the built-in resource-pack description, the telemetry-consent text (also dropping the Microsoft privacy-statement link, which disclosed a pipeline this client lacks), the no-profile sign-in failure message, and an options tooltip naming the Mojang Studios loading screen.

## How to change it

- **New third-party reference.** If `azalea`, `ferrumc`, `Pumpkin` or another project becomes an actual Cargo dependency, add it to `NOTICE`'s "Third-party design references" with its license and confirm the attribution duties (MIT and Apache-2.0 need license text and copyright notice with a distributed binary; GPL-3.0 adds copyleft on the combined work) before merging.
- **UI strings.** If an audit finds Lodestone copy mirroring Mojang wording, list the exact string and location and let the owner decide keep, reword or drop per string.
- **Oracle invariant.** Any new `oracle-java/*.java` file must call into the real jar rather than reproduce a method body, and its header comment must say so (every existing one does); a future audit greps for that sentence first.

## Configuration

None. Lodestone-owned code is `GPL-3.0-or-later`; workspace crates inherit it via `license.workspace = true`.

## Dependencies

`NOTICE`, `LICENSE` and this doc are read together by anyone auditing the IP posture; keep them consistent rather than duplicating facts that could drift.
