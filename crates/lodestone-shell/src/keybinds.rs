//! The keybinding layer: a rebindable table mapping **logical actions** to
//! **physical inputs**, so no gameplay input in the shell names a key literally.
//!
//! ## What this is
//!
//! [`InputAction`] is the closed set of things the player can *ask for*
//! ("move forward", "open the inventory"). [`Binding`] is what they press to ask
//! (a keyboard key, a mouse button, or nothing). [`Keybinds`] is the table
//! joining the two, plus the queries the Controls menu's Key Binds screen
//! needs (`crate::menu::key_binds`): grouping by [`Category`], "is
//! this the default?", and "what else is bound to this?".
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
//! - Action names and categories: vanilla's own persisted-options declarations, which declares every
//!   key binding with its name, GLFW keysym and category.
//! - Category order: vanilla's own key-binding class. `Category.SORT_ORDER` is
//!   *registration* order, and it is **not** alphabetical or intuitive —
//!   `MISC` comes second, before `MULTIPLAYER`, `GAMEPLAY` and `INVENTORY`.
//!   [`Category::SORT_ORDER`] reproduces it because that is the order vanilla's
//!   Controls screen groups by.
//! - Persisted binding names: vanilla's own input-constants `addKey` table
//!   (`key.keyboard.w`, `key.mouse.left`, `key.keyboard.left.shift`, …).
//! - The save-line shape: vanilla's own persisted-options declarations write each mapping as
//!   `key_<name>` → `saveString()`.
//!
//! ## Deliberate divergences from vanilla, and why
//!
//! 1. **F3 *is* a real key binding in 26.2.** This was worth checking rather
//!    than assuming: in older versions the debug keys were handled inline in
//!    the keyboard handler's debug path, but 26.2 declares
//!    a real debug-overlay key binding at keysym 292, debug category,
//!    and the keyboard handler dispatches
//!    it through the ordinary key-matching path like any other binding
//!   . So routing F3 through this table is
//!    vanilla-*correct*, not a divergence. [`Category::Debug`] exists for it.
//!
//!    **And so are the F3 *chords*, which is the opposite of what this file
//!    used to assume.** Vanilla's own persisted-options declarations declares `keyDebugShowHitboxes`,
//!    `keyDebugShowChunkBorders`, `keyDebugShowAdvancedTooltips`,
//!    `keyDebugSpectate`, `keyDebugSwitchGameMode`, `keyDebugFocusPause` and
//!    `keyDebugCopyLocation` as debug-category key bindings, collects them
//!    in `debugKeys`, and folds that array into `keyMappings` — the one
//!    vanilla persists and the Controls screen lists. Vanilla's own
//!    debug-key handling then asks each of them whether it matches the event. All seven are
//!    in this table for that reason. Two chords are **not**, and both are
//!    Lodestone-only rather than ports: `Shift+F3` (the profiler pie chart's
//!    visibility, which vanilla has no mapping for) and the chart's number-row
//!    navigation — vanilla's own four chart chords are key bindings with a
//!    `clashContext`, a concept this table has no equivalent of, and without
//!    one nine digit actions would report a permanent conflict against the
//!    nine hotbar keys they share a keysym with.
//!
//! 2. **Escape is genuinely not a key binding.** Vanilla handles it in
//!    its own screen/keyboard-handler code directly, so it cannot be rebound there. We
//!    route it through the table as [`InputAction::Pause`] because the user
//!    asked for nothing hardcoded and a Controls menu should be able to display
//!    it. **Hazard:** rebinding it away leaves no gameplay route to the pause
//!    screen — see the note on [`InputAction::Pause`].
//!
//! 3. **Menu navigation and text editing stay literal.** Arrow keys, Enter,
//!    Backspace and Delete in `app.rs`'s `menu_key_for`/`handle_chat_key` are
//!    *not* in this table, matching vanilla: those are screen-level keyboard
//!    handling, not key bindings, and a rebindable "move the menu cursor down"
//!    is not a thing vanilla's Controls screen offers either. The boundary is
//!    "gameplay and world bindings are rebindable; UI chrome is not".
//!
//! 4. **`F` is [`InputAction::SwapOffhand`], and it took a deletion to get
//!    there.** This table used to carry a Lodestone-only `key.lodestone.toggleFly`
//!    on `F` — a free-cam/noclip developer affordance with no vanilla
//!    counterpart. That fix deleted it (vanilla's `/gamemode creative` plus
//!    That fix's real double-tap-jump flight cover it), which freed `F` for vanilla's
//!    actual binding: `key.swapOffhand`, vanilla's own persisted-options declarations, GLFW keysym 70.
//!
//!    That collision was a real blocker, not a tidiness complaint — adding
//!    `SwapOffhand` on `F` alongside `ToggleFly` turned
//!    `a_conflict_is_reported_for_both_actions_and_only_for_them` red, which is
//!    that test doing its job, and that fix's off-hand half was correctly reverted
//!    rather than forced past it.
//!
//!    **Vanilla's `F` means two different things depending on what is on
//!    screen, and both halves now work.** They are separate mechanisms, not one
//!    mechanism with a flag — conflating them is the trap that fix was filed
//!    against:
//!
//!    * **container half** (part 3 / that fix). `ContainerInput::SWAP` with
//!      button `40` against the hovered slot
//!     , reached through `app.rs`'s
//!      `KeyOutcome::ContainerSwap` exactly like the number keys `1`–`9`.
//!      `Click::offhand_swap` and `do_swap`'s `button == 40` arm were already in
//!      place and tested; this binding is what finally reached them.
//!    * **gameplay half**. With no screen open, vanilla sends a bare
//!      `ServerboundPlayerActionPacket` / `SWAP_ITEM_WITH_OFFHAND`
//!      — **no slot, no hit test, no container**.
//!      Reached through `KeyOutcome::SwapOffhand`, guarded on
//!      the player not being a spectator and sent with no local prediction, because
//!      vanilla performs none either (the server does the exchange).
//!
//!    The two arms also ask in **different orders relative to the number keys**,
//!    which is not an inconsistency: vanilla's own container-screen hotbar-swap
//!    key handling asks the off-hand
//!    key first, vanilla's own client-side key handling asks the hotbar keys first
//!    (`:1873` vs `:1900`). It only shows if someone rebinds the off-hand key
//!    onto a digit.
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
//! (vanilla's own keysym input type is GLFW's layout-independent keysym). Fixing
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

use lodestone_controller::Action;

// ---------------------------------------------------------------------------
// Categories
// ---------------------------------------------------------------------------

