# v770 clientbound `play` packet coverage, and the 32-packet remainder

## What it is

The measured decode/consumer coverage of protocol 776's `play` clientbound packets in
`crates/protocol/v770`, and a sourced triage of every packet that is still undecoded —
which ones are one small patch away from reaching a real consumer, and which would be
islands (decoded, reaching nothing) if decoded today. This is the record for GitHub
issue #26 ("the remaining clientbound packets").

**Never hand-count this.** Run `cargo run -p xtask -- connectedness`; it parses
`crates/protocol/v770/src/generated/packet_ids.rs` for the denominator and classifies
every `adapter.rs` clientbound match arm as `Emits` (reaches a `Directive::Emit`/`Send`/
world-sink outlet), `DecodedButStranded` (decoded, no outward directive), or
`Unclassified`. The hand-derived version of this number has been wrong four times in
four different ways (`CLAUDE.md`); the tool is the only source of truth.

## Measured coverage

| | before *this* row's session | after |
|---|---|---|
| clientbound decoded | 108/141 | **109/141** |
| clientbound emits (reaches a consumer) | 107/141 | **108/141** |
| decoded-but-stranded | 1 (`CHUNK_BATCH_START`) | 1 (`CHUNK_BATCH_START`, unchanged) |
| serverbound encoded | 53/69 | 53/69 (out of scope this session — issue is about clientbound) |

One packet landed: `move_minecart_along_track`. See below for why it was the only one
of the 32 gap packets that could be decoded into a **real, already-live consumer**
without editing a crate outside `crates/protocol/v770`.

### A second session's addition: `chunks_biomes`

