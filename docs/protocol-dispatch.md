# Protocol packet ranges and data-driven dispatch

## What it is

Two additions that let one packet definition serve a range of protocol versions,
and let a family's clientbound dispatch be checked at construction time instead
of falling through a silent `_ =>` arm. Landed as Stage 1 of the multi-version
protocol dedup plan (`docs/plans/multi-version-protocol-dedup.md`); `v1-8`,
`v1-9` and `v1-14` now all dispatch through it, and `v1-9` is a four-protocol
era crate built on it (see [`protocol-1-9-era.md`](./protocol-1-9-era.md)).

## How it works

**Container-level protocol ranges.** `#[derive(Packet)]` already emitted
`NAME`/`STATE`/`BOUND`; it now also emits `PROTOCOLS: ProtocolRange`
(`lodestone_core::ProtocolRange`), an inclusive `start..=end` pair. Declare one
with `#[mc(protocols = "47..=754")]` on the container; omit it and the
container gets `ProtocolRange::ALL` (every existing packet today). Unlike the
field-level `#[mc(since = N)]`/`#[mc(until = N)]` predicates — which change
which *bytes* a field contributes within an already-valid call — the container
range is a hard precondition: `Encode`/`Decode` (and the `decode_context`
inherent method) check `ctx.version` against it *before* touching the body,
returning `Error::PacketOutOfProtocolRange` when the call is for a version
outside the declared range. A packet with no declared range never runs this
check, so every existing hand-copied packet in `crates/versions/*` is
unaffected until it opts in.

**Data-driven dispatch.** `lodestone_core::dispatch` replaces a family's
`if packet_id == X { .. } else { .. }` chain (and its terminal `_ =>` island)
with `Table::build`, given: the protocol's own `(name, id)` packet table (the
shape `gen-packet-ids` already emits as `ENTRIES`), a slice of
`(name, Handler<T>)` bindings, and a slice of `IGNORED` entries (name +
reason, for packets deliberately left untranslated). `Handler<T>` pairs a
`ProtocolRange` with whatever payload a family actually runs (`T` is generic —
this module has no notion of `ClientEvent`, `Directive`, or any session type),
and `IGNORED` carries a range too (`IGNORED::ranged`; `IGNORED::new` keeps
`ProtocolRange::ALL`).
Construction fails loudly, naming the offending packet, on: a wire id with no
handler and no in-range ignore entry (`UnlistedId`); a handler whose range
excludes the protocol being built for (`OutOfRange`); a handler bound to a name
absent from the id table (`UnboundHandler`); a duplicate handler; or a stale
ignore entry. `Table::get` then does the runtime `id -> &T` lookup.

The two absence checks are qualified by range, and that qualifier is what makes
one handler list serve several protocols: a handler or ignore entry whose
declared range **excludes** the protocol being built for is expected to find no
id, and is skipped. One whose range includes it and still finds none is the
defect it always was. An out-of-range ignore entry does not excuse an id the
protocol really carries — that falls through to `UnlistedId`, so a range cannot
be used to silence a live packet. Each half has a paired negative control in
`dispatch.rs`'s own tests.

**Canonical name aliases.** `xtask`'s packet-id generator (`gen-packet-ids`)
gained a `canonical_name: Option<String>` field on `PacketEntry`. A
Mojang-sourced report is its own canonical name (self-aliased). A
minecraft-data-sourced report resolves through `MINECRAFT_DATA_CANONICAL_ALIASES`
— empty today, since a verified mapping needs real oracle work (a `--reports`
run against the old jar, or a captured-bytes comparison), not a spelling
guess. The generated `packet_ids.rs` carries a `CANONICAL_NAMES: &[(&str,
&str)]` table of every entry with a known canonical name, so a later stage
can join a legacy protocol's table against v26-2's without depending on
matching literal strings across sources (measured: v1-14 and v26-2 agree on
only 7 of 88 `ENTRIES` names as plain strings).

## How to change it

- Add a verified legacy-name alias by appending one `(from, to)` pair to
  `MINECRAFT_DATA_CANONICAL_ALIASES` in `xtask/src/lib.rs` — nothing else
  needs to change to pick it up.
- To convert a family's dispatch to `Table`/`Handler`/`IGNORED`, build the
  table once per adapter construction from that protocol's generated
  `ENTRIES`, a `static CLIENTBOUND` handler list, and a `static IGNORED` list;
  propagate `Table::build`'s error rather than swallowing it, since a
  construction failure here is exactly the island `_ =>` used to hide.
- A multi-protocol family builds one table **per protocol** (`v1-9` caches four
  in an array of `OnceLock`s) and gives a range to every handler or ignore
  entry naming a packet only some of its protocols carry. Leaving such an entry
  at `ProtocolRange::ALL` fails construction for the protocols without it,
  which is the intended loud failure rather than a trap.
- A packet moving into a shared `lodestone-protocol-common`-style crate
  (a later stage) declares its real range via `#[mc(protocols = "a..=b")]`
  instead of leaving it at `ALL`; widen the range only alongside a capture
  from the newly-covered protocol's oracle, per the dedup plan's guard
  against an unreviewed range widening.

## Configuration

None. `ProtocolRange`, `Handler`, `Table`, and `IGNORED` are plain library
types; nothing here reads an environment variable or a feature flag.

## Dependencies

`lodestone-macros` (the `#[mc(protocols = ...)]` attribute) and
`lodestone-core` (`ProtocolRange`, `Error::PacketOutOfProtocolRange`, the
`dispatch` module) are the only two crates involved. Neither depends on any
`crates/versions/*` family, and no family depends on the other — the version
seam (`cargo check -p lodestone-shell --no-default-features`) is unaffected.
