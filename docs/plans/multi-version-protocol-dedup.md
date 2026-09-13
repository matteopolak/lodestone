# Multi-version protocol sharing: eras, ranges, and dispatch

## What it is

This document describes the version-independent protocol layer and the rules for sharing packet
definitions across protocol families. `lodestone-protocol-common` owns packet shapes and adapter
logic that are stable across a declared protocol range; a crate under `crates/versions/` owns the
wire-era-specific framing, generated identifiers, and adapter dispatch for the protocols it serves.

The arrangement reduces copied packet code without weakening the version seam: every negotiated
protocol still constructs one family adapter, and no family depends on another family.

## How it works

`lodestone-registry` maps a negotiated protocol to a `Family`. Most family crates export a
`PROTOCOLS` slice and `adapter_for` constructor; the singleton `v26-2` family instead exports its
single `PROTOCOL` constant and `adapter()` constructor. Its registry entry supplies a one-element
coverage slice and deliberately discards the already-matched protocol argument. The shell accesses
families only through registry lookup functions. The registry also has separate client, server, and
physics tables; hosting support is not implied by a client adapter. `v26-2` is the only family that
implements `ServerProtocol`.

A family is named for the era-start release it covers. Directory names, package names, and feature
names are labels, not protocol numbers. Always query `VersionAdapter::supports`; for a
multi-protocol family, its `PROTOCOLS` declaration is also the source of coverage, while `v26-2`
uses its single `PROTOCOL` constant.

The dependency direction is deliberate:

```text
family crate -> lodestone-protocol-common -> core/model/world/data
       \----------------------------------> shared crates
```

Version crates may depend on version-free shared crates. A shared crate must not depend on a version
crate, and one family must not depend on another. `xtask` isolation and deletability checks enforce
these constraints.

## Duplication, four ways

`cargo xtask protocol-dup` is the re-runnable measurement instrument for deciding what may be
shared. Run it from the workspace root before quoting a number or changing an era boundary; its
output describes the working tree, not a fixed baseline. It emits five sections: four duplication
measures and packet-shape adjacency:

- line-level similarity for same-relative-path Rust files in adjacent families, including generated
  files and tests;
- normalized identity for same-named packet structs and enums under `src/packets/`, excluding test
  modules;
- token similarity for the legacy-only dispatch report over 1.8, 1.9, and 1.14; 26.2 is excluded
  because its adapter has a different module shape. The report first finds `handle_play` chain arms
  and falls back to table-handler discovery only when that family no longer has those arms; and
- normalized free-function identity and near-duplicate source and test shares, excluding generated
  source and test-only modules where appropriate; and
- packet-shape adjacency across the covered target versions.

No one measurement proves that a definition is shareable. File similarity can hide a changed field,
packet identity cannot see a hand-written codec, dispatch similarity is not wire compatibility, and
function identity cannot establish a packet's byte layout. Use the report to find candidates, then
verify their wire shape and capture replay independently.

### Era grouping threshold

The grouping threshold is **at least 85% adjacent packet-shape identity**. Compute that figure from
the versioned packet-shape data after recursively inlining every referenced named type; a change in a
shared nested type therefore changes every packet that carries it. The vendored shape data is a
cross-check, not an authority, and does not cover the newest protocol. When a comparison lacks
authoritative shape data, retain separate eras until captures or generated reports establish the
boundary.

The threshold selects a candidate era, not a blanket permission to reuse every packet. Chunk framing,
metadata, inventory representation, connection choreography, and generated registries can still
require a per-protocol branch inside a grouped era. Conversely, a packet with a proven range may be
shared across an era boundary. A new range or era decision therefore needs the adjacency measurement,
an independent wire fixture, and the normal dispatch-coverage checks.

Shared packet definitions use `Packet::PROTOCOLS` and field-level range attributes where a field is
present only for part of a compatible range. A field whose representation changes uses separate
packet types or an era-specific packet; a range attribute cannot safely change a field's type. Keep
the packet's decode or encode lift next to the shared definition when the translation is also
range-stable.

Each family owns the parts that cannot be shared without hiding a wire difference:

- generated packet-id and registry tables for every supported protocol;
- chunk framing, metadata layout, inventory representation, and connection choreography where they
  differ within the era;
- the `VersionAdapter` implementation and a dispatch table for the negotiated protocol;
- captures and tests whose expected values originate outside Lodestone.

Dispatch must cover every packet id in the negotiated protocol table. An entry is either bound to a
handler or listed in an explicit ignore table with a reason. The adapter must reject a handler that
is outside its declared range, absent from the identifier table, or leaves an identifier unclassified.
This converts silent packet loss into a construction-time error. `cargo xtask connectedness` remains
the cross-family report for decoded, emitted, encoded, and intentionally ignored traffic.

