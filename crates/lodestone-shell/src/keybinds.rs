//! The keybinding layer: a rebindable table mapping **logical actions** to
//! **physical inputs**, so no gameplay input in the shell names a key literally.
//!
//! ## What this is
//!
//! [`InputAction`] is the closed set of things the player can *ask for*
//! ("move forward", "open the inventory"). [`Binding`] is what they press to ask
//! (a keyboard key, a mouse button, or nothing). [`Keybinds`] is the table
//! joining the two, plus the queries a future Controls menu needs: grouping by
//! [`Category`], "is this the default?", and "what else is bound to this?".
//!
//! `app.rs` therefore dispatches on *actions*, not keys:
//!
//! ```ignore
//! } else if binds.is(InputAction::Inventory, code) && pressed {
//! ```
//!
//! ## Where the names and defaults come from
//!
//! Both are read out of the decompiled 26.2 client, **not** from memory:
//!
//! - Action names and categories: `Options.java:655-725`, which declares every
//!   `KeyMapping` with its name, GLFW keysym and `KeyMapping.Category`.
//! - Category order: `KeyMapping.java:203-225`. `Category.SORT_ORDER` is
//!   *registration* order, and it is **not** alphabetical or intuitive —
//!   `MISC` comes second, before `MULTIPLAYER`, `GAMEPLAY` and `INVENTORY`.
//!   [`Category::SORT_ORDER`] reproduces it because that is the order vanilla's
//!   Controls screen groups by.
//! - Persisted binding names: `InputConstants.java:342-478`'s `addKey` table
//!   (`key.keyboard.w`, `key.mouse.left`, `key.keyboard.left.shift`, …).
//! - The save-line shape: `Options.java:1618-1622` writes each mapping as
//!   `key_<name>` → `saveString()`.
//!
//! ## Deliberate divergences from vanilla, and why
//!
//! 1. **F3 *is* a real `KeyMapping` in 26.2.** This was worth checking rather
//!    than assuming: in older versions the debug keys were handled inline in
//!    `KeyboardHandler`'s debug path, but 26.2 declares
//!    `keyDebugOverlay = new KeyMapping("key.debug.overlay", KEYSYM, 292,
//!    Category.DEBUG, -2)` (`Options.java:698`) and `KeyboardHandler` dispatches
//!    it through `KeyMapping::matches` like any other binding
//!    (`KeyboardHandler.java:184-333`). So routing F3 through this table is
//!    vanilla-*correct*, not a divergence. [`Category::Debug`] exists for it.
//!
//! 2. **Escape is genuinely not a `KeyMapping`.** Vanilla handles it in
//!    `Screen`/`KeyboardHandler` directly, so it cannot be rebound there. We
//!    route it through the table as [`InputAction::Pause`] because the user
//!    asked for nothing hardcoded and a Controls menu should be able to display
//!    it. **Hazard:** rebinding it away leaves no gameplay route to the pause
//!    screen — see the note on [`InputAction::Pause`].
//!
//! 3. **Menu navigation and text editing stay literal.** Arrow keys, Enter,
//!    Backspace and Delete in `app.rs`'s `menu_key_for`/`handle_chat_key` are
//!    *not* in this table, matching vanilla: those are `Screen`-level keyboard
//!    handling, not `KeyMapping`s, and a rebindable "move the menu cursor down"
//!    is not a thing vanilla's Controls screen offers either. The boundary is
//!    "gameplay and world bindings are rebindable; UI chrome is not".
//!
//! 4. **[`InputAction::ToggleFly`] is ours.** Vanilla has no fly-toggle binding
//!    (creative flight is a double-tap of jump), so it is namespaced
//!    `key.lodestone.toggleFly` rather than squatting on a vanilla name. `F` in
//!    vanilla is `key.swapOffhand`, which this client does not implement.
//!
//!    **That last sentence is now the only thing keeping `F` here, and it is
//!    load-bearing rather than incidental** (issue #378 part 3). The container
//!    half of `key.swapOffhand` — `ContainerInput::SWAP` with button `40`
//!    against the hovered slot, `AbstractContainerScreen.java:506-522` — is now
//!    implemented on the machine side and reachable from
//!    `app.rs`'s `KeyOutcome::ContainerSwap`; the number keys `1`–`9` route to it
//!    already. The **off-hand key does not**, and the blocker is this table, not
//!    the click path: adding `SwapOffhand` with vanilla's default of `F`
//!    (`Options.java:663`, GLFW keysym 70) collides with `ToggleFly` and turns
//!    `a_conflict_is_reported_for_both_actions_and_only_for_them` red, which is
//!    that test doing its job. Landing the off-hand key needs a **decision**
//!    first, not more code:
//!
//!    * move `key.lodestone.toggleFly` off `F` (restores vanilla parity, changes
//!      a default a player is using today), or
//!    * ship `key.swapOffhand` as [`Binding::Unbound`] and let the player bind it
//!      (no default collision, but the feature is unreachable out of the box).
//!
//!    Whichever is chosen, note the *gameplay* half of vanilla's `F` — the
//!    `ServerboundPlayerActionPacket SWAP_ITEM_WITH_OFFHAND` that swaps hands in
//!    the world — is a separate serverbound action this client does not send at
//!    all, so a binding added for the container half only is half a feature.
//!
//! ## Physical keys, and what that costs on non-QWERTY layouts
//!
//! The keyboard identity is winit's **physical** [`KeyCode`], i.e. the key's
//! position on the board rather than the character it produces. That matches
//! what the shell already did and is the right default for movement: `WASD` is
//! chosen as a *shape* under the left hand, so on an AZERTY board the same
//! physical cluster keeps working and reads `ZQSD` — which is what a French
//! player wants and what they would otherwise have to rebind by hand.
//!
//! The cost is that a binding's *label* is layout-independent: this table calls
//! the key left of `S` "w" regardless of what is printed on it, so a Controls
//! menu built on [`Binding::label`] will show "W" to an AZERTY user pressing the
//! key marked Z. Vanilla has the same tension and resolves it the same way
//! (`InputConstants.Type.KEYSYM` is GLFW's layout-independent keysym). Fixing
//! the *label* means asking the platform for the layout-dependent character for
//! a physical key at display time — winit exposes that as
//! `KeyEvent::logical_key`/`text`, but only on a real event, so a Controls menu
//! can capture it while rebinding and cache it. Deliberately not done here:
//! there is no menu to show it yet, and a cached label with nothing reading it
//! is dead weight.
//!
//! Text entry is unaffected — `menu_key_for` and `handle_chat_key` already use
//! `KeyEvent::text`, which is the composed character, so typing into the chat
//! prompt and the server-address field is correct on any layout.
//!
//! ## Configuration
//!
//! Persisted inside [`crate::config::Options`] (`options.json`) under
//! `"keybinds"`, as a flat `action name → binding name` object. See
//! [`Keybinds::to_json_value`] for the format and why only non-default entries
//! are written.

use std::fmt;

use winit::event::MouseButton;
use winit::keyboard::KeyCode;

use lodestone_controller::Action;

// ---------------------------------------------------------------------------
// Categories
// ---------------------------------------------------------------------------

/// The group a binding is listed under in a Controls menu.
///
/// Mirrors `KeyMapping.Category` (`KeyMapping.java:204-211`). All eight vanilla
/// categories are present even though this client only populates six, so that
/// adding (say) a creative hotbar-save binding later does not also require
/// touching this enum and the menu's grouping at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Movement,
    Misc,
    Multiplayer,
    Gameplay,
    Inventory,
    Creative,
    Spectator,
    Debug,
}

impl Category {
    /// Vanilla's `Category.SORT_ORDER` — **registration** order from
    /// `KeyMapping.java:204-211`, not alphabetical and not the order a reader
    /// would guess. `Misc` really is second. A Controls menu should walk this
    /// rather than sorting the enum.
    pub const SORT_ORDER: [Category; 8] = [
        Category::Movement,
        Category::Misc,
        Category::Multiplayer,
        Category::Gameplay,
        Category::Inventory,
        Category::Creative,
        Category::Spectator,
        Category::Debug,
    ];

