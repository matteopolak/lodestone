# Container screens

## What it is

The container/inventory screen family: the shared model that draws any open `Menu` (chest, furnace,
crafting table, anvil-family, creative inventory, merchant), the client-side click predictor that
mirrors vanilla's `doClick`, and the handful of screens with real bespoke chrome on top of that shared
base — cost readouts, station widgets, the creative grid, the merchant trade list, the 3-D player
preview, and inventory potion-effect icons.

## How it works

### The container screen model

`crates/lodestone-shell/src/container.rs` turns a `Menu` (folded server-side state, owned by
`lodestone-game`) into rectangles and vertex streams. It never mutates anything — slot state is
authoritative in `Menu`, this module only projects it.

`slot_layout(&Menu) -> SlotLayout` is the single dispatch every consumer (draw *and* hit-test) calls.
It checks `Menu::special_layout()` first (anvil, grindstone, smithing, enchanting, merchant, furnace
family, brewing stand, loom, stonecutter, cartography table, dispenser/dropper all carry one), then
falls back to `Menu::craft_layout()` (crafting table), then a plain generic grid. Both extra shapes are
attached *to* `Menu` rather than expressed as new `MenuKind` variants — `MenuKind` is matched
exhaustively across the crate, so a third variant would break every match arm, while `craft_layout`/
`special_layout` are additive. Keeping the discriminator on `Menu` also means both callers — the draw
path and `hit_test`/`hit_test_with_scale` in `app.rs` — see it for free; the shape tried first (a
`menu_type` parameter threaded only into drawing) let clicks and pixels disagree about slot positions,
a bug invisible in any screenshot.

Every slot rect carries the real `menu_index` — there is no constant offset. A `Generic { n }` menu is
always `0..n` container, `n..n+27` main, `n+27..n+36` hotbar, with no armour or offhand slots; the
player's own inventory (window 0) is `0` result, `1..=4` 2×2 craft, `5..=8` armour, `9..=35` main,
`36..=44` hotbar, `45` offhand. Special layouts reposition slots pixel-for-pixel (e.g. the anvil's two
inputs and result do not sit in a row) but keep the same generic quick-move regions underneath.

The panel background is real vanilla art — `ContainerBackground` stitches the actual
`textures/gui/container/*.png` sheets (these are hand-placed sub-rect blits at native size, not
`GuiScaling`-driven sprites, and are not part of `GuiAtlas`). With no background attached, a flat
programmatic fill and per-slot wells draw instead, and the title label switches ink color to stay
legible against whichever surface is behind it. The full-canvas dim gradient behind any open container
panel matches vanilla's `isInGameUi()` full-canvas gradient (distinct from the pause menu's tiled
background) and always draws, independent of whether real background art is attached.

A server-supplied potion stack keeps all four player-visible values from
`minecraft:potion_contents`: the mixed ARGB used by item tinting, the optional potion registry
id, the ordered custom-effect list, and the optional effect-name suffix. The 26.2 adapter reads
them from one component payload, `lodestone-model` carries them, and the model-to-game
`ItemStack` conversion preserves them before the tooltip consumes them. Built-in effects render
before custom effects. A component with custom effects but no potion holder still renders those
effects under the uncraftable-potion title and has no registry id; the client must not invent an
identity from its colour. For titles, the stack-wide styled custom name wins first, then the
potion component's name suffix, then the base potion registry id, and finally the no-holder
`empty` suffix. The holder/effect/name wire order is pinned by the external-server payload in
`crates/versions/26.2/tests/fixtures/potion_contents_complete.hex`, captured before adapter decoding.
Re-capture it with the ignored `live-item` gate after starting
`scripts/live-oracles/survival.sh`; the normal entity suite replays it without a server.

Draw order (four stages, matching vanilla's own layering): dim → background texture → chrome (title,
wells) → 3-D item models (depth-tested) → flat sprite icons and text. The **carried stack** (the item
the player is dragging on the cursor) is its own final stratum, replayed after every slot — vanilla's
`nextStratum()` — because a 3-D block on the cursor needs its own depth-clear pass to draw over a
slot's flat icon reliably; append order alone is not sufficient once two different vertex streams
(model vs. flat sprite) are both in play. The hovered-slot tooltip rides the tail of that same stratum
so it draws above everything else, including any overlay UI that must be inserted *between* strata
rather than after the whole draw.

