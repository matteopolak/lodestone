# v770 serverbound `play` packet wiring, and why decoding is not the bar

## What it is

The measured state of protocol 776's **serverbound** `play` packets on the hosting side —
what `V770ServerProtocol::decode` understands, what actually reaches a consumer in
`lodestone-server`, and why those two numbers differ by more than 3×. This is the record
for the "server-side decode" issue family — five tracker issues, one per packet-name
grouping (movement/player-state, entity actions/combat, inventory/container, world/block
admin, connection lifecycle) — whose bodies all originally framed the gap as a decode gap.
**That framing is stale**: decode is nearly complete and connectedness is the real bar.

## Measured state

Never hand-count this — run `cargo xtask connectedness` (`CLAUDE.md` records four
hand-derived coverage numbers that were wrong in four different ways).

| axis | measured | meaning |
|---|---|---|
| serverbound **decoded** | **63/69** | `decode` has a real arm and reads the wire |
| serverbound **connected** | **43/69** | that arm produces a `ServerBound` variant a consumer acts on |
| decodes-to-`Ignored`-only | **20** | examined, understood, reaching nothing |
| never examined | **6** | no arm at all |

Reading taken at `ee00ba6a` (`cargo xtask connectedness` re-run directly, not
carried forward from the `62/69, 34/69` table below it — that reading is
`c7029614`'s and is kept for its own narrative, not as the current number).
**This packet-id-granularity axis cannot see `PLAYER_ACTION`'s
`SWAP_ITEM_WITH_OFFHAND` ordinal moving from `Ignored` to
`ServerBound::SwapItemInHand` this pass**, because `PLAYER_ACTION` was already
counted connected via its `START_DESTROY_BLOCK`/`DROP_ITEM`/`RELEASE_USE_ITEM`
arms — the same reason `PLAYER_COMMAND`'s own still-`Ignored`
`START_SPRINTING`/`START_RIDING_JUMP`/etc. ordinals do not show up as a
separate stranded row either. An ordinal-level gap inside an
already-"connected" packet id is invisible to this table; only reading the
`match action { … }` block inside the decode arm finds it.

Audited the three "server-side decode" tracker issues directly against this
reading rather than trusting their own stale `N/M connected` headers
(`#262`/`#264`/`#268`, all still open at time of writing): **`#264`'s all nine
packet ids (`ATTACK`, `INTERACT`, `SWING`, `USE_ITEM_ON`, `USE_ITEM`,
`PLAYER_ACTION`, `PLAYER_COMMAND`, `SPECTATOR_ACTION`, `TELEPORT_TO_ENTITY`)
are connected** — none appear in the stranded list below, so the issue's own
`3/9` header is stale and the issue can close once its comment thread reflects
that. `#262`'s eleven (`MOVE_PLAYER_POS(_ROT)`, `MOVE_PLAYER_ROT`,
`MOVE_PLAYER_STATUS_ONLY`, `PLAYER_INPUT`, `MOVE_VEHICLE` connected;
`PLAYER_ABILITIES`, `PLAYER_LOADED`, `ACCEPT_TELEPORTATION`, `CLIENT_TICK_END`,
`PADDLE_BOAT` genuinely stranded, each re-verified this pass against current
source rather than assumed from an older table — see "Why … stay `Ignored`"
below) and `#268`'s thirteen (`SET_COMMAND_BLOCK`, `CHANGE_DIFFICULTY`,
`LOCK_DIFFICULTY`, `SET_GAME_RULE` connected; the jigsaw/structure/test-block/
command-minecart/custom-click-dialog remainder genuinely stranded) both stay
open — not every variant in either is connected, so per this repo's own
"close only if every variant is genuinely connected" standard neither
qualifies yet.