    /// The category's vanilla identifier, as it appears in the translation key
    /// `key.categories.<id>` (`KeyMapping.java:227-229`).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Category::Movement => "movement",
            Category::Misc => "misc",
            Category::Multiplayer => "multiplayer",
            Category::Gameplay => "gameplay",
            Category::Inventory => "inventory",
            Category::Creative => "creative",
            Category::Spectator => "spectator",
            Category::Debug => "debug",
        }
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// A rebindable thing the player can ask for.
///
/// Every variant here has a real consumer in `app.rs` — vanilla mappings this
/// client does not implement (`key.drop`, `key.swapOffhand`, `key.pickItem`,
/// `key.screenshot`, `key.advancements`, …) are deliberately **absent** rather
/// than listed and dead. Adding one is a two-line change here plus the branch
/// that consumes it; adding one *without* the branch is the island defect
/// `CLAUDE.md` §1 is about, and a Controls menu offering a binding that does
/// nothing is exactly how that looks to a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InputAction {
    // -- movement ---------------------------------------------------------
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Sneak,
    Sprint,
    // -- gameplay ---------------------------------------------------------
    /// Mine / hit. Bound to a **mouse button** by default, which is the whole
    /// reason [`Binding`] is not a `KeyCode` newtype.
    Attack,
    /// Use / place.
    Use,
    // -- inventory --------------------------------------------------------
    Inventory,
    Hotbar1,
    Hotbar2,
    Hotbar3,
    Hotbar4,
    Hotbar5,
    Hotbar6,
    Hotbar7,
    Hotbar8,
    Hotbar9,
    // -- multiplayer ------------------------------------------------------
    Chat,
    /// Opens chat pre-filled with `/`. Vanilla's `key.command`.
    Command,
    /// Hold to show the player list.
    PlayerList,
    // -- misc -------------------------------------------------------------
    TogglePerspective,
    /// Lodestone-only; see the module docs. Vanilla has no fly-toggle binding.
    ToggleFly,
    /// Open the pause screen, or close an open container.
    ///
    /// **Not a vanilla `KeyMapping`** — vanilla handles Escape in `Screen` /
    /// `KeyboardHandler` and it cannot be rebound. Routed through the table
    /// here so a Controls menu can show it.
    ///
    /// **Hazard:** this is the only gameplay route to the pause screen (and so
    /// to Quit to Title). Rebinding it to something unreachable, or unbinding
    /// it, leaves a session with no way out but the window's close button. A
    /// Controls menu should refuse to leave this [`Binding::Unbound`]; nothing
    /// enforces that yet because there is no menu to enforce it in.
    Pause,
    // -- debug ------------------------------------------------------------
    /// The F3 overlay. A genuine vanilla `KeyMapping` in 26.2 — see the module
    /// docs, which is not what older versions did.
    DebugOverlay,
}

impl InputAction {
    /// Every action, in declaration order. Declaration order is grouped by
    /// category and, within a category, follows `Options.java`'s own
    /// declaration order — so walking `ALL` filtered by [`Category::SORT_ORDER`]
    /// reproduces vanilla's Controls-screen ordering without a sort.
    pub const ALL: [InputAction; 26] = [
        InputAction::Forward,
        InputAction::Back,
        InputAction::Left,
        InputAction::Right,
        InputAction::Jump,
        InputAction::Sneak,
        InputAction::Sprint,
        InputAction::Attack,
        InputAction::Use,
        InputAction::Inventory,
        InputAction::Hotbar1,
        InputAction::Hotbar2,
        InputAction::Hotbar3,
        InputAction::Hotbar4,
        InputAction::Hotbar5,
        InputAction::Hotbar6,
        InputAction::Hotbar7,
        InputAction::Hotbar8,
        InputAction::Hotbar9,
        InputAction::Chat,
        InputAction::Command,
        InputAction::PlayerList,
        InputAction::TogglePerspective,
        InputAction::ToggleFly,
        InputAction::Pause,
        InputAction::DebugOverlay,
    ];

