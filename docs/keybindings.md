# Keybindings and input options

## What it is

The rebindable action table that maps logical actions (`key.forward`, `key.inventory`) to physical
inputs (a keyboard key or mouse button) so nothing in the gameplay input path names a key literally,
plus the small cluster of input-feel options built on top of it — mouse sensitivity, wheel sensitivity,
axis inversion, hold-vs-toggle sneak/sprint, and the sprint food gate.

## How it works

### The binding model

Three types hold the whole thing: `InputAction` (the closed set of things a player can ask for),
`Binding` (a key, a mouse button, or unbound), and `Keybinds` (the table joining them, plus the queries
the Key Binds settings screen needs — grouping by category, conflict detection, default-ness). Dispatch
always asks the table for whether an action's binding matches an event; it never compares a raw key
code directly, so matching costs a cheap equality test and nothing has to consult the human-readable
name table at runtime.

The entire precedence decision for "what does this keypress mean right now" is a single pure function
of the table, a small set of context flags (is a menu open, is chat open, is a container open, is
gameplay active), and the key itself — deliberately extracted this way so the whole swallowing order is
unit-testable with no window, GPU, or live session. The order matters and is not arbitrary: a menu
screen swallows the entire keyboard first (a text field needs every printable key), chat swallows second
(so typing `w` in chat doesn't move the player), and an open container swallows every unrecognized key
before gameplay ever sees it (so nothing leaks through behind an inventory). Escape is handled above the
container layer specifically so it closes a container through the normal pause-adjacent path rather than
a container-specific escape case.

Default bindings, action names, and category grouping are all sourced directly from the vanilla client
rather than guessed — including category sort order, which is vanilla's own *registration* order and is
not alphabetical (a commonly-mis-guessed detail: the Misc category sorts second, ahead of Multiplayer,
Gameplay and Inventory).

### One action can mean two different things depending on context

Several actions (drop, pick-item, swap-offhand) are not one mechanism but two, selected by whether a
container screen is open — a container-open press becomes a container click (predicted locally,
corrected by the server on mismatch), while a no-screen press sends a distinct gameplay packet with its
own semantics and, in at least one case, no local prediction at all, matching vanilla exactly (vanilla
predicts some client-side effects and not others, so parity means reproducing that asymmetry rather than
"fixing" it to always predict). The general shape worth keeping in mind for any future action like this:
the two mechanisms share only the key, dispatch to genuinely different outcomes, and any state-based
guard (an empty cursor, a hovered slot, whether the player is a spectator) belongs at the point that
actually applies the effect, not inside the pure key-resolution function — that function only knows
about keys, not about session state. A modifier read off currently-held keys (Ctrl, Shift) is the one
exception that does belong inside the resolution function, since it's tracked key state rather than
session state.

### F3 debug chords

The F3 modifier plus a letter (B for hitboxes, G for chunk borders, and others) are real, rebindable
actions in the current vanilla version — not hardcoded, contrary to how older Minecraft versions worked
— and appear in the Key Binds screen's own Debug category. The F3 key itself acts as a gate flag rather
than an eighth bindable action, mirroring vanilla's own modifier-plus-overlay-share-a-key bookkeeping.
Successful chords print vanilla's own colored `[Debug]:` chat feedback line; a couple of chords are
silent on success because vanilla itself only reports their *failure* path, which this client has no
permission model to trigger yet, so there's honestly nothing to report on success either. A handful of
vanilla's debug chords are not implemented at all because they need renderer or filesystem internals
outside this input layer's scope, and are recorded as absent-by-decision rather than left to be
rediscovered as a gap.

### Rebinding through the Key Binds screen

Starting a capture (clicking a bind row) is ordinary menu input with no side effect on the binding
table. Finishing one needs the *next* raw physical key or mouse event, which is a genuinely different
code path from ordinary menu key handling: menu navigation silently drops any key with no printable
text (function keys, modifiers, most non-arrow keys), which is exactly the kind of key a real rebind
target often is, so a capture-in-progress has to intercept the raw event *before* it reaches ordinary
menu key translation, not only when that translation fails to produce anything. The same applies to
mouse buttons — a capture needs any button, not just left-click, and must be checked before an ordinary
click-on-row handler would otherwise consume the same click as "select this row" rather than "bind this
button." Escape cancels a capture without changing the binding, and one particular action (Pause) is
protected against being bound to nothing at all, since an unbound pause key would leave a session with
no way back to the title screen short of closing the window.

A rebind must be read from the same live table dispatch actually consults, not a table copied out at
startup — persisting the new binding to disk and to the settings screen's own display is not the same
as the resolver seeing it. A resolver holding its own independent copy of the table is the shape that
produces "the rebind screen says it worked, and the old key still does the old thing until next
launch," and the fix is to have exactly one live source of truth with no cache to go stale.

### Input options

- **Sprint food gate**: sprinting is only possible above a fixed food-level threshold, or when the
  player has fly-anywhere abilities — matching vanilla's own strict (not inclusive) cutoff. Absence of
  food/ability data (before it's been reported by the server) resolves to "sprinting allowed," not to
  "no food."
- **Toggle sneak/sprint**: either key can be hold-to-activate (default) or press-to-toggle. In toggle
  mode, the effective state only flips on a fresh press edge and a release does nothing — everything
  downstream of the raw key state (movement math, double-tap-sprint detection) reads the same "is this
  effectively held" value either way, so toggle mode is invisible to every consumer except the input
  model itself. The toggle-mode *setting* survives a full input reset (e.g. losing window focus), even
  though the momentary held/toggled state does not.
- **Mouse sensitivity, wheel sensitivity, and axis inversion**: sensitivity and both invert flags are
  applied before the same look-curve math general mouse movement already uses; the sign of an inverted
  delta doesn't interact with that curve, so negating before or after produces the same result. Wheel
  sensitivity is a fractional multiplier applied to scroll deltas *before* any all-or-nothing rounding —
  applying a threshold or an integer step first would silently break sub-1.0 and above-1.0 sensitivity
  alike. A server-list-style scroll (which moves by real pixels) and a hotbar-style scroll (which moves
  by discrete slots) need different accumulation strategies for exactly this reason: a pixel scroll can
  usefully consume a fraction of a notch immediately, while a discrete-slot scroll has to accumulate
  fractional notches until a whole step is due.
- **Discrete (non-smooth) scrolling** takes the sign of the delta before sensitivity scales it, not
  after — scaling first and then taking the sign would cap effective wheel speed at one notch regardless
  of the sensitivity setting.
- Two mouse-related options remain intentionally unwired because there is no underlying subsystem to
  gate, not merely a missing wire: this shell never changes the OS cursor and has no raw-input capture
  mode, so those two settings would be pure decoration if turned on.

## How to change it

- **Adding a new action is a table entry, not a structural change** — a variant, its slot in the
  action list (position matters; the table indexes by discriminant), its name/category/default arms,
  and a dispatch branch with an actual effect. An action wired everywhere except the dispatch effect is
  a settings-screen row that does nothing — the same island shape called out elsewhere in this repo.
- **Physical key identity, not the character it types, is what's stored.** This is right for movement
  keys (WASD is a shape under the left hand regardless of keyboard layout) but means a bound key's
  display label is layout-independent too — expect "W" as a label even for a user whose physical key
  prints something else.
- **Menu navigation, text editing, and container mouse-click semantics stay hardcoded, matching
  vanilla.** Vanilla itself does not route arrow keys, Enter, shift-click, or raw mouse-button-to-click-
  type mapping through its rebindable table, so neither does this client — rebinding sneak does not
  change what shift-click does, and that is correct, not a gap.
- **There is no scroll-wheel binding type.** A wheel notch's one rebindable-adjacent behavior (hotbar
  cycling) is handled outside the binding table in vanilla too.
- **The binding table is a small `Copy` value, not a map** — keep it that way; a heap-allocated
  structure here would ripple outward through every place that currently reads it by value for no
  benefit at this size.
- **A dispatch effect that has no unit test coverage of its own (because it needs a live window/GPU/
  session) should still be factored into a small named function the dispatch match merely calls** — the
  match itself is only checked by the compiler's exhaustiveness requirement, not by any test, so the
  part worth testing needs to live somewhere reachable.
- **Touchscreen and gamepad input are deliberately out of scope**, not an oversight — gamepad support in
  particular is a materially larger analog-input layer that vanilla desktop doesn't even have, and any
  future work here should share an injection seam with other analog-movement needs rather than building
  a second parallel path.

## Configuration

Persisted inside the options file as a flat action-name-to-binding-name mapping, using vanilla's own
naming vocabulary on both sides so a value is meaningful to a human reading the file directly. Only
non-default bindings are written, so a default changed later actually reaches existing users instead of
being pinned forever by a value their file happened to record at save time. Loading never hard-fails —
an unknown action name, a malformed binding, or a non-object value all fall back to defaults for just
that entry, so one bad line can't silently discard everything after it. The input-feel options (toggle
modes, invert flags, wheel sensitivity) live in the same file, each defaulting to vanilla's own default
value and omitted from the file entirely when left at that default.

## Dependencies

- `crates/lodestone-shell/src/keybinds.rs` — the action/binding table itself.
- `crates/lodestone-shell/src/menu/key_binds.rs` — the Key Binds settings screen (see
  [`menu-screens.md`](./menu-screens.md) for where it sits among the other settings pages).
- `crates/lodestone-controller` — the platform-independent input model (`InputState`, toggle/invert/
  sensitivity handling) shared with the browser client.
- `lodestone-ecs::session` — server-reported vitals/abilities data the sprint food gate reads.
- The 26.2 jar under `.cache/mc/26.2/client-src` — behavioral reference only, never transliterated.
