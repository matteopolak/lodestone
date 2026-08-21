//! Key and mouse resolution: the `KeyGate` precedence chain and its outcomes.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

/// The clipboard `KeyOutcome::CopyLocation` writes to — vanilla's
/// `KeyboardHandler.setClipboard`, called from `keyDebugCopyLocation`.
///
/// The same fork [`crate::menu::edit_box`]'s own `clipboard_seam` module
/// doc describes (`CLAUDE.md`'s "test that performs an OS-level side effect"
/// rule): production writes the real OS clipboard through
/// [`crate::platform::clipboard`]; every `#[cfg(test)]` build routes through
/// an in-memory stand-in, so no `cargo test` run touches, or depends on,
/// whatever happens to be on the developer's real clipboard.
///
/// A second, separate module from `menu::edit_box`'s rather than a shared
/// one: that module's seam is private outside `cfg(test)` (`mod
/// clipboard_seam`, no `pub`), by design — it exists only for
/// `EditBox::handle_key`, not as a crate-wide clipboard API — so this file
/// needs its own copy of the same two functions rather than reaching into it.
#[cfg(not(test))]
pub(crate) mod clipboard_seam {
    pub fn set(text: &str) {
        crate::platform::clipboard::set(text);
    }
}

/// The `#[cfg(test)]` half of [`clipboard_seam`] — a `thread_local` string,
/// not the OS clipboard. `pub(crate)` (test-only) so
/// `the_copy_location_chord_writes_the_execute_command_to_the_clipboard`
/// below can read back what `KeyOutcome::CopyLocation`'s driver arm wrote.
#[cfg(test)]
pub(crate) mod clipboard_seam {
    use std::cell::RefCell;

    thread_local! {
        static FAKE: RefCell<String> = const { RefCell::new(String::new()) };
    }

    pub fn get() -> String {
        FAKE.with(|c| c.borrow().clone())
    }

    pub fn set(text: &str) {
        FAKE.with(|c| *c.borrow_mut() = text.to_owned());
    }
}

/// Which input surface owns the keyboard this instant.
///
/// The flags [`resolve_key`] needs, read off [`crate::menu::UiState`] at
/// the call site. Split out as plain data so the precedence below is testable
/// without a window, a GPU or a `Sim`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct KeyGate {
    /// A menu screen owns the frame (`menu::render::owns_frame`), or the pause
    /// overlay is up. Either way the whole keyboard belongs to the menu.
    pub menu: bool,
    /// The chat prompt is open.
    pub chat_open: bool,
    /// A container/inventory screen is open over the world.
    pub container_open: bool,
    /// `UiState::accepts_gameplay_input()` — i.e. `screen == Playing`.
    pub gameplay: bool,
    /// The debug modifier (F3) is currently held, so the next `B`/`G` is a
    /// **chord** rather than a plain key — That fix.
    ///
    /// Vanilla models this as a second `KeyMapping`, `keyDebugModifier`, bound to
    /// the *same* keysym as `keyDebugOverlay` (`Options.java`), plus the
    /// `modifierAndOverlayIsSame` bookkeeping in `KeyboardHandler`. Here it is a
    /// gate flag rather than a bindable action, so it lands in `resolve_key`
    /// where every other input decision already lives instead of behind a driver
    /// `match` arm this function's tests cannot see.
    pub debug_held: bool,
    /// The recipe book's search box has keyboard focus
    /// (`RecipePanelState::search_focused`), so it owns every key except Escape.
    ///
    /// Vanilla's `RecipeBookComponent.keyPressed` (`:437`):
    /// `else if (this.searchBox.isFocused() && this.searchBox.isVisible() &&
    /// !event.isEscape()) { return true; }` — a focused box swallows the key
    /// whether or not the box wanted it, which is what stops a hotbar number key
    /// selecting a slot while you are typing `stone`.
    ///
    /// **Escape is excluded, and that is vanilla for our layout.** The branch
    /// above it closes the *book* on Escape only when
    /// `!isOffsetNextToMainGUI()`, i.e. when the book overlays the main GUI. This
    /// client always draws the book beside the panel (see
    /// `container::recipe_book`'s module doc), so we are always in the offset
    /// case and Escape falls through — to `InputAction::Pause`, which closes the
    /// screen, exactly as it did before this existed.
    pub recipe_search: bool,
    /// The creative screen's search box has focus — the same
    /// swallow-everything-but-Escape rule as [`recipe_search`](Self::recipe_search),
    /// and for the same vanilla reason: `CreativeModeInventoryScreen.keyPressed`
    /// (`:428`) returns `true` for any key while the box is focused and visible
    /// and the event is not Escape.
    ///
    /// Vanilla is *stronger* here than for the recipe book — it calls
    /// `setCanLoseFocus(false)` on entering the search tab (`:610`), so the box
    /// cannot be unfocused by clicking elsewhere. `CreativeState::select_tab`
    /// mirrors that.
    pub creative_search: bool,
    /// The anvil's rename box has focus — the same swallow-everything-but-
    /// the-container-close rule [`recipe_search`](Self::recipe_search)/
    /// [`creative_search`](Self::creative_search) have, and for the same
    /// vanilla reason: `AnvilScreen.keyPressed` routes every key to
    /// `this.name` first and only falls to `super.keyPressed` (the ordinary
    /// container swallow) when the box itself declines
    /// (`!this.name.keyPressed(event) && !this.name.canConsumeInput()`).
    ///
    /// `setCanLoseFocus(false)` (`AnvilScreen.subInit`) is vanilla's version
    /// of the same "always focused while relevant" property
    /// [`creative_search`](Self::creative_search)'s doc names — this box is
    /// active whenever the anvil screen is open **and** its input slot (slot
    /// 0) is occupied, with no separate focus flag to track. See
    /// [`crate::container::AnvilRenameState`]'s own module doc for the whole
    /// chain (issue #603).
    pub anvil_rename_active: bool,
    /// The local player's server-authoritative game mode is `Spectator`
    /// (`Sim::is_spectator`) — issue #613's `TeleportToEntity` remainder.
    /// Gates the hotbar-number-key intercept that opens
    /// [`crate::menu::spectator_menu`]'s screen; **not** gated on
    /// [`gameplay`](Self::gameplay) itself (that is asked separately at the
    /// arm), so this flag alone never opens anything outside the world.
    pub spectator: bool,
}

