# Serverbound coverage: encoders versus producers

## What it is

The record of which serverbound packets this client can **encode** and which it
actually **produces**, and why those are two different numbers. A batch of new
encoders landed the encoder half; this doc exists because the encoder half is the
half that is easy to measure and the wrong one to trust.

## How it works

`cargo xtask connectedness` reports, for v770, `serverbound encoded N/69`. That
counts arms in `crates/protocol/v770/src/adapter/mod.rs`'s `encode_action` match. It
**cannot see a producer**, so an encoder with nothing upstream of it counts as
coverage — and that has shipped four times:

| action | how it surfaced |
|---|---|
| `ClientAction::SetFlying` | four adapters encoded it, zero producers; the server kicked us with `multiplayer.disconnect.flying` |
| `ClientAction::ChangeGameMode` | zero producers until a game-mode switcher was added |
| `ClientAction::PlaceRecipe` | zero producers; the shell synthesises three container clicks instead |
| `PlayerCommand::StartFallFlying` | four adapter encoders, zero producers, until riptide added the first |

So the question to ask of a serverbound packet is never "does it encode" but
**"what sends it"**.

`crates/lodestone-ecs/tests/serverbound_producer_census.rs` snapshots the set with
an encoder and no producer. It fails when someone lands the first producer for a
listed entry, and the fix is to delete the line.

## The measured numbers

Measured with `cargo xtask connectedness`, not carried forward from any issue body:

* **before the encoder batch**: `serverbound encoded 54/69`
* **after the encoder batch**: `serverbound encoded 67/69`

The two remaining are `CHAT_COMMAND_SIGNED` and `CHAT_SESSION_UPDATE`, which belong
to the chat-signing issue and are deliberately out of scope here.

**Note the encoder batch's own tracking undercounted: its title said twelve
packets but its body actually listed thirteen.** Thirteen is right:
`BLOCK_ENTITY_TAG_QUERY`, `CHANGE_DIFFICULTY`, `DEBUG_SUBSCRIPTION_REQUEST`,
`ENTITY_TAG_QUERY`, `JIGSAW_GENERATE`, `LOCK_DIFFICULTY`, `SET_COMMAND_MINECART`,
`SET_GAME_RULE`, `SET_JIGSAW_BLOCK`, `SET_STRUCTURE_BLOCK`, `SET_TEST_BLOCK`,
`TEST_INSTANCE_BLOCK_ACTION`, `CUSTOM_CLICK_ACTION`.

## The three wire shapes a transliterating encoder gets wrong

Each is gated in `crates/protocol/v770/tests/operator_encoders.rs` with an
assertion that fails under the plausible wrong encoding, not merely one that
passes under the right one.

| packet | trap | why the wrong version looks fine |
|---|---|---|
| `set_structure_block` | `offset`/`size` are six **signed bytes**, not two `Vec3i`s of VarInts, and the flags byte is **last**, after `seed` | a VarInt and a byte are the same length for any value in `0..=127`, so only a *negative* offset separates them |
| `set_jigsaw_block` | `joint` is `getSerializedName()`, a **string** | every other enum field in the family is a VarInt ordinal; the server reads a wrong `joint` as a zero-length name and silently defaults to `ALIGNED` |
| `custom_click_action` | **double-framed**: an outer VarInt *byte* length wrapping the optional-NBT body | the payload still "looks like" NBT either way; only the length prefix's presence differs |

`Vec3i.STREAM_CODEC` is three plain `ByteBufCodecs.VAR_INT`s and is **not** zigzag.
An earlier draft of the decode gate assumed it was, and the hand-built bytes caught
it — the decoder was right and the test was wrong, which is the direction you want
that disagreement to go.

## What has an encoder and no producer

Seventeen entries, each verified by hand. The full list with blockers lives in
`KNOWN_UNPRODUCED` in the census gate; the grouping is:

* **Creative/operator editor screens that do not exist** — structure block, jigsaw
  block (plus its Generate button), test block, test instance block, command
  minecart. Six entries. Nothing is missing but the screen.
* **The settings menu** — `ChangeDifficulty`, `LockDifficulty`, `SetGameRules`.
  Note the dependency if that work starts.
