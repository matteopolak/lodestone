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
    /// the *same* keysym as `keyDebugOverlay`, plus the
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
    /// chain.
    pub anvil_rename_active: bool,
    /// The local player's server-authoritative game mode is `Spectator`
    /// (`Sim::is_spectator`) — `TeleportToEntity` remainder.
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
    /// F3+B — vanilla's `key.debug.showHitboxes`. Rebindable: see
    /// [`InputAction::DebugShowHitboxes`].
    ToggleHitboxes,
    /// F3+G — vanilla's `key.debug.showChunkBorders`. Rebindable: see
    /// [`InputAction::DebugShowChunkBorders`].
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
    /// F3+H — vanilla's `key.debug.showAdvancedTooltips`.
    ///
    /// Unlike its two siblings above this does **not** toggle a render flag: it
    /// flips a *persisted option*, `Options.advancedItemTooltips`
    ///, which is what `ItemStack.getTooltipLines` consults
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
    ///. So this variant carries
    /// no payload — there is nothing to hit-test and nothing to address, which
    /// is exactly what distinguishes it from `ContainerSwap`.
    ///
    /// Vanilla's one guard is `!player.isSpectator()`
    ///. That is session state rather than key state,
    /// so like `ContainerSwap`'s two guards it lives at the driver's `match` arm.
    SwapOffhand,
    /// A `ContainerInput::Throw` against the **hovered** slot while a
    /// container screen is open — vanilla's `key.drop` inside
    /// `AbstractContainerScreen.keyPressed`
    ///, gated there on
    /// `hoveredSlot != null && hoveredSlot.hasItem()` — **not** an empty
    /// cursor, which `doClick` applies itself once the click reaches it
    ///. `ctrl` selects drop-**stack**
    /// (button `1`) over drop-one (button `0`), the only thing the modifier
    /// changes; carried here rather than read at the driver arm because
    /// `resolve_key` is where every other input decision already lives (see
    /// [`InputAction::Drop`]'s docs).
    ContainerDrop { ctrl: bool },
    /// `key.drop` pressed with **no screen open** — vanilla's own
    /// `Minecraft.handleKeybinds` drop path. A
    /// different mechanism from [`Self::ContainerDrop`], the same split
    /// [`Self::SwapOffhand`] makes against [`Self::ContainerSwap`]: this one
    /// carries no slot, only which of `ClientAction::DropSelectedItem`/
    /// `DropSelectedItemStack` `ctrl` selects.
    Drop { ctrl: bool },
    /// `key.pickItem` pressed with **no screen open** — vanilla's
    /// `Minecraft.pickBlockOrEntity`. `ctrl` is
    /// `hasControlDown()`, forwarded as `include_data` on whichever
    /// `ClientAction` fires, exactly the same carry-it-here split
    /// [`Self::Drop`] makes.
    PickItem { ctrl: bool },
    /// `key.pickItem` pressed with a container screen open — `ClickType::CLONE`
    /// against the hovered slot. No
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
        //: the inventory binding closes
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
    } else if gate.debug_held && pressed && binds.is(InputAction::DebugShowHitboxes, code) {
        // `key.debug.showHitboxes`. Table-driven, not a literal `KeyCode::KeyB`:
        // vanilla declares this a `Category.DEBUG` `KeyMapping` and dispatches
        // it through `KeyMapping::matches` — see `InputAction::DebugShowHitboxes`.
        // The `gate.debug_held` conjunct stays, because the F3 *modifier* is a
        // gate flag here rather than an eighth bindable action.
        Some(KeyOutcome::ToggleHitboxes)
    } else if gate.debug_held && pressed && binds.is(InputAction::DebugShowChunkBorders, code) {
        // `key.debug.showChunkBorders` — table-driven, as above.
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
    } else if gate.debug_held && pressed && binds.is(InputAction::DebugSpectate, code) {
        // `key.debug.spectate` — table-driven, as above.
        Some(KeyOutcome::ToggleSpectator)
    } else if gate.debug_held && pressed && binds.is(InputAction::DebugSwitchGameMode, code) {
        // `key.debug.switchGameMode` — table-driven, as above.
        Some(KeyOutcome::CycleGameMode)
    } else if gate.debug_held && pressed && binds.is(InputAction::DebugShowAdvancedTooltips, code) {
        // `key.debug.showAdvancedTooltips` — table-driven, as above. **Not gated on
        // `gate.gameplay`**, the same as its two siblings: F3 chords are debug
        // instruments and vanilla's `KeyboardHandler.handleDebugKeys` runs
        // regardless of the open screen — which matters more for this one than
        // for the others, because the thing it changes is only *visible* with a
        // container screen open.
        Some(KeyOutcome::ToggleAdvancedTooltips)
    } else if gate.debug_held && pressed && binds.is(InputAction::DebugFocusPause, code) {
        // `key.debug.focusPause` — table-driven, as above. Same "not gated on
        // `gate.gameplay`" reasoning as its siblings above.
        Some(KeyOutcome::TogglePauseOnLostFocus)
    } else if gate.debug_held && pressed && binds.is(InputAction::DebugCopyLocation, code) {
        // `key.debug.copyLocation` — table-driven, as above. Vanilla additionally
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
/// chat, with the prefix bold and `ChatFormatting.YELLOW`.
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
///: `/execute in %s run tp @s %.2f %.2f %.2f %.2f
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
/// `Minecraft.handleKeybinds` is the entire client
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
/// `!player.isSpectator()`, the exact same guard
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

impl WindowApp {
    /// The live action → input table every key and mouse event resolves
    /// against (`docs/keybindings.md`).
    ///
    /// **A read, not a copy, and that is the whole point of it existing.**
    /// `WindowApp` used to hold its own `keybinds: Keybinds` field, seeded from
    /// [`crate::config::Options::load`] in the constructor. The Controls
    /// screen's Key Binds page writes the *other* copy — `MenuNav` owns the
    /// loaded [`crate::config::Options`] and persists them eagerly
    /// ([`crate::menu::nav::MenuNav::capture_binding`] and the two reset arms)
    /// — so a rebind reached the file and the menu's own labels while the
    /// resolver kept answering from the table it had read at startup. Every
    /// rebind therefore took effect only on the *next* launch: the owner bound
    /// Toggle Perspective to `G`, and `F5` kept cycling the camera while `G`
    /// did nothing.
    ///
    /// Reading through [`MenuNav`] here means there is no second copy to keep
    /// in step and so no invalidation to forget. [`Keybinds`] is `Copy`, so
    /// this returns by value: the immutable borrow of `self` ends at the call,
    /// which is what lets `handle_keyboard_input` hold the table across the
    /// `&mut self` effect calls that follow it.
    #[must_use]
    pub(crate) fn keybinds(&self) -> Keybinds {
        self.nav.options().keybinds
    }
}

/// Translate one winit key press into the [`crate::menu::focus::KeyEvent`]
/// every ported text field speaks — the GLFW key code plus the GLFW modifier
/// bitmask vanilla's `InputWithModifiers` predicates test.
///
/// # Why this exists rather than a second edit implementation
///
/// The chat prompt had no text editing at all: no caret, so no Left/Right,
/// Home/End, shift-selection, word skip or select-all, and copy/cut with
/// nothing to read. All of that is already ported, once, on
/// [`crate::menu::edit_box::EditBox`] — the missing piece was a *producer*, a
/// way to get a real `KeyEvent` out of a winit event. `menu_key_for` could not
/// be it as it stood: it targets [`crate::menu::nav::MenuKey`], whose
/// vocabulary had no Left/Right/Home/End at all, so routing chat through it
/// could never have carried caret motion — and for the same reason the menu's
/// own text fields (the sign, the book, the command block, the server-edit
/// form) had none either, silently, since `EditBox` implements all four and
/// simply never received them. `menu_key_for` now produces
/// [`crate::menu::nav::MenuKey::Edit`] from *this* function for exactly those
/// keys, so both paths share one translator and cannot drift apart in what a
/// chord means.
///
/// # The modifier mapping is faithful, not platform-folded
///
/// Each winit modifier maps to the GLFW bit that literally means it —
/// Shift→`MOD_SHIFT`, Ctrl→`MOD_CONTROL`, Alt→`MOD_ALT`, Cmd→`MOD_SUPER`. The
/// Mac-versus-elsewhere split the owner asked about is **not** applied here on
/// purpose: it lives once, in
/// [`crate::menu::focus::EDIT_SHORTCUT_MODIFIER`], which is what
/// `has_control_down_with_quirk`/`is_copy`/`is_cut`/`is_paste`/`is_select_all`
/// test. Folding Cmd onto `MOD_CONTROL` here would work for the shortcuts and
/// then be wrong for the reverse case — a Mac user pressing *Ctrl*+Left, which
/// vanilla treats as a plain caret step, would get a word skip.
///
/// Returns `None` for any key no text field acts on, which is the caller's
/// signal to fall through to its own text-insertion path.
#[must_use]
pub(crate) fn text_key_event(
    physical_key: PhysicalKey,
    modifiers: ModifiersState,
) -> Option<crate::menu::focus::KeyEvent> {
    use crate::menu::focus;
    let PhysicalKey::Code(code) = physical_key else {
        return None;
    };
    let key = match code {
        KeyCode::Backspace => focus::KEY_BACKSPACE,
        KeyCode::Delete => focus::KEY_DELETE,
        KeyCode::ArrowLeft => focus::KEY_LEFT,
        KeyCode::ArrowRight => focus::KEY_RIGHT,
        KeyCode::Home => focus::KEY_HOME,
        KeyCode::End => focus::KEY_END,
        // The four `EditBox` reads through `isSelectAll`/`isCopy`/`isCut`/
        // `isPaste`. Passed through unconditionally rather than gated on the
        // modifier here: the predicates do that themselves, and gating twice
        // is how the two conditions drift apart.
        KeyCode::KeyA => focus::KEY_A,
        KeyCode::KeyC => focus::KEY_C,
        KeyCode::KeyV => focus::KEY_V,
        KeyCode::KeyX => focus::KEY_X,
        _ => return None,
    };
    Some(crate::menu::focus::KeyEvent::with_modifiers(
        key,
        glfw_modifiers(modifiers),
    ))
}

/// The GLFW modifier bitmask winit's tracked [`ModifiersState`] stands for.
///
/// `modifiers` is `WindowApp::modifiers`, tracked from
/// `WindowEvent::ModifiersChanged` — winit reports modifier state as its own
/// event rather than on the key press, so reading it off the `KeyEvent` gives
/// zero for every chord.
#[must_use]
pub(crate) fn glfw_modifiers(modifiers: ModifiersState) -> i32 {
    use crate::menu::focus::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_SUPER};
    let mut bits = 0;
    if modifiers.shift_key() {
        bits |= MOD_SHIFT;
    }
    if modifiers.control_key() {
        bits |= MOD_CONTROL;
    }
    if modifiers.alt_key() {
        bits |= MOD_ALT;
    }
    if modifiers.super_key() {
        bits |= MOD_SUPER;
    }
    bits
}