/// The single thing a key event means, once precedence has been applied.
///
/// One variant per side effect the driver can perform, so the effects `match` in
/// `window_event` is exhaustive: a new variant must fail to compile there rather
/// than silently do nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    /// Hand the event to the menu navigator (needs the full `KeyEvent`, so the
    /// driver does the work — this only says *who* gets it).
    Menu,
    /// Hand the event to the chat prompt's editor.
    Chat,
    Pause,
    CloseContainer,
    /// The debug modifier (F3) went down (`true`) or up (`false`) — That fix.
    ///
    /// The overlay toggle happens on the **release**, and only when no chord
    /// consumed the hold; the driver owns that bookkeeping because it is the
    /// thing that knows whether a chord fired. See [`KeyGate::debug_held`].
    DebugModifier(bool),
    /// F3+B — vanilla's `key.debug.showHitboxes`.
    ToggleHitboxes,
    /// F3+G — vanilla's `key.debug.showChunkBorders`.
    ToggleChunkBorders,
    /// Shift+F3 — the profiler pie chart's own visibility toggle. Not a
    /// vanilla `KeyMapping` (vanilla has no chart of its own to toggle
    /// separately from the F3 overlay); `docs/frame-profiling.md` records why
    /// this chord rather than a bindable action.
    ToggleProfilerChart,
    /// A number-row key pressed while F3 is held — the profiler pie chart's
    /// navigation. `Some(i)` drills into wedge `i` (`Digit1..=Digit8` →
    /// `0..8`, one per [`crate::app::frame_profile::FramePhase`]); `None`
    /// (`Digit0`) returns to the root. A **chord**, not a bare number press:
    /// the number row is already the (rebindable) hotbar selector, so this
    /// only fires while `KeyGate::debug_held` is true, exactly like
    /// [`Self::ToggleHitboxes`]/[`Self::ToggleChunkBorders`].
    ProfilerChartSelect(Option<usize>),
    /// A key aimed at the recipe book's focused search box — see
    /// [`KeyGate::recipe_search`].
    ///
    /// Carries no payload: the driver already holds the `KeyEvent` and needs its
    /// `text` for the character, which this enum (deliberately `Copy` and
    /// `'static`) cannot hold. Same shape as [`Self::Menu`].
    RecipeSearch,
    /// A key aimed at the creative screen's focused search box — see
    /// [`KeyGate::creative_search`]. Payload-free for the same reason
    /// [`Self::RecipeSearch`] is.
    CreativeSearch,
    /// A key aimed at the anvil's rename box — see
    /// [`KeyGate::anvil_rename_active`]. Payload-free for the same reason
    /// [`Self::RecipeSearch`] is.
    AnvilRename,
    /// F3+N — vanilla's `key.debug.spectate` (keysym 78): in and out of spectator.
    ///
    /// The first producer of `ClientAction::ChangeGameMode` outside
    /// `crates/protocol/`; see [`WindowApp::toggle_spectator`].
    ToggleSpectator,
    /// F3+F4 — vanilla's `key.debug.switchGameMode` (keysym 293). Cycles rather
    /// than opening the radial picker; see [`WindowApp::cycle_game_mode`].
    CycleGameMode,
    /// F3+H — vanilla's `key.debug.advancedTooltips`.
    ///
    /// Unlike its two siblings above this does **not** toggle a render flag: it
    /// flips a *persisted option*, `Options.advancedItemTooltips`
    /// (`Options.java`), which is what `ItemStack.getTooltipLines` consults
    /// through `TooltipFlag.Default.ADVANCED`. So the driver's arm writes it
    /// through `MenuNav` (which owns `Options` and persists eagerly) rather than
    /// into an `AtomicBool` — see
    /// [`crate::config::Options::advanced_item_tooltips`].
    ToggleAdvancedTooltips,
    /// F3+P — vanilla's `key.debug.focusPause`
    /// (`Options.pauseOnLostFocus`).
    TogglePauseOnLostFocus,
    /// F3+C — vanilla's `key.debug.copyLocation`: copy a `/execute in <dim>
    /// run tp @s x y z yaw pitch` command for the local player to the
    /// clipboard.
    CopyLocation,
    /// `key.screenshot`: capture the window's own frame to a PNG.
    ///
    /// **No payload**, and the two things it does not carry are deliberate,
    /// both recorded in `docs/keybindings.md`'s "Screenshot" section: vanilla's
    /// Ctrl-held panorama variant is gated behind
    /// `SharedConstants.DEBUG_PANORAMA_SCREENSHOT`, which ships `false`, so a
    /// player's Ctrl+F2 is byte-identical to a plain F2; and vanilla's
    /// screen-independence (`Minecraft.handleGlobalKeyPress`) is not modelled
    /// here, because this `resolve_key` swallows every key behind `gate.menu`
    /// before any action arm runs — exactly as it already does for
    /// `key.debug.overlay`.
    ///
    /// The effect is deferred rather than performed here: the frame has not
    /// been drawn yet at key time, so the arm only sets
    /// `WindowApp::pending_screenshot` and `redraw()` drains it immediately
    /// before `present`.
    Screenshot,
    /// Hold-to-show the player list; carries the new held state.
    PlayerList(bool),
    /// Open the chat prompt. `command` pre-fills the `/` prefix.
    OpenChat { command: bool },
    OpenContainer,
    TogglePerspective,
    /// Select hotbar slot `0..=8`.
    SelectSlot(usize),
    /// A hotbar-number key while spectating (issue #613's
    /// `TeleportToEntity` remainder) — opens
    /// [`crate::menu::spectator_menu`]'s screen instead of selecting a
    /// (meaningless, for a spectator) hotbar slot. See [`KeyGate::spectator`].
    OpenSpectatorMenu,
    /// A `ContainerInput::SWAP` against the **hovered** slot while a container
    /// screen is open: vanilla's number keys and `key.swapOffhand`, which do
    /// *not* change the selected hotbar slot while a screen is up
    /// (`AbstractContainerScreen.checkHotbarKeyPressed`,
    /// `AbstractContainerScreen.java`).
    ///
    /// `button` is the wire button number: `0..=8` for a hotbar key, `40` for the
    /// off-hand key. It is carried raw rather than as an enum because that is
    /// exactly what `Click::hotbar_swap`/`offhand_swap` encode and what the
    /// server reads.
    ///
    /// Vanilla's own two guards — an **empty cursor** and a **hovered slot** —
    /// are session state, not key state, so they live at the driver's `match`
    /// arm. Failing either does nothing, which is what a container screen did
    /// with these keys before (it swallowed them), so a miss is not a
    /// regression.
    ContainerSwap { button: i32 },
    /// `key.swapOffhand` pressed with **no screen open**.
    ///
    /// A different mechanism from [`Self::ContainerSwap`], not a variation on
    /// it, and conflating the two is the trap this issue exists to avoid.
    /// Vanilla has two entirely separate code paths for the same physical key:
    ///
    /// | context | mechanism |
    /// |---|---|
    /// | screen open, slot hovered | container click, `ClickType.SWAP`, button `40` |
    /// | no screen, normal play | `ServerboundPlayerActionPacket` / `SWAP_ITEM_WITH_OFFHAND` |
    ///
    /// The gameplay one carries **no slot**: it is a bare action, and the server
    /// exchanges main hand for off hand itself
    /// (`ServerGamePacketListenerImpl.java`). So this variant carries
    /// no payload — there is nothing to hit-test and nothing to address, which
    /// is exactly what distinguishes it from `ContainerSwap`.
    ///
    /// Vanilla's one guard is `!player.isSpectator()`
    /// (`Minecraft.java`). That is session state rather than key state,
    /// so like `ContainerSwap`'s two guards it lives at the driver's `match` arm.
    SwapOffhand,
    /// A `ContainerInput::Throw` against the **hovered** slot while a
    /// container screen is open — vanilla's `key.drop` inside
    /// `AbstractContainerScreen.keyPressed`
    /// (`AbstractContainerScreen.java`), gated there on
    /// `hoveredSlot != null && hoveredSlot.hasItem()` — **not** an empty
    /// cursor, which `doClick` applies itself once the click reaches it
    /// (`AbstractContainerMenu.java`). `ctrl` selects drop-**stack**
    /// (button `1`) over drop-one (button `0`), the only thing the modifier
    /// changes; carried here rather than read at the driver arm because
    /// `resolve_key` is where every other input decision already lives (see
    /// [`InputAction::Drop`]'s docs).
    ContainerDrop { ctrl: bool },
    /// `key.drop` pressed with **no screen open** — vanilla's own
    /// `Minecraft.handleKeybinds` drop path (`Minecraft.java`). A
    /// different mechanism from [`Self::ContainerDrop`], the same split
    /// [`Self::SwapOffhand`] makes against [`Self::ContainerSwap`]: this one
    /// carries no slot, only which of `ClientAction::DropSelectedItem`/
    /// `DropSelectedItemStack` `ctrl` selects.
    Drop { ctrl: bool },
    /// `key.pickItem` pressed with **no screen open** — vanilla's
    /// `Minecraft.pickBlockOrEntity` (`Minecraft.java`). `ctrl` is
    /// `hasControlDown()`, forwarded as `include_data` on whichever
    /// `ClientAction` fires, exactly the same carry-it-here split
    /// [`Self::Drop`] makes.
    PickItem { ctrl: bool },
    /// `key.pickItem` pressed with a container screen open — `ClickType::CLONE`
    /// against the hovered slot (`AbstractContainerScreen.java`). No
    /// payload: unlike [`Self::ContainerDrop`], this click has no
    /// modifier-selected variant.
    ContainerPickItem,
    /// Begin (`true`) or end (`false`) a dig.
    Attack(bool),
    /// Press (`true`) or release (`false`) the use/place button.
    ///
    /// Both edges matter, the same reason `Attack`'s do: a release with no
    /// producer is exactly how `ClientAction::ReleaseUseItem` stayed a
    /// serverbound island (encoded by all four protocol adapters, called by
    /// nothing in this shell) — bow, crossbow and shield are all
    /// `useOnRelease()`-gated and cannot complete without it. See
    /// `Sim::end_use`.
    Use(bool),
    /// Set a movement action's held state on the controller.
    Movement(Action, bool),
    /// A plugin claimed this physical key in
    /// [`lodestone_ecs::KeyInterceptMode::Consume`] (see [`resolve_key`]'s own
    /// doc for exactly where this ranks). No gameplay binding sharing the key
    /// sees this edge at all — the driver's only job on this outcome is to do
    /// nothing else. The raw transition itself reaches the plugin a
    /// different way: the driver queues it into
    /// `lodestone_ecs::PendingPluginKeyEvents` unconditionally whenever a
    /// plugin has claimed the key (`Consume` or `Observe`), independent of
    /// which `KeyOutcome` this function returns — see the call site.
    PluginConsumed,
}

