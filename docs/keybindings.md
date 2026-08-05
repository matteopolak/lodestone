# Keybindings

## What it is

A rebindable table mapping **logical actions** (`key.forward`, `key.inventory`)
to **physical inputs** (a keyboard key or a mouse button), so nothing in the
shell's gameplay input path names a key literally. It exists so the Controls
menu is a later, small addition rather than a rewrite of `app.rs`.

Three types, all in [`crates/lodestone-shell/src/keybinds.rs`](../crates/lodestone-shell/src/keybinds.rs):

| type | is |
|---|---|
| `InputAction` | the closed set of things a player can ask for |
| `Binding` | what they press: `Key(KeyCode)`, `Mouse(MouseButton)`, or `Unbound` |
| `Keybinds` | the table joining them, plus the queries a Controls menu needs |

**The Controls menu UI exists now** (issue #15, `crates/lodestone-shell/src/menu/key_binds.rs`) — this doc's own module docs called it "a later, small addition", and it was: everything in `Keybinds` this doc describes was already there, and the screen was a last hop over it rather than construction. See that module's own doc for the screen's geometry (`KeyBindsList`, a different `AbstractSelectionList` from every other settings page's `OptionsList`) and [`docs/settings-screen.md`](./settings-screen.md) for how it slots into the rest of the tree. **Finishing a rebind is landed too** — `app.rs` now forwards the raw key/mouse event a capture needs; see "Wiring the Controls menu" below for the exact patch.

## How it works

### Dispatch is on actions, not keys

`app.rs` asks the table, never a `KeyCode`:

```rust
} else if binds.is(InputAction::DebugOverlay, code) && pressed {
```

`Keybinds::is` is a `Binding` equality test — it does not consult the name table,
so matching a key costs nothing per event.

### The precedence chain is a pure function

`app::resolve_key(binds, gate, code, pressed, ctrl) -> Option<KeyOutcome>`
holds the entire decision. `KeyGate` is the four booleans it reads off
`UiState` (`menu`, `chat_open`, `container_open`, `gameplay`); `ctrl` is
whether Control is currently held, tracked by the driver the same way
`shift_held` is (see "One action, two mechanisms: `key.drop`" below — it is
the only thing that reads this parameter); `KeyOutcome` is the one side
effect the driver should then perform.

**The order is behaviour, not layout.** Three arms swallow keys:

1. `gate.menu` first — a menu screen owns the whole keyboard (the server-address
   form needs every printable key).
2. `gate.chat_open` second — while the prompt is up, `W` types a `w`.
3. `gate.container_open` before every gameplay binding — and it returns `None`
   for an unrecognised key rather than falling through, so nothing leaks to
   gameplay behind an open inventory.

The `Pause` arm sits **above** the container arm, so Escape closes a container
through `on_escape` rather than through `CloseContainer`. That is why the
container arm handles only the inventory binding: its Escape case was already
unreachable in the original chain.

Extracting this was the point — the swallowing order is now unit-testable
without a window, a GPU or a `Sim`. `app.rs`'s `mod tests` drives it directly.

### Where the defaults come from

Read out of `.cache/mc/26.2/client-src`, not from memory:

| what | where |
|---|---|
| action names, keysyms, categories | `net/minecraft/client/Options.java:655-725` |
| category sort order | `net/minecraft/client/KeyMapping.java:203-225` |
| persisted key/button names | `com/mojang/blaze3d/platform/InputConstants.java:342-478` |
| the save-line shape | `Options.java:1618-1622` |

`Category::SORT_ORDER` reproduces vanilla's **registration** order, which is not
alphabetical and not what a reader would guess — `Misc` is *second*, before
`Multiplayer`, `Gameplay` and `Inventory`. That is the order the Controls screen
groups by.

## Configuration

Persisted inside `Options` (`options.json`, alongside `servers.json`) under
`"keybinds"`, as a flat `action name → binding name` object:

```json
{
  "gui_scale": 3,
  "keybinds": {
    "key.forward": "key.keyboard.up",
    "key.attack": "key.mouse.middle",
    "key.use": "key.keyboard.unknown"
  }
}
```

Why this shape:

- **Flat strings, not integers.** Meaningful to a human editing the file, and
  stable across winit upgrades. A `KeyCode`'s numeric discriminant has no
  stability guarantee, so an upgrade could silently move every binding.
- **Vanilla's vocabulary** on both sides of the colon, so a future importer of a
  real `options.txt` is a lookup rather than a translation layer.
- **Only non-default entries are written.** The file says exactly what the user
  changed — and, the reason that matters, a default we change later actually
  *reaches* existing users instead of being pinned forever by a value their file
  happened to record. Vanilla writes every line and has the opposite behaviour.
  A fresh install has no `keybinds` key at all.

**Loading never fails.** A non-object value is the defaults; an unknown action
name is skipped; an unparseable or non-string binding leaves that action at its
default. Every case keeps reading later entries — an early `?` there is how one
stale line silently discards every binding after it. `gui_scale` is read
independently, so a broken `keybinds` block cannot cost it.

## How to change it

### Adding an action

A table entry, not a structural change. Four places, all in `keybinds.rs`:

1. a variant on `InputAction` — **append it in category order**, matching the
   position you give it in `ALL`;
2. the same variant in `InputAction::ALL` (a test asserts `action as usize`
   equals its index, because `Keybinds` indexes the array by discriminant — get
   this wrong and two bindings cross-wire silently);
3. arms in `name()`, `category()` and `default_binding()` — the compiler demands
   all three;
4. a branch in `app::resolve_key` plus a `KeyOutcome` variant, and the effect in
   `window_event`'s match.

**Step 4 is not optional.** An action with no consumer is a Controls-menu row
that does nothing — the island defect of `CLAUDE.md` §1. Only actions this client
actually dispatches are in the table today, which is why vanilla mappings we do
not implement (`key.advancements`, `key.fullscreen`, …) are absent rather than
listed and dead.

**There is a fifth place, easy to miss because it lives outside `keybinds.rs`
entirely: `action_caption` in `crates/lodestone-shell/src/menu/key_binds.rs`.**
It is a second exhaustive `match` over `InputAction` — the Controls-menu row
label — and the compiler enforces it exactly as hard as the three arms step 3
names, just from a different file. Adding a variant here and forgetting that
match is not a silent island (it will not compile), but it means "the compiler
demands all three" above undercounts by one match arm that happens to sit in a
sibling module.

If the new action is a movement one, also add it to `movement()` so it reaches
`lodestone-controller`.

### One action, two mechanisms: `key.swapOffhand` (issues #382, #385)

`key.swapOffhand` was the table's one **partial** entry until #385 closed it, and
it is the reason step 4 above says "a branch" rather than "the branch": vanilla's
`F` means two genuinely different things depending on what is on screen, and they
share nothing but the key.

| context | mechanism | our route |
|---|---|---|
| screen open, slot hovered | container click, `ClickType.SWAP`, button `40` (`AbstractContainerScreen.java:506-522`) | `KeyOutcome::ContainerSwap { button: 40 }` → `Click::offhand_swap` |
| no screen, normal play | `ServerboundPlayerActionPacket` / `SWAP_ITEM_WITH_OFFHAND` (`Minecraft.java:1900-1905`) | `KeyOutcome::SwapOffhand` → `ClientAction::SwapItemWithOffhand` |

The gameplay one carries **no slot**. There is nothing to hit-test, because the
server does the exchange itself
(`ServerGamePacketListenerImpl.java:1294-1300`). Reusing `ContainerSwap` for it
would hit-test a screen that is not open, resolve nothing, and silently return —
a dead key that looks wired. `app::tests` asserts the two outcomes are
`assert_ne!` distinct for exactly that reason.

**No local prediction, and that is vanilla parity rather than a shortcut.**
`handleKeybinds`' entire client half is the `send`; there is no `Inventory`
mutation and no animation. Contrast issue #381's block placement, where vanilla
*does* predict and not predicting is the divergence — the direction of the
argument is opposite, so do not generalise one to the other.

**The one guard is `!player.isSpectator()`**, applied at the driver's `match` arm
rather than in `resolve_key`, because it is session state and `resolve_key` only
knows about keys — the same split `ContainerSwap`'s empty-cursor and
hovered-slot guards use.

**The two arms ask in different orders relative to the number keys**, and that is
each context's own source rather than an inconsistency:
`checkHotbarKeyPressed` asks the off-hand key first;
`Minecraft.handleKeybinds` asks `keyHotbarSlots` at `:1873` and
`keySwapOffhand` at `:1900`. Only visible if someone rebinds the off-hand key
onto a digit.

**What the protocol layer contributed: nothing, and that was worth checking.**
`ClientAction::SwapItemWithOffhand` and its v770 encoder already existed, tested
byte-exact — so this was a wiring job, not the island `#304` ("12 serverbound
packets we cannot encode") would have made it. `cargo xtask connectedness`
reports `serverbound encoded 53/69` with `player_action` among the 53, and is
**silent on this issue either way**: it measures packet coverage, and the gap was
one missing `else if` two layers above the wire. A tool that cannot see the
defect class is not evidence about it.

### One action, two mechanisms: `key.drop` (issues #16, #27)

Landed the same shape as `key.swapOffhand` above, and once that precedent
existed this one took no new design: same two-mechanism split, same "one
guard at the driver arm, not in `resolve_key`" boundary, same ordering
argument. The one genuinely new piece is `ctrl`.

| context | mechanism | our route |
|---|---|---|
| screen open, slot hovered | `ContainerInput::Throw`, button `0`/`1` (`AbstractContainerScreen.java:495-501`) | `KeyOutcome::ContainerDrop { ctrl }` → `MenuInput::key_pressed` → `Click::drop_one`/`drop_stack` |
| no screen, normal play | bare `PLAYER_ACTION`/`DROP_ITEM`\|`DROP_ALL_ITEMS` (`Minecraft.java:1907-1911`) | `KeyOutcome::Drop { ctrl }` → `ClientAction::DropSelectedItem`/`DropSelectedItemStack` |

**`resolve_key` gained a fifth parameter, `ctrl: bool`, instead of the
modifier being read at the driver's `match` arm.** The rule this follows:
`resolve_key` is where every other input decision already lives, and a
decision made outside it is invisible to this function's own tests — the
same reasoning that keeps the empty-cursor/hovered-slot guards for
`ContainerSwap` at the driver arm (they are *session* state, not key state)
while `ctrl` stays inside `resolve_key` (it is read off a tracked *key*
state, `WindowApp::ctrl_held`, the same shape `shift_held` already has).
Threading it through was not invasive: one new parameter, one new tracked
field, and every existing call site took a mechanical `, false`.

**Both wire-side actions were already fully built before this landed, with
zero producers** — `Click::drop_one`/`drop_stack`/`do_throw`
(`lodestone-game`, #27) and `ClientAction::DropSelectedItem`/
`DropSelectedItemStack` (all four protocol adapters). **`MenuInput::
key_pressed`'s `Drop` arm also already existed** by the time this landed
(`container.rs`, landed concurrently with the research that scoped this
work) — the actual gap was purely the `app.rs` call sites on both sides of
the container-open boundary. Worth recording: a "producer is missing" claim
needs re-verifying against the current tree before assuming which hop is
actually the gap, the same lesson `CLAUDE.md`'s staleness section already
gives for other claims.

See `docs/combat.md`'s "The drop key (`Q`)" for the vanilla-source detail
(the `hoveredSlot.hasItem()` gate, why it is `else if` not two `if`s, why
`PickItem` is not creative-gated at this layer) and the live-gate story.

### One action, two mechanisms: `key.pickItem` (issue #16)

Same shape again, and the container half was *already wired* before this
variant existed at all — the one genuine novelty vanilla's own source adds is
that the no-screen half branches on **what the crosshair is over**, not on a
fixed packet.

| context | mechanism | our route |
|---|---|---|
| screen open, slot hovered | container click, `ClickType.CLONE` (`AbstractContainerScreen.java:495-501`, `MenuKey::PickItem` → `Click::clone_slot`) | `KeyOutcome::ContainerPickItem` → `MenuInput::key_pressed` |
| no screen, crosshair on a block | `ServerboundPickItemFromBlockPacket` (`Minecraft.pickBlockOrEntity`, `Minecraft.java:2342-2354`) | `KeyOutcome::PickItem { ctrl }` → `ClientAction::PickItemFromBlock { pos, include_data: ctrl }` |
| no screen, crosshair on an entity | `ServerboundPickItemFromEntityPacket` | `KeyOutcome::PickItem { ctrl }` → `ClientAction::PickItemFromEntity { entity_id, include_data: ctrl }` |
| no screen, crosshair on nothing | nothing sent | `KeyOutcome::PickItem { ctrl }` resolves to a no-op inside `Sim` |

**Default binding is a mouse button, not a key** — `Options.java:669` declares
`keyPickItem` as `Type.MOUSE` button `2` (middle-click), category `GAMEPLAY`,
the same reason `Attack`/`Use` default to mouse buttons rather than keys. The
container half of middle-click was already live before this issue, through
`crate::app::menu_button_for`'s `MouseButton::Middle → MenuButton::Pick`
mapping — the only genuinely new plumbing is the *keyboard-rebind* form (both
in and out of a container) and the entire no-screen gameplay form.

**`include_data` is vanilla's `hasControlDown()`**, read off the same tracked
`ctrl_held` field `key.drop`'s `ctrl` already established — one boolean, two
completely different meanings depending which of the two `ClientAction`
variants it rides on (a block's block-entity data vs. an entity's own data),
matching `Minecraft.pickBlockOrEntity`'s single `includeData` local passed to
either branch of its own switch.

**Target resolution reuses the attack/use raycast, not a new one.** Vanilla's
`pickBlockOrEntity` reads `this.hitResult`, the same field `startAttack` and
`startUseItem` already read — so the gameplay half is "whatever `Sim` already
tracks as the current entity/block target", not a second ray cast. `case
ENTITY` still wins over `case BLOCK` for the identical reason
[`Self::begin_attack`]'s own doc gives (the entity ray target is already the
nearer of the two picks).

**Both `ClientAction` variants, and the container-key arm, were already built
and tested with zero producer** before this issue closed the gap — the same
"verb built, no caller" shape `key.drop` was in, confirmed against the tree
rather than assumed: `PickItemFromBlock`/`PickItemFromEntity` are encoded and
byte-exact tested in all four protocol families (`v770`'s own encoder, `v47`/
`v340`/`v735` all `AdapterError::Unsupported` — pre-`v770` versions have no
such packet), and `MenuKey::PickItem => Click::clone_slot` in `container.rs`
already had a passing test
(`the_pick_block_key_clones_even_in_survival_where_the_mouse_does_not`) with
no `app.rs` call site reaching it.

**Both routes — mouse and keyboard, gameplay and container — are verified
live on the current tree.** `mouse_action_for` and `resolve_key` both consult
`Keybinds` generically (neither hardcodes `MouseButton::Middle`), so rebinding
`key.pickItem` off the middle button silences the mouse route and a keyboard
rebind reaches `KeyOutcome::PickItem`/`ContainerPickItem` correctly — checked
by tracing both dispatch sites, not assumed from the table entry existing.

**One stale test surfaced while verifying this**, and it was red on committed
`main`: `app::tests::the_mouse_path_resolves_the_default_attack_and_use_buttons`
asserted `mouse_action_for(&binds, MouseButton::Middle) == None` under the
comment "Middle is the container pick gesture, not a gameplay binding" — true
before `InputAction::PickItem` existed, false since `9d66cba`, and nothing
caught it because the assertion is inside a test whose *name* is about
attack/use, so a reviewer skimming for pick-item coverage would not think to
look here. `cargo test -p lodestone-shell --lib --no-fail-fast` confirms it as
the only keybinding-related failure (four unrelated `entities.rs` failures
also present — not this pass's, not touched). Fix handed to the orchestrator
in the same brokered `app.rs` patch as the screenshot wiring below, since both
land in the same file.

### Screenshot: `key.screenshot`, the one verb with no packet (issue #16)

**The odd one out.** Every other action in this table ends at a
`ClientAction` or a container `Click` — something that leaves the client.
`key.screenshot` ends at a file: vanilla's `Screenshot.grab`
(`Screenshot.java:37-70`) copies the main render target's colour texture to a
CPU buffer and writes it as a PNG, entirely locally, to
`<gameDirectory>/screenshots/`.

| | vanilla | this client |
|---|---|---|
| default binding | `Options.java:675`, GLFW keysym `291` = F2, category `MISC` | `InputAction::Screenshot`, `Binding::Key(KeyCode::F2)` |
| file location | `<gameDirectory>/screenshots/` | `screenshots/`, relative to the process's working directory (this client has no separate "game directory" concept yet) |
| filename | `Util.getFilenameFormattedDateTime()` (`yyyy-MM-dd_HH.mm.ss`) + `_2`, `_3`, … on a same-second collision, `.png` (`Screenshot.getFile`, `:136-148`) | same scheme — never overwrites an existing file |
| format | PNG, straight from the render target's own colour data | PNG via the `png` crate. **This row used to claim `png` was already a `lodestone-shell` dependency. It was not** — it sat in `[dev-dependencies]`, where its only job was encoding fixture images *into* the favicon-mosaic tests, so nothing shippable could reach it. Landing the verb moved it to `[dependencies]`; a plausible-sounding claim about a manifest is still a claim (§12) |
| capture point | `RenderSystem`'s `copyTextureToBuffer` against `GameRenderer.mainRenderTarget()`, mapped and walked into a `NativeImage` | the same `copy_texture_to_buffer` → `map_async(MapMode::Read)` idiom this repo already uses for pixel-gate readback (`lodestone_render::HeadlessTarget::read_texels`, `crates/lodestone-render/src/target.rs`) — applied to the **window** target instead of a headless one |

**Not modelled: the Control-held panorama variant.**
`Minecraft.handleGlobalKeyPress` passes `controlDown` to `Screenshot.grab`,
which only takes the four-angle `panorama_0..3.png` branch when
`SharedConstants.DEBUG_PANORAMA_SCREENSHOT` is also true — a developer-only
flag vanilla ships `false` in every release build, so a normal player's
Ctrl+F2 is byte-identical to a plain F2. `InputAction::Screenshot` carries no
`ctrl` payload for this reason.

**Not modelled: `handleGlobalKeyPress`'s screen-independence.** Vanilla checks
`keyScreenshot` *outside* `Screen.keyPressed` (`Minecraft.java:2224-2234`), so
a screenshot can be taken from the pause menu or an open inventory — the same
category as `key.fullscreen` and `key.friends`, which share that call site.
This client's `resolve_key` swallows every key behind `gate.menu`/
`gate.chat_open`/`gate.container_open` before any action-specific arm runs, so
`key.screenshot` — like `key.debug.overlay` before it — does not fire from
behind a menu here. Fixing both together, if ever wanted, is one change:
hoist both arms above the early-return gates.

**Why this verb is brokered further than the other three.** Pick-item, drop
and swap-offhand all bottom out in something `Sim`/`net.rs` already knows how
to send. A screenshot needs the live window target's actual surface texture,
and reads it back **after** the frame is drawn rather than before — see the
correction below.

**Confirmed, not assumed: the open risk this section used to flag was real.**
`SurfaceTarget::new` (`crates/lodestone-render/src/target.rs`) builds its
config from `surface.get_default_config(adapter, …)`, which sets only
`TextureUsages::RENDER_ATTACHMENT` — unlike `HeadlessTarget::USAGE`, which
explicitly ORs in `COPY_SRC`. A window's `AcquiredFrame` cannot be copied out
until the config adds `COPY_SRC` before `configure()`, and `AcquiredFrame`
itself has no accessor to its `wgpu::SurfaceTexture`'s backing `Texture` at
all (`view()` returns only the `TextureView`, which `copy_texture_to_buffer`
cannot source from). Both are `lodestone-render` changes, which is why this
verb reaches a third crate no other action in this table touches.

**One correction to this section's own previous plan, caught by working
through the actual sequencing rather than trusting the sketch:** capturing
"right after `target.acquire()` succeeds and before the frame is handed to
the renderer" would copy out an **undrawn** frame — a swapchain image has no
defined content until something renders into it, so a capture at that point
reads garbage, not a screenshot. The correct point is symmetric with
`frame.present(queue)`: drain `pending_screenshot` and copy the texture out
**immediately before** `present`, once every render pass (world, HUD, menu
overlays) has already written into `frame.view()`. This is the kind of stale
plan CLAUDE.md's §2 warns about — true-sounding when sketched, wrong once
someone actually traces where pixels land — caught here before it shipped
rather than after, only because implementing it required tracing `redraw()`'s
real call order instead of restating the earlier note.

**Landed.** The patch below was drafted a session earlier and brokered through
issue #436 because `app.rs` was a choke point; the `app.rs` → `app/` split
(`7be1b2f`) removed that constraint and it was applied directly. What follows is
therefore a record of what is in the tree, not a plan — with one correction
found on applying it, and one on re-verification:

- **The `lodestone-render` half was still genuinely missing**, exactly as the
  "Confirmed, not assumed" paragraph above says. A later report claimed the
  `COPY_SRC`/`texture()` pieces already existed and had merely been found on
  `HeadlessTarget`; re-reading `SurfaceTarget::new` settled it — the window
  target had neither piece, and `HeadlessTarget` having both is *why* the
  confusion is available. **A neighbouring type answering the question is
  harder to catch than a stale claim.**
- The code lives in `crates/lodestone-shell/src/screenshot.rs`
  (`timestamp_name`, `unused_path`, `to_rgba8`, `encode_png`, `capture`), wired
  from `app/input.rs` (`KeyOutcome::Screenshot` + the `resolve_key` arm),
  `app.rs` (`pending_screenshot`), `app/lifecycle.rs` (the effects arm) and
  `app/redraw.rs` (the drain, immediately before `frame.present(queue)`).

- `lodestone-render/src/target.rs`: OR `wgpu::TextureUsages::COPY_SRC` into
  the config in `SurfaceTarget::new` before `surface.configure(device,
  &config)`; add `AcquiredFrame::texture(&self) -> Option<&wgpu::Texture>`
  (`None` for a headless frame, whose `surface_texture` field is already
  `None` — screenshots never run headless, so this is a natural sentinel, not
  a new case to handle).
- `app.rs`: `KeyOutcome::Screenshot` (no payload — see the two "not modelled"
  notes above, both of which still hold), a `resolve_key` arm sitting right
  after `ToggleDebugOverlay`'s (same tier: gated on `pressed` only, not
  `gate.gameplay`, matching F3's own precedent since vanilla's `key.screenshot`
  is `Category.MISC` and screen-independent the same way `key.debug.overlay`
  is treated here), a `pending_screenshot: bool` field on `WindowApp`
  (mirrors `ctrl_held`), the effects-match arm setting it, and the actual
  readback + PNG encode (same `copy_texture_to_buffer` →
  `map_async(MapMode::Read)` idiom as `HeadlessTarget::read_texels`, plus a
  BGRA→RGBA channel swap since a Metal/Vulkan swapchain typically hands back
  `Bgra8*` and the `png` crate only writes RGBA) called from `redraw()`
  immediately before `frame.present(queue)`.
- Filename: vanilla's own `yyyy-MM-dd_HH.mm.ss[_N].png` scheme, hand-rolled
  against `SystemTime` (civil-from-days algorithm) rather than pulling in a
  calendar crate for one filename — **in UTC, not local time**, a deliberate,
  named divergence from `Util.getFilenameFormattedDateTime()`'s local clock,
  worth revisiting if a date crate ever lands in this workspace for another
  reason.
- **`#[cfg(test)]`-forked path, not a `cfg!(test)` runtime check** — per
  CLAUDE.md's "OS-level side effect" hazard (a unit test must never write into
  a player's real `screenshots/` directory): a `screenshot_dir()` function
  with two bodies, one under `#[cfg(test)]` returning a temp directory, one
  without returning `"screenshots"`, plus a test asserting the temp-dir body
  is the one actually compiled — so deleting the fork fails a test instead of
  silently reverting to writing into the real directory on every `cargo test`.

**What only a live frame can confirm.** Everything above the GPU is unit-gated
(`screenshot.rs`'s five tests: the `#[cfg(test)]` directory fork, the civil-date
arithmetic against hand-checked leap years, same-second collision suffixing, the
BGRA/RGBA swizzle with an unrepresentable-format refusal, and padded-row
stripping across three rows). What no test here can reach is the one thing that
needed `lodestone-render`: that a **real windowed swapchain**, configured with
`COPY_SRC`, actually permits `copy_texture_to_buffer` on this adapter, and that
the resulting file shows the frame the player saw rather than the previous one.
A headless target's `AcquiredFrame::texture()` is `None` by construction, so the
capture path cannot be exercised without a window. Press F2 in-game and open the
file.

Tracked on [#436](https://github.com/matteopolak/lodestone/issues/436) and
[#16](https://github.com/matteopolak/lodestone/issues/16).

### Wiring the Controls menu — fully landed

Issue #15 landed. `crates/lodestone-shell/src/menu/key_binds.rs` is the screen;
`SettingsPage::KeyBinds` (`crates/lodestone-shell/src/menu/options.rs`) is
where it slots into the settings tree, reached from the Controls page's own
"Key Binds..." button. Per-row Reset, the footer's Reset Keys, viewing every
one of `InputAction::ALL`'s actions (29 as of issue #16's `PickItem`/
`Screenshot`; check `InputAction::ALL`'s own length rather than trusting this
number, which has already gone stale twice) grouped by category in vanilla's
registration order (`Category::SORT_ORDER`, not `InputAction::ALL`'s
declaration order — see that module's own tests for the trap), and starting a
rebind are all wired and persist immediately, the same eager-persistence rule
every other live row in
this tree follows.

What this section used to sketch as three methods to add landed almost
exactly as written, on `MenuNav` (`crates/lodestone-shell/src/menu/nav.rs`):

- `awaiting_key_capture(&self) -> bool` — whether a bind button is mid-capture.
- `capture_binding(&mut self, binding: Binding)` — finishes it: sets the
  action's binding and persists, unless the action is `InputAction::Pause` and
  `binding` is `Binding::Unbound`, which it refuses (the hazard this doc
  already named — "nothing enforces that yet" is no longer true).
- The `rebind`/`reset_keybinds` sketch became `SettingsNav`/`KeyBindsNav`
  internals reached through `KeyBindsOutcome` rather than public `MenuNav`
  methods, because the *starting* half of a rebind (clicking the bind button)
  is ordinary menu input with no `Keybinds` mutation of its own — only
  *finishing* one needs to reach `Options`, which is what `capture_binding`
  is for.

**The last hop needed `app.rs`, and it is landed.** Starting a capture is a
click like any other, handled entirely inside this crate. *Finishing* one
needs the **next raw key or mouse event**, and `app.rs`'s `menu_key_for` — the
function that turns a `winit::event::KeyEvent` into a `MenuKey` — silently
drops any physical key with no printable `text` (an F-key, a modifier, an
arrow other than Up/Down: see that function's own `_ => {}` branch). Rebinding
to exactly one of those is a real, common case (F-keys are a standard rebind
target), and none of it should ever reach `resolve_key`'s gameplay dispatch or
the ordinary `MenuKey` path while a bind button is capturing.

The patch, in `crates/lodestone-shell/src/app.rs`'s `window_event`:

1. **`WindowEvent::KeyboardInput`'s `Some(KeyOutcome::Menu)` arm.** Checks
   `self.nav.awaiting_key_capture()` **before** calling `menu_key_for` at all,
   not only when it returns `None` — a capture target can be a printable key
   too (most vanilla rebinds are), and `menu_key_for` would otherwise consume
   it as `MenuKey::Char` first. The decision itself is a free function,
   `capture_key_for(physical_key) -> Option<CaptureKey>`, extracted the same
   way `resolve_key` and `menu_key_for` are — unit-testable without a window
   — rather than inlined into the match arm:

   ```rust
   enum CaptureKey {
       Cancel,
       Bind(KeyCode),
   }

   fn capture_key_for(physical_key: PhysicalKey) -> Option<CaptureKey> {
       match physical_key {
           PhysicalKey::Code(KeyCode::Escape) => Some(CaptureKey::Cancel),
           PhysicalKey::Code(code) => Some(CaptureKey::Bind(code)),
           PhysicalKey::Unidentified(_) => None,
       }
   }
   ```

   and the arm itself:

   ```rust
   if pressed && self.nav.awaiting_key_capture() {
       match capture_key_for(event.physical_key) {
           // `KeyBindsNav::escape` via the *ordinary* MenuKey path, not
           // `capture_binding`. Vanilla sets `InputConstants.UNKNOWN` on
           // Escape unconditionally (`KeyBindsScreen.java:73-74`); this
           // client deliberately does not — see `capture_binding`'s own doc
           // on why unconditional-Unbound is the `Pause` hazard.
           Some(CaptureKey::Cancel) => self.handle_menu_key(MenuKey::Escape),
           Some(CaptureKey::Bind(code)) => self.nav.capture_binding(Binding::Key(code)),
           None => {}
       }
   } else if pressed && let Some(key) = Self::menu_key_for(&event) {
       self.handle_menu_key(key);
       let want = self.ui.wants_cursor_grab();
       if want != self.grabbed {
           self.set_grab(want);
       }
   }
   ```

2. **`WindowEvent::MouseInput`'s menu arm** (guarded on
   `owns_frame(self.ui.screen()) || self.ui.is_paused() || self.ui.is_death()`).
   A mouse-button rebind (vanilla defaults `key.attack` to the left button,
   `key.pickItem` to the middle one — real cases, not hypothetical) needs any
   button, not only Left, and runs *before* the existing "click acts on the
   row under the cursor" branch — otherwise a capture would immediately
   consume its own confirming click as a hover-row activation instead:

   ```rust
   if state == ElementState::Pressed {
       if self.nav.awaiting_key_capture() {
           self.nav.capture_binding(Binding::Mouse(button));
       } else if button == MouseButton::Left {
           if let Some(row) = self.menu_row_at(self.cursor.0, self.cursor.1) {
               let action = self.nav.click(&mut self.ui, row);
               self.apply_menu_action(action);
           }
       }
   }
   ```

   The grab-sync lines after the existing `if` block are unaffected — a
   capture never changes `wants_cursor_grab()`, so nothing there needed
   touching.

Neither arm touches `resolve_key`, `KeyGate`, or anything on the *gameplay*
dispatch path — capture only ever intercepts inside the two menu-input arms,
which already run only while a menu screen owns the frame.

`crates/lodestone-shell/src/menu/nav.rs`'s
`clicking_a_bind_button_then_capturing_a_key_rebinds_and_persists`,
`escape_while_capturing_cancels_without_changing_the_binding` and
`capturing_pause_refuses_to_leave_it_unbound` already drive
`MenuNav::capture_binding` directly and prove everything on this crate's side
of the hop — persistence (asserting the exact persisted string, e.g.
`"key.keyboard.f1"` for an F-key in `app.rs`'s own
`capture_key_for_forwards_a_function_key`), the Escape-cancels behaviour, the
`Pause` guard — so `app.rs`'s patch had nothing left to get wrong on the model
side; it only had to *call* the method, which `capture_key_for`'s own tests
(`app::tests::capture_key_for_*`) confirm it does for each physical-key case.

The menu's own needs beyond that were already queries, unchanged from this
section's original sketch:

- `Keybinds::in_category(c)` + `Category::SORT_ORDER` — grouping and order
- `Keybinds::is_default(a)` — whether to show a reset affordance
- `Keybinds::conflicts(a)` / `has_conflict(a)` — vanilla's duplicate highlight,
  answered symmetrically for both sides, and what decorates a bind button's
  label with `[ … ]` in `key_binds.rs`
- `Binding::label()` — a short button caption

### Census: what still bypasses this layer, and what does not

Swept with `grep -rn "KeyCode::"` / `"MouseButton::"` across
`crates/lodestone-shell/src/` outside `keybinds.rs` itself, to find every raw
physical-input match that is not a table lookup.

**Genuine gaps, closed or being closed this pass:** the two named at the top
of this doc's own history — `key.pickItem`'s mouse route (verified already
correct, see above) and `key.screenshot` (drafted, see above).

**Everything else that matched is deliberately literal, not a missed gap**,
each for a reason already argued elsewhere in this doc or in vanilla's own
source:

- `app.rs`'s `menu_key_for`/`handle_chat_key` (arrows, Enter, Tab, Backspace,
  Delete, the server-list F5 refresh) — menu/chat chrome, matching vanilla's
  `Screen`-level handling. See "Menu navigation and text editing stay
  literal" below.
- `app.rs`'s `menu_button_for` (`MouseButton::Left/Right/Middle` →
  `MenuButton::Left/Right/Pick`) — a container screen's own click-type
  mapping, hardcoded in vanilla too (`AbstractContainerScreen.mouseClicked`
  switches on the raw button index, never on a `KeyMapping`). This is *not*
  the same thing as `key.pickItem`'s keyboard form: the container's
  **mouse** click-to-clone is vanilla-hardcoded and stays that way regardless
  of how `key.pickItem` itself is bound, which is why `menu_button_for` and
  `InputAction::PickItem` can disagree about the middle button without either
  being wrong.
- `app.rs`'s `ctrl_held`/`shift_held` tracking (`ShiftLeft`/`ShiftRight`,
  `ControlLeft`/`ControlRight`) — modifier *state*, read at a dispatch arm
  (`key.drop`'s stack-vs-one, `key.pickItem`'s `include_data`), not a
  dispatch itself. See `key.drop`'s section above on why this lives outside
  `resolve_key`'s table lookups but still inside `resolve_key`'s signature.
- `config.rs`'s and `menu/nav.rs`'s hits are all test fixtures constructing
  `Binding`/`Keybinds` values, not input handling.

No other file under `crates/lodestone-shell/src/` matched either pattern at
all — `hud.rs`, `container.rs`, `chat.rs`, `interact.rs`, `sim.rs` and every
`sim/*`/`menu/*` module besides `nav.rs`/`key_binds.rs` consume only
`InputAction`/`Binding`/`ClientAction`/`Click` values that already passed
through `app.rs`'s dispatch, never a raw key or button of their own.

## Gotchas

- **Physical keys, not characters.** The identity is winit's `KeyCode` — the
  key's *position*. Right for movement (`WASD` is a shape under the left hand, so
  AZERTY gets `ZQSD` for free) but it means `Binding::label()` is
  layout-independent: it says "W" for the key an AZERTY user has marked Z.
  Vanilla has the same tension and resolves it the same way. Fixing the label
  means capturing `KeyEvent::logical_key` at rebind time and caching it — still
  not done, but the reason changed now that the menu exists (#15): the landed
  `app.rs` patch (see "Wiring the Controls menu") forwards a `KeyCode` to
  `capture_binding`, not a `KeyEvent`, so today's label is layout-independent
  by construction rather than by omission. Caching the
  logical key would mean threading the whole event through instead of just the
  physical code. Text entry is unaffected — the chat prompt and address field
  already use `KeyEvent::text`.
- **F3 *is* a real vanilla `KeyMapping` in 26.2.** Worth checking rather than
  assuming: older versions handled the debug keys inline in `KeyboardHandler`,
  but 26.2 declares `key.debug.overlay` at `Options.java:698` with
  `Category.DEBUG` and dispatches it through `KeyMapping::matches` like anything
  else. Routing it through the table is vanilla-correct, not a divergence.
- **Escape genuinely is not a `KeyMapping`.** Vanilla handles it in
  `Screen`/`KeyboardHandler` and it cannot be rebound. We expose it as
  `key.lodestone.pause` so a Controls menu can show it. **Hazard:** it is the
  only gameplay route to the pause screen and so to Quit to Title; unbinding it
  leaves a session with no exit but the window close button. Nothing enforces
  that yet.
- **Menu navigation and text editing stay literal.** Arrow keys, Enter,
  Backspace and Delete in `menu_key_for`/`handle_chat_key` are not in the table,
  matching vanilla — those are `Screen`-level handling. The boundary is
  "gameplay and world bindings are rebindable; UI chrome is not".
- **Container shift-click and slot clicks stay literal too**, and vanilla agrees:
  it checks `Screen.hasShiftDown()` and raw button indices 0/1/2, not
  `KeyMapping`s. So rebinding sneak does *not* move shift-click.
- **Right-hand modifiers no longer alias.** The old hardcoded table bound sneak
  to *either* shift and sprint to *either* control. A `Binding` names one key,
  matching vanilla's `LEFT_SHIFT`/`LEFT_CONTROL` defaults. This is the one
  intentional behaviour change in the refactor — the right-hand keys are now
  rebindable rather than a silent alias.
- **No `Scroll` variant, checked rather than assumed.** Vanilla's
  `InputConstants.Type` is exactly `KEYSYM`/`SCANCODE`/`MOUSE`, so no vanilla
  binding can be a wheel direction. The wheel's one job here (cycling the hotbar)
  is handled outside the table in vanilla too. Adding one later is local: the
  persisted format is a string and unknown names already fall back to defaults.
- **`Keybinds` is `Copy` on purpose** — a fixed array, not a map — so `Options`
  stays `Copy` and the menu layer that reads it by value did not have to change.
  A `HashMap` here would have rippled outward for no benefit at 27 entries.
- **`Hotbar1` is slot `0`.** The off-by-one lives only in
  `InputAction::hotbar_slot`.
- **`window_event`'s effects `match` is covered by the compiler and by nothing
  else.** Measured while closing #385: no test in this crate constructs a
  `WindowApp` (it needs a window, a surface and a GPU), so every arm in that
  match — `Attack`, `Use`, `SelectSlot`, `SwapOffhand`, all of them — is unit-
  tested only up to `resolve_key` on one side and the method it calls on the
  other. **Deleting** an arm fails to compile, which is why `KeyOutcome` is a
  closed enum and the match is exhaustive; replacing one with `=> {}` would not
  be caught by anything. When adding an action, make the effect a *named method
  or function* the arm merely calls (see `offhand_swap_action`), so the part
  worth testing sits somewhere a test can reach it.

## Deliberately out of scope: touchscreen mode and gamepads (issue #219)

Neither exists here, and the absence is a decision rather than a gap. Recorded so a
future reader does not read "no hits for `gamepad`" as an oversight and start building.

- **Touchscreen mode** (`Options.touchscreen` in vanilla) changes hit-target sizing
  and a couple of tooltip/long-press behaviours that only matter without a mouse.
  Worth a settings checkbox for completeness someday; not worth input engineering.
- **Gamepads** are a materially larger effort — a full analog mapping layer parallel
  to keyboard-and-mouse — and vanilla *desktop* Minecraft does not ship it at all, so
  there is no `Options.java` behaviour to be faithful to.

The reason this is one entry rather than two: `docs/baritone-port.md` §3.7 already
wants **an analog movement-intent injection point**, for an unrelated reason — a
pathfinder needs finer control than the ±1.0 axes `InputState` exposes. A human with
a gamepad and a bot with a path want the *same* missing seam. So if this is ever
built, build that seam once and let both consume it; building gamepad support on its
own would produce a second analog path and leave the nav plugin still asking for the
first.

Revisit if a real request arrives, jointly with that seam.

## Dependencies

- `winit` — `KeyCode` (physical key identity) and `MouseButton`
- `serde_json` — the persisted representation
- `lodestone-controller` — `Action`, the movement seam that owns
  double-tap-to-sprint timing. The shell's only job is to call
  `InputState::set(action, pressed)` once per real press and release; the timing
  is fixed-tick and lives there (see [`swimming.md`](./swimming.md))
- `crate::config::Options` — persistence
- `crate::menu::UiState` — the four booleans `KeyGate` is built from
