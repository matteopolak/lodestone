# Multi-version protocol sharing: eras, ranges, and dispatch

## What it is

The rules for sharing packet definitions across protocol families. `lodestone-protocol-common` owns packet shapes and adapter logic stable across a declared protocol range; a crate under `crates/versions/` owns the era-specific framing, generated identifiers and adapter dispatch for the protocols it serves. Every negotiated protocol still constructs one family adapter, and no family depends on another.

## How it works

`lodestone-registry` maps a negotiated protocol to a `Family`. Most family crates export a `PROTOCOLS` slice and `adapter_for`; the singleton `v26-2` exports a single `PROTOCOL` constant and `adapter()`. The shell reaches families only through registry lookups, and client, server and physics tables are separate (hosting is not implied by a client adapter; the modern host is `v26-3`, protocol 777, reusing `v26-2`'s `ServerProtocol`). A family is named for its era-start release: directory, package and feature names are labels, so always query `VersionAdapter::supports`.

The dependency direction is deliberate: `family crate -> lodestone-protocol-common -> core/model/world/data`, plus the shared crates. A shared crate must not depend on a version crate and one family must not depend on another; `xtask` isolation and deletability checks enforce this.

### Measuring duplication

`cargo xtask protocol-dup` measures the working tree (run it before quoting a number or moving an era boundary): line similarity of same-path files in adjacent families, normalised identity of same-named packet structs, token similarity of the legacy dispatch over 1.8, 1.9 and 1.14, normalised free-function identity, and packet-shape adjacency across target versions. No single measure proves a definition shareable (file similarity hides changed fields, packet identity cannot see a hand-written codec, dispatch similarity is not wire compatibility), so use it to find candidates and verify wire shape with captures.

### Era grouping threshold

Group at **at least 85% adjacent packet-shape identity**, computed from versioned shape data after recursively inlining every named type (a change in a shared nested type changes every packet carrying it). The vendored shape data is a cross-check, not an authority, and does not cover the newest protocol; without authoritative shapes keep eras separate until captures establish the boundary. The threshold selects a candidate era, not blanket reuse: chunk framing, metadata, inventory representation, connection choreography and generated registries can still need a per-protocol branch, while a packet with a proven range may be shared across an era boundary. A range or era decision needs the adjacency measurement, an independent wire fixture and the dispatch-coverage checks.

Shared packets use `Packet::PROTOCOLS` and field-level range attributes for fields present in part of a range; a field whose representation changes needs separate packet types (a range attribute cannot change a type). Keep the decode/encode lift next to the shared definition when range-stable.

Each family owns: generated packet-id and registry tables per protocol; chunk framing, metadata, inventory and connection choreography where they differ in the era; the `VersionAdapter` and its dispatch table; and captures whose expected values originate outside Lodestone. Dispatch must cover every packet id in the negotiated table: each entry is bound to a handler or listed in an explicit ignore table with a reason, and the adapter rejects a handler outside its range, absent from the id table, or leaving an id unclassified (silent packet loss becomes a construction-time error). `cargo xtask connectedness` reports decoded, emitted, encoded and ignored traffic across families.

### Where sharing genuinely breaks

Eras are defined by representation boundaries, not packet names: coordinate encoding, state and item identity, chunk lighting and biome layout, height and section shape, chat authentication, connection configuration, item-component representation. Keep changes crossing one in the era crate even when nearby names match. Generated state mappings translate wire values into canonical state space; a missing mapping must become a counted, logged fallback, never a silent substitution, and connectedness does not prove a decoded state is right (validate with captures or oracle data).

### Cost evidence

Adding a version to an existing era cost 20, 69 and 131 hand-written lines (the 131 included a chunk-framing change, so a line count never replaces a byte fixture). Founding an era costs a full family and grows with newer protocols' own mechanisms: 1.13 5,677 lines, 1.17 6,376, 1.19 6,811, 1.20.6 7,547, 1.21.11 8,278 (`cargo xtask codegen-ratio`). Payoff starts with the second compatible protocol. 1.9 and 1.14 prove adjacent versions can share an era; the two protocols after 1.20.6's first share 204 of 226 packet shapes (90.3%); the 771-774 range has 88.5%, 87.4% and 94.0% adjacency to its implemented endpoint, each a candidate only after its own tables, captures and dispatch classification land. Protocols below 85% on both adjacent comparisons found a new era.

## How to change it

Adding a protocol to an existing era:
1. Obtain authoritative captures and generated tables.
2. Extend the family's `PROTOCOLS` and select its tables in `adapter_for`.
3. Reuse a common definition only after a byte-level or independently decoded comparison proves the range includes the protocol; otherwise add an era-specific one.
4. Classify every table entry as handled or explicitly ignored.
5. Add capture replay and negative controls showing a wrong id table or out-of-range handler fails.

A new era is a crate under `crates/versions/` depending on shared crates only: generated tables, adapter and dispatch coverage first, common packet modules imported by proven range, one module per shared packet to avoid a high-contention file. Do not copy a neighbouring adapter as the strategy; shared codecs, packets, test drivers and adapter state belong in version-free crates when range-stable, but a helper must not introduce a reverse dependency from a shared crate to `lodestone-model` or a family crate. Every range widening needs an external capture or oracle-derived expected value (a round trip through our own codec is not evidence); keep per-protocol fixtures even with a shared replay driver.

After changes run `just check-seam`, the affected family tests, `cargo xtask connectedness`, `cargo xtask check-isolation` and the deletability check, and `just wasm-check` after dependency or `cfg` changes.

## Configuration

`lodestone-registry` features decide which families compile (none by default; the shell's `live` feature enables the supported live family). `LODESTONE_REGEN=1` makes generate-or-assert tests write regenerated oracle tables; table generation and fetching are explicit `xtask` operations.

## Dependencies

`lodestone-core` codecs and `ProtocolRange`, `lodestone-macros` range-aware derives, `lodestone-model` adapter types, `lodestone-world`, `lodestone-data`, `lodestone-registry`, `lodestone-testsupport`, and the `xtask` checks. See [multi-protocol seam](../multi-protocol-seam.md).