/// The group a binding is listed under in a Controls menu.
///
/// Mirrors vanilla's own key-binding category. All eight vanilla
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
    /// vanilla's own key-binding class, not alphabetical and not the order a reader
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
    /// `key.categories.<id>`.
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
/// client does not implement (`key.advancements`, `key.fullscreen`, …) are
/// deliberately **absent** rather than listed and dead. Adding one is a
/// two-line change here plus the branch that consumes it; adding one
/// *without* the branch is the island defect `CLAUDE.md` §1 is about, and a
/// Controls menu offering a binding that does nothing is exactly how that
/// looks to a player.
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
    /// Vanilla's `key.pickItem` (vanilla's own persisted-options declarations, mouse-button type, button `2`,
    /// category `GAMEPLAY`). Middle-click (or, once rebound, a key) requests
    /// the item under the crosshair be placed in the hotbar.
    ///
    /// **Two mechanisms, split the same way as [`Self::SwapOffhand`]/
    /// [`Self::Drop`], except the container half already had its producer.**
    /// With a container screen open, middle-click was already wired straight
    /// through `MenuButton::Pick` (`crate::app::menu_button_for`) before this
    /// variant existed at all, and `MenuInput::key_pressed`'s `MenuKey::
    /// PickItem` arm (`container.rs`) was built and tested with **no**
    /// producer for the *keyboard* form of the same screen — see
    /// [`crate::app::KeyOutcome::ContainerPickItem`]. With no screen open,
    /// vanilla's own pick-block-or-entity handling
    /// switches on the current `HitResult`: `ClientAction::
    /// PickItemFromEntity` when the crosshair is over an entity,
    /// `ClientAction::PickItemFromBlock` when it is over a block, nothing on
    /// a miss — both already encoded and round-trip tested
    /// (`crates/protocol/v770/tests/serverbound_interaction_tier2.rs`) with
    /// zero producers before this. `include_data` on either action is
    /// `hasControlDown()`, read the same place [`Self::Drop`]'s `ctrl` is.
    /// See [`crate::app::KeyOutcome::PickItem`].
    PickItem,
    // -- inventory --------------------------------------------------------
    Inventory,
    /// Vanilla's `key.swapOffhand`.
    ///
    /// **Both halves are implemented, and they are two different mechanisms.**
    /// With a screen open it is a `ContainerInput::SWAP` with button `40` against
    /// the hovered slot; with no screen open it is a bare
    /// `ServerboundPlayerAction`/`SWAP_ITEM_WITH_OFFHAND` carrying no slot at all
    /// `app.rs`'s `resolve_key` routes the two from different arms
    /// — see [`crate::app::KeyOutcome::SwapOffhand`] and the module docs.
    SwapOffhand,
    /// Vanilla's `key.drop`. Drop one item, or — with
    /// Control held — the whole stack.
    ///
    /// **Two mechanisms depending on context, the same shape as
    /// [`Self::SwapOffhand`].** With a container screen open and a slot
    /// hovered, it is a `ContainerInput::Throw` click against that slot
    /// (vanilla's own container-screen key handling, gated on `hoveredSlot != null
    /// && hoveredSlot.hasItem()`, not an empty cursor); with no screen open it
    /// is a bare `ClientAction::DropSelectedItem`/`DropSelectedItemStack`
    /// (vanilla's own client-side key handling, `PLAYER_ACTION`/`DROP_ITEM`). `app.rs`'s
    /// `resolve_key` routes the two from different arms — see
    /// [`crate::app::KeyOutcome::ContainerDrop`]/[`crate::app::KeyOutcome::Drop`].
    Drop,
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
    /// Vanilla's `key.screenshot` (vanilla's own persisted-options declarations, GLFW keysym `291` =
    /// F2, category `MISC`).
    ///
    /// **The odd one out: purely local, no packet.** Every other action in
    /// this enum ends at a `ClientAction`/container `Click`; this one ends at
    /// a file. Vanilla's own screenshot-grab copies
    /// the main render target's colour texture to a CPU buffer and writes it
    /// as a PNG to `<gameDirectory>/screenshots/`, named
    /// by vanilla's own filename-formatted-date-time helper (`yyyy-MM-dd_HH.mm.ss`, the
    /// system clock at capture time) with a `_2`, `_3`, … suffix appended on
    /// a same-second collision (vanilla's own get-file lookup) — never
    /// overwriting an existing file. See [`crate::app::KeyOutcome::
    /// Screenshot`] for the capture itself, which needs the GPU device/
    /// surface texture and so lives in `gpu.rs`, not here.
    ///
    /// **Not modelled: the Control-held panorama variant.**
    /// Vanilla's own global-key-press handling passes `controlDown` straight to
    /// its own screenshot-grab, which only takes the
    /// four-angle `panorama_0..3.png` branch when
    /// a build-time debug-panorama-screenshot flag is also true — a
    /// developer-only flag vanilla ships `false` in every release build. A
    /// normal player's Ctrl+F2 is byte-identical to a plain F2, so this
    /// action carries no `ctrl` payload; the consumer may still read the
    /// driver's tracked `ctrl_held` if it ever wants to build the debug
    /// variant, but nothing here requires it to.
    ///
    /// **Not modelled: `handleGlobalKeyPress`'s screen-independence.**
    /// Vanilla checks this key *outside* its own screen key-handling entirely, so a
    /// screenshot can be taken from the pause menu or an open inventory.
    /// This port routes it through [`crate::app::resolve_key`] like every
    /// other action, which the menu/chat/container gates swallow first — the
    /// same simplification [`Self::DebugOverlay`] already accepts (F3 does
    /// not open the debug overlay from behind a menu here either). Fixing
    /// both together, if ever wanted, is one change: hoist both above the
    /// `gate.menu`/`gate.chat_open` early returns.
    Screenshot,
    TogglePerspective,
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
    /// F3+B — `key.debug.showHitboxes`, GLFW 66.
    ///
    /// **The seven chords below are real key bindings in 26.2, checked in the jar
    /// rather than assumed.** Vanilla's own persisted-options declarations declare each one with a
    /// debug category and puts them in `debugKeys`, which is folded into
    /// `keyMappings` — the array vanilla persists and the Controls screen
    /// lists — and vanilla's own debug-key handling dispatches every one
    /// through its own key-matching check, not through a literal keysym.
    /// So a rebindable F3+B is vanilla-*correct*; hardcoding it was the
    /// divergence. See [`crate::app::KeyGate::debug_held`] for why the F3
    /// *modifier* itself stays a gate flag rather than becoming an eighth
    /// action here.
    DebugShowHitboxes,
    /// F3+G — `key.debug.showChunkBorders`, GLFW 71.
    DebugShowChunkBorders,
    /// F3+H — `key.debug.showAdvancedTooltips`, GLFW 72. Note the vanilla
    /// name carries `show`; `key.debug.advancedTooltips` is not a real key.
    DebugShowAdvancedTooltips,
    /// F3+N — `key.debug.spectate`, GLFW 78.
    DebugSpectate,
    /// F3+F4 — `key.debug.switchGameMode`, GLFW 293.
    DebugSwitchGameMode,
    /// F3+P — `key.debug.focusPause`, GLFW 80.
    DebugFocusPause,
    /// F3+C — `key.debug.copyLocation`, GLFW 67.
    DebugCopyLocation,
}

