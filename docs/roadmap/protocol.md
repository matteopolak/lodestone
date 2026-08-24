# Protocol, networking and multi-version — roadmap

1:1 protocol parity for 26.2 (protocol 776) in both directions, on both the client *and*
the server side — a vanilla client must be able to connect to our server, and a vanilla
server to our client — plus the multi-version question. All 23 issues below are filed as
sub-issues of epic [#5](https://github.com/matteopolak/lodestone/issues/5) (Tier 4, 13
issues, server-side) or epic [#4](https://github.com/matteopolak/lodestone/issues/4)
(Tier 3, 10 issues, completeness). A handful of pre-existing issues already covered part
of this domain and were commented on rather than duplicated — see
["What was already filed"](#what-was-already-filed) below.

Not covered here (owned by other tracks): mob AI, redstone, block ticks, world
persistence and the rest of the game-simulation content that a completed protocol layer
would carry ([`server-entities.md`](./server-entities.md),
[`server-simulation.md`](./server-simulation.md) if/when filed); the client-side GUI for
any of this (options screens, chat box UI — [`client-rendering.md`](./client-rendering.md),
[#46](https://github.com/matteopolak/lodestone/issues/46)); command *execution*
server-side ([#48](https://github.com/matteopolak/lodestone/issues/48), pre-existing).

## Measured coverage

```
cargo xtask connectedness
```

```
protocol connectedness (denominators from each family play::{clientbound,serverbound} packet_ids.rs):
v47  clientbound decoded 59/74; emits 59/74; decoded-but-stranded 0; serverbound encoded 21/26; examined 59 arm(s); serverbound decode: not applicable (no src/server_protocol.rs — family does not implement ServerProtocol, so it cannot host)
v340  clientbound decoded 62/80; emits 62/80; decoded-but-stranded 0; serverbound encoded 24/33; examined 62 arm(s); serverbound decode: not applicable (no src/server_protocol.rs — family does not implement ServerProtocol, so it cannot host)
v735  clientbound decoded 54/92; emits 54/92; decoded-but-stranded 0; serverbound encoded 25/48; examined 54 arm(s); serverbound decode: not applicable (no src/server_protocol.rs — family does not implement ServerProtocol, so it cannot host)
v770  clientbound decoded 141/141; emits 139/141; decoded-but-stranded 0; serverbound encoded 68/69; examined 141 arm(s); serverbound decoded 66/69, connected 47/69; examined 66 arm(s); decodes-to-Ignored-only 19 [ACCEPT_TELEPORTATION, BLOCK_ENTITY_TAG_QUERY, CHAT_ACK, CLIENT_TICK_END, CONFIGURATION_ACKNOWLEDGED, CUSTOM_CLICK_ACTION, ENTITY_TAG_QUERY, JIGSAW_GENERATE, PLAYER_ABILITIES, PLAYER_LOADED, PONG, RECIPE_BOOK_CHANGE_SETTINGS, RECIPE_BOOK_SEEN_RECIPE, RESOURCE_PACK, SEEN_ADVANCEMENTS, SET_COMMAND_MINECART, SET_JIGSAW_BLOCK, SET_STRUCTURE_BLOCK, SET_TEST_BLOCK]
```

Re-measured 2026-08-23. This leading block is the current snapshot; the dated sections below
retain their original measurements because they describe the state before and after specific
historical changes. Re-run the command before quoting current coverage rather than promoting a
dated subsection's count back into this block.

### `connected` moved to 15/69: `CLIENT_INFORMATION`/`CHUNK_BATCH_RECEIVED` were dead-code decode arms (2026-08-04)

The 13/69 figure above's own decode-arm half turned out to be the exact bug behind a separate,
concurrently-reported chunk-streaming regression: `cargo test -p lodestone-v770 --test
block_edit -- dig_and_place_persist_through_forget_and_reload` timed out waiting for a
forgotten chunk to be re-sent after walking back into it, and `cargo test -p lodestone-v770
--test server_liveness -- real_client_view_follows_player_across_chunk_boundaries` failed the
same way. Root cause: `server_protocol.rs`'s `decode()` match had `CLIENT_INFORMATION` and
`CHUNK_BATCH_RECEIVED` both decode-then-drop to `ServerBound::Ignored` unconditionally — a
leftover from before this crate had any consumer for either. Issue #270 later added
`ServerBound::ClientInformationChanged`/`ChunkBatchAcknowledged` and their `crate::server`
consumers (`ViewTracker::set_view_radius`, the `awaiting_chunk_batch_ack` flow-control gate)
but never came back to update this decode arm, so both variants were **constructed nowhere** —
exactly the kind of gap this doc's own `connected` column exists to catch, and exactly why the
13/69 figure two sections up undercounted by two. Fixed in `30a45aa`; both decode arms now
match on `decode_full::<T>(payload)` and construct the real variant. `connected` reads **15/69**
after the fix, with `CLIENT_INFORMATION`/`CHUNK_BATCH_RECEIVED` no longer in the
`decodes-to-Ignored-only` list:

```
v770  clientbound decoded 113/141; emits 111/141; decoded-but-stranded 0; serverbound encoded 54/69; examined 113 arm(s); serverbound decoded 60/69, connected 15/69; examined 60 arm(s); decodes-to-Ignored-only 45
```

Re-verified against `66c895a` (several unrelated commits later): both tests still pass, and
the full `cargo test -p lodestone-v770 --no-fail-fast` suite (all binaries) stays green. Five
new regression tests in `server_protocol.rs`'s `view_streaming_decode_tests` module chain the
real client encoder/adapter into this module's decoder for both packet ids, so a future decode
arm reverting to `Ignored` fails loudly rather than silently reopening this gap.

### A per-species `SET_ENTITY_DATA` encoder and an `EXPLODE` encoder (2026-08-04)

Not a `connected`-measured gap — that column is clientbound/serverbound **decode**
connectivity; this is server-side **encode**, the opposite direction. `crates/lodestone-server`
had exactly one metadata encoder before this (`encode_air_supply_update`, hardcoded to
`LOCAL_PLAYER_ENTITY_ID` and one `INT` field) and zero `EXPLODE` encoders anywhere, so a
creeper primed and detonated by `MobSim`'s own real AI (issue #213's exposure/damage maths,
`SwellGoal`) was invisible to any client connected to *our* server: no swell animation, no
particle, no sound, even though the client-side decode/render chain for all three was already
complete and validated against real captured vanilla bytes (see
[`entity-rendering.md`](../entity-rendering.md)'s creeper section).

Closed by a general `ServerProtocol::encode_set_entity_data(entity_id, fields:
&[MetadataField])` — replacing "one hardcoded method per field" with one vocabulary type a
future mob's field reaches through the same `EntityStreamer::sync` diff loop
position/rotation already use, no new plumbing — plus `ServerProtocol::encode_explode(centre,
radius)`, fed from a new `MobSim::take_detonations()` drain and an `ExplosionFeed`
(`crate::tick`, the same publish/drain_all idiom `BlockTickFeed` already establishes).
`V770ServerProtocol`'s implementations of both are byte-format-verified against the
`EntityDataIndexOracle` dump (indices 16/18) and Mojang's decompiled
`ClientboundExplodePacket`/`Level.java`/`ServerExplosion.java` respectively, not guessed from
our own decoder. See `crates/protocol/v770/tests/server_creeper_metadata_and_explode.rs` for
the gate: encodes with our own server, decodes with the real, already-live-validated
`V770Adapter`.

Re-measured 2026-08-04, after landing decode arms for issues #262/#264/#266/#268/#270 (this
session). **Use this command, never a hand count** — a hand-derived figure for this exact
domain has been wrong four times in four different ways, and the tool itself has now been
wrong twice (see below), including a third instance caught and fixed in this same pass: two
arms (`SET_CREATIVE_MODE_SLOT`/`SET_BEACON`, #266) called a local helper function as a bare
call, which the classifier follows to check for a `ServerBound` reference — the helpers
return `Option<Option<T>>`, not `ServerBound`, so it reported both arms `UNCLASSIFIED`
(exit code 1) despite their own bodies having a literal, unconditional `ServerBound::Ignored`
the classifier never got to check. Fixed by qualifying both calls as `self::…`, which the
classifier's own `receiver_call` exclusion already treats as "not a delegate" — see `c5a5d9d`.

**`connected` stayed at 13/69 through all five issues.** Every one of the 47 new arms decodes
cleanly and deterministically to `ServerBound::Ignored` — none has an existing `ServerBound`
variant to lift into, and `crates/lodestone-server` is currently owned by a concurrent
random-tick/scheduled-tick-queue agent, so no new variants or consumers could be added in the
same pass. See each issue's own tracker comment for which packets are flagged as the
cheapest real next step (`SET_CREATIVE_MODE_SLOT` writes into the exact slot space #408's
`PlayerInventory` already models; `CLIENT_INFORMATION`/`CLIENT_COMMAND`/`CHUNK_BATCH_RECEIVED`
are #270's).

### Four client-completeness gaps closed (2026-08-04)

A same-day pass closed the four client-side (not server-side) gaps this doc's own
["What was already filed"](#what-was-already-filed) table pointed at:

- **`BUNDLE_DELIMITER` had no decode arm.** Measured first, per the issue's own
  instruction: before any fix, the packet fell through the play-state catch-all to
  `Ok(Vec::new())` — a silent no-op that never mis-framed anything, because every real
  packet is independently length-framed by the transport regardless of bundling. The
  actual gap was atomicity, not framing: vanilla groups the packets between two
  delimiters into one atomic apply, and this client had no equivalent. Now decodes to
  `Directive::BundleDelimiter`, and `lodestone-client`'s driver (`Driver::absorb_bundle`)
  buffers directives between an opening and closing delimiter, releasing them to
  `execute()` as one batch on the close.
- **`update_tags` was never decoded.** Decoded in both Configuration and Play
  (one wire shape, `ClientCommonPacketListener` handles both); the `minecraft:block`
  registry's tags are installed as a process-wide override in
  `lodestone_data::tool::set_block_tag_overrides`, consulted by `block_tag_members` —
  process-wide rather than per-connection because `lodestone-shell`'s collision/mining
  code resolves its `VersionAdapter` once from the compiled family
  (`inferred_version_data`), a different instance than the one that decoded the live
  session's packets.
- **Cookies and transfers were dead ends.** Added `ClientAction::CookieResponse`
  (Login/Configuration/Play, one wire shape) and an in-memory cookie store on the driver
  mirroring vanilla's own `ClientCommonPacketListenerImpl.serverCookies`. `transfer` now
  ends the session with `SessionOutcome::Transferred { host, port, cookies }` rather than
  reaching nothing — short of a silent reconnect, since the driver has no generic way to
  open a new transport from inside itself (native TCP and wasm32 are different
  `ClientBuilder::connect`/`connect_with` entry points).
- **Custom payload/plugin channels had no registry.** Filled the two remaining
  decode gaps (`custom_payload` in Configuration, `custom_query` in Login — the latter
  answered unconditionally with no payload, matching vanilla's own reference client
  exactly), added `ClientAction::SendCustomPayload` for the general case `SendBrand` is
  vanilla's one built-in instance of, and added `lodestone_client::ChannelRegistry` — the
  per-channel handler dispatch layer the issue's own partial-progress note said was still
  missing. Server-side `custom_payload` handling remains entirely absent;
  `lodestone-server` was off-limits this pass.

Measured before this pass: `v770 clientbound decoded 111/141; serverbound encoded 53/69`.
After: `112/141` → `113/141` clientbound decoded (`+2`: `BUNDLE_DELIMITER`, `UPDATE_TAGS` —
the `112` mid-session reading briefly overlapped with unrelated concurrent work, see the
note on the code block above), `54/69` serverbound encoded (`+1`: `COOKIE_RESPONSE`).
`connected` is unaffected (still `13/69`) — none of these four reach
`crates/lodestone-server`, which was off-limits.

`UPDATE_TAGS` needed a small `cargo xtask connectedness` fix to classify at all: it
delegates to a helper returning `Result<(), AdapterError>` (a side effect on a global
table, not a directive to produce), which the classifier's delegate-follow couldn't
match to any recognized outlet, and a followed delegate's own `DecodedButStranded`
verdict is discarded rather than propagated to the caller. Extended the existing
`PROTOCOL_INTERNAL_CLIENTBOUND` allowlist (previously only consulted for
`DecodedButStranded`) to also cover `Unclassified`, and added `UPDATE_TAGS` to it —
same shape as `CHUNK_BATCH_START`, decoded on purpose with nothing to observe at its
own edge.

### Consumer landed, decode arm still off-limits (2026-08-04)

A follow-up pass this same day built real `lodestone-server` consumers for four of the
47 `decodes-to-Ignored-only` arms above, without touching the protocol crate (that file
was explicitly off-limits this pass, owned by a concurrent client-side decode agent):

| packet | `ServerBound` variant | consumer |
|---|---|---|
| `SET_CREATIVE_MODE_SLOT` (#266) | `CreativeModeSlotSet { slot, item }` | `apply_creative_mode_slot_set` → `PlayerInventory::apply_menu_slot_change` (the same table `CONTAINER_CLICK` already uses) |
| `CLIENT_COMMAND` (#270) | `ClientCommand { action }` | `apply_client_command` — `action == 0` (respawn) resets `PlayerVitals` once actually dead and confirms via `encode_set_health`/`encode_air_supply_update`; `action == 2` replies with `WorldAdminState`'s game rules via the existing `encode_game_rule_values` |
| `CLIENT_INFORMATION` (#270) | `ClientInformationChanged { view_distance }` | `ViewTracker::set_view_radius`, clamped to the server's configured cap |
| `CHUNK_BATCH_RECEIVED` (#270) | `ChunkBatchAcknowledged { desired_chunks_per_tick }` | closes the real gap this section's own body used to name: `crate::server` now holds at most one unacknowledged chunk batch in flight (`send_view_update`'s queue), instead of starting a fresh one on every `recenter` regardless of any pending ack |

All four are exercised end-to-end through the real `dispatch_play_packet`/`serve_play`
loop in `crates/lodestone-server/tests/serve_play.rs`, using that file's own hermetic
`FakeProtocol` (own wire format, `ServerBound` constructed directly in its `decode` —
the same pattern the pre-existing `difficulty_change_is_confirmed_back_to_the_connection`
test already used for issue #268). At the time this section was written, what this
**could not** prove was that a real vanilla client's bytes decoded to anything but
`ServerBound::Ignored` for all four, because
`crates/protocol/v770/src/server_protocol.rs`'s own `V770ServerProtocol::decode` arms for
`SET_CREATIVE_MODE_SLOT`, `CLIENT_COMMAND`, `CLIENT_INFORMATION` and
`CHUNK_BATCH_RECEIVED` still discarded the parsed fields with
`let _ = decoded;` and unconditionally returned `Ignored`.

**That has since changed.** All four arms now match on the decoded payload and construct
their real `ServerBound` variant (`CreativeModeSlotSet`, `ClientCommand`,
`ClientInformationChanged`, `ChunkBatchAcknowledged`) instead of discarding it — confirmed
directly against the current source, not carried forward from this section's original
claim. The `CLIENT_INFORMATION`/`CHUNK_BATCH_RECEIVED` half of that fix is dated and
measured in ["`connected` moved to 15/69"](#connected-moved-to-1569-client_informationchunk_batch_received-were-dead-code-decode-arms-2026-08-04)
above; the `SET_CREATIVE_MODE_SLOT`/`CLIENT_COMMAND` half landed later still and is not
otherwise dated in this doc. Re-run `cargo xtask connectedness` before quoting a
`connected` figure from this section — it predates both fixes.

`v47`/`v340`/`v735` were **never measured before today** — `xtask`'s connectedness scanner
had a hard `if family != "v770" { continue; }` while its own header claimed to take
"denominators from each family." That filter is gone; every `vNN` directory under
`crates/protocol/` is scanned, and a family that can't be scanned (missing
`packet_ids.rs`/`adapter.rs`) is now named in a `SKIPPED` section instead of silently
vanishing from the family list. Nothing above is a defect in the legacy families — they're
dormant by design (see [multi-version](#multi-version-what-it-would-cost-and-the-call-this-roadmap-does-not-make)
below) — it's just the first time the number has existed at all.

`cargo xtask check-connected` currently fails on one unrelated finding — `lodestone-nav`
has no workspace dependents outside dev-dependencies — which is a separate,
pre-existing issue in a different domain (autonomous navigation/pathfinding), not
protocol. Not filed here; flagged in the final report instead.

### The measurement everything else in this doc adds to

`53/69` serverbound **encoded** (our client can send it) and the new **serverbound
decoded/connected** figure at the top of this section are two different axes, and
conflating them is exactly how this doc went stale the first time. Encoded means *our
client* can send the packet.
Decoded means *our server* can receive and act on it — measured directly against
`crates/protocol/v770/src/server_protocol.rs`'s `ServerProtocol::decode` (the
`State::Play if packet_id == play::serverbound::…` arms) joined against
`crates/lodestone-server/src/server.rs`'s dispatcher, which is a second, cross-crate hop:
a packet can decode to a real `ServerBound` variant and still be stranded if nothing
outside the protocol crate does anything with that variant. `xtask`'s scanner now
measures both hops and gates on the first (classifier failures) the same way it already
did for clientbound.

**This section was itself wrong twice before landing on an automated number, which is
the whole argument for automating it instead of re-counting by hand.** It originally
said "completely zero" (true when written, stale within a day — issue #268 landed three
decode arms the same day). `CLAUDE.md` separately recorded "5/69 → 8/69" from a manual
count that was also already stale by the time it was read, and even the first automated
figure this correction landed with (11/69) drifted to 13/69 from concurrent work in the
time it took to write this paragraph — see the header measurement above for whichever
number is current now. This is not a defect in the tool; it is exactly why the number
belongs in one place (the `cargo xtask connectedness` header above, re-run on demand)
rather than repeated by hand in prose that immediately starts rotting. **Do not hand-copy
the decoded-packet list into this doc again** — run the command.

Only 9 of 69 `play::serverbound` packets have no decode arm at all as of this writing —
`CHAT_ACK`/`CHAT_COMMAND`/`CHAT_COMMAND_SIGNED`/`CHAT`/`CHAT_SESSION_UPDATE`/
`COMMAND_SUGGESTION`/`COOKIE_RESPONSE`/`DEBUG_SUBSCRIPTION_REQUEST`/
`TEST_INSTANCE_BLOCK_ACTION` — the first six belong to chat/commands (issue #271 and #48,
not this cluster), and the last three are each deliberately deferred with a reason in
their own decode arm's doc comment (no client encoder to cross-check against and no cookie
ever set; a registry-keyed dev-only F3 subscription with no id table here; a nested codec
this crate has no type for). Issues #262/#264/#266/#268/#270 below are now **fully decoded**
(their titles' `(0/…)` counts are stale in the other direction) but still mostly
**unconnected** — 13/69 total, unchanged by this pass, since `lodestone-server` is
off-limits to the protocol crate right now. See each issue's own comment thread for the
per-family decoded/connected split; the per-issue counts are still not wired to the
automated figure, which only measures the protocol-crate half of "connected" (decode →
real `ServerBound` variant) and does not know which issue a given packet id belongs to.

### Decoded-but-stranded: `CHUNK_BATCH_START`, and why it isn't actually a defect

`xtask`'s one flagged island is `CHUNK_BATCH_START`. On inspection
(`crates/protocol/v770/src/adapter/chunk.rs`) the arm calls a real
`self.begin_chunk_batch()` — it starts the rate-timing window that `CHUNK_BATCH_FINISHED`
closes and reports back via `chunk_batch.rs`'s vanilla-matching `ChunkBatchSizeCalculator`.
It emits zero `ClientEvent`s, which is all the heuristic can see, but it is not an
unconsumed island in the harmful sense — nothing needs to be wired to it. Filed as a
correction on [#26](https://github.com/matteopolak/lodestone/issues/26) rather than a new
issue. The **real** chunk-batch gap is one-directional and server-side: the client
computes and sends a real receive-rate reply, but the server (i) never reads it — folded
into [#270](https://github.com/matteopolak/lodestone/issues/270) — and (ii) sends the
entire initial view as one uninterruptible dump rather than pacing it.

The eleven-islands figure the domain brief cited (`ClientboundAnimatePacket`,
`Directive::BeginEncryption`, `TakeItemEntity`/`SET_EQUIPMENT`) is **partially stale**:
see [corrections](#corrections-to-the-briefing-this-roadmap-started-from).

### v47 (1.8.9) / v340 (1.12.2): ten clientbound decode arms landed (2026-08-04)

Picked by what a player would actually notice, not by packet id order — the two
families' "under a quarter decoded" figure at the top of this section was concentrated
in exactly the packets a live session needs continuously, not just at join. Four
packets decode identically on both families (same shape confirmed against
minecraft-data's `pc/1.8` and `pc/1.12.2` `protocol.json`) plus a fifth pair unique to
v340:

- **`update_health`/`respawn` were dead code, not missing.** Both structs already
  existed in `packets/game.rs` on both crates — `UpdateHealth`/`Respawn` — with a
  round-trip test in `tests/join_flow.rs` and **zero references from `handle_play`**.
  This is the exact island shape CLAUDE.md's "nothing consumes this" rule describes:
  green tests, tested only against this crate's own encoder, never decoded from a real
  packet. Both are now wired to `ClientEvent::HealthChanged`/`Respawned`; `respawn` also
  re-arms the adapter's `ChunkShape` (sky-light-or-not) the same way `LOGIN` does, since
  a portal respawn changes dimension before the next `map_chunk` arrives.
- **`entity_status`/`entity_head_rotation`** (hurt/death animation flashes, totem
  particles, and independent head-turn) had no struct at all; both are hand-decoded with
  a `Reader` directly, matching `lodestone-v770`'s own `ENTITY_EVENT`/`ROTATE_HEAD` arms
  byte-for-byte (raw `i32` entity id + raw status byte; VarInt entity id + packed yaw
  byte).
- **v340 only: `block_change`/`multi_block_change`** → `ClientEvent::SectionBlocksChanged`,
  the single biggest "player would notice" gap in either family (block breaks/places from
  other players or the server were invisible before this). This reuses `v340`'s existing
  `canonical::resolve_or_air` id:meta→26.2-state bridge (built against the real 1.13.2
  jar's own flattening fix — see `src/canonical.rs`) rather than inventing a second
  table; `multi_block_change` records can span several of `lodestone-world`'s 16-tall
  sections in one packet (1.12.2 has no chunk sections on the wire, just full-height
  columns), so records are grouped by section before emitting. **Not done for v47**: it
  has no flattening table of its own, and reusing v340's would break each crate's
  documented "deletable by removing this one folder" independence — filed as follow-up
  scope below rather than silently borrowed.

**Evidence provenance, explicitly, per the brief's own warning that this is where the
trap is worst:**

- Every wire shape above came from `vendor/minecraft-data/data/pc/{1.8,1.12.2}/protocol.json`
  — **not** from this crate's own `Encode` derive. The `tests/join_flow.rs` additions and
  the new `crates/protocol/v340/tests/block_updates.rs` hand-assemble raw byte vectors
  from that spec (mirroring `lodestone-v770`'s own `tests/block_updates.rs`), so a
  symmetric encode/decode bug in the derive macro cannot pass silently the way the
  `decode(encode(x))` trap allows.
- **One exception, flagged rather than hidden**: `multi_block_change`'s `horizontalPos`
  nibble order (which nibble is relative X vs. Z) is not given by `protocol.json` — it
  only states the field is a `u8`. This pass took the bit order from the long-stable
  external wire documentation for this exact, decade-old packet shape, **not** from the
  jar or a live capture. A real 1.12.2 live-oracle gate (`scripts/live-oracles/legacy-1.12.sh`,
  which this pass located but did not run — see below) would upgrade this from
  "well-established external doc" to "confirmed against a real server" and is the
  natural next step if this nibble order is ever in doubt.
- No live oracle was run this pass. `scripts/live-oracles/legacy-1.12.sh` exists and
  targets Apple `container`, but this session ran under active memory pressure from
  several concurrent agents (free pages dropped below 40k twice mid-session), and adding
  a JVM container on top of that was judged the wrong trade against the four
  hand-verified byte-level tests already covering the new decode arms. Flagged here as
  unfinished evidence, not silently skipped.
- The entity-metadata index tables the brief warned about (`EntityDataIndexOracle`-style
  hand-count bugs) were **not touched this pass** — no `entity_metadata`/`entity_equipment`
  decode was added, so there is no legacy metadata index to get wrong yet. Equipment in
  particular is blocked on a real gap: `ClientEvent::EntityEquipmentUpdated` needs an
  `ItemStack`, and neither crate has an item-id → `ResourceKey` registry (the same gap
  `ClientAction::SetCreativeModeSlot`'s existing `Unsupported` arm already names on both
  families) — filed as follow-up rather than guessed.

**Measured**, `cargo xtask connectedness` before/after this pass:

```
before: v47  clientbound decoded 17/74; serverbound encoded 17/26
        v340 clientbound decoded 16/80; serverbound encoded 20/33
after:  v47  clientbound decoded 21/74; serverbound encoded 17/26
        v340 clientbound decoded 22/80; serverbound encoded 20/33
```

`serverbound encoded` is unchanged on both — this pass added no new `ClientAction`
encode arms, only clientbound decode. Neither family implements `ServerProtocol`, so
serverbound *decode* stays "not applicable" for both, per the header measurement.

**Follow-up scope, not done here:**

- A v47-native `id:meta`→state table (or a shared version-free bridge both crates use),
  so `block_change`/`multi_block_change` can land on 1.8.9 too.
- An item-id → `ResourceKey` registry, unblocking `entity_equipment` on both families and
  the existing `SetCreativeModeSlot`/`ContainerClick` gaps the interaction tests already
  document as `Unsupported`.
- `entity_metadata` decode on both families (mob-visible state: sheep colour, villager
  profession, health-flash tinting) — deliberately deferred rather than hand-counting a
  legacy metadata index without an oracle dump to check it against, per the brief's own
  warning about that exact failure mode.
- The `legacy-1.12.sh` live-oracle confirmation of `multi_block_change`'s nibble order,
  above.

## What was already filed

These pre-existing issues cover ground this domain brief asked about; commented on each
rather than duplicating:

| issue | covers | this pass added |
|---|---|---|
| [#26](https://github.com/matteopolak/lodestone/issues/26) | remaining clientbound packets | family breakdown, `CHUNK_BATCH_START` correction |
| [#34](https://github.com/matteopolak/lodestone/issues/34) | dimension sky light matches on name | pointed at the root cause, [#288](https://github.com/matteopolak/lodestone/issues/288) |
| [#46](https://github.com/matteopolak/lodestone/issues/46) | client command UX (Brigadier tree) | not touched — audited, still accurate |
| [#48](https://github.com/matteopolak/lodestone/issues/48) | server-side command dispatcher | cross-referenced the serverbound-decode issues it depends on |
| [#10](https://github.com/matteopolak/lodestone/issues/10), [#29](https://github.com/matteopolak/lodestone/issues/29) | `ClientboundAnimatePacket`/`TakeItemEntity`/`SET_EQUIPMENT` islands | not touched — audited, still accurate, already correctly labelled `island` |
| [#63](https://github.com/matteopolak/lodestone/issues/63), [#73](https://github.com/matteopolak/lodestone/issues/73) | account switcher UI, auth composition duplication | out of this domain's scope (UI/refactor, not protocol coverage) — not touched |

## Server-side

Being a server is a different axis, not a further step along the client axis, and it
starts from much further back than the client does. The **client's** login/configuration
handshake, encryption (verified against a real vanilla server), and compression are all
real and working. The **server's** equivalent is, for encryption and compression,
entirely absent. Play-state serverbound decode **used to be** completely zero when this
section was written; as of this pass it is 60/69 decoded (see "Measured coverage" above)
but still mostly unconnected — 13/69 — since decoding a packet and having a real
`ServerBound` variant/`lodestone-server` consumer for it are two different, still-mostly-
unmet bars. Every one of the issues below was found by direct inspection of
`crates/protocol/v770/src/server_protocol.rs` and `crates/lodestone-server/src/`, not by
extrapolation.

| issue | title | note |
|---|---|---|
| [#262](https://github.com/matteopolak/lodestone/issues/262) | Server-side decode: movement and player-state (0/11) | **decoded 11/11, connected 3/11** — remaining 8 need new `ServerBound` variants, `lodestone-server` is off-limits right now |
| [#264](https://github.com/matteopolak/lodestone/issues/264) | Server-side decode: entity actions, combat, interaction (0/9) | **decoded 9/9, connected 3/9** — same blocker |
| [#266](https://github.com/matteopolak/lodestone/issues/266) | Server-side decode: inventory and container (0/16) | **decoded 17/16, connected 3/16** — cross-checked against `docs/container-clicks.md`; `SET_CREATIVE_MODE_SLOT` now has a real, tested `PlayerInventory` consumer (see ["Consumer landed"](#consumer-landed-decode-arm-still-off-limits-2026-08-04) above), still counted `Ignored` by the automated figure until the v770 decode arm itself is edited |
| [#268](https://github.com/matteopolak/lodestone/issues/268) | Server-side decode: world/block-admin (0/13) | **decoded 12/13, connected 3/13** — lowest priority of the five; `TEST_INSTANCE_BLOCK_ACTION` deliberately left undecoded (nested codec this crate has no type for) |
| [#270](https://github.com/matteopolak/lodestone/issues/270) | Server-side decode: connection-lifecycle and system | **decoded 11/13, connected 1/13** — `CLIENT_COMMAND`/`CLIENT_INFORMATION`/`CHUNK_BATCH_RECEIVED` now have real, tested consumers (respawn, view-distance resize, and the actual one-batch-in-flight gate — see ["Consumer landed"](#consumer-landed-decode-arm-still-off-limits-2026-08-04) above), still counted `Ignored` until the v770 decode arms are edited; `COOKIE_RESPONSE`/`DEBUG_SUBSCRIPTION_REQUEST` deliberately left undecoded |
| [#271](https://github.com/matteopolak/lodestone/issues/271) | Server-side chat: no decode, no verification, no secure-profile enforcement | pairs with #283 (client-side signing) |
| [#273](https://github.com/matteopolak/lodestone/issues/273) | Server-side login has no encryption or compression | client-side crypto is proven; this is the mirror |
| [#275](https://github.com/matteopolak/lodestone/issues/275) | Server sends no registries/known-packs/tags during configuration | pairs with #288 (client-side ingestion) — one wire format, ideally |
| [#277](https://github.com/matteopolak/lodestone/issues/277) | Server never answers the Status phase | small, but the first thing a real client does |
| [#279](https://github.com/matteopolak/lodestone/issues/279) | Server never sends a Disconnect packet, in any phase | distinct from #68 (client-side stale-key display) |
| [#280](https://github.com/matteopolak/lodestone/issues/280) | Neither side enforces a keep-alive timeout | both directions, one issue |
| [#281](https://github.com/matteopolak/lodestone/issues/281) | No net/game thread split on the server; shell relay channel unbounded | `chore`, not urgent today |
| [#282](https://github.com/matteopolak/lodestone/issues/282) | No fuzz/property-testing harness for any wire decoder | security concern once #262–270 land and we accept real bytes |

### Suggested order

```
Status (#277) ──► Login: encryption+compression (#273) ──► Configuration: registries/tags (#275)
                                                                       │
                                                                       ▼
        Decode: movement (#262) ──► entity actions (#264) ──► inventory (#266) ──► world-admin (#268)
                                                                       │
                                                                       ▼
                                    connection-lifecycle (#270) ──► chat (#271)
                                                                       │
                              ┌────────────────────────────────────────┼───────────────────┐
                              ▼                                        ▼                   ▼
                     keep-alive/timeout (#280)              thread-split/backpressure (#281)   fuzzing (#282)
                                                                                        Disconnect (#279) — needed early for login failures, do in parallel
```

A stranger's vanilla client cannot even find the server without #277, cannot log in as a
real (non-offline-mode) account without #273, and cannot do anything once in the world
without #262–266. #268, #281, #282 and the keep-alive/disconnect issues are hardening
that matters most once the earlier ones are real and strangers' bytes start arriving.

## Client-side completeness

The client already does far more than the server does — this list is real gaps, not a
symmetric mirror of the server list.

| issue | title | note |
|---|---|---|
| [#283](https://github.com/matteopolak/lodestone/issues/283) | Secure chat signing entirely absent client-side | would get us silently dropped by `enforce-secure-profile` servers |
| [#286](https://github.com/matteopolak/lodestone/issues/286) | `MessageSignatureCache` built, tested, unconsumed | `island` label — cheap, standalone fix |
| [#288](https://github.com/matteopolak/lodestone/issues/288) | Client never ingests `registry_data` | **client half landed** — dimension types + world clocks decoded and consumed; see [`registry-data-ingest.md`](../registry-data-ingest.md). Server half is still #275 |
| [#291](https://github.com/matteopolak/lodestone/issues/291) | Cookies and transfers are dead ends | `transfer` is a player-visible gap (hub/lobby networks) |
| [#294](https://github.com/matteopolak/lodestone/issues/294) | `resource_pack_push`/`pop` only handled in Play, not Configuration | near-direct lift of existing Play-state logic |
| [#296](https://github.com/matteopolak/lodestone/issues/296) | `update_tags` never decoded | invisible against vanilla today because our hardcoded tables happen to match |
| [#299](https://github.com/matteopolak/lodestone/issues/299) | `BUNDLE_DELIMITER` has no decode arm | risk is against a **real vanilla server**, not our own |
| [#301](https://github.com/matteopolak/lodestone/issues/301) | Custom-payload/plugin channels: no registry, no `minecraft:brand` | blocks server-side plugin brand-detection of us |
| [#304](https://github.com/matteopolak/lodestone/issues/304) | 12 serverbound packets we cannot encode at all | mostly creative/admin/debug, low urgency |
| [#306](https://github.com/matteopolak/lodestone/issues/306) | Multi-version: cost of a fifth family, and whether it's worth it | design question, see below |

`registry_data` ingestion (#288) was the highest-leverage single item in this list, and
the **client half is done**: `crates/protocol/v770/src/packets/registry.rs` decodes the
packet, `minecraft:dimension_type` and `minecraft:world_clock` are typed, and three
previously-hardcoded values now come off the wire (column height, `has_skylight`, which
clock is the day clock). It was the confirmed root cause of two already-observed bugs
(#34 and the clock selection — the End really was following the overworld's clock on
plain vanilla). See [`registry-data-ingest.md`](../registry-data-ingest.md).

The **server** half (#275) remains: the wire type there is decode-only. Building it
against that same `RegistryData` struct is still the right move — an `Encode` impl beside
the existing `Decode`, not a second implementation.

## Chat and secure signing

26.2 requires secure chat. The picture split cleanly into "the bookkeeping is real and
wired" versus "the cryptography does not exist at all, in either role":

- `crates/lodestone-game/src/chat_ack.rs`'s `LastSeenTracker` (the acknowledgement-window
  half) is genuinely wired end to end — `lodestone-client`'s driver maintains it and emits
  `ClientAction::ChatAck`, encoded and tested. Not a gap.
- Everything cryptographic is absent: no session keypair, no per-message signature, no
  `chat_session_update`, no `chat_command_signed`; the one public key the client does
  parse (`player_info_update`'s `INITIALIZE_CHAT`) is explicitly discarded rather than
  stored. Filed as #283 (client) and #271 (server — a strictly harder problem, since a
  server must *verify*, not just produce).
- **Concrete, sourced consequence**: per `.cache/mc/26.2/src`'s
  `ServerGamePacketListenerImpl.handleMessageDecodeFailure`, a real server with
  `enforce-secure-profile=true` silently drops (not kicks) every message we send today.
  Servers with it off show our chat as "not secure" but still deliver it.

## Login, configuration and cookies

The client's login phase is essentially complete and independently verified (the RSA/AES
handshake round-tripped against a real vanilla server, per `docs/accounts.md`).
Everything downstream of that has gaps:

- **Server login** has no encryption and no compression at all (#273) — it only ever
  speaks to our own tolerant client today.
- **Cookies** are a dead end in both phases: the client can receive a `cookie_request` but
  has no `cookie_response` encoder anywhere in the tree (#291).
- **`transfer`** decodes into an event and nothing acts on it — a straightforward
  consumer-wiring gap, folded into #291.
- **Configuration-phase resource-pack push** is unhandled — the Play-state version works
  and is well-tested; it just isn't reachable from the phase real servers most commonly
  use it in (#294).
- **`registry_data`** is now ingested by the client (#288, landed) but still not sent by
  the server (#275). Was the single biggest data-driven-content gap in the domain and the
  reason two dimension/world-clock bugs existed; what remains of it client-side is the
  27 registries kept as names-only (damage types, chat types, biomes, …) and the
  dimension type's `attributes` map, which the visual presets still hardcode.
- **`update_tags`** is never decoded (#296) — invisible today only because the hardcoded
  fallback tables happen to agree with vanilla defaults.

## Networking robustness

The framing layer itself is in good shape: `crates/lodestone-net/src/codec.rs` bounds the
length-prefix VarInt and caps both compressed (2 MiB) and decompressed (8 MiB) sizes
*before* allocating, and the packet-decode path has zero `.unwrap()`/`.expect()` calls —
an unmatched packet id falls through to a safe no-op rather than erroring or panicking.
That conclusion currently rests entirely on manual code reading, though — there is no
fuzz or property-testing harness anywhere in the tree (#282), so it is a claim someone
made once rather than a gate that runs continuously as new decode paths land.

Backpressure is real but partial: the client library's internal event channel is bounded
and genuinely throttles the socket-read task when a consumer lags, but the shell's own
relay channel downstream of it is an unbounded `std::sync::mpsc`, and the server has no
net/game thread split to have backpressure over at all (#281 — filed as a `chore`, since
neither gap is attacker-facing today).

Keep-alive exists as a mechanism on both sides and is armed on neither: the server never
sends a probe, and the client never sets the read-timeout it already has the plumbing for
(#280). A silent peer is currently held forever by both roles.

`BUNDLE_DELIMITER` (#299) and plugin/custom-payload channels (#301) are the two gaps most
likely to bite specifically when our **client** talks to a **real vanilla server**, since
our own server never generates either case today — a hermetic, self-authored fixture
structurally cannot surface them.

## Multi-version: what it would cost, and the call this roadmap does not make

Filed as [#306](https://github.com/matteopolak/lodestone/issues/306), a design question,
per the domain brief's instruction not to assume the answer. `HANDOFF.md` §1 already
contains the load-bearing analysis — read it before touching this. Reduced to what's
new or worth restating here:

- The reduction from 17 target families to v770-only happened for a structural reason,
  not a scoping preference: **neither adapter dispatch nor wire-shape migration can be
  code-generated.** `xtask new-version` mechanically cloning v340 → v735 produced "a
  1.12.2 client wearing 1.16 packet IDs." Only packet ids and registry tables are the
  cheap, generatable part.
- The measured **irreducible** cost of one family is ~900 hand-written lines (~1 day),
  concentrated in `adapter.rs` (dispatch) and `chunk.rs` (paletted decode/light-split).
- Confirmed today: `v47`/`v340`/`v735` were all last touched at the identical timestamp
  (a mechanical fixture update unrelated to version work), two days before `v770`'s last
  touch — dormant, not actively rotting, receiving zero new work, exactly as
  `CLAUDE.md`'s stated scope implies.
- Confirmed today: there is **no shared packet-definition layer** across families beyond
  the `lodestone-macros` derive codegen and the fixed `VersionAdapter` trait contract.
  Each version hand-writes its own packets and dispatch independently — the *marginal*
  crate has not gotten cheaper in packet-porting terms as more crates were added, only in
  scaffolding/integration terms.
- If resumed: `ClientAction` encode breadth is 16–17/43 on the older families versus
  42/43 on v770 (a 1.8.9 client cannot break a block today), and some of that gap is
  **correct by design** — some actions have no pre-1.9 wire form — so `HANDOFF.md`'s own
  requirement stands: produce the absent-by-design-vs-not-done-yet table *before*
  resuming, because a table where those look identical is exactly how v735 previously
  shipped registered-but-unreviewed.

No recommendation is made here. The mechanical facts say a fifth family is cheap in
isolation (~1 day) and the *integration/review* discipline around it (the
`SHAPE_REVIEW.toml` gate exists specifically because that part goes wrong silently) is
the part this project has already been burned by once.

## Corrections to the briefing this roadmap started from

- **`Directive::BeginEncryption` having "no handler at all" is stale.** It was true when
  written but issue #65 ("Wire lodestone-auth into the join flow so online-mode servers
  work") closed that gap — `Driver::execute` (`crates/lodestone-client/src/driver.rs`) has a real
  `BeginEncryption` arm today, exercised by `crates/lodestone-client/tests/online_mode_handshake.rs`.
  The client-side crypto path is verified end to end against a real vanilla server per
  `docs/accounts.md`; the actual remaining gap in this area is entirely server-side
  (#273) and chat-signing-specific (#283/#271), not the join-flow handler the brief named.
- **`CHUNK_BATCH_START`'s "decoded-but-stranded" status is a benign mechanical fact, not
  a defect** — see above. It has a real side effect (starting the rate-timing window);
  it just emits no `ClientEvent`, which is all the connectedness heuristic measures.
- **The eleven-confirmed-islands figure is a whole-repo count, not a protocol-domain
  one** — of the three protocol-specific examples the brief named, `ClientboundAnimatePacket`
  and `TakeItemEntity`/`SET_EQUIPMENT` were already filed as their own issues (#10, #29,
  both already correctly labelled `island`) before this pass started; only
  `Directive::BeginEncryption` (above) was actually stale.
- **The server side is not zero-effort greenfield** — `lodestone-server` has a real
  `serve_connection` loop, a real entity-diffing streamer, and a real `TcpListener::bind`
  path proven to serve *our own* client end to end (`crates/lodestone-server/tests/`).
  "A fresh axis" (the brief's framing) is accurate for what a real vanilla client needs
  (encryption, compression, registries, and — starkly — any play-state decode at all);
  it undersells what already exists for serving our own client, which is the right
  foundation to extend rather than a build-from-nothing.