/// Maps a physical number-row key to the profiler chart's navigation
/// payload: `Some(Some(i))` for `Digit1..=Digit8` (wedge index `0..8`, one
/// per [`crate::app::frame_profile::FramePhase`]), `Some(None)` for `Digit0`
/// (back to the root), `None` for every other key — including `Digit9`,
/// since there are only eight phases to drill into. A three-way answer in one
/// call rather than matching `KeyCode` twice at the call site.
fn profiler_chart_digit(code: KeyCode) -> Option<Option<usize>> {
    match code {
        KeyCode::Digit0 => Some(None),
        KeyCode::Digit1 => Some(Some(0)),
        KeyCode::Digit2 => Some(Some(1)),
        KeyCode::Digit3 => Some(Some(2)),
        KeyCode::Digit4 => Some(Some(3)),
        KeyCode::Digit5 => Some(Some(4)),
        KeyCode::Digit6 => Some(Some(5)),
        KeyCode::Digit7 => Some(Some(6)),
        KeyCode::Digit8 => Some(Some(7)),
        _ => None,
    }
}

/// Resolve one key event to at most one [`KeyOutcome`].
///
/// # The order of this chain is behaviour, not layout
///
/// Every arm is `else if`, so the **first** match wins and every later arm is
/// skipped. Three arms exist wholly or partly to *swallow* keys, and reordering
/// them breaks things that no type error would catch:
///
/// 1. **`gate.menu` is first.** A menu screen captures the entire keyboard — the
///    server-address edit form needs every printable key, and no gameplay
///    binding may fire behind it. `Screen::Paused` is folded in here even though
///    it is not an `owns_frame` screen, because it has its own keyboard
///    navigation that needs identical routing.
/// 2. **`gate.chat_open` is second.** Same reason: while the prompt is up, `W`
///    types a `w`, it does not walk.
/// 3. **`gate.container_open` is checked before every gameplay binding.** While
///    a container is open the key press is consumed whether or not it is bound
///    to anything — which is why this arm returns `None` for an unrecognised key
///    rather than falling through. The debug overlay, player list, chat, hotbar
///    and movement arms below therefore cannot fire behind an open inventory.
///
/// The `Pause` arm sits *above* the container arm, so Escape closes a container
/// through `on_escape` rather than through `CloseContainer`. That ordering is
/// why the container arm handles only the inventory binding: its Escape case in
/// the original chain was unreachable, and spelling out dead code invites
/// someone to "fix" it by moving it up.
///
/// **A plugin's `Consume` claim (`plugin_mode`) is checked immediately after
/// the container arm, ahead of every gameplay binding below it** (issue
/// #162). That rank is deliberate on both sides: behind chat/menu/container,
/// which keep first claim on the keyboard regardless of what a plugin wants
/// (`docs/plugin-api.md`'s doctrine clause 4 — a human's own input always
/// outranks installed intent, and an open chat box or container screen *is*
/// the human's current input target); ahead of gameplay, so a plugin hotkey
/// cannot be shadowed by a rebind onto the same physical key. See
/// `lodestone_ecs::input` for the registration side.
///
/// Everything from the debug overlay down is additionally gated on
/// `gate.gameplay`, so those arms are inert behind any screen regardless of
/// order — but the order is still what stops the *swallowing* arms above from
/// being bypassed.
///
/// `code` is `None` for a physical key winit could not name (`PhysicalKey::
/// Unidentified`). Note such an event still reaches the menu and chat arms —
/// they route the whole `KeyEvent`, whose `text` may well be meaningful — and
/// only the keybind chain needs a `KeyCode` to match against.
///
/// `ctrl` is whether Control is currently held — vanilla's own
/// `Screen.hasControlDown()`/`hasControlDown()`, read by the driver off a
/// tracked modifier state (mirroring `shift_held`) rather than off this event,
/// since Control and `key.drop` are two different physical keys. It decides
/// nothing except which of [`KeyOutcome::ContainerDrop`]/[`KeyOutcome::Drop`]'s
/// two payload states is produced — threaded through the signature rather than
/// read at the driver's `match` arm, because `resolve_key` is where every
/// other input decision already lives and a decision made outside it is
/// invisible to this function's own tests.
///
/// `plugin_mode` is `lodestone_ecs::PluginKeybinds::mode_of(&physical_key)`
/// for this event's key, read by the driver through a short
/// `lodestone_ecs::hold_read` guard before calling this function — plain
/// data, the same reason `gate` is passed in rather than queried here, so
/// the precedence chain stays testable without a `World`. Only
/// `KeyInterceptMode::Consume` affects this function's output; `Observe`
/// resolves exactly as if no plugin existed (the driver still delivers the
/// raw event to the plugin regardless — see the call site).
pub(crate) fn resolve_key(
    binds: &Keybinds,
    gate: KeyGate,
    code: Option<KeyCode>,
    pressed: bool,
    ctrl: bool,
    plugin_mode: Option<lodestone_ecs::KeyInterceptMode>,
) -> Option<KeyOutcome> {
    if gate.menu {
        return Some(KeyOutcome::Menu);
    }
    if gate.chat_open {
        return Some(KeyOutcome::Chat);
    }
    let code = code?;
    if binds.is(InputAction::Pause, code) && pressed {
        Some(KeyOutcome::Pause)
    } else if gate.recipe_search && pressed {
        // **Before the container swallow, after Pause.** Order is the whole
        // content of this arm: `gate.container_open` below returns `None` for
        // anything it does not recognise, so a search box placed after it would
        // never see a single key — and `Pause` above it is Escape, which vanilla
        // deliberately does not route into the box (see `KeyGate::recipe_search`).
        Some(KeyOutcome::RecipeSearch)
    } else if gate.creative_search && pressed {
        // Same position and the same reason as the recipe box above: after
        // `Pause` so Escape still closes the screen, before the container
        // swallow so the box sees any key at all.
        Some(KeyOutcome::CreativeSearch)
    } else if gate.anvil_rename_active && pressed {
        // Same position and the same reason as the two boxes above —
        // matches `AnvilScreen.keyPressed`'s own order exactly: `isEscape()`
        // (→ `Pause`, above) is checked before the box, and every other key
        // reaches `this.name` before the ordinary container swallow gets a
        // chance to drop it.
        Some(KeyOutcome::AnvilRename)
    } else if gate.container_open && pressed {
        // Vanilla's order, from `AbstractContainerScreen.keyPressed`
        // (`AbstractContainerScreen.java`): the inventory binding closes
        // the screen first, then `checkHotbarKeyPressed` tries the off-hand key
        // and then the nine hotbar keys. Anything else is swallowed — hence
        // `None`, not a fall-through, so no gameplay binding fires behind an open
        // inventory.
        //
        // The number keys used to fall into that swallow, which is that fix's
        // part 3: they neither selected a slot (correct) nor swapped (the gap).
        //
        // The off-hand key is here too: it was blocked purely
        // by a keybind collision (`key.swapOffhand` defaults to `F`, which the
        // now-deleted `key.lodestone.toggleFly` squatted on), never by the click
        // path — `Click::offhand_swap` and `do_swap`'s `button == 40` arm were
        // already in place and tested. Asked **before** the hotbar keys, matching
        // `checkHotbarKeyPressed`'s own order, so rebinding the off-hand key onto
        // a number key swaps with slot `40` rather than that number's slot.
        if binds.is(InputAction::Inventory, code) {
            Some(KeyOutcome::CloseContainer)
        } else if binds.is(InputAction::SwapOffhand, code) {
            Some(KeyOutcome::ContainerSwap {
                button: OFFHAND_SWAP_BUTTON,
            })
        } else if let Some(slot) = hotbar_slot_for(binds, code) {
            Some(KeyOutcome::ContainerSwap { button: slot as i32 })
        } else if binds.is(InputAction::PickItem, code) {
            // Before the `Drop` arm below, matching vanilla's own
            // `keyPickItem`-then-`keyDrop` order in
            // `AbstractContainerScreen.keyPressed` (`:495-501`). The
            // hovered-slot gate lives in `MenuInput::key_pressed`, so a miss
            // resolves here and produces zero clicks downstream, exactly as the
            // `Drop` arm's own comment describes.
            Some(KeyOutcome::ContainerPickItem)
        } else if binds.is(InputAction::Drop, code) {
            // Vanilla checks this *after* `checkHotbarKeyPressed` returns,
            // not folded into it — `AbstractContainerScreen.java` is
            // two separate `if`s, one wrapping `checkHotbarKeyPressed` and a
            // second, independent one for pick/drop. The hovered-slot-has-item
            // gate itself lives in `MenuInput::key_pressed`, not here, so a
            // miss (empty slot, no slot at all) resolves to this outcome and
            // then produces zero clicks downstream — matching vanilla's own
            // `return false` either way.
            Some(KeyOutcome::ContainerDrop { ctrl })
        } else {
            None
        }
    } else if gate.gameplay
        && matches!(plugin_mode, Some(lodestone_ecs::KeyInterceptMode::Consume))
    {
        // A plugin has claimed this physical key exclusively — see this
        // function's own doc for exactly why it ranks here (behind
        // chat/menu/container, ahead of every gameplay binding below).
        // **No `&& pressed`**: both edges reach here, matching `Attack`/
        // `Use`'s own both-edges requirement, so a consumed key's release
        // cannot leak past this arm into whatever gameplay binding happens
        // to share the same physical key.
        Some(KeyOutcome::PluginConsumed)
    } else if binds.is(InputAction::DebugOverlay, code) {
        // **Both edges**, and the toggle has moved to the release — that fix's
        // chords. Vanilla's own rule is
        // `keyDebugModifier.setDown(!didDebugAction)` at
        // `KeyboardHandler.java`: releasing F3 toggles the overlay only
        // if no chord fired while it was held. Toggling on the *press* (what
        // this did before) makes F3+B both open the overlay and toggle
        // hitboxes, which is why the modifier cannot just be a held flag with
        // the old press-toggle left in place.
        Some(KeyOutcome::DebugModifier(pressed))
    } else if gate.debug_held && pressed && code == KeyCode::KeyB {
        // `key.debug.showHitboxes`, vanilla keysym 66 (`Options.java`).
        Some(KeyOutcome::ToggleHitboxes)
    } else if gate.debug_held && pressed && code == KeyCode::KeyG {
        // `key.debug.showChunkBorders`, vanilla keysym 71.
        Some(KeyOutcome::ToggleChunkBorders)
    } else if gate.debug_held
        && pressed
        && matches!(code, KeyCode::ShiftLeft | KeyCode::ShiftRight)
    {
        // Shift+F3 — the profiler pie chart. Both physical shifts, the same
        // way the shift-click swallow above reads "is a shift modifier
        // down" rather than a single rebindable key; see
        // [`KeyOutcome::ToggleProfilerChart`]'s doc for why this is not a
        // vanilla `KeyMapping`.
        Some(KeyOutcome::ToggleProfilerChart)
    } else if gate.debug_held && pressed && let Some(digit) = profiler_chart_digit(code) {
        // The profiler chart's own number-row navigation — see
        // [`KeyOutcome::ProfilerChartSelect`]'s doc for why this is chorded
        // rather than a bare press.
        Some(KeyOutcome::ProfilerChartSelect(digit))
    } else if gate.debug_held && pressed && code == KeyCode::KeyN {
        // `key.debug.spectate`, vanilla keysym 78.
        Some(KeyOutcome::ToggleSpectator)
    } else if gate.debug_held && pressed && code == KeyCode::F4 {
        // `key.debug.switchGameMode`, vanilla keysym 293 (GLFW's F4).
        Some(KeyOutcome::CycleGameMode)
    } else if gate.debug_held && pressed && code == KeyCode::KeyH {
        // `key.debug.advancedTooltips`, vanilla keysym 72. **Not gated on
        // `gate.gameplay`**, the same as its two siblings: F3 chords are debug
        // instruments and vanilla's `KeyboardHandler.handleDebugKeys` runs
        // regardless of the open screen — which matters more for this one than
        // for the others, because the thing it changes is only *visible* with a
        // container screen open.
        Some(KeyOutcome::ToggleAdvancedTooltips)
    } else if gate.debug_held && pressed && code == KeyCode::KeyP {
        // `key.debug.focusPause`, vanilla keysym 80. Same "not gated on
        // `gate.gameplay`" reasoning as its siblings above.
        Some(KeyOutcome::TogglePauseOnLostFocus)
    } else if gate.debug_held && pressed && code == KeyCode::KeyC {
        // `key.debug.copyLocation`, vanilla keysym 67. Vanilla additionally
        // gates this on `!player.isReducedDebugInfo()`, a concept this client
        // does not model yet — see `docs/keybindings.md`'s F3 section.
        Some(KeyOutcome::CopyLocation)
    } else if binds.is(InputAction::Screenshot, code) && pressed {
        // Same tier as `DebugOverlay` immediately above, and for the same
        // reason: vanilla's `key.screenshot` is `Category.MISC` and takes no
        // account of what the player is doing. So it is gated on `pressed`
        // only — **not** on `gate.gameplay`, which would make a screenshot
        // impossible with the debug overlay's own subject on screen.
        Some(KeyOutcome::Screenshot)
    } else if binds.is(InputAction::PlayerList, code) && gate.gameplay {
        // Deliberately *not* gated on `pressed`: this tracks a held state, so it
        // needs both edges. Gating it would leave the overlay stuck on.
        Some(KeyOutcome::PlayerList(pressed))
    } else if (binds.is(InputAction::Chat, code) || binds.is(InputAction::Command, code))
        && pressed
        && gate.gameplay
    {
        // The command binding pre-fills `/`. Asked directly rather than inferred
        // from the `||` above, so binding both actions to one key yields the
        // command prefix instead of depending on which side matched first.
        Some(KeyOutcome::OpenChat {
            command: binds.is(InputAction::Command, code),
        })
    } else if binds.is(InputAction::Inventory, code) && pressed && gate.gameplay {
        Some(KeyOutcome::OpenContainer)
    } else if binds.is(InputAction::TogglePerspective, code) && pressed && gate.gameplay {
        Some(KeyOutcome::TogglePerspective)
    } else if hotbar_slot_for(binds, code).is_some() && pressed && gate.gameplay && gate.spectator {
        // Issue #613's `TeleportToEntity` remainder: while spectating, every
        // hotbar-number key opens the Spectator Menu instead of selecting a
        // slot — see [`KeyOutcome::OpenSpectatorMenu`]'s own doc for why
        // this ranks *ahead of* the ordinary `SelectSlot` arm immediately
        // below rather than folding a branch into it. A spectator's hotbar
        // selection is otherwise inert (no inventory), so this loses nothing
        // real.
        Some(KeyOutcome::OpenSpectatorMenu)
    } else if let Some(slot) = hotbar_slot_for(binds, code)
        && pressed
        && gate.gameplay
    {
        Some(KeyOutcome::SelectSlot(slot))
    } else if binds.is(InputAction::SwapOffhand, code) && pressed && gate.gameplay {
        // The *gameplay* half of `key.swapOffhand`. The container
        // half is up in the `gate.container_open` arm and is a different
        // mechanism — see [`KeyOutcome::SwapOffhand`].
        //
        // **Placed after the hotbar keys, unlike the container arm, and that
        // asymmetry is vanilla's own.** `Minecraft.handleKeybinds` asks
        // `keyHotbarSlots` at `:1873` and `keySwapOffhand` at `:1900`;
        // `AbstractContainerScreen.checkHotbarKeyPressed` asks the off-hand key
        // *first*. Both orders only matter once someone rebinds the off-hand key
        // onto a number key, and matching each context's own source is cheaper
        // than picking one and being wrong in half the cases.
        Some(KeyOutcome::SwapOffhand)
    } else if binds.is(InputAction::Drop, code) && pressed && gate.gameplay {
        // `Minecraft.handleKeybinds` asks `keyDrop` (`:1907`) immediately
        // after `keySwapOffhand` (`:1900`) and before `keyAttack`/`keyUse`
        // (`:1913+`) — matched here for the same reason the off-hand arm's
        // own doc gives: the two orders (this one and the container arm's)
        // only diverge once someone rebinds one action onto another's key.
        Some(KeyOutcome::Drop { ctrl })
    } else if binds.is(InputAction::Attack, code) && gate.gameplay {
        // Only reachable once `key.attack` has been rebound off its default
        // mouse button; the mouse path is what fires out of the box. Both edges
        // matter — mining is hold-to-dig.
        Some(KeyOutcome::Attack(pressed))
    } else if binds.is(InputAction::Use, code) && gate.gameplay {
        // As above: dormant under the default mouse binding. Both edges
        // matter here too, not just on press — see `KeyOutcome::Use`'s docs.
        Some(KeyOutcome::Use(pressed))
    } else if binds.is(InputAction::PickItem, code) && pressed && gate.gameplay {
        // Press-only: vanilla's `pickBlockOrEntity` is a one-shot, unlike
        // attack/use whose release edge also matters. Reachable by keyboard only
        // once `key.pickItem` is rebound off its default middle mouse button;
        // the mouse path in the button handler is what fires out of the box.
        Some(KeyOutcome::PickItem { ctrl })
    } else if let Some(action) = movement_action_for(binds, code)
        && gate.gameplay
    {
        Some(KeyOutcome::Movement(action, pressed))
    } else {
        None
    }
}