* **Debug/shell input** — `QueryBlockEntityTag` and `QueryEntityTag` want vanilla's
  F3+I copy-NBT keybind; `SubscribeDebug` wants a debug-overlay toggle.
* **The dialog screen** — `CustomClickAction`. `show_dialog` now decodes into
  `SessionServerInfo`, so the inbound half is done and the reply is waiting on a
  renderer.
* **`PlaceRecipe`** — brokered separately.
* **The `PlayerCommand` family** — `StopSleeping`, `StartRidingJump`,
  `StopRidingJump`, `OpenInventory`. All four are keypresses, so all four are shell
  input. Found by grepping the family after `StartFallFlying` turned out to be the
  fourth instance of the `SetFlying` shape; **the whole family was worth checking
  precisely because one member of it had already been wrong**.

  Two of the four have since resolved, in *opposite* directions, and the pair is the
  useful reading. `StartRidingJump` gained a producer
  (`lodestone_ecs::vehicle::charge_riding_jump`) once the client became authoritative
  over the vehicle it rides. `StopRidingJump` never will: it exists in
  `ServerboundPlayerCommandPacket.Action` and **the vanilla client has no sender for
  it** — only `LocalPlayer.sendRidingJump` exists, and `AbstractHorse.handleStopJump`
  is an empty method. So "encoded with no producer" is not always a gap waiting on a
  screen; sometimes zero is the correct count, and the entry's blocker should say so
  rather than naming an input that will never be written.

### Two paired halves, which is the useful pattern

`SubscribeDebug` and the four `debug_*` clientbound packets are one loop: the
server sends **nothing** on a debug feed until a client subscribes, so the request
without the response is silence and the response without the request is dead code.
The same is true of `show_dialog` → `CustomClickAction`. When a serverbound packet
looks unmotivated, check whether it is the request half of something clientbound.

### The eighteen that are *not* verified — now individually audited

A whole-enum sweep of `ClientAction` reported eighteen further variants with no
producer found by name. **Two of the eighteen — `MoveVehicle` and `PaddleBoat` — have
since gained producers** in `lodestone_ecs::vehicle::send_vehicle_actions`, which is a
data point about the list rather than only about riding: they were sitting here
*unverified* while being genuine islands of exactly the `SetFlying` shape, so an
unverified entry is a real lead and not noise.

**The remaining sixteen have now been read one at a time, per the call-path rule
below, and the doc's own worked example was itself wrong.** It claimed
`SignUpdate` was a false positive, produced through `submit.into_action()` in
`lodestone-shell/src/app/menus.rs`. That call resolves to
`CommandBlockSubmit::into_action`, which returns `ClientAction::SetCommandBlock` —
a *different* variant. `SignUpdate` had **no producer anywhere outside
`crates/protocol/` and `lodestone-model`'s own dispatch** at the time that claim was
written: sign text could be encoded by every adapter and nothing in the shell could
ever send it.

The lesson is the reverse of the one originally drawn here. An indirection no name
scanner can follow does not mean the scanner is wrong — it means nobody checked,
and "the code exists" is not evidence a variant is produced. Audit each entry by
**reading the call path to its terminal `ClientAction`**, not by finding an
indirection and assuming it lands where the name suggests, and not by grepping the
variant name alone (a real producer can sit behind a `*Submit::into_action()`-shaped
type, as `SetCommandBlock` and now `SignUpdate`/`RenameItem` do).

**Four of the sixteen turned out to already have real producers** — two were
already fixed since this doc was last written, and two were fixed as part of that
audit:

* **`EndClientTick`** — **has a producer.** `lodestone-shell/src/sim/step.rs`
  pushes it every tick, gated on `Egress::in_world`, with a comment recording that
  this was itself once "encoded with no producer outside a test" and got fixed.
  The stale claim below ("only found in a test") was true when written and is not
  now — see `CLAUDE.md`'s note on stale status annotations being the highest-decay
  content in this repo.
* **`SelectTrade`** — **has a producer.** `Sim::send_select_trade`
  (`lodestone-shell/src/sim/session.rs`) is called from
  `app/container_input.rs`'s merchant-screen row click.
