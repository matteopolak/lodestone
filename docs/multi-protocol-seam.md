# The multi-protocol seam: version crates, canonicalisation, and framing

## What it is

How `crates/versions/<family>` crates are structured and named, how the registry resolves a negotiated protocol number to an adapter, how each older family translates its wire block states into the canonical 26.x block-state space, the table of protocol and data-version numbers per release, and the packet framing every family shares under `lodestone-net`.

## How it works

### Family shape and dispatch

Each family crate implements `lodestone_model::VersionAdapter`, the client-direction seam (`begin_login`, inbound decode into `ClientEvent`/`Directive`, `ClientAction` encoding). Hosted rows also implement `ServerProtocol` (`lodestone-server`'s server-direction seam). The registry keeps a separate host-protocol list, since joining and hosting coverage differ. Currently hosted: protocols 5, 47, 110/210/316/340, 404, 498/578/754, 756/758, 762, 766, 774, 776; a family may stay join-only for any other protocol it understands.

`lodestone-registry`'s `FAMILIES` table holds a `label`, `protocols: &'static [i32]` and `make: fn(i32) -> Box<dyn VersionAdapter>`. `SERVER_FAMILIES` holds host coverage, an explicit worldgen scope and the server constructor. `protocols` points at the family crate's own `PROTOCOLS` const, so the registry cannot drift from `VersionAdapter::supports`. Resolution matches `protocols` without allocating and constructs one adapter. `v26-2` is asymmetric: no `PROTOCOLS`/`adapter_for`, coverage spelled `&[lodestone_v26_2::PROTOCOL]`, protocol argument ignored (single protocol, 776). Its server adapter reports `WorldgenScope::V26_2`, the only embedded terrain bundle; every other hosted family reports `WorldgenScope::None`, and the checked chunk-source path turns that into a visible refusal, so an older protocol never gets 26.2 terrain silently. `lodestone_registry::worldgen_scope_for_protocol` exposes it for diagnostics.

`crates/lodestone-registry/tests/worldgen_scope_matrix.rs` spells every protocol row per registry feature, checks registry and boxed server protocol agree, and passes the scope to `lodestone_server::overworld_chunk_source_checked`: legacy rows must return `WorldgenScopeMismatch`, 776 must construct the source, and `--no-default-features` must resolve no hosted rows.

`lodestone-core` shares `encode_body`, `decode_body`, `decode_body_exact` and `unpack_degrees` (no version behaviour; their error type downgrades to `String`). Helpers returning `lodestone-model` types (`send`, `json_reason_text`, `game_mode`) stay duplicated per family, because `lodestone-model` already depends on `lodestone-core`.

### Hosting seam layout

`crates/lodestone-server/src/protocol.rs` is the public façade re-exporting three private modules: `protocol/session.rs` (version-free snapshots and records), `protocol/packets.rs` (client-to-server packet and server directive vocabulary) and `protocol/codec.rs` (`ServerProtocol` and chunk-encoding traits). Imports are unchanged and the split adds no family dependency.

### Naming

Package and feature names (`lodestone-v1-8`, feature `v1-8`, and `v1-9`, `v1-14`, `v26-2`, ...) name the era-start Minecraft version, not a protocol number. A crate spans a wire era (`v1-9` serves 110, 210, 316, 340; see [1.9 era](protocol-1-9-era.md)). The folder (`crates/versions/1.14`) is the same version dotted, but a third thing: `crates/versions/1.14` is not protocol 754's own name any more than `26.2` is 776. Never derive a protocol or its coverage from a folder or package name; ask `VersionAdapter::supports`. A new family names folder (dotted) and package/feature (dashed) after its era-start version and confirms coverage through `supports`.

### Canonicalisation: wire ids into the canonical block-state space

`v26-2` decodes into the canonical space directly. Every other family needs translation, or worlds mesh and collide as wrong blocks with a green suite, because `PalettedContainer` is version-free and accepts any `u32`.

- **`v1-8` (pre-Flattening)**: the wire carries `(blockId << 4) | meta`; `decode_column` passes each cell through `lodestone-canonical`'s `canonical::resolve_composite_or_air` per cell (no palette).
- **`v1-9` (1.9.4 to 1.12.2, pre-Flattening)**: the same `id:meta`, resolved through a generated flattening table (`lodestone-canonical`'s `flattening::lookup`, built and verified against the 1.13.2 server jar's own data-fixer conversion) and a bridging pass (`canonical::bridge`) that renames names and properties the 1.13.2 table spells in a superseded way (`mob_spawner` to `spawner`, `sign` to `oak_sign`, leaves' `decayable`/`check_decay` to `persistent`/`distance`) and fills properties 26.x added (`waterlogged` defaults `false`). The table has four outcomes: `Resolved`, `NoTableEntry` (a pair never assigned), `RequiresAdditionalContext` (identity depends on tile-entity data not decoded: flower pots, skulls, double-plant upper halves) and `OutOfBounds`. Every non-`Resolved` outcome becomes a counted, logged air. Resolution is per palette entry, or per cell for direct/global palettes.
- **`v1-14` (1.16.5, protocol 754, post-Flattening)**: the wire carries a flat id, but in 1.16.5's own space, and thousands of blocks were added since. The whole mapping is baked into `lodestone_v1_14::generated_canonical::STATE_TO_CANONICAL`, built from two jar-derived state dumps (1.16.5's `--reports` and 26.2's) with a small rename table and two property fallbacks (`waterlogged=false`, `powered=false`).

`v1-21-11` uses the same representation but shards its 29,671-entry source: the root `crates/versions/1.21.11/src/generated/canonical.rs` keeps one flat `STATE_TO_CANONICAL` and includes 1,024-entry `src/generated/canonical/state_to_canonical_*.in` files (shard `N` covers `N * 1024 .. min((N + 1) * 1024, SOURCE_STATE_COUNT)`). The ignored generator renders root and shards from one mapping, compares the whole expected filename set and bytes before writing, and publishes each file through a temporary sibling, so a missing, extra or stale shard fails the drift guard. Lookup is still one array index.

All fallbacks share one shape: an unresolvable value becomes air, counted on a `FallbackTally` and logged once per column. `cargo xtask connectedness` cannot see this class (it asks whether a packet reaches anything, not what value flows); only a jar-derived oracle (a captured server section, or a live server via RCON `/setblock` plus `/testforblock`) can.

### Version table

`crates/lodestone-registry/src/version_table.rs` (data in `generated/version_table.rs`) records each targeted version's protocol number, save `DataVersion` and release date. `cargo run -p xtask -- version-table` derives rows from Mojang's version manifest (date and server jar URL), the jar's own `version.json` (authoritative when present) and `vendor/minecraft-data`'s `protocolVersions.json` (fallback, cross-check grade only). Where jar and minecraft-data both exist they must agree exactly or the tool hard-errors. The `version.json` boundary is 1.13.2 (none) to 1.14.4 (present); from 1.14.4 on both agree.

### Packet framing

`Codec` (`crates/lodestone-net/src/codec.rs`) and `Connection::read_packet` produce `(packet_id, fields)`. Uncompressed: `[VarInt length][packet id][fields]`. After `login_compression`: `[VarInt frame length][VarInt uncompressed length][data]`, with `0` meaning `data` is uncompressed. A one-byte `0x00` frame is legal and declares zero bytes, no packet id; the reference pipeline drops it silently, and this codec once hit `UnexpectedEof` and ended the session. `read_packet` now skips an empty body; `read_packet_raw` returns it unmodified for callers to decide.

## How to change it

- Adding a protocol to a family is one line in its `PROTOCOLS`; the registry borrows the slice.
- A grouped family must store the negotiated protocol and branch on it in `adapter_for` to pick the packet-id table; copying a single-protocol body (which only `debug_assert!`s membership) compiles, passes and serves one era's ids to all.
- `Family::protocols` must point at the family's `PROTOCOLS`, never restate numbers.
- `just check-seam` (`cargo check -p lodestone-shell --no-default-features`) is this seam's health check.
- Generated canonical tables (`generated/flattening.rs`, `generated/canonical.rs` under `v1-9` and `v1-14`) are never hand-edited: regenerate with `LODESTONE_REGEN=1 cargo test -p <crate> --test <name> -- --ignored --nocapture` after a jar or registry change, then rerun the exhaustive drift guard (zero unmapped slots).
- A new `lodestone-core` codec helper moves only if its return type is primitive-payload with a `String`-downgradable error.
- Do not resolve a version-table jar/minecraft-data disagreement by picking a source in the generator.

## Configuration

- `vNNN` features on `lodestone-registry` choose compiled families; only `live` (which enables `v26-2`) is on by default.
- `LODESTONE_REGEN=1` switches a canonicalisation generator from assert to write (the `v1-21-11` one rewrites root and shards together; without it, missing, extra or stale shards are rejected).
- `cargo run -p xtask -- version-table [--check] [--fetch-missing]` (`--fetch-missing` is the only network-heavy path).
- `MAX_PACKET_LEN` (2 MiB), `MAX_DECOMPRESSED_LEN` (8 MiB), `MAX_LENGTH_VARINT_BYTES` (3) in `codec.rs`, matching the reference decoder's limits.

## Dependencies

- `lodestone-model` and `lodestone-core` for every family.
- `lodestone-canonical` (flattening table and bridge for `v1-8`/`v1-9`) and each post-Flattening family's own generated table; `lodestone-data` is a dev-dependency of generators only, so `cargo xtask check-deletable <family>` stays accurate.
- `vendor/minecraft-data` and Mojang's manifest (version table only), and `flate2` in `lodestone-net`.
