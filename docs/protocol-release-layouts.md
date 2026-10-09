# Selected protocol release layouts

## What it is

The 26.2 compatibility core carries selected wire layouts for protocols 776 and 777. Release-specific bodies share one parser with their production adapter callers; the 26.3 crate reexports them rather than duplicating a client.

## How it works

- `ProtocolDialect` selects packet tables, fixed-registry callbacks and `GameDataVersion` before body decoding. State and item IDs are translated into the append-only canonical union, never cast into the previous release's space. Chunk decoding reads the selected 15- or 16-bit palette into the canonical 16-bit container (indirect palettes translate each entry once, direct palettes each cell).
- `packets::release_layout` holds the changed movement, sign, particle-tail and action bodies. Timed movements keep every waypoint and duration (`EntityMovedAlongPath`); particle bursts keep independent speed components and distribution; explosion decoding consumes the whole weighted particle list before the release's sound-presence flag. Decoding weighted debris and living-effect particle options does not mean they render.
- Nested items, particle options and appearance holders use the connection's borrowed `StackCodecContext`; dynamic appearance and painting names resolve in received registry order, including server-defined names. Explicit legacy metadata wrappers keep their 776 tables and production metadata never installs a process-wide override. An unmodelled component ends the unframed item patch and its metadata list at the known partial boundary. Resolvable-profile metadata stays unsupported.
- Login and respawn use the real protocol `Ctx` both ways. Protocol 777 sends a VarInt current mode and optional VarInt previous mode (`0` absent, `n + 1` mode `n`); 776 keeps its byte pair.
- New packet names are selected before translation to the core's IDs. Swing animation keeps hand, motion kind and duration in `EntitySwingAnimation`, shared by ECS ingestion, animation ticking and draw extraction. Public Play admission still precedes this dispatcher and stays closed for an unreviewed dialect.
- Post-effects replace an ordered resource-defined list (an empty list resets); the current overlay pass is not that pipeline. Transient blocks are render-only visuals keyed by position (replacement at the same position, one-second life from render insertion, early removal when the owning section mesh completes a compile started no earlier than receipt); they write no terrain and make no falling entities. Both bodies decode strictly but return explicit unsupported-consumer errors until the presentation paths exist, and transient state IDs are translated first (an unknown wire state never becomes air).

## How to change it

- Review the released read/write instructions before changing a body; constructor order is not wire order. Add known-answer bytes from a capture or independent arithmetic, with inputs separating old and selected layouts. Controls live beside the shared readers and in `crates/versions/26.3/tests/wire_bodies.rs`; `adapter::release_dispatch` controls cover selected names against colliding 776 IDs, strict payload exhaustion and the public Play gate; swing controls are in the shell's entity extraction tests.
- The three new body fixtures record released artifact and reviewed class-byte digests with bytes built by independent VarInt, UTF-8 and packed-coordinate arithmetic. `tools/verify_wire_controls.py` checks them and runs negative controls; optional `--jar` and `--packet-report` authenticate the cached corpus and selected IDs without a JVM (they do not make arithmetic fixtures live captures).
- `crates/versions/26.3/tools/class_contract.py` inspects cached class files with the Python standard library: pass a jar path and slash-separated class name without extension, `--method REGEX` to narrow, `--concise` for calls and constants, `--compare BASE_JAR --prefix PREFIX` to diff instruction projections. Keep extracted identifiers and disassembly in private cache artifacts. It is a review aid, not proof that a changed instruction changes the wire.

## Configuration

The connection dialect selects layouts; no environment variables. `ChunkShape` supplies dimension framing independently of the release. An unreviewed Play dialect stays gated even with a packet-ID report and standalone body controls.

## Dependencies

`lodestone-core` readers and writers, `lodestone-model` events, `lodestone-data` release translations, `lodestone-world` palette storage; the inspector needs only Python 3. Released jars and reports are development references.