`cargo run -p xtask -- connectedness` measured **110/141** decoded at the start of the
biome-climate-lane session (#25/#26) that landed `chunks_biomes` — one higher than the
"109" row above, from unrelated packet work landing between sessions; per this doc's own
rule, that is the tool's number, not a hand count. `chunks_biomes` moves it to **111/141**
decoded / **111/141** emits (it both decodes and reaches `World::merge_biomes` +
`ClientEvent::ChunkLoaded` in the same arm, so it never has an "decoded but not yet
emitting" interim state the way a few other rows in this table did). **Not re-confirmed
by the tool after landing**: `cargo xtask connectedness` panics on the current tree —
`xtask/src/lib.rs:4021`, a byte-index-not-a-char-boundary slice of a doc comment in
`crates/lodestone-server/src/server.rs` containing an em dash — reproduced twice,
unrelated to this session's files (`xtask/` and `lodestone-server` are both outside this
session's ownership; flagged as a follow-up rather than fixed here). The 111/141 figures
above are computed by hand from the same denominator this doc already cites and should
be re-verified with the tool once that panic is fixed.

## Why the gap is 32, not smaller: the easy tier is already gone

`crates/protocol/v770/tests/clientbound_backlog.rs` documents an earlier pass that
landed 17 packets (`player_combat_enter`, `transfer`, `resource_pack_push`, `mount_screen_open`,
etc.) — every one of them decoded into a `ClientEvent` variant that **already existed**
in `crates/lodestone-model/src/event.rs`, just unconstructed. Reading the full
`ClientEvent` enum today (92 variants, `event.rs:857`–`1733`) against the 32 packets
still undecoded finds **zero** matches: no packet in the current gap has a
pre-existing, unconstructed `ClientEvent` variant waiting for it. This codebase adds a
`ClientEvent` variant, its adapter producer, and its ECS/shell consumer as one atomic
unit — so what's left is, almost by construction, the tier that needs a new consumer
type in a crate this task does not own (`lodestone-model`, `lodestone-ecs`,
`lodestone-shell`, …; see `CLAUDE.md`'s "Files you own" list for this issue).

## Triage table

Ordered by tier. "Consumer" cites the file:line evidence that something downstream is
already live and would react, or notes that nothing is.

### Landed

| packet | what it is | consumer | notes |
|---|---|---|---|
| `MOVE_MINECART_ALONG_TRACK` | `ClientboundMoveMinecartPacket` — the *sole* movement channel `NewMinecartBehavior` uses once a minecart exists (vanilla stops sending ordinary `move_entity_*` for cart entities). A `List<MinecartStep>` of `(position, movement, yRot, xRot, weight)` for smooth spline interpolation across the tick window. | `ClientEvent::EntityMoved`/`EntityVelocity`, ingested by the same generic entity-interpolation path as every other entity (`crates/lodestone-entity/src/interpolation.rs`, not edited here). | Every field decodes with primitives already in this crate (`Vec3` = 3×f64 BE, `ROTATION_BYTE` = the same signed-byte angle `unpack_degrees` already inverts for `rotate_head`). The only approximation: this adapter has no multi-waypoint movement event, so only the **terminal** step's pose is applied as an absolute jump — real movement will look stepped on curved rail rather than eased. Every byte, including interior steps, is still read and validated, so a wire-format drift is still caught. See `crates/protocol/v770/src/adapter.rs`'s `handle_move_minecart_along_track` doc comment and `crates/protocol/v770/tests/move_minecart_along_track.rs`. |
| `CHUNKS_BIOMES` | `ClientboundChunksBiomesPacket` — `List<(ChunkPos, byte[] buffer))>`, where `buffer` is the concatenated per-section biome `PalettedContainer` for an **already-loaded** column (no block data), so the server can push a biome edit (e.g. `/fillbiome`) without a full chunk resend. | `World::merge_biomes`/`WorldSink::merge_biomes` (`crates/lodestone-world/src/world.rs`) writes the decoded sections straight into the loaded column, then `ClientEvent::ChunkLoaded` (reused, not a new variant) signals the remesh — exactly the pattern this table's own patch spec below described, before it was written. | Landed via the patch spec this row used to hold (kept below, struck through, for the record): the read side reused `PalettedContainer::decode` with the current dimension's `biome_kind`, one section-loop shorter than `level_chunk_with_light`'s because a biomes-only entry has no block container and no leading counts. The write side is `ChunkSection::set_biomes` (whole-container replace, `crates/lodestone-world/src/section.rs`) + `ChunkColumn::set_biome_section` (allocate-on-demand/elide-on-empty, `crates/lodestone-world/src/column.rs`) + a new `BiomePatch`/`World::merge_biomes` pair mirroring `LightPatch`/`merge_light`'s sparse-per-section, no-op-when-absent shape. A live gate against the `/fillbiome`-triggering real sender (`ChunkMap.resendBiomesForChunks`, `.cache/mc/26.2/src/net/minecraft/server/level/ChunkMap.java:1274-1292`) was not run this session (no oracle command wired to trigger it); coverage here is the hermetic decode/world-write path plus the packet's exact wire shape read from `ClientboundChunksBiomesPacket.java`. See `crates/protocol/v770/tests/chunks_biomes.rs`. ~~**Patch spec:** add `fn set_biome(&mut self, x: i32, y: i32, z: i32, biome_id: u32)` (or a section-batched sibling mirroring `set_blocks`) to `WorldSink` + its `World` impl, following the existing no-op-when-chunk-absent contract `set_block` already uses. Once that lands, `CHUNKS_BIOMES` reuses `ClientEvent::ChunkLoaded` as its dirty-region signal exactly like `LIGHT_UPDATE` does today.~~ (superseded — landed as a whole-container `BiomePatch`, not a per-block mutator; a per-`/fillbiome`-edit column can touch every cell in a section, so a batched write was worth it over reusing the single-cell `set_biome`.) |

### Tier A — spec'd, not landed: needs a small patch in a crate this task doesn't own

| packet | what it is | what's blocking it |
|---|---|---|
| `EXPLODE` | `ClientboundExplodePacket` — explosion center, radius, a block-removal *count* (not a position list — 26.x no longer sends destroyed-block coordinates in this packet), an `Optional<Vec3>` local-player knockback, a generic `ParticleOptions` explosion particle, a `Holder<SoundEvent>` explosion sound, and a trailing `WeightedList<ExplosionParticleInfo>` of debris particles. | `ClientEvent::Particles` (live-consumed at `crates/lodestone-shell/src/net.rs:938`), `ClientEvent::Sound` (`:970`), and `ClientEvent::EntityVelocity` (ingested at `crates/lodestone-ecs/src/ingest.rs:374`) are all real, wired outlets an `EXPLODE` decode could reuse **without any new `ClientEvent` variant** — this one is blocked by risk, not ownership. `explosionSound` reuses the existing `read_sound_holder` helper (`adapter.rs:1231`ff, already used by `SOUND`/`SOUND_ENTITY`) directly. The blocker is `explosionParticle`: it sits *before* `explosionSound` and `blockParticles` in the wire order, so (unlike `LEVEL_PARTICLES`, whose particle option bytes can be swallowed because they're the packet's last field) its per-particle-type option payload must be decoded with a real width, not skipped — and that means a generic `ParticleOptions` decoder covering every particle type with non-empty extra fields (`dust`, `dust_color_transition`, `block`, `block_marker`, `item`, `sculk_charge`, `shriek`, `trail`, `vibration`, `entity_effect`, …; see `.cache/mc/26.2/src/net/minecraft/core/particles/*.java`), which does not exist anywhere in this crate yet. That is real, substantial, separately-verifiable work — not a one-arm addition — and `CLAUDE.md`'s standing warning against a decoder that "looks right and is wrong" argues for not rushing it into this session without a dedicated live-oracle verification pass (e.g. an actual TNT detonation captured on one of the `scripts/live-oracles/` servers). Also needs one small, in-scope addition: the adapter does not currently track the local player's own entity id (needed to route `playerKnockback` through `EntityVelocity`) — trivial to add via the same `Login`-time capture pattern `set_dimension` already uses. |

### Tier B — a dormant serverbound `ClientAction` exists, but the UI it belongs to does not

These have *half* a wiring already (a `ClientAction` variant nobody constructs, or a
`ClientEvent` variant with nothing to navigate), but decoding the clientbound side alone
would not reach a screen — the UI itself is unbuilt. Landing any of these without the
matching UI would recreate the exact island pattern `CLAUDE.md` names nine confirmed
instances of.

| packet | what it is | dormant half |
|---|---|---|
| `COMMANDS` | `ClientboundCommandsPacket` — the full server command graph (for client-side tab-complete/validation), sent at join. | A live chat input box sends `/`-prefixed text via `ClientAction::SendCommand` (`crates/lodestone-shell/src/chat.rs:100`), but there is no command-graph store to validate or complete against. |
| `COMMAND_SUGGESTIONS` | `ClientboundCommandSuggestionsPacket` — the server's tab-completion reply. | `ClientAction::CommandSuggestion` (the request half) exists at `crates/lodestone-model/src/action.rs:299` but nothing constructs it, and there is no suggestion-list UI to hold a reply. |
| `MERCHANT_OFFERS` | `ClientboundMerchantOffersPacket` — a villager/wandering-trader's trade list; opens the trading screen. | `ClientAction::SelectTrade` (`action.rs:219`) exists, unconstructed. No trading screen anywhere in `lodestone-shell`/`lodestone-ecs`. |
| `RECIPE_BOOK_ADD` / `RECIPE_BOOK_REMOVE` / `RECIPE_BOOK_SETTINGS` | Recipe-book contents and open/filter state sync. | `ClientAction::PlaceRecipe`/`RecipeBookSeenRecipe`/`SetRecipeBookSettings` (`action.rs:340`–`365`) exist, unconstructed. No recipe-book UI/state store. |
| `PLACE_GHOST_RECIPE` | `ClientboundPlaceGhostRecipePacket` — ghost-preview of a clicked recipe in an open crafting grid. | Depends on the recipe-book UI above existing first. |
| `UPDATE_RECIPES` | `ClientboundUpdateRecipesPacket` — the full server crafting-recipe registry. | Same recipe-book gap; crafting today is driven purely by container-slot events, not decoded recipe data — no recipe store/solver exists to populate. |
| `UPDATE_ADVANCEMENTS` | `ClientboundUpdateAdvancementsPacket` — advancement definitions plus per-advancement progress. | `ClientEvent::AdvancementsTabSelected` (`event.rs:1605`) and `ClientAction::SeenAdvancements` (`action.rs:293`) already model advancements-screen *navigation* — but there is no advancements data store/screen for that navigation to act on. |
| `DEBUG_BLOCK_VALUE` / `DEBUG_CHUNK_VALUE` / `DEBUG_ENTITY_VALUE` / `DEBUG_EVENT` / `DEBUG_SAMPLE` | 26.x's generalized F3 "debug widget" family: a labeled value tied to a block/chunk/entity, or a periodic sample. | A real F3 overlay exists (`crates/lodestone-shell/src/hud.rs`'s `DebugStats`, keybind at `crates/lodestone-shell/src/keybinds.rs:383`), but `DebugStats` is a closed struct (position/yaw/fps/chunk_count/…) with no open key-value slot these packets could populate. |

### Tier C — genuinely irrelevant to a player-facing client, or a whole new unbuilt subsystem

No existing consumer, no dormant half, and no planned UI. Decoding any of these today
would be a pure island. Listed with the record that was actually read (not a summary of
one) so the "irrelevant" call is falsifiable later if the project's scope changes.

| packet | what it is | why it's skipped |
|---|---|---|
| `AWARD_STATS` | `ClientboundAwardStatsPacket` — the statistics screen's `Object2IntMap<Stat<?>>`. | No stats screen; scoreboard (`ScoreUpdate`/`ObjectiveUpdate`) is a separate, already-live system this does not feed. |
| `CUSTOM_REPORT_DETAILS` | `ClientboundCustomReportDetailsPacket` (`network.protocol.common`) — metadata for the player-report/abuse-report UI. | No reporting UI; this is a vanilla-launcher-integration feature, not gameplay. |
| `GAME_TEST_HIGHLIGHT_POS` / `TEST_INSTANCE_BLOCK_STATUS` | `/test` gametest framework debug markers. | Dev tooling for Mojang's own test harness, not a player-facing feature. |
| `LOW_DISK_SPACE_WARNING` | `ClientboundLowDiskSpaceWarningPacket` — a toast that the server is low on disk space. | No toast/notification system exists in `lodestone-shell`. |
| `TICKING_STATE` / `TICKING_STEP` | Server tick-rate/frozen state and step-while-frozen notifications (`/tick freeze` family). | No tick-rate display or `/tick` UI. |
| `TAG_QUERY` | `ClientboundTagQueryPacket` — reply to a debug NBT query on a block/entity/entity, an F3+creative debug tool. | No matching serverbound query action exists in `ClientAction`, no debug-data UI. |
| `CLEAR_DIALOG` / `SHOW_DIALOG` | `ClientboundClearDialogPacket`/`ClientboundShowDialogPacket` (`network.protocol.common`) — the 1.21.6+ server-defined "dialog" screen system (quest dialogs, custom forms). | Whole new UI subsystem; zero references anywhere outside `protocol`. |
| `WAYPOINT` | `ClientboundTrackedWaypointPacket` (registry name `minecraft:waypoint`, `network.protocol.game`) — locator-bar waypoint tracking (1.21.6+). | Whole new UI subsystem (locator bar); zero references outside `protocol`. |
| `MAP_ITEM_DATA` | `ClientboundMapItemDataPacket` — pixel/icon data for a `filled_map` item. | No map-item rendering pipeline anywhere. |
| `SERVER_LINKS` | `ClientboundServerLinksPacket` (`network.protocol.common`) — server-supplied links shown in the pause/disconnect menu. | `docs/pause-menu.md` confirms a real pause menu exists, but it has no server-links section today (checked: zero mentions of "link" in that doc). Needs both a new `ClientEvent` and a pause-menu UI addition. |
| `CUSTOM_CHAT_COMPLETIONS` | `ClientboundCustomChatCompletionsPacket` — add/remove custom words to chat's tab-complete dictionary. | Downstream of the `COMMANDS`/chat-completion gap above; no dictionary to add words to. |
| `UPDATE_TAGS` | `ClientboundUpdateTagsPacket` (`network.protocol.common`) — the block/item/entity/fluid tag registries (`#minecraft:logs`, `#minecraft:mineable/pickaxe`, …). | No hardcoded or generated tag table exists anywhere outside `protocol`, and no gameplay code branches on tag membership yet. This is the one Tier C entry worth flagging as **high future value**: tags are load-bearing everywhere in real Minecraft (tool effectiveness, fuel, flammability, …), so whoever eventually adds tag-aware gameplay logic will need this decoded first — it's listed here only because nothing consumes it *yet*, not because it's low-value long-term. |
| `BUNDLE_DELIMITER` | `BundleDelimiterPacket`/`ClientboundBundlePacket` — a pure transport marker. Vanilla's own doc comment: `"This packet should be handled by pipeline"` — it groups the packets between two delimiters into one atomic client-frame update, not game state. | Decoding this correctly is architecturally different from every other packet in this table: it needs no new `ClientEvent` and no crate outside `protocol/v770` — it would wrap `handle_play`'s entire dispatch in an internal buffer-until-closing-delimiter, gated by adapter-held state exactly like the existing `begin_chunk_batch`/`finish_chunk_batch` pair (`adapter.rs:243`). It is not decoded here because that wrap touches the return path of all 109 already-decoded arms at once — the highest blast-radius change available in this file — and nothing observed live so far shows a visibly wrong frame from its absence (unlike `CHUNK_BATCH_START`/`FINISHED`, which have a measurable external effect via the ack rate). Worth doing in a dedicated session with its own golden-vector coverage across a representative slice of the existing decoders, not folded into this one. |

## A note on `CHUNK_BATCH_START`'s "decoded-but-stranded" flag

`cargo xtask connectedness` has flagged `CHUNK_BATCH_START` as decoded-but-stranded since
before this session, and issue #26 is right to insist that be treated as a defect class,
not dismissed. Reviewed here: it is a **known classifier limitation, not dead code**.
`play::clientbound::CHUNK_BATCH_START`'s arm (`adapter.rs:2030`) calls
`self.begin_chunk_batch()`, which starts an `Instant` the paired `CHUNK_BATCH_FINISHED`
arm (`adapter.rs:2036`) reads via `self.finish_chunk_batch(...)` to compute the
`desired_chunks_per_tick` it sends back as `CHUNK_BATCH_RECEIVED` — a real, externally
observable effect (the server throttles chunk delivery on this ack). The classifier only
detects an *outward* `Directive::Emit`/`Send`/world-sink call from the arm being
examined; it cannot see that this arm's effect is consumed by a *different* arm two
packets later through adapter-held state. This was not changed in this session (fixing
the classifier to trace cross-call state is a real xtask enhancement, but out of scope
for a coverage triage) — noted here so the flag isn't mistaken for a fresh finding next
time this doc is read.

## How to change it

- Re-run `cargo run -p xtask -- connectedness` after any adapter change; it is the only
  trustworthy source for the decoded/emits/stranded counts.
- To land a Tier A packet: do the crate patch described above first (in the owning
  crate, by whoever holds it), then the `protocol/v770` decode is a normal addition
  following the pattern in `crates/protocol/v770/src/adapter.rs`'s existing
  `handle_move_minecart_along_track`/`decode_sound` functions — decode with `Reader`,
  validate with `ensure_empty`, emit through the now-real outlet, and add a
  golden-byte-vector test (hand-built from `.cache/mc/26.2/src`, not round-tripped
  through this crate's own encoder) under `crates/protocol/v770/tests/`.
- To land a Tier B packet: build the UI/data-store first in the owning crate (chat
  autocomplete list, recipe book, trading screen, advancements screen, or F3 open-slot
  debug fields), *then* decode the packet — decoding first reproduces the island pattern
  this doc exists to avoid.
- Tier C is not "never" — it's "no plan yet." If the project ever adds dialogs, a
  locator bar, maps, tag-aware gameplay, or a stats/report screen, start from the record
  citations in that table rather than re-deriving them.

## Dependencies

- `.cache/mc/26.2/{src,client-src}` — decompiled 26.2 source, authoritative for every
  record shape cited above.
- `crates/protocol/v770/src/generated/packet_ids.rs` — the packet-id denominator
  `connectedness` reads.
- `crates/lodestone-model/src/event.rs` (`ClientEvent`) and
  `crates/lodestone-model/src/action.rs` (`ClientAction`) — read for this triage, not
  edited; both are outside this task's owned crates.
- `xtask/src/lib.rs`'s `connectedness_report`/`classify_clientbound_dispatch` — the
  measurement tool itself.