* **`SignUpdate`** — **fixed.** The sign-editing screen
  (`crate::menu::sign_edit::SignEditState`) now exists, is opened from a real
  server-driven trigger (`ClientEvent::SignEditorOpened` → `Sim::poll_net` →
  `app::session::drive_ui_from_session` → `MenuNav::open_sign_edit`), and sends on
  every exit (Done or Escape) via `MenuAction::SignUpdate`.
* **`RenameItem`** — **fixed.** The anvil's rename box
  (`crate::container::AnvilRenameState`) now has real keyboard focus and a
  responder that sends `ClientAction::RenameItem` on every edit, including
  vanilla's "identical to the item's own unmodified name normalises to empty
  string" rule.
* **`ResourcePackResponse`** — **fixed, twice over.** It first got a producer
  as an unconditional auto-decline (`net.rs`'s `auto_resource_pack_response`,
  landed against exactly the `SetFlying` failure mode this section describes
  below) — correct at the time, but a dead end: it answered every push
  without ever showing the player anything, so a server's resource pack
  could never actually be seen or installed. It now has a **real** producer
  set instead: `net.rs`'s `route_resource_pack_pushed`/`apply_pack_response`/
  `spawn_pack_download` send every status in vanilla's own sequence
  (`ACCEPTED` → `DOWNLOADED` → `SUCCESSFULLY_LOADED`/a failure status, or an
  immediate `DECLINED`/`INVALID_URL`), driven by the per-server
  `menu::servers::ServerPackPolicy` and — when that policy says to ask — a
  real accept/decline dialog (`Screen::ResourcePackPrompt`,
  `menu::confirm::ResourcePackPromptNav`). `ClientEvent::ResourcePackPushed`/
  `Popped` are still `Route::NOWHERE` below — they are answered directly in
  `net.rs`'s own loop, not through the `forward`/`poll_net` path.

**Eleven were confirmed genuine islands; all eleven have since gained real
producers** — zero hits for the bare variant name anywhere in
`lodestone-shell` or `lodestone-controller`, in any form, at the time this
section was written: `ContainerButtonClick`, `EditBook`, `PingRequest`,
`RecipeBookSeenRecipe`, `SeenAdvancements`, `SelectBundleItem`,
`SetBeaconEffects`, `SetContainerSlotState`, `SpectatorAction`, `Stab`,
`TeleportToEntity`. Each was screen- or input-blocked in the same shape as the
seventeen in `KNOWN_UNPRODUCED`
above (an editor/UI that does not exist yet, or a keybind that is not wired).

Since fixed:

* **`ResourcePackResponse`** — see the entry above; it went through two
  producers, the second one real.
* **`SetBeaconEffects`** — the beacon screen's power buttons and confirm/cancel
  (`crate::container::beacon`) call `Sim::send_set_beacon_effects` from
  `app::container_input::WindowApp::handle_beacon_click` on a valid confirm.
* **`EditBook`** — the book-and-quill editor (`crate::menu::book_edit`,
  `crate::menu::text_area`) sends on Done/Finalize from `WindowApp::try_use`'s
  writable-book fork.
* **`SeenAdvancements`** — `Sim::send_seen_advancements`, called every frame
  the Advancements screen's open tab or open/closed state changes
  (`app::advancements_screen::WindowApp::advancement_progress`, via the pure
  `seen_advancements_transition` helper it delegates to).
* **`ContainerButtonClick`** — the enchanting table's three enchant-offer rows
  (`crate::container::enchant`) call `Sim::send_container_button_click` from
  `app::container_input::WindowApp::handle_enchant_click` on a click the
  client-side gate (`offer_clickable`, `EnchantmentMenu.clickMenuButton`'s own
  predicate) accepts. Vanilla's other two `ContainerButtonClick` screens
  (stonecutter, loom) remain unproduced — both pick from a server-populated
  recipe/pattern list this tree has no registry sync for, a different shape
  from the enchant offers' `container_data`-driven costs. See
  [`container-cost-screens.md`](container-cost-screens.md) for the geometry
  and the client-side gate.