impl InputAction {
    /// Every action, in declaration order. Declaration order is grouped by
    /// category and, within a category, follows vanilla's own persisted-options
    /// declarations' own
    /// declaration order — so walking `ALL` filtered by [`Category::SORT_ORDER`]
    /// reproduces vanilla's Controls-screen ordering without a sort.
    pub const ALL: [InputAction; 36] = [
        InputAction::Forward,
        InputAction::Back,
        InputAction::Left,
        InputAction::Right,
        InputAction::Jump,
        InputAction::Sneak,
        InputAction::Sprint,
        InputAction::Attack,
        InputAction::Use,
        InputAction::PickItem,
        InputAction::Inventory,
        InputAction::SwapOffhand,
        InputAction::Drop,
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
        InputAction::Screenshot,
        InputAction::TogglePerspective,
        InputAction::Pause,
        InputAction::DebugOverlay,
        InputAction::DebugShowHitboxes,
        InputAction::DebugShowChunkBorders,
        InputAction::DebugShowAdvancedTooltips,
        InputAction::DebugSpectate,
        InputAction::DebugSwitchGameMode,
        InputAction::DebugFocusPause,
        InputAction::DebugCopyLocation,
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
            InputAction::PickItem => "key.pickItem",
            InputAction::Inventory => "key.inventory",
            InputAction::SwapOffhand => "key.swapOffhand",
            InputAction::Drop => "key.drop",
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
            InputAction::Screenshot => "key.screenshot",
            InputAction::TogglePerspective => "key.togglePerspective",
            InputAction::Pause => "key.lodestone.pause",
            InputAction::DebugOverlay => "key.debug.overlay",
            InputAction::DebugShowHitboxes => "key.debug.showHitboxes",
            InputAction::DebugShowChunkBorders => "key.debug.showChunkBorders",
            InputAction::DebugShowAdvancedTooltips => "key.debug.showAdvancedTooltips",
            InputAction::DebugSpectate => "key.debug.spectate",
            InputAction::DebugSwitchGameMode => "key.debug.switchGameMode",
            InputAction::DebugFocusPause => "key.debug.focusPause",
            InputAction::DebugCopyLocation => "key.debug.copyLocation",
        }
    }

    /// Resolve a persisted action name. `None` for anything unrecognised, which
    /// the loader treats as "skip this line" rather than an error — see
    /// [`Keybinds::from_json_value`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        InputAction::ALL.into_iter().find(|a| a.name() == name)
    }

    /// The Controls-menu group. Matches the category vanilla gives
    /// the corresponding mapping.
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
            InputAction::Attack | InputAction::Use | InputAction::PickItem => Category::Gameplay,
            InputAction::Inventory
            | InputAction::SwapOffhand
            | InputAction::Drop
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
            InputAction::Screenshot | InputAction::TogglePerspective | InputAction::Pause => {
                Category::Misc
            }
            InputAction::DebugOverlay
            | InputAction::DebugShowHitboxes
            | InputAction::DebugShowChunkBorders
            | InputAction::DebugShowAdvancedTooltips
            | InputAction::DebugSpectate
            | InputAction::DebugSwitchGameMode
            | InputAction::DebugFocusPause
            | InputAction::DebugCopyLocation => Category::Debug,
        }
    }

    /// Vanilla's default binding.
    ///
    /// Every keyboard default is the winit [`KeyCode`] for the GLFW keysym
    /// vanilla's own persisted-options declarations declares, and every mouse default is the
    /// mouse-button index it declares. The GLFW number is
    /// named in each comment so the mapping is checkable against the source
    /// without trusting this table.
    #[must_use]
    pub fn default_binding(self) -> Binding {
        match self {
            // vanilla's own persisted-options declarations — 87/83/65/68/32/340/341.
            InputAction::Forward => Binding::Key(Key::KeyW),
            InputAction::Back => Binding::Key(Key::KeyS),
            InputAction::Left => Binding::Key(Key::KeyA),
            InputAction::Right => Binding::Key(Key::KeyD),
            InputAction::Jump => Binding::Key(Key::Space),
            InputAction::Sneak => Binding::Key(Key::ShiftLeft),
            InputAction::Sprint => Binding::Key(Key::ControlLeft),
            // vanilla's own persisted-options declarations — mouse-button type, buttons 0 and 1. Note the *order*
            // in the source is `keyUse` (button 1) then `keyAttack` (button 0).
            InputAction::Attack => Binding::Mouse(MouseButton::Left),
            InputAction::Use => Binding::Mouse(MouseButton::Right),
            // vanilla's own persisted-options declarations — mouse-button type, button 2.
            InputAction::PickItem => Binding::Mouse(MouseButton::Middle),
            // vanilla's own persisted-options declarations — 69, 70 and 81.
            InputAction::Inventory => Binding::Key(Key::KeyE),
            InputAction::SwapOffhand => Binding::Key(Key::KeyF),
            InputAction::Drop => Binding::Key(Key::KeyQ),
            // vanilla's own persisted-options declarations — 49..57, i.e. the number row, not the keypad.
            InputAction::Hotbar1 => Binding::Key(Key::Digit1),
            InputAction::Hotbar2 => Binding::Key(Key::Digit2),
            InputAction::Hotbar3 => Binding::Key(Key::Digit3),
            InputAction::Hotbar4 => Binding::Key(Key::Digit4),
            InputAction::Hotbar5 => Binding::Key(Key::Digit5),
            InputAction::Hotbar6 => Binding::Key(Key::Digit6),
            InputAction::Hotbar7 => Binding::Key(Key::Digit7),
            InputAction::Hotbar8 => Binding::Key(Key::Digit8),
            InputAction::Hotbar9 => Binding::Key(Key::Digit9),
            // vanilla's own persisted-options declarations — 84/47/258.
            InputAction::Chat => Binding::Key(Key::KeyT),
            InputAction::Command => Binding::Key(Key::Slash),
            InputAction::PlayerList => Binding::Key(Key::Tab),
            // vanilla's own persisted-options declarations — 291.
            InputAction::Screenshot => Binding::Key(Key::F2),
            // vanilla's own persisted-options declarations — 294.
            InputAction::TogglePerspective => Binding::Key(Key::F5),
            // No vanilla counterpart (Escape is not a `KeyMapping`); GLFW 256.
            InputAction::Pause => Binding::Key(Key::Escape),
            // vanilla's own persisted-options declarations — 292.
            InputAction::DebugOverlay => Binding::Key(Key::F3),
            // vanilla's own persisted-options declarations's `debugKeys` — 66/71/72/78/293/80/67.
            InputAction::DebugShowHitboxes => Binding::Key(Key::KeyB),
            InputAction::DebugShowChunkBorders => Binding::Key(Key::KeyG),
            InputAction::DebugShowAdvancedTooltips => Binding::Key(Key::KeyH),
            InputAction::DebugSpectate => Binding::Key(Key::KeyN),
            InputAction::DebugSwitchGameMode => Binding::Key(Key::F4),
            InputAction::DebugFocusPause => Binding::Key(Key::KeyP),
            InputAction::DebugCopyLocation => Binding::Key(Key::KeyC),
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

/// A physical keyboard key position, mirroring winit's own `KeyCode` —
/// see the module docs on why identity is physical rather than logical.
///
/// This crate's own type, not a re-export: [`Binding`] is reachable from
/// [`crate::config`] and [`crate::menu::nav`], neither of which should have
/// to pull in `winit` (and, with the `window` Cargo feature off, cannot —
/// see `docs/runtime-presentation.md` on the winit-free headless build).
/// [`From<winit::keyboard::KeyCode>`](Key#impl-From<KeyCode>-for-Key) is the
/// one place a raw winit key becomes one of these, and it exists only when
/// the `window` feature is on — see the bottom of this file.
///
/// Every variant here is winit 0.30's own `KeyCode`, transcribed exactly
/// (same names, same physical keys) rather than picked by hand, so the
/// conversion below is a rename, not a redesign. `KeyCode` is
/// `#[non_exhaustive]` on winit's side (a future release can add a variant);
/// [`Key::Unknown`] is the sink for that case and is not reachable against
/// the pinned winit version this crate builds against today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Backquote,
    Backslash,
    BracketLeft,
    BracketRight,
    Comma,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Equal,
    IntlBackslash,
    IntlRo,
    IntlYen,
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
    Minus,
    Period,
    Quote,
    Semicolon,
    Slash,
    AltLeft,
    AltRight,
    Backspace,
    CapsLock,
    ContextMenu,
    ControlLeft,
    ControlRight,
    Enter,
    SuperLeft,
    SuperRight,
    ShiftLeft,
    ShiftRight,
    Space,
    Tab,
    Convert,
    KanaMode,
    Lang1,
    Lang2,
    Lang3,
    Lang4,
    Lang5,
    NonConvert,
    Delete,
    End,
    Help,
    Home,
    Insert,
    PageDown,
    PageUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    NumLock,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadAdd,
    NumpadBackspace,
    NumpadClear,
    NumpadClearEntry,
    NumpadComma,
    NumpadDecimal,
    NumpadDivide,
    NumpadEnter,
    NumpadEqual,
    NumpadHash,
    NumpadMemoryAdd,
    NumpadMemoryClear,
    NumpadMemoryRecall,
    NumpadMemoryStore,
    NumpadMemorySubtract,
    NumpadMultiply,
    NumpadParenLeft,
    NumpadParenRight,
    NumpadStar,
    NumpadSubtract,
    Escape,
    Fn,
    FnLock,
    PrintScreen,
    ScrollLock,
    Pause,
    BrowserBack,
    BrowserFavorites,
    BrowserForward,
    BrowserHome,
    BrowserRefresh,
    BrowserSearch,
    BrowserStop,
    Eject,
    LaunchApp1,
    LaunchApp2,
    LaunchMail,
    MediaPlayPause,
    MediaSelect,
    MediaStop,
    MediaTrackNext,
    MediaTrackPrevious,
    Power,
    Sleep,
    AudioVolumeDown,
    AudioVolumeMute,
    AudioVolumeUp,
    WakeUp,
    Meta,
    Hyper,
    Turbo,
    Abort,
    Resume,
    Suspend,
    Again,
    Copy,
    Cut,
    Find,
    Open,
    Paste,
    Props,
    Select,
    Undo,
    Hiragana,
    Katakana,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    F25,
    F26,
    F27,
    F28,
    F29,
    F30,
    F31,
    F32,
    F33,
    F34,
    F35,
    /// A physical key winit does not yet name as of the pinned version —
    /// see this type's own doc. Not constructed anywhere in this crate.
    Unknown,
}

/// A mouse button, mirroring winit's own `MouseButton` exactly (that type is
/// not `#[non_exhaustive]`, so this conversion is total). See [`Key`]'s doc
/// for why this crate keeps its own copy rather than depending on winit here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    /// The raw platform button index, for a button vanilla has no name for.
    Other(u16),
}