    /// The stable identifier used in `options.json` and as the translation key,
    /// matching vanilla's `KeyMapping::getName` where a counterpart exists.
    ///
    /// Lodestone-only actions are namespaced `key.lodestone.*` so they can never
    /// collide with a vanilla name we later want.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            InputAction::Forward => "key.forward",
            InputAction::Back => "key.back",
            InputAction::Left => "key.left",
            InputAction::Right => "key.right",
            InputAction::Jump => "key.jump",
            InputAction::Sneak => "key.sneak",
            InputAction::Sprint => "key.sprint",
            InputAction::Attack => "key.attack",
            InputAction::Use => "key.use",
            InputAction::Inventory => "key.inventory",
            InputAction::Hotbar1 => "key.hotbar.1",
            InputAction::Hotbar2 => "key.hotbar.2",
            InputAction::Hotbar3 => "key.hotbar.3",
            InputAction::Hotbar4 => "key.hotbar.4",
            InputAction::Hotbar5 => "key.hotbar.5",
            InputAction::Hotbar6 => "key.hotbar.6",
            InputAction::Hotbar7 => "key.hotbar.7",
            InputAction::Hotbar8 => "key.hotbar.8",
            InputAction::Hotbar9 => "key.hotbar.9",
            InputAction::Chat => "key.chat",
            InputAction::Command => "key.command",
            InputAction::PlayerList => "key.playerlist",
            InputAction::TogglePerspective => "key.togglePerspective",
            InputAction::ToggleFly => "key.lodestone.toggleFly",
            InputAction::Pause => "key.lodestone.pause",
            InputAction::DebugOverlay => "key.debug.overlay",
        }
    }

    /// Resolve a persisted action name. `None` for anything unrecognised, which
    /// the loader treats as "skip this line" rather than an error — see
    /// [`Keybinds::from_json_value`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        InputAction::ALL.into_iter().find(|a| a.name() == name)
    }

    /// The Controls-menu group. Matches the `KeyMapping.Category` vanilla gives
    /// the corresponding mapping (`Options.java:655-725`).
    #[must_use]
    pub fn category(self) -> Category {
        match self {
            InputAction::Forward
            | InputAction::Back
            | InputAction::Left
            | InputAction::Right
            | InputAction::Jump
            | InputAction::Sneak
            | InputAction::Sprint => Category::Movement,
            InputAction::Attack | InputAction::Use => Category::Gameplay,
            InputAction::Inventory
            | InputAction::Hotbar1
            | InputAction::Hotbar2
            | InputAction::Hotbar3
            | InputAction::Hotbar4
            | InputAction::Hotbar5
            | InputAction::Hotbar6
            | InputAction::Hotbar7
            | InputAction::Hotbar8
            | InputAction::Hotbar9 => Category::Inventory,
            InputAction::Chat | InputAction::Command | InputAction::PlayerList => {
                Category::Multiplayer
            }
            InputAction::TogglePerspective | InputAction::ToggleFly | InputAction::Pause => {
                Category::Misc
            }
            InputAction::DebugOverlay => Category::Debug,
        }
    }

    /// Vanilla's default binding.
    ///
    /// Every keyboard default is the winit [`KeyCode`] for the GLFW keysym
    /// `Options.java` declares, and every mouse default is the
    /// `InputConstants.Type.MOUSE` button index it declares. The GLFW number is
    /// named in each comment so the mapping is checkable against the source
    /// without trusting this table.
    #[must_use]
    pub fn default_binding(self) -> Binding {
        match self {
            // `Options.java:655-661` — 87/83/65/68/32/340/341.
            InputAction::Forward => Binding::Key(KeyCode::KeyW),
            InputAction::Back => Binding::Key(KeyCode::KeyS),
            InputAction::Left => Binding::Key(KeyCode::KeyA),
            InputAction::Right => Binding::Key(KeyCode::KeyD),
            InputAction::Jump => Binding::Key(KeyCode::Space),
            InputAction::Sneak => Binding::Key(KeyCode::ShiftLeft),
            InputAction::Sprint => Binding::Key(KeyCode::ControlLeft),
            // `Options.java:665-668` — `Type.MOUSE` 0 and 1. Note the *order*
            // in the source is `keyUse` (button 1) then `keyAttack` (button 0).
            InputAction::Attack => Binding::Mouse(MouseButton::Left),
            InputAction::Use => Binding::Mouse(MouseButton::Right),
            // `Options.java:662` — 69.
            InputAction::Inventory => Binding::Key(KeyCode::KeyE),
            // `Options.java:684-692` — 49..57, i.e. the number row, not the keypad.
            InputAction::Hotbar1 => Binding::Key(KeyCode::Digit1),
            InputAction::Hotbar2 => Binding::Key(KeyCode::Digit2),
            InputAction::Hotbar3 => Binding::Key(KeyCode::Digit3),
            InputAction::Hotbar4 => Binding::Key(KeyCode::Digit4),
            InputAction::Hotbar5 => Binding::Key(KeyCode::Digit5),
            InputAction::Hotbar6 => Binding::Key(KeyCode::Digit6),
            InputAction::Hotbar7 => Binding::Key(KeyCode::Digit7),
            InputAction::Hotbar8 => Binding::Key(KeyCode::Digit8),
            InputAction::Hotbar9 => Binding::Key(KeyCode::Digit9),
            // `Options.java:670-672` — 84/47/258.
            InputAction::Chat => Binding::Key(KeyCode::KeyT),
            InputAction::Command => Binding::Key(KeyCode::Slash),
            InputAction::PlayerList => Binding::Key(KeyCode::Tab),
            // `Options.java:676` — 294.
            InputAction::TogglePerspective => Binding::Key(KeyCode::F5),
            // No vanilla counterpart; this client's existing key.
            InputAction::ToggleFly => Binding::Key(KeyCode::KeyF),
            // No vanilla counterpart (Escape is not a `KeyMapping`); GLFW 256.
            InputAction::Pause => Binding::Key(KeyCode::Escape),
            // `Options.java:698` — 292.
            InputAction::DebugOverlay => Binding::Key(KeyCode::F3),
        }
    }

    /// The hotbar slot index `0..=8` this action selects, if it is a hotbar
    /// action. Keeps the `Hotbar1`→`0` off-by-one in exactly one place.
    #[must_use]
    pub fn hotbar_slot(self) -> Option<usize> {
        match self {
            InputAction::Hotbar1 => Some(0),
            InputAction::Hotbar2 => Some(1),
            InputAction::Hotbar3 => Some(2),
            InputAction::Hotbar4 => Some(3),
            InputAction::Hotbar5 => Some(4),
            InputAction::Hotbar6 => Some(5),
            InputAction::Hotbar7 => Some(6),
            InputAction::Hotbar8 => Some(7),
            InputAction::Hotbar9 => Some(8),
            _ => None,
        }
    }

    /// The controller-level movement [`Action`] this drives, if any.
    ///
    /// This is the seam onto `lodestone-controller`, which owns the
    /// double-tap-to-sprint timing: the shell's only job is to call
    /// `InputState::set(action, pressed)` once per real press and release, and
    /// this function is what decides *which* action that is.
    #[must_use]
    pub fn movement(self) -> Option<Action> {
        Some(match self {
            InputAction::Forward => Action::Forward,
            InputAction::Back => Action::Back,
            InputAction::Left => Action::Left,
            InputAction::Right => Action::Right,
            InputAction::Jump => Action::Jump,
            InputAction::Sneak => Action::Sneak,
            InputAction::Sprint => Action::Sprint,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

/// What the player presses to invoke an [`InputAction`].
///
/// Covers a keyboard key **and a mouse button**, because vanilla binds
/// `key.attack` and `key.use` to mouse buttons and both are rebindable — a
/// `KeyCode`-only table cannot express the default configuration, let alone a
/// rebind of it.
///
/// ## Why there is no `Scroll` variant
///
/// Checked rather than assumed. Vanilla's `InputConstants.Type` is exactly
/// `KEYSYM`, `SCANCODE` and `MOUSE` (`InputConstants.java:300-312`) — there is
/// no scroll type, so no vanilla `KeyMapping` can be bound to a wheel direction.
/// The one thing this client does with the wheel is cycle the hotbar, which
/// vanilla also handles outside the mapping table (in `MouseHandler`, not as a
/// `KeyMapping`), so nothing in the shell needs it. Adding a
/// `Scroll(ScrollDirection)` variant later is a local change: the persisted
/// format is a string, [`Binding::parse`] already returns `None` for names it
/// does not know, and unknown names fall back to the default rather than
/// failing the load.
///
/// `SCANCODE` is likewise absent: it is vanilla's fallback for keys GLFW cannot
/// name, and winit's [`KeyCode`] already *is* a physical-position identity, so
/// there is no second identity to fall back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    /// Bound to nothing. Vanilla's `InputConstants.UNKNOWN`; such a binding
    /// never matches any input.
    Unbound,
    /// A **physical** key position — see the module docs on non-QWERTY layouts.
    Key(KeyCode),
    Mouse(MouseButton),
}

impl Binding {
    /// The persisted name, in vanilla's `InputConstants` spelling where one
    /// exists (`key.keyboard.w`, `key.mouse.left`, `key.keyboard.left.shift`).
    ///
    /// Keys and buttons vanilla has no name for get a `…winit.*` name so they
    /// still round-trip rather than being silently dropped on save.
    #[must_use]
    pub fn name(self) -> String {
        match self {
            Binding::Unbound => "key.keyboard.unknown".to_string(),
            Binding::Key(code) => key_name(code).to_string(),
            Binding::Mouse(button) => mouse_name(button),
        }
    }

    /// Parse a persisted name. `None` for anything unrecognised — the loader
    /// turns that into "keep the default for this action", never an error.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        if name == "key.keyboard.unknown" {
            return Some(Binding::Unbound);
        }
        if let Some(code) = key_from_name(name) {
            return Some(Binding::Key(code));
        }
        mouse_from_name(name).map(Binding::Mouse)
    }

    /// A short human label for a Controls menu button ("W", "Left Button").
    ///
    /// Derived from the persisted name rather than carrying a second table, so
    /// the two can never disagree about which key this is. Note the layout
    /// caveat in the module docs: this is the *physical* key's conventional
    /// name, not the character an AZERTY user sees printed on it.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Binding::Unbound => "Not bound".to_string(),
            Binding::Mouse(MouseButton::Left) => "Left Button".to_string(),
            Binding::Mouse(MouseButton::Right) => "Right Button".to_string(),
            Binding::Mouse(MouseButton::Middle) => "Middle Button".to_string(),
            Binding::Mouse(_) => {
                let n = self.name();
                let tail = n.rsplit('.').next().unwrap_or(&n).to_string();
                format!("Button {tail}")
            }
            Binding::Key(_) => {
                let n = self.name();
                let tail = n.strip_prefix("key.keyboard.").unwrap_or(&n);
                // `left.shift` reads better as "Left Shift" than "left.shift".
                tail.split('.')
                    .map(|part| {
                        let mut chars = part.chars();
                        match chars.next() {
                            Some(first) => {
                                first.to_uppercase().collect::<String>() + chars.as_str()
                            }
                            None => String::new(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        }
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// winit [`KeyCode`] ↔ persisted-name table.
///
/// The vanilla-named entries are transcribed from `InputConstants.java:342-478`
/// verbatim; the `winit.*`-namespaced tail covers physical keys winit can report
/// that GLFW/vanilla has no name for, so that saving a binding to one is
/// lossless instead of quietly reverting on the next load.
///
/// One table serves both directions (see [`key_name`] / [`key_from_name`]) so a
/// name and its reverse cannot drift. Scanned linearly, which is fine: it is
/// touched only on save and load, never per keypress — matching a binding is a
/// [`Binding`] equality test that never consults this table.
const KEY_NAMES: &[(KeyCode, &str)] = &[
    // Number row.
    (KeyCode::Digit0, "key.keyboard.0"),
    (KeyCode::Digit1, "key.keyboard.1"),
    (KeyCode::Digit2, "key.keyboard.2"),
    (KeyCode::Digit3, "key.keyboard.3"),
    (KeyCode::Digit4, "key.keyboard.4"),
    (KeyCode::Digit5, "key.keyboard.5"),
    (KeyCode::Digit6, "key.keyboard.6"),
    (KeyCode::Digit7, "key.keyboard.7"),
    (KeyCode::Digit8, "key.keyboard.8"),
    (KeyCode::Digit9, "key.keyboard.9"),
    // Letters.
    (KeyCode::KeyA, "key.keyboard.a"),
    (KeyCode::KeyB, "key.keyboard.b"),
    (KeyCode::KeyC, "key.keyboard.c"),
    (KeyCode::KeyD, "key.keyboard.d"),
    (KeyCode::KeyE, "key.keyboard.e"),
    (KeyCode::KeyF, "key.keyboard.f"),
    (KeyCode::KeyG, "key.keyboard.g"),
    (KeyCode::KeyH, "key.keyboard.h"),
    (KeyCode::KeyI, "key.keyboard.i"),
    (KeyCode::KeyJ, "key.keyboard.j"),
    (KeyCode::KeyK, "key.keyboard.k"),
    (KeyCode::KeyL, "key.keyboard.l"),
    (KeyCode::KeyM, "key.keyboard.m"),
    (KeyCode::KeyN, "key.keyboard.n"),
    (KeyCode::KeyO, "key.keyboard.o"),
    (KeyCode::KeyP, "key.keyboard.p"),
    (KeyCode::KeyQ, "key.keyboard.q"),
    (KeyCode::KeyR, "key.keyboard.r"),
    (KeyCode::KeyS, "key.keyboard.s"),
    (KeyCode::KeyT, "key.keyboard.t"),
    (KeyCode::KeyU, "key.keyboard.u"),
    (KeyCode::KeyV, "key.keyboard.v"),
    (KeyCode::KeyW, "key.keyboard.w"),
    (KeyCode::KeyX, "key.keyboard.x"),
    (KeyCode::KeyY, "key.keyboard.y"),
    (KeyCode::KeyZ, "key.keyboard.z"),
    // Function row. Vanilla names f1..f25; winit reports up to F35, and the
    // tail is covered by the `winit.*` entries below.
    (KeyCode::F1, "key.keyboard.f1"),
    (KeyCode::F2, "key.keyboard.f2"),
    (KeyCode::F3, "key.keyboard.f3"),
    (KeyCode::F4, "key.keyboard.f4"),
    (KeyCode::F5, "key.keyboard.f5"),
    (KeyCode::F6, "key.keyboard.f6"),
    (KeyCode::F7, "key.keyboard.f7"),
    (KeyCode::F8, "key.keyboard.f8"),
    (KeyCode::F9, "key.keyboard.f9"),
    (KeyCode::F10, "key.keyboard.f10"),
    (KeyCode::F11, "key.keyboard.f11"),
    (KeyCode::F12, "key.keyboard.f12"),
    (KeyCode::F13, "key.keyboard.f13"),
    (KeyCode::F14, "key.keyboard.f14"),
    (KeyCode::F15, "key.keyboard.f15"),
    (KeyCode::F16, "key.keyboard.f16"),
    (KeyCode::F17, "key.keyboard.f17"),
    (KeyCode::F18, "key.keyboard.f18"),
    (KeyCode::F19, "key.keyboard.f19"),
    (KeyCode::F20, "key.keyboard.f20"),
    (KeyCode::F21, "key.keyboard.f21"),
    (KeyCode::F22, "key.keyboard.f22"),
    (KeyCode::F23, "key.keyboard.f23"),
    (KeyCode::F24, "key.keyboard.f24"),
    (KeyCode::F25, "key.keyboard.f25"),
    // Keypad.
    (KeyCode::NumLock, "key.keyboard.num.lock"),
    (KeyCode::Numpad0, "key.keyboard.keypad.0"),
    (KeyCode::Numpad1, "key.keyboard.keypad.1"),
    (KeyCode::Numpad2, "key.keyboard.keypad.2"),
    (KeyCode::Numpad3, "key.keyboard.keypad.3"),
    (KeyCode::Numpad4, "key.keyboard.keypad.4"),
    (KeyCode::Numpad5, "key.keyboard.keypad.5"),
    (KeyCode::Numpad6, "key.keyboard.keypad.6"),
    (KeyCode::Numpad7, "key.keyboard.keypad.7"),
    (KeyCode::Numpad8, "key.keyboard.keypad.8"),
    (KeyCode::Numpad9, "key.keyboard.keypad.9"),
    (KeyCode::NumpadAdd, "key.keyboard.keypad.add"),
    (KeyCode::NumpadDecimal, "key.keyboard.keypad.decimal"),
    (KeyCode::NumpadEnter, "key.keyboard.keypad.enter"),
    (KeyCode::NumpadEqual, "key.keyboard.keypad.equal"),
    (KeyCode::NumpadMultiply, "key.keyboard.keypad.multiply"),
    (KeyCode::NumpadDivide, "key.keyboard.keypad.divide"),
    (KeyCode::NumpadSubtract, "key.keyboard.keypad.subtract"),
    // Arrows.
    (KeyCode::ArrowDown, "key.keyboard.down"),
    (KeyCode::ArrowLeft, "key.keyboard.left"),
    (KeyCode::ArrowRight, "key.keyboard.right"),
    (KeyCode::ArrowUp, "key.keyboard.up"),
    // Punctuation. Note winit's `Quote`/`Backquote` are vanilla's
    // `apostrophe`/`grave.accent` — the names do not line up, which is exactly
    // why this is a table and not a `format!`.
    (KeyCode::Quote, "key.keyboard.apostrophe"),
    (KeyCode::Backslash, "key.keyboard.backslash"),
    (KeyCode::Comma, "key.keyboard.comma"),
    (KeyCode::Equal, "key.keyboard.equal"),
    (KeyCode::Backquote, "key.keyboard.grave.accent"),
    (KeyCode::BracketLeft, "key.keyboard.left.bracket"),
    (KeyCode::Minus, "key.keyboard.minus"),
    (KeyCode::Period, "key.keyboard.period"),
    (KeyCode::BracketRight, "key.keyboard.right.bracket"),
    (KeyCode::Semicolon, "key.keyboard.semicolon"),
    (KeyCode::Slash, "key.keyboard.slash"),
    (KeyCode::Space, "key.keyboard.space"),
    (KeyCode::Tab, "key.keyboard.tab"),
    // Modifiers. Vanilla says `win` where winit says `Super`.
    (KeyCode::AltLeft, "key.keyboard.left.alt"),
    (KeyCode::ControlLeft, "key.keyboard.left.control"),
    (KeyCode::ShiftLeft, "key.keyboard.left.shift"),
    (KeyCode::SuperLeft, "key.keyboard.left.win"),
    (KeyCode::AltRight, "key.keyboard.right.alt"),
    (KeyCode::ControlRight, "key.keyboard.right.control"),
    (KeyCode::ShiftRight, "key.keyboard.right.shift"),
    (KeyCode::SuperRight, "key.keyboard.right.win"),
    // Editing / navigation.
    (KeyCode::Enter, "key.keyboard.enter"),
    (KeyCode::Escape, "key.keyboard.escape"),
    (KeyCode::Backspace, "key.keyboard.backspace"),
    (KeyCode::Delete, "key.keyboard.delete"),
    (KeyCode::End, "key.keyboard.end"),
    (KeyCode::Home, "key.keyboard.home"),
    (KeyCode::Insert, "key.keyboard.insert"),
    (KeyCode::PageDown, "key.keyboard.page.down"),
    (KeyCode::PageUp, "key.keyboard.page.up"),
    (KeyCode::CapsLock, "key.keyboard.caps.lock"),
    (KeyCode::Pause, "key.keyboard.pause"),
    (KeyCode::ScrollLock, "key.keyboard.scroll.lock"),
    (KeyCode::ContextMenu, "key.keyboard.menu"),
    (KeyCode::PrintScreen, "key.keyboard.print.screen"),
    // -- keys vanilla has no name for -------------------------------------
    // Namespaced so a rebind to one survives a save/load cycle. Vanilla's
    // `world.1`/`world.2` (GLFW 161/162) are deliberately *not* claimed for the
    // `Intl*` keys: GLFW's WORLD_1/2 and winit's IntlBackslash/IntlRo/IntlYen
    // are not the same keys, and asserting an equivalence we have not measured
    // is how a binding ends up landing on the wrong physical key.
    (KeyCode::IntlBackslash, "key.keyboard.winit.intl.backslash"),
    (KeyCode::IntlRo, "key.keyboard.winit.intl.ro"),
    (KeyCode::IntlYen, "key.keyboard.winit.intl.yen"),
    (KeyCode::Fn, "key.keyboard.winit.fn"),
    (KeyCode::FnLock, "key.keyboard.winit.fn.lock"),
    (KeyCode::Help, "key.keyboard.winit.help"),
    (KeyCode::Convert, "key.keyboard.winit.convert"),
    (KeyCode::NonConvert, "key.keyboard.winit.non.convert"),
    (KeyCode::KanaMode, "key.keyboard.winit.kana.mode"),
    (KeyCode::NumpadComma, "key.keyboard.winit.keypad.comma"),
    (KeyCode::NumpadStar, "key.keyboard.winit.keypad.star"),
    (KeyCode::NumpadHash, "key.keyboard.winit.keypad.hash"),
    (KeyCode::NumpadBackspace, "key.keyboard.winit.keypad.backspace"),
    (KeyCode::NumpadClear, "key.keyboard.winit.keypad.clear"),
    (KeyCode::NumpadParenLeft, "key.keyboard.winit.keypad.paren.left"),
    (KeyCode::NumpadParenRight, "key.keyboard.winit.keypad.paren.right"),
    (KeyCode::F26, "key.keyboard.winit.f26"),
    (KeyCode::F27, "key.keyboard.winit.f27"),
    (KeyCode::F28, "key.keyboard.winit.f28"),
    (KeyCode::F29, "key.keyboard.winit.f29"),
    (KeyCode::F30, "key.keyboard.winit.f30"),
    (KeyCode::F31, "key.keyboard.winit.f31"),
    (KeyCode::F32, "key.keyboard.winit.f32"),
    (KeyCode::F33, "key.keyboard.winit.f33"),
    (KeyCode::F34, "key.keyboard.winit.f34"),
    (KeyCode::F35, "key.keyboard.winit.f35"),
];

/// The persisted name for a physical key.
///
/// Falls back to a `winit.unknown.*` name for a [`KeyCode`] not in
/// [`KEY_NAMES`] — winit's `KeyCode` is `#[non_exhaustive]`, so this cannot be
/// an exhaustive match and a future winit release can add variants. Such a name
/// does not parse back (there is no reverse entry), so it reverts to the default
/// on load: a lossy round-trip, but a *quiet, non-fatal* one, and adding the key
/// to the table above fixes it.
fn key_name(code: KeyCode) -> &'static str {
    match KEY_NAMES.iter().find(|(c, _)| *c == code) {
        Some((_, name)) => name,
        None => "key.keyboard.winit.unknown",
    }
}

/// Reverse of [`key_name`]. `None` for an unrecognised name.
fn key_from_name(name: &str) -> Option<KeyCode> {
    KEY_NAMES
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(code, _)| *code)
}

/// The persisted name for a mouse button.
///
/// Vanilla names buttons 0/1/2 `left`/`right`/`middle` and numbers the rest
/// `key.mouse.<n + 1>` (`InputConstants.java:343-351`), so winit's
/// `Other(3)` — the raw platform button index — is `key.mouse.4`, matching.
/// winit's named `Back`/`Forward` have no vanilla counterpart and no fixed
/// index, so they are namespaced rather than guessed onto 4/5.
fn mouse_name(button: MouseButton) -> String {
    match button {
        MouseButton::Left => "key.mouse.left".to_string(),
        MouseButton::Right => "key.mouse.right".to_string(),
        MouseButton::Middle => "key.mouse.middle".to_string(),
        MouseButton::Back => "key.mouse.winit.back".to_string(),
        MouseButton::Forward => "key.mouse.winit.forward".to_string(),
        MouseButton::Other(n) => format!("key.mouse.{}", u32::from(n) + 1),
    }
}

/// Reverse of [`mouse_name`]. `None` for an unrecognised name.
fn mouse_from_name(name: &str) -> Option<MouseButton> {
    match name {
        "key.mouse.left" => Some(MouseButton::Left),
        "key.mouse.right" => Some(MouseButton::Right),
        "key.mouse.middle" => Some(MouseButton::Middle),
        "key.mouse.winit.back" => Some(MouseButton::Back),
        "key.mouse.winit.forward" => Some(MouseButton::Forward),
        other => {
            let n: u32 = other.strip_prefix("key.mouse.")?.parse().ok()?;
            // Vanilla's numbering is 1-based, so `key.mouse.4` is button index
            // 3. `key.mouse.1`..`3` would be a second spelling of
            // left/right/middle, which are never emitted that way; reject them
            // rather than accepting two names for one button.
            if n < 4 {
                return None;
            }
            u16::try_from(n - 1).ok().map(MouseButton::Other)
        }
    }
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// The live action → binding table.
///
/// Stored as a fixed array indexed by `action as usize`, which keeps this type
/// `Copy` — that matters because it lives inside [`crate::config::Options`],
/// which is `Copy` and is read by value in a few places. A `HashMap` here would
/// have forced `Options` to stop being `Copy` and rippled into the menu layer
/// for no benefit at 26 entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keybinds {
    bindings: [Binding; InputAction::ALL.len()],
}

impl Default for Keybinds {
    fn default() -> Self {
        let mut bindings = [Binding::Unbound; InputAction::ALL.len()];
        for action in InputAction::ALL {
            bindings[action as usize] = action.default_binding();
        }
        Self { bindings }
    }
}

impl Keybinds {
    /// Vanilla's defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The binding currently assigned to `action`.
    #[must_use]
    pub fn binding(&self, action: InputAction) -> Binding {
        self.bindings[action as usize]
    }

    /// Assign a binding. A Controls menu's "press a key" flow ends here.
    pub fn set(&mut self, action: InputAction, binding: Binding) {
        self.bindings[action as usize] = binding;
    }

    /// Restore one action to vanilla's default.
    pub fn reset(&mut self, action: InputAction) {
        self.bindings[action as usize] = action.default_binding();
    }

    /// Restore every action (vanilla's "Reset All" button).
    pub fn reset_all(&mut self) {
        *self = Self::default();
    }

    /// Whether `action` still holds vanilla's default — vanilla's
    /// `KeyMapping::isDefault`, used to decide whether to show a reset affordance.
    #[must_use]
    pub fn is_default(&self, action: InputAction) -> bool {
        self.binding(action) == action.default_binding()
    }

    /// Whether `action` is bound to this physical key. **The dispatch predicate**
    /// `app.rs`'s key chain is built on.
    ///
    /// [`Binding::Unbound`] never matches, so unbinding an action really does
    /// disable it rather than making it fire on some arbitrary key.
    #[must_use]
    pub fn is(&self, action: InputAction, code: KeyCode) -> bool {
        self.binding(action) == Binding::Key(code)
    }

    /// Whether `action` is bound to this mouse button. The mouse-side twin of
    /// [`Keybinds::is`].
    #[must_use]
    pub fn is_mouse(&self, action: InputAction, button: MouseButton) -> bool {
        self.binding(action) == Binding::Mouse(button)
    }

    /// Every action bound to `binding`, in [`InputAction::ALL`] order.
    ///
    /// [`Binding::Unbound`] yields nothing: "everything that is bound to
    /// nothing" is not a conflict, and treating it as one would light up the
    /// whole unbound half of a Controls menu in red.
    pub fn actions_bound_to(&self, binding: Binding) -> impl Iterator<Item = InputAction> + '_ {
        InputAction::ALL
            .into_iter()
            .filter(move |a| binding != Binding::Unbound && self.binding(*a) == binding)
    }

    /// The *other* actions sharing `action`'s binding — vanilla's duplicate
    /// highlight in the Controls screen (`KeyMapping::same`).
    ///
    /// Exposed as a query so a menu never has to reach into [`Keybinds`]'s
    /// internals to compute it. Empty for an unbound action.
    pub fn conflicts(&self, action: InputAction) -> impl Iterator<Item = InputAction> + '_ {
        self.actions_bound_to(self.binding(action))
            .filter(move |a| *a != action)
    }

    /// Whether `action` shares its binding with any other action.
    #[must_use]
    pub fn has_conflict(&self, action: InputAction) -> bool {
        self.conflicts(action).next().is_some()
    }

    /// The actions in one category, in [`InputAction::ALL`] order.
    ///
    /// Walk [`Category::SORT_ORDER`] over this to lay out a Controls screen the
    /// way vanilla groups one.
    pub fn in_category(category: Category) -> impl Iterator<Item = InputAction> {
        InputAction::ALL
            .into_iter()
            .filter(move |a| a.category() == category)
    }

    // -- persistence ------------------------------------------------------

    /// The persisted form: a flat `action name → binding name` object.
    ///
    /// ## Why this shape
    ///
    /// - **Flat strings, not integers.** A binding written as
    ///   `"key.forward": "key.keyboard.w"` is meaningful to a human editing the
    ///   file and stable across winit or wgpu upgrades. A `KeyCode`'s numeric
    ///   discriminant is neither: winit gives no stability guarantee for it, so
    ///   an upgrade could silently move every binding to a different key.
    /// - **Vanilla's vocabulary.** Same names vanilla uses on both sides of the
    ///   colon, so anyone who has edited an `options.txt` can read this, and a
    ///   future importer of a real `options.txt` is a lookup rather than a
    ///   translation layer.
    /// - **Only non-default entries are written.** The file then says exactly
    ///   what the user changed, and — the reason that matters — a default we
    ///   change later actually *reaches* existing users instead of being pinned
    ///   forever by a value their file happened to record. Vanilla writes every
    ///   line and has the opposite behaviour.
    /// - **An object, not an array.** Order carries no meaning, and a map makes
    ///   the loader's "skip what you don't recognise" rule natural.
    ///
    /// A default-valued table produces an empty object, which the caller may
    /// omit from `options.json` entirely.
    #[must_use]
    pub fn to_json_value(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        for action in InputAction::ALL {
            if !self.is_default(action) {
                obj.insert(
                    action.name().to_string(),
                    serde_json::Value::String(self.binding(action).name()),
                );
            }
        }
        serde_json::Value::Object(obj)
    }

    /// Read a table back, starting from the defaults and applying whatever the
    /// value contains.
    ///
    /// **Nothing here fails.** Per the same rule the server list and
    /// [`crate::config::Options`] follow, a broken settings file must not stop
    /// the game launching — and specifically must not stop it at the *first*
    /// broken entry:
    ///
    /// - a value that is not an object → the defaults;
    /// - an unrecognised action name → that entry is skipped, later entries
    ///   still apply;
    /// - an unrecognised or non-string binding name → **that action keeps its
    ///   default**, later entries still apply.
    ///
    /// The middle two are the interesting ones: an early `return`/`?` there is
    /// how one stale line in a config file silently discards every binding after
    /// it.
    #[must_use]
    pub fn from_json_value(value: &serde_json::Value) -> Self {
        let mut binds = Self::default();
        let Some(obj) = value.as_object() else {
            return binds;
        };
        for (name, raw) in obj {
            // Unknown action: skip this entry, keep reading.
            let Some(action) = InputAction::from_name(name) else {
                continue;
            };
            // Unparseable or non-string binding: leave the default in place,
            // keep reading.
            let Some(binding) = raw.as_str().and_then(Binding::parse) else {
                continue;
            };
            binds.set(action, binding);
        }
        binds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- defaults, against the decompiled source ---------------------------
    //
    // The expected values below are **GLFW keysyms transcribed from
    // `Options.java:655-725`**, paired with the winit `KeyCode` for that
    // physical key. They do not come from `default_binding`, so a mistake there
    // cannot also be the thing this test asserts. `CLAUDE.md`'s rule: an
    // expected value must originate outside the code under test.

    /// GLFW keysym → the winit `KeyCode` naming the same physical key.
    ///
    /// Only the codes the default table uses. Written out so the assertion below
    /// is against vanilla's *numbers*, which is what the source actually says,
    /// rather than against a second copy of our own key table.
    fn winit_code_for_glfw(keysym: u32) -> KeyCode {
        match keysym {
            32 => KeyCode::Space,
            47 => KeyCode::Slash,
            49 => KeyCode::Digit1,
            50 => KeyCode::Digit2,
            51 => KeyCode::Digit3,
            52 => KeyCode::Digit4,
            53 => KeyCode::Digit5,
            54 => KeyCode::Digit6,
            55 => KeyCode::Digit7,
            56 => KeyCode::Digit8,
            57 => KeyCode::Digit9,
            65 => KeyCode::KeyA,
            68 => KeyCode::KeyD,
            69 => KeyCode::KeyE,
            70 => KeyCode::KeyF,
            83 => KeyCode::KeyS,
            84 => KeyCode::KeyT,
            87 => KeyCode::KeyW,
            256 => KeyCode::Escape,
            258 => KeyCode::Tab,
            292 => KeyCode::F3,
            294 => KeyCode::F5,
            340 => KeyCode::ShiftLeft,
            341 => KeyCode::ControlLeft,
            other => panic!("no winit mapping recorded for GLFW keysym {other}"),
        }
    }

    #[test]
    fn defaults_match_the_decompiled_vanilla_key_mappings() {
        // Transcribed from `.cache/mc/26.2/client-src/net/minecraft/client/
        // Options.java:655-725`: (action, GLFW keysym, KeyMapping.Category).
        let vanilla: &[(InputAction, u32, Category)] = &[
            (InputAction::Forward, 87, Category::Movement),
            (InputAction::Left, 65, Category::Movement),
            (InputAction::Back, 83, Category::Movement),
            (InputAction::Right, 68, Category::Movement),
            (InputAction::Jump, 32, Category::Movement),
            (InputAction::Sneak, 340, Category::Movement),
            (InputAction::Sprint, 341, Category::Movement),
            (InputAction::Inventory, 69, Category::Inventory),
            (InputAction::Chat, 84, Category::Multiplayer),
            (InputAction::PlayerList, 258, Category::Multiplayer),
            (InputAction::Command, 47, Category::Multiplayer),
            (InputAction::TogglePerspective, 294, Category::Misc),
            (InputAction::Hotbar1, 49, Category::Inventory),
            (InputAction::Hotbar2, 50, Category::Inventory),
            (InputAction::Hotbar3, 51, Category::Inventory),
            (InputAction::Hotbar4, 52, Category::Inventory),
            (InputAction::Hotbar5, 53, Category::Inventory),
            (InputAction::Hotbar6, 54, Category::Inventory),
            (InputAction::Hotbar7, 55, Category::Inventory),
            (InputAction::Hotbar8, 56, Category::Inventory),
            (InputAction::Hotbar9, 57, Category::Inventory),
            // `Options.java:698` — a real `KeyMapping` in 26.2, category DEBUG.
            (InputAction::DebugOverlay, 292, Category::Debug),
        ];
        for &(action, keysym, category) in vanilla {
            assert_eq!(
                action.default_binding(),
                Binding::Key(winit_code_for_glfw(keysym)),
                "{} should default to GLFW keysym {keysym}",
                action.name()
            );
            assert_eq!(
                action.category(),
                category,
                "{} is in the wrong Controls-menu group",
                action.name()
            );
        }

        // The two mouse defaults: `Options.java:665-668` declares
        // `keyUse` as `Type.MOUSE` 1 and `keyAttack` as `Type.MOUSE` 0, and
        // `InputConstants.java:343-344` names those `left` and `right`.
        assert_eq!(
            InputAction::Attack.default_binding(),
            Binding::Mouse(MouseButton::Left)
        );
        assert_eq!(
            InputAction::Use.default_binding(),
            Binding::Mouse(MouseButton::Right)
        );
        assert_eq!(InputAction::Attack.category(), Category::Gameplay);
        assert_eq!(InputAction::Use.category(), Category::Gameplay);
    }

    #[test]
    fn a_binding_can_hold_a_mouse_button_not_just_a_key() {
        // The requirement that rules out a `KeyCode`-only table: vanilla's
        // *default* configuration cannot be expressed without this, since
        // attack and use are mouse-bound out of the box.
        let binds = Keybinds::new();
        assert!(binds.is_mouse(InputAction::Attack, MouseButton::Left));
        assert!(!binds.is(InputAction::Attack, KeyCode::KeyW));

        // …and a mouse button can be rebound to a key, and vice versa.
        let mut binds = binds;
        binds.set(InputAction::Attack, Binding::Key(KeyCode::KeyR));
        assert!(binds.is(InputAction::Attack, KeyCode::KeyR));
        assert!(!binds.is_mouse(InputAction::Attack, MouseButton::Left));

        binds.set(InputAction::Jump, Binding::Mouse(MouseButton::Middle));
        assert!(binds.is_mouse(InputAction::Jump, MouseButton::Middle));
        assert!(!binds.is(InputAction::Jump, KeyCode::Space));
    }

    #[test]
    fn every_action_has_a_unique_name_that_resolves_back() {
        let mut seen = std::collections::BTreeSet::new();
        for action in InputAction::ALL {
            assert!(
                seen.insert(action.name()),
                "duplicate action name {}",
                action.name()
            );
            assert_eq!(InputAction::from_name(action.name()), Some(action));
        }
        assert_eq!(seen.len(), InputAction::ALL.len());
        assert_eq!(InputAction::from_name("key.nonexistent"), None);
    }

    #[test]
    fn the_array_index_matches_the_declaration_order() {
        // `Keybinds` indexes by `action as usize`, so a variant added to the
        // enum but not to `ALL` (or added in a different position) would index
        // into the wrong slot — a silent cross-wiring of two bindings.
        for (i, action) in InputAction::ALL.into_iter().enumerate() {
            assert_eq!(action as usize, i, "{} is out of position", action.name());
        }
    }

    #[test]
    fn a_rebind_takes_effect_and_the_old_key_stops_working() {
        let mut binds = Keybinds::new();
        assert!(binds.is(InputAction::Inventory, KeyCode::KeyE));

        binds.set(InputAction::Inventory, Binding::Key(KeyCode::KeyI));
        assert!(binds.is(InputAction::Inventory, KeyCode::KeyI));
        // The half that a naive implementation gets wrong: the *old* key must
        // stop firing, not merely the new one start.
        assert!(!binds.is(InputAction::Inventory, KeyCode::KeyE));
        assert!(!binds.is_default(InputAction::Inventory));

        binds.reset(InputAction::Inventory);
        assert!(binds.is(InputAction::Inventory, KeyCode::KeyE));
        assert!(binds.is_default(InputAction::Inventory));
    }

    #[test]
    fn an_unbound_action_never_fires() {
        let mut binds = Keybinds::new();
        binds.set(InputAction::Jump, Binding::Unbound);
        assert!(!binds.is(InputAction::Jump, KeyCode::Space));
        // Nor on any other key — an `Unbound` that compared equal to something
        // would fire on whatever that something was.
        for code in [KeyCode::KeyW, KeyCode::Escape, KeyCode::F3] {
            assert!(!binds.is(InputAction::Jump, code));
        }
        assert!(!binds.is_mouse(InputAction::Jump, MouseButton::Left));
    }

    #[test]
    fn a_conflict_is_reported_for_both_actions_and_only_for_them() {
        let mut binds = Keybinds::new();
        // Vanilla's defaults must themselves be conflict-free, or the menu
        // would light up red on a fresh install.
        for action in InputAction::ALL {
            assert!(
                !binds.has_conflict(action),
                "{} conflicts with {:?} in the default table",
                action.name(),
                binds.conflicts(action).map(InputAction::name).collect::<Vec<_>>()
            );
        }

        // Put jump on the inventory key.
        binds.set(InputAction::Jump, Binding::Key(KeyCode::KeyE));
        assert_eq!(
            binds.conflicts(InputAction::Jump).collect::<Vec<_>>(),
            vec![InputAction::Inventory]
        );
        // Symmetric: the menu highlights *both* rows, so the query must answer
        // for either side.
        assert_eq!(
            binds.conflicts(InputAction::Inventory).collect::<Vec<_>>(),
            vec![InputAction::Jump]
        );
        // And nothing else is dragged in.
        assert!(!binds.has_conflict(InputAction::Forward));
        assert_eq!(
            binds
                .actions_bound_to(Binding::Key(KeyCode::KeyE))
                .collect::<Vec<_>>(),
            vec![InputAction::Jump, InputAction::Inventory]
        );

        // A mouse-button conflict is found the same way — the query must not be
        // keyboard-only.
        binds.set(InputAction::Use, Binding::Mouse(MouseButton::Left));
        assert_eq!(
            binds.conflicts(InputAction::Attack).collect::<Vec<_>>(),
            vec![InputAction::Use]
        );
    }

    #[test]
    fn unbound_actions_do_not_count_as_conflicting_with_each_other() {
        // The trap in a naive `conflicts`: two unbound actions share a "binding"
        // and would report a conflict, reddening the whole unbound half of a
        // Controls screen.
        let mut binds = Keybinds::new();
        binds.set(InputAction::Jump, Binding::Unbound);
        binds.set(InputAction::Sneak, Binding::Unbound);
        assert!(!binds.has_conflict(InputAction::Jump));
        assert!(!binds.has_conflict(InputAction::Sneak));
        assert_eq!(binds.actions_bound_to(Binding::Unbound).count(), 0);
    }

    #[test]
    fn categories_are_grouped_in_vanillas_registration_order() {
        // `KeyMapping.java:204-211`. The point of pinning this: the order is
        // *not* alphabetical and not what a reader would guess — MISC is second.
        assert_eq!(
            Category::SORT_ORDER.map(Category::id),
            [
                "movement",
                "misc",
                "multiplayer",
                "gameplay",
                "inventory",
                "creative",
                "spectator",
                "debug",
            ]
        );

        // Every action lands in exactly one category, and walking SORT_ORDER
        // visits all of them — so a menu built this way cannot silently omit a
        // binding.
        let mut total = 0;
        for category in Category::SORT_ORDER {
            total += Keybinds::in_category(category).count();
        }
        assert_eq!(total, InputAction::ALL.len());

        // The two categories this client does not populate are empty rather
        // than missing, which is what makes adding one later a local change.
        assert_eq!(Keybinds::in_category(Category::Creative).count(), 0);
        assert_eq!(Keybinds::in_category(Category::Spectator).count(), 0);
        assert_eq!(Keybinds::in_category(Category::Movement).count(), 7);
        assert_eq!(Keybinds::in_category(Category::Inventory).count(), 10);
    }

    // -- persistence -------------------------------------------------------

    #[test]
    fn the_persisted_form_is_this_exact_literal_string() {
        // `parse(write(x)) == x` is satisfied by two symmetric misunderstandings
        // (`CLAUDE.md`'s round-trip warning), so the format is pinned against a
        // literal written out by hand from vanilla's own vocabulary — the
        // `key_key.forward:key.keyboard.w` shape of `Options.java:1618-1622`
        // and `InputConstants.java:342-478`, as JSON.
        let mut binds = Keybinds::new();
        binds.set(InputAction::Forward, Binding::Key(KeyCode::ArrowUp));
        binds.set(InputAction::Sneak, Binding::Key(KeyCode::ShiftRight));
        binds.set(InputAction::Attack, Binding::Mouse(MouseButton::Middle));
        binds.set(InputAction::Use, Binding::Unbound);

        let text = serde_json::to_string_pretty(&binds.to_json_value()).unwrap();
        assert_eq!(
            text,
            r#"{
  "key.attack": "key.mouse.middle",
  "key.forward": "key.keyboard.up",
  "key.sneak": "key.keyboard.right.shift",
  "key.use": "key.keyboard.unknown"
}"#,
            "the on-disk keybind format changed"
        );

        // And the *defaults* are absent, which is the property that lets a
        // future default change reach an existing user's file.
        assert!(!text.contains("key.jump"), "defaults must not be written");
        assert_eq!(
            serde_json::to_string(&Keybinds::new().to_json_value()).unwrap(),
            "{}",
            "a fully-default table writes nothing"
        );
    }

    #[test]
    fn a_hand_written_file_parses_to_the_bindings_it_names() {
        // The other direction, also against a hand-written literal rather than
        // against our own writer.
        let text = r#"{
            "key.forward": "key.keyboard.up",
            "key.jump": "key.mouse.middle",
            "key.debug.overlay": "key.keyboard.f7",
            "key.hotbar.9": "key.keyboard.keypad.9",
            "key.lodestone.pause": "key.keyboard.grave.accent"
        }"#;
        let binds = Keybinds::from_json_value(&serde_json::from_str(text).unwrap());
        assert!(binds.is(InputAction::Forward, KeyCode::ArrowUp));
        assert!(binds.is_mouse(InputAction::Jump, MouseButton::Middle));
        assert!(binds.is(InputAction::DebugOverlay, KeyCode::F7));
        assert!(binds.is(InputAction::Hotbar9, KeyCode::Numpad9));
        assert!(binds.is(InputAction::Pause, KeyCode::Backquote));
        // Untouched actions keep their defaults.
        assert!(binds.is(InputAction::Back, KeyCode::KeyS));
        assert!(binds.is_mouse(InputAction::Attack, MouseButton::Left));
    }

    #[test]
    fn a_round_trip_is_stable_across_repeated_saves() {
        let mut binds = Keybinds::new();
        binds.set(InputAction::Forward, Binding::Key(KeyCode::ArrowUp));
        binds.set(InputAction::Attack, Binding::Key(KeyCode::KeyR));
        binds.set(InputAction::Use, Binding::Mouse(MouseButton::Other(4)));
        binds.set(InputAction::Chat, Binding::Unbound);

        let once = Keybinds::from_json_value(&binds.to_json_value());
        assert_eq!(once, binds);
        // A second cycle must not drift — a writer that normalised a name
        // differently from the parser would show up here and not in one pass.
        let twice = Keybinds::from_json_value(&once.to_json_value());
        assert_eq!(twice, binds);
        assert_eq!(once.to_json_value(), twice.to_json_value());
    }

    #[test]
    fn every_named_key_round_trips_through_its_name() {
        // The whole table, both directions. This is what stops a typo in one
        // entry from silently reverting that key on the next launch.
        for &(code, name) in KEY_NAMES {
            assert_eq!(
                Binding::parse(name),
                Some(Binding::Key(code)),
                "{name} did not parse back"
            );
            assert_eq!(Binding::Key(code).name(), name);
        }
        // No duplicate names, or `key_from_name` would resolve one of them to
        // the wrong key.
        let mut seen = std::collections::BTreeSet::new();
        for &(_, name) in KEY_NAMES {
            assert!(seen.insert(name), "duplicate key name {name}");
        }
        // And no duplicate codes, or `key_name` would pick arbitrarily.
        let mut codes = std::collections::BTreeSet::new();
        for &(code, _) in KEY_NAMES {
            assert!(codes.insert(format!("{code:?}")), "duplicate code {code:?}");
        }
    }

    #[test]
    fn mouse_buttons_round_trip_including_vanillas_one_based_numbering() {
        for button in [
            MouseButton::Left,
            MouseButton::Right,
            MouseButton::Middle,
            MouseButton::Back,
            MouseButton::Forward,
            MouseButton::Other(3),
            MouseButton::Other(7),
        ] {
            let binding = Binding::Mouse(button);
            assert_eq!(
                Binding::parse(&binding.name()),
                Some(binding),
                "{} did not round-trip",
                binding.name()
            );
        }
        // Vanilla's numbering: button index 3 is `key.mouse.4`
        // (`InputConstants.java:346`), not `key.mouse.3`.
        assert_eq!(Binding::Mouse(MouseButton::Other(3)).name(), "key.mouse.4");
        assert_eq!(Binding::Mouse(MouseButton::Other(7)).name(), "key.mouse.8");
        // The low numbers are left/right/middle's territory and must not also
        // be accepted as `Other`, or one button would have two spellings.
        assert_eq!(Binding::parse("key.mouse.1"), None);
        assert_eq!(Binding::parse("key.mouse.3"), None);
    }

    #[test]
    fn an_unknown_binding_falls_back_to_the_default_without_poisoning_the_parse() {
        // The failure mode this exists to prevent: one stale or misspelled line
        // discarding every entry after it. The bad entries here are deliberately
        // *interleaved* with good ones and sorted so they come first
        // alphabetically, so a `?`/`return` in the loop would be caught.
        let text = r#"{
            "key.attack": "key.keyboard.zzz.not.a.key",
            "key.back": "key.keyboard.up",
            "key.chat": 42,
            "key.drop": "key.keyboard.q",
            "key.forward": "key.keyboard.down",
            "key.jump": null,
            "key.left": "scancode.30",
            "key.right": "key.keyboard.right",
            "key.zzz.unknown.action": "key.keyboard.p"
        }"#;
        let binds = Keybinds::from_json_value(&serde_json::from_str(text).unwrap());

        // Bad entries fell back to the default…
        assert!(
            binds.is_mouse(InputAction::Attack, MouseButton::Left),
            "an unknown key name must fall back to the default"
        );
        assert!(binds.is(InputAction::Chat, KeyCode::KeyT), "non-string value");
        assert!(binds.is(InputAction::Jump, KeyCode::Space), "null value");
        assert!(
            binds.is(InputAction::Left, KeyCode::KeyA),
            "an unsupported binding *type* must also fall back, not error"
        );
        // …and unknown *action* names were simply ignored.
        assert!(InputAction::from_name("key.drop").is_none());

        // The load kept going: every good entry after a bad one still applied.
        assert!(
            binds.is(InputAction::Back, KeyCode::ArrowUp),
            "the entry after a bad one was dropped — the parse was poisoned"
        );
        assert!(binds.is(InputAction::Forward, KeyCode::ArrowDown));
        assert!(
            binds.is(InputAction::Right, KeyCode::ArrowRight),
            "the last entry, after four bad ones, was dropped"
        );

        // -- negative control -------------------------------------------------
        // Prove the assertions above can fail: the identical file with the bad
        // entries removed produces *different* values for exactly those actions.
        // Without this, "fell back to the default" is indistinguishable from
        // "the parser did nothing at all".
        let good = r#"{
            "key.attack": "key.keyboard.x",
            "key.back": "key.keyboard.up",
            "key.chat": "key.keyboard.y",
            "key.forward": "key.keyboard.down",
            "key.jump": "key.keyboard.z",
            "key.left": "key.keyboard.v",
            "key.right": "key.keyboard.right"
        }"#;
        let control = Keybinds::from_json_value(&serde_json::from_str(good).unwrap());
        assert!(control.is(InputAction::Attack, KeyCode::KeyX));
        assert!(control.is(InputAction::Chat, KeyCode::KeyY));
        assert!(control.is(InputAction::Jump, KeyCode::KeyZ));
        assert!(control.is(InputAction::Left, KeyCode::KeyV));
        assert_ne!(control, binds, "control must differ, or nothing was tested");
    }

    #[test]
    fn a_non_object_value_is_the_defaults_rather_than_a_panic() {
        for text in ["null", "[]", "42", "\"nope\"", "true"] {
            let value: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(
                Keybinds::from_json_value(&value),
                Keybinds::new(),
                "{text} should degrade to the defaults"
            );
        }
        assert_eq!(
            Keybinds::from_json_value(&serde_json::json!({})),
            Keybinds::new()
        );
    }

    #[test]
    fn labels_are_readable_for_a_controls_menu() {
        assert_eq!(Binding::Key(KeyCode::KeyW).label(), "W");
        assert_eq!(Binding::Key(KeyCode::ShiftLeft).label(), "Left Shift");
        assert_eq!(Binding::Key(KeyCode::F3).label(), "F3");
        assert_eq!(Binding::Key(KeyCode::Space).label(), "Space");
        assert_eq!(Binding::Mouse(MouseButton::Left).label(), "Left Button");
        assert_eq!(Binding::Mouse(MouseButton::Other(3)).label(), "Button 4");
        assert_eq!(Binding::Unbound.label(), "Not bound");
    }

    #[test]
    fn movement_actions_map_onto_the_controllers_action_set() {
        // The seam onto `lodestone-controller`, which owns double-tap sprint.
        // Every one of its `Action`s must be reachable, or a movement key would
        // be unbindable and the physics would never see it.
        let mapped: Vec<Action> = InputAction::ALL
            .into_iter()
            .filter_map(InputAction::movement)
            .collect();
        assert_eq!(
            mapped,
            vec![
                Action::Forward,
                Action::Back,
                Action::Left,
                Action::Right,
                Action::Jump,
                Action::Sneak,
                Action::Sprint,
            ]
        );
        // And nothing outside the movement category claims one.
        for action in InputAction::ALL {
            assert_eq!(
                action.movement().is_some(),
                action.category() == Category::Movement,
                "{} disagrees about being a movement action",
                action.name()
            );
        }
    }

    #[test]
    fn hotbar_actions_cover_slots_zero_through_eight_exactly_once() {
        // The `Hotbar1` → slot `0` off-by-one is the kind of thing that silently
        // shifts every hotbar key by one.
        let slots: Vec<usize> = InputAction::ALL
            .into_iter()
            .filter_map(InputAction::hotbar_slot)
            .collect();
        assert_eq!(slots, (0..9).collect::<Vec<_>>());
        assert_eq!(InputAction::Hotbar1.hotbar_slot(), Some(0));
        assert_eq!(InputAction::Hotbar9.hotbar_slot(), Some(8));
        assert_eq!(InputAction::Forward.hotbar_slot(), None);
    }
}