**The crafting result slot is always read from the server, never locally recomputed.** Vanilla computes
crafting results server-side and sends a `container_set_slot` for the result; there is no local recipe
matcher here. `Menus::predicted_craft_result` exists only for recipe-book ghost previews and must never
be written into the real result slot.

A screen's title comes from a `Text` component the server sends (e.g. `translate("container.crafting")`)
and must be resolved through the language table at the point it's read, not flattened with a bare
plain-string conversion — a raw fallback prints the literal translation key rather than words. The
title anchor and the (usually present) second "Inventory" label both derive their x/y from the same
expression the panel art is blitted with, never as a restated constant, since panel height varies with
row count.

### Click handling

`crates/lodestone-game/src/click.rs` and `menu.rs` are a version-free reimplementation of vanilla's
own container-click handler, run locally the instant the player clicks so the screen updates before
the server confirms. It is deliberately faithful to vanilla's quirks rather than "corrected" — a
corrected implementation would predict a different outcome than the server computes and desync the
display for a round trip. The server holds its own independent port of the same function as the
authority; the two must agree by construction (same vanilla source, ported twice) rather than by
sharing a crate, and when they disagree the server's `container_set_content` correction is what
reconciles the client.

Seven click modes exist (`ContainerInput`): `Pickup` (left/right click), `QuickMove` (shift-click),
`Swap` (hotbar/offhand number key), `Clone` (creative middle-click), `Throw` (drop one/stack), `QuickCraft`
(the paint-drag sequence), `PickupAll` (double-click gather). The drag sequence is a three-stage state
machine — START arms a drag type, ADD records a painted slot, END distributes the cursor's stack across
every recorded slot — reset by a bad header sequence, an empty cursor, or an invalid drag type, but
**not** by painting an invalid slot (that slot is silently skipped and the drag stays armed). While a
drag is held, the client also draws vanilla's provisional preview (a translucent wash plus the count
each cell would receive), computed from the exact same split arithmetic used to finish the drag rather
than a second copy — a preview that could disagree with the real outcome would be worse than no
preview.

The drag distribution field has one closed internal domain: `QuickCraftType::{Even, One, Clone}`.
`Menu::do_click` validates the raw two-bit button value before arming a drag, and `Menu` stores the
validated type through the ADD/END state machine and preview arithmetic. The public preview and
stateful drag entry points retain raw integers for the shell's input layer, but reject an unknown
value instead of falling through to a distribution formula. `Clone` is additionally gated on infinite
materials; `3` remains unused and cannot arm or paint a drag.

Quick-move (shift-click) destination order is per-menu-kind and transcribed exactly: a generic
container moves backwards into the player inventory (hotbar first) and forwards out of it; a crafting
table tries to load its grid before falling back to the main/hotbar hop; the player's own inventory
screen has an eight-step order (result → craft grid → armour → auto-equip armour/offhand → main↔hotbar
→ everything else) where auto-equip must be reachable from every source slot, including the off-hand,
or armor silently stops auto-equipping from one direction. The furnace family receives one narrow
item-kind override: when the server's recipe-book sync declares the source item's numeric registry id
in that screen's cooking-input property set, the shell resolves it to an identifier and prediction
targets slot 0 only. It does not guess fuel routing: non-input items and a missing property set retain
generic region order, while an input that cannot fit remains for server reconciliation rather than
spilling into the fuel slot. Brewing stand routing is still generic because no equivalent complete
input classification is available.

The screen's own input protocol (press/drag/release/keyPress, `MenuInput` in `container.rs`) is a
separate layer from the click predictor above and has its own defect class: the machine can be
perfectly correct while no input path ever calls it. The number keys 1–9 while a container is open are
`SWAP` against the hovered slot, not hotbar-selection — that binding is swallowed entirely while any
screen is open, matching vanilla's own keybind-handling gate on no active screen. `Q` (drop)
inside a container screen and `Q` during ordinary gameplay are two structurally different mechanisms —
one is a container click that gets a server round-trip correction on mismatch, the other
(`DropSelectedItem`/`DropSelectedItemStack`) gets no confirmation packet at all and must predict
locally or the dropped count never updates on screen.

## Cost screens: anvil, grindstone, smithing, enchanting

These four screens share vanilla's `ItemCombinerMenu` shape (or, for the enchanting table, an
equivalent positional layout): a small `Generic`-sized menu whose *slot kinds* differ (a take-only
output slot; the enchanting table's lapis-only input) and whose *pixel positions* are a `SpecialLayout`
rather than the generic grid. The client renders each with real vanilla background art and real slot
positions; what's still missing client-side is the input-slot `mayPlace` predicates that need registry
data this tree doesn't carry yet (smithing's recipe check, the grindstone's damageable/enchanted
check) — anything placed there is accepted and corrected by the server if wrong.