// ---------------------------------------------------------------------------
// winit conversions — confined to the windowed path.
//
// Everything above this point compiles with `winit` entirely out of the
// dependency graph (`cargo tree -p lodestone-shell --no-default-features -i
// winit` reports nothing). These two `From` impls are the one place a raw
// winit key/button becomes a `Key`/`MouseButton`, and they exist only behind
// the `window` Cargo feature — the same feature that gates `winit` itself as
// a dependency and the whole `app` module that is winit's real, unavoidable
// consumer (`ApplicationHandler`, `WindowEvent`, …). See
// `docs/runtime-presentation.md`'s "genuinely winit-free headless build"
// section.
// ---------------------------------------------------------------------------

#[cfg(feature = "window")]
impl From<winit::keyboard::KeyCode> for Key {
    fn from(code: winit::keyboard::KeyCode) -> Self {
        match code {
                winit::keyboard::KeyCode::Backquote => Key::Backquote,
                winit::keyboard::KeyCode::Backslash => Key::Backslash,
                winit::keyboard::KeyCode::BracketLeft => Key::BracketLeft,
                winit::keyboard::KeyCode::BracketRight => Key::BracketRight,
                winit::keyboard::KeyCode::Comma => Key::Comma,
                winit::keyboard::KeyCode::Digit0 => Key::Digit0,
                winit::keyboard::KeyCode::Digit1 => Key::Digit1,
                winit::keyboard::KeyCode::Digit2 => Key::Digit2,
                winit::keyboard::KeyCode::Digit3 => Key::Digit3,
                winit::keyboard::KeyCode::Digit4 => Key::Digit4,
                winit::keyboard::KeyCode::Digit5 => Key::Digit5,
                winit::keyboard::KeyCode::Digit6 => Key::Digit6,
                winit::keyboard::KeyCode::Digit7 => Key::Digit7,
                winit::keyboard::KeyCode::Digit8 => Key::Digit8,
                winit::keyboard::KeyCode::Digit9 => Key::Digit9,
                winit::keyboard::KeyCode::Equal => Key::Equal,
                winit::keyboard::KeyCode::IntlBackslash => Key::IntlBackslash,
                winit::keyboard::KeyCode::IntlRo => Key::IntlRo,
                winit::keyboard::KeyCode::IntlYen => Key::IntlYen,
                winit::keyboard::KeyCode::KeyA => Key::KeyA,
                winit::keyboard::KeyCode::KeyB => Key::KeyB,
                winit::keyboard::KeyCode::KeyC => Key::KeyC,
                winit::keyboard::KeyCode::KeyD => Key::KeyD,
                winit::keyboard::KeyCode::KeyE => Key::KeyE,
                winit::keyboard::KeyCode::KeyF => Key::KeyF,
                winit::keyboard::KeyCode::KeyG => Key::KeyG,
                winit::keyboard::KeyCode::KeyH => Key::KeyH,
                winit::keyboard::KeyCode::KeyI => Key::KeyI,
                winit::keyboard::KeyCode::KeyJ => Key::KeyJ,
                winit::keyboard::KeyCode::KeyK => Key::KeyK,
                winit::keyboard::KeyCode::KeyL => Key::KeyL,
                winit::keyboard::KeyCode::KeyM => Key::KeyM,
                winit::keyboard::KeyCode::KeyN => Key::KeyN,
                winit::keyboard::KeyCode::KeyO => Key::KeyO,
                winit::keyboard::KeyCode::KeyP => Key::KeyP,
                winit::keyboard::KeyCode::KeyQ => Key::KeyQ,
                winit::keyboard::KeyCode::KeyR => Key::KeyR,
                winit::keyboard::KeyCode::KeyS => Key::KeyS,
                winit::keyboard::KeyCode::KeyT => Key::KeyT,
                winit::keyboard::KeyCode::KeyU => Key::KeyU,
                winit::keyboard::KeyCode::KeyV => Key::KeyV,
                winit::keyboard::KeyCode::KeyW => Key::KeyW,
                winit::keyboard::KeyCode::KeyX => Key::KeyX,
                winit::keyboard::KeyCode::KeyY => Key::KeyY,
                winit::keyboard::KeyCode::KeyZ => Key::KeyZ,
                winit::keyboard::KeyCode::Minus => Key::Minus,
                winit::keyboard::KeyCode::Period => Key::Period,
                winit::keyboard::KeyCode::Quote => Key::Quote,
                winit::keyboard::KeyCode::Semicolon => Key::Semicolon,
                winit::keyboard::KeyCode::Slash => Key::Slash,
                winit::keyboard::KeyCode::AltLeft => Key::AltLeft,
                winit::keyboard::KeyCode::AltRight => Key::AltRight,
                winit::keyboard::KeyCode::Backspace => Key::Backspace,
                winit::keyboard::KeyCode::CapsLock => Key::CapsLock,
                winit::keyboard::KeyCode::ContextMenu => Key::ContextMenu,
                winit::keyboard::KeyCode::ControlLeft => Key::ControlLeft,
                winit::keyboard::KeyCode::ControlRight => Key::ControlRight,
                winit::keyboard::KeyCode::Enter => Key::Enter,
                winit::keyboard::KeyCode::SuperLeft => Key::SuperLeft,
                winit::keyboard::KeyCode::SuperRight => Key::SuperRight,
                winit::keyboard::KeyCode::ShiftLeft => Key::ShiftLeft,
                winit::keyboard::KeyCode::ShiftRight => Key::ShiftRight,
                winit::keyboard::KeyCode::Space => Key::Space,
                winit::keyboard::KeyCode::Tab => Key::Tab,
                winit::keyboard::KeyCode::Convert => Key::Convert,
                winit::keyboard::KeyCode::KanaMode => Key::KanaMode,
                winit::keyboard::KeyCode::Lang1 => Key::Lang1,
                winit::keyboard::KeyCode::Lang2 => Key::Lang2,
                winit::keyboard::KeyCode::Lang3 => Key::Lang3,
                winit::keyboard::KeyCode::Lang4 => Key::Lang4,
                winit::keyboard::KeyCode::Lang5 => Key::Lang5,
                winit::keyboard::KeyCode::NonConvert => Key::NonConvert,
                winit::keyboard::KeyCode::Delete => Key::Delete,
                winit::keyboard::KeyCode::End => Key::End,
                winit::keyboard::KeyCode::Help => Key::Help,
                winit::keyboard::KeyCode::Home => Key::Home,
                winit::keyboard::KeyCode::Insert => Key::Insert,
                winit::keyboard::KeyCode::PageDown => Key::PageDown,
                winit::keyboard::KeyCode::PageUp => Key::PageUp,
                winit::keyboard::KeyCode::ArrowDown => Key::ArrowDown,
                winit::keyboard::KeyCode::ArrowLeft => Key::ArrowLeft,
                winit::keyboard::KeyCode::ArrowRight => Key::ArrowRight,
                winit::keyboard::KeyCode::ArrowUp => Key::ArrowUp,
                winit::keyboard::KeyCode::NumLock => Key::NumLock,
                winit::keyboard::KeyCode::Numpad0 => Key::Numpad0,
                winit::keyboard::KeyCode::Numpad1 => Key::Numpad1,
                winit::keyboard::KeyCode::Numpad2 => Key::Numpad2,
                winit::keyboard::KeyCode::Numpad3 => Key::Numpad3,
                winit::keyboard::KeyCode::Numpad4 => Key::Numpad4,
                winit::keyboard::KeyCode::Numpad5 => Key::Numpad5,
                winit::keyboard::KeyCode::Numpad6 => Key::Numpad6,
                winit::keyboard::KeyCode::Numpad7 => Key::Numpad7,
                winit::keyboard::KeyCode::Numpad8 => Key::Numpad8,
                winit::keyboard::KeyCode::Numpad9 => Key::Numpad9,
                winit::keyboard::KeyCode::NumpadAdd => Key::NumpadAdd,
                winit::keyboard::KeyCode::NumpadBackspace => Key::NumpadBackspace,
                winit::keyboard::KeyCode::NumpadClear => Key::NumpadClear,
                winit::keyboard::KeyCode::NumpadClearEntry => Key::NumpadClearEntry,
                winit::keyboard::KeyCode::NumpadComma => Key::NumpadComma,
                winit::keyboard::KeyCode::NumpadDecimal => Key::NumpadDecimal,
                winit::keyboard::KeyCode::NumpadDivide => Key::NumpadDivide,
                winit::keyboard::KeyCode::NumpadEnter => Key::NumpadEnter,
                winit::keyboard::KeyCode::NumpadEqual => Key::NumpadEqual,
                winit::keyboard::KeyCode::NumpadHash => Key::NumpadHash,
                winit::keyboard::KeyCode::NumpadMemoryAdd => Key::NumpadMemoryAdd,
                winit::keyboard::KeyCode::NumpadMemoryClear => Key::NumpadMemoryClear,
                winit::keyboard::KeyCode::NumpadMemoryRecall => Key::NumpadMemoryRecall,
                winit::keyboard::KeyCode::NumpadMemoryStore => Key::NumpadMemoryStore,
                winit::keyboard::KeyCode::NumpadMemorySubtract => Key::NumpadMemorySubtract,
                winit::keyboard::KeyCode::NumpadMultiply => Key::NumpadMultiply,
                winit::keyboard::KeyCode::NumpadParenLeft => Key::NumpadParenLeft,
                winit::keyboard::KeyCode::NumpadParenRight => Key::NumpadParenRight,
                winit::keyboard::KeyCode::NumpadStar => Key::NumpadStar,
                winit::keyboard::KeyCode::NumpadSubtract => Key::NumpadSubtract,
                winit::keyboard::KeyCode::Escape => Key::Escape,
                winit::keyboard::KeyCode::Fn => Key::Fn,
                winit::keyboard::KeyCode::FnLock => Key::FnLock,
                winit::keyboard::KeyCode::PrintScreen => Key::PrintScreen,
                winit::keyboard::KeyCode::ScrollLock => Key::ScrollLock,
                winit::keyboard::KeyCode::Pause => Key::Pause,
                winit::keyboard::KeyCode::BrowserBack => Key::BrowserBack,
                winit::keyboard::KeyCode::BrowserFavorites => Key::BrowserFavorites,
                winit::keyboard::KeyCode::BrowserForward => Key::BrowserForward,
                winit::keyboard::KeyCode::BrowserHome => Key::BrowserHome,
                winit::keyboard::KeyCode::BrowserRefresh => Key::BrowserRefresh,
                winit::keyboard::KeyCode::BrowserSearch => Key::BrowserSearch,
                winit::keyboard::KeyCode::BrowserStop => Key::BrowserStop,
                winit::keyboard::KeyCode::Eject => Key::Eject,
                winit::keyboard::KeyCode::LaunchApp1 => Key::LaunchApp1,
                winit::keyboard::KeyCode::LaunchApp2 => Key::LaunchApp2,
                winit::keyboard::KeyCode::LaunchMail => Key::LaunchMail,
                winit::keyboard::KeyCode::MediaPlayPause => Key::MediaPlayPause,
                winit::keyboard::KeyCode::MediaSelect => Key::MediaSelect,
                winit::keyboard::KeyCode::MediaStop => Key::MediaStop,
                winit::keyboard::KeyCode::MediaTrackNext => Key::MediaTrackNext,
                winit::keyboard::KeyCode::MediaTrackPrevious => Key::MediaTrackPrevious,
                winit::keyboard::KeyCode::Power => Key::Power,
                winit::keyboard::KeyCode::Sleep => Key::Sleep,
                winit::keyboard::KeyCode::AudioVolumeDown => Key::AudioVolumeDown,
                winit::keyboard::KeyCode::AudioVolumeMute => Key::AudioVolumeMute,
                winit::keyboard::KeyCode::AudioVolumeUp => Key::AudioVolumeUp,
                winit::keyboard::KeyCode::WakeUp => Key::WakeUp,
                winit::keyboard::KeyCode::Meta => Key::Meta,
                winit::keyboard::KeyCode::Hyper => Key::Hyper,
                winit::keyboard::KeyCode::Turbo => Key::Turbo,
                winit::keyboard::KeyCode::Abort => Key::Abort,
                winit::keyboard::KeyCode::Resume => Key::Resume,
                winit::keyboard::KeyCode::Suspend => Key::Suspend,
                winit::keyboard::KeyCode::Again => Key::Again,
                winit::keyboard::KeyCode::Copy => Key::Copy,
                winit::keyboard::KeyCode::Cut => Key::Cut,
                winit::keyboard::KeyCode::Find => Key::Find,
                winit::keyboard::KeyCode::Open => Key::Open,
                winit::keyboard::KeyCode::Paste => Key::Paste,
                winit::keyboard::KeyCode::Props => Key::Props,
                winit::keyboard::KeyCode::Select => Key::Select,
                winit::keyboard::KeyCode::Undo => Key::Undo,
                winit::keyboard::KeyCode::Hiragana => Key::Hiragana,
                winit::keyboard::KeyCode::Katakana => Key::Katakana,
                winit::keyboard::KeyCode::F1 => Key::F1,
                winit::keyboard::KeyCode::F2 => Key::F2,
                winit::keyboard::KeyCode::F3 => Key::F3,
                winit::keyboard::KeyCode::F4 => Key::F4,
                winit::keyboard::KeyCode::F5 => Key::F5,
                winit::keyboard::KeyCode::F6 => Key::F6,
                winit::keyboard::KeyCode::F7 => Key::F7,
                winit::keyboard::KeyCode::F8 => Key::F8,
                winit::keyboard::KeyCode::F9 => Key::F9,
                winit::keyboard::KeyCode::F10 => Key::F10,
                winit::keyboard::KeyCode::F11 => Key::F11,
                winit::keyboard::KeyCode::F12 => Key::F12,
                winit::keyboard::KeyCode::F13 => Key::F13,
                winit::keyboard::KeyCode::F14 => Key::F14,
                winit::keyboard::KeyCode::F15 => Key::F15,
                winit::keyboard::KeyCode::F16 => Key::F16,
                winit::keyboard::KeyCode::F17 => Key::F17,
                winit::keyboard::KeyCode::F18 => Key::F18,
                winit::keyboard::KeyCode::F19 => Key::F19,
                winit::keyboard::KeyCode::F20 => Key::F20,
                winit::keyboard::KeyCode::F21 => Key::F21,
                winit::keyboard::KeyCode::F22 => Key::F22,
                winit::keyboard::KeyCode::F23 => Key::F23,
                winit::keyboard::KeyCode::F24 => Key::F24,
                winit::keyboard::KeyCode::F25 => Key::F25,
                winit::keyboard::KeyCode::F26 => Key::F26,
                winit::keyboard::KeyCode::F27 => Key::F27,
                winit::keyboard::KeyCode::F28 => Key::F28,
                winit::keyboard::KeyCode::F29 => Key::F29,
                winit::keyboard::KeyCode::F30 => Key::F30,
                winit::keyboard::KeyCode::F31 => Key::F31,
                winit::keyboard::KeyCode::F32 => Key::F32,
                winit::keyboard::KeyCode::F33 => Key::F33,
                winit::keyboard::KeyCode::F34 => Key::F34,
                winit::keyboard::KeyCode::F35 => Key::F35,
                // Non-exhaustive on winit's side only — every variant that
                // exists in the pinned winit version is matched above.
                _ => Key::Unknown,
        }
    }
}

