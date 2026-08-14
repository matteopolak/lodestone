# Protocol version crate naming, and what `v47`/`v340`/`v735` already are

## What it is

Groundwork for GitHub epic #343 (support 1.7.10 through 26.2, one crate per major
version's latest patch, via a single canonical internal version plus a per-version
translation layer — the ViaVersion shape, not sixteen parallel simulations). This doc
settles two questions before fifteen more `crates/protocol/vNNN` crates get created:

1. What does the `vNNN` suffix actually denote today, given `CLAUDE.md` calls the active
   crate `v770` while stating the protocol is `776`?
2. What are the three existing non-770 families (`v47`, `v340`, `v735`) — dead weight, or
   a head start?

No crate was renamed and no code changed as part of this doc. Renaming is recommended, not
executed — see "Recommendation" below.

## What the suffix denotes: two different rules are already in use

Reading each crate's own `PROTOCOL` constant and `VersionAdapter::protocol_version()`
against its folder name:

| crate | folder suffix | `PROTOCOL` it implements | Minecraft version it implements | suffix == implemented protocol? |
|---|---|---|---|---|
| `v47` | 47 | 47 | 1.8.8/1.8.9 | yes |
| `v340` | 340 | 340 | 1.12.2 | yes |
| `v735` | 735 | **754** | 1.16.5 | **no** |
| `v770` | 770 | **776** | 26.2 | **no** |

`v47` and `v340` are named for the exact protocol number the crate implements. `v735` and
`v770` are not — `crates/protocol/v735/src/adapter.rs` implements protocol **754** (its own
doc comment says so: `"Version adapter implementing protocol 754 (Minecraft 1.16.5)"`), and
`crates/protocol/v770/src/adapter/` implements protocol **776** (26.2, per `CLAUDE.md`
and `DESIGN.md`).

`DESIGN.md` §2 explains why, in an aspirational 17-family plan that was later cut to four
(`HANDOFF.md` §1): each crate was to be "named for its **lowest** protocol number," where
a single crate's *family* could span several minor releases that share a wire format —
e.g. the plan's `v210` for "1.10–1.12.2" (1.10's own protocol is 210; 1.12.2's is 340).
Checked against `vendor/minecraft-data`'s `protocolVersions.json`:

- **1.16** (the initial 1.16 release) has protocol **735**. `v735` implements 1.16.5
  (protocol 754) — the *latest patch* of that family, named for the family's *lowest*
  member.
- **1.21.5** has protocol **770**, matching `DESIGN.md`'s own statement that `v770` "sits
  exactly on the boundary" of the family spanning 1.21.5–26.2. `v770` implements 26.2
  (protocol 776) — again the latest patch, named for the family's lowest member.
- **1.12** (the initial 1.12 release) has protocol **335**, one lower than `v340`'s own
  340. Under the "named for the family's lowest protocol" rule, this crate should be
  `v335`. It is not — it is named for 1.12.2's own protocol number, exactly what it
  implements. `v47` has no such ambiguity since 1.8.0–1.8.9 never bumped the protocol
  number, so "the version's own number" and "the family's lowest number" coincide by
  accident.

So the inconsistency predates the `v770`/protocol-776 note in `CLAUDE.md` — it is already
present *between* `v340` and `v735`, not introduced by `v770`. Two different rules
generated the four existing names, and they only ever agreed when the underlying numbers
happened to collide (`v47`) or nobody built the earlier minors of the family (`v340`,
arguably — 1.12/1.12.1 were never implemented, so nothing forced the "lowest" question).

## Why this matters for the next fifteen crates

This epic's plan — one crate per major version's *latest patch*, sixteen discrete targets,
not families spanning several minors — removes the scenario the "lowest protocol in the
family" rule was built for. Under a one-version-per-crate plan, "the family" *is* the one
version, so "named for the family's lowest protocol" and "named for the version's own
protocol" become the same rule for every *new* crate. The only place the two rules still
diverge is the two crates that were built under the old, wider-family mental model:
`v735` (built for a family that was never widened past 1.16.5) and `v770` (ditto, past
26.2).

## Recommendation

**Name each new crate after the exact protocol number of the single version it
implements** — the `v47`/`v340` rule, which is also what every new crate reduces to under
the current one-version-per-crate plan. Concretely, per `docs/version-table.md`'s table:

`v5` (1.7.10) · `v47` (1.8.9, exists) · `v110` (1.9.4) · `v210` (1.10.2) · `v316` (1.11.2)
· `v340` (1.12.2, exists) · `v404` (1.13.2) · `v498` (1.14.4) · `v578` (1.15.2) · `v754`
(1.16.5) · `v756` (1.17.1) · `v758` (1.18.2) · `v762` (1.19.4) · `v766` (1.20.6) · `v774`
(1.21.11) · `v776` (26.2)

That makes `v47`/`v340` stay correct as-is, and leaves `v735`→`v754` and `v770`→`v776` as
the two existing crates whose names would need to change for the whole set to follow one
rule.

### The tradeoff

- **Keep `v735`/`v770` as they are, document the rule as "protocol number of the *oldest*
  release the crate was scoped for, not necessarily what it implements today."** Zero
  mechanical cost. Permanent tax: every new contributor re-derives (or is told) that
  `v770` means 776, forever, and the fifteen new crates either inherit a second naming
  rule that only applied historically (confusing) or silently diverge from `v735`/`v770`'s
  convention (inconsistent within the same `crates/protocol/` directory).
- **Rename `v735`→`v754` and `v770`→`v776`.** One rule for all nineteen eventual crates,
  matching what each crate actually implements — the property a reader most wants from a
  version-number-suffixed folder name. Cost is a wide, purely mechanical change across
  files this task does not own, and it collides with whatever other agents are doing in
  this checkout right now (`CLAUDE.md`: "Single shared checkout, no per-agent worktrees").
  **Not executed here** — recommended only, with the exact steps below so it can be done
  as its own change when the checkout is quiet.

### Mechanical steps for the rename (not executed)

1. `git mv crates/protocol/v735 crates/protocol/v754` and `git mv crates/protocol/v770
   crates/protocol/v776` (or plain `mv` + `git add`, given the "never stage a directory"
   rule — stage the moved files explicitly).
2. In each moved crate's `Cargo.toml`: `name = "lodestone-v735"` → `"lodestone-v754"`
   (same for 770/776).
3. Every crate that depends on them by name: `lodestone-registry/Cargo.toml` (dependency
   line + feature name), root workspace `Cargo.toml` (`[workspace.dependencies]` alias,
   `members`/`default-members` if listed there), `lodestone-server/Cargo.toml` (the direct
   path dependency `v770` takes on `lodestone-server` is the reverse direction and doesn't
   need touching, but anything depending *on* `lodestone-v770` does).
4. `crates/lodestone-registry/src/lib.rs`: the `FAMILIES` const's `label`, `protocols` and
   `make` fields, and the `#[cfg(feature = "v770")]` / `"v735"` gates. (`protocols` and
   `make`'s protocol argument arrived with the multi-protocol seam — see
   [`multi-protocol-seam.md`](./multi-protocol-seam.md).)
5. Every in-source reference: `grep -rn "lodestone_v770\|lodestone-v770\|\bv770\b"` and the
   `735` equivalent, across `crates/`, `xtask/`, `scripts/`, `docs/`, `DESIGN.md`,
   `HANDOFF.md`, `CLAUDE.md`. This is the bulk of the work — `CLAUDE.md`'s "Active scope is
   v770 only" line is explicitly called out as *not* this task's edit to make, but a rename
   commit would need to touch it in the same breath as the folder move, which is exactly
   why this task does not execute the rename.
6. `xtask/check-connected.toml` and any other allowlist/config files naming the crate by
   package name.
7. Live-oracle feature flags and test files that embed the name, e.g. `live-v770`
   (`HANDOFF.md` references `live_physics_bot.rs` behind `live-v770`).
8. Re-run `cargo xtask check-isolation`, `cargo xtask check-connected`, `cargo xtask
   connectedness`, and `cargo check --workspace --all-targets` after, since the rename
   touches dependency edges the isolation/connectedness tooling specifically watches.

None of this was done. It is scoped here so the decision-maker can size it precisely
rather than estimate it.

## What `v47`, `v340`, `v735` already are (factual survey, no refactor)

All three are **already structured as translation layers**, not "sixteen parallel
simulations" — this is the same shape this epic wants, just not yet extended to all
sixteen target versions and not yet complete in breadth. Concretely, each:

