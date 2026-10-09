# Protocol packet ranges and data-driven dispatch

## What it is

Two mechanisms that let one packet definition serve a range of protocol versions and let a family's clientbound dispatch be checked at construction time instead of falling through a silent `_ =>` arm. The sharing design is in [multi-version-protocol-dedup](plans/multi-version-protocol-dedup.md); `v1-8`, `v1-9` and `v1-14` dispatch through it, and `v1-9` is a four-protocol era crate built on it ([protocol-1-9-era](protocol-1-9-era.md)).

## How it works

**Container-level protocol ranges.** `#[derive(Packet)]` emits `NAME`/`STATE`/`BOUND` and `PROTOCOLS: ProtocolRange` (`lodestone_core::ProtocolRange`, inclusive). Declare it with `#[mc(protocols = "47..=754")]`; omitted means `ProtocolRange::ALL`. Unlike field-level `#[mc(since = N)]`/`#[mc(until = N)]`, which change which bytes a field contributes within a valid call, the container range is a hard precondition: `Encode`, `Decode` and `decode_context` check `ctx.version` first and return `Error::PacketOutOfProtocolRange`. Packets without a range never run the check.

**Data-driven dispatch.** `lodestone_core::dispatch` replaces an `if packet_id == X` chain (and its `_ =>` island) with `Table::build`, given the protocol's `(name, id)` table (the `ENTRIES` shape `gen-packet-ids` emits), a slice of `(name, Handler<T>)` bindings and a slice of `IGNORED` entries (name plus reason, for packets deliberately untranslated). `Handler<T>` pairs a `ProtocolRange` with a family-defined payload (the module knows nothing of `ClientEvent` or sessions); `IGNORED::ranged` carries a range, `IGNORED::new` means `ALL`. Construction fails loudly naming the packet on: a wire id with no handler and no in-range ignore (`UnlistedId`), a handler whose range excludes the protocol (`OutOfRange`), a handler bound to a name absent from the table (`UnboundHandler`), a duplicate handler, or a stale ignore. `Table::get` is the runtime `id -> &T` lookup.

The absence checks are range-qualified, which lets one handler list serve several protocols: an entry whose range excludes the protocol is expected to find no id and is skipped; one whose range includes it and finds none is a defect. An out-of-range ignore never excuses an id the protocol really carries (it falls to `UnlistedId`), so a range cannot silence a live packet. Each half has a negative control in `dispatch.rs`.

**Canonical name aliases.** `gen-packet-ids` gives `PacketEntry` a `canonical_name: Option<String>`. Mojang-sourced reports are self-aliased; minecraft-data-sourced ones resolve through `MINECRAFT_DATA_CANONICAL_ALIASES`, empty today because a verified mapping needs oracle work (a `--reports` run on the old jar or a captured-bytes comparison), not a spelling guess. Generated `packet_ids.rs` carries a `CANONICAL_NAMES` table so a legacy table can be joined against v26-2's (v1-14 and v26-2 share only 7 of 88 `ENTRIES` names as plain strings).

## How to change it

- Add a verified alias by appending one `(from, to)` pair to `MINECRAFT_DATA_CANONICAL_ALIASES` in `xtask/src/lib.rs`.
- To convert a family: build the table once per adapter construction from the protocol's `ENTRIES`, a `static CLIENTBOUND` handler list and a `static IGNORED` list, and propagate `Table::build`'s error (swallowing it recreates the hidden island).
- A multi-protocol family builds one table per protocol (`v1-9` caches four in `OnceLock`s) and gives a range to every handler or ignore naming a packet only some protocols carry; leaving it at `ALL` fails construction for the others, intentionally.
- A packet moving into a shared protocol-common crate declares its real range via `#[mc(protocols = "a..=b")]`; widen it only alongside a capture from the newly covered protocol's oracle.

## Configuration

None; `ProtocolRange`, `Handler`, `Table` and `IGNORED` are plain library types.

## Dependencies

`lodestone-macros` (the attribute) and `lodestone-core` (`ProtocolRange`, `Error::PacketOutOfProtocolRange`, `dispatch`). Neither depends on any `crates/versions/*` family, so the version seam (`cargo check -p lodestone-shell --no-default-features`) is unaffected.