#[cfg(feature = "window")]
impl From<winit::event::MouseButton> for MouseButton {
    fn from(button: winit::event::MouseButton) -> Self {
        match button {
            winit::event::MouseButton::Left => MouseButton::Left,
            winit::event::MouseButton::Right => MouseButton::Right,
            winit::event::MouseButton::Middle => MouseButton::Middle,
            winit::event::MouseButton::Back => MouseButton::Back,
            winit::event::MouseButton::Forward => MouseButton::Forward,
            winit::event::MouseButton::Other(n) => MouseButton::Other(n),
        }
    }
}

/// What the player presses to invoke an [`InputAction`].
///
/// Covers a keyboard key **and a mouse button**, because vanilla binds
/// `key.attack` and `key.use` to mouse buttons and both are rebindable — a
/// `KeyCode`-only table cannot express the default configuration, let alone a
/// rebind of it.
///
/// ## Why there is no `Scroll` variant
///
/// Checked rather than assumed. Vanilla's own input-source classification has
/// exactly three kinds of binding target — a named keysym, a raw scancode,
/// and a mouse button — there is no scroll type, so no vanilla key binding
/// can be bound to a wheel direction. The one thing this client does with the
/// wheel is cycle the hotbar, which vanilla also handles outside the binding
/// table (as part of its mouse handling, not as a bindable key), so nothing
/// in the shell needs it. Adding a `Scroll(ScrollDirection)` variant later is
/// a local change: the persisted format is a string, [`Binding::parse`]
/// already returns `None` for names it does not know, and unknown names fall
/// back to the default rather than failing the load.
///
/// A raw scancode kind is likewise absent: it is vanilla's fallback for keys
/// GLFW cannot name, and winit's [`KeyCode`] already *is* a physical-position
/// identity, so there is no second identity to fall back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    /// Bound to nothing. Matches vanilla's own unbound-key sentinel; such a
    /// binding never matches any input.
    Unbound,
    /// A **physical** key position — see the module docs on non-QWERTY layouts.
    Key(Key),
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
/// The vanilla-named entries are transcribed from vanilla's own key-name
/// table verbatim; the `winit.*`-namespaced tail covers physical keys winit can report
/// that GLFW/vanilla has no name for, so that saving a binding to one is
/// lossless instead of quietly reverting on the next load.
///
/// One table serves both directions (see [`key_name`] / [`key_from_name`]) so a
/// name and its reverse cannot drift. Scanned linearly, which is fine: it is
/// touched only on save and load, never per keypress — matching a binding is a
/// [`Binding`] equality test that never consults this table.
const KEY_NAMES: &[(Key, &str)] = &[
    // Number row.
    (Key::Digit0, "key.keyboard.0"),
    (Key::Digit1, "key.keyboard.1"),
    (Key::Digit2, "key.keyboard.2"),
    (Key::Digit3, "key.keyboard.3"),
    (Key::Digit4, "key.keyboard.4"),
    (Key::Digit5, "key.keyboard.5"),
    (Key::Digit6, "key.keyboard.6"),
    (Key::Digit7, "key.keyboard.7"),
    (Key::Digit8, "key.keyboard.8"),
    (Key::Digit9, "key.keyboard.9"),
    // Letters.
    (Key::KeyA, "key.keyboard.a"),
    (Key::KeyB, "key.keyboard.b"),
    (Key::KeyC, "key.keyboard.c"),
    (Key::KeyD, "key.keyboard.d"),
    (Key::KeyE, "key.keyboard.e"),
    (Key::KeyF, "key.keyboard.f"),
    (Key::KeyG, "key.keyboard.g"),
    (Key::KeyH, "key.keyboard.h"),
    (Key::KeyI, "key.keyboard.i"),
    (Key::KeyJ, "key.keyboard.j"),
    (Key::KeyK, "key.keyboard.k"),
    (Key::KeyL, "key.keyboard.l"),
    (Key::KeyM, "key.keyboard.m"),
    (Key::KeyN, "key.keyboard.n"),
    (Key::KeyO, "key.keyboard.o"),
    (Key::KeyP, "key.keyboard.p"),
    (Key::KeyQ, "key.keyboard.q"),
    (Key::KeyR, "key.keyboard.r"),
    (Key::KeyS, "key.keyboard.s"),
    (Key::KeyT, "key.keyboard.t"),
    (Key::KeyU, "key.keyboard.u"),
    (Key::KeyV, "key.keyboard.v"),
    (Key::KeyW, "key.keyboard.w"),
    (Key::KeyX, "key.keyboard.x"),
    (Key::KeyY, "key.keyboard.y"),
    (Key::KeyZ, "key.keyboard.z"),
    // Function row. Vanilla names f1..f25; winit reports up to F35, and the
    // tail is covered by the `winit.*` entries below.
    (Key::F1, "key.keyboard.f1"),
    (Key::F2, "key.keyboard.f2"),
    (Key::F3, "key.keyboard.f3"),
    (Key::F4, "key.keyboard.f4"),
    (Key::F5, "key.keyboard.f5"),
    (Key::F6, "key.keyboard.f6"),
    (Key::F7, "key.keyboard.f7"),
    (Key::F8, "key.keyboard.f8"),
    (Key::F9, "key.keyboard.f9"),
    (Key::F10, "key.keyboard.f10"),
    (Key::F11, "key.keyboard.f11"),
    (Key::F12, "key.keyboard.f12"),
    (Key::F13, "key.keyboard.f13"),
    (Key::F14, "key.keyboard.f14"),
    (Key::F15, "key.keyboard.f15"),
    (Key::F16, "key.keyboard.f16"),
    (Key::F17, "key.keyboard.f17"),
    (Key::F18, "key.keyboard.f18"),
    (Key::F19, "key.keyboard.f19"),
    (Key::F20, "key.keyboard.f20"),
    (Key::F21, "key.keyboard.f21"),
    (Key::F22, "key.keyboard.f22"),
    (Key::F23, "key.keyboard.f23"),
    (Key::F24, "key.keyboard.f24"),
    (Key::F25, "key.keyboard.f25"),
    // Keypad.
    (Key::NumLock, "key.keyboard.num.lock"),
    (Key::Numpad0, "key.keyboard.keypad.0"),
    (Key::Numpad1, "key.keyboard.keypad.1"),
    (Key::Numpad2, "key.keyboard.keypad.2"),
    (Key::Numpad3, "key.keyboard.keypad.3"),
    (Key::Numpad4, "key.keyboard.keypad.4"),
    (Key::Numpad5, "key.keyboard.keypad.5"),
    (Key::Numpad6, "key.keyboard.keypad.6"),
    (Key::Numpad7, "key.keyboard.keypad.7"),
    (Key::Numpad8, "key.keyboard.keypad.8"),
    (Key::Numpad9, "key.keyboard.keypad.9"),
    (Key::NumpadAdd, "key.keyboard.keypad.add"),
    (Key::NumpadDecimal, "key.keyboard.keypad.decimal"),
    (Key::NumpadEnter, "key.keyboard.keypad.enter"),
    (Key::NumpadEqual, "key.keyboard.keypad.equal"),
    (Key::NumpadMultiply, "key.keyboard.keypad.multiply"),
    (Key::NumpadDivide, "key.keyboard.keypad.divide"),
    (Key::NumpadSubtract, "key.keyboard.keypad.subtract"),
    // Arrows.
    (Key::ArrowDown, "key.keyboard.down"),
    (Key::ArrowLeft, "key.keyboard.left"),
    (Key::ArrowRight, "key.keyboard.right"),
    (Key::ArrowUp, "key.keyboard.up"),
    // Punctuation. Note winit's `Quote`/`Backquote` are vanilla's
    // `apostrophe`/`grave.accent` — the names do not line up, which is exactly
    // why this is a table and not a `format!`.
    (Key::Quote, "key.keyboard.apostrophe"),
    (Key::Backslash, "key.keyboard.backslash"),
    (Key::Comma, "key.keyboard.comma"),
    (Key::Equal, "key.keyboard.equal"),
    (Key::Backquote, "key.keyboard.grave.accent"),
    (Key::BracketLeft, "key.keyboard.left.bracket"),
    (Key::Minus, "key.keyboard.minus"),
    (Key::Period, "key.keyboard.period"),
    (Key::BracketRight, "key.keyboard.right.bracket"),
    (Key::Semicolon, "key.keyboard.semicolon"),
    (Key::Slash, "key.keyboard.slash"),
    (Key::Space, "key.keyboard.space"),
    (Key::Tab, "key.keyboard.tab"),
    // Modifiers. Vanilla says `win` where winit says `Super`.
    (Key::AltLeft, "key.keyboard.left.alt"),
    (Key::ControlLeft, "key.keyboard.left.control"),
    (Key::ShiftLeft, "key.keyboard.left.shift"),
    (Key::SuperLeft, "key.keyboard.left.win"),
    (Key::AltRight, "key.keyboard.right.alt"),
    (Key::ControlRight, "key.keyboard.right.control"),
    (Key::ShiftRight, "key.keyboard.right.shift"),
    (Key::SuperRight, "key.keyboard.right.win"),
    // Editing / navigation.
    (Key::Enter, "key.keyboard.enter"),
    (Key::Escape, "key.keyboard.escape"),
    (Key::Backspace, "key.keyboard.backspace"),
    (Key::Delete, "key.keyboard.delete"),
    (Key::End, "key.keyboard.end"),
    (Key::Home, "key.keyboard.home"),
    (Key::Insert, "key.keyboard.insert"),
    (Key::PageDown, "key.keyboard.page.down"),
    (Key::PageUp, "key.keyboard.page.up"),
    (Key::CapsLock, "key.keyboard.caps.lock"),
    (Key::Pause, "key.keyboard.pause"),
    (Key::ScrollLock, "key.keyboard.scroll.lock"),
    (Key::ContextMenu, "key.keyboard.menu"),
    (Key::PrintScreen, "key.keyboard.print.screen"),
    // -- keys vanilla has no name for -------------------------------------
    // Namespaced so a rebind to one survives a save/load cycle. Vanilla's
    // `world.1`/`world.2` (GLFW 161/162) are deliberately *not* claimed for the
    // `Intl*` keys: GLFW's WORLD_1/2 and winit's IntlBackslash/IntlRo/IntlYen
    // are not the same keys, and asserting an equivalence we have not measured
    // is how a binding ends up landing on the wrong physical key.
    (Key::IntlBackslash, "key.keyboard.winit.intl.backslash"),
    (Key::IntlRo, "key.keyboard.winit.intl.ro"),
    (Key::IntlYen, "key.keyboard.winit.intl.yen"),
    (Key::Fn, "key.keyboard.winit.fn"),
    (Key::FnLock, "key.keyboard.winit.fn.lock"),
    (Key::Help, "key.keyboard.winit.help"),
    (Key::Convert, "key.keyboard.winit.convert"),
    (Key::NonConvert, "key.keyboard.winit.non.convert"),
    (Key::KanaMode, "key.keyboard.winit.kana.mode"),
    (Key::NumpadComma, "key.keyboard.winit.keypad.comma"),
    (Key::NumpadStar, "key.keyboard.winit.keypad.star"),
    (Key::NumpadHash, "key.keyboard.winit.keypad.hash"),
    (Key::NumpadBackspace, "key.keyboard.winit.keypad.backspace"),
    (Key::NumpadClear, "key.keyboard.winit.keypad.clear"),
    (Key::NumpadParenLeft, "key.keyboard.winit.keypad.paren.left"),
    (Key::NumpadParenRight, "key.keyboard.winit.keypad.paren.right"),
    (Key::F26, "key.keyboard.winit.f26"),
    (Key::F27, "key.keyboard.winit.f27"),
    (Key::F28, "key.keyboard.winit.f28"),
    (Key::F29, "key.keyboard.winit.f29"),
    (Key::F30, "key.keyboard.winit.f30"),
    (Key::F31, "key.keyboard.winit.f31"),
    (Key::F32, "key.keyboard.winit.f32"),
    (Key::F33, "key.keyboard.winit.f33"),
    (Key::F34, "key.keyboard.winit.f34"),
    (Key::F35, "key.keyboard.winit.f35"),
];