/// Vanilla's `KeyboardHandler.decorateDebugComponent`/`debugFeedback`: every
/// F3 chord that changes something prints a `[Debug]: <message>` line to
/// chat, with the prefix bold and `ChatFormatting.YELLOW`
/// (`KeyboardHandler.java`).
///
/// `YELLOW` is a legacy-representable colour — `TextColor::legacy_code()`
/// returns `Some` for it — so this needs no
/// [`lodestone_model::text::TextSpan`] construction of its own: a plain
/// string carrying embedded `§` codes is exactly what
/// [`lodestone_model::text::Text::to_spans`] exists to expand (its own doc:
/// "`from_legacy` consumes every `§`+code pair"), and
/// `Sim::push_local_chat`/`ChatLog::push_system` already route every chat
/// line through `to_spans()` for the HUD's draw
/// (`ChatLog::recent_ages_spans`), never through `to_legacy_string()` — the
/// lossy path that cannot carry an RGB colour but has no trouble with a named
/// one. So the colour reaches a vertex without this function, or its caller,
/// touching a span directly.
///
/// Vanilla has a red `debugWarningComponent` sibling for the failure paths
/// (no-permission errors); every chord ported here either always succeeds or
/// has no failure path we can detect client-side (see
/// `docs/keybindings.md`), so only the yellow feedback path exists — add the
/// red one back the day a chord needs it rather than shipping it unused now.
#[must_use]
pub(crate) fn debug_feedback(message: impl std::fmt::Display) -> String {
    format!("§e§l[Debug]:§r {message}")
}

