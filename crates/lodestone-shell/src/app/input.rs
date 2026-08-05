//! Key and mouse resolution: the `KeyGate` precedence chain and its outcomes.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

/// Which input surface owns the keyboard this instant.
///
/// The four flags [`resolve_key`] needs, read off [`crate::menu::UiState`] at
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
    ToggleDebugOverlay,
    /// Hold-to-show the player list; carries the new held state.
    PlayerList(bool),
    /// Open the chat prompt. `command` pre-fills the `/` prefix.
    OpenChat { command: bool },
    OpenContainer,
    TogglePerspective,
    /// Select hotbar slot `0..=8`.
    SelectSlot(usize),
    /// A `ContainerInput::SWAP` against the **hovered** slot while a container
    /// screen is open: vanilla's number keys and `key.swapOffhand`, which do
    /// *not* change the selected hotbar slot while a screen is up
    /// (`AbstractContainerScreen.checkHotbarKeyPressed`,
    /// `AbstractContainerScreen.java:506-522`).
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
    /// `key.swapOffhand` pressed with **no screen open** (issue #385).
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
    /// (`ServerGamePacketListenerImpl.java:1294-1300`). So this variant carries
    /// no payload — there is nothing to hit-test and nothing to address, which
    /// is exactly what distinguishes it from `ContainerSwap`.
    ///
    /// Vanilla's one guard is `!player.isSpectator()`
    /// (`Minecraft.java:1900-1905`). That is session state rather than key state,
    /// so like `ContainerSwap`'s two guards it lives at the driver's `match` arm.
    SwapOffhand,
    /// A `ContainerInput::Throw` against the **hovered** slot while a
    /// container screen is open — vanilla's `key.drop` inside
    /// `AbstractContainerScreen.keyPressed`
    /// (`AbstractContainerScreen.java:495-501`), gated there on
    /// `hoveredSlot != null && hoveredSlot.hasItem()` — **not** an empty
    /// cursor, which `doClick` applies itself once the click reaches it
    /// (`AbstractContainerMenu.java:513`). `ctrl` selects drop-**stack**
    /// (button `1`) over drop-one (button `0`), the only thing the modifier
    /// changes; carried here rather than read at the driver arm because
    /// `resolve_key` is where every other input decision already lives (see
    /// [`InputAction::Drop`]'s docs).
    ContainerDrop { ctrl: bool },
    /// `key.drop` pressed with **no screen open** — vanilla's own
    /// `Minecraft.handleKeybinds` drop path (`Minecraft.java:1907-1911`). A
    /// different mechanism from [`Self::ContainerDrop`], the same split
    /// [`Self::SwapOffhand`] makes against [`Self::ContainerSwap`]: this one
    /// carries no slot, only which of `ClientAction::DropSelectedItem`/
    /// `DropSelectedItemStack` `ctrl` selects.
    Drop { ctrl: bool },
    /// `key.pickItem` pressed with **no screen open** — vanilla's
    /// `Minecraft.pickBlockOrEntity` (`Minecraft.java:2342-2354`). `ctrl` is
    /// `hasControlDown()`, forwarded as `include_data` on whichever
    /// `ClientAction` fires, exactly the same carry-it-here split
    /// [`Self::Drop`] makes.
    PickItem { ctrl: bool },
    /// `key.pickItem` pressed with a container screen open — `ClickType::CLONE`
    /// against the hovered slot (`AbstractContainerScreen.java:495-501`). No
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
pub(crate) fn resolve_key(
    binds: &Keybinds,
    gate: KeyGate,
    code: Option<KeyCode>,
    pressed: bool,
    ctrl: bool,
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
    } else if gate.container_open && pressed {
        // Vanilla's order, from `AbstractContainerScreen.keyPressed`
        // (`AbstractContainerScreen.java:489-503`): the inventory binding closes
        // the screen first, then `checkHotbarKeyPressed` tries the off-hand key
        // and then the nine hotbar keys. Anything else is swallowed — hence
        // `None`, not a fall-through, so no gameplay binding fires behind an open
        // inventory.
        //
        // The number keys used to fall into that swallow, which is issue #378's
        // part 3: they neither selected a slot (correct) nor swapped (the gap).
        //
        // The off-hand key is here too, as of issue #382: it was blocked purely
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
            // not folded into it — `AbstractContainerScreen.java:494-500` is
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
    } else if binds.is(InputAction::DebugOverlay, code) && pressed {
        Some(KeyOutcome::ToggleDebugOverlay)
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
    } else if let Some(slot) = hotbar_slot_for(binds, code)
        && pressed
        && gate.gameplay
    {
        Some(KeyOutcome::SelectSlot(slot))
    } else if binds.is(InputAction::SwapOffhand, code) && pressed && gate.gameplay {
        // Issue #385: the *gameplay* half of `key.swapOffhand`. The container
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

/// The action a gameplay off-hand-key press should send, or `None` (issue #385).
///
/// Split out of the driver so the *decision* — which is entirely "is this player
/// a spectator" — is a pure function a test can drive.
/// `WindowApp::send_offhand_swap` is the few lines that read the game mode and
/// push the result, and they are the part no test in this crate can reach (see
/// `docs/keybindings.md`'s gotcha on the effects `match`).
///
/// # No local prediction, and that is the vanilla behaviour rather than a shortcut
///
/// `Minecraft.handleKeybinds` (`Minecraft.java:1900-1905`) is the entire client
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
/// **server-side only** (`ServerGamePacketListenerImpl.java:1294-1300` does the
/// three-way exchange plus `stopUsingItem`), and the client learns the result
/// from the ordinary inventory-sync packets that follow.
///
/// This is why issue #385's round-trip worry does not apply here the way it does
/// to #381's block placement: vanilla *does* predict a placement, so not
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
/// `!player.isSpectator()` (`Minecraft.java:1908`), the exact same guard
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