/// The persisted name for a physical key.
///
/// Falls back to a `winit.unknown.*` name for a [`KeyCode`] not in
/// [`KEY_NAMES`] — winit's `KeyCode` is `#[non_exhaustive]`, so this cannot be
/// an exhaustive match and a future winit release can add variants. Such a name
/// does not parse back (there is no reverse entry), so it reverts to the default
/// on load: a lossy round-trip, but a *quiet, non-fatal* one, and adding the key
/// to the table above fixes it.
fn key_name(code: Key) -> &'static str {
    match KEY_NAMES.iter().find(|(c, _)| *c == code) {
        Some((_, name)) => name,
        None => "key.keyboard.winit.unknown",
    }
}

/// Reverse of [`key_name`]. `None` for an unrecognised name.
fn key_from_name(name: &str) -> Option<Key> {
    KEY_NAMES
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(code, _)| *code)
}

/// The persisted name for a mouse button.
///
/// Vanilla names buttons 0/1/2 `left`/`right`/`middle` and numbers the rest
/// `key.mouse.<n + 1>`, so winit's
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
    pub fn is(&self, action: InputAction, code: Key) -> bool {
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
    // The raw winit type, distinct from this module's own `Key`: these tests
    // build a real winit `KeyCode` and run it through `Key::from` so the
    // conversion table itself is exercised, not just `Key`'s own variants.
    use winit::keyboard::KeyCode;

    // -- defaults, against the decompiled source ---------------------------
    //
    // The expected values below are **GLFW keysyms transcribed from
    // vanilla's own persisted-options declarations**, paired with the winit `KeyCode` for that
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
            81 => KeyCode::KeyQ,
            83 => KeyCode::KeyS,
            84 => KeyCode::KeyT,
            87 => KeyCode::KeyW,
            256 => KeyCode::Escape,
            258 => KeyCode::Tab,
            291 => KeyCode::F2,
            292 => KeyCode::F3,
            293 => KeyCode::F4,
            294 => KeyCode::F5,
            // The seven `debugKeys` chords.
            66 => KeyCode::KeyB,
            67 => KeyCode::KeyC,
            71 => KeyCode::KeyG,
            72 => KeyCode::KeyH,
            78 => KeyCode::KeyN,
            80 => KeyCode::KeyP,
            340 => KeyCode::ShiftLeft,
            341 => KeyCode::ControlLeft,
            other => panic!("no winit mapping recorded for GLFW keysym {other}"),
        }
    }

    #[test]
    fn defaults_match_the_decompiled_vanilla_key_mappings() {
        // Transcribed from the decompiled client's own default keybind
        // table: (action, GLFW keysym, key-mapping category).
        let vanilla: &[(InputAction, u32, Category)] = &[
            (InputAction::Forward, 87, Category::Movement),
            (InputAction::Left, 65, Category::Movement),
            (InputAction::Back, 83, Category::Movement),
            (InputAction::Right, 68, Category::Movement),
            (InputAction::Jump, 32, Category::Movement),
            (InputAction::Sneak, 340, Category::Movement),
            (InputAction::Sprint, 341, Category::Movement),
            (InputAction::Inventory, 69, Category::Inventory),
            (InputAction::Drop, 81, Category::Inventory),
            (InputAction::Chat, 84, Category::Multiplayer),
            (InputAction::PlayerList, 258, Category::Multiplayer),
            (InputAction::Command, 47, Category::Multiplayer),
            (InputAction::Screenshot, 291, Category::Misc),
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
            // vanilla's own persisted-options declarations — a real key binding in 26.2, category debug.
            (InputAction::DebugOverlay, 292, Category::Debug),
            // The debug-only bindings below are, in vanilla, folded into the
            // ordinary binding table and dispatched through the same
            // per-frame key-matching pass as every other binding —
            // which is why they are in this table rather than literal
            // `KeyCode`s in `resolve_key`.
            (InputAction::DebugShowHitboxes, 66, Category::Debug),
            (InputAction::DebugShowChunkBorders, 71, Category::Debug),
            (InputAction::DebugShowAdvancedTooltips, 72, Category::Debug),
            (InputAction::DebugSpectate, 78, Category::Debug),
            (InputAction::DebugSwitchGameMode, 293, Category::Debug),
            (InputAction::DebugFocusPause, 80, Category::Debug),
            (InputAction::DebugCopyLocation, 67, Category::Debug),
        ];
        for &(action, keysym, category) in vanilla {
            assert_eq!(
                action.default_binding(),
                Binding::Key(winit_code_for_glfw(keysym).into()),
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

        // The three mouse defaults: vanilla's own persisted-options declarations declares
        // `keyUse` as mouse-button type button 1, `keyAttack` as mouse-button type button 0 and
        // `keyPickItem` as mouse-button type button 2; vanilla's own input-constants module names
        // those `left`, `right` and `middle`.
        assert_eq!(
            InputAction::Attack.default_binding(),
            Binding::Mouse(MouseButton::Left)
        );
        assert_eq!(
            InputAction::Use.default_binding(),
            Binding::Mouse(MouseButton::Right)
        );
        assert_eq!(
            InputAction::PickItem.default_binding(),
            Binding::Mouse(MouseButton::Middle)
        );
        assert_eq!(InputAction::Attack.category(), Category::Gameplay);
        assert_eq!(InputAction::Use.category(), Category::Gameplay);
        assert_eq!(InputAction::PickItem.category(), Category::Gameplay);
    }

    #[test]
    fn a_binding_can_hold_a_mouse_button_not_just_a_key() {
        // The requirement that rules out a `KeyCode`-only table: vanilla's
        // *default* configuration cannot be expressed without this, since
        // attack and use are mouse-bound out of the box.
        let binds = Keybinds::new();
        assert!(binds.is_mouse(InputAction::Attack, MouseButton::Left));
        assert!(!binds.is(InputAction::Attack, KeyCode::KeyW.into()));

        // …and a mouse button can be rebound to a key, and vice versa.
        let mut binds = binds;
        binds.set(InputAction::Attack, Binding::Key(KeyCode::KeyR.into()));
        assert!(binds.is(InputAction::Attack, KeyCode::KeyR.into()));
        assert!(!binds.is_mouse(InputAction::Attack, MouseButton::Left));

        binds.set(InputAction::Jump, Binding::Mouse(MouseButton::Middle));
        assert!(binds.is_mouse(InputAction::Jump, MouseButton::Middle));
        assert!(!binds.is(InputAction::Jump, KeyCode::Space.into()));
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
        assert!(binds.is(InputAction::Inventory, KeyCode::KeyE.into()));

        binds.set(InputAction::Inventory, Binding::Key(KeyCode::KeyI.into()));
        assert!(binds.is(InputAction::Inventory, KeyCode::KeyI.into()));
        // The half that a naive implementation gets wrong: the *old* key must
        // stop firing, not merely the new one start.
        assert!(!binds.is(InputAction::Inventory, KeyCode::KeyE.into()));
        assert!(!binds.is_default(InputAction::Inventory));

        binds.reset(InputAction::Inventory);
        assert!(binds.is(InputAction::Inventory, KeyCode::KeyE.into()));
        assert!(binds.is_default(InputAction::Inventory));
    }

    #[test]
    fn an_unbound_action_never_fires() {
        let mut binds = Keybinds::new();
        binds.set(InputAction::Jump, Binding::Unbound);
        assert!(!binds.is(InputAction::Jump, KeyCode::Space.into()));
        // Nor on any other key — an `Unbound` that compared equal to something
        // would fire on whatever that something was.
        for code in [KeyCode::KeyW, KeyCode::Escape, KeyCode::F3].map(Key::from) {
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
        binds.set(InputAction::Jump, Binding::Key(KeyCode::KeyE.into()));
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
                .actions_bound_to(Binding::Key(KeyCode::KeyE.into()))
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
        // Matches vanilla's own key-binding registration order. The point of
        // pinning this: the order is *not* alphabetical and not what a
        // reader would guess — MISC is second.
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
        // Inventory: `key.inventory`, `key.swapOffhand`, `key.drop`, and the
        // nine hotbar slots — all twelve of vanilla's own inventory-category
        // bindings.
        assert_eq!(Keybinds::in_category(Category::Inventory).count(), 12);
        // Misc lost a member to that fix (`key.lodestone.toggleFly` is gone) and
        // gained one back here (`key.screenshot`, that fix): `key.screenshot`,
        // `key.togglePerspective` and this client's non-vanilla pause entry.
        assert_eq!(Keybinds::in_category(Category::Misc).count(), 3);
    }

    // -- persistence -------------------------------------------------------

    #[test]
    fn the_persisted_form_is_this_exact_literal_string() {
        // `parse(write(x)) == x` is satisfied by two symmetric misunderstandings
        // (`CLAUDE.md`'s round-trip warning), so the format is pinned against a
        // literal written out by hand from vanilla's own vocabulary — the
        // `key_key.forward:key.keyboard.w` shape of vanilla's own
        // persisted-options declarations and its own key-name table, as JSON.
        let mut binds = Keybinds::new();
        binds.set(InputAction::Forward, Binding::Key(KeyCode::ArrowUp.into()));
        binds.set(InputAction::Sneak, Binding::Key(KeyCode::ShiftRight.into()));
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
        assert!(binds.is(InputAction::Forward, KeyCode::ArrowUp.into()));
        assert!(binds.is_mouse(InputAction::Jump, MouseButton::Middle));
        assert!(binds.is(InputAction::DebugOverlay, KeyCode::F7.into()));
        assert!(binds.is(InputAction::Hotbar9, KeyCode::Numpad9.into()));
        assert!(binds.is(InputAction::Pause, KeyCode::Backquote.into()));
        // Untouched actions keep their defaults.
        assert!(binds.is(InputAction::Back, KeyCode::KeyS.into()));
        assert!(binds.is_mouse(InputAction::Attack, MouseButton::Left));
    }

    #[test]
    fn a_round_trip_is_stable_across_repeated_saves() {
        let mut binds = Keybinds::new();
        binds.set(InputAction::Forward, Binding::Key(KeyCode::ArrowUp.into()));
        binds.set(InputAction::Attack, Binding::Key(KeyCode::KeyR.into()));
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
        //, not `key.mouse.3`.
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
            "key.advancements": "key.keyboard.q",
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
        assert!(binds.is(InputAction::Chat, KeyCode::KeyT.into()), "non-string value");
        assert!(binds.is(InputAction::Jump, KeyCode::Space.into()), "null value");
        assert!(
            binds.is(InputAction::Left, KeyCode::KeyA.into()),
            "an unsupported binding *type* must also fall back, not error"
        );
        // …and unknown *action* names were simply ignored. `key.advancements`
        // is deliberately still absent from this table (see `InputAction`'s
        // own module doc) — `key.pickItem` used to fill this slot before it
        // gained an `InputAction::PickItem` of its own (and `key.drop` filled
        // it before that), which would have made this line silently start
        // asserting something else instead of failing loudly.
        assert!(InputAction::from_name("key.advancements").is_none());

        // The load kept going: every good entry after a bad one still applied.
        assert!(
            binds.is(InputAction::Back, KeyCode::ArrowUp.into()),
            "the entry after a bad one was dropped — the parse was poisoned"
        );
        assert!(binds.is(InputAction::Forward, KeyCode::ArrowDown.into()));
        assert!(
            binds.is(InputAction::Right, KeyCode::ArrowRight.into()),
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
        assert!(control.is(InputAction::Attack, KeyCode::KeyX.into()));
        assert!(control.is(InputAction::Chat, KeyCode::KeyY.into()));
        assert!(control.is(InputAction::Jump, KeyCode::KeyZ.into()));
        assert!(control.is(InputAction::Left, KeyCode::KeyV.into()));
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
        assert_eq!(Binding::Key(KeyCode::KeyW.into()).label(), "W");
        assert_eq!(Binding::Key(KeyCode::ShiftLeft.into()).label(), "Left Shift");
        assert_eq!(Binding::Key(KeyCode::F3.into()).label(), "F3");
        assert_eq!(Binding::Key(KeyCode::Space.into()).label(), "Space");
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