/// The `"<label>: shown"`/`"<label>: hidden"` shape three chords share —
/// `debug.show_hitboxes.on`/`.off`, `debug.chunk_boundaries.on`/`.off` and
/// `debug.advanced_tooltips.on`/`.off` (`KeyboardHandler.debugFeedbackTranslated`).
/// Pure so `debug_feedback`'s driver call sites and this module's own tests
/// share one source of the exact vanilla wording, rather than each arm in
/// `app/lifecycle.rs` spelling the strings out separately.
#[must_use]
pub(crate) fn debug_shown_feedback(label: &str, now: bool) -> String {
    debug_feedback(format!("{label}: {}", if now { "shown" } else { "hidden" }))
}

/// The `"<label>: enabled"`/`"<label>: disabled"` shape
/// `debug.pause_focus.on`/`.off` uses — same reason as
/// [`debug_shown_feedback`], different vanilla wording.
#[must_use]
pub(crate) fn debug_enabled_feedback(label: &str, now: bool) -> String {
    debug_feedback(format!(
        "{label}: {}",
        if now { "enabled" } else { "disabled" }
    ))
}

/// `KeyboardHandler.keyDebugCopyLocation`'s `String.format`
/// (`KeyboardHandler.java`): `/execute in %s run tp @s %.2f %.2f %.2f %.2f
/// %.2f`, the dimension identifier then x/y/z/yaw/pitch each to two decimal
/// places. Pure so `KeyOutcome::CopyLocation`'s driver arm and this module's
/// own test share the exact format rather than the test re-deriving it from
/// the same literal the arm uses.
#[must_use]
pub(crate) fn copy_location_command(
    dimension: &str,
    position: [f64; 3],
    yaw: f32,
    pitch: f32,
) -> String {
    let [x, y, z] = position;
    format!("/execute in {dimension} run tp @s {x:.2} {y:.2} {z:.2} {yaw:.2} {pitch:.2}")
}