* **`PingRequest`** — `Sim::send_ping_request` (`sim/session.rs`), the first
  production caller anywhere outside `crates/protocol/`. Sent from
  `app/redraw.rs`'s per-frame housekeeping, gated on the F3 debug overlay
  being open and throttled to once a second (`should_send_ping_request`).
  Real vanilla only sends this while its network-chart sub-panel shows
  (`PingDebugMonitor.tick`); this client has no such sub-panel, so F3 itself
  is the closest honest equivalent.
* **`SelectBundleItem`** — `crate::container::bundle::bundle_slot_scrolled`
  (`ScrollWheelHandler.getNextScrollWheelSelection` transcribed) resolves a
  `MouseWheel` notch over a bundle-holding slot into the new selection;
  `app::container_input::WindowApp::handle_bundle_scroll` reaches it from a
  new `MouseWheel` arm gated the same way the container click arm is
  (`self.ui.is_container_open()`), and `Sim::send_select_bundle_item` sends.
  Needed the `minecraft:bundle_contents` item component to exist first — see
  `lodestone_model::ItemComponents::bundle_contents`'s own doc.
* **`RecipeBookSeenRecipe`** — `Sim::send_recipe_book_seen_recipe`
  (`sim/session.rs`), called from `app/session.rs`'s
  `WindowApp::sync_recipe_book_seen` every frame the recipe-book panel is
  open, the same shape `restore_recipe_book_settings`/`sync_recipe_toasts`
  already use. Transcribes vanilla's real trigger
  (`RecipeButton.init` → `RecipeBookPage.recipeShown` →
  `RecipeBookComponent.recipeShown` → `LocalPlayer.removeRecipeHighlight`):
  a recipe is reported seen the moment a `RecipeButton` for it is populated
  onto a visible page, not on a click, and only while that page is on
  screen. `WindowApp::recipe_book_seen: HashSet<i32>` dedups so a recipe
  whose button stays on screen for many frames is reported exactly once.

**`Stab`, `SpectatorAction` and `SetContainerSlotState` are now fixed; only
`TeleportToEntity` remains.**

* **`Stab`** — `Sim::begin_attack_live` (`sim/actions.rs`) now checks
  `lodestone_game::item::is_piercing_weapon` on the main-hand stack *before*
  its ordinary entity/block/miss switch, matching `Minecraft.startAttack()`
  reading `heldItem.get(DataComponents.PIERCING_WEAPON)` in the same place —
  unconditionally, even with no entity in range (an "air stab"), then
  swinging the arm. A prior claim that this had "no reference anywhere in the
  decompiled source beyond an unrelated enum value" was wrong (cost two
  agents a declined guess); the real call chain is
  `MultiPlayerGameMode.piercingAttack(weapon)`, sending exactly
  `ClientAction::Stab`'s wire shape
  (`ServerboundPlayerActionPacket(Action.STAB, BlockPos.ZERO, Direction.DOWN)`).
  **Named simplification**: this crate has no item-prototype default-component
  merge step, so there is no actual `piercing_weapon` component value on a
  stack to read even for a real spear — `is_piercing_weapon` checks item
  identity against the fixed list of the seven real spear tiers instead (same
  shape `is_bundle` already takes for the missing `#minecraft:bundles` tag).
* **`SpectatorAction`** — the same call site, one branch earlier:
  `MultiPlayerGameMode.isSpectator()` is `Minecraft.startAttack()`'s *first*
  gate, ahead of the piercing-weapon check and the ordinary switch alike.
  `Sim::begin_attack_live` now checks the local player's `ServerGameMode`
  first and, while spectating, sends `SpectatorAction { target_entity_id:
  self.entity_target() }` — `Some(id)` on a left-click over an entity
  (`MultiPlayerGameMode.spectate`), `None` otherwise
  (`spectatorNoAction`) — reusing the same nearer entity-or-block pick every
  other left-click branch here already resolves. Neither vanilla arm swings
  the arm.