/// Chat text editing.
///
/// # Which half each gate covers, and why the split is not laziness
///
/// The chat line had no caret at all, so two things needed proving and they are
/// not the same claim: that [`crate::chat::ChatInput`] *does* the editing, and
/// that a real key press *reaches* it. The second is the one a gate calling an
/// editing helper directly cannot make, and it is the half that was broken.
///
/// So the routing gates below go in through [`WindowApp::handle_chat_key_parts`]
/// — the exact function `apply_key_outcome`'s `KeyOutcome::Chat` arm calls,
/// minus only the `winit::event::KeyEvent` wrapper nothing outside winit can
/// construct. They are deliberately few: `WindowApp::new` costs tens of seconds
/// even headless, and once the dispatch is proven, the behaviour gates below
/// them run through [`text_key_event`] — the real translator, the piece that
/// actually had to be written — into a bare `ChatInput`, in microseconds.
#[cfg(test)]
mod chat_editing {
    use super::*;
    use crate::chat::ChatInput;
    use crate::menu::focus::{EDIT_SHORTCUT_MODIFIER, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_SUPER};

    /// The platform's edit-shortcut modifier as winit reports it — Cmd on a
    /// Mac, Ctrl elsewhere. Derived from `cfg!` rather than hard-coded so these
    /// gates assert vanilla's behaviour on whichever machine runs them; the
    /// mapping *itself* is asserted, on both platforms at once, by
    /// [`the_edit_modifier_is_cmd_on_a_mac_and_ctrl_elsewhere`] below.
    fn edit_mod() -> ModifiersState {
        if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        }
    }

    /// A chat line, driven through [`text_key_event`] exactly as
    /// `handle_chat_key_parts` drives it.
    struct Line(ChatInput);

    impl Line {
        fn new(text: &str) -> Self {
            let mut input = ChatInput::new();
            input.set(text);
            Self(input)
        }

        fn press(&mut self, code: KeyCode, modifiers: ModifiersState) {
            let event = text_key_event(PhysicalKey::Code(code), modifiers)
                .expect("this gate presses a text-editing key");
            self.0.handle_key(event);
        }

        fn text(&self) -> &str {
            self.0.as_str()
        }

        fn cursor(&self) -> usize {
            self.0.cursor_position()
        }
    }

    // ---- the routing: a real key press reaches the box ----------------------

    /// Every routing claim, on **one** `WindowApp`, because building a headless
    /// one measures ~73 seconds against ~0.00 for every behaviour gate below —
    /// four separate gates put five minutes on `cargo test -p lodestone-shell
    /// --lib` to re-prove the same dispatch four times.
    ///
    /// The claims are collected rather than asserted in place, for the reason
    /// `CLAUDE.md` gives about an `assert!` inside a loop: a gate that stops at
    /// the first failure turns every later claim into an argument instead of an
    /// observation. This one always reports all six.
    #[test]
    fn a_real_key_press_reaches_the_chat_box() {
        let mut app = WindowApp::new(Config {
            mode: Mode::Headless,
            ..Config::default()
        });
        let mut seen: Vec<(&str, String, String)> = Vec::new();
        let mut claim = |label: &'static str, actual: &str, expected: &str| {
            if actual != expected {
                seen.push((label, actual.to_owned(), expected.to_owned()));
            }
        };

        // Caret motion, and typing where the caret is rather than at the end —
        // the whole point of the line having one.
        app.chat_input.set("hello");
        app.handle_chat_key_parts(
            PhysicalKey::Code(KeyCode::Home),
            None,
            ModifiersState::empty(),
        );
        claim(
            "Home moves the caret",
            &app.chat_input.cursor_position().to_string(),
            "0",
        );
        app.handle_chat_key_parts(
            PhysicalKey::Code(KeyCode::KeyX),
            Some("X"),
            ModifiersState::empty(),
        );
        claim("typing lands at the caret", app.chat_input.as_str(), "Xhello");

        // The fall-through, which no behaviour gate can see: a letter the box
        // declines must still type, a key it consumes must not type as well,
        // and an unrecognised chord must do neither.
        app.chat_input.set("");
        app.handle_chat_key_parts(
            PhysicalKey::Code(KeyCode::KeyA),
            Some("a"),
            ModifiersState::empty(),
        );
        claim("a declined shortcut key still types", app.chat_input.as_str(), "a");
        crate::menu::edit_box::clipboard_seam::set("Z");
        app.handle_chat_key_parts(PhysicalKey::Code(KeyCode::KeyV), Some("v"), edit_mod());
        claim(
            "a consumed shortcut does not also type its letter",
            app.chat_input.as_str(),
            "aZ",
        );
        app.handle_chat_key_parts(PhysicalKey::Code(KeyCode::KeyB), Some("b"), edit_mod());
        claim("an unrecognised chord does nothing", app.chat_input.as_str(), "aZ");

        // Delete had no arm at all before the box was wired in: it fell through
        // to the text path, where a key with no composed `text` reaches nothing.
        app.chat_input.set("abc");
        app.handle_chat_key_parts(
            PhysicalKey::Code(KeyCode::Home),
            None,
            ModifiersState::empty(),
        );
        app.handle_chat_key_parts(
            PhysicalKey::Code(KeyCode::Delete),
            None,
            ModifiersState::empty(),
        );
        claim("Delete reaches the box", app.chat_input.as_str(), "bc");

        // The headline behaviour the bespoke paste arm this replaced could not
        // have had: it appended, because the line had no selection to replace.
        crate::menu::edit_box::clipboard_seam::set("goodbye");
        app.chat_input.set("hello world");
        app.handle_chat_key_parts(
            PhysicalKey::Code(KeyCode::Home),
            None,
            ModifiersState::empty(),
        );
        for _ in 0..5 {
            app.handle_chat_key_parts(
                PhysicalKey::Code(KeyCode::ArrowRight),
                None,
                ModifiersState::SHIFT,
            );
        }
        app.handle_chat_key_parts(PhysicalKey::Code(KeyCode::KeyV), Some("v"), edit_mod());
        claim(
            "paste replaces the selection",
            app.chat_input.as_str(),
            "goodbye world",
        );

        assert!(seen.is_empty(), "chat key routing: {seen:#?}");
    }

    // ---- word boundaries ----------------------------------------------------
    //
    // `EditBox.getWordPosition` breaks on **space and nothing else**, and it
    // strips runs of spaces on the far side of the jump. Each gate below is
    // chosen so a plausible wrong implementation lands on a *different* number
    // — the assertion messages name the one it would land on, because a fixture
    // where both answers coincide is not a test.

    /// Punctuation is not a word break. A "skip to the previous alphanumeric
    /// run" implementation — the obvious one, and what most editors do — stops
    /// at the colon.
    #[test]
    fn word_skip_back_crosses_punctuation_because_only_a_space_breaks_a_word() {
        let mut line = Line::new("say hi:there");
        line.press(KeyCode::ArrowLeft, edit_mod());
        assert_eq!(
            line.cursor(),
            4,
            "vanilla skips back to `hi:there`'s start; a punctuation-aware \
             implementation would stop at 7, just past the colon"
        );
    }

    /// A run of spaces is stripped *and* the word beyond it is crossed, in one
    /// press. Stopping at the near edge of the run is the naive answer.
    #[test]
    fn word_skip_back_strips_the_whole_space_run_and_the_word_before_it() {
        let mut line = Line::new("hi   there");
        line.press(KeyCode::ArrowLeft, edit_mod());
        assert_eq!(line.cursor(), 5, "first press lands at the start of `there`");
        line.press(KeyCode::ArrowLeft, edit_mod());
        assert_eq!(
            line.cursor(),
            0,
            "the second press strips all three spaces and then crosses `hi`; \
             stopping at the run's near edge would give 2, and stopping at the \
             first space would give 4"
        );
    }

    /// Forwards, the strip happens on the far side too: the caret lands at the
    /// start of the next word, not on the first space of the run.
    #[test]
    fn word_skip_forward_lands_past_the_space_run_not_on_it() {
        let mut line = Line::new("hi   there");
        line.press(KeyCode::Home, ModifiersState::empty());
        line.press(KeyCode::ArrowRight, edit_mod());
        assert_eq!(
            line.cursor(),
            5,
            "vanilla strips the spaces after the break; stopping at the first \
             space would give 2"
        );
    }

    /// Trailing spaces are stripped before the word is crossed, so one press
    /// from the very end of `"hi there   "` goes past `there` as well.
    #[test]
    fn word_skip_back_from_trailing_spaces_strips_them_and_then_the_word() {
        let mut line = Line::new("hi there   ");
        line.press(KeyCode::ArrowLeft, edit_mod());
        assert_eq!(
            line.cursor(),
            3,
            "the trailing run is stripped and `there` crossed in one press; an \
             implementation that only strips would stop at 8"
        );
    }

    /// Forwards with no further space runs to the end rather than staying put —
    /// vanilla's `indexOf` returning `-1` becomes the length.
    #[test]
    fn word_skip_forward_with_no_further_space_runs_to_the_end() {
        let mut line = Line::new("hi there");
        line.press(KeyCode::Home, ModifiersState::empty());
        line.press(KeyCode::ArrowRight, edit_mod());
        line.press(KeyCode::ArrowRight, edit_mod());
        assert_eq!(line.cursor(), 8, "the last word ends at the end of the line");
    }

    /// Both ends clamp rather than wrapping or panicking.
    #[test]
    fn word_skip_clamps_at_the_start_and_the_end_of_the_line() {
        let mut line = Line::new("hi there");
        line.press(KeyCode::Home, ModifiersState::empty());
        line.press(KeyCode::ArrowLeft, edit_mod());
        assert_eq!(line.cursor(), 0);
        line.press(KeyCode::End, ModifiersState::empty());
        line.press(KeyCode::ArrowRight, edit_mod());
        assert_eq!(line.cursor(), 8);
    }

    /// The empty line is the degenerate case both directions have to survive.
    #[test]
    fn word_skip_on_an_empty_line_does_nothing() {
        let mut line = Line::new("");
        line.press(KeyCode::ArrowLeft, edit_mod());
        line.press(KeyCode::ArrowRight, edit_mod());
        assert_eq!(line.cursor(), 0);
        assert_eq!(line.text(), "");
    }

    /// Word motion extends a selection under Shift exactly as a plain arrow
    /// does — `moveCursorTo(pos, event.hasShiftDown())`, one shared argument.
    #[test]
    fn shift_with_the_edit_modifier_selects_a_whole_word() {
        let mut line = Line::new("hello world");
        line.press(KeyCode::ArrowLeft, edit_mod() | ModifiersState::SHIFT);
        assert_eq!(line.0.selection(), Some((6, 11)));
    }

    // ---- word deletion ------------------------------------------------------

    #[test]
    fn the_edit_modifier_with_backspace_deletes_a_whole_word() {
        let mut line = Line::new("hi   there");
        line.press(KeyCode::Backspace, edit_mod());
        assert_eq!(line.text(), "hi   ");
        line.press(KeyCode::Backspace, edit_mod());
        assert_eq!(line.text(), "");
    }

    /// `EditBox.deleteWords` checks for a selection **first**, so a word-delete
    /// over a selection removes the selection only — it does not also eat the
    /// word in front of it.
    #[test]
    fn a_live_selection_wins_over_the_word_delete() {
        let mut line = Line::new("hello world");
        line.press(KeyCode::ArrowLeft, ModifiersState::SHIFT);
        line.press(KeyCode::ArrowLeft, ModifiersState::SHIFT);
        assert_eq!(line.0.selection(), Some((9, 11)));
        line.press(KeyCode::Backspace, edit_mod());
        assert_eq!(
            line.text(),
            "hello wor",
            "the selection alone goes; eating `world` too would leave `hello `"
        );
    }

    #[test]
    fn plain_backspace_still_deletes_one_character_at_the_caret() {
        let mut line = Line::new("abc");
        line.press(KeyCode::ArrowLeft, ModifiersState::empty());
        line.press(KeyCode::Backspace, ModifiersState::empty());
        assert_eq!(line.text(), "ac");
        assert_eq!(line.cursor(), 1);
    }

    // ---- caret and selection ------------------------------------------------

    #[test]
    fn shift_extends_the_selection_and_a_plain_arrow_collapses_it() {
        let mut line = Line::new("abcd");
        line.press(KeyCode::ArrowLeft, ModifiersState::SHIFT);
        line.press(KeyCode::ArrowLeft, ModifiersState::SHIFT);
        assert_eq!(line.0.selection(), Some((2, 4)));
        line.press(KeyCode::ArrowLeft, ModifiersState::empty());
        assert_eq!(line.0.selection(), None);
    }

    #[test]
    fn home_and_end_move_the_caret_to_the_ends_and_shift_selects_to_them() {
        let mut line = Line::new("abcd");
        line.press(KeyCode::Home, ModifiersState::empty());
        assert_eq!(line.cursor(), 0);
        line.press(KeyCode::End, ModifiersState::SHIFT);
        assert_eq!(line.0.selection(), Some((0, 4)));
    }

    #[test]
    fn select_all_covers_the_whole_line_and_the_next_insert_replaces_it() {
        let mut line = Line::new("hello world");
        line.press(KeyCode::KeyA, edit_mod());
        assert_eq!(line.0.selection(), Some((0, 11)));
        line.0.push_char('x');
        assert_eq!(line.text(), "x");
    }

    /// `isSelectAll` and friends require the quirked modifier and *neither*
    /// Shift nor Alt, so Cmd+Shift+A must not select all.
    #[test]
    fn the_shortcuts_refuse_an_extra_modifier() {
        let mut line = Line::new("hello");
        line.press(KeyCode::KeyA, edit_mod() | ModifiersState::SHIFT);
        assert_eq!(line.0.selection(), None);
    }

    // ---- clipboard ----------------------------------------------------------

    #[test]
    fn copy_writes_the_selection_to_the_clipboard_and_leaves_the_line_alone() {
        let mut line = Line::new("hello world");
        line.press(KeyCode::Home, ModifiersState::empty());
        for _ in 0..5 {
            line.press(KeyCode::ArrowRight, ModifiersState::SHIFT);
        }
        line.press(KeyCode::KeyC, edit_mod());
        assert_eq!(crate::menu::edit_box::clipboard_seam::get(), "hello");
        assert_eq!(line.text(), "hello world");
    }

    #[test]
    fn cut_removes_the_selection_and_leaves_it_on_the_clipboard() {
        let mut line = Line::new("hello world");
        line.press(KeyCode::Home, ModifiersState::empty());
        for _ in 0..6 {
            line.press(KeyCode::ArrowRight, ModifiersState::SHIFT);
        }
        line.press(KeyCode::KeyX, edit_mod());
        assert_eq!(crate::menu::edit_box::clipboard_seam::get(), "hello ");
        assert_eq!(line.text(), "world");
    }

    #[test]
    fn paste_lands_at_the_caret_when_nothing_is_selected() {
        crate::menu::edit_box::clipboard_seam::set("XY");
        let mut line = Line::new("ab");
        line.press(KeyCode::Home, ModifiersState::empty());
        line.press(KeyCode::KeyV, edit_mod());
        assert_eq!(line.text(), "XYab");
    }

    /// `StringUtil.filterText` is what a paste goes through, so a multi-line
    /// clipboard cannot inject a newline into a chat line, and the 256-char cap
    /// still holds.
    #[test]
    fn a_paste_is_filtered_and_capped_like_typed_text() {
        crate::menu::edit_box::clipboard_seam::set("a\nb\u{a7}c");
        let mut line = Line::new("");
        line.press(KeyCode::KeyV, edit_mod());
        assert_eq!(line.text(), "abc");

        crate::menu::edit_box::clipboard_seam::set(&"z".repeat(300));
        let mut line = Line::new("");
        line.press(KeyCode::KeyV, edit_mod());
        assert_eq!(line.text().chars().count(), crate::chat::MAX_CHAT_LENGTH);
    }

    // ---- the platform modifier itself ---------------------------------------

    /// The owner's own question, and the half a `cfg!`-free assertion can make
    /// on any machine: the quirked modifier is Cmd on a Mac and Ctrl elsewhere,
    /// and — the part that is easy to get wrong by folding one onto the other —
    /// the *other* one must not also work.
    #[test]
    fn the_edit_modifier_is_cmd_on_a_mac_and_ctrl_elsewhere() {
        let cmd = text_key_event(PhysicalKey::Code(KeyCode::ArrowLeft), ModifiersState::SUPER)
            .expect("ArrowLeft is a text key");
        let ctrl = text_key_event(PhysicalKey::Code(KeyCode::ArrowLeft), ModifiersState::CONTROL)
            .expect("ArrowLeft is a text key");
        assert_eq!(cmd.has_control_down_with_quirk(), cfg!(target_os = "macos"));
        assert_eq!(ctrl.has_control_down_with_quirk(), !cfg!(target_os = "macos"));
        assert_ne!(
            cmd.has_control_down_with_quirk(),
            ctrl.has_control_down_with_quirk(),
            "exactly one of the two is the edit modifier — never both, which is \
             what folding Cmd onto MOD_CONTROL in the translation would give"
        );
        assert_eq!(
            EDIT_SHORTCUT_MODIFIER & cmd.modifiers != 0,
            cfg!(target_os = "macos")
        );
    }

    /// Each winit modifier reaches its own GLFW bit. Asserted one at a time and
    /// then combined: two adjacent flags folded into one mask transpose without
    /// a trace if every fixture sets them together.
    #[test]
    fn glfw_modifiers_maps_each_winit_modifier_to_its_own_bit() {
        assert_eq!(glfw_modifiers(ModifiersState::empty()), 0);
        assert_eq!(glfw_modifiers(ModifiersState::SHIFT), MOD_SHIFT);
        assert_eq!(glfw_modifiers(ModifiersState::CONTROL), MOD_CONTROL);
        assert_eq!(glfw_modifiers(ModifiersState::ALT), MOD_ALT);
        assert_eq!(glfw_modifiers(ModifiersState::SUPER), MOD_SUPER);
        assert_eq!(
            glfw_modifiers(ModifiersState::SHIFT | ModifiersState::SUPER),
            MOD_SHIFT | MOD_SUPER
        );
    }

    /// A key no text field acts on declines, which is the caller's signal to
    /// fall through to its own text path.
    #[test]
    fn text_key_event_declines_keys_no_text_field_acts_on() {
        for code in [KeyCode::KeyB, KeyCode::F5, KeyCode::Enter, KeyCode::Escape] {
            assert!(
                text_key_event(PhysicalKey::Code(code), ModifiersState::empty()).is_none(),
                "{code:?} is not a text-editing key"
            );
        }
    }
}