/// The action a gameplay off-hand-key press should send, or `None`.
///
/// Split out of the driver so the *decision* — which is entirely "is this player
/// a spectator" — is a pure function a test can drive.
/// `WindowApp::send_offhand_swap` is the few lines that read the game mode and
/// push the result, and they are the part no test in this crate can reach (see
/// `docs/keybindings.md`'s gotcha on the effects `match`).
///
/// # No local prediction, and that is the vanilla behaviour rather than a shortcut
///
/// `Minecraft.handleKeybinds` (`Minecraft.java`) is the entire client
/// half of this feature:
///
/// ```text
/// while (this.options.keySwapOffhand.consumeClick()) {
///    if (!this.player.isSpectator()) {
///       this.getConnection().send(new ServerboundPlayerActionPacket(
///          Action.SWAP_ITEM_WITH_OFFHAND, BlockPos.ZERO, Direction.DOWN));
///    }
/// }
/// ```
///
/// No `Inventory` mutation, no animation, no prediction. The swap happens
/// **server-side only** (`ServerGamePacketListenerImpl.java` does the
/// three-way exchange plus `stopUsingItem`), and the client learns the result
/// from the ordinary inventory-sync packets that follow.
///
/// This is why that fix's round-trip worry does not apply here the way it does
/// to that fix's block placement: vanilla *does* predict a placement, so not
/// predicting one is a divergence; vanilla does **not** predict this, so adding a
/// local swap would be the divergence. Two consequences if you are tempted
/// anyway: our prediction would have to guess `stopUsingItem`'s effect on an
/// in-progress bow draw or eat, and a creative-mode client whose server refuses
/// the swap would show a phantom exchange that only the next full inventory sync
/// corrects.
///
/// # The one guard
///
/// `!player.isSpectator()`. A spectator has no inventory to swap and vanilla
/// declines to send at all — so the packet never reaches a server that would
/// silently drop it (the server re-checks, `:1295`). Reading the mode as
/// `Option` and treating unknown as *not* a spectator matches the rest of the
/// shell: before login there is no mode, and refusing input until one arrives
/// would be a worse default than sending a packet no server is listening for.
#[must_use]
pub(crate) fn offhand_swap_action(
    game_mode: Option<lodestone_client::GameMode>,
) -> Option<lodestone_model::ClientAction> {
    if game_mode == Some(lodestone_client::GameMode::Spectator) {
        return None;
    }
    Some(lodestone_model::ClientAction::SwapItemWithOffhand)
}

