# Selected protocol release layouts

## What it is

The 26.2 compatibility core has selected wire layouts for protocols 776 and
777. Release-specific bodies share one parser with their production adapter
callers; the 26.3 crate reexports those bodies instead of duplicating a client.

## How it works

`ProtocolDialect` selects packet tables, fixed-registry callbacks and
`GameDataVersion` before packet-body decoding. State and item IDs are translated
into the append-only canonical union, not cast into the previous release's ID
space. Chunk decoding consumes the selected 15- or 16-bit wire palette and
maps its owned container into the canonical 16-bit representation. Indirect
palettes translate each palette entry once; direct palettes translate cells.

`packets::release_layout` contains changed movement, sign, particle-tail and
action bodies. Timed movements retain every waypoint and duration through
`EntityMovedAlongPath`. Particle bursts retain independent speed components
and their distribution. Explosion decoding consumes its complete weighted
particle list before the release-specific sound-presence flag. Weighted debris
presentation and living-effect particle presentation are separate consumer
boundaries; consuming those options is not a claim that they are rendered.

Nested items, particle options and appearance holders use the connection's
borrowed `StackCodecContext`. Dynamic appearance and painting names resolve in
the received registry order, including server-defined names. Explicit legacy
metadata wrappers retain their 776 tables; production metadata never installs
a process-wide registry override. A component whose body is not modelled ends
the unframed item patch and its surrounding metadata list at the known partial
boundary. Resolvable-profile metadata remains explicitly unsupported.

Login and respawn use the actual protocol `Ctx` for both decoding and encoding.
Protocol 777 uses a VarInt current mode and an optional VarInt previous mode:
`0` means absent and `n + 1` means mode `n`. Protocol 776 retains its byte pair.

New packet names are selected before translation into the compatibility core's
packet IDs. Swing animation retains hand, motion kind and duration in
`EntitySwingAnimation`; ECS ingestion, animation ticking and draw extraction
share those fields without a second shell event channel. Public Play admission
still precedes this dispatcher and remains closed for an unreviewed dialect.

Post-effects replace an ordered resource-defined effect list, including an
empty-list reset. The current overlay pass is not that effect pipeline.
Transient blocks are render-only visuals keyed by position: replacement at the
same position, one-second lifetime from render insertion, and early removal
when the owning section mesh completes a compile started no earlier than receipt.
They neither write terrain nor create falling entities. Both bodies decode
strictly, but return explicit unsupported-consumer errors until those
presentation paths exist. Transient state IDs are translated before that gate;
an unknown wire state never becomes air.

## How to change it

Review released read/write instructions or stream composition before changing
a body. Constructor order is not wire order. Add known-answer bytes derived
from a capture or independent arithmetic, plus inputs that distinguish the old
and selected layouts. The release controls live beside the shared production
readers and in `crates/versions/26.3/tests/wire_bodies.rs`.
`adapter::release_dispatch` controls distinguish selected names from colliding
776 IDs, exercise strict payload exhaustion and preserve the public Play gate.
Swing presentation controls live in the shell's entity extraction tests.
The three new packet-body fixtures record released artifact and reviewed
class-byte digests, with bytes constructed by independent VarInt, UTF-8 and
packed-coordinate arithmetic. `tools/verify_wire_controls.py` checks those
bytes and executes discriminating negative controls. Its optional `--jar` and
`--packet-report` inputs authenticate the cached evidence corpus and selected
IDs without a JVM; they do not turn arithmetic fixtures into live captures.

`crates/versions/26.3/tools/class_contract.py` inspects cached class files with
Python's standard library, without a JVM. Pass a jar path and a slash-separated
class name without its file extension; `--method REGEX` narrows output and
`--concise` selects calls and constants. `--compare BASE_JAR --prefix PREFIX`
compares instruction projections across jars. Keep extracted external
identifiers and full disassembly in private cache artifacts, not tracked docs.
This inspector is a review aid, not a proof that every changed instruction
changes the wire or that an unchanged projection proves gameplay equivalence.

## Configuration

Layouts are selected by the connection dialect; there are no environment
variables. `ChunkShape` supplies dimension framing independently of the wire
release. An unreviewed Play dialect remains gated even if its packet-ID report
and standalone body controls are available.

## Dependencies

The compatibility core uses `lodestone-core` readers and writers,
`lodestone-model` events, `lodestone-data` release translations and
`lodestone-world` palette storage. The inspector depends only on Python 3.
Released jars and generated reports are development references, not runtime
dependencies. Root-coordinated Cargo checks and live acceptance are distinct
from independent fixture construction.