/// The **other** text fields — the server-edit form, the sign, the book and the
/// command block — reached through the same translator the chat line uses.
///
/// # What was missing, and why nothing was red
///
/// Those four already had select-all, copy, cut and paste, because
/// `menu_key_for` produces a [`crate::menu::nav::MenuKey`] for each and
/// `KeyEvent::from_menu_key` knows the GLFW code and modifier bit each stands
/// for. They had **no caret motion at all**: `MenuKey`'s vocabulary had no
/// Left, Right, Home or End, so a real arrow key reached
/// [`crate::menu::edit_box::EditBox`] — which has implemented all four,
/// correctly and with its own tests, the whole time — as nothing.
///
/// That is the closed loop `CLAUDE.md` names: `EditBox`'s own suite is green
/// either way, because it calls `handle_key` directly. The gates below go in
/// through `menu_key_for` and [`crate::menu::nav::MenuNav::key`] instead — the
/// real producer and the real dispatch — so they fail if either end of the
/// chain is missing rather than only if the leaf is.
///
/// The screens are not interchangeable and each is checked: the command block
/// and the sign own their `EditBox` directly and were given an explicit arm,
/// while the server-edit form routes every key through the focus layer and
/// needed no change — a difference that is invisible from the leaf and is
/// exactly what a per-screen gate is for.
#[cfg(test)]
mod menu_text_editing {
    use super::*;
    use crate::menu::command_block::CommandBlockOpen;
    use crate::menu::UiState;
    use crate::menu::nav::{MenuKey, MenuNav};
    use crate::menu::sign_edit::SignEditOpen;