Reading taken at `c7029614` (`cargo xtask connectedness` re-run directly, not
copied from an older pass — see `CLAUDE.md`'s standing instruction to always
re-run this rather than carry a number forward). **Watch the arithmetic when
comparing to an older copy of this table**, because the two counts move
independently and a naive subtraction misreads what happened. Five packets
moved from `Ignored` to connected since the `e577b4bd` reading this table
previously carried (29 → 34): `RENAME_ITEM` and `CONTAINER_BUTTON_CLICK`
(the anvil rename field and the enchanting table's offer button), then
`PICK_ITEM_FROM_BLOCK`/`PICK_ITEM_FROM_ENTITY` (vanilla's `tryPickItem`
three-way split) and `SET_COMMAND_BLOCK` (issue #48's remainder). None of
that pass's audit found a fresh duplicate-arm hazard, but the instrument
**was** briefly unable to run at all in an earlier pass: `cargo xtask
connectedness` bailed with "duplicate play serverbound decode arm" until that
pass fixed a genuine dead arm — `USE_ITEM` had two `State::Play if packet_id
== play::serverbound::USE_ITEM` match arms in `server_protocol.rs`, an older
`Ignored` stub shadowed by a real one added later and never deleted. **If
this bails again with the same message, grep for `if packet_id ==
play::serverbound::` and diff the packet names for a duplicate** — the
scanner does not (and should not) try to prove reachability itself.

Past that fix, one packet moved from `Ignored` to connected this pass:
`PING_REQUEST` (play-state). Its decode arm already parsed the frame but
discarded the result; `ServerBound::PingRequest` (shared with the Status-state
ping, vanilla's `ServerboundPingRequestPacket` is one class in both states)
sat in `dispatch_play_packet`'s "unreachable here by construction" catch-all
next to `Handshake`/`LoginStart`, which was true only until the decode arm
started constructing it. Fixed in both crates: the decode arm now builds
`ServerBound::PingRequest { time }`, and `dispatch_play_packet` answers with
`ServerProtocol::encode_pong_response` — a trait method that already existed
(added for a different caller, never wired to this one), so no new encode
method was needed. Matches `ServerGamePacketListenerImpl.handlePingRequest`
(`this.connection.send(new ClientboundPongResponsePacket(packet.getTime()))`)
exactly.

The seven with no arm at all: `CHAT_ACK`, `CHAT_COMMAND_SIGNED`,
`CHAT_SESSION_UPDATE`, `COMMAND_SUGGESTION` (the remaining chat/command family —
`CHAT_COMMAND` itself now decodes and connects — needing signature verification
that does not exist anywhere in the tree), plus `COOKIE_RESPONSE`,
`DEBUG_SUBSCRIPTION_REQUEST` and `TEST_INSTANCE_BLOCK_ACTION`. Each of the last
three is deliberately undecoded for a reason stated at the wildcard arm in
`crates/protocol/v770/src/server_protocol.rs` — no encoder to cross-check
against, no VarInt id table to resolve a registry-keyed set, and no codec
support for a nested `Optional<ResourceKey>`/`Vec3i`/`Rotation` composite,
respectively.

## The defect this doc exists to prevent

`ServerBound` is declared in `crates/lodestone-server/src/protocol.rs`. The arms that
construct it are in `crates/protocol/v770/src/server_protocol.rs`, **a different crate**.
Nothing in the type system joins them. So a variant can be:

- declared on the enum,
- matched by `dispatch_play_packet`,
- given a fully-written `apply_*` consumer with a doc comment citing the matching vanilla
  handler,
- covered by an end-to-end test that drives a real transport through a full login,

and still be **constructed by no decode arm at all** — in which case the packet is silently
discarded on the wire forever, and everything above stays green.

This has happened twice, both from one commit. `c4ad474` wired **four** consumers and
updated only **two** decode arms. A later audit found `ClientInformationChanged` and
`ChunkBatchAcknowledged`; `CreativeModeSlotSet` and `ClientCommand`, from the same commit,
stayed dead longer still. Costs, both user-visible:

- **Creative inventory**: every `set_creative_mode_slot` a real client sent was dropped.
- **Respawn**: `apply_client_command`'s `PERFORM_RESPAWN` path was unreachable, so a player
  who died could never leave the death screen. Per `CLAUDE.md`'s live-server hazards a dead
  player is held there and is sent **no chunks** — so the connection became a permanent,
  silent chunk blackout with keep-alives and entity movement still flowing perfectly.

### Why the existing tests all passed

This is the sharpest island instance in the repo, and it is worth studying because none of
the three tests covering these packets was badly written:

| test | what it proved | why it could not fail |
|---|---|---|
| `v770/tests/interaction_actions.rs::set_creative_mode_slot_with_item_is_byte_exact` | the **client encoder** is byte-exact | encode side only |
| `v770/tests/death_respawn.rs::client_command_perform_respawn_is_single_zero_byte` | same, for `client_command` | encode side only |
| `lodestone-server/tests/serve_play.rs::creative_mode_slot_write_lands_in_the_real_inventory` | dispatch **and** consumer, end-to-end over a real transport through a full login/join | it runs against **`FakeProtocol`**, a test `ServerProtocol` (`serve_play.rs`'s own private struct) with its own decode arms and invented packet ids (`SET_CREATIVE_MODE_SLOT_C2S`/`CLIENT_COMMAND_C2S`) |

The third is the instructive one. It is a genuine end-to-end test, and it is an instance of
`CLAUDE.md`'s ***world* species** of vacuous test — the flaw is in the input data, not the
assertions, and cannot be found by reading the test: it was pointed at the one
`ServerProtocol` implementation in the tree that structurally cannot exercise the
production decoder. Compare the recorded colour-fix case verified against `--headless`,
which renders through a different mesher than live terrain.

**So: a `FakeProtocol` test proves dispatch and consumer, never decode.** Any packet whose
only end-to-end coverage runs through `FakeProtocol` still needs a `V770ServerProtocol`
decode assertion, or it is unverified on the wire.

## The gate

`crates/protocol/v770/tests/serverbound_wiring.rs` closes the class structurally:
**every variant declared on `ServerBound` must be constructed somewhere in
`server_protocol.rs`'s non-test code.** No allowlist — the lifecycle variants are built by
the handshake/login/config arms and `Ignored` by the wildcard, so the rule is total.

Two of its scoping decisions are load-bearing, and its own first draft had neither, which
made it **half-vacuous** — the same failure it exists to catch:

- **Comments must be blanked.** A comment in `server_protocol.rs`'s container-remainder
  block contained the prose
  ``// it up is "add a `ServerBound::CreativeModeSlotSet { slot,`` — and the draft gate
  reported only `ClientCommand`, silently missing one of the two live islands. One comment
  was the whole difference between finding both and finding half.
- **`#[cfg(test)]` modules must be blanked.** Otherwise a test assertion
  (`assert_eq!(decoded, ServerBound::Foo { .. })`) counts as a construction and a broken arm
  is masked by its own test.

Scoping to `fn decode`'s body instead was tried and is **too narrow**: it reported
`ContainerClicked` stranded when it is correctly wired, because several arms delegate to a
helper that returns `ServerBound` rather than constructing inline.

The stripper is lifetime-aware by **lookahead**, not by a toggle, because `CLAUDE.md`
records that all three of this repo's pre-existing source scanners were silently broken for
months by a `'` opening a lifetime and never closing — which disabled comment detection for
the rest of every affected file and surfaced only as an unrelated UTF-8 panic.
`the_stripper_survives_a_lifetime_before_a_comment` fails if it is reduced to a toggle.

**Observed control**: run against pristine `HEAD` in an isolated worktree, the gate fails
naming exactly `["ClientCommand", "CreativeModeSlotSet"]` — no false positives, no misses —
while its three self-controls pass.

### What it does not measure

Reachability of the **consumer**, only of the constructor. A variant that decodes and lands
in `dispatch_play_packet`'s no-op group is still stranded, and this gate is silent about it.
`cargo xtask connectedness` reports that half, as `decodes-to-Ignored-only`. The two
instruments are complements: `connectedness` cannot see a missing constructor because it
does not know a consumer exists; this gate cannot see a missing consumer.

## Why 33 packets stay `Ignored`, and what would consume them

Each is decoded — the wire shape is verified and the bytes are consumed — but has no
consumer because the **subsystem** does not exist. Decoding was still worth doing: it moves
a packet from "never examined" to "examined, no consumer", which `connectedness` tracks
separately, and it means the wire work is not the blocker when the subsystem lands.
`CHANGE_GAME_MODE` moved out of this table since the last pass — it decodes and connects
through `game_mode: &mut GameMode` in `dispatch_play_packet`'s own parameter list.

| family | packets | missing subsystem / what would consume it |
|---|---|---|
| flight / load / tick | `PLAYER_ABILITIES`, `PLAYER_LOADED`, `CLIENT_TICK_END` | no flight model, and — checked against `ServerGamePacketListenerImpl.handleAcceptPlayerLoad`/`handleClientTickEnd` this pass — both vanilla handlers only ever *feed a gate* (`hasClientLoaded()`, `receivedMovementThisTick`) that nothing in this crate reads back. Tracking the flag with no gate consulting it would be its own island, not a fix. |
| position-sync-dependent | `ACCEPT_TELEPORTATION`, `SPECTATOR_ACTION`, `TELEPORT_TO_ENTITY` | **there is no clientbound position-sync/teleport encode anywhere in this crate** — checked this pass by grepping `protocol.rs` for a `fn encode_*` naming `position`/`teleport`: zero hits. Vanilla's spectator-teleport handler (`ServerGamePacketListenerImpl.handleTeleportToEntityPacket`) calls `Entity.teleportTo`, which server-authoritatively repositions the player and is exactly what a real `ClientboundPlayerPositionPacket` (with a teleport id `ACCEPT_TELEPORTATION` would confirm) does not exist to send. Building that send path is the subsystem, not a per-packet consumer — out of scope here. |
| pick-item | `PICK_ITEM_FROM_BLOCK`, `PICK_ITEM_FROM_ENTITY` | vanilla's `tryPickItem` (`ServerGamePacketListenerImpl`) needs a block-state→item lookup (`BlockState.getCloneItemStack`) and an entity-type→item lookup (`Entity.getPickResult`, spawn eggs for most mobs). `lodestone_data::block_items` only has the *reverse* direction (`block_for_item`, used for placement) — no `item_for_block`/pick-result table exists to decode into. |
| interaction / combat remainder | `PADDLE_BOAT`, `PLAYER_COMMAND`'s non-`STOP_SLEEPING` ordinals, `PLAYER_ACTION`'s `STAB` ordinal | **`SWING` moved out of this row — `PlayerRegistry` now has a real broadcast push (`registry.swing`), so the "no push path" reasoning below is stale for it specifically and was re-verified against current `server.rs` this pass.** `PADDLE_BOAT` is a distinct, narrower gap: `AbstractBoat.defineSynchedData`'s `DATA_ID_PADDLE_LEFT`/`DATA_ID_PADDLE_RIGHT` share metadata index 18 with 37 total claimants across the committed `EntityDataIndexOracle` dump, so a producer needs a `MetadataField` variant plus a class guard, not just a queue — see `lodestone_server::mobs::MobSim`'s vehicle-snapshot loop, whose own comment discloses the omission as deliberate until "a second player needs to see someone else rowing." `PLAYER_COMMAND`'s `START_SPRINTING`/`STOP_SPRINTING` are redundant with the already-connected `PLAYER_INPUT` sprint bit (the same boolean `apply_attack`'s knockback and sprint-exhaustion already read), so wiring them would write the same value with no new observable effect — not a genuine island, just a second producer for a fact this crate already has. `START_RIDING_JUMP`/`STOP_RIDING_JUMP` need a horse jump-charge model (`PlayerRideableJumping`) this crate has no trace of (`grep`'d `mobs/mod.rs` for `jump_strength`/`canJump`/`handleStartJump`: zero hits). `OPEN_INVENTORY` needs a vehicle-inventory screen (`HasCustomInventoryScreen`), also absent. `START_FALL_FLYING` needs elytra flight, out of scope the same way. `PLAYER_ACTION`'s `STAB` ordinal (7) has no vanilla handler documented in this doc's own source tree pass beyond its enum entry — left unmodelled pending that. |
| container remainder | `CONTAINER_SLOT_STATE_CHANGED`, `RECIPE_BOOK_CHANGE_SETTINGS`, `RECIPE_BOOK_SEEN_RECIPE`, `SELECT_TRADE`, `SET_BEACON`, `EDIT_BOOK`, `SIGN_UPDATE`, `BUNDLE_ITEM_SELECTED` | Re-verified against `.cache/mc/26.2/src`'s `ServerGamePacketListenerImpl` while auditing issue #266 (`CONTAINER_BUTTON_CLICK`, `RENAME_ITEM` and `PLACE_RECIPE` moved out of this row since the last pass — see "Measured state" above). `CONTAINER_SLOT_STATE_CHANGED` needs `CrafterBlockEntity`, which `crate::block_entities::BlockEntity` has no variant for (the enum's own `Composter`/`Furnace`/`Hopper`/`BrewingStand`/`Container`/`Opaque`/`CommandBlock` list has no `Crafter`). `SET_BEACON` needs `BeaconMenu`'s effect state, which `PlayerInventory` does not model at all (no beacon-adjacent field alongside its existing `workstation`/`pending_rename`/`enchant_seed`). `EDIT_BOOK` needs `minecraft:writable_book_content`/`written_book_content` on `ItemComponents` (`crates/lodestone-model/src/item.rs`), which is not one of the modelled components. `SIGN_UPDATE` needs a position-keyed sign store the way `BlockEntityRegistry` has for composter/furnace/hopper/brewing — sign text (`lodestone_world::SignText`) is modelled but nothing keeps one *placed*, so there is nowhere to write the new lines into. `BUNDLE_ITEM_SELECTED`'s blocker moved: `minecraft:bundle_contents` now exists on
`ItemComponents` (`crates/lodestone-model/src/item.rs`), but its own doc comment discloses
that `BundleContents`'s `selectedItem` field never reaches the wire — its only consumer in
vanilla is `BundleItem.overrideOtherStackedOnMe`'s `SECONDARY`-click removal
(`contents.removeOne()` prefers the selected index over the last item), which is
`crate::container_click`'s `tryItemClickBehaviourOverride` gap, itself already disclosed as
unmodelled on that module's own doc comment. Wiring the decode arm without that consumer
would be a pure island — a selected-index write nothing ever reads — so it stays `Ignored`
until the click-behaviour override lands, not because the field is missing anymore. `RECIPE_BOOK_CHANGE_SETTINGS`/`RECIPE_BOOK_SEEN_RECIPE` are the `PONG` shape, not a modelling gap: `handleRecipeBookChangeSettingsPacket`/`handleRecipeBookSeenRecipePacket` only ever write `player.getRecipeBook()`'s GUI-only open/filter/highlight state, which nothing server-side reads back and vanilla itself never syncs to another client — tracking it here would be a write nothing consumes. `SELECT_TRADE` is the one with a real, scoped subsystem gap rather than a missing component: `MerchantMenu.tryMoveItems`/`moveFromInventoryToPaymentSlot` auto-fill two payment slots from the player's inventory against the selected `MerchantOffer`'s cost, and the actual purchase happens on a later `CONTAINER_CLICK` take of `MerchantResultSlot` (an `ItemCombinerMenu`-shaped 2-input+1-result window this crate's `container_click::MenuKind::ItemCombiner` could plausibly grow a `Station::Merchant` arm for) — `crate::server::open_merchant_screen`'s own doc comment already discloses "trade purchase is not wired" for the same reason. |
| world/block admin | `SET_COMMAND_MINECART`, `SET_JIGSAW_BLOCK`, `JIGSAW_GENERATE`, `SET_STRUCTURE_BLOCK`, `SET_TEST_BLOCK`, `CUSTOM_CLICK_ACTION`, `TEST_INSTANCE_BLOCK_ACTION` (never decoded — nested `Optional<ResourceKey>`/`Vec3i`/`Rotation` composite, no codec support) | **`SET_COMMAND_BLOCK` moved out of this row** (see "Measured state" above — it now reuses `crate::command_block` the same way `#48`'s remainder describes). `SET_COMMAND_MINECART` is a distinct gap from `SET_COMMAND_BLOCK`, not a variant of the same fix: `self.minecarts`'s cart data (`lodestone_server::mobs::mod`) has no command-storage field or `Crafter`-style variant the way `BlockEntity::CommandBlock` does. `MinecartCommandBlock.DATA_ID_COMMAND_NAME` shares metadata index 13 with `MinecartFuel` under a different serializer; the existing comment in `mobs/mod.rs` notes the two can never collide today because only the furnace-cart loop produces index 13 — a future command-minecart producer would need the same class guard `MetadataField::Item`'s own doc already establishes the pattern for. The rest: no jigsaw generation, no structure blocks, no game-test framework, no data-driven custom-UI dispatch. **And no permission model of any kind** — see below. |
| lifecycle/telemetry remainder | `PONG`, `CUSTOM_PAYLOAD`'s unregistered channels, `RESOURCE_PACK`, `SEEN_ADVANCEMENTS`, `ENTITY_TAG_QUERY`, `BLOCK_ENTITY_TAG_QUERY`, `CONFIGURATION_ACKNOWLEDGED` | `PONG`: checked against `ServerCommonPacketListenerImpl.handlePong` this pass — it is a genuine **empty method** in vanilla itself, so staying `Ignored` matches vanilla's own behaviour rather than stranding something vanilla acts on; this is not a gap. The rest need a resource-pack push to respond *to* (none is ever sent), an `AdvancementManager` mutator for tab selection (cosmetic-only even in vanilla — `setSelectedTab` has no gameplay effect), an NBT debug-query responder, and a mid-session reconfigure this server never initiates. |

`PING_REQUEST` left this table this pass — see "Measured state" above for what changed.

### The permission model is the blocker, and it should stay one

There is **no op system and no permission model anywhere** in `lodestone-server`
(disclosed on `apply_difficulty_change`'s own doc comment); every connection is the
singleplayer owner. `CHANGE_DIFFICULTY`, `LOCK_DIFFICULTY` and `SET_GAME_RULE` are already
wired *with that omission disclosed on each consumer's doc comment*, which is the right
shape — the alternative is a fake permission check that reads as security and is not.
Anything in the world/block-admin family should decode and say so rather than grow a
half-built permission subsystem underneath it.

### `apply_container_clicked` now re-runs `doClick` server-side — a stale claim corrected 2026-08-14

This section used to say `apply_container_clicked` trusted the client's own predicted
per-slot diff, with a server-authoritative `doClick` port as the eventual shape. That is no
longer true and was verified stale while auditing issue #266, not merely reworded:
`crate::container_click` (`crates/lodestone-server/src/container_click.rs`) is a full port of
vanilla's `AbstractContainerMenu.doClick` over a flat, menu-ordered slot vector — all seven
`ContainerInput` modes (pickup, quick-move, swap, clone, throw, quick-craft/drag, pickup-all),
per-menu `quickMoveStack`/`mayPlace` rules for the player screen, block-entity containers,
the crafting table, the three `ItemCombinerMenu` stations (anvil/grindstone/smithing) and the
enchanting table. `apply_container_clicked` re-derives the result from the click itself and
only consults the client's `changed_slots` map to decide whether a correcting
`container_set_content` is worth sending, per that function's own doc comment. The mint-
anything hole this section used to describe is closed (see `container_click`'s own module doc
for the issue history).

**What is still genuinely unmodelled**, verified directly against that module's own doc
comment while auditing this issue: `tryItemClickBehaviourOverride` (bundle-specific click
behaviour), `canDropItems`, the tutorial hooks, and — found and left as a follow-up rather
than fixed in this pass — `Slot.mayPickup`. Every take in `container_click` succeeds
unconditionally; vanilla's own use of the gate is per-slot rather than uniform, and the one
this crate's anvil economy already half-relies on is `AnvilMenu.mayPickup`:
`(player.hasInfiniteMaterials() || player.experienceLevel >= this.cost.get()) &&
this.cost.get() > 0`. `crate::server`'s `ContainerClicked` arm charges XP levels via
`PlayerExperience::take_levels` **after** the take has already happened rather than gating the
take on having enough levels first, so a survival player with no XP can currently take a
repaired/renamed item off an anvil for free. Fixing it needs an economy-aware
`may_pickup(index, item) -> bool` hook threaded through `container_click` (the same shape
`ResultRecipe` already is for the result slot) — more than a connectivity pass, and
`container_click`'s own module doc now carries the same disclosure at the code site.

## How to change it

- **Adding a `ServerBound` variant** makes `serverbound_wiring.rs` fail until a decode arm
  constructs it. Fix the arm; do not add an exemption.
- **Adding a decode arm** that lifts a packet out of `Ignored` needs three edits, in
  different crates: the variant (`lodestone-server/src/protocol.rs`), the arm
  (`v770/src/server_protocol.rs`), and a `dispatch_play_packet` arm plus consumer
  (`lodestone-server/src/server.rs`). Missing the third leaves the variant stranded, which
  `connectedness` reports; missing the second leaves the consumer dead, which
  `serverbound_wiring.rs` reports.
- **Expected values must come from outside our code.** `decode(encode(x)) == x` is satisfied
  by two symmetric misunderstandings and has already cost real work here. Use
  `.cache/mc/26.2/src`'s decompiled `STREAM_CODEC` (names are real in 26.2),
  `generated/reports/registries.json` for numeric ids, or captured client bytes. The gates
  for the two packets fixed here take their bytes from
  `ServerboundSetCreativeModeSlotPacket`, `ServerboundClientCommandPacket`,
  `ByteBufCodecs.SHORT` (big-endian), `FriendlyByteBuf.writeEnum` (a VarInt
  ordinal) and the registry report's `minecraft:cobblestone = 62`.
- **A capture from a real vanilla client is still the strongest evidence and we have none.**
  Every gate here is hand-decoded from decompiled source, which `CLAUDE.md` accepts, but a
  real client's bytes would be better and no harness exists to collect them — the live
  oracles are *servers*. Building a serverbound capture harness would raise the evidence
  floor for this whole axis.

## Configuration

None. No feature flags gate any of this; `V770ServerProtocol` is the only `ServerProtocol`
implementation, so `v770` is the only family that can host (see
`lodestone-registry`'s `Family`/`ServerFamily` split).

## Dependencies

- `crates/protocol/v770/src/server_protocol.rs` — the decode arms.
- `crates/lodestone-server/src/protocol.rs` — the `ServerBound` enum and the
  `ServerProtocol` trait.
- `crates/lodestone-server/src/server.rs` — `dispatch_play_packet` and the `apply_*`
  consumers.
- `cargo xtask connectedness` — the decoded/connected measurement.
- `.cache/mc/26.2/{src,generated/reports}` — the outside-our-code evidence source.
