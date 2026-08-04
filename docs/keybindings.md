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

**No Controls menu UI exists yet.** This is the layer and the persistence only.

## How it works

### Dispatch is on actions, not keys

`app.rs` asks the table, never a `KeyCode`:

```rust
} else if binds.is(InputAction::DebugOverlay, code) && pressed {
```

`Keybinds::is` is a `Binding` equality test — it does not consult the name table,
so matching a key costs nothing per event.

### The precedence chain is a pure function

`app::resolve_key(binds, gate, code, pressed) -> Option<KeyOutcome>` holds the
entire decision. `KeyGate` is the four booleans it reads off `UiState`
(`menu`, `chat_open`, `container_open`, `gameplay`); `KeyOutcome` is the one side
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
not implement (`key.drop`, `key.pickItem`, `key.screenshot`) are absent rather
than listed and dead.

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

### Wiring the Controls menu

`MenuNav` already owns the loaded `Options` and the path to persist them, so the
writer belongs there. `app.rs`'s `keybinds` field currently loads once at
construction; point it at nav instead. In
`crates/lodestone-shell/src/menu/nav.rs`, after `options_save_error` (~`:503`):

```rust
#[must_use]
pub fn keybinds(&self) -> &crate::keybinds::Keybinds {
    &self.options.keybinds
}

pub fn rebind(
    &mut self,
    action: crate::keybinds::InputAction,
    binding: crate::keybinds::Binding,
) {
    self.options.keybinds.set(action, binding);
    self.persist_options();   // saves on change, like gui_scale
}

pub fn reset_keybinds(&mut self) {
    self.options.keybinds.reset_all();
    self.persist_options();
}
```

Then `WindowApp::keybinds` becomes `*self.nav.keybinds()`, re-read per event.
Deliberately not done yet: `nav.rs` is a shared file and an accessor with no
caller is itself an island.

The menu's own needs are already queries, so it need not reach into internals:

- `Keybinds::in_category(c)` + `Category::SORT_ORDER` — grouping and order
- `Keybinds::is_default(a)` — whether to show a reset affordance
- `Keybinds::conflicts(a)` / `has_conflict(a)` — vanilla's duplicate highlight,
  answered symmetrically for both sides
- `Binding::label()` — a short button caption

To capture a rebind, take the `KeyCode` from a `KeyEvent`'s `physical_key` or the
`MouseButton` from a `MouseInput`, wrap it in `Binding`, and call `rebind`.

## Gotchas

- **Physical keys, not characters.** The identity is winit's `KeyCode` — the
  key's *position*. Right for movement (`WASD` is a shape under the left hand, so
  AZERTY gets `ZQSD` for free) but it means `Binding::label()` is
  layout-independent: it says "W" for the key an AZERTY user has marked Z.
  Vanilla has the same tension and resolves it the same way. Fixing the label
  means capturing `KeyEvent::logical_key` at rebind time and caching it; not done
  because there is no menu to show it. Text entry is unaffected — the chat prompt
  and address field already use `KeyEvent::text`.
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
  A `HashMap` here would have rippled outward for no benefit at 26 entries.
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
