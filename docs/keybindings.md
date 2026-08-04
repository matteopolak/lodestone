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
not implement (`key.pickItem`, `key.screenshot`) are absent rather than listed
and dead.

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

### Wiring the Controls menu — fully landed

Issue #15 landed. `crates/lodestone-shell/src/menu/key_binds.rs` is the screen;
`SettingsPage::KeyBinds` (`crates/lodestone-shell/src/menu/options.rs`) is
where it slots into the settings tree, reached from the Controls page's own
"Key Binds..." button. Per-row Reset, the footer's Reset Keys, viewing every
one of the 27 actions grouped by category in vanilla's registration order
(`Category::SORT_ORDER`, not `InputAction::ALL`'s declaration order — see that
module's own tests for the trap), and starting a rebind are all wired and
persist immediately, the same eager-persistence rule every other live row in
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