The anvil's XP-cost readout and the enchanting table's three per-row offer costs both reach pixels: the
server streams them as `container_data` properties, folded into `ContainerFrame::cost_data`, and drawn
with the vanilla font/outline machinery (see [`hud.md`](./hud.md) for that font stack). The same feed
is what lets the furnace family's lit/burn bars and the brewing stand's fuel/brew bars draw with no
extra plumbing — they're the same property stream, just a different `special_layout` match. The
enchanting table's three offer rows are clickable (not just drawn): the click hit-test uses the exact
same rect the draw code computes, and the click-eligibility gate (lapis count, level, cost) mirrors
vanilla's own client-side pre-check before ever sending a packet.

## Station widgets: enchanting offers, stonecutter, loom

Three screens each need a "predict, then send" click surface with no local pending state — a valid
click *is* the send, matching vanilla's own client-side menu mirror. They're offered a click in a
fixed precedence chain (merchant trade rows → beacon buttons → enchant offers → stonecutter grid →
loom grid → recipe-book panel → ordinary slot click); each stage only sees a click the earlier stages
refused, so adding a new click surface means adding it to every later stage's guard, not just its own
line.

The stonecutter's ordered result rows arrive in recipe synchronization data and are the single source
for its redraw, wheel range and click validation. This matters for datapacked servers: consulting the
shell's bundled `RecipeBook` could draw one order while sending a button id for another. The frame
carries both the server rows and the start index derived from the persisted wheel offset; drawing
applies `skip(start).take(12)` before filtering unresolvable icons, so a blank row cannot renumber a
later server button. The loom's offer list is instead a small hardcoded table transcribed from the
pack's banner-pattern tag JSON (a pattern item does not always grant its namesake pattern). Both grids'
scroll math is wheel-only; scrollbar thumb-drag remains unwired. Stonecutter result icons draw from the
server rows, while loom pattern icons remain a disclosed cut.

## Creative inventory

The 14-tab strip, scrollable 5×9 grid, search tab and inventory tab, with contents transcribed from
vanilla's real creative-tab table (1725 items) and cross-checked against the item registry. It is
**not** a `MenuKind` — vanilla's own backing menu (`ItemPickerMenu`) has no server container at all, so
this screen owns its own layout rather than extending the shared dispatch; everything downstream of
layout (vertex streams, draw passes) is the same shared container pipeline. The signal that opens this
screen instead of the ordinary inventory is the player's `instabuild` ability flag, matching vanilla's
own gate — not a game-mode enum, which this shell doesn't have a reader for anyway.

Three deliberate departures from vanilla, each because the creative grid has no real cursor-carrying
menu behind it: clicking a grid cell places the item directly into the selected hotbar slot (one wire
action) rather than picking it onto a cursor first; the saved-hotbars tab is honestly empty (no on-disk
store for saved hotbars exists); clicks on the real inventory slots underneath are consumed and
ignored rather than passed through, since the screen shown is not the one those slots belong to.
Search matches a case-insensitive substring of the item's registry path, not its display name; tag
queries are not modelled.

## Merchant / trading screen

The villager/wandering-trader screen, reached when the server opens a `minecraft:merchant` menu
followed by its trade offers. This is the client UI half only — trade generation, professions and the
right-click-to-open path are server-side work that doesn't exist yet, so today this screen is only
reachable by synthesizing the relevant packets. The menu shape is a `Generic { 3 }` (two payment slots,
one take-only result) with `SpecialLayout::Merchant`; its player-inventory section is the one special
layout whose x-offset isn't the panel's own left edge, because the panel itself is wider than usual.

The seven-row trade list is drawn as fixed pixel offsets, not real slots ("fake items" in vanilla's own
terms) — cost/result icons, a demand-adjusted price with a strikethrough when discounted, and an
in/out-of-stock arrow. Selecting a row sends `SelectTrade`; nothing else about the trade (payment
auto-fill, the scroller past seven visible offers, the XP/level progress bar) is modelled yet, each
because it needs either the not-yet-built server half or registry data this tree doesn't carry.

## Player preview

