# Keybindings and input options

## What it is

The rebindable action table mapping logical actions (`key.forward`, `key.inventory`) to physical inputs (a key or mouse button), so nothing in gameplay input names a key literally, plus the input-feel options on top: mouse and wheel sensitivity, axis inversion, hold-versus-toggle sneak and sprint, and the sprint food gate.

## How it works

### Binding model

`InputAction` (closed set of actions), `Binding` (key, mouse button or unbound) and `Keybinds` (the table, plus queries for the Key Binds screen: category grouping, conflicts, default-ness). Dispatch asks the table whether an action's binding matches an event; it never compares raw key codes, and nothing consults the name table at runtime.

"What does this keypress mean now" is one pure function of the table, context flags (menu, chat, container, gameplay active) and the key, so the swallowing order is unit-testable without a window. Order: a menu swallows the whole keyboard (text fields need every printable key), chat second (typing `w` must not move the player), an open container swallows every unrecognised key before gameplay. Escape is handled above the container layer so it closes a container through the normal pause-adjacent path.

Opening the inventory (`key.inventory`) or any server or plugin container releases pointer capture and focuses the pointer at the integer centre of the physical framebuffer. The windowed path moves the native pointer and shared hit-test cursor once on that open edge, and later movement is authoritative. The terminal surface applies the same shared-cursor centre so the first slot hover does not depend on the last gameplay cell.

Default bindings, names and categories come from the reference client, including category order, which is registration order, not alphabetical (Misc sorts second, ahead of Multiplayer, Gameplay and Inventory). The Multiplayer category includes `key.friends` (default `O`), active only in gameplay, which opens the Friends screen as a world overlay through the pause-origin route (Done or Escape returns to the paused world), with menu, chat and container focus still winning.

### Context-dependent actions

Drop, pick-item and swap-offhand are two mechanisms chosen by whether a container screen is open: a container press becomes a container click (predicted locally, corrected by the server), a no-screen press sends a distinct gameplay packet with its own semantics and sometimes no local prediction, matching the reference's asymmetry (do not "fix" it to always predict). State guards (empty cursor, hovered slot, spectator) belong where the effect is applied, not in the pure key-resolution function; only a modifier read from held keys (Ctrl, Shift) belongs inside it.

Gameplay middle-click is a one-shot pick. If the first click must also acquire pointer capture, the window keeps the pick up to 500 ms and dispatches once capture and a ray target exist; a failed grab, focus or menu transition, or expiry drops it, so the first left or right click stays capture-only. The terminal surface has no capture boundary and routes directly to `Sim::pick_block_or_entity`.

### F3 debug chords

F3 plus a letter (B hitboxes, G chunk borders, and others) are real rebindable actions in the current version and appear in the Debug category; F3 itself is a gate flag, not a bindable action. Successful chords print the coloured `[Debug]:` chat line; a couple are silent because the reference only reports their failure path (this client has no permission model to trigger it). Chords needing renderer or filesystem internals are absent by decision.

### Rebinding

Starting a capture (clicking a bind row) is ordinary menu input. Finishing needs the next raw key or mouse event, which must be intercepted before ordinary menu key translation, because that translation drops keys with no printable text (function keys, modifiers), exactly what rebind targets often are. Mouse buttons are likewise checked before the row-click handler consumes them as "select row". Escape cancels, and Pause cannot be unbound (no way back to the title screen otherwise).

A rebind must be read from the same live table dispatch consults, not a startup copy; a resolver with its own copy yields "the screen says it worked and the old key still works until next launch". Keep one live source of truth.

### Input options

- **Sprint food gate**: sprinting needs food strictly above a fixed threshold or fly-anywhere abilities; absent food or ability data (before the server reports) means sprinting allowed.
- **Toggle sneak and sprint**: hold (default) or toggle. Toggle flips on a fresh press edge and ignores release; everything downstream (movement, double-tap sprint) reads the same effective-held value. The mode setting survives an input reset (focus loss); the momentary state does not.
- **Sensitivity and inversion** apply before the shared look curve (negating before or after is equivalent). Wheel sensitivity is a fractional multiplier applied before any all-or-nothing rounding. Pixel scrolls (server list) consume fractions immediately; discrete-slot scrolls (hotbar) accumulate fractional notches until a whole step is due. Discrete scrolling takes the delta's sign before sensitivity scales it (the other order caps speed at one notch).
- Two mouse options stay unwired because there is no subsystem to gate: the shell never changes the OS cursor and has no raw-input capture mode.

## How to change it

- A new action is a table entry: a variant, its slot in the action list (the table indexes by discriminant, so position matters), name, category and default arms, and a dispatch branch with a real effect (otherwise it is a settings row that does nothing).
- Physical key identity, not the typed character, is stored; labels are layout-independent (a user may see "W" on another physical print).
- Menu navigation, text editing and container click semantics stay hardcoded as in the reference (rebinding sneak does not change shift-click). There is no scroll-wheel binding type.
- Keep the table a small `Copy` value, not a map.
- Factor dispatch effects that need a live window or session into small named functions the match calls; the match itself is checked only by exhaustiveness.
- Touchscreen and gamepad are out of scope; any future gamepad layer should share an injection seam with other analog-movement needs.

## Configuration

Persisted in the options file as a flat action-name to binding-name mapping in the reference's vocabulary. Only non-default bindings are written, so a changed default reaches existing users. Loading never hard-fails: an unknown action, malformed binding or non-object value falls back for that entry only. The input-feel options (toggle modes, invert flags, wheel sensitivity) live in the same file and are omitted at their defaults.

## Dependencies

- `crates/lodestone-shell/src/keybinds.rs` (the table) and `menu/key_binds.rs` (the screen; see [menu screens](menu-screens.md)).
- `crates/lodestone-controller` (platform-independent `InputState`, toggle, invert and sensitivity handling, shared with the browser).
- `lodestone-ecs::session` (vitals and abilities for the sprint gate).