### Where sharing genuinely breaks

Wire eras are defined by representation boundaries, not by superficial packet names. The usual
boundaries include coordinate encoding, state and item identity, chunk lighting and biome layout,
height and section shape, chat authentication, connection configuration, and item-component
representation. Keep changes that cross one of these boundaries in the era crate even when nearby
packet names match.

Generated state mappings translate a family wire value into the canonical state space. A missing
mapping must become a counted and logged fallback, never a silent substitution. Connectedness proves
that a packet reaches a consumer; it does not prove that the decoded state value is correct. Validate
state mappings with captures or generated oracle data.

### Cost evidence and candidate boundaries

Era founding and adding a version to an existing era are different costs. The
measured marginal additions were 20, 69, and 131 hand-written lines; use
20--131 lines as the observed in-era range, not a promise that a new wire
boundary is cheap. The 131-line case included a chunk-framing change, which is
why a line-count threshold must never replace an independent byte fixture.

Fresh `cargo xtask codegen-ratio` source counts make the founding trend clear:

| founding era | hand-written lines |
|---|---:|
| 1.13 | 5,677 |
| 1.17 | 6,376 |
| 1.19 | 6,811 |
| 1.20.6 | 7,547 |
| 1.21.11 | 8,278 |

The trend is a sizing signal, not evidence of accidental duplication: newer
eras carry more protocol-specific mechanisms. Budget a new era as a full
family, then add the cost of its distinct chunk, registry, item, metadata, and
connection behavior. Its payoff begins only with the second compatible
protocol.

The measured candidates illustrate the distinction. The established 1.9 and
1.14 families prove that adjacent versions can stay within one era. The 1.20.6
family has a measured lower boundary below its first protocol, while the next
two protocols share 204 of 226 packet shapes (90.3%), above the 85% threshold,
making them in-era candidates rather than new-family candidates. For the later
family, the measured 771--774 range has 88.5%, 87.4%, and 94.0% adjacency to
its implemented endpoint; each is a candidate only after its own identifier
tables, captures, and dispatch classification land. Protocols below 85% on both
adjacent comparisons found a new era; do not force them into a neighbouring
crate for scaffolding convenience.

## How to change it

To add a protocol to an existing era:

1. Obtain authoritative captures and generated tables for that protocol.
2. Extend the family's `PROTOCOLS` and select its tables in `adapter_for`.
3. Reuse a common definition only after a byte-level or independently decoded comparison proves that
   its range includes the new protocol; otherwise add an era-specific definition.
4. Classify every protocol-table entry as handled or explicitly ignored.
5. Add capture replay and negative controls that demonstrate a wrong identifier table or out-of-range
   handler fails.

To create a new era, create a version crate under `crates/versions/` that depends on shared crates
only. Start with generated tables, an adapter, and dispatch coverage; import common packet modules
by their proven ranges. Keep each shared packet in its own module so parallel work does not create a
single high-contention source file.

Do not copy a neighboring adapter as the primary implementation strategy. Shared codecs, packet
definitions, test drivers, and adapter state belong in version-free crates when their behavior and
dependencies are range-stable. Do not move a helper into a shared crate if its success value would
introduce a reverse dependency from a shared crate to `lodestone-model` or a family crate.

Every range widening requires an external capture or oracle-derived expected value for the newly
included protocol. A round trip through Lodestone's own encoder and decoder is not evidence that the
range is correct. Keep per-protocol fixtures even when the replay driver is shared.

Use `just check-seam` after changing family dependencies or registry wiring. Also run the affected
family tests, `cargo xtask connectedness`, `cargo xtask check-isolation`, and the relevant
deletability check. Run `just wasm-check` after dependency or conditional-compilation changes.

## Configuration

`lodestone-registry` features determine which families compile. No client family is enabled by
default; the shell's `live` feature enables the supported live family. `LODESTONE_REGEN=1` switches
generate-or-assert tests to write regenerated oracle tables. Version-table generation and network
fetching are explicit `xtask` operations; routine checks should use committed inputs and captures.

## Dependencies

The design depends on `lodestone-core` codecs and `ProtocolRange`, `lodestone-macros` range-aware
derives, `lodestone-model` adapter types, `lodestone-world` containers, `lodestone-data` generated
inputs, `lodestone-registry`, `lodestone-testsupport`, and `xtask` isolation, connectedness, and
deletability checks. The registry and canonicalisation contracts are detailed in
[`multi-protocol-seam.md`](../multi-protocol-seam.md).