* **`SetContainerSlotState`** — `crate::container::crafter::toggle_decision`
  (pure, unit-tested) transcribes `CrafterScreen.slotClicked`'s `PICKUP` case:
  re-enable a disabled slot unconditionally, or disable an enabled one only
  when nothing is carried. `app::container_input::WindowApp::
  maybe_toggle_crafter_slot` gates it on the open menu's `menu_type ==
  "crafter_3x3"` and an empty slot inside the 9-slot craft grid, then
  `Sim::send_set_container_slot_state` sends. **Unlike every other producer in
  this file, this one never *consumes* the click** — vanilla's own override
  still calls `super.slotClicked(...)` right after its toggle check, so it is
  wired as a side effect alongside the ordinary slot click, not as a
  first-refusal widget. **Named simplifications**: only a plain click is
  handled (vanilla's `SWAP` case — a hotbar number pressed over a disabled
  slot holding a matching item — is not), and there is no local, optimistic
  `containerData` mutation the way `CrafterMenu.setSlotState` gives vanilla's
  own client; the toggle becomes visible only once the server's
  `container_set_data` echoes it back. No render support for the
  disabled-slot sprite exists yet either.
* **`TeleportToEntity`** — see the paragraph immediately below for the full
  chain; the short version is `crate::menu::spectator_menu` plus
  `app/menus.rs`'s `MenuAction::TeleportToEntity` arm.

**`TeleportToEntity` is now fixed too — all twelve of this issue's variants
have real producers.** The issue's own framing of the trigger was wrong, and
that is worth keeping as recorded, exactly the "search for the capability,
not the name" trap: `ClientTabListScreen`/`PlayerTabOverlay` (the tab-list HUD
this repo already renders in `tablist.rs`) has **no click handling anywhere**
in `client-src` — it is a pure readout. The real, sole trigger is
`PlayerMenuItem.selectItem`, reached only through vanilla's dedicated
**Spectator Menu** (`SpectatorMenu`/`SpectatorGui`, opened by pressing a
hotbar-number key while spectating, since spectator mode has no hotbar
selection to intercept those keys for instead). Bolting a teleport action
onto the Social Interactions screen was considered and rejected for the
reason recorded here before: it has real player rows and click handling, but
attaching a teleport action there would be a fabricated vanilla behaviour,
not a simplification of a real one.

`crates/lodestone-shell/src/menu/spectator_menu.rs` is the real menu that
landed instead — see that module's own doc for the full shape and every
named simplification (a scrolling vertical list rather than vanilla's
paginated icon-slot bar; a placeholder head icon rather than a real skin
face; team grouping without vanilla's exact `TeleportToTeam` construction).
`spectator_menu_entries` folds a live `TabList` + `Scoreboard` snapshot into
root categories/players; `SpectatorMenuState::activate` resolves a clicked
row into an expand, a "Back", or a
`ClientAction::TeleportToEntity { target }` send, the last of which closes
the screen (`app/menus.rs`'s `MenuAction::TeleportToEntity` arm sends it
through `Sim::net()`). Opened by any hotbar-number key while
`Sim::is_spectator()` (`app/input.rs`'s `KeyGate::spectator`, checked ahead
of the ordinary hotbar-slot-select arm).

## How to change it

Adding an encoder is fine and useful ahead of its producer — **say which of the
three buckets it is in**: has a producer, waiting on a named screen, or waiting on
shell input. Add the entry to `KNOWN_UNPRODUCED` with that blocker. An entry with
no stated blocker is the actual defect, because it is one nobody decided about.

Do not wire a consumer for a feature that is client-authoritative in vanilla. An
issue-body plan to "decode `USE_ITEM`" for riptide pointed the wrong way: riptide's
launch is **client-predicted**, so the server is not the source of that motion and a
serverbound decode would have been building the wrong half.

## Configuration

None. `cargo xtask connectedness` takes no arguments; the census gate runs under
`cargo test -p lodestone-ecs`.

## Dependencies

`crates/protocol/v770/src/adapter/mod.rs` for the encoders,
`crates/lodestone-model/src/action.rs` for `ClientAction` and `PlayerCommand`, and
`xtask/src/lib.rs`'s `connectedness_report` for the encoder count.