    /// The platform's edit-shortcut modifier as winit reports it. Same
    /// derivation, and same reason, as `chat_editing::edit_mod`.
    fn edit_mod() -> ModifiersState {
        if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        }
    }

    /// One real key press, all the way from a winit code to whichever field the
    /// open screen focuses — `menu_key_for` then `MenuNav::key`, with nothing
    /// short-circuited in between.
    fn press(
        ui: &mut UiState,
        nav: &mut MenuNav,
        code: KeyCode,
        text: Option<&str>,
        modifiers: ModifiersState,
    ) {
        let key = WindowApp::menu_key_for(PhysicalKey::Code(code), text, modifiers)
            .expect("this gate presses a key the menu layer understands");
        nav.key(ui, key);
    }

    /// A `MenuNav` on a fresh temp directory holding one account, so nothing
    /// here reads the developer's real `profiles.json`.
    ///
    /// `MenuNav::default()` reads the real one, and that made these gates take
    /// the person running them as an input: with the ownership gate in place,
    /// a machine whose roster happens to be empty gets the gate intercepting
    /// every keystroke, while the author's machine passes. The seeded account is
    /// the premise every gate in this module wants — a player who can play.
    fn nav_with_account(tag: &str) -> MenuNav {
        let dir = std::env::temp_dir().join(format!(
            "lodestone-input-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir for the seeded roster");
        let mut meta = lodestone_auth::AccountsMetadata::default();
        let id = uuid::Uuid::new_v4();
        meta.upsert(lodestone_auth::AccountProfile {
            profile_id: id,
            username: "OwnerAccount".to_owned(),
            skin_url: None,
            last_used: 1,
        });
        meta.selected = Some(id);
        meta.save_to(&dir.join("profiles.json"))
            .expect("the temp roster must be writable");
        MenuNav::with_path(dir.join("servers.json"))
    }

    /// A command-block screen open on `command`, with the caret at the end.
    fn command_block(command: &str) -> (UiState, MenuNav) {
        let (mut ui, mut nav) = (UiState::default(), nav_with_account("command-block"));
        ui.enter_dev_world();
        nav.open_command_block(
            &mut ui,
            CommandBlockOpen {
                command: command.to_owned(),
                ..CommandBlockOpen::default()
            },
        );
        (ui, nav)
    }

    fn command_field(nav: &MenuNav) -> &crate::menu::edit_box::EditBox {
        &nav.command_block().expect("the screen is open").command
    }

    /// The headline claim, and the one that was false: an arrow key moves the
    /// caret in a menu text field.
    ///
    /// Asserted as a collection so the neuter reports every arm rather than
    /// stopping at the first — with the arm inside a loop, three of these four
    /// numbers would never be printed.
    #[test]
    fn the_caret_keys_reach_a_menu_text_field() {
        let (mut ui, mut nav) = command_block("say hi");
        let mut seen: Vec<(&str, usize, usize)> = Vec::new();
        let mut claim = |label: &'static str, actual: usize, expected: usize| {
            if actual != expected {
                seen.push((label, actual, expected));
            }
        };

        claim("the caret starts at the end", command_field(&nav).cursor_position(), 6);
        press(&mut ui, &mut nav, KeyCode::ArrowLeft, None, ModifiersState::empty());
        claim("Left steps one back", command_field(&nav).cursor_position(), 5);
        press(&mut ui, &mut nav, KeyCode::Home, None, ModifiersState::empty());
        claim("Home goes to the start", command_field(&nav).cursor_position(), 0);
        press(&mut ui, &mut nav, KeyCode::ArrowRight, None, ModifiersState::empty());
        claim("Right steps one forward", command_field(&nav).cursor_position(), 1);
        press(&mut ui, &mut nav, KeyCode::End, None, ModifiersState::empty());
        claim("End goes to the end", command_field(&nav).cursor_position(), 6);

        assert!(seen.is_empty(), "(label, got, want): {seen:#?}");
    }

    /// Typing lands **at** the caret. This is the observable half of the fix:
    /// before it, every character went to the end of the line no matter where
    /// the player had tried to put the caret, because the caret could not be
    /// put anywhere.
    #[test]
    fn a_typed_character_lands_at_the_moved_caret() {
        let (mut ui, mut nav) = command_block("say hi");
        press(&mut ui, &mut nav, KeyCode::Home, None, ModifiersState::empty());
        press(&mut ui, &mut nav, KeyCode::KeyX, Some("X"), ModifiersState::empty());
        assert_eq!(
            command_field(&nav).value(),
            "Xsay hi",
            "appending would give `say hiX`"
        );
    }

    /// The modifiers travel with the key, which is the whole reason this went
    /// through a variant carrying a `KeyEvent` rather than an abstract `Left`:
    /// under the platform's edit modifier the same physical key is a *word*
    /// skip, and under Shift it extends a selection instead of moving.
    #[test]
    fn the_same_arrow_is_four_different_edits_depending_on_the_modifiers() {
        let (mut ui, mut nav) = command_block("say hi:there");

        press(&mut ui, &mut nav, KeyCode::ArrowLeft, None, ModifiersState::empty());
        assert_eq!(command_field(&nav).cursor_position(), 11, "plain: one character");

        let (mut ui, mut nav) = command_block("say hi:there");
        press(&mut ui, &mut nav, KeyCode::ArrowLeft, None, edit_mod());
        assert_eq!(
            command_field(&nav).cursor_position(),
            4,
            "the edit modifier skips a whole word, and a colon is not a word \
             break — a punctuation-aware skip would stop at 7"
        );

        let (mut ui, mut nav) = command_block("say hi:there");
        press(&mut ui, &mut nav, KeyCode::ArrowLeft, None, ModifiersState::SHIFT);
        let field = command_field(&nav);
        assert_eq!(
            (field.cursor_position(), field.highlight_position()),
            (11, 12),
            "Shift extends a selection rather than collapsing to a caret"
        );

        let (mut ui, mut nav) = command_block("say hi:there");
        press(
            &mut ui,
            &mut nav,
            KeyCode::ArrowLeft,
            None,
            edit_mod() | ModifiersState::SHIFT,
        );
        let field = command_field(&nav);
        assert_eq!(
            (field.cursor_position(), field.highlight_position()),
            (4, 12),
            "both together select the whole word"
        );
    }

    /// The sign is a second screen with its own arm, and its own focus notion
    /// (one of four lines). A caret key must reach the *active* line.
    #[test]
    fn the_caret_keys_reach_the_signs_active_line() {
        let (mut ui, mut nav) = (UiState::default(), nav_with_account("sign-edit"));
        ui.enter_dev_world();
        nav.open_sign_edit(
            &mut ui,
            SignEditOpen {
                lines: [
                    "top".to_owned(),
                    "second".to_owned(),
                    String::new(),
                    String::new(),
                ],
                ..SignEditOpen::default()
            },
        );
        // Down moves to line 1 (`AbstractSignEditScreen`'s own line cycling),
        // so the caret keys below must act on `second`, not on `top`.
        press(&mut ui, &mut nav, KeyCode::ArrowDown, None, ModifiersState::empty());
        press(&mut ui, &mut nav, KeyCode::Home, None, ModifiersState::empty());
        press(&mut ui, &mut nav, KeyCode::KeyZ, Some("Z"), ModifiersState::empty());
        let state = nav.sign_edit().expect("the screen is open");
        assert_eq!(state.lines[1].value(), "Zsecond");
        assert_eq!(state.lines[0].value(), "top", "the inactive line is untouched");
    }

    /// The server-edit form takes the same key through a *different* route —
    /// `EditForm::handle_key` hands it to the focus layer rather than to a
    /// field it owns — so it needed no arm of its own. That is a claim about
    /// production wiring, not about `EditBox`, and only a gate on this screen
    /// can make it.
    #[test]
    fn the_caret_keys_reach_the_server_edit_form_through_the_focus_layer() {
        let (mut ui, mut nav) = (UiState::default(), nav_with_account("server-edit"));
        ui.open_server_list();
        ui.open_server_edit();
        for ch in "abc".chars() {
            press(
                &mut ui,
                &mut nav,
                KeyCode::KeyA,
                Some(&ch.to_string()),
                ModifiersState::empty(),
            );
        }
        assert_eq!(nav.form().name(), "abc", "premise: the name field has focus");
        press(&mut ui, &mut nav, KeyCode::Home, None, ModifiersState::empty());
        press(&mut ui, &mut nav, KeyCode::KeyA, Some("Z"), ModifiersState::empty());
        assert_eq!(
            nav.form().name(),
            "Zabc",
            "Home reached the focused box; appending would give `abcZ`"
        );
    }

    /// `menu_key_for` produces the caret keys as
    /// [`crate::menu::nav::MenuKey::Edit`] carrying the real event, and
    /// `from_menu_key` hands that same event straight back — the round trip the
    /// three screens above rely on.
    #[test]
    fn the_caret_keys_round_trip_through_menu_key_and_back() {
        for (code, glfw) in [
            (KeyCode::ArrowLeft, crate::menu::focus::KEY_LEFT),
            (KeyCode::ArrowRight, crate::menu::focus::KEY_RIGHT),
            (KeyCode::Home, crate::menu::focus::KEY_HOME),
            (KeyCode::End, crate::menu::focus::KEY_END),
        ] {
            let key = WindowApp::menu_key_for(
                PhysicalKey::Code(code),
                None,
                ModifiersState::SHIFT | ModifiersState::SUPER,
            )
            .expect("a caret key is a menu key");
            let event = crate::menu::focus::KeyEvent::from_menu_key(key)
                .expect("`Edit` always yields its own event back");
            assert_eq!(event.key, glfw, "{code:?} carries its GLFW code");
            assert!(event.has_shift_down(), "{code:?} keeps Shift");
            assert_eq!(
                event.modifiers,
                crate::menu::focus::MOD_SHIFT | crate::menu::focus::MOD_SUPER,
                "{code:?} keeps every modifier, not just the ones it uses"
            );
        }
    }
}