/// The action `key.drop` pressed with **no screen open** should send, or
/// `None` — the gameplay half of [`KeyOutcome::Drop`], mirroring
/// [`offhand_swap_action`]'s shape and reasoning.
///
/// # The one guard
///
/// `!player.isSpectator()` (`Minecraft.java`), the exact same guard
/// `offhand_swap_action` applies and for the same reason: a spectator has
/// nothing to drop, vanilla declines to send at all, and the server re-checks
/// regardless (`ServerGamePacketListenerImpl`'s handling of
/// `Action.DROP_ITEM`/`DROP_ALL_ITEMS` no-ops for a spectator same as any
/// other player action). An unknown mode (before login) is treated as *not*
/// spectator, matching `offhand_swap_action`'s own default.
///
/// # `ctrl` selects the wire action, not a client-side stack split
///
/// Vanilla's `Player.drop(boolean dropStack)` chooses between dropping one
/// item and the whole stack **client-side**, mutating the local inventory as
/// part of the drop and then sending whichever `ServerboundPlayerActionPacket`
/// action matches. This shell has no client-side inventory mutation for the
/// hotbar outside the container-click predictor (see `send_offhand_swap`'s
/// own note on the same gap), so `ctrl` selects the wire action directly —
/// `DropSelectedItemStack` for `true`, `DropSelectedItem` for `false` — and the
/// next inventory sync corrects the held count, exactly the trade
/// `send_offhand_swap` already makes for the exchange itself.
#[must_use]
pub(crate) fn drop_selected_action(
    game_mode: Option<lodestone_client::GameMode>,
    ctrl: bool,
) -> Option<lodestone_model::ClientAction> {
    if game_mode == Some(lodestone_client::GameMode::Spectator) {
        return None;
    }
    Some(if ctrl {
        lodestone_model::ClientAction::DropSelectedItemStack
    } else {
        lodestone_model::ClientAction::DropSelectedItem
    })
}