The player model rendered live inside the inventory screen, head and eyes tracking the mouse — the
first place in this codebase to draw a full 3-D entity rig inside a 2-D GUI panel, and the first to use
a GPU scissor rect. The pose math is ported field-for-field from vanilla's own inventory-avatar
transform, reusing the *same* function that places every mob in the world (composed rather than
restated) so the entity rig and its GUI-panel placement can never drift apart. One detail is easy to
get backwards: the head yaw is relative to the body yaw, not a second absolute angle — the head
genuinely rotates twice as far as the body for the "eyes follow you" effect, and treating both as
absolute yaws draws a player permanently looking over their own shoulder. The pass records after the
panel background and before slot items, matching vanilla's own draw order.

## Potion effects (inventory)

The column of active-effect widgets beside the inventory panel — icon, translated name with a level
numeral, and remaining duration, on a nine-sliced background. Ordering follows vanilla's real
comparator (non-ambient before ambient, finite before infinite, shorter duration first, then effect
color) rather than insertion order. The effect icons are **not** part of the ordinary GUI sprite atlas
— they come from a second declared source directory the pack's own atlas definition points at, so any
code that assumes one source directory for GUI sprites silently finds none of them. This inventory
widget and the HUD's own status-effect overlay are two different widgets over the same effect state,
not one shared draw call, and the HUD one gates on a per-effect "show icon" flag this inventory widget
does not consult.

## How to change it

- **Match vanilla's decompiled source line-for-line before touching a click mode or menu layout.**
  Every hand-derived expected value in this family's tests comes from the decompile, never from this
  port's own prior implementation — a self-consistent wrong answer passes a test built the same way.
- **Never add a `MenuKind` variant.** It's matched exhaustively across the crate; a new screen shape
  goes on `Menu` as an additive descriptor (`CraftLayout`, `SpecialLayout`) instead.
- **A restated pixel constant instead of a shared expression is the most common regression here.**
  Label anchors, panel height and the second "Inventory" label position must all be derived from the
  same expression the panel art uses; a chest with more rows moves every one of these, and hardcoding
  one breaks as soon as row count varies.
- **The 3-D item-model pass needs both `models` and a real `depth` view passed to the renderer.** An
  attached-but-unfed model pipeline is a real, previously-shipped bug class here — flat sprite icons
  still draw fine (they don't need depth), which is exactly why the symptom reads as "block items
  render flat" rather than "nothing renders."
- **A server-initiated container close needs its own reconciliation**, separate from the screen state
  machine — closing only the menu model without a matching screen transition leaves the screen drawing
  a stale player inventory once the real window's state is gone.
- **Furnace input routing is driven only by the live server property set.** Keep numeric-id resolution
  at the shell boundary and carry identifiers through `PlayerCtx`; do not add a data dependency to
  `lodestone-game` or infer fuel eligibility. Brewing-stand and merchant item-kind routing still need
  their own authoritative inputs rather than hardcoded slot numbers.

## Configuration

None of this family has its own config file or flags. Behavior is driven entirely by what's attached at
setup time: whether an item atlas, item-model pipeline, background art and vanilla font are attached to
the renderer (each has a documented, legible fallback when absent — a colour swatch, flat sprites only,
a flat panel fill, and a fixed-advance debug font, respectively), and whether a `ContainerFrame` carries
a cursor, cost data or trade offers at all.

## Dependencies

- `lodestone-game` — `Menu`, `MenuKind`, `CraftLayout`, `SpecialLayout`, `click.rs`, `menus.rs`, the
  predict/reconcile seam.
- `lodestone-server` — the independent server-authoritative port of the same click semantics (not a
  build dependency; agreement is by construction against the same vanilla source).
- `crates/lodestone-shell/src/container.rs` and `container/{background,geometry,builder,renderer,
  layout,enchant,stonecutter,loom,merchant,player_preview}.rs` — layout, background art, vertex
  streams, and the per-screen click surfaces.
- `crate::hud::item_icon` — the shared icon-drawing pass also used by the hotbar.
- `lodestone-render`/`lodestone-assets` — block models, the GUI atlas and its palette, and the
  container-art atlas built separately from `GuiAtlas`.
- The 26.2 jar under `.cache/mc/26.2/{client-src,client.jar}` — behavioral reference only, never
  transliterated.
- [`ui-framework.md`](./ui-framework.md) — the shared widget/frame conventions this family builds
  the panel and label chrome from.
- [`hud.md`](./hud.md) — the vanilla font/outline machinery cost readouts and potion-effect text share
  with the HUD.
