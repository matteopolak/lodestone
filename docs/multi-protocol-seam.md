# The multi-protocol seam: constructing an adapter for the protocol it negotiated

## What it is

The change that lets one `crates/protocol/vNNN` crate serve several protocol revisions
instead of exactly one. Unit U2 of epic #343's dispatch plan
([`plans/multi-version-protocol.md`](./plans/multi-version-protocol.md)), and a
prerequisite for every grouped family the plan schedules — `v110` (1.9.4/1.10.2/1.11.2),
`v498` (1.14.4/1.15.2), `v756` (1.17.1/1.18.2). Building those single-protocol first and
retrofitting the seam afterwards is the expensive order, which is why this landed before
any of them.

## What was actually blocking it

`lodestone-registry`'s `FAMILIES` table built each family with a **zero-argument**
`make: fn() -> Box<dyn VersionAdapter>`. The negotiated protocol number reached the adapter
nowhere, so a family crate had nothing to select a per-protocol `packet_ids` table by — it
could only ever be one revision. Resolution also worked by *constructing every family's
adapter in turn* and asking each one `supports`, allocating and discarding a box per family
it skipped.

Note what was **not** blocking it: nothing above the registry. The shell already passes the
negotiated protocol to `adapter_for_protocol` — that is the version seam's whole design (see
[`singleplayer.md`](./singleplayer.md)) — so `net.rs`, `app.rs` and `sim.rs` are untouched by
this.

## How it works

Three pieces, one per layer:

| layer | before | after |
|---|---|---|
| family crate | `pub const PROTOCOL: i32`, `supports` compares against it, `adapter()` | adds `pub const PROTOCOLS: &[i32]` and `pub fn adapter_for(protocol: i32)`; `supports` tests membership in `PROTOCOLS` |
| registry `Family` | `{ label, make: fn() -> Box<dyn …> }` | `{ label, protocols: &'static [i32], make: fn(i32) -> Box<dyn …> }` |
| registry resolution | construct every adapter, ask each `supports` | match `protocols` with no allocation, then construct exactly one, **for that protocol** |

`Family::protocols` **points at the family crate's own `PROTOCOLS` const**; it never
restates the numbers. That is the same reasoning `ServerFamily::supports` already gave for
delegating to the family's `VersionAdapter::supports`: a family's coverage gets one
definition, so the registry's view cannot drift from the adapter it will resolve to. The
test `every_family_entry_agrees_with_its_own_adapter` is the guard, and its negative half
(a family must deny `protocol + 1`) is the load-bearing one — a `supports` returning `true`
unconditionally passes the positive half alone.

`adapter_for_protocol` delegates to a private `resolve_adapter(&[Family], i32)`, which is
what the tests drive. That factoring is not cosmetic: **no compiled-in family is
multi-protocol yet**, so asserting against the real `FAMILIES` table structurally cannot
distinguish an adapter that carries the negotiated protocol from one that ignores it. The
tests supply their own table containing a fake two-protocol family whose two packet-id
tables disagree, and assert the resolved adapter encodes the id belonging to the protocol it
was built for. The dispatch under test is the production function; only the adapter is fake.

## v770 is the one asymmetric entry

`v770` has no `PROTOCOLS`/`adapter_for`; its registry entry spells coverage as
`&[lodestone_v770::PROTOCOL]` and discards the protocol argument. That is sound rather than
deferred work: `v770` is single-protocol (776), and the plan keeps it that way deliberately
because it is both the canonical block-state space and the only family implementing
`ServerProtocol`, so leaving the hosting seam single-valued is the simplification. Give it
the pair if it ever gains a second revision.

## How to change it, and the gotchas

- **Adding a protocol to an existing family is now one line** — extend that crate's
  `PROTOCOLS`. The registry needs no edit at all, because it borrows the slice. Everything
  else is inside the family: branch on the negotiated protocol in `adapter_for` to pick the
  right generated `packet_ids` module, and store it on the adapter so
  `protocol_version()` reports what was negotiated rather than a fixed constant.
- **A grouped family must actually store the protocol.** The three existing families do
  not, because they have one each and there is nothing to select; `adapter_for` only
  `debug_assert!`s membership. Copying that body into a multi-protocol family would compile,
  pass its own tests, and silently serve one era's packet ids to every version in the group —
  the "1.12.2 client wearing 1.16 packet IDs" failure the roadmap already recorded once.
- **Never derive a protocol from a folder name.** `v735` speaks **754**. The registry entry
  reads `lodestone_v735::PROTOCOLS` rather than naming a number, which is what makes that
  impossible to get wrong there.
- **`supported_protocols()` changed meaning**, from "the primary protocol of every family"
  to "every protocol any family handles". Identical output while every compiled family is
  single-protocol; a caller wanting one-number-per-family needs a different function.
- **`just check-seam` is this unit's health check.** `cargo check -p lodestone-shell
  --no-default-features` must still pass — the shell has to compile with **no** family
  compiled in, and this is the unit most able to break that, since it changes the shape of
  the table the version-free build compiles to empty.

## Configuration

None. The `vNNN` cargo features on `lodestone-registry` decide which families exist, exactly
as before.

## Dependencies

`lodestone-model` (the `VersionAdapter` trait, whose `supports` doc now states the set
contract) and the feature-gated family crates. Unchanged by this work.
