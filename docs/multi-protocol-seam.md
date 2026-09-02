# The multi-protocol seam: version crates, canonicalisation, and framing

## What it is

How `crates/versions/<family>` family crates are structured and named, how the registry resolves
a negotiated protocol number to the right adapter, how each pre-26.2 family translates its own
wire block-state representation into the canonical 26.2 block-state space, the reference table
of protocol/data-version numbers this project tracks per Minecraft release, and the packet-framing
shape (length prefix, compression, the one frame that carries no packet) all four families
share underneath `lodestone-net`.

## How it works

### Family crate shape and dispatch

Each `vNNN` crate implements `lodestone_model::VersionAdapter` — the **client-direction** seam
(joining an external server of that version: `begin_login`, inbound decode into
`ClientEvent`/`Directive`, `ClientAction` encoding outbound). Only `v26-2` also implements
`ServerProtocol` (`lodestone-server`'s **server-direction** seam), so it is the only family we
can host; `v1-8`, `v1-9`, and `v1-14` are client-only today.

`lodestone-registry`'s `FAMILIES` table holds, per family, a `label`, a `protocols: &'static
[i32]` slice, and a `make: fn(i32) -> Box<dyn VersionAdapter>` constructor. `protocols` points
at the family crate's own `PROTOCOLS` const rather than restating the numbers, so the registry's
view of a family's coverage cannot drift from what `VersionAdapter::supports` actually resolves
to. Resolution matches `protocols` with no allocation, then constructs exactly one adapter for
the negotiated protocol — replacing an older scheme that built every family's adapter in turn
and asked each `supports`. `v26-2` is the one asymmetric entry: it has no `PROTOCOLS`/
`adapter_for`, its registry entry spells coverage as `&[lodestone_v26_2::PROTOCOL]`, and it
discards the protocol argument, because it is genuinely single-protocol (776) and also the only
family implementing `ServerProtocol` — leaving its hosting seam single-valued is the
simplification, not deferred work.

A small set of codec helpers (`encode_body`, `decode_body`, `decode_body_exact`,
`unpack_degrees`) is shared in `lodestone-core` rather than hand-copied per family, because they
carry no version-specific behaviour and their *error* type can be downgraded to `String` (each
family wraps it back into its own `AdapterError` locally). Helpers whose **success** value is a
`lodestone-model` type (`send`, `json_reason_text`, `game_mode` — returning `Directive`, `Text`,
`GameMode`) could not move the same way, because `lodestone-model` already depends on
`lodestone-core` and the reverse edge would be a dependency cycle; they stay duplicated per
family until `lodestone-model` itself moves those types down, which is a separate decision.

### What the `vNNN` suffix denotes

Package and feature names (`lodestone-v1-8`, feature `v1-8`; and the same pattern for `v1-9`,
`v1-14`, `v26-2`) are named for the *era-start* Minecraft version each family covers, not for a
protocol number — a deliberate move away from an earlier scheme where the suffix was the exact
(or, for two of the four, the *lowest*) protocol number the family implemented. Each crate spans
a whole wire era (`v1-9` really does serve all four of 1.9.4, 1.10.2, 1.11.2 and 1.12.2 —
protocols 110, 210, 316 and 340; see [`protocol-1-9-era.md`](./protocol-1-9-era.md)), and the
Minecraft version reads at a glance in a way a bare protocol number does not.

The directory a family lives in (`crates/versions/1.8`, `crates/versions/1.9`,
`crates/versions/1.14`, `crates/versions/26.2`) is that same era-start version, spelled with a dot
instead of a dash — but it is a *third* thing, independent of both the package suffix and the
protocol number: `crates/versions/1.14` is not `lodestone-v1-14`'s own protocol (754, Minecraft
1.16.5) any more than `crates/versions/26.2` is 776. **Never derive a protocol number — or a
protocol's own coverage — from a folder or package name; ask `VersionAdapter::supports`.** For any
new family added under the current one-crate-per-version plan, name both the folder
(`crates/versions/1.17`, dotted) and the package/feature (`lodestone-v1-17`, feature `v1-17`,
dashed) after the family's own era-start Minecraft version — not a protocol number — and confirm
real coverage through `supports` rather than either name.

### Canonicalisation: translating each family's wire ids into 26.2 block-state ids

`v26-2` decodes directly into the canonical 26.2 block-state space, because it *is* that space.
The other three families do not, and each needed a retrofit — without it, every world joined
through that family meshed and collided as the wrong blocks with a fully green test suite,
because `lodestone-world`'s `PalettedContainer` is version-free and accepts any `u32`.

- **`v1-8` (1.8.9, pre-Flattening)**: the wire carries `(blockId << 4) | meta`, not a
  block-state id at all. `decode_column` passes every cell through
  `lodestone-canonical`'s `canonical::resolve_composite_or_air`, resolved **per cell** (1.8 has
  no palette to translate once).
- **`v1-9` (1.9.4–1.12.2, pre-Flattening)**: the wire carries the same `id:meta` shape, but resolution
  goes through a **generated flattening table** (`lodestone-canonical`'s
  `flattening::lookup`, built and verified against the real 1.13.2 server jar's own
  `DataFixerUpper` — the same conversion vanilla itself runs to upgrade a pre-1.13 world) and
  then a **bridging pass** (`canonical::bridge`) that renames a handful of names/properties the
  1.13.2-era table still spells in an intermediate, later-superseded way (`mob_spawner`→
  `spawner`, `sign`→`oak_sign`, leaves' `decayable`/`check_decay`→`persistent`/`distance`, and
  similar) and fills in properties 26.2 added that pre-1.13 storage cannot express
  (`waterlogged`, defaulted `false`). The flattening table itself distinguishes four outcomes —
  `Resolved`, `NoTableEntry` (a real id/meta pair vanilla never assigned), `RequiresAdditional-
  Context` (identity depends on TileEntity data this crate does not decode — flower pots,
  skulls, double-plant upper halves), `OutOfBounds` (one unreachable slot) — and the bridge maps
  every non-`Resolved` outcome to a **counted, logged air substitution**, never a silent one.
  Resolution happens per **palette entry** where a section has one, per cell for the direct/
  global-palette case.
- **`v1-14` (1.16.5, protocol 754, post-Flattening)**: the wire already carries a single flat
  block-state id, decoded correctly by `PalettedContainer::decode` — but it is 1.16.5's *own*
  flat id space, and thousands of blocks have been registered since, so the same number now
  names a different block in 26.2. Because a post-Flattening state already carries full
  identity, there is nothing left to resolve at runtime: the whole `1.16.5 id -> 26.2 id`
  mapping is baked into a generated array (`lodestone_v1_14::generated_canonical::
  STATE_TO_CANONICAL`) built from two jar-derived state dumps (1.16.5's own `--reports` output,
  and 26.2's), with a small rename table and two generic property fallbacks
  (`waterlogged=false`, `powered=false`) for the states each source names differently.

All three fallback paths share one shape: an unresolvable value becomes air, counted on a
`FallbackTally` and logged once per column if the tally is non-empty — never silent, per the
project's "if you choose air, it must be visible, logged, counted" rule. **`cargo xtask
connectedness` cannot see any of this class of bug** — it answers "is this clientbound packet
reaching anything", and a canonicalisation defect changes *what value* flows through an
already-connected wire, not whether it arrives. A green connectedness run is not evidence a
decoded block id is correct; only a jar-derived oracle (a captured real-server section, or a
live server via RCON `/setblock` + `/testforblock`) is.

### The version table

`crates/lodestone-registry/src/version_table.rs` (generated data in
`generated/version_table.rs`) records, for each of the sixteen versions this project targets
(1.7.10 through 26.2), its protocol number, save-format `DataVersion`, and release date.
`cargo run -p xtask -- version-table` derives each row from, in priority order: Mojang's
version manifest (release date + server jar URL), the jar's own `version.json` (authoritative
for protocol/data version when present), and `vendor/minecraft-data`'s
`protocolVersions.json` as a fallback used only where the jar has none — cross-check-grade
only, never authoritative. Where both a jar and `minecraft-data` are available they must agree
exactly or the tool hard-errors rather than silently preferring one. **The jar-`version.json`
boundary is empirically 1.13.2 → 1.14.4**: 1.13.2's server jar has none, 1.14.4's does — no
target version falls between them, so the boundary is settled without checking intervening
snapshots. Every version from 1.14.4 onward, jar and `minecraft-data` agree exactly.

### Packet framing

`Codec` (`crates/lodestone-net/src/codec.rs`) and `Connection::read_packet`
(`connection.rs`) turn the byte stream into `(packet_id, fields)`. Uncompressed, a frame is
`[VarInt length][packet id VarInt][fields…]`; once `login_compression` sets a threshold it
becomes `[VarInt frame length][VarInt uncompressed length][data]`, where an uncompressed
length of `0` means `data` is not compressed. A **one-byte frame of `0x00`** is legal under
that shape and declares zero bytes of packet data — no packet id at all. Vanilla's own
pipeline drops it silently (`ByteToMessageDecoder` only calls its packet decoder while the
buffer is readable); this codec used to read an id out of it, hit `UnexpectedEof`, and treat
that as a fatal transport error, ending the session. `read_packet` now skips an empty body and
reads the next frame; `read_packet_raw` still returns the empty body unmodified, so anything
reading raw frames must decide for itself.

## How to change it, and the gotchas

- Adding a protocol to an existing family is one line: extend that family's `PROTOCOLS`. The
  registry needs no edit, since it borrows the slice.
- **A grouped (multi-protocol) family must actually store the negotiated protocol** and branch
  on it in `adapter_for` to pick the right packet-id table; copying a single-protocol family's
  body (which only `debug_assert!`s membership) would compile, test green, and silently serve
  one era's packet ids to every version in the group.
- `Family::protocols` must keep pointing at the family crate's own `PROTOCOLS` const, never
  restate the numbers — that is what keeps the registry's view from drifting from the adapter.
- **`just check-seam` (`cargo check -p lodestone-shell --no-default-features`) is this seam's
  health check** — the shell must still compile with no family enabled at all.
- `lodestone-canonical`'s generated tables (`generated/flattening.rs`,
  `generated/canonical.rs` under `v1-9`/`v1-14`) are generated — never hand-edit; regenerate
  with `LODESTONE_REGEN=1 cargo test -p <crate> --test <name> -- --ignored --nocapture` after
  a source jar or the 26.2 registry changes, and re-run the exhaustive drift-guard test (each
  crate has one asserting zero unmapped slots) before trusting the result.
- Adding a fifth shared codec helper to `lodestone-core`: check its return type first — only a
  primitive-payload helper with a `String`-downgradable error can move; anything returning a
  `lodestone-model` type cannot, without changing `lodestone-model` itself.
- The version table's generator hard-errors on jar/`minecraft-data` disagreement by design; do
  not "fix" a future disagreement by picking a source in the generator.

## Configuration

- `vNNN` cargo features on `lodestone-registry` decide which families are compiled in at all;
  none are on by default except `live`, which enables `v26-2`.
- `LODESTONE_REGEN=1` switches any canonicalisation-table generator from assert to write.
- `cargo run -p xtask -- version-table [--check] [--fetch-missing]` regenerates or drift-checks
  the version table; `--fetch-missing` is the only network/disk-heavy path.
- `MAX_PACKET_LEN` (2 MiB), `MAX_DECOMPRESSED_LEN` (8 MiB), `MAX_LENGTH_VARINT_BYTES` (3) in
  `codec.rs` bound frame sizes, matching vanilla's own decoder limits.

## Dependencies

- `lodestone-model` (`VersionAdapter`, `ClientEvent`/`Directive`/`ClientAction`) and
  `lodestone-core` (shared codec helpers, `Reader`/`Writer`), depended on by every family.
- `lodestone-canonical` (pre-Flattening flattening table + bridge, shared by `v1-8`/`v1-9`) and
  each post-Flattening family's own generated per-family table (`v1-14`); `lodestone-data` is a
  **dev**-dependency only for the table generators, never a runtime one, so
  `cargo xtask check-deletable <family>` stays accurate.
- `vendor/minecraft-data` and Mojang's version manifest, cross-check/fallback grade only, for
  the version table.
- `flate2` for zlib decompression in `lodestone-net`.