- Depends on only `lodestone-core`, `lodestone-macros`, `lodestone-model`, and
  `lodestone-world` (`v340`/`v735` also pull in `uuid`) — the same version-free crates
  `v770` depends on, confirmed by reading each crate's `Cargo.toml`. None of the three
  duplicate the canonical model; each decodes its own wire format directly into
  `lodestone-model`'s `ClientEvent`/`Directive` types and `lodestone-world`'s paletted
  chunk storage, exactly the "decode old versions and map them into our existing
  26.2-shaped state" architecture this epic specifies.
- Implements `VersionAdapter` — the **client-direction** seam only (Lodestone-as-client
  joining an external server of that version: `begin_login`, inbound packet handling into
  `ClientEvent`/`Directive`, `ClientAction` encoding for the outbound side). None of the
  three implements `ServerProtocol` (`crates/lodestone-server/src/protocol.rs`'s
  **server-direction** seam — Lodestone-as-server accepting an inbound client of that
  version), which only `v770` does today via `V770ServerProtocol`. So today, an old-version
  *client* joining a real vanilla server of that version is proven (each has a
  `#[ignore]`d live gate against a real server of its own version — see below); an
  old-version *client* joining Lodestone's own integrated server is not implemented for any
  of the three. If this epic's real end goal is old clients connecting to a Lodestone
  server (the more ViaVersion-like direction), that inbound half is unbuilt for all three
  and is exactly the gap a genuinely new translation layer would need to close first.
- Is live-verified against a real server of its target version (`HANDOFF.md` §1, spot
  checked this session for `v735`: `crates/protocol/v735/tests/live_chunk.rs` targets
  `127.0.0.1:25573` for a live 1.16.5 server — an earlier `HANDOFF.md` note about this same
  test pointing at a stale 1.12.2 port (25568) has since been fixed; that specific
  staleness claim is resolved, not current).
- Is measurably **incomplete in action-encode breadth**: per `HANDOFF.md` §1 (not
  independently re-run here — this is a survey, not a re-verification pass), `v47`/`v340`
  encode 16/43 and `v735` encodes 17/43 of `ClientAction` variants versus `v770`'s 42/43.
  Concretely, **a 1.8.9/1.12.2/1.16.5 client cannot break a block or use `ContainerClick`
  today.** Some of that gap is *correct by design* (some actions have no wire form on
  older protocols — e.g. `SetPlayerInput` postdates all three), not unfinished work;
  `HANDOFF.md` explicitly requires producing an absent-by-design-vs-not-done table before
  resuming any of the three, because conflating the two is exactly how `v735` previously
  shipped registered with an undischarged `SHAPE_REVIEW.toml`.
- Is cleanly deletable and independently measured as such — `cargo xtask check-deletable
  <family>` reports `v47` at 5 manifest lines, `v340` at 4, `v770` at 8 (`HANDOFF.md` §1).
  Not re-run for this survey; cited as an existing, reproducible measurement.

**Which of the sixteen target versions do the existing three already cover?**
All three, exactly: `v47` is 1.8.9, `v340` is 1.12.2, `v735` implements 1.16.5 — all three
appear verbatim in `EPIC_343_VERSIONS`/the version table in `docs/version-table.md`. That
is not a coincidence worth ignoring: three of the sixteen target versions already have a
real, live-verified (if incomplete and client-direction-only) implementation, rather than
starting from zero.

**Net assessment: head start, not dead weight** — with the specific caveat that the head
start is entirely on the outbound/client-adapter side. The inbound/server-translation side
that a ViaVersion-shaped "old client joins our server" architecture needs is unbuilt in
all three, same as it would be for a brand-new crate.

## Prior art already in the repo

`docs/roadmap/protocol.md`'s "Multi-version: what it would cost" section and
`HANDOFF.md` §1 cover the cost/risk analysis this doc's survey draws on (measured ~900
irreducible hand-written lines per family, ~1 day each, concentrated in `adapter.rs`
dispatch and `chunk.rs` decode) and were filed as a design question the
project's brief explicitly declined to answer at the time. This epic reads as the
follow-on decision to that open question; this doc does not re-litigate the cost analysis,
only the two questions in scope here (naming, and what already exists).

## Dependencies

- `crates/protocol/{v47,v340,v735,v770}` (read-only for this doc; not modified).
- `crates/lodestone-registry/src/lib.rs`'s `FAMILIES` table (read-only for this doc).
- `vendor/minecraft-data/data/pc/common/protocolVersions.json` for the protocol-number
  cross-checks in the "What the suffix denotes" table.
- `docs/version-table.md` for the full sixteen-version reference table.
