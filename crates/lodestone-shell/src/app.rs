//! The windowed driver and the headless / connect runners.
//!
//! This is deliberately the *thin* layer: all simulation lives in [`crate::sim`]
//! and all GPU state in [`crate::gpu`] / [`crate::hud`]. Here we only translate
//! winit lifecycle + input events into calls on those, and own the per-frame
//! acquire → render → present dance — including treating surface loss/outdated
//! as routine (on macOS they are), reconfiguring and moving on.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lodestone_render::{
    GpuContext, HeadlessTarget, RenderTarget, TargetError, fog::FogSettings, window::attach_window,
};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use crate::chat::ChatInput;
use crate::config::{Config, Mode};
use crate::container::{
    ContainerFrame, ContainerRenderer, MenuButton, MenuContext, MenuHit, MenuInput,
    MenuKey as ContainerMenuKey, hit_test_with_scale,
};
use crate::effects::EffectsRenderer;
use crate::gpu::RenderState;
use crate::hud::{HotbarSlot, HudFrame, HudRenderer};
use crate::keybinds::{Binding, InputAction, Keybinds};
use crate::menu::nav::{MenuAction, MenuKey, MenuNav};
use crate::menu::render::MenuRenderer;
use crate::menu::status::StatusCache;
use crate::menu::{SessionKind, UiState};
use crate::net::NetClient;
use crate::sim::Sim;
use lodestone_assets::ResourceLocation;
use lodestone_controller::{Action, InputState};
use lodestone_game::click::{Click, PlayerCtx};
use lodestone_game::menu::Menu;
use lodestone_game::recipe::RecipeBook;

/// Entry point: dispatch on the configured mode.
///
/// # Errors
/// Returns an error if GPU bring-up or the event loop fails.
pub fn run(config: Config) -> anyhow::Result<()> {
    match config.mode {
        Mode::Headless => run_headless(config),
        Mode::Connect => run_connect(config),
        Mode::Window => run_windowed(config),
    }
}

/// Sky distance fog sized to the shell's real render distance, so terrain
/// dissolves into the sky exactly where chunks stop loading rather than at the
/// render crate's default 8-chunk fallback. Driven once at render bring-up on
/// both the windowed and headless paths (render distance is fixed for the
/// session).
///
/// Delegates to [`crate::sim::fog_for_render_distance`] so the colour and the
/// fade band have one definition shared with the frame clear — a second copy of
/// the sky colour here is how the horizon ends up banding in a colour the sky
/// never is.
fn sky_fog(render_distance: u32) -> FogSettings {
    crate::sim::fog_for_render_distance(render_distance)
}

/// Maps a winit mouse button to the container-click gesture it drives.
/// `None` for anything but left/right/middle (e.g. the back/forward mouse
/// buttons some mice send), which the container screen has no use for.
///
/// **Deliberately not routed through [`crate::keybinds`]**, and vanilla agrees:
/// `AbstractContainerScreen` tests raw button indices 0/1/2 rather than
/// consulting a `KeyMapping`. Slot-click gestures are container-UI chrome, not
/// gameplay bindings — the same boundary that keeps the arrow keys out of the
/// keybind table (see that module's docs).
fn menu_button_for(button: MouseButton) -> Option<MenuButton> {
    Some(match button {
        MouseButton::Left => MenuButton::Left,
        MouseButton::Right => MenuButton::Right,
        MouseButton::Middle => MenuButton::Pick,
        _ => return None,
    })
}

/// The movement [`Action`], if any, that `code` drives under `binds`.
///
/// Replaces the old hardcoded `action_for`. Two behavioural notes:
///
/// * The old table bound Sneak to **either** shift and Sprint to **either**
///   control. A [`Binding`] names one physical key, matching vanilla (whose
///   defaults are `LEFT_SHIFT` / `LEFT_CONTROL` specifically), so the
///   right-hand modifiers no longer walk. This is the one intentional
///   behaviour change in the refactor; the right-hand keys are now rebindable
///   to whatever the player wants instead of being a silent alias.
/// * The scan is over [`InputAction::ALL`] in declaration order, so the
///   movement actions are checked before anything else and a key bound to two
///   movement actions resolves to the earlier one — deterministic rather than
///   map-iteration-order dependent.
fn movement_action_for(binds: &Keybinds, code: KeyCode) -> Option<Action> {
    InputAction::ALL
        .into_iter()
        .filter(|a| binds.is(*a, code))
        .find_map(InputAction::movement)
}

/// Wire button number for an off-hand `SWAP` click — vanilla's literal `40` in
/// `AbstractContainerScreen.checkHotbarKeyPressed` and the `buttonNum == 40`
/// guard in `AbstractContainerMenu.doClick`'s `SWAP` arm. Not a slot index: the
/// off-hand's *native* index happens to be 40 too, which is why this is named
/// after the button rather than the slot.
const OFFHAND_SWAP_BUTTON: i32 = 40;

/// The hotbar slot index `0..=8` that `code` selects under `binds`, or `None`.
///
/// Replaces the old hardcoded number-row table. The `Hotbar1 → 0` off-by-one
/// lives in [`InputAction::hotbar_slot`], not here, so there is one place to get
/// it wrong.
fn hotbar_slot_for(binds: &Keybinds, code: KeyCode) -> Option<usize> {
    InputAction::ALL
        .into_iter()
        .filter(|a| binds.is(*a, code))
        .find_map(InputAction::hotbar_slot)
}

/// The gameplay action, if any, that this **mouse button** invokes under
/// `binds` — the mouse-side twin of [`movement_action_for`].
///
/// Only attack and use are mouse-bindable in practice, but this scans the whole
/// table so a player who binds, say, jump to the middle button gets what they
/// asked for rather than nothing.
fn mouse_action_for(binds: &Keybinds, button: MouseButton) -> Option<InputAction> {
    InputAction::ALL
        .into_iter()
        .find(|a| binds.is_mouse(*a, button))
}

/// Whether the world's own HUD — hotbar, hearts, hunger, the XP bar — draws on
/// this screen.
///
/// **It belongs to the world, not to active play.** Vanilla extracts the entire
/// HUD whenever a level is loaded and lets whatever screen is open paint its own
/// translucent background *over* it; that background is the dim, and it is the
/// only reason an open inventory looks different from playing:
///
/// - `GameRenderer.extract` computes
///   `readyForLevelRendering = resourcesLoaded && advanceGameTime && level != null`
///   and passes it straight into the GUI (`GameRenderer.java:377,389`) — note it
///   asks about the *level*, never about `screen`.
/// - `Gui.extractRenderState` calls `hud.extractRenderState` under that flag
///   alone (`Gui.java:152-156`), then draws the open screen **afterwards**
///   (`Gui.java:171-189`), i.e. on top.
/// - `Hud.extractRenderState` itself gates only on F1 (`isHidden`) and
///   `LevelLoadingScreen` (`Hud.java:218-221`). Inside it, the hotbar, hearts,
///   hunger, the XP bar and the held-item name are gated on **game mode** only
///   (`Hud.java:534-562`) — nothing there consults `screen()`.
///
/// Exactly two HUD elements in vanilla do consult `screen()`, and neither is a
/// vital: the potion-effect icons (`Hud.java:486-488`, suppressed only when the
/// screen `showsActiveEffects()`, which is overridden `true` by `InventoryScreen`
/// and `CreativeModeInventoryScreen` because those draw their own) and the
/// subtitle overlay (`Hud.java:238-241`). The crosshair is **not** one of them
/// (`Hud.java:439-470` gates on camera type and spectator mode only) — we still
/// hide it with [`crate::menu::UiState::is_playing`], a deliberate divergence
/// while container screens have no dimmed background pass to hide behind
/// (issue #51).
///
/// [`Screen::Connecting`] is excluded because there is no world yet — it reaches
/// the world render path only because it is not an `owns_frame` screen. The
/// menu and error screens never get here at all: `draw_menu` returns early.
fn hud_follows_world(screen: crate::menu::Screen) -> bool {
    use crate::menu::Screen;
    // `Screen::Death` (issue #103) follows the same rule as `Paused`: vanilla's
    // `Hud.extractRenderState` gates only on F1/`LevelLoadingScreen`, never on
    // which screen is open, so the hotbar/hearts/hunger keep drawing (dimmed by
    // the death screen's own background pass) behind the death screen too.
    matches!(
        screen,
        Screen::Playing | Screen::Chat | Screen::Container | Screen::Paused | Screen::Death
    )
}

/// Vanilla's `ScrollWheelHandler.onMouseScroll` (issue #203): folds a
/// sensitivity-scaled, possibly-fractional scroll offset into a whole number
/// of hotbar slots, carrying the remainder in `accum` across calls so a
/// `mouseWheelSensitivity` below 1.0 does not silently drop sub-notch scroll.
///
/// `accum` resets to zero on a direction reversal
/// (`Math.signum(scaledYOffset) != Math.signum(this.accumulatedScrollY)`,
/// `ScrollWheelHandler.java:14-16`) rather than fighting the new direction with
/// old carry — one hard flick back should not need to "pay off" the previous
/// direction's fractional debt first.
fn accumulate_scroll(accum: &mut f64, scaled: f64) -> i32 {
    if *accum != 0.0 && scaled.signum() != accum.signum() {
        *accum = 0.0;
    }
    *accum += scaled;
    let whole = accum.trunc();
    *accum -= whole;
    whole as i32
}

/// What one raw physical key means while a Controls-menu bind button is
/// mid-capture (issue #15's last hop) — extracted as a pure function so the
/// decision is unit-testable without a window, the same reason
/// [`resolve_key`] and [`WindowApp::menu_key_for`] are split out. Deliberately
/// **not** `menu_key_for`: that function drops any physical key with no
/// printable `text` (F-keys, modifiers, arrows other than Up/Down), which is
/// exactly the common rebind target a Controls menu exists to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureKey {
    /// Escape: cancel the capture without changing the binding. Vanilla sets
    /// `InputConstants.UNKNOWN` on Escape unconditionally
    /// (`KeyBindsScreen.java:73-74`); this client deliberately does not — see
    /// [`crate::menu::nav::MenuNav::capture_binding`]'s own doc on why
    /// unconditional-Unbound is the `Pause` hazard.
    Cancel,
    /// Any other identified key: finish the capture with it.
    Bind(KeyCode),
}

fn capture_key_for(physical_key: PhysicalKey) -> Option<CaptureKey> {
    match physical_key {
        PhysicalKey::Code(KeyCode::Escape) => Some(CaptureKey::Cancel),
        PhysicalKey::Code(code) => Some(CaptureKey::Bind(code)),
        // No `KeyCode` to persist — nothing to do, the same as `menu_key_for`
        // falling through to its own `_ => {}`.
        PhysicalKey::Unidentified(_) => None,
    }
}

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

// ---------------------------------------------------------------------------
// Frame pacing
// ---------------------------------------------------------------------------

/// Vanilla's cap on how many 20 Hz client ticks a single update may run.
///
/// Read from the decompiled 26.2 client, not guessed:
/// `.cache/mc/26.2/client-src/net/minecraft/client/Minecraft.java:262` declares
/// `private static final int MAX_TICKS_PER_UPDATE = 10;` and `:1176` applies it
/// as `for (int i = 0; i < Math.min(10, ticksToDo); i++)`. Note *where* the cap
/// lives: `DeltaTracker.Timer::advanceGameTime` returns the full uncapped tick
/// count and keeps the sub-tick residual, and `runTick` then simply **runs at
/// most ten of them and drops the rest**. Missed real time is discarded, never
/// replayed — which is the whole point.
///
/// **Aliased, not re-derived**, since §4.1(c): the number the simulation actually
/// clamps to lives beside the one accumulator, and this file's copy of it was how
/// the shell came to run five catch-up ticks while claiming ten.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const MAX_TICKS_PER_UPDATE: u32 = lodestone_ecs::MAX_CATCH_UP_TICKS;

/// Length of one client tick in seconds (20 Hz).
///
/// An alias, like [`MAX_TICKS_PER_UPDATE`]: the accumulator that counts in this
/// period lives in `lodestone-ecs`, and a local copy is how the two clocks §4.1(c)
/// unified came to disagree in the first place. Only this file's tests and doc
/// links read it, hence the `dead_code` allowance in non-test builds.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const TICK_SECS: f64 = lodestone_ecs::TICK_PERIOD;

/// The most real time one update may hand the simulation, in seconds.
///
/// `10 × 0.05 = 0.5`. Anything beyond this is dropped rather than replayed, so
/// alt-tabbing away for a minute costs ten ticks of catch-up, not 1200. The pacer
/// clamps here and `FrameClock::begin_frame` clamps to the same constant, so the
/// two agree by construction rather than by coincidence.
pub(crate) const MAX_CATCHUP_SECS: f64 = lodestone_ecs::MAX_CATCH_UP_SECS;

/// Presentation rate while the window is visible but **unfocused**. The
/// simulation keeps running at the full 20 Hz either way; only presentation is
/// throttled.
pub(crate) const UNFOCUSED_FPS: u32 = 30;

/// [`UNFOCUSED_FPS`] as the interval between presented frames.
pub(crate) const UNFOCUSED_FRAME_INTERVAL: Duration =
    Duration::from_nanos(1_000_000_000 / UNFOCUSED_FPS as u64);

/// How long the event loop sleeps between iterations while unfocused. Kept
/// comfortably shorter than [`TICK_SECS`] so the tick loop is never the thing
/// being paced — if this ever exceeded 50 ms the sim would fall behind the
/// server even though we are "still ticking".
pub(crate) const BACKGROUND_POLL: Duration = Duration::from_millis(8);

/// Maximum gap between two left-clicks on the same container slot for the
/// second to count as a double-click gather. Winit hands us raw button
/// up/down events with no click-count of its own, so this app tracks it —
/// [`container::MenuInput::press`] still requires the *same slot* on top of
/// this timing before it arms the gather.
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// What one iteration of the event loop should do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FrameStep {
    /// Real seconds to advance the simulation by, already clamped to
    /// [`MAX_CATCHUP_SECS`].
    pub dt: f64,
    /// Whether to acquire a swapchain image and draw. When `false` the sim still
    /// steps — we skip *presenting*, never ticking.
    pub render: bool,
}

/// Owns the frame clock and decides, per iteration, how far to advance the sim
/// and whether to draw.
///
/// ## Why this lives here and not in `sim`
///
/// `Sim::step` already clamps its own accumulator, but the *policy* — how much
/// catch-up is acceptable, and whether an unfocused window should present —
/// belongs to the driver, alongside the winit focus/occlusion events that inform
/// it. Keeping the clock here also means the sim is advanced by an explicit,
/// injectable `dt`, so this is testable against a real `Sim` with a synthetic
/// clock and no window.
///
/// ## The bug this exists to fix
///
/// Presentation used to gate simulation: `redraw` stepped the sim and then
/// acquired a swapchain image in the same call, with the GPU-readiness guard
/// *before* the step. A backgrounded or occluded window makes `acquire()` slow
/// (macOS stops vending drawables to an occluded `CAMetalLayer` and the call
/// stalls until it times out), so the loop's iteration rate collapsed — and with
/// it the tick rate, since ticks only advanced when a frame did. Skipping
/// presentation instead of skipping the tick is what keeps keep-alives and
/// movement packets flowing while tabbed out; a client the server considers
/// stalled stops receiving chunks entirely.
///
/// ## Why the unfocused frame schedule is absolute, not "elapsed since the last
/// frame"
///
/// This was measured, not reasoned about. The obvious gate —
/// `now - last_render >= interval`, then `last_render = now` — **loses frames**,
/// because it can only fire on a loop iteration and each iteration pushes the
/// next deadline out by however far it overshot. At a 120 Hz loop with a 30 fps
/// target there are only four chances per interval, and the accumulated
/// overshoot cost 4 of every 30 frames: a one-second unfocused run presented
/// **26** frames, not 30. A 30 fps limiter that silently delivers 26 is the
/// quiet kind of wrong.
///
/// So the deadline is absolute: `next_render` advances by exactly one interval
/// from *itself*, never from `now`, and phase error cannot accumulate. The one
/// exception is a stall longer than an interval, where the schedule is re-based
/// onto `now` — otherwise coming back from a two-minute alt-tab would present a
/// burst of catch-up frames, which is the same mistake as replaying catch-up
/// ticks.
#[derive(Debug)]
pub(crate) struct FramePacer {
    last_step: Instant,
    /// The absolute time the next unfocused frame is due. Advanced by whole
    /// intervals so the presented rate does not drift below the target.
    next_render: Instant,
    focused: bool,
    occluded: bool,
}

impl FramePacer {
    /// A pacer whose clock starts at `now`, focused and visible.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_step: now,
            next_render: now + UNFOCUSED_FRAME_INTERVAL,
            focused: true,
            occluded: false,
        }
    }

    /// Record a focus change. Does **not** touch the step clock: the elapsed
    /// time since the last step is real time the sim still owes, and it is
    /// clamped on the next `begin_frame` like any other stall.
    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Record an occlusion change (window fully covered / minimised).
    pub(crate) fn set_occluded(&mut self, occluded: bool) {
        self.occluded = occluded;
    }

    /// Whether the window currently has focus. Test-only: the app never asks,
    /// because focus must not gate anything except presentation — which
    /// [`Self::begin_frame`] already decides.
    #[cfg(test)]
    pub(crate) fn focused(&self) -> bool {
        self.focused
    }

    /// Advance the frame clock to `now` and decide what this iteration does.
    ///
    /// The returned `dt` is the real elapsed time **clamped** to
    /// [`MAX_CATCHUP_SECS`]; the excess is dropped, exactly as vanilla drops
    /// ticks past `MAX_TICKS_PER_UPDATE`.
    pub(crate) fn begin_frame(&mut self, now: Instant) -> FrameStep {
        let dt = now.saturating_duration_since(self.last_step).as_secs_f64();
        self.last_step = now;

        let render = if self.occluded {
            // Nothing is on screen to update, and acquiring a drawable is what
            // stalls. Drop presentation entirely and keep ticking.
            false
        } else if self.focused {
            // Vsync (or the compositor) paces us; do not second-guess it.
            self.next_render = now + UNFOCUSED_FRAME_INTERVAL;
            true
        } else {
            now >= self.next_render
        };
        if render && !self.focused {
            // Advance the deadline from itself, not from `now`, so overshoot
            // does not accumulate into a lower delivered frame rate. Re-base
            // only when we are more than a whole interval late, which means a
            // real stall rather than ordinary jitter — replaying the backlog as
            // a burst of frames would be the presentation-side version of the
            // catch-up-tick bug.
            self.next_render += UNFOCUSED_FRAME_INTERVAL;
            if self.next_render <= now {
                self.next_render = now + UNFOCUSED_FRAME_INTERVAL;
            }
        }

        FrameStep {
            dt: dt.min(MAX_CATCHUP_SECS),
            render,
        }
    }

    /// How the event loop should wait after this iteration: spin while focused
    /// (vsync paces us), otherwise sleep briefly so a backgrounded window stops
    /// burning a core while still ticking well above 20 Hz.
    pub(crate) fn control_flow(&self, now: Instant) -> ControlFlow {
        if self.focused && !self.occluded {
            ControlFlow::Poll
        } else {
            ControlFlow::WaitUntil(now + BACKGROUND_POLL)
        }
    }
}

// ---------------------------------------------------------------------------
// Windowed
// ---------------------------------------------------------------------------

/// Why an integrated-server (Singleplayer) launch could not proceed.
///
/// Typed rather than a string so the Error screen can distinguish causes. There
/// is exactly one today, and it is a *build* property rather than a runtime
/// failure: everything else on this path is infallible (see
/// [`launch_singleplayer`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchError {
    /// No version family is compiled into this build that can be **hosted**, so
    /// `lodestone_registry::server_protocol_for_protocol` returned `None`.
    ///
    /// This is what `--no-default-features` produces, and it is the whole reason
    /// the shell asks the registry for a trait object instead of naming a
    /// version: the version-free build must *compile* and report, not fail to
    /// build. It is also reachable with a family compiled in but no
    /// `ServerProtocol` for it — a family can be joinable and unhostable.
    NoVersionFamily {
        /// The protocol number that found no server protocol.
        protocol: i32,
    },
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::NoVersionFamily { protocol } => {
                let compiled = lodestone_registry::compiled_server_families();
                write!(
                    f,
                    "Singleplayer is unavailable in this build: no version family \
                     compiled in can host protocol {protocol}"
                )?;
                if compiled.is_empty() {
                    write!(f, " (none are). Build with the `live` feature.")
                } else {
                    write!(f, " (this build can host: {}).", compiled.join(", "))
                }
            }
        }
    }
}

/// Start singleplayer: an integrated server in-process, with the client speaking
/// to it over an in-memory duplex (issue #287).
///
/// This is vanilla's own architecture — one client, one dispatch, a different
/// transport — and the whole of it is three steps:
///
/// 1. ask the registry for the **serverbound** half of the version family
///    (`server_protocol_for_protocol`, the twin of the `adapter_for_protocol`
///    call `net.rs` already makes for the clientbound half);
/// 2. hand that trait object to [`NetClient::open_singleplayer`], which starts
///    `lodestone_server::IntegratedServer::open_in_memory` on the net thread's
///    runtime and connects the client to the returned duplex;
/// 3. attach the result to the `Sim` exactly as a multiplayer connect does.
///
/// **The shell names no version here, and that is load-bearing rather than
/// stylistic.** `cargo check -p lodestone-shell --no-default-features` exists to
/// prove this crate compiles with *no* version family, and a `V770ServerProtocol`
/// on this line would break it — which is why the previous version of this
/// function was a deliberate stub returning an error. What changed is not the
/// constraint; it is that the registry now has a serverbound table to ask.
///
/// The only failure is [`LaunchError::NoVersionFamily`]: `open_in_memory` cannot
/// fail (no port to bind), and `connect_with` cannot fail (no dial). So a
/// successful return means a server is running and a client is talking to it —
/// though login is asynchronous, so "running" is proven by the session reaching
/// `Screen::Playing`, not by this returning `Ok`.
pub(crate) fn launch_singleplayer(
    protocol: i32,
    view_radius: i32,
    session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
    seed: i64,
) -> Result<NetClient, LaunchError> {
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)
        .ok_or(LaunchError::NoVersionFamily { protocol })?;
    Ok(NetClient::open_singleplayer(
        server_protocol,
        protocol,
        seed,
        view_radius,
        session,
    ))
}

/// Vanilla's own seed rule (issue #190's queued patch) —
/// `WorldOptions.parseSeed`/`randomSeed()`
/// (`.cache/mc/26.2/client-src/net/minecraft/world/level/levelgen/
/// WorldOptions.java:75-89`): trim, empty means a fresh random `i64`, a valid
/// `i64` literal is used verbatim, and anything else — vanilla accepts
/// free-text seeds rather than rejecting them — falls back to Java's own
/// `String.hashCode()` widened (sign-extended) to `i64`.
///
/// `None` means "use the bundled world's own seed" (`Screen::WorldSelect`'s
/// **Play Selected World**, which collects no seed of its own); `Some(cfg)`
/// is `Screen::CreateWorld`'s **Create** button, carrying whatever the player
/// typed into the Seed field (`WorldCreationConfig::seed`, empty by default).
fn resolve_launch_seed(config: Option<&crate::menu::create_world::WorldCreationConfig>) -> i64 {
    match config {
        Some(cfg) => parse_seed(&cfg.seed),
        None => crate::menu::world_select::BUNDLED_WORLD.seed,
    }
}

fn parse_seed(raw: &str) -> i64 {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return random_seed();
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return n;
    }
    i64::from(java_string_hash_code(trimmed))
}

/// `RandomSource.create().nextLong()` — vanilla asks for *some* fresh long,
/// with no algorithm this port needs to match (a world seed is opaque once
/// generated); `std::collections::hash_map::RandomState` already draws a
/// fresh random key from the OS per instance for exactly this reason, so
/// hashing a timestamp through one needs no new dependency for a value this
/// crate treats as a black box.
fn random_seed() -> i64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    hasher.write_u128(nanos);
    hasher.finish() as i64
}

/// Java's `String.hashCode()`: `s[0]*31^(n-1) + … + s[n-1]`, over UTF-16 code
/// units (not bytes, not `char`s) with wrapping 32-bit arithmetic — the exact
/// formula `WorldOptions.parseSeed`'s catch arm calls. Widening the result to
/// `i64` (its caller's job, not this function's) is sign-extending, matching
/// Java's own `int`→`long` widening.
fn java_string_hash_code(s: &str) -> i32 {
    let mut h: i32 = 0;
    for unit in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    h
}

/// Whether argv asked for a connection, i.e. whether to bypass the main menu.
///
/// True for `--live`, or for any `--host`/`--port` at all.
///
/// This used to compare against `Config::default()`, which made
/// `--host 127.0.0.1 --port 25565` — spelling out the defaults — indistinguishable
/// from passing nothing, so it silently landed on the main menu. That is the
/// launch the two-worlds report came from: the user asked for a server on the
/// command line and got the title screen. [`Config::address_given`] now records
/// whether the flag was *seen*, which is the question actually being asked.
fn requested_a_connection(config: &Config) -> bool {
    config.connect_in_window || config.address_given
}

/// Extrapolates the server's `time_of_day` continuously between the ~1/sec
/// `SET_TIME` packets that are its only source (`WorldTime` is a flat
/// snapshot — see the doc at both [`WindowApp::connect_to`] call sites for
/// why the raw value alone made the sky's cloud scroll visibly step once a
/// second). `advance` is meant to be polled once per frame from a
/// [`RenderState::set_time_of_day_source`](crate::gpu::RenderState::set_time_of_day_source)
/// closure: on a still-current tick it adds elapsed wall-clock time at the
/// standard 20 ticks/sec, and on a new tick from the network it re-anchors —
/// the same local-prediction-then-correct shape vanilla's own client-side
/// day-time uses. `Mutex`, not `Cell`, only because the closure trait bound is
/// `Fn` (shared refs) rather than `FnMut`.
struct ContinuousTimeOfDay(std::sync::Mutex<Option<(i64, Instant)>>);

impl ContinuousTimeOfDay {
    fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    fn advance(&self, server_tick: i64) -> i64 {
        let mut anchor = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        match *anchor {
            Some((tick, at)) if tick == server_tick => {
                tick + (now.duration_since(at).as_secs_f64() * 20.0) as i64
            }
            _ => {
                *anchor = Some((server_tick, now));
                server_tick
            }
        }
    }
}

/// How long a lightning flash is held, in wall-clock time.
///
/// [`lodestone_render::LIGHTNING_FLASH_TICKS`] is 5 game ticks, which is 250 ms at
/// the standard 20 ticks/sec. Timed off the wall clock rather than the tick clock
/// because the two consumers are a per-*frame* render source and a per-frame fog
/// composition, neither of which has a tick edge to hang a countdown on — the same
/// reason [`ContinuousTimeOfDay`] extrapolates rather than stepping.
const LIGHTNING_FLASH_HOLD: Duration = Duration::from_millis(
    (lodestone_render::LIGHTNING_FLASH_TICKS as u64) * 1000 / 20,
);

/// Resolves the net thread's raw weather scalars into a
/// [`lodestone_render::WeatherState`], and times the lightning flash.
///
/// One of these is shared (`Arc`) between the `set_sky_darken_source` closure and
/// `redraw`'s per-frame fog/column composition, so both halves of "weather" read
/// the **same** state on the same frame. Two independent reads of the cell would
/// be almost identical and occasionally not, and a lightmap disagreeing with the
/// sky it is lit by is exactly the class of bug that reads as a shader problem.
///
/// `Mutex` for the same reason [`ContinuousTimeOfDay`] uses one: the render
/// source's trait bound is `Fn`, not `FnMut`.
#[derive(Debug)]
struct WeatherTracker {
    cell: crate::net::SharedWeather,
    flash: std::sync::Mutex<(u64, Option<Instant>)>,
}

impl WeatherTracker {
    fn new(cell: crate::net::SharedWeather) -> Self {
        Self {
            cell,
            flash: std::sync::Mutex::new((0, None)),
        }
    }

    /// This frame's weather.
    ///
    /// The two levels are handed to `WeatherState` **raw** and it does the
    /// clamping and the `thunder × rain` composition — see
    /// `lodestone_render::weather`'s module doc for why composing them here
    /// instead would black out a clear sky on join.
    fn state(&self) -> lodestone_render::WeatherState {
        let snapshot = self.cell.snapshot();
        let mut state = lodestone_render::WeatherState::clear();
        state.apply_rain_level(snapshot.rain_level);
        state.apply_thunder_level(snapshot.thunder_level);

        let now = Instant::now();
        let mut flash = self
            .flash
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if flash.0 != snapshot.lightning_seq {
            // A new bolt (or several — the seq can jump by more than one between
            // frames; one flash either way, which is also what vanilla shows).
            *flash = (snapshot.lightning_seq, Some(now));
        }
        if let Some(started) = flash.1 {
            if now.duration_since(started) < LIGHTNING_FLASH_HOLD {
                state.flash();
            } else {
                // Cleared rather than left set, so a long session does not keep
                // re-reading a stale `Instant` every frame.
                flash.1 = None;
            }
        }
        state
    }
}

/// The world knowledge [`lodestone_render::extract_columns`] needs, resolved from
/// **one** light sample per frame rather than one per column.
///
/// # Why one sample, and what it costs
///
/// Vanilla samples a heightmap, a biome and a lightmap **per column** (441 of each
/// at the default radius, `WeatherEffectRenderer.java:72-88`), reading a level it
/// owns directly. This client reaches the world through
/// [`crate::net::entity_light_at`], which takes the client's world lock **per
/// call**; 441 locks per frame at 60 fps is not a trade worth making for a first
/// landing, so the probe is built from a single sample at the camera and answers
/// every column from it.
///
/// The three divergences, in order of how visible they are:
///
/// * **No per-column terrain height.** `column_top` is `None`, so every column
///   spans `camera_y ± radius` instead of stopping at the ground. Invisible — the
///   pass is depth-tested, so sub-surface fragments are occluded — but it costs
///   vertices that vanilla would not draw. Closing it needs a `column_height`
///   accessor on `ClientHandle`; the heightmaps are already decoded into
///   `lodestone_world::LoadedChunk::heightmaps` and nothing reads them yet.
/// * **Sky visibility is the camera's, not the column's.** In a cave the camera's
///   own sky light is 0 and the whole pass draws nothing, which is right; standing
///   at a cave *mouth* it draws rain across the cavern, which is wrong. Vanilla's
///   per-column `canSeeSky` is what fixes it, and it needs the same heightmap
///   accessor.
/// * **One light level for the whole square.** Rain seen through a shaded gully is
///   as bright as rain in the open. Barely visible in practice: rain is drawn
///   outdoors, where sky light is uniform.
///
/// Rain-versus-snow is **not** in that list, because it is not an approximation
/// here — it is missing data. See
/// [`lodestone_render::WeatherProbe::precipitation`].
struct ShellWeatherProbe {
    /// The already-resolved lightmap term at the camera, weather included.
    light: f32,
    /// Whether any sky light reaches the camera. `false` draws no precipitation at
    /// all, which is the cave case.
    sky_visible: bool,
    /// The client-owned world, resolved once per frame the same way `packed`
    /// above is (a plain `Arc` clone out of the `SharedHandle`'s `OnceLock`,
    /// not a lock held across the frame) — needed for the per-column biome
    /// lookup [`Self::biome_precipitation`] does. `None` before login.
    handle: Option<Arc<lodestone_client::ClientHandle>>,
    /// Every biome's declared climate (issue #25), published once at `Login`
    /// by [`crate::net::forward`]'s `BiomeClimates` arm. `None` off a live
    /// connection.
    biome_climates: Option<crate::net::SharedBiomeClimates>,
}

impl ShellWeatherProbe {
    /// Resolve `(x, y, z)`'s standing biome and translate its declared
    /// climate to a [`lodestone_render::Precipitation`] via vanilla's own
    /// `getPrecipitationAt` (`Biome.java:104-108`), height-adjusted the same
    /// way `Biome.getHeightAdjustedTemperature` is (`Biome.java:110-121`).
    ///
    /// `None` at any hop — world not loaded, section elided (all-air), the
    /// climate table still empty, or the biome's own `temperature`/
    /// `has_precipitation` unresolved — is exactly "the server has not told
    /// us yet", the same open set `Sim::biome_sky_color`'s doc already
    /// enumerates for the sky-colour lookup this mirrors. The caller decides
    /// the fallback.
    fn biome_precipitation(&self, x: i32, y: i32, z: i32) -> Option<lodestone_render::Precipitation> {
        let handle = self.handle.as_ref()?;
        let dims = handle.world_dimensions()?;
        let chunk = lodestone_client::ChunkPos {
            x: x.div_euclid(16),
            z: z.div_euclid(16),
        };
        let base_si = dims.min_y.div_euclid(16);
        let si = y.div_euclid(16) - base_si;
        if si < 0 || (si as usize) >= dims.section_count() {
            return None;
        }
        let section = handle.section_at(chunk, si as usize)?;
        let biome = section.biome_at_block(
            x.rem_euclid(16) as usize,
            y.rem_euclid(16) as usize,
            z.rem_euclid(16) as usize,
        );
        let climate = self
            .biome_climates
            .as_ref()?
            .get(usize::try_from(biome).ok()?)?;
        // `worldgen::SEA_LEVEL` (63), not a second `63` constant — see the
        // #25 report's own note to grep for one before adding a duplicate.
        let temperature = lodestone_render::weather::height_adjusted_temperature(
            climate.temperature?,
            y,
            crate::worldgen::SEA_LEVEL,
        );
        Some(lodestone_render::weather::precipitation_for_temperature(
            climate.has_precipitation?,
            temperature,
        ))
    }
}

impl lodestone_render::WeatherProbe for ShellWeatherProbe {
    fn column_top(&self, _x: i32, _z: i32) -> Option<i32> {
        None
    }

    fn precipitation(&self, x: i32, y: i32, z: i32) -> lodestone_render::Precipitation {
        if !self.sky_visible {
            return lodestone_render::Precipitation::None;
        }
        // Issue #25: the biome climate lane now reaches the client
        // (`ClientEvent::BiomeClimates`, decoded and folded via
        // `net::BiomeClimateCell`), so this resolves a real per-column
        // answer instead of hardcoding `Rain`. Every unresolved hop still
        // falls back to `Rain` — matching `sky_visible`'s own "absent data
        // reads as open sky" rule: an unlit fallback here would make the
        // first rainy frame after joining silently show nothing.
        self.biome_precipitation(x, y, z)
            .unwrap_or(lodestone_render::Precipitation::Rain)
    }

    fn light(&self, _x: i32, _y: i32, _z: i32) -> f32 {
        self.light
    }
}

/// This frame's precipitation quads and the rain/snow split point, ready for
/// [`crate::gpu::RenderState::prepare_weather`].
///
/// A free function, not a `WindowApp` method: `redraw` holds a live `&mut` borrow
/// of `self.render` across the call site, so any `&self` method would be a second
/// borrow of the same struct.
///
/// The two returned values travel together on purpose. `extract_columns` sorts
/// rain-first so the pass can bind two textures over one buffer, and the count is
/// only meaningful against *that* ordering — a count taken from a differently
/// sorted list textures snow as rain with no error anywhere.
fn weather_columns_for_frame(
    weather: &lodestone_render::WeatherState,
    camera: &lodestone_render::Camera,
    tick: u64,
    probe: &dyn lodestone_render::WeatherProbe,
) -> (Vec<lodestone_render::WeatherInstance>, usize) {
    let camera_pos = [
        f64::from(camera.position.x),
        f64::from(camera.position.y),
        f64::from(camera.position.z),
    ];
    // The animation phase is driven by the **tick** clock, not by frame time.
    // `rain_column`'s scroll is `-(ticks + offset + partial) / 32 * speed`, so
    // feeding it a frame counter makes the fall speed frame-rate dependent — the
    // defect `entities.rs`'s
    // `limb_swing_tracks_per_tick_travel_not_the_interpolation_gap` records for the
    // walk cycle. `partial_ticks` is 0.0 rather than the real sub-tick alpha: at
    // 3-4 texture tiles per tick the sub-tick smoothing is below one texel, and
    // `Sim` exposes no partial tick to this layer.
    let columns = lodestone_render::extract_columns(
        weather,
        lodestone_render::DEFAULT_WEATHER_RADIUS,
        tick as i64,
        0.0,
        camera_pos,
        probe,
    );
    let rain = lodestone_render::rain_count(&columns);
    let offsets = lodestone_render::column_offset_table();
    let instances = columns
        .iter()
        .map(|c| {
            lodestone_render::column_instance(
                c,
                camera_pos,
                &offsets,
                lodestone_render::DEFAULT_WEATHER_RADIUS,
                weather.rain_level(),
            )
        })
        .collect();
    (instances, rain)
}

/// Persisted recipe-book panel UI state (issue #163) — see
/// [`WindowApp::recipe_panel`].
///
/// `tab` is an index into [`crate::container::RecipeBookPanelLayout::tabs`],
/// which is [`lodestone_game::recipe::RecipeBook::visible_tabs`]'s own order;
/// `None` is the all-categories view. `page` is clamped by
/// [`crate::container::recipe_book_panel_contents`] on read, so a stale page
/// left over from a wider search degrades to the last real page rather than
/// showing an empty grid.
#[derive(Debug, Default, Clone)]
struct RecipePanelState {
    /// Whether the panel body is open. The toggle button draws either way.
    open: bool,
    /// Current search text (substring match on the result id — see
    /// `RecipeBook::browse`).
    search: String,
    /// Selected category tab, or `None` for all categories.
    tab: Option<usize>,
    /// Current page within the filtered result set.
    page: usize,
    /// Whether the search box has keyboard focus, so typing edits
    /// [`Self::search`] instead of reaching the container's own key handling.
    ///
    /// Vanilla focuses its `EditBox` the same way (a click inside it), and this
    /// flag is what stops `search` being a field nothing ever writes — an
    /// island one layer down.
    search_focused: bool,
}

/// Wall-clock milliseconds for the recipe-toast window.
///
/// [`lodestone_game::recipe::RecipeToastQueue`] takes "now" from its caller and
/// only ever compares two of these against each other, so any clock with
/// millisecond resolution works. The epoch clock is used because that is what
/// vanilla's own toast timing is keyed off (`System.currentTimeMillis()`, see
/// `RECIPE_TOAST_DISPLAY_MS`'s doc) — so whoever wires
/// `RecipeToastQueue::push` from the decode reaches for the same function
/// rather than inventing a second, incompatible origin.
fn recipe_toast_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// This frame's recipe-unlock toast, if one should be on screen at `now_ms`.
///
/// `now_ms` is injected rather than read here so this is a pure function of the
/// queue plus a timestamp, which is what lets a test drive the toast at an exact
/// point in its 5000ms window without a sleep.
///
/// `visible_portion` is fixed at `1.0` — fully on screen. Vanilla's 600ms slide
/// (`ToastManager.java:229-232`) needs an animation origin, and
/// [`lodestone_game::recipe::RecipeToastQueue`] exposes none (its
/// `last_changed_ms` is private, and it has no notion of a visibility
/// transition). Drawing at rest is the honest subset; whoever lands the decode
/// and gives the queue a real producer is the right person to add the slide, and
/// [`crate::hud::RecipeToastView::visible_portion`] already takes it.
///
/// A free function over the queue rather than a `&self` method: `redraw` holds a
/// `&mut` borrow of `self.render` across the whole frame, so anything taking
/// `&self` there fails the borrow check. Taking the one field it reads keeps the
/// borrows disjoint — and makes it directly unit-testable against a queue with
/// no `WindowApp` in sight.
fn recipe_toast_view(
    queue: &lodestone_game::recipe::RecipeToastQueue,
    now_ms: u64,
) -> Option<crate::hud::RecipeToastView> {
    if !queue.visible(now_ms) {
        return None;
    }
    let (station, unlocked) = queue.displayed_entry(now_ms)?;
    Some(crate::hud::RecipeToastView {
        station: toast_icon(station)?,
        unlocked: toast_icon(unlocked)?,
        visible_portion: 1.0,
    })
}

/// The recipe-book panel's own layout, derived from the *same* state and scale
/// the draw uses.
///
/// Shared by the hit-test and draw paths on purpose: `container.rs`'s own
/// `hit_test_with_scale` carries a warning that a layout built with a different
/// `gui_scale` than the frame was drawn with silently mis-resolves every click,
/// and one function used twice is the only way to guarantee they agree.
fn recipe_panel_layout(
    panel: &RecipePanelState,
    menu: &Menu,
    gui_scale: u32,
    w: u32,
    h: u32,
    tab_count: usize,
    total_pages: usize,
) -> crate::container::RecipeBookPanelLayout {
    crate::container::recipe_book_panel_layout_with_scale(
        menu,
        gui_scale,
        w,
        h,
        tab_count,
        panel.page > 0,
        panel.page + 1 < total_pages,
    )
}

/// The panel's contents for one frame as `(tab_count, total_pages, page_ids)`,
/// with the ids **owned** so the borrow of `book` ends before a caller mutates
/// its own panel state.
///
/// Degrades to "no tabs, one empty page" with no corpus loaded (jar-less run),
/// which draws an empty-but-present panel rather than hiding the toggle.
fn recipe_panel_contents(
    book: Option<&RecipeBook>,
    panel: &RecipePanelState,
    book_type: lodestone_model::RecipeBookType,
) -> (usize, usize, Vec<lodestone_model::Identifier>) {
    let Some(book) = book else {
        return (0, 1, Vec::new());
    };
    let contents = crate::container::recipe_book_panel_contents(
        book,
        book_type,
        panel.tab,
        &panel.search,
        panel.page,
    );
    (
        contents.tabs.len(),
        contents.total_pages,
        contents.page_ids.into_iter().cloned().collect(),
    )
}

/// Build one frame of recipe-book panel geometry, or `None` when `menu` has no
/// recipe book at all (a chest, an anvil) and the panel is suppressed.
///
/// `items`/`models` are the atlases the icons resolve against; both absent is
/// the jar-less path, which falls back to
/// [`crate::container::recipe_book_panel_geometry`]'s hash-derived colour
/// swatches — the same degradation every other icon in this shell uses, and what
/// lets a headless gate exercise this at all.
///
/// Free rather than a method for the same borrow reason as
/// [`recipe_toast_view`].
#[allow(clippy::too_many_arguments)]
fn recipe_panel_geometry(
    book: Option<&RecipeBook>,
    panel: &RecipePanelState,
    menu: &Menu,
    gui_scale: u32,
    items: Option<&lodestone_assets::ItemAtlas>,
    models: Option<&lodestone_render::BlockModels>,
    w: u32,
    h: u32,
) -> Option<crate::container::RecipeBookPanelGeometry> {
    let book_type = recipe_book_type_for(menu)?;
    let (tab_count, total_pages, results) = match book {
        Some(book) => {
            let contents = crate::container::recipe_book_panel_contents(
                book,
                book_type,
                panel.tab,
                &panel.search,
                panel.page,
            );
            // `map_while`, not `filter_map`: `page_results[i]` must line up with
            // `layout.recipes[i]`, so a recipe with no result stack has to *end*
            // the slice rather than shift every later icon one cell left.
            // Truncating is the documented "fewer entries than populated cells
            // draws only what is given" behaviour.
            let results: Vec<&lodestone_game::item::ItemStack> = contents
                .page_ids
                .iter()
                .map_while(|id| {
                    book.get(id)
                        .and_then(lodestone_game::recipe::Recipe::result_stack)
                })
                .collect();
            (contents.tabs.len(), contents.total_pages, results)
        }
        None => (0, 1, Vec::new()),
    };
    let layout = recipe_panel_layout(panel, menu, gui_scale, w, h, tab_count, total_pages);
    Some(match items {
        Some(items) => crate::container::recipe_book_panel_geometry_with_icons(
            &layout,
            panel.open,
            panel.tab,
            &results,
            gui_scale,
            w,
            h,
            items,
            models,
        ),
        None => crate::container::recipe_book_panel_geometry(
            &layout,
            panel.open,
            panel.tab,
            &results,
            gui_scale,
            w,
            h,
        ),
    })
}

/// One toast icon: a single-item [`HotbarSlot`] for `id`.
///
/// `None` for an id the [`ResourceLocation`] parser rejects, which suppresses
/// the whole toast rather than drawing half of one.
fn toast_icon(id: &lodestone_model::Identifier) -> Option<HotbarSlot> {
    Some(HotbarSlot {
        item: ResourceLocation::parse(&id.to_string()).ok()?,
        count: 1,
        damage: None,
        max_damage: None,
        enchanted: false,
    })
}

/// Turn an auto-fill plan into the container clicks that realise it.
///
/// # Why this is not "two clicks per step"
///
/// [`lodestone_game::recipe::plan_auto_fill`] emits **one step per grid cell**,
/// each moving a *single* item, and several steps can name the same
/// `source_slot` (one stack of coal supplying three cells). The obvious
/// "pick up from `source_slot`, place into `cell`" pair does not express that,
/// because [`Click::left`] on a slot places the **whole** carried stack
/// (`click.rs`: "pick up whole / place whole") — so a 5-coal stack would land
/// entirely in the first cell and every later cell would be empty.
///
/// The sequence that actually produces one item per cell is vanilla's own
/// manual gesture, grouped by source:
///
/// 1. [`Click::left`] the source slot — pick the whole stack onto the cursor;
/// 2. [`Click::right`] each cell that source supplies — "place one" each;
/// 3. [`Click::left`] the source slot again — return the remainder.
///
/// Step 3 is a no-op when the source was exhausted exactly (left-clicking an
/// empty slot with an empty cursor does nothing), so it needs no guard.
///
/// Grouping is by **first appearance** of each `source_slot`, not by adjacency:
/// steps are ordered by grid cell, so one source's cells need not be
/// consecutive.
fn auto_fill_clicks(steps: &[lodestone_game::recipe::PlacementStep]) -> Vec<Click> {
    let mut clicks = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    for step in steps {
        if seen.contains(&step.source_slot) {
            continue;
        }
        seen.push(step.source_slot);
        clicks.push(Click::left(step.source_slot));
        for cell in steps
            .iter()
            .filter(|s| s.source_slot == step.source_slot)
            .map(|s| s.cell)
        {
            clicks.push(Click::right(cell));
        }
        clicks.push(Click::left(step.source_slot));
    }
    clicks
}

/// Which recipe book, if any, a menu shows — the same fork
/// [`lodestone_game::menu::Menu::plan_recipe_auto_fill`] makes internally, kept
/// in one place so the panel's *contents* and its *auto-fill* can never
/// disagree about which book they are in.
///
/// `None` means this menu has no recipe book at all (a chest, an anvil), and
/// the panel is suppressed entirely rather than drawing an empty one.
fn recipe_book_type_for(menu: &Menu) -> Option<lodestone_model::RecipeBookType> {
    use lodestone_game::menu::SpecialLayout;
    use lodestone_model::RecipeBookType;
    if menu.craft_layout().is_some() {
        return Some(RecipeBookType::Crafting);
    }
    match menu.special_layout()? {
        SpecialLayout::Furnace => Some(RecipeBookType::Furnace),
        SpecialLayout::BlastFurnace => Some(RecipeBookType::BlastFurnace),
        SpecialLayout::Smoker => Some(RecipeBookType::Smoker),
        _ => None,
    }
}

struct WindowApp {
    config: Config,
    sim: Sim,
    window: Option<Arc<Window>>,
    gpu: Option<GpuContext>,
    target: Option<lodestone_render::SurfaceTarget<'static>>,
    render: Option<RenderState>,
    hud: Option<HudRenderer>,
    /// Self-contained overlay for active status effects (owns its own pipeline
    /// so it composites over the HUD without touching the HUD renderer).
    effects: Option<EffectsRenderer>,
    container: Option<ContainerRenderer>,
    grabbed: bool,
    /// Frame clock: clamps catch-up and throttles presentation when the window
    /// is unfocused or occluded. The sim ticks regardless.
    pacer: FramePacer,
    /// Playing ↔ paused screen state; owns cursor-grab intent and shutdown.
    ui: UiState,
    /// Menu selection, the add/edit form, and the saved server list.
    nav: MenuNav,
    /// Per-server status pings for the multiplayer list. Probes run on their own
    /// threads; `pump()` moves results into slots once per frame.
    statuses: StatusCache,
    /// Self-contained menu pipeline (own shader, own buffer, clears the frame).
    /// `None` until GPU bring-up.
    menu: Option<MenuRenderer>,
    /// Decoded favicon mosaics, so a server's PNG is inflated once rather than
    /// once per frame.
    favicons: crate::menu::render::FaviconCache,
    /// Last cursor position in physical pixels, for menu hit-testing. Physical
    /// because that is the space `SurfaceTarget::size` and the menu layout use;
    /// mixing in logical coordinates puts the hit rects at half scale on a
    /// Retina display.
    cursor: (f32, f32),
    /// F3 debug overlay visibility. **Starts off, as vanilla does** — press F3
    /// to bring it up.
    ///
    /// It used to start on, because §S4 treats it as the instrument rather than
    /// a feature. That reasoning still holds for the *content*, but it does not
    /// require the thing to be on screen by default, and a permanently-visible
    /// overlay is simply not what the game looks like. The stdout status line
    /// (`pos=… fps=… chunks=…`) is unaffected and remains the instrument that
    /// does not depend on anyone having pressed a key.
    show_debug: bool,
    /// Whether the player-list binding is currently held (shows the overlay).
    tab_held: bool,
    /// The rebindable action → input table (`docs/keybindings.md`), loaded from
    /// the persisted [`crate::config::Options`] at construction.
    ///
    /// Held here rather than reached for through [`MenuNav`] because this is the
    /// *consumer*: every input event reads it, and `Keybinds` is `Copy` so the
    /// read is a field access rather than a borrow that would fight the
    /// `&mut self` effect calls in `window_event`.
    ///
    /// **A Controls menu will need a writer**, and this field is the reason that
    /// is a small addition rather than a rewrite: `MenuNav` already owns the
    /// loaded `Options` and the path to persist them to, so the menu's rebind
    /// call belongs there (a `nav.rebind(action, binding)` that sets the field
    /// and calls the existing `persist_options`), and this field then becomes
    /// `*self.nav.keybinds()`, re-read once per frame or on change. Deliberately
    /// not done yet: `nav.rs` is a shared file and an accessor with no caller is
    /// the island pattern `CLAUDE.md` §1 warns about. See `docs/keybindings.md`
    /// for the exact patch.
    keybinds: Keybinds,
    /// Editable buffer for the chat prompt; only consumed while chat is open.
    chat_input: ChatInput,
    /// Press/drag/release state machine for the open container screen; see
    /// [`container::MenuInput`]. Drives every predicted click this app sends.
    menu_input: MenuInput,
    /// Whether either Shift key is currently held, tracked independently of
    /// `sim.input` (which only feeds movement `Action`s and is not read back).
    /// Container shift-clicks (`QuickMove`) need this even while the sim's own
    /// gameplay input is not being accepted.
    shift_held: bool,
    /// Whether either Control key is currently held, tracked the same way as
    /// [`Self::shift_held`] and for the same reason `resolve_key` needs it: to
    /// distinguish `key.drop`'s drop-one from drop-stack
    /// (`Screen.hasControlDown()`/`Minecraft.hasControlDown()`), which is a
    /// modifier read at drop time, not a `KeyMapping` of its own.
    ctrl_held: bool,
    /// Fractional carry for the hotbar mouse-wheel scroll (issue #203), so a
    /// `mouseWheelSensitivity` below 1.0 does not lose sub-notch scroll and
    /// above 1.0 can cross more than one slot per notch. Mirrors vanilla's
    /// `ScrollWheelHandler.accumulatedScrollY`: each event adds
    /// `dy * sensitivity`, [`accumulate_scroll`] truncates off the whole
    /// slots and keeps the remainder, and a direction reversal drops
    /// whatever was carried in the old direction rather than fighting it.
    /// Not persisted — vanilla's own accumulator does not survive a restart
    /// either, being a field on a `MouseHandler` that is rebuilt with the
    /// window.
    scroll_accum: f64,
    /// When the left button last pressed on the container screen, for
    /// [`DOUBLE_CLICK_WINDOW`]-based double-click detection.
    last_menu_click: Option<Instant>,
    fps_ema: f32,
    last_log: Instant,
    /// The fog settings last uploaded to the renderer, so submerged fog is
    /// re-uploaded only when it actually changes (the player crossing a
    /// water/lava surface) rather than every frame. Seeded to the sky fog set at
    /// render bring-up so the first frame above water is a no-op.
    applied_fog: Option<FogSettings>,
    /// This driver's own `bevy_ecs` `App` (`docs/bevy-migration.md` Stage 0),
    /// stepped once per frame in [`WindowApp::redraw`] via `Runner::Winit`
    /// (`app.update()` called directly, no internal timer — packet ingest
    /// must never gate on frame rate). Only `CorePlugin` is added, and nothing
    /// in the shell reads from it — `self.ecs.update()` is the only other
    /// reference to this field. It is a *separate* `World` from `Sim`'s own
    /// `EcsHandle`, which §4.1(c) made the one that `lodestone_client::SharedState`
    /// adopts; this field predates that unification and was not folded into
    /// it. See the two-`World`s note in `docs/bevy-migration.md`'s Stage 0
    /// report for how this scaffold started, and §4.1(c)'s report for which
    /// `World` actually won.
    ecs: lodestone_ecs::app::App,
    /// The local crafting-recipe corpus (`crate::resources::load_recipe_book`),
    /// loaded once at GPU bring-up. `None` on a jar-less run or before it has
    /// loaded. Used only for the container screen's ghost-preview draw and the
    /// debug-overlay counter — the crafting result slot itself is always the
    /// server's, never a local match (see `docs/crafting.md`).
    recipe_book: Option<RecipeBook>,
    /// Persisted recipe-book **panel** state (issue #163): whether the panel is
    /// open, and the search/tab/page the user last left it on.
    ///
    /// Persisted across frames *and* across container open/close, deliberately:
    /// vanilla's `RecipeBookComponent` state lives on the client's own
    /// `RecipeBook`, not on the screen, so reopening a crafting table keeps the
    /// book open with the same tab. Rebuilding it per frame would reset the
    /// search box on every mouse move.
    recipe_panel: RecipePanelState,
    /// The recipe-unlock toast queue (issue #163) —
    /// [`lodestone_game::recipe::RecipeToastQueue`], drained into
    /// [`crate::hud::HudFrame::recipe_toast`] each frame.
    ///
    /// **Has no live producer yet.** The only thing that can fill it is the
    /// `recipe_book_add` decode, which does not exist in
    /// `crates/protocol/v770` (tracked on #436), so on a real server this stays
    /// empty and no toast draws. That is the honest degradation, not a bug: the
    /// render path is wired and gated so the toast appears the moment decode
    /// lands. Deliberately **no** fake producer was added to light it up early.
    recipe_toasts: lodestone_game::recipe::RecipeToastQueue,
    /// The world's weather, resolved from the net thread's cell once per frame.
    /// `None` before a session exists; installed alongside the other render
    /// sources by [`WindowApp::install_session_render_sources`] and cleared with
    /// the session, so a fresh connect never inherits the last one's storm.
    /// The rain **ambience** is deliberately absent from this struct. Its cadence
    /// lives in [`lodestone_render::RainAmbience`] (vanilla's `rainSoundTime`,
    /// unit-tested), but it has **no producer**, because the only `ShellAudio` in
    /// the process is a private field on `Sim` with no public play method. Adding
    /// one `pub fn play_local_sound(&mut self, name: &str, category, pos, volume,
    /// pitch)` to `crate::sim::Sim` — forwarding to `self.audio` exactly as the
    /// `NetUpdate::Sound` arm at `sim.rs:4722` already does — is the whole
    /// remaining wiring. Recorded here rather than left as two dead fields, per
    /// `CLAUDE.md`'s island rule: an unused field reads as an oversight, a named
    /// blocker does not.
    weather: Option<Arc<WeatherTracker>>,
}

impl WindowApp {
    fn new(config: Config) -> Self {
        let sim = Sim::new(config.clone());
        // Matches the sky fog set at render bring-up, so the fog reconciliation's
        // first above-water frame is a no-op rather than a redundant upload.
        let applied_fog = Some(crate::sim::fog_for_render_distance(config.render_distance));
        let mut ecs = lodestone_ecs::app::App::new();
        ecs.add_plugins(lodestone_ecs::CorePlugin);
        Self {
            config,
            sim,
            window: None,
            gpu: None,
            target: None,
            render: None,
            hud: None,
            effects: None,
            container: None,
            grabbed: false,
            pacer: FramePacer::new(Instant::now()),
            ui: UiState::new(),
            nav: MenuNav::new(),
            statuses: StatusCache::new(),
            menu: None,
            favicons: crate::menu::render::FaviconCache::new(),
            cursor: (0.0, 0.0),
            show_debug: false,
            tab_held: false,
            // Read from `options.json` via the same loader the menu uses.
            // Missing, partial or corrupt is vanilla's defaults, never an error
            // — see `Keybinds::from_json_value`.
            keybinds: crate::config::Options::load().keybinds,
            chat_input: ChatInput::new(),
            menu_input: MenuInput::new(),
            shift_held: false,
            ctrl_held: false,
            scroll_accum: 0.0,
            last_menu_click: None,
            fps_ema: 0.0,
            last_log: Instant::now(),
            applied_fog,
            ecs,
            recipe_book: None,
            recipe_panel: RecipePanelState::default(),
            recipe_toasts: lodestone_game::recipe::RecipeToastQueue::new(),
            // No session yet, so no weather cell to read; see
            // `install_session_render_sources`.
            weather: None,
        }
    }

    fn set_grab(&mut self, grabbed: bool) {
        let Some(window) = &self.window else { return };
        if grabbed {
            let locked = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
            if locked.is_ok() {
                window.set_cursor_visible(false);
                self.grabbed = true;
            }
        } else {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            window.set_cursor_visible(true);
            self.grabbed = false;
            self.sim.input_mut(InputState::release_all);
            // Releasing the pointer also ends any held dig, so mining does not
            // continue while the player is in a menu or the window is unfocused.
            self.sim.end_attack();
        }
    }

    /// Reconcile the menu state machine with the session's real phase, then keep
    /// the cursor grab in sync with whatever screen we ended up on. Called each
    /// frame so the loading screen is never a lie: it clears the moment the
    /// server logs us in, and flips to Error the moment the session ends.
    fn drive_ui_from_session(&mut self) {
        use crate::sim::SessionPhase;
        match self.sim.session_phase() {
            // LocalOnly never drives the menu — the dev world is already Playing.
            SessionPhase::LocalOnly | SessionPhase::Connecting => {}
            SessionPhase::Connected => self.ui.session_ready(),
            SessionPhase::Ended(reason) => {
                // Only transition in once; re-setting every frame would keep
                // re-latching the same reason (harmless but wasteful).
                if self.ui.screen() != crate::menu::Screen::Error {
                    self.ui.session_failed(reason.clone());
                }
            }
        }
        // The death screen (issue #103): `net::run` now builds the client
        // with `RespawnPolicy::Manual`, so nothing auto-respawns any more —
        // `Sim::is_dead` is the ground truth for whether the screen should be
        // up, reconciled here the same way `SessionPhase` is reconciled into
        // `UiState` above. The `!self.ui.is_death()` guard makes `die` fire
        // exactly once per death rather than re-latching (and re-cloning) the
        // message every frame the screen stays up; the `respawn_confirmed`
        // side needs no such guard — it is already a no-op off `Screen::Death`.
        if self.sim.is_dead() {
            if !self.ui.is_death() {
                self.ui.die(self.sim.death_message().map(str::to_string));
            }
        } else if self.ui.is_death() {
            self.ui.respawn_confirmed();
        }
        // The credits screen (issue #192): `Sim::has_won()` is the ground
        // truth `NetUpdate::WinGame` sets in `poll_net`, reconciled here the
        // same way `is_dead()` is reconciled above. The `!= Screen::Credits`
        // guard mirrors the `!self.ui.is_death()` one: `show_credits` is
        // already idempotent (it only moves the screen from a live-gameplay
        // screen), but this avoids re-latching every frame the screen stays
        // up. No "un-won" transition is needed on the other side — unlike
        // death, winning has no server-confirmed reversal to reconcile
        // against, and `Sim::end_session` clears the flag for the next
        // session.
        if self.sim.has_won() && self.ui.screen() != crate::menu::Screen::Credits {
            self.ui.show_credits();
        }
        // A transition may have changed grab intent (Connected → Playing grabs;
        // Ended/Death → menu-owned screens release). Only touch the OS grab
        // when it disagrees.
        let want = self.ui.wants_cursor_grab();
        if want != self.grabbed {
            self.set_grab(want);
        }

        // Issue #189: keep the Social Interactions roster live.
        // `social::entries_from_tablist` was pure and tested with **no
        // production caller** — this is the queued call
        // `docs/social-interactions.md`'s "How to change it" names. Only
        // `Screen::Social` ever reads `MenuNav::social()`, but this runs every
        // frame regardless of which screen is open (matching every other
        // reconciliation in this function) rather than gating on the screen:
        // a `TabList` clone plus a short `Vec` build is cheap, and refreshing
        // only-while-open would mean the roster the player sees the instant
        // they open it is one frame stale.
        if self.sim.session_phase() == crate::sim::SessionPhase::Connected {
            let tab_list = self.sim.tab_list();
            let entries =
                crate::menu::social::entries_from_tablist(&tab_list, self.sim.local_uuid());
            self.nav.refresh_social(entries);
        }
    }

    /// Staged Singleplayer entry point. Vanilla's singleplayer starts an
    /// integrated server in-process and connects to it over a local transport;
    /// that server (`impl-worldgen`'s `lodestone-server`, via a future
    /// `IntegratedServer::start`) is not wired yet. Rather than fork a second
    /// launch path or silently do nothing, this drives the honest failure path:
    /// the menu shows an Error explaining the feature is staged. Kept here so the
    /// wiring is a one-call swap once the seam lands.
    /// Install the block-outline source, which needs a live `Sim` — it reads the
    /// version adapter's per-state outline census through the shared handle.
    ///
    /// Must run *after* `attach_net`: `Sim::outline_shape_source` returns `None`
    /// without a net client. Until this is installed the selection box falls back
    /// to a unit cube, which is wrong for roughly nine block states in ten — only
    /// 3,328 of 32,366 have a full-cube outline.
    ///
    /// Note the outline census is deliberately *not* the collision census: they
    /// are different vanilla shape families and disagree for over half of all
    /// states, so a slab's box and a slab's collider are not the same box.
    fn install_outline_source(&mut self) {
        if let (Some(render), Some(f)) = (self.render.as_mut(), self.sim.outline_shape_source()) {
            render.set_outline_shape_source(f);
        }
    }

    /// Install the debug-lines source: the render half of `ExtractSet::Debug`
    /// (`docs/plugin-api.md`), the channel a plugin (e.g. a navigator) uses to
    /// push world-space line geometry onto screen via
    /// `lodestone_ecs::player::DebugLines`. `RenderState::set_debug_lines_source`
    /// and the line pipeline it drives already existed with no caller —
    /// `gpu.rs`'s own `DebugLinesSource` doc names this as "the one wire this
    /// crate cannot lay itself."
    ///
    /// Unlike [`install_outline_source`](Self::install_outline_source), this
    /// needs no live connection: `Sim::new`/`Sim::with_demo_world` always add
    /// `LocalPlayerPlugin` (`crates/lodestone-ecs/src/player.rs`), which
    /// `init_resource`s `DebugLines` on the one `World` regardless of session
    /// kind, so `self.sim.ecs()` is enough. Callable — and safe to call
    /// repeatedly, since it only replaces the closure with an equivalent one —
    /// the moment `self.render` exists.
    fn install_debug_lines_source(&mut self) {
        let Some(render) = self.render.as_mut() else {
            return;
        };
        let ecs = self.sim.ecs().clone();
        render.set_debug_lines_source(move || {
            lodestone_ecs::hold_read(&ecs, |world| {
                crate::gpu::debug_line_vertices(&world.resource::<lodestone_ecs::DebugLines>().0)
            })
        });
    }

    /// Start singleplayer and show the loading screen (issue #287).
    ///
    /// The multiplayer twin of this is [`Self::connect_to`], and after the
    /// session is attached the two are *the same function*: both call
    /// [`Self::install_session_render_sources`], because the sky, fog clock,
    /// entity light sampler and screen-effect passes are properties of having a
    /// session, not of how it was obtained. That sharing is the point — a
    /// singleplayer path with its own render wiring is how one of the two ends up
    /// silently missing a pass.
    ///
    /// `attach_net` rather than a `Sim::connect`-style helper because the client
    /// is already built *with* this `Sim`'s `World` and local entity: that is what
    /// [`launch_singleplayer`]'s `session` argument is, threaded through
    /// `NetClient::open_singleplayer` into `ClientBuilder::ecs` (§4.1(c)).
    /// Attaching without it is the silent failure `Sim::connect`'s docs warn
    /// about — every HUD accessor would read an empty default.
    fn begin_singleplayer(&mut self, config: Option<crate::menu::create_world::WorldCreationConfig>) {
        self.ui.begin(crate::menu::SessionKind::Singleplayer);
        let seed = resolve_launch_seed(config.as_ref());
        let session = Some((self.sim.ecs().clone(), self.sim.local_player()));
        // Vanilla streams `simulationDistance`/`viewDistance` chunks around the
        // player; ours is the same number the camera's far plane and the mesher
        // already use, so the server never sends a column the renderer would
        // discard and never withholds one it wants.
        //
        // **Plus one, and the `+ 1` is not slack — it is the buffer ring the
        // mesher's invariant requires.** Vanilla's own server tracks
        // `center + viewDistance + 1` (`ChunkTrackingView.java:92, 96`), and it has
        // to: a section is only meshed once all its neighbours are resident, so the
        // outermost ring of a radius-`n` stream permanently lacks a neighbour and
        // **never draws**. Streaming exactly `render_distance` made singleplayer
        // silently lose its last ring of chunks — reported as "some water far away
        // is blocky", because a large flat surface is where a missing outer ring
        // reads as a hard step rather than as absent scenery.
        //
        // This does not widen the view: fog and the far plane read
        // `config.render_distance` directly, not this value.
        let view_radius = i32::try_from(self.config.render_distance)
            .unwrap_or(i32::MAX)
            .saturating_add(1);
        match launch_singleplayer(self.config.protocol, view_radius, session, seed) {
            Ok(net) => {
                self.sim.attach_net(net);
                self.install_session_render_sources();
            }
            // Reported, never routed around: the only cause is a build with no
            // hostable version family, and telling the player that is strictly
            // better than a world that silently never loads.
            Err(e) => self.ui.session_failed(e.to_string()),
        }
    }

    /// Open a live connection to `host:port` and show the loading screen.
    ///
    /// Factored out of `resumed` because the menu's Join button needs the exact
    /// same sequence, including the entity light sampler — which must be
    /// installed at connect time, not after login (see the long note at the
    /// `resumed` call site for why).
    fn connect_to(&mut self, host: String, port: u16) {
        // §4.1(c): `Sim::connect` builds the client *with* the shell's one `World`
        // and attaches it, so the render sources below are installed from the
        // already-attached client's shared handle rather than from a `NetClient`
        // this function still owns. `shared_handle` survives the move either way
        // (it is an `Arc<OnceLock<_>>` the net thread publishes into).
        self.sim.connect(host, port, self.config.protocol);
        self.install_session_render_sources();
    }

    /// Install every render source a live session feeds, for **either** session
    /// kind: the fog/sky clock, the entity light sampler, the sky pass and the
    /// screen-effect overlays, plus the outline and debug-line sources.
    ///
    /// Shared by [`Self::connect_to`] and [`Self::begin_singleplayer`] (issue
    /// #287) rather than duplicated, because a source installed for one session
    /// kind and not the other is invisible until someone plays the other one —
    /// and the two differ *only* in transport (see `net.rs`'s `Origin`). A no-op
    /// when there is no session or no GPU yet, so it is safe to call from either
    /// path unconditionally.
    fn install_session_render_sources(&mut self) {
        // `sky_clock.get().map(|h| h.world_time().1)` used to be handed to
        // `set_time_of_day_source` directly. `WorldTime` is a flat snapshot the
        // network thread only overwrites on a decoded `SET_TIME`
        // (`ClientEvent::TimeChanged` — `lodestone-client/src/state.rs`), and the
        // server sends that roughly once per second
        // (`docs/served-session-liveness.md`'s `TIME_SYNC_INTERVAL`), so the raw
        // value steps once/sec instead of advancing per frame. That produced the
        // reported once-a-second cloud "teleport" (`sky.rs::cloud_plane_geometry`'s
        // `scroll_x` is `time_of_day * CLOUD_SCROLL_BLOCKS_PER_TICK`, so a
        // once/sec step is a visible ~0.6-block jump).
        //
        // `ContinuousTimeOfDay::advance` wraps the same raw value with a local,
        // wall-clock extrapolation between packets — the same trick vanilla's own
        // client-side day-time prediction uses, and it keeps `sky.rs` itself
        // clock-agnostic per its own module docs ("there is deliberately no
        // second clock... anywhere in this module"): the extrapolation lives here,
        // at the render-source boundary, not inside the sky module.
        //
        // The handle comes from the already-attached client rather than from a
        // `NetClient` a caller still owns; `shared_handle` survives the move
        // either way (it is an `Arc<OnceLock<_>>` the net thread publishes into).
        let Some(net_handle) = self.sim.net().map(crate::net::NetClient::shared_handle) else {
            return;
        };
        // The weather cell, cloned out for the same reason `shared_handle` is: the
        // `NetClient` is moved into `Sim::attach_net` and the closures below outlive
        // it. Re-created on every connect so a new session starts clear.
        let weather = self
            .sim
            .net()
            .map(|net| Arc::new(WeatherTracker::new(net.shared_weather())));
        self.weather = weather.clone();
        // The dimension's absent-sky-light policy, cloned out for the same reason as
        // the two above. The entity-light closure is installed **once** and must
        // still be right after a portal, so it reads the policy per call from this
        // cell rather than capturing today's value — `Sim::refresh_mesh_policy`
        // publishes into it. See `net::SkyDefaultCell`.
        let sky_policy = self
            .sim
            .net()
            .map(crate::net::NetClient::shared_sky_default);
        if let Some(render) = self.render.as_mut() {
            let handle = net_handle.clone();
            let light_policy = sky_policy.clone();
            // Terrain and mobs must read the same clock: `RenderState` folds this
            // factor into the fog lane both the model and entity passes sample.
            // Installing it for one and not the other makes mobs darker than the
            // blocks they stand on at midnight.
            let clock = net_handle.clone();
            // The sky pass's own clock — see `set_time_of_day_source`'s doc for
            // why it needs the raw tick rather than `set_sky_darken_source`'s
            // already-derived factor.
            let sky_clock = net_handle;
            let continuous_time_of_day = ContinuousTimeOfDay::new();
            // Weather rides *this* lane rather than getting one of its own.
            // `EnvironmentAttributes.SKY_LIGHT_FACTOR` is a single attribute in
            // vanilla too: the time-of-day curve is its base and
            // `WeatherAttributes`' two layers modify it
            // (`WeatherAttributes.java:19`, `:30`), so a separate uniform would be
            // a second writer of one value and the two would drift. This is the
            // exact `sky_darken` `lodestone_render::light`'s module doc derives,
            // and terrain, mobs and the first-person arm all read it through the
            // same fog lane — so one line here darkens all three under a storm.
            let darken_weather = weather.clone();
            render.set_sky_darken_source(move || {
                let base = clock.get().map(|h| {
                    lodestone_render::entity::sky_darken_for_time_of_day(h.world_time().1)
                })?;
                Some(match &darken_weather {
                    Some(w) => lodestone_render::weather_sky_light_factor(base, &w.state()),
                    None => base,
                })
            });
            render.set_entity_light_source(move |feet| {
                crate::net::entity_light_at(
                    &handle,
                    feet.x.floor() as i32,
                    feet.y.floor() as i32,
                    feet.z.floor() as i32,
                    // Read per call, not captured: a portal changes this mid-session.
                    light_policy.as_ref().map_or(
                        lodestone_render::SkyDefault::Full,
                        |cell| cell.get(),
                    ),
                )
            });
            render.set_time_of_day_source(move || {
                sky_clock
                    .get()
                    .map(|h| continuous_time_of_day.advance(h.world_time().1))
            });
        }
        // The sky pass itself needs GPU handles `RenderState::set_*_source`'s
        // closures don't (it uploads the celestial atlas + cloud texture
        // immediately, via `crate::resources::load_sky`), so it is installed
        // from a separate `self.gpu`/`self.target` borrow rather than folded
        // into the block above. `has_sky` guards a re-connect from re-loading
        // and re-uploading the same jar's textures a second time.
        if let (Some(gpu), Some(target)) = (self.gpu.as_ref(), self.target.as_ref()) {
            let (device, queue, format) = (gpu.device(), gpu.queue(), target.format());
            if let Some(render) = self.render.as_mut()
                && !render.has_sky()
                && let Some(sky) = crate::resources::load_sky(device, queue, format)
            {
                render.install_sky(sky);
            }
            // The underwater/fire overlay pass (issues #108, #112): same
            // shape and same reason as the sky install just above (needs GPU
            // handles immediately, so it is loaded here rather than folded
            // into a `set_*_source` closure). `has_screen_effects` guards a
            // re-connect the same way `has_sky` does.
            if let Some(render) = self.render.as_mut()
                && !render.has_screen_effects()
                && let Some(fx) = crate::resources::load_screen_effects(device, queue, format)
            {
                render.install_screen_effects(fx);
            }
            // The rain/snow pass: same shape and same `has_*` re-connect guard as
            // the two above. Note this is only the *droplets* — a jar-less run
            // still darkens correctly, because that half went in through
            // `set_sky_darken_source` and `set_fog` above.
            if let Some(render) = self.render.as_mut()
                && !render.has_weather()
                && let Some(textures) = crate::resources::load_weather_textures()
            {
                render.install_weather(device, queue, format, &textures);
            }
        }
        self.install_outline_source();
        self.install_debug_lines_source();
    }

    /// The menu currently drawn as the container screen — the open non-player
    /// menu if the server has one open, else the player inventory while `E`
    /// has it up — or `None` when no container UI is showing.
    ///
    /// Mirrors the `container_menu` selection `redraw` makes for drawing, so
    /// hit-testing and drawing never disagree about which menu is on screen
    /// (see the layout module's own warning about that class of bug).
    fn active_container_menu(&self) -> Option<Menu> {
        if let Some(open) = self.sim.open_menu() {
            Some(open.menu)
        } else if self.ui.is_container_open() {
            Some(self.sim.player_menu())
        } else {
            None
        }
    }

    /// Predicts a container click against the live client state and submits
    /// it to the server.
    ///
    /// This goes straight to [`lodestone_client::ClientHandle::menu_click`]
    /// rather than through `Sim`/`NetClient`'s `send_action` queue, and
    /// deliberately so: the prediction has to run inside the read-model the
    /// live `Menus` session lives in (see the doc comment on
    /// `ClientHandle::menu_click`), and `NetClient::send_action` only ever
    /// forwards an *already-built* [`lodestone_model::ClientAction`] — it has
    /// no menu to predict a click against. `NetClient::shared_handle()` is
    /// the existing, already-public seam onto that same live handle (used
    /// today for the sky-darken and entity-light samplers), so this needs no
    /// change to `net.rs` or `sim.rs`.
    ///
    /// Silently drops the click if there is no live connection yet (matches
    /// every other best-effort send in this app, e.g. `NetClient::send_action`
    /// itself).
    fn send_menu_click(&self, click: Click) {
        let Some(net) = self.sim.net() else { return };
        // Named separately from its `.get()` below rather than chained: the
        // `Arc<OnceLock<_>>` `shared_handle()` returns is an owned value, and
        // `.get()` borrows from it — keeping it in a binding of its own avoids
        // relying on let-else's temporary-scope-extension rules to keep that
        // borrow valid.
        let shared = net.shared_handle();
        let Some(handle) = shared.get() else { return };
        // `Sim` has no game-mode accessor to source a real `PlayerCtx` from
        // (see the report on this change) — hardcoded survival, matching the
        // only existing production-shaped precedent
        // (`container.rs`'s own click-driving tests use `PlayerCtx::survival()`
        // /`::creative()` explicitly rather than reading one off anything).
        let _ = handle.menu_click(click, PlayerCtx::survival());
    }

    /// Resolve a click at the current cursor against the recipe-book panel and
    /// act on it, returning whether the panel **consumed** the click.
    ///
    /// Called before the container's own `hit_test_with_scale` so the panel —
    /// which overlaps the main panel's left edge at narrow canvases, by
    /// `container.rs`'s documented design — wins over the slot underneath it.
    /// Returning `false` leaves the click to the normal slot path untouched.
    fn handle_recipe_panel_click(&mut self, menu: &Menu, w: u32, h: u32) -> bool {
        let Some(book_type) = recipe_book_type_for(menu) else {
            return false;
        };
        let (tab_count, total_pages, page_ids) =
            recipe_panel_contents(self.recipe_book.as_ref(), &self.recipe_panel, book_type);
        let layout = recipe_panel_layout(
            &self.recipe_panel,
            menu,
            self.nav.gui_scale(),
            w,
            h,
            tab_count,
            total_pages,
        );
        let Some(hit) = crate::container::recipe_book_panel_hit_test_with_scale(
            &layout,
            self.recipe_panel.open,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
        ) else {
            return false;
        };

        use crate::container::RecipeBookPanelHit as Hit;
        match hit {
            Hit::Toggle => {
                self.recipe_panel.open = !self.recipe_panel.open;
                self.recipe_panel.search_focused = false;
            }
            Hit::SearchBox => self.recipe_panel.search_focused = true,
            Hit::Tab(i) => {
                // Clicking the selected tab again clears the filter, so there is
                // always a way back to all categories without a dedicated
                // "all" tab (this client's tab list has none — see
                // `recipe_book_panel_contents`).
                self.recipe_panel.tab = if self.recipe_panel.tab == Some(i) {
                    None
                } else {
                    Some(i)
                };
                self.recipe_panel.page = 0;
                self.recipe_panel.search_focused = false;
            }
            Hit::PageForward => {
                if self.recipe_panel.page + 1 < total_pages {
                    self.recipe_panel.page += 1;
                }
                self.recipe_panel.search_focused = false;
            }
            Hit::PageBack => {
                self.recipe_panel.page = self.recipe_panel.page.saturating_sub(1);
                self.recipe_panel.search_focused = false;
            }
            Hit::Recipe(i) => {
                self.recipe_panel.search_focused = false;
                // A cell can be empty on a short final page — `page_ids` is the
                // authority on which of the 20 fixed cells is populated, exactly
                // as `RecipeBookPanelHit::Recipe`'s own doc requires.
                if let Some(id) = page_ids.get(i).cloned() {
                    self.auto_fill_recipe(menu, &id);
                }
            }
            // A click on the panel body or the unimplemented All/Craftable
            // filter is still *consumed*, so it does not fall through and
            // click the container slot behind the panel.
            Hit::FilterButton | Hit::Panel => self.recipe_panel.search_focused = false,
        }
        true
    }

    /// Auto-fill the crafting grid for `id` (issue #163's "click a recipe to
    /// fill the grid").
    ///
    /// Every click goes out through [`Self::send_menu_click`], i.e. the **same**
    /// per-click predict-then-send path a manual `MenuInput::press`/`release`
    /// takes. That is deliberate and load-bearing: a second dispatch path would
    /// diverge from `container.rs`'s vanilla-exact click semantics, and the
    /// prediction has to see each click in order for the next one's `ctx` to be
    /// right.
    fn auto_fill_recipe(&self, menu: &Menu, id: &lodestone_model::Identifier) {
        let Some(book) = self.recipe_book.as_ref() else {
            return;
        };
        let Some(recipe) = book.get(id) else { return };
        let Some(steps) = menu.plan_recipe_auto_fill(recipe, book.tags()) else {
            return;
        };
        for click in auto_fill_clicks(&steps) {
            self.send_menu_click(click);
        }
    }

    /// A number-key / off-hand-key `SWAP` against the slot under the cursor
    /// (issue #378 part 3).
    ///
    /// Vanilla's `AbstractContainerScreen.checkHotbarKeyPressed`
    /// (`AbstractContainerScreen.java:506-522`) guards on exactly two pieces of
    /// **state**: `menu.getCarried().isEmpty()` and `hoveredSlot != null`. Both
    /// are checked here rather than in `resolve_key`, which only knows about keys.
    /// Failing either does nothing — the same thing an open container did with
    /// these keys before this landed, so a miss is not a new dead end.
    ///
    /// The hover is resolved through the identical
    /// `active_container_menu` + `hit_test_with_scale` pair the mouse path uses,
    /// so the key and the mouse can never disagree about which slot is under the
    /// pointer (the layout module's own warning about that class of bug).
    fn send_container_swap(&self, button: i32) {
        let (Some(menu), Some((w, h))) = (
            self.active_container_menu(),
            self.target.as_ref().map(RenderTarget::size),
        ) else {
            return;
        };
        // An occupied cursor is vanilla's first guard, and it is not arbitrary: a
        // swap with something already in hand has no defined meaning, so vanilla
        // lets the key fall through to nothing.
        if menu.carried().is_some() {
            return;
        }
        let hit = hit_test_with_scale(&menu, self.nav.gui_scale(), w, h, self.cursor.0, self.cursor.1);
        let MenuHit::Slot(index) = hit else { return };
        // Vanilla's `40` is the off-hand button and `do_swap`'s `button == 40` arm
        // handles it. Since #382 freed `F` the off-hand binding does reach here;
        // note this is the **container** route only — the no-screen route is
        // `send_offhand_swap` below, a different packet entirely (#385).
        let click = if button == OFFHAND_SWAP_BUTTON {
            Click::offhand_swap(index)
        } else if let Ok(hotbar) = u8::try_from(button) {
            Click::hotbar_swap(index, hotbar)
        } else {
            return;
        };
        self.send_menu_click(click);
    }

    /// `key.drop` pressed with a container screen open (the container half of
    /// the drop-key island pair).
    ///
    /// Goes through [`MenuInput::key_pressed`] rather than building the
    /// `Click` directly the way [`Self::send_container_swap`] does, because
    /// `key_pressed` already carries vanilla's `hoveredSlot.hasItem()` guard
    /// (`AbstractContainerScreen.java:495`) and the `PickItem`/`Drop`
    /// `else if` — duplicating either here would be a second copy that can
    /// drift from the one `container.rs` already tests. `Click::drop_one`/
    /// `drop_stack` and `do_throw` (`lodestone-game`) were built and tested
    /// under #27 with zero producers before this; this is the first caller.
    fn send_container_drop(&self, ctrl: bool) {
        let (Some(menu), Some((w, h))) = (
            self.active_container_menu(),
            self.target.as_ref().map(RenderTarget::size),
        ) else {
            return;
        };
        let hit = hit_test_with_scale(&menu, self.nav.gui_scale(), w, h, self.cursor.0, self.cursor.1);
        let ctx = MenuContext {
            cursor_loaded: menu.carried().is_some(),
            // Same gap `send_container_swap`'s own click construction has: no
            // game-mode plumbing exists on `Sim` yet, and `key_pressed`'s
            // `Drop` arm does not read `ctx` regardless (see its doc comment).
            creative: false,
        };
        for click in self.menu_input.key_pressed(hit, ContainerMenuKey::Drop { ctrl }, ctx, &menu) {
            self.send_menu_click(click);
        }
    }

    /// `key.pickItem` pressed with a container screen open — `ClickType::CLONE`
    /// against the hovered slot (`AbstractContainerScreen.java:495-501`).
    ///
    /// Identical in shape to [`Self::send_container_drop`] except that there is
    /// no modifier variant to carry: vanilla's clone click has no `ctrl` form.
    /// The same `creative: false` gap applies — no game-mode plumbing exists on
    /// `Sim` yet, which matters more here than for drop, because vanilla's clone
    /// click is *creative-only*; until that lands this resolves and then produces
    /// no clicks, which is the honest degradation rather than a fabricated one.
    fn send_container_pick_item(&self) {
        let (Some(menu), Some((w, h))) = (
            self.active_container_menu(),
            self.target.as_ref().map(RenderTarget::size),
        ) else {
            return;
        };
        let hit = hit_test_with_scale(&menu, self.nav.gui_scale(), w, h, self.cursor.0, self.cursor.1);
        let ctx = MenuContext {
            cursor_loaded: menu.carried().is_some(),
            creative: false,
        };
        for click in self.menu_input.key_pressed(hit, ContainerMenuKey::PickItem, ctx, &menu) {
            self.send_menu_click(click);
        }
    }

    /// `key.drop` pressed in normal gameplay (no screen open) — the gameplay
    /// half of the drop-key island pair. `ClientAction::DropSelectedItem`/
    /// `DropSelectedItemStack` were encoded and round-trip tested with zero
    /// producers anywhere in `lodestone-shell` before this; this is the first
    /// caller. Thin by design, like [`Self::send_offhand_swap`]: everything
    /// decidable is in [`drop_selected_action`], testable without a window, a
    /// GPU or a live `Sim`.
    fn send_drop_selected(&self, ctrl: bool) {
        let Some(net) = self.sim.net() else { return };
        let game_mode = net
            .shared_handle()
            .get()
            .cloned()
            .and_then(|handle| handle.game_mode());
        if let Some(action) = drop_selected_action(game_mode, ctrl) {
            net.send_action(action);
        }
    }

    /// The off-hand key pressed in normal gameplay (issue #385).
    ///
    /// Thin by design: everything decidable is in [`offhand_swap_action`], which
    /// takes the game mode rather than reading it off `self`, so the whole
    /// decision is testable without a window, a GPU or a live `Sim`.
    fn send_offhand_swap(&self) {
        let Some(net) = self.sim.net() else { return };
        // Same read the fire/underwater overlay pass uses for `spectator`.
        let game_mode = net
            .shared_handle()
            .get()
            .cloned()
            .and_then(|handle| handle.game_mode());
        if let Some(action) = offhand_swap_action(game_mode) {
            net.send_action(action);
        }
    }

    /// Translate one winit key event into a [`MenuKey`], or `None` if the menu
    /// has no use for it.
    fn menu_key_for(event: &winit::event::KeyEvent) -> Option<MenuKey> {
        if let PhysicalKey::Code(code) = event.physical_key {
            match code {
                KeyCode::ArrowUp => return Some(MenuKey::Up),
                KeyCode::ArrowDown => return Some(MenuKey::Down),
                KeyCode::Enter | KeyCode::NumpadEnter => return Some(MenuKey::Enter),
                KeyCode::Escape => return Some(MenuKey::Escape),
                KeyCode::Tab => return Some(MenuKey::Tab),
                KeyCode::Backspace => return Some(MenuKey::Backspace),
                KeyCode::Delete => return Some(MenuKey::Delete),
                // F5 refreshes the multiplayer list (#396), which is
                // `JoinMultiplayerScreen.keyPressed`'s only key. It has to be here
                // rather than falling through to the text path below: a function
                // key has no `text`, so without this it would reach nothing.
                KeyCode::F5 => return Some(MenuKey::Refresh),
                _ => {}
            }
        }
        // Anything else is text. `KeyEvent::text` is already the composed
        // character, so this is the path that makes non-US layouts type
        // correctly into the address field.
        event
            .text
            .as_ref()
            .and_then(|t| t.chars().next())
            .filter(|c| !c.is_control())
            .map(MenuKey::Char)
    }

    /// Feed one menu key through the navigator and act on what it asks for.
    fn handle_menu_key(&mut self, key: MenuKey) {
        let action = self.nav.key(&mut self.ui, key);
        self.apply_menu_action(action);
    }

    /// Perform the one side effect a [`MenuAction`] names. Exhaustive on purpose:
    /// a new variant must fail to compile here rather than silently do nothing.
    fn apply_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::None => {}
            MenuAction::Singleplayer(config) => {
                // A real integrated server (#287), not the old offline demo
                // world. `Sim::new` no longer builds one (see its docs): a client
                // holds the server's world or none at all, and a demo world left
                // resident under a later multiplayer join is the two-worlds defect
                // this button used to be the entry point for. Singleplayer now
                // takes the *same* path a join does, so there is only ever one
                // world and it always came off the wire.
                //
                // `None` when `Screen::WorldSelect`'s Play Selected World produced
                // the action (no seed of its own, so this resolves to
                // `BUNDLED_WORLD.seed` via `resolve_launch_seed`); `Some(config)`
                // when `Screen::CreateWorld`'s Create button did (issue #190,
                // `menu/nav.rs`'s `apply_create_world`). `begin_singleplayer`,
                // `resolve_launch_seed` and `launch_singleplayer` handle both
                // uniformly (see this file's `resolved_seeds_from_different_world_
                // creation_configs_generate_different_terrain`).
                self.begin_singleplayer(config);
            }
            MenuAction::Connect(entry) => {
                self.connect_to(entry.host.clone(), entry.effective_port());
            }
            MenuAction::Quit => {}
            MenuAction::Reprobe(Some(entry)) => self.statuses.refresh_one(&entry),
            MenuAction::Reprobe(None) => {
                self.statuses.refresh(self.nav.list().entries());
            }
            // F5 or the Refresh button (#396). `refresh_all`, not `refresh`:
            // `refresh` skips any address it already has a result for, so it would
            // make the button do nothing at all.
            MenuAction::RefreshList => {
                let entries = self.nav.list().entries().to_vec();
                self.statuses.refresh_all(&entries);
            }
            MenuAction::Forget(entry) => {
                self.statuses.forget(&entry);
                // A delete or re-address changes the row set; probe whatever is
                // now in the list (idempotent, so this costs nothing per frame).
                self.statuses.refresh(self.nav.list().entries());
            }
            MenuAction::QuitToTitle => {
                // `UiState` has already moved to `MainMenu` — `nav.rs`'s
                // `key_paused` (and, issue #103, `key_death`) calls
                // `ui.quit_to_title()` before returning this action. What is
                // left is tearing down whatever live session is attached to
                // `Sim` so a fresh connect afterward starts clean; see
                // `Sim::end_session` for exactly what resets vs. persists.
                self.sim.end_session();
                // The pause/death screen already released the pointer on
                // entry, so this is normally a no-op; cheap insurance against
                // a future caller reaching `QuitToTitle` some other way.
                self.set_grab(false);
            }
            // The death screen's Respawn button (issue #103): submit the
            // manual `ClientAction::Respawn` — `Sim::respawn` is a no-op
            // unless `Sim::is_dead` is still true, so a stray/duplicate call
            // (e.g. a double-click before the server's confirmation lands)
            // costs nothing. `UiState` stays on `Screen::Death` until
            // `net::NetUpdate::Respawned` arrives; see `drive_ui_from_session`.
            MenuAction::Respawn => self.sim.respawn(),
            // The command-block screen's Done button (issue #47):
            // `populateAndSendPacket` (`CommandBlockEditScreen.java:96-114`).
            //
            // `into_action` is the one step `MenuAction`'s `Eq` derive cannot
            // cross — `ClientAction` holds a float in a sibling variant, so
            // `nav.rs` carries the `Eq`-able `CommandBlockSubmit` and rebuilds
            // the real action here. See `command_block::CommandBlockSubmit`.
            //
            // Goes out through `Sim`'s own `NetClient`, not a `Sim` method:
            // there is no `Sim::set_command_block` to add, and unlike
            // `Sim::respawn` there is no state-dependent guard to enforce (a
            // command block edit is unconditional — the server validates op
            // level). `Sim::net()` is `None` off a live session, so this is a
            // no-op in single-player-menu or pre-join states rather than a
            // panic.
            //
            // **This makes the screen submit; it does not make it reachable.**
            // Nothing opens `Screen::CommandBlock` from a real interaction yet —
            // no command-block block-entity NBT decode, no `interact.rs`
            // trigger. That is issue #442, deliberately not fixed here.
            MenuAction::SetCommandBlock(submit) => {
                if let Some(net) = self.sim.net() {
                    net.send_action(submit.into_action());
                }
            }
        }
    }

    /// Route a mouse position (physical pixels) to a menu row, if it is over one.
    ///
    /// `Screen::Paused` gets its frame from [`crate::menu::render::pause_frame`]
    /// directly rather than [`crate::menu::render::frame_for`], which returns
    /// `None` for it by design (see that function's doc on why the pause
    /// overlay is not an `owns_frame` screen).
    ///
    /// Both branches convert the physical framebuffer size and cursor down to
    /// the same logical canvas [`MenuRenderer::render`]/`render_overlay`
    /// actually draw into (via [`crate::menu::render::logical_canvas`]) before
    /// calling [`crate::menu::render::row_rect`] — mirroring
    /// `container::hit_test_with_scale`'s own `x / scale` pattern. Skipping
    /// this (as this function used to) is exactly the "clicks land one slot
    /// off, invisible in any screenshot" bug that module warns about: it is
    /// only invisible at `gui_scale == 1`, which is why it went unnoticed.
    fn menu_row_at(&mut self, x: f32, y: f32) -> Option<usize> {
        let frame = if self.ui.is_paused() {
            crate::menu::render::pause_frame(&self.nav)
        } else if self.ui.is_death() {
            // Same reasoning as the `is_paused()` branch above (issue #103):
            // `Screen::Death` gets its frame from `death_frame` directly, not
            // `frame_for`, which returns `None` for it by design (see
            // `owns_frame`'s doc on why the death screen is not one).
            crate::menu::render::death_frame(&self.nav, self.sim.death_message())
        } else {
            crate::menu::render::frame_for(
                &self.ui,
                &self.nav,
                &self.statuses,
                &mut self.favicons,
            )?
        };
        let (fb_w, fb_h) = self.target.as_ref().map(RenderTarget::size)?;
        let (w, h) = crate::menu::render::logical_canvas(frame.gui_scale, fb_w, fb_h);
        let scale = crate::config::calculate_gui_scale(frame.gui_scale, fb_w, fb_h).max(1) as f32;
        let (lx, ly) = (x / scale, y / scale);
        // Record the logical position as well as the row (#396). The multiplayer
        // list needs the position itself — which quadrant of a row's favicon the
        // cursor is in decides whether a click joins or reorders — and this is the
        // one place that has already converted physical pixels to the canvas the
        // draw uses, so recording it here covers hover *and* click with no new
        // plumbing at either site. Recorded before the hit-test, so a cursor over
        // the backdrop still updates it.
        self.nav.set_menu_cursor(lx, ly, w, h);
        (0..frame.rows.len()).find(|&i| {
            crate::menu::render::row_rect(&frame.rows, i, w, h)
                .is_some_and(|(rx, ry, rw, rh)| {
                    lx >= rx && lx <= rx + rw && ly >= ry && ly <= ry + rh
                })
        })
    }

    /// Draw one menu screen. Returns `false` when the current screen is not a
    /// menu, so the caller falls through to the world path.
    fn draw_menu(&mut self) -> bool {
        // Land any finished status pings before building the frame, or a row
        // shows "PINGING" for one frame longer than it needs to.
        self.statuses.pump();
        // `frame_for` is the authority on which screens this renderer owns — it
        // covers the three menu screens *and* the error screen. Asking it,
        // rather than re-deriving the set here, is what keeps the two from
        // drifting apart into a screen that is drawn twice or not at all.
        let Some(frame) = crate::menu::render::frame_for(
            &self.ui,
            &self.nav,
            &self.statuses,
            &mut self.favicons,
        ) else {
            return false;
        };
        let (Some(gpu), Some(target), Some(menu)) = (
            self.gpu.as_ref(),
            self.target.as_mut(),
            self.menu.as_mut(),
        ) else {
            // GPU not up yet; still report the screen as handled so the world
            // path does not run for a menu.
            return true;
        };
        let (w, h) = target.size();
        let device = gpu.device();
        let queue = gpu.queue();
        let surface = match target.acquire() {
            Ok(f) => f,
            Err(e) => {
                if e.needs_reconfigure() {
                    target.reconfigure(device);
                }
                return true;
            }
        };
        menu.render(device, queue, surface.view(), &frame, w, h);
        if let Some(window) = &self.window {
            window.pre_present_notify();
        }
        surface.present(queue);
        true
    }

    /// Route one key press to the open chat prompt. Enter sends the line through
    /// the client's chat/command seam, Escape cancels, Backspace edits, and any
    /// printable text is appended (control chars and `§` are filtered by
    /// [`ChatInput`]). Both Enter and Escape close the prompt and re-grab.
    fn handle_chat_key(&mut self, event: &winit::event::KeyEvent) {
        if let PhysicalKey::Code(code) = event.physical_key {
            match code {
                KeyCode::Escape => {
                    let _ = self.chat_input.take();
                    self.ui.close_chat();
                    self.set_grab(self.ui.wants_cursor_grab());
                    return;
                }
                KeyCode::Enter | KeyCode::NumpadEnter => {
                    let line = self.chat_input.take();
                    self.sim.send_chat(&line);
                    self.ui.close_chat();
                    self.set_grab(self.ui.wants_cursor_grab());
                    return;
                }
                KeyCode::Backspace => {
                    self.chat_input.backspace();
                    return;
                }
                _ => {}
            }
        }
        if let Some(text) = &event.text {
            self.chat_input.push_str(text.as_str());
        }
    }

    fn redraw(&mut self) {
        // Reconcile the menu with the live session before we borrow GPU state.
        self.drive_ui_from_session();
        if self.sim.open_menu().is_some() && self.ui.is_playing() {
            self.ui.open_container();
            self.set_grab(false);
        }

        // Pace the frame and tick **before** the GPU-readiness guard. Simulation
        // must never be conditional on a swapchain image: keep-alives and the
        // per-tick movement packet ride this loop, and a client the server
        // considers stalled is sent no chunks at all. `step.dt` is already
        // clamped to vanilla's ten-tick catch-up budget, so a long stall is
        // dropped rather than replayed in a burst.
        let frame_start = Instant::now();
        let step = self.pacer.begin_frame(frame_start);
        let dt = step.dt;
        // `Runner::Winit`: the host event loop drives this driver's `App`
        // itself, once per `RedrawRequested`, by calling `update()` directly
        // — no internal timer, so packet ingest is never gated on frame rate.
        self.ecs.update();
        // Issues #202/#203: pushed down before `step`, not after like View
        // Bobbing below — `step` is what actually reads them this call
        // (`apply_mouse`'s look-inversion, and the toggle-mode push into
        // `InputState` for every catch-up tick this call runs), so pushing
        // them post-step would apply this frame's option change one frame
        // late.
        self.sim
            .set_mouse_invert(self.nav.invert_mouse_x(), self.nav.invert_mouse_y());
        self.sim
            .set_toggle_modes(self.nav.toggle_sneak(), self.nav.toggle_sprint());
        self.sim.step(dt);
        if !step.render {
            // Unfocused (throttled to ~30 fps) or occluded: skip presenting
            // only. `acquire()` is the call that stalls on a backgrounded
            // window, so it is precisely what must not run here.
            return;
        }

        // Vanilla's View Bobbing option, pushed down before either draw path
        // because the toggle lives on a menu screen and should take effect while
        // that screen is still showing. Polled per frame rather than fired on the
        // toggle for the same reason the present-mode sync was: `MenuNav` owns the
        // `Options` and is pure, and `Sim` owns none.
        self.sim.set_view_bobbing(self.nav.view_bobbing());

        // A menu screen owns the whole frame — its pass clears, so there is no
        // world render behind it and none of the HUD state below is built.
        if self.draw_menu() {
            return;
        }

        let (Some(gpu), Some(target), Some(render), Some(hud), Some(container_renderer)) = (
            self.gpu.as_ref(),
            self.target.as_mut(),
            self.render.as_mut(),
            self.hud.as_mut(),
            self.container.as_mut(),
        ) else {
            return;
        };
        let device = gpu.device();
        let queue = gpu.queue();

        // Upload any freshly-meshed sections, and drop sections emptied by edits.
        for meshed in self.sim.drain_meshes() {
            render.upload_section(device, queue, meshed.key, &meshed.mesh);
        }
        for key in self.sim.drain_removals() {
            render.remove_section(&key);
        }

        let (w, h) = target.size();
        let frame = match target.acquire() {
            Ok(frame) => frame,
            Err(e) => {
                if e.needs_reconfigure() {
                    target.reconfigure(device);
                    render.resize(device, w, h);
                }
                // Transient (timeout/occluded/validation): just skip this frame.
                return;
            }
        };

        let aspect = w as f32 / h as f32;
        // Recompute the targeted block from the interpolated camera each frame.
        self.sim.update_target(aspect);
        // The true first-person eye: block targeting and the audio listener
        // deliberately keep reading this one even in third person (see
        // `Sim::camera`'s doc) — only the actual draw call below wants the
        // pulled-back camera.
        let camera = self.sim.camera(aspect);
        // What the frame is actually drawn from: `camera` unmodified in first
        // person, or `camera` pulled back (collision-clamped) behind the
        // player in third person. Installing the third-person body source
        // every frame is cheap (one small `Option` clone, no live borrow of
        // `Sim` needed inside the closure) and keeps the two in lock-step —
        // see `RenderState::set_third_person_body_source`'s doc for why a
        // `None`/`Some` source *is* the camera-mode toggle.
        let render_camera = self.sim.render_camera(aspect);
        let body_state = self.sim.third_person_body_state();
        render.set_third_person_body_source(move || body_state.clone());
        // This frame's arm-swing progress, for the first-person arm pass. Sampled
        // here and moved into the closure rather than captured by reference, for
        // the same reason as `body_state` above: the source outlives this call and
        // must not borrow `Sim`.
        //
        // **Installed every frame, and it has to be** — the value is a partial-tick
        // interpolation, so a one-shot install at connect time would freeze the arm
        // at whatever the swing looked like the instant we joined. `body_state`
        // right above it has the identical requirement, which is why the two sit
        // together. Only the *reading* is per frame; the swing clock itself
        // advances on the 20 Hz tick inside `Sim::step`.
        let hand_swing = self.sim.hand_swing_progress();
        render.set_hand_swing_source(move || hand_swing);

        // The hand needs its own copy of the view bob: vanilla applies `bobView`
        // a *second* time to a fresh pose stack seeded with the unbobbed
        // model-view (`GameRenderer.java:333-362`), rather than letting the hand
        // inherit the world's bobbed matrix. Without this the whole chain is an
        // island — `hand_view_proj` reads a source nothing installs, so the arm
        // stays rigid while the camera bobs, which is what the player reported.
        let hand_bob = self.sim.bob_frame();
        render.set_hand_bob_source(move || hand_bob);

        // Snapshot the player's nine hotbar slots into owned draw records.
        //
        // **Hoisted above the world render on purpose.** The HUD is the obvious
        // consumer, but `set_main_hand_source` below is read inside
        // `RenderState::render`, so this has to exist before that call. Doing it
        // once here rather than twice serves both from a single `Menu` clone —
        // `Sim::player_menu` clones all 46 slots, and a second call per frame is
        // exactly the cost the mining-freeze fix removed from the tick path.
        let player_menu = self.sim.player_menu();
        let hotbar_records: Vec<Option<HotbarSlot>> = (0..9)
            .map(|i| {
                player_menu.player_native(i).and_then(|st| {
                    let item = ResourceLocation::parse(&st.item().to_string()).ok()?;
                    let damage = st
                        .components()
                        .get_int(lodestone_game::item::DAMAGE_COMPONENT)
                        .and_then(|v| u32::try_from(v).ok());
                    let max_damage = st
                        .components()
                        .get_int(lodestone_game::item::MAX_DAMAGE_COMPONENT)
                        .and_then(|v| u32::try_from(v).ok());
                    Some(HotbarSlot {
                        item,
                        count: st.count().max(0) as u32,
                        damage,
                        max_damage,
                        enchanted: false,
                    })
                })
            })
            .collect();
        drop(player_menu);

        // What the player is holding, for the first-person hand pass. Vanilla's
        // `ItemInHandRenderer` forks on `isEmpty()` and draws *either* the item or
        // the bare arm, never both — `None` here is that empty hand, which is also
        // what the demo path and every headless test get.
        //
        // Installed every frame for the same reason as the swing above: the value
        // changes the instant the player scrolls the hotbar, so a one-shot install
        // would freeze slot 0 into the hand forever. Sampled and moved, because the
        // source outlives this call and must not borrow `Sim`.
        let held = hotbar_records
            .get(self.sim.selected_slot())
            .and_then(|record| record.as_ref())
            .map(|record| record.item.clone());
        // Cloned rather than moved whole into the closure below: issue #154's
        // spyglass FOV/vignette needs this same value further down in this
        // function (`ScreenEffects::scoping`), and the closure otherwise
        // takes ownership for the render source's lifetime.
        let held_for_scoping = held.clone();
        render.set_main_hand_source(move || held.clone());

        // Block entities — chests (issue #23). **This install is what makes a
        // chest visible at all**: a 26.2 chest has no block model (its
        // `block/chest.json` declares only a particle texture, zero elements), so
        // without this the terrain mesher leaves a hole where every chest is.
        //
        // Installed every frame, like the swing and the held item above and for
        // the same reason: the closure captures this frame's partial tick and a
        // snapshot of the lid map, so a one-shot install at connect would draw
        // every lid frozen at the fraction of a tick we happened to join on.
        if let Some(f) = self.sim.block_entity_source() {
            render.set_block_entity_source(f);
        }

        // Skulls and heads. Same per-frame install as the chests above, though for
        // a weaker reason: none of the ported skull types animate, so there is no
        // partial tick to go stale. It is installed here anyway rather than once at
        // connect so the two block-entity sources cannot drift into different
        // lifetimes — a skull source that survived a disconnect would keep handing
        // out spawns from a dead world's handle.
        if let Some(f) = self.sim.skull_source() {
            render.set_skull_source(f);
        }

        // Signs. Same per-frame install as chests and skulls above; see
        // `Sim::sign_source` for why it captures no partial tick.
        if let Some(f) = self.sim.sign_source() {
            render.set_sign_source(f);
        }

        // Bells. Same per-frame install as the three above — the render pass,
        // the GPU-side wiring in `gpu.rs` and the CPU-side gather
        // (`Sim::bell_source`) were all already landed; this call site was
        // the one remaining hop before a live client draws a bell at all
        // (`docs/block-entity-renderers.md`'s Bell section).
        if let Some(f) = self.sim.bell_source() {
            render.set_bell_source(f);
        }

        // Reconcile fog with the player's bit-exact fluid state each frame,
        // re-uploading only when it changes (crossing a water/lava surface) so a
        // submerged eye dissolves terrain into short water/lava fog and the
        // surface restores the render-distance sky fog.
        //
        // Weather darkens *both* ends of the gradient before the change check, so
        // the storm reaches the sky disc's centre, its horizon, the terrain fog and
        // the below-horizon clear colour from one place. Doing it after would leave
        // the clear colour bright and put a hard clear-vs-fog seam at the horizon,
        // which is exactly what `set_clear_color`'s own doc warns about.
        //
        // A ramping rain level therefore re-uploads the fog uniform every tick
        // rather than only on a fluid crossing. That is intended: the ramp is
        // ±0.01/tick over ~100 ticks (`ServerLevel.java:762-768`), and a
        // change-detected upload that ignored it would render a storm at clear-sky
        // colours until the player happened to swim.
        let weather_state = self.weather.as_ref().map(|w| w.state());
        let desired_fog = {
            let base = self.sim.fog_settings();
            match &weather_state {
                Some(w) => {
                    let rain = w.rain_level();
                    let thunder = w.thunder_level();
                    let flashing = w.flashing();
                    // Vanilla's layer order: the flash tint is added by
                    // `ClientLevel.addEnvironmentAttributeLayers` and the weather
                    // darkening by `WeatherAttributes.addBuiltinLayers` on top, so
                    // a bolt during a storm brightens a sky that is *then* darkened
                    // — not the other way round, which would wash the flash out.
                    let sky = lodestone_render::weather_darken_linear(
                        lodestone_render::lightning_flash_linear(base.sky_color, flashing),
                        rain,
                        thunder,
                    );
                    let fog = lodestone_render::weather_darken_linear(
                        lodestone_render::lightning_flash_linear(base.color, flashing),
                        rain,
                        thunder,
                    );
                    lodestone_render::fog::FogSettings {
                        color: fog,
                        sky_color: sky,
                        ..base
                    }
                }
                None => base,
            }
        };
        if self.applied_fog != Some(desired_fog) {
            render.set_fog(desired_fog, self.config.render_distance);
            // The clear colour must never disagree with the fog colour it is
            // set alongside — see `RenderState::set_clear_color`'s doc and
            // `docs/dimension-visuals.md`'s wiring note. Piggybacking on the
            // same change-detected `if` this fog upload already used is free:
            // there is no separate "did the clear colour change" condition to
            // get out of sync with it.
            // `_tracked`: applies the same `FOG_COLOR` day/night track
            // `fog_with_clock` applies, so the clear colour and the terrain fog
            // cannot drift apart. `desired_fog.color` is the untracked day base
            // (weather-darkened, not clock-tracked), which is exactly what this
            // wants and what `set_fog` two lines up already receives — passing an
            // already-tracked colour would apply the track twice.
            render.set_clear_color_tracked(desired_fog.color);
            self.applied_fog = Some(desired_fog);
        }
        // Drive the audio listener from the exact camera we render, so what the
        // player hears is spatialised to match what they see. No-op when audio
        // is disabled.
        self.sim.set_audio_listener(&camera);
        let outline = self.sim.target().map(|hit| hit.block);
        let entity_draws = self.sim.entity_draws();
        // Extraction lives in `Sim` because resolving each particle's light
        // needs the world; doing it here would hand out two borrows of `Sim`.
        let particle_frame = self.sim.extract_particles(&camera);
        render.prepare_particles(device, queue, &self.sim.particle_instances(), &camera);
        let tick = self.sim.tick_count();
        render.update_animation(queue, tick);

        // Precipitation columns. Inlined rather than a `self.` method because
        // `render` is a live `&mut` borrow of `self.render` for the rest of this
        // function, so any `&self` method call here is a second borrow; the pure
        // half lives in `weather_columns_for_frame` instead.
        //
        // Skipped entirely in clear weather — `extract_columns` returns empty on a
        // zero rain level, and the light sample below is the one world lock this
        // costs, so a clear frame pays nothing.
        {
            let (columns, rain_columns) = weather_state
                .as_ref()
                .filter(|w| w.any_precipitation())
                .map(|w| {
                    // ONE light sample per frame, at the eye, reused for every
                    // column — see `ShellWeatherProbe`'s doc for the three
                    // divergences that buys and why 441 world locks per frame was
                    // not the trade to make first. `sky_darken()` is the
                    // weather-folded factor the terrain and entity passes are
                    // already using this frame, so the rain cannot be lit by a
                    // different sky than the blocks it falls past.
                    let packed = self
                        .sim
                        .net()
                        .map(|n| (n.shared_handle(), n.shared_sky_default()))
                        .and_then(|(h, policy)| {
                            crate::net::entity_light_at(
                                &h,
                                camera.position.x.floor() as i32,
                                camera.position.y.floor() as i32,
                                camera.position.z.floor() as i32,
                                // Load-bearing for `sky_visible` below, not just for
                                // brightness: absent sky data used to resolve to 0
                                // here, so `(p >> 4) & 0x0F > 0` was false and rain
                                // rendered **nowhere in open sky** — the one place a
                                // player is guaranteed to be looking at it.
                                policy.get(),
                            )
                        });
                    let probe = ShellWeatherProbe {
                        light: lodestone_render::light::light_term(
                            packed.unwrap_or(lodestone_render::ENTITY_FULLBRIGHT),
                            render.sky_darken(),
                        ),
                        // No sample at all is "world not loaded yet", which must
                        // read as open sky: a `false` here would make the very
                        // first rainy frames after a join silently empty, which is
                        // indistinguishable from the pass being unwired.
                        sky_visible: packed.is_none_or(|p| ((p >> 4) & 0x0F) > 0),
                        handle: self.sim.net().and_then(|n| n.shared_handle().get().cloned()),
                        biome_climates: self.sim.net().map(crate::net::NetClient::shared_biome_climates),
                    };
                    weather_columns_for_frame(w, &camera, tick, &probe)
                })
                .unwrap_or_default();
            render.prepare_weather(device, queue, &columns, rain_columns, &camera);
        }
        // The underwater/fire overlay pass's per-frame input (issues #108,
        // #112). `eye_in_water` is the *same* `PhysicsState` predicate the
        // submerged fog and the air-bubble row already read
        // (`docs/sky-and-air-bubbles.md`) — not a second derivation. `on_fire`
        // now comes from `PlayerSnapshot::on_fire`, folded by
        // `apply_local_player_on_fire`: the shared-flags byte reaches a generic
        // `EntityFlags` component for any entity, but the local player is
        // deliberately excluded from the generic entity-view path, so it needs a
        // session-scoped fold to arrive at all. `false` without a live
        // connection, which is also the pre-first-packet answer.
        let on_fire = self
            .sim
            .net()
            .and_then(|n| n.shared_handle().get().cloned())
            .is_some_and(|h| h.player().on_fire);
        let spectator = self
            .sim
            .net()
            .and_then(|n| n.shared_handle().get().cloned())
            .and_then(|h| h.game_mode())
            == Some(lodestone_client::GameMode::Spectator);
        // Native slot 39 is the head, per `Menu::player`'s own table (menu slots
        // `5..=8` are head/chest/legs/feet at native `39/38/37/36`, running
        // backwards feet-first) — the same indices `Sim::third_person_body_state`
        // reads for the armour layers.
        //
        // Matched on the item id rather than on
        // `minecraft:equippable.camera_overlay`, which is what vanilla actually
        // keys on (`Hud.extractCameraOverlays`, `Hud.java:269-291`). That is a
        // deliberate narrowing and it matches `ScreenEffects::wearing_pumpkin`'s
        // own doc: carved pumpkin is the only item shipping with that component
        // field set, so the general per-item lookup would have exactly one entry.
        // If a second item ever gains it, this is the line that has to become the
        // component read.
        const HEAD_NATIVE_SLOT: usize = 39;
        let wearing_pumpkin = self
            .sim
            .player_menu()
            .player_native(HEAD_NATIVE_SLOT)
            .is_some_and(|st| st.item().to_string() == "minecraft:carved_pumpkin");
        // The freeze overlay's per-frame input (issue #139). `PlayerState::
        // percent_frozen` is real, tested physics state (`update_freezing`,
        // issue #212, `lodestone-physics`) — not a stub. `Sim::player()`
        // already returns `PlayerState` by value, so this needs no new `Sim`
        // accessor. See `docs/screen-overlays.md`'s "Freeze" section.
        let freeze_percent = self.sim.player().percent_frozen();
        let screen_effects = crate::gpu::ScreenEffects {
            eye_in_water: self.sim.player().eye_in_water,
            on_fire,
            spectator,
            tick,
            wearing_pumpkin,
            freeze_percent,
            // `Player.isScoping()` is `isUsingItem() && getUseItem().is(Items.
            // SPYGLASS)` (`Player.java:1936-1938`). Both halves: `Sim::
            // using_item()` (the two-line accessor issue #154 was waiting
            // on) and `held_for_scoping`, the same item id already computed
            // above for the first-person hand pass.
            scoping: self.sim.using_item()
                && held_for_scoping
                    .as_ref()
                    .is_some_and(|loc| loc.namespace() == "minecraft" && loc.path() == "spyglass"),
            // No potion-effect-duration tracker or nether-portal-proximity
            // tracker exists anywhere in this codebase yet to compute these
            // — `0.0` is the honest current answer, not a placeholder
            // pretending to work. See `docs/screen-overlays.md`'s "Confusion
            // and portal" section.
            nausea_intensity: 0.0,
            portal_intensity: 0.0,
        };
        // Route the progressive-mining crack overlay(s) (issue #410): the local
        // player's own dig plus one slot for every *other* player's overlay the
        // server has reported. `CrackPipeline`/`render_with_crack_and_effects`
        // accept any number of targets in one pass, and `Sim::crack_targets`
        // is the accessor that actually walks `SessionBlockDestruction`/
        // `BlockDestructionOverlays` via `crate::gpu::gather_crack_targets` —
        // the hop that was still missing when #410 was closed: the gather and
        // the pipeline were both proven in isolation, but nothing in
        // production called the gather, so only the local target ever reached
        // this vec.
        let cracks: Vec<crate::gpu::CrackTarget> = self.sim.crack_targets();
        let stats = render.render_with_crack_and_effects(
            device,
            queue,
            frame.view(),
            &render_camera,
            outline,
            &entity_draws,
            &cracks,
            screen_effects,
        );

        // Fold GPU counters + timing into the debug overlay.
        let frame_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
        let inst_fps = if dt > 0.0 { (1.0 / dt) as f32 } else { 0.0 };
        self.fps_ema = if self.fps_ema == 0.0 {
            inst_fps
        } else {
            self.fps_ema * 0.9 + inst_fps * 0.1
        };
        self.sim.stats.section_count = stats.sections_drawn;
        self.sim.stats.quads = stats.total_quads;
        self.sim.stats.vram_bytes = stats.vram_bytes;
        self.sim.stats.entities_drawn = stats.entities_drawn;
        self.sim.stats.particles_alive = particle_frame.alive;
        self.sim.stats.particles_drawn = stats.particles_drawn;
        self.sim.stats.particles_unresolved = particle_frame.unresolved;
        self.sim.stats.frame_ms = frame_ms;
        self.sim.stats.fps = self.fps_ema;
        // Issue #411: `ServerDifficulty` reached a real, tested ECS fold but
        // nothing in the shell read it — this is that last hop, onto the F3
        // debug overlay's own `DIFFICULTY` line (`hud.rs`'s `DebugStats::lines`).
        self.sim.stats.difficulty = self.sim.difficulty();

        // The baked 3-D item geometry, shared by the container screen below and the
        // HUD hotbar further down. It borrows `self.sim`, so it cannot be hoisted
        // above the `self.sim.stats` writes just above — but it must exist before
        // the container overlay, which is the pass that was missing it.
        let item_models = self.sim.vanilla_atlas().and_then(|a| a.models());

        // Assemble the HUD frame: debug overlay, chat log + prompt, tab list,
        // and the survival gauges. Locals are collected up-front so their
        // borrows outlive the frame struct.
        let chat_open = self.ui.is_chat_open();
        // Pull enough history for the HUD to fade/scroll; it caps and ages them.
        // The feed hands back owned legacy strings (flattened from the canonical
        // `ChatFeed`'s `Text` at read time); borrow them into the `&str` slice
        // the HUD frame takes, keeping both locals alive for the frame's scope.
        let chat_owned: Vec<(String, f32)> = self.sim.recent_chat(if chat_open { 20 } else { 10 });
        let chat_lines: Vec<(&str, f32)> = chat_owned
            .iter()
            .map(|(line, age)| (line.as_str(), *age))
            .collect();
        let player_rows: Vec<String> = if self.tab_held {
            self.sim.player_rows()
        } else {
            Vec::new()
        };
        let health = self.sim.health();
        let food = self.sim.food();
        // `HudState::MAX_AIR` — the same constant `PlayerSnapshot::air` fills
        // an unreported value with — rather than a second hardcoded `300`.
        let air = self
            .sim
            .air()
            .map(|a| (a, lodestone_game::player_state::HudState::MAX_AIR, self.sim.player().eye_in_water));
        let sidebar = self.sim.sidebar();
        let boss_bars = self.sim.boss_bars();
        // Two different questions, and they used to share one boolean named
        // `crosshair` — which is why the hotbar vanished behind the pause menu
        // and the inventory (issue #61). The crosshair is the aiming reticle and
        // belongs to *active* play; the hotbar belongs to the **world**, and
        // vanilla keeps it on screen behind every in-game screen.
        let crosshair = self.ui.is_playing();
        let world_hud = hud_follows_world(self.ui.screen());

        let mut hud_frame = HudFrame::new(&self.sim.stats);
        hud_frame.show_debug = self.show_debug;
        hud_frame.crosshair = crosshair;
        hud_frame.chat = &chat_lines;
        hud_frame.chat_input = chat_open.then(|| self.chat_input.as_str());
        // Vanilla blinks the text cursor on a 300 ms half-period:
        // `TextCursorUtils.CURSOR_BLINK_INTERVAL_MS == 300` and
        // `isCursorVisible(ms) == (ms / 300) % 2 == 0`
        // (`.cache/mc/26.2/client-src/.../TextCursorUtils.java:9,20-22`). The
        // phase has to come from wall time rather than the tick clock, because
        // the caret keeps blinking while the game is paused.
        hud_frame.chat_caret_visible = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_millis() / 300) % 2 == 0)
            .unwrap_or(true);
        // Without this the whole chat-option chain is an island: the fields are
        // persisted, `ChatDisplayOptions` is read by the draw, and the live
        // client would still show vanilla defaults forever.
        let chat_opts = self.nav.options();
        hud_frame.chat_options = crate::hud::ChatDisplayOptions {
            scale: chat_opts.chat_scale,
            width_pct: chat_opts.chat_width,
            height_pct_unfocused: chat_opts.chat_height_unfocused,
            height_pct_focused: chat_opts.chat_height_focused,
            line_spacing: chat_opts.chat_line_spacing,
            text_opacity: chat_opts.chat_opacity,
            background_opacity: chat_opts.chat_background_opacity,
            colors: chat_opts.chat_colors,
        };
        hud_frame.players = self.tab_held.then_some(player_rows.as_slice());
        hud_frame.sidebar = sidebar.as_ref();
        hud_frame.boss_bars = &boss_bars;
        hud_frame.health = health;
        hud_frame.food = food;
        // Without this the hunger wobble (issue #30) is computed correctly and
        // never fires: vanilla shakes the row only while saturation is
        // exhausted, so an unfed `saturation` reads as "always satisfied".
        hud_frame.saturation = self.sim.saturation();
        hud_frame.air = air;
        hud_frame.hotbar = world_hud.then(|| self.sim.selected_slot());
        hud_frame.hotbar_items = world_hud.then_some(hotbar_records.as_slice());
        hud_frame.xp = self.sim.xp();
        hud_frame.title = self.sim.title_overlay();
        hud_frame.action_bar = self.sim.action_bar_overlay();
        hud_frame.held_item = self.sim.held_item_overlay();
        hud_frame.recipe_stats = self
            .recipe_book
            .as_ref()
            .map(|book| (book.len(), book.tags().len()));
        // The recipe-unlock toast (issue #163). `None` on every real session
        // today, because the queue's only possible producer is the
        // `recipe_book_add` decode that does not exist yet — see the field's own
        // doc. Wired here anyway so it lights up the moment that lands.
        hud_frame.recipe_toast = recipe_toast_view(&self.recipe_toasts, recipe_toast_now_ms());
        // Always `Some`: `Sim::attack_strength_scale` is defined on both the
        // demo and live worlds (the ticker and the `attack_speed` attribute
        // default both exist before any server connection), unlike
        // `health`/`food`/`xp` which stay `None` until a server reports them.
        // `hud.rs`'s draw site is what actually gates this on
        // `frame.crosshair` — see that field's doc for why the crosshair
        // hides behind an open screen but the hotbar does not (issue #61).
        hud_frame.attack_cooldown = Some(self.sim.attack_strength_scale());
        // The 3-D block-item icons need the baked model set (for geometry) and a
        // depth attachment (so the near faces of the mini-block win over the far
        // ones). Both are `None` on the demo path, which degrades to flat sprites.
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            Some(render.depth_view()),
            &hud_frame,
            item_models,
            self.nav.gui_scale(),
            w,
            h,
        );
        // Status-effect overlay, composited over the HUD in its own Load pass.
        if let Some(effects) = self.effects.as_mut() {
            effects.render(device, queue, frame.view(), &self.sim.active_effects(), w, h);
        }

        // The container overlay draws **after** the HUD (issue #51/#61): vanilla's
        // `Gui.render` draws the HUD unconditionally behind any world-following
        // screen (`hud_follows_world` above), and the screen then paints its own
        // translucent background over it (`Screen.java:375-386`,
        // `AbstractContainerScreen::isInGameUi`) — the dim is draw order, not a
        // per-element alpha. Drawing this block before the HUD (as it used to)
        // meant the HUD painted back over the container's dim every frame and the
        // hotbar never actually looked dimmed behind an open chest. Both this pass
        // and the HUD's own model sub-pass independently clear the shared depth
        // buffer immediately before drawing their own GUI items, so swapping the
        // two relative to each other is safe — see `docs/container-screen.md`.
        let open_menu = self.sim.open_menu();
        let player_menu;
        let (container_menu, container_title) = if let Some(open) = open_menu.as_ref() {
            // Through the language table, not `Text::to_plain_string` — the
            // server sends `translate("container.crafting")`, and the model's
            // stub table has no `container.*` key, so flattening it directly put
            // the raw key on screen (issue #52). See `container::menu_title`.
            (
                Some(&open.menu),
                crate::container::menu_title(&open.title, self.sim.translator().as_ref()),
            )
        } else if self.ui.is_container_open() {
            player_menu = self.sim.player_menu();
            // **"Crafting"**, not "Inventory" (issue #370). `InventoryScreen`
            // passes `translatable("container.crafting")` as its title
            // (`InventoryScreen.java:28`) — it names the 2x2 grid — and the
            // literal `"Inventory"` that used to sit here was wrong twice: wrong
            // word, and, going in as the *title*, drawn at the title anchor,
            // which on this one screen is `x = 97`. The word "Inventory" does
            // exist in vanilla, as the *second* label, which this screen is the
            // only one to omit; see `container::label_layout`.
            (
                Some(&player_menu),
                crate::container::player_inventory_title(self.sim.translator().as_ref()),
            )
        } else {
            (None, String::new())
        };
        if container_menu.is_some() {
            // `playerInventoryTitle` through the same language table. A local
            // constant here is not the #52 defect class repeating: vanilla reads
            // it from `Inventory.getDisplayName()`, itself the client-side
            // constant `translatable("container.inventory")`
            // (`Inventory.java:55`), so there is no server component to resolve.
            let inventory_label =
                crate::container::player_inventory_label(self.sim.translator().as_ref());
            // The carried stack follows the pointer, so the frame needs the cursor
            // in physical pixels — the same space `hit_test` and the menu layout
            // use (see the `cursor` field). Without this the stack is built but
            // never positioned, and nothing draws.
            // The live drag preview (issue #378 part 2). `drag_paint` is the
            // *same* paint set `MenuInput::release` will turn into the
            // QUICK_CRAFT sequence, and the counts drawn from it come out of
            // `Menu::quick_craft_plan`, which is what distributes them — so the
            // preview cannot show a split the release will not produce.
            let container_frame = ContainerFrame::new(container_menu, &container_title)
                .with_inventory_label(&inventory_label)
                .with_cursor(Some([self.cursor.0, self.cursor.1]))
                .with_drag(self.menu_input.drag_paint())
                // The wire `menu_type`, which is what `menu_type_title_anchor`
                // keys on. Without this line the nine per-screen title anchors
                // are correct and **unfed**, so a furnace or an anvil silently
                // falls back to the generic `(8, 6)` — the same class of gap as
                // a source installed but never set.
                .with_menu_type(open_menu.as_ref().map(|open| &open.menu_type))
                .with_recipe_book(self.recipe_book.as_ref())
                // The anvil's XP cost and the enchanting table's three level
                // costs (`docs/container-cost-screens.md`'s "What is not yet
                // wired" gap). `&[]` on the player-inventory screen (no
                // `open_menu`), which draws neither cost — correct, since
                // neither special layout is ever the player's own inventory.
                .with_cost_context(
                    open_menu.as_ref().map_or(&[][..], |open| open.data.as_slice()),
                    self.sim.has_infinite_materials(),
                    self.sim.xp().map_or(0, |(level, _)| level),
                );
            // `render_with_icons_scaled`, **not** `render_scaled`: the latter
            // hardcodes `depth: None, models: None`, so `want_models` was always
            // false and `push_item_model` returned early. Flat sprite icons still
            // drew (they need only `attach_items`), which is exactly why the symptom
            // read as "block items render *flat*" rather than "nothing renders" —
            // and why it survived as an island with `attach_items` *and*
            // `attach_item_models` both already wired.
            //
            // The `_scaled` variant is required: the plain one lays out against
            // `AUTO_GUI_SCALE` and would disagree with `hit_test_with_scale` about
            // where the slots are.
            container_renderer.render_with_icons_scaled(
                device,
                queue,
                frame.view(),
                Some(render.depth_view()),
                &container_frame,
                item_models,
                self.nav.gui_scale(),
                w,
                h,
            );

            // The recipe-book panel (issue #163), as its own pass **over** the
            // container panel it belongs to — the toggle button sits on the
            // container's own chrome and the book body overlaps its left edge at
            // narrow canvases (`container.rs`'s documented clamp), so drawing it
            // before the container would bury both.
            //
            // This call is what stops the whole
            // `recipe_book_panel_layout`/`_hit_test`/`_geometry` family being an
            // island: it was built and unit-tested with 75 tests and reached
            // zero pixels because nothing composited the vertices.
            if let Some(menu) = container_menu {
                let items = hud.item_atlas();
                if let Some(geo) = recipe_panel_geometry(
                    self.recipe_book.as_ref(),
                    &self.recipe_panel,
                    menu,
                    self.nav.gui_scale(),
                    items.as_deref(),
                    item_models,
                    w,
                    h,
                ) {
                    hud.render_recipe_book_panel(
                        device,
                        queue,
                        frame.view(),
                        Some(render.depth_view()),
                        &geo,
                        self.nav.gui_scale(),
                        w,
                        h,
                    );
                }
            }
        }

        // The pause overlay draws *over* the world/HUD/container passes above
        // rather than replacing them — see `Screen::Paused`'s doc comment and
        // `menu::render::owns_frame`'s, which is deliberately why `Paused` is
        // not in that set: adding it there would route this screen through
        // `draw_menu`'s `Clear` pass instead and stop the world rendering
        // behind it for as long as the game is paused.
        if self.ui.is_paused()
            && let Some(menu) = self.menu.as_mut()
        {
            let pause_frame = crate::menu::render::pause_frame(&self.nav);
            menu.render_overlay(device, queue, frame.view(), &pause_frame, w, h);
        }

        // The death screen (issue #103) follows exactly the same overlay
        // shape as pause, for the same reason: a live server holds a dead
        // player with no chunk stream until it respawns, so this must draw
        // over the still-rendering, still-ticking world rather than replace
        // it — see `Screen::Death`'s doc comment.
        if self.ui.is_death()
            && let Some(menu) = self.menu.as_mut()
        {
            let death_frame = crate::menu::render::death_frame(&self.nav, self.sim.death_message());
            menu.render_overlay(device, queue, frame.view(), &death_frame, w, h);
        }

        // In-world Options, from a player report: settings opened mid-game used
        // to draw the *panorama* behind itself, which belongs to the main menu
        // only. `menu::render::frame_for` now returns `None` for
        // `Screen::Settings` when `ui.settings_in_world()`, so the frame has to
        // be drawn here as an overlay over the still-rendering paused world —
        // the same shape as pause and death above. Without this block that
        // `None` means the screen draws *nothing*, which is worse than the
        // panorama it replaced, so the two halves must stay together.
        if self.ui.is_settings()
            && self.ui.settings_in_world()
            && let Some(menu) = self.menu.as_mut()
        {
            let settings_frame = crate::menu::options::settings_frame(
                self.nav.settings(),
                self.nav.options(),
                self.nav.options_save_error(),
            );
            menu.render_overlay(device, queue, frame.view(), &settings_frame, w, h);
        }

        if let Some(window) = &self.window {
            window.pre_present_notify();
        }
        frame.present(queue);

        if self.last_log.elapsed() >= Duration::from_secs(1) {
            self.last_log = Instant::now();
            println!("{}", self.sim.stats.one_line());
        }
    }
}

impl ApplicationHandler for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Lodestone")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        let (gpu, target) = match attach_window(window.clone()) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("failed to attach GPU to window: {e}");
                event_loop.exit();
                return;
            }
        };

        let (w, h) = target.size();
        let format = target.format();
        let mut render = RenderState::new(
            gpu.device(),
            gpu.queue(),
            format,
            w,
            h,
            self.sim.vanilla_atlas(),
        );
        // Size the sky fog to our real render distance so terrain fades into the
        // sky where chunks actually stop, not at the render crate's 8-chunk default.
        render.set_fog(sky_fog(self.config.render_distance), self.config.render_distance);
        // Upload the stitched particle sheet the emitter already resolves its
        // flame/smoke/crit UVs against (issue #45). `load_particle_atlas` is
        // memoised, so this is the **same** `ParticleAtlas` object `Sim` built
        // its `(Sheet, frame) -> UV` table from — not a second stitch that
        // happens to pack the same way. The bug being closed here is a UV table
        // addressing a different image than the one bound, and every counter
        // reads perfectly healthy while it is happening, so the identity is made
        // structural rather than assumed.
        if let Some(sheet) = crate::resources::load_particle_atlas() {
            render.install_particle_sheet_atlas(gpu.device(), gpu.queue(), sheet.atlas());
        }
        let mut hud = HudRenderer::new(gpu.device(), format);
        // Attach the vanilla GUI sprite atlas so the survival vitals draw from
        // real textures; on a jar-less run this is `None` and the HUD keeps its
        // procedural fallback.
        if let Some(gui) = crate::resources::load_gui_atlas() {
            hud.attach_gui(gpu.device(), gpu.queue(), format, gui);
        }
        // Load the real crafting-recipe corpus from `client.jar`, once. Feeds
        // the container screen's ghost-preview draw and the debug-overlay
        // counter; a jar-less run leaves this `None` and neither draws.
        self.recipe_book = crate::resources::load_recipe_book();
        // Attach the flat item-sprite atlas so hotbar/container slots draw real
        // item icons; jar-less runs leave this `None` and slots stay empty wells.
        // Loaded once and shared: the container screen needs the same atlas, and
        // `ItemAtlas` is behind an `Arc` precisely so the second consumer is a
        // refcount bump rather than a second stitch of the whole item corpus.
        let item_atlas = crate::resources::load_item_atlas();
        if let Some(items) = item_atlas.clone() {
            hud.attach_items(gpu.device(), gpu.queue(), format, items);
        }
        // Attach the 3-D block-item pass, which borrows the world renderer's own
        // block atlas, tint palette and animation slots rather than uploading a
        // second copy of any of them. Present only on the live vanilla path (the
        // demo world bakes no models), where block items would otherwise draw an
        // empty well.
        if let (Some(atlas_view), Some(atlas_sampler), Some(palette), Some(anim)) = (
            render.model_atlas_view(),
            render.model_atlas_sampler(),
            render.model_palette_buffer(),
            render.model_anim_buffer(),
        ) {
            hud.attach_item_models(
                gpu.device(),
                format,
                atlas_view,
                atlas_sampler,
                palette,
                anim,
            );
        }
        let effects = EffectsRenderer::new(gpu.device(), format);

        // The container screen draws real item icons through the *same* shared
        // pass the hotbar uses (`hud::item_icon`), so both must be attached or
        // slots fall back to hash-derived colour swatches. Without this the
        // capability is complete, gated and reaches zero pixels — the island
        // pattern this project has hit eleven times.
        let mut container = ContainerRenderer::new(gpu.device(), format);
        if let Some(items) = item_atlas {
            container.attach_items(gpu.device(), gpu.queue(), format, items);
        }
        if let (Some(atlas_view), Some(atlas_sampler), Some(palette), Some(anim)) = (
            render.model_atlas_view(),
            render.model_atlas_sampler(),
            render.model_palette_buffer(),
            render.model_anim_buffer(),
        ) {
            container.attach_item_models(
                gpu.device(),
                format,
                atlas_view,
                atlas_sampler,
                palette,
                anim,
            );
        }
        // Vanilla's real `container/*.png` panel art (issue #51). A jar-less
        // run leaves this `None` and the screen keeps its flat programmatic
        // fill — the same "is a thing attached" degradation as the two calls
        // above.
        if let Some(background) = crate::resources::load_container_background() {
            container.attach_background(gpu.device(), gpu.queue(), format, background);
        }

        // Upload whatever has already meshed; the rest streams in per frame.
        for meshed in self.sim.drain_meshes() {
            render.upload_section(gpu.device(), gpu.queue(), meshed.key, &meshed.mesh);
        }

        let menu = MenuRenderer::new(gpu.device(), format);

        // Choose the session per config. A connection target on the command line
        // dials it immediately (and shows a loading screen until login);
        // otherwise the window opens on the **main menu**, which is now the GUI
        // entry point. Singleplayer from the menu enters the local worldgen world
        // — *not* the integrated server, which isn't wired yet (see
        // `WindowApp::begin_singleplayer`).
        if requested_a_connection(&self.config) {
            self.ui.begin(SessionKind::Multiplayer);
            self.sim.connect(
                self.config.host.clone(),
                self.config.port,
                self.config.protocol,
            );
            let net = self
                .sim
                .net()
                .expect("Sim::connect always leaves a client attached");
            // Install the entity light sampler now, at connect time, not after
            // login: `set_entity_light_source` wants a `'static` closure
            // installed *once*, and the shared handle it needs is available
            // immediately (it is an `Arc<OnceLock<_>>` the net thread resolves
            // later — see `net::SharedHandle`). Waiting for `LoggedIn` would
            // just delay the install for no benefit, since the closure already
            // tolerates an unresolved handle (`entity_light_at` reads `None`
            // and the sampler falls back to full-bright, exactly matching the
            // "no world yet" state during connect). This has to happen before
            // `attach_net` moves `net` into `self.sim` — `NetClient` itself
            // isn't `Clone` and doesn't outlive this function, only the shared
            // handle inside it does.
            let entity_light_handle = net.shared_handle();
            // See `connect_to`: same clock for terrain and mobs, installed here
            // too because this is the second, independent connect path.
            let clock = net.shared_handle();
            // See `connect_to`: the sky pass's own clock, next to (but distinct
            // from) `set_sky_darken_source`'s already-derived factor.
            let sky_clock = net.shared_handle();
            // See `connect_to`: extrapolates between the ~1/sec `SET_TIME`
            // packets so the cloud scroll advances smoothly instead of
            // stepping once a second.
            let continuous_time_of_day = ContinuousTimeOfDay::new();
            // See `install_session_render_sources` for why weather rides the
            // `sky_darken` lane rather than getting its own uniform. Installed on
            // this path too, or a `--connect` launch renders a storm at full
            // daylight brightness while a menu-launched session does not: the
            // duplicated-source hazard this whole function's doc warns about.
            let weather = Arc::new(WeatherTracker::new(net.shared_weather()));
            self.weather = Some(weather.clone());
            render.set_sky_darken_source(move || {
                let base = clock.get().map(|h| {
                    lodestone_render::entity::sky_darken_for_time_of_day(h.world_time().1)
                })?;
                Some(lodestone_render::weather_sky_light_factor(
                    base,
                    &weather.state(),
                ))
            });
            // Same cell as `install_session_render_sources`, installed on this path
            // too for the reason that function's doc gives about duplicated
            // sources: a `--connect` launch that skipped it would black out mobs in
            // open air while a menu-launched session did not.
            let light_policy = net.shared_sky_default();
            render.set_entity_light_source(move |feet| {
                crate::net::entity_light_at(
                    &entity_light_handle,
                    feet.x.floor() as i32,
                    feet.y.floor() as i32,
                    feet.z.floor() as i32,
                    // Read per call, not captured: a portal changes this mid-session.
                    light_policy.get(),
                )
            });
            render.set_time_of_day_source(move || {
                sky_clock
                    .get()
                    .map(|h| continuous_time_of_day.advance(h.world_time().1))
            });
            // See `install_session_render_sources`: the sky pass itself, from the
            // GPU handles this path already has locally (`self.gpu`/`self.target`
            // are not set until the end of this function).
            if !render.has_sky()
                && let Some(sky) = crate::resources::load_sky(gpu.device(), gpu.queue(), format)
            {
                render.install_sky(sky);
            }
            // See `install_session_render_sources`: the overlay pass, from the
            // same local GPU handles.
            if !render.has_screen_effects()
                && let Some(fx) =
                    crate::resources::load_screen_effects(gpu.device(), gpu.queue(), format)
            {
                render.install_screen_effects(fx);
            }
            // See `install_session_render_sources`: the rain/snow pass, from the
            // same local GPU handles.
            if !render.has_weather()
                && let Some(textures) = crate::resources::load_weather_textures()
            {
                render.install_weather(gpu.device(), gpu.queue(), format, &textures);
            }
        }
        // No target requested: stay on `Screen::MainMenu`, which `UiState::new`
        // already put us on. Nothing else to do.

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.target = Some(target);
        self.render = Some(render);
        // Now that `self.render` exists and `attach_net` has already run above,
        // the outline source can be installed on this path too.
        self.install_outline_source();
        // Debug lines need no connection at all (see the method doc), so this
        // is the one call that actually matters — the two above are just
        // keeping the three connect paths uniform.
        self.install_debug_lines_source();
        self.hud = Some(hud);
        self.effects = Some(effects);
        self.container = Some(container);
        self.menu = Some(menu);
        // Grab only if the chosen screen wants it (menus and loading: no).
        self.set_grab(self.ui.wants_cursor_grab());
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let (Some(gpu), Some(target), Some(render)) = (
                    self.gpu.as_ref(),
                    self.target.as_mut(),
                    self.render.as_mut(),
                ) {
                    target.resize(gpu.device(), size.width, size.height);
                    render.resize(gpu.device(), size.width, size.height);
                }
            }
            WindowEvent::Focused(false) => {
                // Losing focus pauses (and releases the pointer) so we don't
                // keep grabbing the mouse of a backgrounded window. The *world*
                // is not paused by this: `Screen::Paused` is local UI state and
                // the sim keeps ticking (see `FramePacer`), which is what keeps
                // keep-alives and movement flowing to the server.
                self.ui.pause();
                self.set_grab(false);
                self.pacer.set_focused(false);
            }
            WindowEvent::Focused(true) => {
                // Presentation resumes at full rate. The pointer is *not*
                // re-grabbed here — the player clicks to resume, as before.
                self.pacer.set_focused(true);
            }
            WindowEvent::Occluded(occluded) => {
                // Fully covered or minimised: there is nothing on screen to
                // update and acquiring a drawable is what stalls, so drop
                // presentation entirely while continuing to tick.
                self.pacer.set_occluded(occluded);
            }
            // Hovering a menu row highlights it, so the mouse and the keyboard
            // drive one selection rather than two. `Screen::Paused` shares this
            // arm too even though it is not `owns_frame` — see `menu_row_at`'s
            // doc — because it has its own row navigation to hover just like
            // every screen this renderer owns.
            WindowEvent::CursorMoved { position, .. }
                if crate::menu::render::owns_frame(self.ui.screen())
                    || self.ui.is_paused()
                    || self.ui.is_death() =>
            {
                self.cursor = (position.x as f32, position.y as f32);
                if let Some(row) = self.menu_row_at(self.cursor.0, self.cursor.1) {
                    self.nav.hover(&self.ui, row);
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if crate::menu::render::owns_frame(self.ui.screen())
                    || self.ui.is_paused()
                    || self.ui.is_death() =>
            {
                if state == ElementState::Pressed {
                    // Issue #15's other capture half: a mouse-button rebind
                    // (vanilla defaults `key.attack` to the left button,
                    // `key.pickItem` to the middle one — real cases, not
                    // hypothetical) needs *any* button, not only Left, and must
                    // run before the "click acts on the row under the cursor"
                    // branch below — otherwise a capture would immediately
                    // consume its own confirming click as a hover-row
                    // activation instead of finishing the rebind.
                    if self.nav.awaiting_key_capture() {
                        self.nav.capture_binding(Binding::Mouse(button));
                    } else if button == MouseButton::Left {
                        // Only a click *on a row* activates: clicking the backdrop
                        // must not confirm whatever happens to be highlighted.
                        //
                        // `MenuNav::click` and not `hover` + `MenuKey::Enter`: that
                        // pair is still what happens on every screen with a single
                        // row cursor and a single meaning of Enter, and it was wrong
                        // on the settings screen, which had no cursor and gave each
                        // control its own key. There, a click on the GUI SCALE row
                        // arrived as `Enter` and therefore as "toggle View Bobbing" —
                        // issue #391, where the whole bob chain was working and the
                        // option had been silently persisted off by a click on an
                        // unrelated row. Issue #55 gave that screen 135 controls and
                        // a real cursor, so a click now resolves its row to that
                        // row's own control; `MenuNav::click`'s doc has the history.
                        if let Some(row) = self.menu_row_at(self.cursor.0, self.cursor.1) {
                            let action = self.nav.click(&mut self.ui, row);
                            self.apply_menu_action(action);
                        }
                    }
                }
                // Every `owns_frame` action handles its own grab (each of them
                // either stays on a menu screen, which never grabs, or moves to
                // Playing through a path that already calls `set_grab`).
                // `PauseButton::BackToGame` does not — `handle_menu_key` only
                // calls `MenuNav::key`, which flips `UiState` to `Playing` and
                // returns, with nothing here to notice. Without this a click on
                // Back to Game resumes play with the pointer still released:
                // visible but unusable.
                let want = self.ui.wants_cursor_grab();
                if want != self.grabbed {
                    self.set_grab(want);
                }
            }
            // Track the cursor and, mid-drag, the slots it paints while a
            // container screen is up. This is a separate arm from the menu one
            // above because `Screen::Container` is not `owns_frame` — the
            // container overlay draws over the world, it does not replace it.
            WindowEvent::CursorMoved { position, .. } if self.ui.is_container_open() => {
                self.cursor = (position.x as f32, position.y as f32);
                if self.menu_input.is_dragging() {
                    if let (Some(menu), Some((w, h))) = (
                        self.active_container_menu(),
                        self.target.as_ref().map(RenderTarget::size),
                    ) {
                        let hit = hit_test_with_scale(
                            &menu,
                            self.nav.gui_scale(),
                            w,
                            h,
                            self.cursor.0,
                            self.cursor.1,
                        );
                        // `&menu` supplies the cursor stack and the slot rules
                        // vanilla's `shouldAddSlotToQuickCraft` gate needs — see
                        // `MenuInput::dragged`, and issue #378 part 1 for what an
                        // unfiltered paint set costs.
                        self.menu_input.dragged(hit, &menu);
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } if self.ui.is_container_open() => {
                if let Some(menu_button) = menu_button_for(button)
                    && let (Some(menu), Some((w, h))) = (
                        self.active_container_menu(),
                        self.target.as_ref().map(RenderTarget::size),
                    )
                {
                    // The recipe-book panel gets first refusal on the click
                    // (issue #163). It overlaps the main panel's left edge at
                    // narrow canvases by `container.rs`'s documented design, so
                    // testing it *after* the slot layout would make its own
                    // widgets unclickable there. Only a press is offered: a
                    // release landing on the panel must still reach
                    // `MenuInput::release` so an in-flight drag that started on
                    // a real slot can terminate.
                    // Deliberately not an early `return`: the tail of
                    // `window_event` latches `quit_requested`, and returning
                    // from here would skip it.
                    let consumed_by_recipe_panel = matches!(state, ElementState::Pressed)
                        && menu_button == MenuButton::Left
                        && self.handle_recipe_panel_click(&menu, w, h);
                    if !consumed_by_recipe_panel {
                        let hit = hit_test_with_scale(
                            &menu,
                            self.nav.gui_scale(),
                            w,
                            h,
                            self.cursor.0,
                            self.cursor.1,
                        );
                        let ctx = MenuContext {
                            cursor_loaded: menu.carried().is_some(),
                            // No game-mode plumbing exists on `Sim` to source this
                            // from yet — see the report on this change.
                            creative: false,
                        };
                        let clicks = match state {
                            ElementState::Pressed => {
                                let now = Instant::now();
                                let is_repeat = menu_button == MenuButton::Left
                                    && self.last_menu_click.is_some_and(|t| {
                                        now.duration_since(t) < DOUBLE_CLICK_WINDOW
                                    });
                                self.last_menu_click = Some(now);
                                self.menu_input
                                    .press(hit, menu_button, self.shift_held, ctx, is_repeat, &menu)
                            }
                            ElementState::Released => {
                                self.menu_input
                                    .release(hit, menu_button, self.shift_held, ctx, &menu)
                            }
                        };
                        for click in clicks {
                            self.send_menu_click(click);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // `Screen::Paused` no longer reaches this catch-all at all — the
                // `owns_frame(...) || self.ui.is_paused()` arm above now handles
                // every click while paused (hover + activate the highlighted
                // pause-menu row, including Back to Game via `MenuKey::Enter`).
                if self.grabbed {
                    // `key.attack` mines (hold-to-mine on live; one-shot break on
                    // demo) and `key.use` uses/places against the targeted face.
                    // Both default to a mouse button — left and right
                    // respectively — which is exactly why `Binding` has to be
                    // able to hold a mouse button and not just a key.
                    match (mouse_action_for(&self.keybinds, button), state) {
                        (Some(InputAction::Attack), ElementState::Pressed) => {
                            self.sim.begin_attack();
                        }
                        (Some(InputAction::Attack), ElementState::Released) => {
                            self.sim.end_attack();
                        }
                        (Some(InputAction::Use), ElementState::Pressed) => {
                            self.sim.use_item();
                        }
                        (Some(InputAction::Use), ElementState::Released) => {
                            self.sim.end_use();
                        }
                        // Middle-click by default (`Options.java:669` binds
                        // `key.pickItem` to `Type.MOUSE, 2`), so unlike
                        // attack/use this is the *primary* route rather than the
                        // rebound one. Press-only: `pickBlockOrEntity` is a
                        // one-shot with no release edge.
                        (Some(InputAction::PickItem), ElementState::Pressed) => {
                            self.sim.pick_block_or_entity(self.ctrl_held);
                        }
                        // A movement action bound to a mouse button still drives
                        // the controller, on both edges.
                        (Some(action), _) => {
                            if let Some(movement) = action.movement() {
                                let held = state == ElementState::Pressed;
                                self.sim.input_mut(|i| i.set(movement, held));
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Scroll cycles the hotbar (down = right, like vanilla) only
            // during active play; menus and the chat prompt ignore it. The
            // step is scaled by `mouseWheelSensitivity` (issue #203) through
            // the same fractional accumulator vanilla's `ScrollWheelHandler`
            // uses, so sensitivity below 1.0 can take more than one notch to
            // move a slot and sensitivity above 1.0 can cross several in one
            // notch — not just a threshold on the existing ±1 step.
            WindowEvent::MouseWheel { delta, .. } if self.ui.accepts_gameplay_input() => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y,
                };
                let scaled = dy * f64::from(self.nav.mouse_wheel_sensitivity());
                let step = accumulate_scroll(&mut self.scroll_accum, scaled);
                if step != 0 {
                    self.sim.cycle_slot(-step);
                }
            }
            // The multiplayer server list (issues #402, #445): the notch count
            // goes through **verbatim**, as vanilla's `scrollY`, and
            // `MenuNav::scroll_server_list` turns it into
            // `scrollY * scrollRate()` pixels — 18 px for a 36 px row
            // (`AbstractScrollArea.java:34`, `AbstractSelectionList.java:44`).
            //
            // **This used to collapse `dy` to `-1`/`0`/`+1` rows**, and that was
            // the owner's bug report: a list that jumps a whole 36 px entry per
            // notch instead of scrolling. The information was destroyed here, at
            // the input, not in the geometry — a row index cannot represent the
            // half-entry position vanilla lands on, so no amount of work
            // downstream could have recovered it. Passing the real `dy` also
            // makes a trackpad's fractional `PixelDelta` move proportionally
            // rather than snapping to a whole row.
            //
            // Deliberately not run through `accumulate_scroll`, which exists for
            // the hotbar's sub-notch *quantization* — the opposite problem:
            // `cycle_slot` takes a discrete slot step, so it has to accumulate
            // fractions until one whole step is due. A pixel offset needs no
            // accumulator, because a fraction of a notch is already a meaningful
            // number of pixels.
            //
            // Needs the *real* canvas height, which this handler has via
            // `RenderTarget::size` and `gui_scale`, unlike keyboard
            // scroll-into-view which uses the canvas-independent window estimate
            // (see `MenuNav::scroll_server_list`'s doc).
            WindowEvent::MouseWheel { delta, .. }
                if self.ui.screen() == crate::menu::Screen::ServerList =>
            {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y,
                };
                if dy != 0.0
                    && let Some((fb_w, fb_h)) = self.target.as_ref().map(RenderTarget::size)
                {
                    let (_, canvas_h) =
                        crate::menu::render::logical_canvas(self.nav.gui_scale(), fb_w, fb_h);
                    self.nav.scroll_server_list(dy as f32, canvas_h);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;

                // Tracked unconditionally (not gated on `accepts_gameplay_input`
                // like the movement bindings below): a container shift-click is a
                // `QuickMove`, not movement, and must still work while gameplay
                // input is not being accepted.
                //
                // **Deliberately still a literal key, and vanilla agrees**: it
                // checks `Screen.hasShiftDown()` — the raw modifier state — not
                // `options.keyShift`, so rebinding sneak does *not* move
                // shift-click. Same boundary as `menu_button_for`: container
                // gestures are UI chrome, not gameplay bindings. Both shifts
                // count, because this is asking "is a shift modifier down".
                if let PhysicalKey::Code(code) = event.physical_key
                    && matches!(code, KeyCode::ShiftLeft | KeyCode::ShiftRight)
                {
                    self.shift_held = pressed;
                }
                // Same tracking, for Control — `resolve_key`'s `ctrl` parameter.
                // Deliberately a running flag rather than read off this event:
                // `key.drop` is a different physical key from Control, so the
                // modifier's state has to outlive the keypress that changed it.
                if let PhysicalKey::Code(code) = event.physical_key
                    && matches!(code, KeyCode::ControlLeft | KeyCode::ControlRight)
                {
                    self.ctrl_held = pressed;
                }

                // Resolve *what this key means* before touching any state, then
                // perform the one side effect it names. The precedence lives in
                // [`resolve_key`] — a pure function, so the swallowing order can
                // be unit-tested without a window (see its docs and the tests at
                // the bottom of this file). This match is only the effects half.
                let gate = KeyGate {
                    menu: crate::menu::render::owns_frame(self.ui.screen())
                        || self.ui.is_paused()
                        || self.ui.is_death(),
                    chat_open: self.ui.is_chat_open(),
                    // `active_container_menu`, **not** `ui.is_container_open()`.
                    // That flag only tracks the *locally* opened player inventory;
                    // a server-opened menu (crafting table, chest, furnace) lives
                    // in `sim.open_menu()` and leaves it `false`. Reading the flag
                    // meant the swallow arm never fired for a server menu, so the
                    // inventory binding could not close a crafting table and every
                    // gameplay key stayed live behind it. This is the same
                    // predicate `redraw` draws from, so hit-testing, drawing and
                    // key dispatch cannot disagree about what is on screen.
                    container_open: self.active_container_menu().is_some(),
                    gameplay: self.ui.accepts_gameplay_input(),
                };
                let code = match event.physical_key {
                    PhysicalKey::Code(code) => Some(code),
                    _ => None,
                };
                // Resolved into a local first so the immutable borrow of
                // `self.keybinds` ends before the `&mut self` calls below.
                let outcome = resolve_key(&self.keybinds, gate, code, pressed, self.ctrl_held);
                match outcome {
                    Some(KeyOutcome::Menu) => {
                        // Issue #15's last hop: a bind button mid-capture needs the
                        // *next raw key*, not `menu_key_for`'s translation —
                        // `menu_key_for` silently drops any physical key with no
                        // printable `text` (F-keys, modifiers, arrows other than
                        // Up/Down), which is exactly the common rebind case
                        // (`docs/keybindings.md`'s "Wiring the Controls menu").
                        // Checked *before* calling `menu_key_for` at all, not only
                        // when it returns `None`: a capture target can be a
                        // printable key too, and `menu_key_for` would otherwise
                        // consume it as `MenuKey::Char` first.
                        if pressed && self.nav.awaiting_key_capture() {
                            match capture_key_for(event.physical_key) {
                                Some(CaptureKey::Cancel) => {
                                    self.handle_menu_key(MenuKey::Escape);
                                }
                                Some(CaptureKey::Bind(code)) => {
                                    self.nav.capture_binding(Binding::Key(code));
                                }
                                None => {}
                            }
                        } else if pressed
                            && let Some(key) = Self::menu_key_for(&event)
                        {
                            self.handle_menu_key(key);
                            // Entering the world grabs; leaving it releases.
                            let want = self.ui.wants_cursor_grab();
                            if want != self.grabbed {
                                self.set_grab(want);
                            }
                        }
                    }
                    Some(KeyOutcome::Chat) => {
                        if pressed {
                            self.handle_chat_key(&event);
                        }
                    }
                    Some(KeyOutcome::Pause) => {
                        // Escape on a container screen **closes the container and
                        // returns to gameplay** — it does not open the pause menu.
                        // That is `Screen.onClose()` in vanilla, and it is why this
                        // is an `else` rather than a close followed by `on_escape`:
                        // the old form paused *as well*, leaving the pause menu
                        // drawn over a menu that was still open server-side.
                        //
                        // Also note it must clear both halves. `close_open_menu`
                        // only releases the *server* menu; `close_container` clears
                        // the local inventory flag. Whichever one was showing, the
                        // other is already false and clearing it is a no-op.
                        if self.active_container_menu().is_some() {
                            self.sim.close_open_menu();
                            self.ui.close_container();
                        } else {
                            // Context-sensitive: Playing↔Paused, Error→menu, etc.
                            self.ui.on_escape();
                        }
                        self.set_grab(self.ui.wants_cursor_grab());
                    }
                    Some(KeyOutcome::CloseContainer) => {
                        self.sim.close_open_menu();
                        self.ui.close_container();
                        self.set_grab(self.ui.wants_cursor_grab());
                    }
                    Some(KeyOutcome::ToggleDebugOverlay) => {
                        // Toggle the debug instrument (§S4). Unlike older
                        // vanilla, 26.2 makes this a real `KeyMapping`, so it
                        // belongs in the table — see `keybinds`' module docs.
                        self.show_debug = !self.show_debug;
                    }
                    Some(KeyOutcome::PlayerList(held)) => self.tab_held = held,
                    Some(KeyOutcome::OpenChat { command }) => {
                        // Release held movement so we don't walk while typing.
                        self.sim.input_mut(InputState::release_all);
                        let _ = self.chat_input.take();
                        if command {
                            self.chat_input.push_char('/');
                        }
                        self.ui.open_chat();
                        self.tab_held = false;
                        self.set_grab(false);
                    }
                    Some(KeyOutcome::OpenContainer) => {
                        self.sim.input_mut(InputState::release_all);
                        self.ui.open_container();
                        self.tab_held = false;
                        self.set_grab(false);
                    }
                    // Vanilla's own third-/first-person toggle.
                    Some(KeyOutcome::TogglePerspective) => self.sim.toggle_third_person(),
                    Some(KeyOutcome::SelectSlot(slot)) => self.sim.select_slot(slot),
                    Some(KeyOutcome::ContainerSwap { button }) => {
                        self.send_container_swap(button);
                    }
                    Some(KeyOutcome::ContainerDrop { ctrl }) => {
                        self.send_container_drop(ctrl);
                    }
                    Some(KeyOutcome::ContainerPickItem) => self.send_container_pick_item(),
                    Some(KeyOutcome::Drop { ctrl }) => self.send_drop_selected(ctrl),
                    Some(KeyOutcome::PickItem { ctrl }) => self.sim.pick_block_or_entity(ctrl),
                    // The *other* off-hand route (#385): no screen, no slot, a
                    // bare `ServerboundPlayerAction`. Sent straight through
                    // `NetClient` rather than queued into `ActionQueue`, which is
                    // the sanctioned shape for a per-frame input-driven action —
                    // see `interact.rs`' module doc on why `end_attack`,
                    // `use_item_live` and `send_chat` do the same.
                    Some(KeyOutcome::SwapOffhand) => self.send_offhand_swap(),
                    Some(KeyOutcome::Attack(true)) => self.sim.begin_attack(),
                    Some(KeyOutcome::Attack(false)) => self.sim.end_attack(),
                    Some(KeyOutcome::Use(true)) => self.sim.use_item(),
                    Some(KeyOutcome::Use(false)) => self.sim.end_use(),
                    Some(KeyOutcome::Movement(action, held)) => {
                        self.sim.input_mut(|i| i.set(action, held));
                    }
                    // Either nothing is bound to this key, or a screen above
                    // swallowed it. Both are "do nothing", deliberately.
                    None => {}
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }

        // Clean shutdown path: any handler may latch a quit request.
        if self.ui.quit_requested() {
            event_loop.exit();
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if let DeviceEvent::MouseMotion { delta } = event
            && self.ui.is_playing()
            && self.grabbed
        {
            self.sim
                .input_mut(|i| i.add_mouse(delta.0 as f32, delta.1 as f32));
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        // Spin while focused (vsync paces the loop); otherwise sleep in short
        // `BACKGROUND_POLL` slices so a backgrounded window stops burning a core
        // yet still wakes far more often than the 20 Hz tick needs.
        event_loop.set_control_flow(self.pacer.control_flow(Instant::now()));
    }
}

fn run_windowed(config: Config) -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = WindowApp::new(config);
    event_loop.run_app(&mut app)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Headless: render one frame offscreen, save a PPM, print stats.
// ---------------------------------------------------------------------------

fn run_headless(config: Config) -> anyhow::Result<()> {
    let ctx = GpuContext::new_headless_blocking()
        .map_err(|e| anyhow::anyhow!("headless GPU bring-up failed: {e}"))?;
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (1280u32, 720u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    let render_distance = config.render_distance;
    // The offline evidence path, and the one place the demo world still exists:
    // this renders a single frame with no server and *fails* below 5% terrain
    // coverage, so it needs a world that does not come from a connection. The
    // interactive client has none — see `Sim::new`. (`Sim::new` would delegate
    // here anyway on `Mode::Headless`; spelled out so the dependency is visible
    // at the call site rather than hidden in a mode check.)
    let mut sim = Sim::with_demo_world(config);
    let mut render = RenderState::new(device, queue, format, w, h, sim.vanilla_atlas());
    render.set_fog(sky_fog(render_distance), render_distance);

    // Mesh everything and upload.
    let meshes = sim.drain_all_meshes();
    let mut meshed_quads = 0usize;
    for m in &meshes {
        meshed_quads += m.mesh.quad_count();
        render.upload_section(device, queue, m.key, &m.mesh);
    }

    // Let the player settle onto the ground so the camera sits at a sane height.
    for _ in 0..40 {
        sim.step(1.0 / 20.0);
    }

    let camera = sim.camera(w as f32 / h as f32);
    // Outline the block directly under the settled player, as a visible probe.
    let outline = {
        let p = sim.player().position;
        Some([
            p.x.floor() as i32,
            p.y.floor() as i32 - 1,
            p.z.floor() as i32,
        ])
    };
    let start = Instant::now();
    let frame = target
        .acquire()
        .map_err(|e: TargetError| anyhow::anyhow!("headless acquire failed: {e}"))?;
    let entity_draws = sim.entity_draws();
    let _ = sim.extract_particles(&camera);
    render.prepare_particles(device, queue, &sim.particle_instances(), &camera);
    render.update_animation(queue, sim.tick_count());
    let stats = render.render(device, queue, frame.view(), &camera, outline, &entity_draws);
    let pixels = target.read_texels(device, queue);
    let frame_ms = start.elapsed().as_secs_f64() * 1000.0;

    // Coverage: fraction of pixels that clearly aren't the sky clear colour.
    //
    // This target is *not* an sRGB-format texture, so these bytes are the
    // shader's linear output scaled straight to 0..255 with no gamma encode
    // (unlike the swapchain, which is sRGB and would encode them). That's
    // `SKY_COLOR * 255` rounded, not the on-screen sky colour — read
    // `gpu::SKY_COLOR`'s doc comment before touching this to keep the two in
    // sync.
    let sky = [62i32, 118, 211];
    let mut terrain_px = 0usize;
    for px in pixels.chunks_exact(4) {
        let d = (i32::from(px[0]) - sky[0]).abs()
            + (i32::from(px[1]) - sky[1]).abs()
            + (i32::from(px[2]) - sky[2]).abs();
        if d > 60 {
            terrain_px += 1;
        }
    }
    let coverage = terrain_px as f64 / (w * h) as f64 * 100.0;

    let out = "lodestone-frame.ppm";
    write_ppm(out, w, h, &pixels)?;

    sim.stats.section_count = stats.sections_drawn;
    sim.stats.quads = stats.total_quads;
    sim.stats.vram_bytes = stats.vram_bytes;
    sim.stats.frame_ms = frame_ms as f32;

    println!("=== lodestone headless render ===");
    println!("world chunks      = {}", sim.chunk_count());
    println!("sections meshed   = {}", meshes.len());
    println!("sections drawn    = {}", stats.sections_drawn);
    println!("quads (meshed)    = {meshed_quads}");
    println!("quads (drawn)     = {}", stats.total_quads);
    println!("draw calls        = {}", stats.draw_calls);
    println!("mesh VRAM (bytes) = {}", stats.vram_bytes);
    println!("terrain coverage  = {coverage:.1}%");
    println!("frame time (ms)   = {frame_ms:.3}");
    println!("saved frame       = {out}");
    println!("{}", sim.stats.one_line());

    if coverage < 5.0 {
        anyhow::bail!("rendered frame shows <5% terrain ({coverage:.1}%) — nothing visible");
    }
    Ok(())
}

/// Write a binary (P6) PPM — no image-crate dependency needed for evidence.
fn write_ppm(path: &str, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut buf = Vec::with_capacity((w * h * 3 + 32) as usize);
    buf.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for px in rgba.chunks_exact(4) {
        buf.extend_from_slice(&px[..3]);
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&buf)
}

// ---------------------------------------------------------------------------
// Connect: stream live events for a bounded time, no GPU.
// ---------------------------------------------------------------------------

fn run_connect(config: Config) -> anyhow::Result<()> {
    println!(
        "connecting to {}:{} (protocol {}) for {}s…",
        config.host,
        config.port,
        config.protocol,
        config.connect_for.as_secs()
    );
    // `None`: `--connect` is the event-stream diagnostic. It has no `Sim`, no
    // renderer and no `World` of its own, so the client mints one — there is
    // nothing for it to be shared *with*.
    let net = NetClient::connect(config.host.clone(), config.port, config.protocol, None);
    let deadline = Instant::now() + config.connect_for;
    let mut seen = 0usize;

    while Instant::now() < deadline {
        for update in net.poll() {
            seen += 1;
            println!("[net] {update:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("streamed {seen} update(s); exiting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Java's `String.hashCode()`, computed by hand from the well-known
    /// public algorithm — an oracle that lives outside this file, per
    /// `CLAUDE.md`'s evidence standard. `"hello"`: `h = 0`, then
    /// `104, 3325, 103183, 3198781, 99162322` after `'h','e','l','l','o'`
    /// (`h = h*31 + c` each step) — a commonly-cited constant, reproduced
    /// here from the formula rather than trusted from memory alone.
    #[test]
    fn java_string_hash_code_matches_the_known_constant() {
        assert_eq!(java_string_hash_code("hello"), 99_162_322);
        assert_eq!(java_string_hash_code(""), 0);
    }

    /// **Issue #47's queued patch, exercised through production code.**
    ///
    /// The command-block screen's Done button computed a fully-tested payload
    /// and **dropped it on the floor** — `activate_command_block_row`'s `Done`
    /// arm bound it to `let _submit` because `MenuAction` had no variant to
    /// carry it and `app.rs` had no arm to consume it. This drives the whole
    /// chain rather than re-asserting either half: the real
    /// [`crate::menu::nav::MenuNav::key`] on the real `Done` row produces the
    /// action, the real [`WindowApp::apply_menu_action`] consumes it, and the
    /// `ClientAction` is read off the socket seam a live session would write to.
    ///
    /// **The expected value is predicted, not round-tripped.** Every field is
    /// stated from the edits made below (a typed command, a cycled mode, two
    /// toggles) rather than from `to_submit()`'s own output, so a payload that
    /// dropped or transposed a field fails here — `decode(encode(x)) == x` would
    /// not.
    ///
    /// **Negative control, executed:** deleting the
    /// `MenuAction::SetCommandBlock` arm from `apply_menu_action` (replacing it
    /// with `{}`) makes this fail at `try_recv`, `Err(Empty)` — nothing reaches
    /// the socket. That is the island this patch closes, and it is invisible to
    /// `cargo check`: an arm that matches and does nothing compiles perfectly.
    ///
    /// Reachability is a **separate** and still-open matter: nothing opens this
    /// screen from a real interaction (no command-block block-entity NBT decode,
    /// no `interact.rs` trigger), which is issue #442. This test opens it
    /// directly, exactly as `MenuNav::open_command_block` is written to allow.
    #[test]
    fn the_command_block_done_button_sends_a_real_set_command_block_action() {
        use crate::menu::command_block::{CommandBlockOpen, CommandBlockRow, COMMAND_BLOCK_ROWS};
        use crate::menu::nav::MenuKey;
        use lodestone_model::{BlockPos, CommandBlockMode};

        let mut app = WindowApp::new(Config {
            mode: Mode::Headless,
            ..Config::default()
        });
        let (net, actions, _feed) = NetClient::loopback_with_feed();
        app.sim.attach_net(net);

        // `MenuNav::open_command_block` and `UiState::open_command_block` both
        // guard on `Screen::Playing` (a command block is opened from the world,
        // not from a menu), so reach that first — `enter_dev_world` is the
        // headless entry point's own route to it.
        app.ui.enter_dev_world();

        // Open the screen on a specific block with known stored contents, then
        // *edit* it — an unedited screen would let a `to_submit` that returned
        // `CommandBlockOpen`'s values verbatim pass.
        let pos = BlockPos::new(12, -7, 340);
        app.nav.open_command_block(
            &mut app.ui,
            CommandBlockOpen {
                pos,
                command: "say hi".into(),
                track_output: false,
                previous_output: None,
                mode: CommandBlockMode::Redstone,
                conditional: false,
                automatic: false,
            },
        );
        assert_eq!(
            app.ui.screen(),
            crate::menu::Screen::CommandBlockEdit,
            "precondition: the screen must actually be open, or every key below \
             lands somewhere else"
        );

        // Type into the command field, through the real key path.
        for ch in "!".chars() {
            let action = app.nav.key(&mut app.ui, MenuKey::Char(ch));
            app.apply_menu_action(action);
        }
        // Cycle the mode once (Redstone -> its successor) and flip two toggles,
        // each by activating that row the way a click or Enter does.
        for row in [
            CommandBlockRow::Mode,
            CommandBlockRow::TrackOutput,
            CommandBlockRow::Conditional,
        ] {
            let idx = COMMAND_BLOCK_ROWS
                .iter()
                .position(|r| *r == row)
                .expect("every CommandBlockRow is in COMMAND_BLOCK_ROWS");
            let action = app.nav.click(&mut app.ui, idx);
            app.apply_menu_action(action);
        }

        // Read the mode the cycle actually produced from the screen itself, so
        // this test does not hardcode `next_mode`'s table (which has its own
        // gate in `command_block.rs`) — but every *other* field is predicted.
        let expected_mode = app
            .nav
            .command_block()
            .expect("the screen is still open")
            .mode;
        assert_ne!(
            expected_mode,
            CommandBlockMode::Redstone,
            "precondition: cycling the mode must have changed it, or this field \
             is not under test"
        );

        // Nothing may have reached the socket yet — the control for the
        // assertion below, and it is not vacuous: the toggle rows above all
        // return `MenuAction::None`, so a `_ =>` arm that sent something for
        // every action would be caught here.
        assert!(
            actions.try_recv().is_err(),
            "no action may be sent before Done is pressed"
        );

        // Press Done.
        let done = COMMAND_BLOCK_ROWS
            .iter()
            .position(|r| *r == CommandBlockRow::Done)
            .expect("Done is a CommandBlockRow");
        let action = app.nav.click(&mut app.ui, done);
        assert!(
            matches!(action, crate::menu::nav::MenuAction::SetCommandBlock(_)),
            "the Done row must produce the action, not swallow it: {action:?}"
        );
        app.apply_menu_action(action);

        // And it reached the wire, with exactly the edited payload.
        let sent = actions
            .try_recv()
            .expect("Done must put a ClientAction on the outbound seam");
        assert_eq!(
            sent,
            lodestone_model::ClientAction::SetCommandBlock {
                pos,
                command: "say hi!".into(),
                mode: expected_mode,
                track_output: true,
                conditional: true,
                automatic: false,
            },
            "the action must carry the screen's edits, field for field"
        );

        // Vanilla closes after sending (`CommandBlockEditScreen.java:111-114`).
        assert_ne!(
            app.ui.screen(),
            crate::menu::Screen::CommandBlockEdit,
            "Done sends and then closes"
        );
    }

    /// `WorldOptions.parseSeed` (issue #190): a valid `i64` literal is used
    /// verbatim (vanilla tries `Long.parseLong` first), whitespace is
    /// trimmed, and non-numeric text falls back to the Java hash — not a new
    /// rule, just `parse_seed` calling straight through to the constant test
    /// above.
    #[test]
    fn parse_seed_follows_vanillas_own_rule() {
        assert_eq!(parse_seed("12345"), 12345);
        assert_eq!(parse_seed("-42"), -42);
        assert_eq!(parse_seed("  42  "), 42, "vanilla trims before parsing");
        assert_eq!(
            parse_seed("hello"),
            99_162_322,
            "non-numeric text must hash exactly like Java's own String.hashCode, \
             not this crate's own notion of a hash"
        );
    }

    /// An empty seed means "random" (`WorldOptions.defaultWithRandomSeed`) —
    /// asserted by absence of a fixed answer, the only honest assertion for
    /// "random": two draws must not collide (astronomically unlikely for a
    /// real `i64` random source, impossible for a constant-returning bug).
    #[test]
    fn empty_seed_is_random_not_a_fixed_fallback() {
        let a = parse_seed("");
        let b = parse_seed("   ");
        assert_ne!(
            a, b,
            "two empty-seed draws must not produce the same i64 — a constant \
             here would silently make every \"random\" world identical"
        );
    }

    /// Issue #190's queued patch, driven end to end: two different
    /// `WorldCreationConfig`s (the exact type `Screen::CreateWorld` collects)
    /// resolved through the *production* `resolve_launch_seed` must generate
    /// **different real terrain** at the same coordinate — not merely
    /// different `i64`s, which `parse_seed`'s own tests above already cover
    /// and which would be the isolated-unit species of this gate. And the
    /// same config must reproduce identical terrain.
    ///
    /// `lodestone_server::overworld_generator` is exactly what
    /// `crate::net::run`'s `Origin::Integrated` arm calls with this
    /// function's resolved seed (`net.rs:1354` at the time of writing) — so
    /// this proves the seed that would reach the wire, not a stand-in.
    #[test]
    fn resolved_seeds_from_different_world_creation_configs_generate_different_terrain() {
        let config_a = crate::menu::create_world::WorldCreationConfig {
            seed: "100".to_string(),
            ..Default::default()
        };
        let config_b = crate::menu::create_world::WorldCreationConfig {
            seed: "999999".to_string(),
            ..Default::default()
        };

        let seed_a = resolve_launch_seed(Some(&config_a));
        let seed_b = resolve_launch_seed(Some(&config_b));
        assert_eq!(seed_a, 100);
        assert_eq!(seed_b, 999_999);

        let column_a = lodestone_server::overworld_generator(seed_a).column(0, 0);
        let column_b = lodestone_server::overworld_generator(seed_b).column(0, 0);

        let mut differences = 0usize;
        for lz in 0..16usize {
            for lx in 0..16usize {
                for y in (column_a.min_y()..column_a.min_y() + column_a.height()).step_by(4) {
                    if column_a.block_state(lx, y, lz) != column_b.block_state(lx, y, lz) {
                        differences += 1;
                    }
                }
            }
        }
        assert!(
            differences > 0,
            "two different entered seeds must generate different terrain \
             somewhere in the same column — the config's seed is reaching \
             nowhere if this is 0"
        );

        // Reproducibility: the same config, resolved and generated twice,
        // must be byte-identical — `overworld_generator` is a pure function
        // of its seed, and this is the exact call `net.rs::run` makes, called
        // twice rather than reimplemented.
        let seed_a_again = resolve_launch_seed(Some(&config_a));
        assert_eq!(seed_a_again, seed_a, "the same typed seed must resolve identically");
        let column_a_again = lodestone_server::overworld_generator(seed_a_again).column(0, 0);
        for lz in 0..16usize {
            for lx in 0..16usize {
                for y in column_a.min_y()..column_a.min_y() + column_a.height() {
                    assert_eq!(
                        column_a.block_state(lx, y, lz),
                        column_a_again.block_state(lx, y, lz),
                        "the same seed must reproduce identical terrain at ({lx},{y},{lz})"
                    );
                }
            }
        }
    }

    /// `None` (`Screen::WorldSelect`'s Play Selected World) must still resolve
    /// to the bundled world's own seed — the pre-#190 behaviour, unchanged.
    #[test]
    fn no_config_resolves_to_the_bundled_worlds_seed() {
        assert_eq!(
            resolve_launch_seed(None),
            crate::menu::world_select::BUNDLED_WORLD.seed
        );
    }

    /// A cheap sim: headless mode with the smallest render distance that still
    /// generates real terrain, so physics ticks do real collision work.
    fn pacing_sim() -> Sim {
        // Explicitly the demo-world fixture: this needs real terrain so the
        // physics ticks do collision work, and the client `Sim::new` has none.
        Sim::with_demo_world(Config {
            mode: Mode::Headless,
            render_distance: 2,
            ..Config::default()
        })
    }

    /// Ticks a real `Sim` executes when advanced by `dt` in one call.
    fn ticks_for(sim: &mut Sim, dt: f64) -> u64 {
        let before = sim.tick_count();
        sim.step(dt);
        sim.tick_count() - before
    }

    /// Issue #203: at the vanilla default sensitivity (`1.0`), one wheel
    /// notch (`LineDelta` magnitude `1.0`) must move exactly one hotbar slot
    /// — the pre-#203 behaviour — so the sensitivity feature is provably a
    /// pure addition, not a regression of the common case.
    #[test]
    fn accumulate_scroll_moves_one_slot_per_notch_at_default_sensitivity() {
        let mut accum = 0.0;
        assert_eq!(accumulate_scroll(&mut accum, 1.0 * 1.0), 1);
        assert_eq!(accum, 0.0, "a whole-notch scroll must leave no carry");
        assert_eq!(accumulate_scroll(&mut accum, -1.0 * 1.0), -1);
    }

    /// A sensitivity below 1.0 must take more than one notch to move a slot
    /// — the exact scaled amount, not merely "less than at 1.0". At `0.25`,
    /// four notches of `1.0` each accumulate to exactly one slot, with the
    /// third notch still producing zero.
    #[test]
    fn accumulate_scroll_carries_a_fractional_remainder_at_low_sensitivity() {
        let mut accum = 0.0;
        let scaled = 1.0 * 0.25_f64;
        assert_eq!(accumulate_scroll(&mut accum, scaled), 0);
        assert_eq!(accumulate_scroll(&mut accum, scaled), 0);
        assert_eq!(accumulate_scroll(&mut accum, scaled), 0);
        assert!(
            (accum - 0.75).abs() < 1e-12,
            "three quarter-notches must carry exactly 0.75, not round or clamp: got {accum}"
        );
        assert_eq!(
            accumulate_scroll(&mut accum, scaled),
            1,
            "the fourth quarter-notch must complete the first slot"
        );
        assert!(accum.abs() < 1e-12, "the completed slot must consume the whole carry");
    }

    /// A sensitivity above 1.0 must cross more than one slot per notch —
    /// the exact scaled amount again, not a threshold on the existing ±1
    /// step. At `10.0`, one notch is 10 whole slots with no carry.
    #[test]
    fn accumulate_scroll_moves_several_slots_per_notch_at_high_sensitivity() {
        let mut accum = 0.0;
        assert_eq!(accumulate_scroll(&mut accum, 1.0 * 10.0), 10);
        assert_eq!(accum, 0.0);
    }

    /// A direction reversal must drop the old carry rather than fight it
    /// (`ScrollWheelHandler.java:14-16`): three-quarters of a slot built up
    /// scrolling one way must not partially cancel a fresh scroll the other
    /// way, or a player flicking back and forth would see scroll amounts
    /// depend on unrelated history.
    #[test]
    fn accumulate_scroll_resets_the_carry_on_direction_reversal() {
        let mut accum = 0.0;
        assert_eq!(accumulate_scroll(&mut accum, 0.75), 0);
        assert!((accum - 0.75).abs() < 1e-12);
        // Reversed direction: a naive `accum += scaled` would land at
        // `0.75 - 0.25 = 0.5`, still short of a slot. The reset makes this
        // scroll's own `-0.25` the entire story.
        assert_eq!(accumulate_scroll(&mut accum, -0.25), 0);
        assert!(
            (accum - -0.25).abs() < 1e-12,
            "the old positive carry must be discarded, not partially offset: got {accum}"
        );
    }

    /// Issue #61: the hotbar belongs to the world, not to active play.
    ///
    /// Oracle is vanilla, not our own reasoning — see `hud_follows_world`'s docs
    /// for the four source lines. The regression was one boolean
    /// (`self.ui.is_playing()`, *named* `crosshair`) gating both the reticle and
    /// the hotbar, so opening the pause menu or the inventory took the hotbar with
    /// it.
    #[test]
    fn the_hotbar_survives_every_screen_drawn_over_the_world() {
        use crate::menu::Screen;

        for screen in [
            Screen::Playing,
            Screen::Chat,
            Screen::Container,
            Screen::Paused,
            Screen::Death,
        ] {
            assert!(
                hud_follows_world(screen),
                "{screen:?} draws the world, so it must draw the world's hotbar"
            );
        }

        // -- negative control ------------------------------------------------
        // The predicate has to be able to say no, or the loop above is vacuous.
        // `Connecting` reaches the world render path (it is not an `owns_frame`
        // screen) but has no world yet; the menu screens never get here at all
        // because `draw_menu` returns first — asserted anyway so a future
        // `owns_frame` change cannot quietly turn this into `true` everywhere.
        for screen in [
            Screen::Connecting,
            Screen::MainMenu,
            Screen::ServerList,
            Screen::ServerEdit,
            Screen::Settings,
            Screen::Error,
        ] {
            assert!(
                !hud_follows_world(screen),
                "{screen:?} has no world on screen, so it must have no hotbar"
            );
        }
    }

    /// The two questions must not collapse back into one boolean. `Paused` is the
    /// screen that separates them: the crosshair goes, the hotbar stays.
    #[test]
    fn the_crosshair_and_the_hotbar_disagree_behind_a_screen() {
        let mut ui = UiState::new();
        ui.begin(SessionKind::Singleplayer);
        ui.session_ready();
        assert!(ui.is_playing(), "a ready session is in the world");
        assert!(hud_follows_world(ui.screen()));

        ui.pause();
        assert!(
            !ui.is_playing(),
            "the reticle's gate must go false behind the pause menu"
        );
        assert!(
            hud_follows_world(ui.screen()),
            "the hotbar's gate must stay true behind the pause menu"
        );
    }

    #[test]
    fn vanillas_cap_is_ten_ticks_of_real_time() {
        // Guards the constant against a silent edit. 10 ticks × 50 ms = 500 ms;
        // read from Minecraft.java:262 / :1176 (see `MAX_TICKS_PER_UPDATE`).
        assert_eq!(MAX_TICKS_PER_UPDATE, 10);
        assert!((MAX_CATCHUP_SECS - 0.5).abs() < 1e-12, "{MAX_CATCHUP_SECS}");
    }

    #[test]
    fn a_long_stall_is_clamped_not_replayed() {
        // The reported bug: tab out for a minute, tab back in, and the client
        // tries to run every tick it missed. Sixty seconds is 1200 ticks.
        let stall = Duration::from_secs(60);
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        let step = pacer.begin_frame(t0 + stall);

        assert!(
            (step.dt - MAX_CATCHUP_SECS).abs() < 1e-12,
            "a {stall:?} stall must be clamped to {MAX_CATCHUP_SECS}s, got {}",
            step.dt
        );

        // Drive a *real* sim with it and count the ticks that actually run.
        let mut sim = pacing_sim();
        let clamped = ticks_for(&mut sim, step.dt);
        assert!(
            clamped <= u64::from(MAX_TICKS_PER_UPDATE),
            "catch-up must never exceed vanilla's cap, got {clamped}"
        );

        // Measured: **10**. It used to be 5, because `Sim::step` applied its own,
        // tighter `dt.clamp(0.0, 0.25)` to the accumulator before the tick loop and
        // so silently halved this pacer's budget. That assertion said as much out
        // loud ("if this changed, reconcile the two caps") and this is the change
        // that reconciled them: §4.1(c) left one accumulator
        // (`lodestone_ecs::FrameClock`) on one policy
        // (`lodestone_ecs::MAX_CATCH_UP_SECS`), and the surviving number is
        // vanilla's ten — the only one of the two candidates with an external
        // oracle. See that constant's docs for the full argument.
        assert_eq!(
            clamped,
            u64::from(MAX_TICKS_PER_UPDATE),
            "one clamp now: `FrameClock::begin_frame` banks at most \
             {MAX_CATCHUP_SECS} s, so a maximal stall runs exactly vanilla's \
             {MAX_TICKS_PER_UPDATE} catch-up ticks"
        );
        // …and the shell's clamp *is* the ECS's, not a second one that happens to
        // agree. A copy that agreed today is how the five-vs-ten divergence
        // started.
        assert!(
            (MAX_CATCHUP_SECS - lodestone_ecs::MAX_CATCH_UP_SECS).abs() < 1e-12,
            "app.rs and lodestone-ecs must not carry two catch-up budgets"
        );

        // -- negative control ------------------------------------------------
        // Prove the detector fires: the same real `Sim`, driven the
        // *proportional* way the bug describes (one tick's worth of dt at a
        // time until the stall is consumed), executes the full 1200 ticks. If
        // `tick_count` could not observe a burst, this would not move either.
        let mut control = pacing_sim();
        let mut unclamped = 0u64;
        for _ in 0..(stall.as_secs_f64() / TICK_SECS) as u32 {
            unclamped += ticks_for(&mut control, TICK_SECS);
        }
        assert_eq!(unclamped, 1200, "control must replay every missed tick");
        assert!(
            unclamped > clamped * 100,
            "clamp must be a large reduction: {clamped} vs {unclamped}"
        );
    }

    #[test]
    fn a_normal_frame_is_untouched_by_the_clamp() {
        // The clamp must be invisible at playable frame rates, or it would be
        // silently dropping game time during ordinary play (which is exactly
        // what a too-tight cap does: at 4 fps a 0.25 s cap discards 75% of it).
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        let frame = Duration::from_micros(16_667); // 60 fps
        let step = pacer.begin_frame(t0 + frame);
        assert!(
            (step.dt - frame.as_secs_f64()).abs() < 1e-9,
            "60 fps frame was altered: {}",
            step.dt
        );

        // And a 4 fps frame — the rate an occluded window degrades to — must
        // still deliver all 250 ms, i.e. five whole ticks, not be truncated.
        let mut pacer = FramePacer::new(t0);
        let step = pacer.begin_frame(t0 + Duration::from_millis(250));
        let mut sim = pacing_sim();
        assert_eq!(ticks_for(&mut sim, step.dt), 5);
    }

    #[test]
    fn an_unfocused_window_keeps_ticking_and_presents_at_thirty_fps() {
        // The whole point: presentation throttles, simulation does not.
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        pacer.set_focused(false);

        let mut sim = pacing_sim();
        let mut rendered = 0u32;
        let mut ticks = 0u64;
        // One simulated second at a 120 Hz loop rate.
        for i in 1..=120u32 {
            let step = pacer.begin_frame(t0 + Duration::from_secs_f64(f64::from(i) / 120.0));
            if step.render {
                rendered += 1;
            }
            ticks += ticks_for(&mut sim, step.dt);
        }

        // 19 or 20: one simulated second at 20 Hz, modulo where the fixed-step
        // residual happens to land (1/120 is not exact in binary, so the last
        // tick can fall just past the second boundary).
        assert!(
            (19..=20).contains(&ticks),
            "unfocused must still tick at ~20 Hz, got {ticks}"
        );
        assert!(
            (30..=31).contains(&rendered),
            "unfocused presentation should be ~30 fps, got {rendered}"
        );
        assert!(
            u64::from(rendered) > ticks,
            "sanity: 30 fps presentation must still outpace 20 Hz ticking"
        );
    }

    /// Counts frames a naive "elapsed since the last presented frame" gate would
    /// deliver over `iters` iterations of a `loop_hz` loop. This is verbatim the
    /// implementation [`FramePacer`] used to have — including the `as_secs_f64()`
    /// comparison against a `1.0 / 30.0` target, which is part of why it drifted:
    /// a `Duration` is whole nanoseconds, so an interval that lands on
    /// 33 333 333 ns is *always* a hair short of 1/30 s and the very iteration
    /// that should have presented never does.
    fn naive_gate_frames(loop_hz: u32, iters: u32) -> u32 {
        let target_secs = 1.0 / f64::from(UNFOCUSED_FPS);
        let t0 = Instant::now();
        let mut last_render = t0;
        let mut n = 0;
        for i in 1..=iters {
            let now = t0 + Duration::from_secs_f64(f64::from(i) / f64::from(loop_hz));
            if now.saturating_duration_since(last_render).as_secs_f64() >= target_secs {
                last_render = now;
                n += 1;
            }
        }
        n
    }

    /// Same span, driven through the real pacer while unfocused.
    fn paced_frames(loop_hz: u32, iters: u32) -> u32 {
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        pacer.set_focused(false);
        let mut n = 0;
        for i in 1..=iters {
            let now = t0 + Duration::from_secs_f64(f64::from(i) / f64::from(loop_hz));
            if pacer.begin_frame(now).render {
                n += 1;
            }
        }
        n
    }

    #[test]
    fn the_unfocused_frame_schedule_does_not_drift_below_its_target() {
        // The bug, and the negative control for the fix. A 30 fps limiter that
        // quietly delivers 26 fps is the whole reason the deadline is absolute:
        // the naive gate can only fire on a loop iteration, and each firing
        // pushes the next deadline out by however far it overshot.
        //
        // Measured, one simulated second each:
        //   loop     naive   paced   target
        //   120 Hz     26      30      30
        //    75 Hz     25      30      30
        //    77 Hz     26      30      30
        for loop_hz in [120u32, 75, 77, 144, 240] {
            let naive = naive_gate_frames(loop_hz, loop_hz);
            let paced = paced_frames(loop_hz, loop_hz);
            assert!(
                (UNFOCUSED_FPS..=UNFOCUSED_FPS + 1).contains(&paced),
                "at {loop_hz} Hz the absolute schedule delivered {paced}, \
                 wanted {UNFOCUSED_FPS}"
            );
            // The control must be observed *failing* the same assertion, or this
            // test proves only that some number came out of some function.
            assert!(
                naive < UNFOCUSED_FPS,
                "control did not fire at {loop_hz} Hz: the naive gate delivered \
                 {naive}, so this test is not measuring the drift it exists for"
            );
        }
        // Exact pre-fix number at the loop rate the sibling test uses, pinned so
        // a future refactor that reintroduces drift is unambiguous.
        assert_eq!(naive_gate_frames(120, 120), 26);
    }

    #[test]
    fn coming_back_from_a_stall_resumes_the_rate_rather_than_replaying_a_backlog() {
        // The presentation-side twin of the catch-up-tick bug: a schedule that
        // advanced by whole intervals *unconditionally* would owe 3600 frames
        // after a two-minute stall and present them as fast as the loop spins.
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        pacer.set_focused(false);
        // Two minutes with no iterations at all, then a tight 120 Hz loop for
        // half a second.
        let resume = t0 + Duration::from_secs(120);
        assert!(pacer.begin_frame(resume).render, "the first frame back draws");

        let mut after = 0;
        for i in 1..=60u32 {
            if pacer
                .begin_frame(resume + Duration::from_secs_f64(f64::from(i) / 120.0))
                .render
            {
                after += 1;
            }
        }
        // Half a second at 30 fps is 15 frames. The backlog would be ~3600.
        assert!(
            (14..=16).contains(&after),
            "expected the steady ~30 fps rate after resuming, got {after} frames \
             in 0.5 s — a replayed backlog looks like ~60 (loop-rate-bound)"
        );
    }

    #[test]
    fn an_occluded_window_skips_presenting_entirely_but_still_ticks() {
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        pacer.set_occluded(true);

        let mut sim = pacing_sim();
        let mut ticks = 0u64;
        for i in 1..=120u32 {
            let step = pacer.begin_frame(t0 + Duration::from_secs_f64(f64::from(i) / 120.0));
            assert!(!step.render, "occluded windows must not acquire a drawable");
            ticks += ticks_for(&mut sim, step.dt);
        }
        assert!(
            (19..=20).contains(&ticks),
            "occluded must still tick at ~20 Hz, got {ticks}"
        );

        // Control: the identical loop with occlusion cleared *does* render, so
        // the assertion above is testing occlusion and not a dead pacer.
        pacer.set_occluded(false);
        let step = pacer.begin_frame(t0 + Duration::from_secs(2));
        assert!(step.render, "clearing occlusion must restore presentation");
    }

    #[test]
    fn focus_selects_the_control_flow_without_ever_stopping_the_loop() {
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(t0);
        assert!(matches!(pacer.control_flow(t0), ControlFlow::Poll));
        assert!(pacer.focused());

        pacer.set_focused(false);
        match pacer.control_flow(t0) {
            ControlFlow::WaitUntil(at) => {
                let slice = at.saturating_duration_since(t0);
                assert!(
                    slice < Duration::from_secs_f64(TICK_SECS),
                    "background poll {slice:?} must wake faster than one 50 ms tick, \
                     or the sim falls behind the server while merely unfocused"
                );
            }
            other => panic!("unfocused must sleep, not spin or wait forever: {other:?}"),
        }
        assert!(!pacer.focused());
    }

    // -- key dispatch and precedence ----------------------------------------
    //
    // These drive [`resolve_key`] directly. It is the whole of the key chain's
    // decision-making, so a precedence regression shows up here rather than
    // needing a window, a GPU and a live `Sim` to observe.

    use crate::keybinds::{Binding, InputAction};

    /// The gate while the world is being played normally.
    fn playing() -> KeyGate {
        KeyGate {
            gameplay: true,
            ..KeyGate::default()
        }
    }

    fn resolve(gate: KeyGate, code: KeyCode, pressed: bool) -> Option<KeyOutcome> {
        resolve_key(&Keybinds::new(), gate, Some(code), pressed, false)
    }

    /// Like [`resolve`], but with Control held — only the drop-key tests need
    /// this axis, so it is a separate helper rather than a fifth argument on
    /// every existing call above.
    fn resolve_ctrl(gate: KeyGate, code: KeyCode, pressed: bool) -> Option<KeyOutcome> {
        resolve_key(&Keybinds::new(), gate, Some(code), pressed, true)
    }

    /// Issue #15's last hop: an F-key has no printable `text`, so it is
    /// exactly the case `menu_key_for` drops and `capture_key_for` must not.
    /// `F1` (not `F5`, which `resolve_key`'s own default table already binds
    /// to `TogglePerspective` — picking a bound key here would prove nothing
    /// about the *unbound*, no-text case a real Controls-menu rebind targets)
    /// persists as vanilla's own `"key.keyboard.f1"`.
    #[test]
    fn capture_key_for_forwards_a_function_key() {
        assert_eq!(
            capture_key_for(PhysicalKey::Code(KeyCode::F1)),
            Some(CaptureKey::Bind(KeyCode::F1)),
            "an F-key must reach the capture as a bindable key, not be \
             dropped the way menu_key_for drops it"
        );
    }

    /// Escape must cancel through the ordinary `MenuKey` path
    /// (`CaptureKey::Cancel`), never through `capture_binding` — the latter
    /// is exactly the `Pause`-unbinding hazard `capture_binding`'s own doc
    /// warns about, and this is the one physical key capture must special-case
    /// rather than forward.
    #[test]
    fn capture_key_for_treats_escape_as_cancel_not_a_binding() {
        assert_eq!(
            capture_key_for(PhysicalKey::Code(KeyCode::Escape)),
            Some(CaptureKey::Cancel)
        );
    }

    /// A printable key must forward too — a capture target is not always an
    /// unprintable one (most vanilla rebinds are ordinary letters), so this
    /// is the control proving `capture_key_for` is not secretly just
    /// `menu_key_for` under another name.
    #[test]
    fn capture_key_for_forwards_a_printable_key_too() {
        assert_eq!(
            capture_key_for(PhysicalKey::Code(KeyCode::KeyF)),
            Some(CaptureKey::Bind(KeyCode::KeyF))
        );
    }

    /// No `KeyCode` exists to persist for an unidentified physical key, so
    /// there is nothing to bind — matches `menu_key_for`'s own `_ => {}`.
    #[test]
    fn capture_key_for_ignores_an_unidentified_key() {
        assert_eq!(
            capture_key_for(PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified
            )),
            None
        );
    }

    /// Every key the default table binds, with what it should resolve to while
    /// playing. Written out rather than derived from the table, so this is a
    /// second statement of intent and not a restatement of the implementation.
    fn default_playing_expectations() -> Vec<(KeyCode, KeyOutcome)> {
        vec![
            (KeyCode::KeyW, KeyOutcome::Movement(Action::Forward, true)),
            (KeyCode::KeyS, KeyOutcome::Movement(Action::Back, true)),
            (KeyCode::KeyA, KeyOutcome::Movement(Action::Left, true)),
            (KeyCode::KeyD, KeyOutcome::Movement(Action::Right, true)),
            (KeyCode::Space, KeyOutcome::Movement(Action::Jump, true)),
            (KeyCode::ShiftLeft, KeyOutcome::Movement(Action::Sneak, true)),
            (
                KeyCode::ControlLeft,
                KeyOutcome::Movement(Action::Sprint, true),
            ),
            (KeyCode::KeyE, KeyOutcome::OpenContainer),
            (KeyCode::KeyT, KeyOutcome::OpenChat { command: false }),
            (KeyCode::Slash, KeyOutcome::OpenChat { command: true }),
            (KeyCode::Tab, KeyOutcome::PlayerList(true)),
            (KeyCode::F5, KeyOutcome::TogglePerspective),
            (KeyCode::F3, KeyOutcome::ToggleDebugOverlay),
            (KeyCode::Escape, KeyOutcome::Pause),
            (KeyCode::Digit1, KeyOutcome::SelectSlot(0)),
            (KeyCode::Digit2, KeyOutcome::SelectSlot(1)),
            (KeyCode::Digit3, KeyOutcome::SelectSlot(2)),
            (KeyCode::Digit4, KeyOutcome::SelectSlot(3)),
            (KeyCode::Digit5, KeyOutcome::SelectSlot(4)),
            (KeyCode::Digit6, KeyOutcome::SelectSlot(5)),
            (KeyCode::Digit7, KeyOutcome::SelectSlot(6)),
            (KeyCode::Digit8, KeyOutcome::SelectSlot(7)),
            (KeyCode::Digit9, KeyOutcome::SelectSlot(8)),
        ]
    }

    #[test]
    fn the_default_bindings_dispatch_exactly_as_they_did_before_the_refactor() {
        // The no-regression gate for the whole change: every key the hardcoded
        // chain used to handle still resolves to the same effect.
        for (code, want) in default_playing_expectations() {
            assert_eq!(
                resolve(playing(), code, true),
                Some(want),
                "{code:?} regressed"
            );
        }
    }

    #[test]
    fn the_hotbar_number_keys_select_the_slot_one_below_their_digit() {
        // Called out as one of the two things most likely to break quietly: the
        // digits are 1..9 and the slots are 0..8, so an off-by-one here shifts
        // every hotbar key by one and looks almost right.
        let digits = [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
        ];
        for (i, code) in digits.into_iter().enumerate() {
            assert_eq!(
                resolve(playing(), code, true),
                Some(KeyOutcome::SelectSlot(i)),
                "{code:?} should select slot {i}"
            );
        }
        // Digit0 is unbound in vanilla and must stay unbound — binding it to
        // slot 9 would be a tenth hotbar slot that does not exist.
        assert_eq!(resolve(playing(), KeyCode::Digit0, true), None);
        // Releasing a hotbar key does nothing (it is not a held state).
        assert_eq!(resolve(playing(), KeyCode::Digit1, false), None);
    }

    #[test]
    fn slash_opens_chat_with_the_command_prefix_and_t_opens_it_without() {
        // The other quiet-breakage candidate. The distinction is a single bool,
        // and getting it backwards means every chat message starts with `/`
        // (or no command can ever be typed).
        assert_eq!(
            resolve(playing(), KeyCode::Slash, true),
            Some(KeyOutcome::OpenChat { command: true })
        );
        assert_eq!(
            resolve(playing(), KeyCode::KeyT, true),
            Some(KeyOutcome::OpenChat { command: false })
        );

        // …and the prefix follows the *`key.command` binding*, not the physical
        // slash key. Rebinding chat and command to other keys must carry the
        // distinction with them.
        let mut binds = Keybinds::new();
        binds.set(InputAction::Command, Binding::Key(KeyCode::Backquote));
        binds.set(InputAction::Chat, Binding::Key(KeyCode::KeyY));
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::Backquote), true, false),
            Some(KeyOutcome::OpenChat { command: true })
        );
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::KeyY), true, false),
            Some(KeyOutcome::OpenChat { command: false })
        );
        // The old keys stop opening chat at all.
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::Slash), true, false),
            None
        );
    }

    #[test]
    fn an_open_container_swallows_every_gameplay_key() {
        // The precedence that matters most: while a container is up, keys must
        // not reach gameplay.
        //
        // Two gates are checked, and the second is the one that actually tests
        // the *arm*. In production `container_open` implies `!gameplay` (the
        // screen is `Container`, so `accepts_gameplay_input()` is false), which
        // means the first gate would swallow most keys through the `gate.gameplay`
        // guards even if the container arm were deleted — a vacuous test of the
        // "world" species, passing because of the input it was handed rather than
        // the code it names. The `gameplay: true` gate cannot occur in practice
        // but isolates the container arm: with it, *only* the arm's early return
        // stands between these keys and gameplay.
        for gate in [
            KeyGate {
                container_open: true,
                ..KeyGate::default()
            },
            KeyGate {
                container_open: true,
                gameplay: true,
                ..KeyGate::default()
            },
        ] {
            for (code, would_have) in default_playing_expectations() {
                // Escape and the inventory key have their own jobs on this screen,
                // and since #378 part 3 so do the nine number keys — they issue a
                // `SWAP` against the hovered slot rather than being swallowed.
                // Their own test is `the_number_keys_swap_with_the_hovered_slot`
                // below; excluding them here is not weakening this test, because
                // what it asserts is that nothing reaches *gameplay*, and
                // `ContainerSwap` is not a gameplay outcome.
                if matches!(code, KeyCode::Escape | KeyCode::KeyE)
                    || hotbar_slot_for(&Keybinds::new(), code).is_some()
                {
                    continue;
                }
                assert_eq!(
                    resolve(gate, code, true),
                    None,
                    "{code:?} leaked through an open container (gate {gate:?})"
                );
                // -- negative control -----------------------------------------
                // The same key on the same table *does* resolve while playing, so
                // this test is observing the swallow and not a dead resolver.
                assert_eq!(
                    resolve(playing(), code, true),
                    Some(would_have),
                    "control failed: {code:?} does nothing even while playing, so \
                     asserting it is swallowed proves nothing"
                );
            }
        }
    }

    #[test]
    fn the_inventory_key_closes_a_container_and_escape_pauses_instead() {
        let gate = KeyGate {
            container_open: true,
            ..KeyGate::default()
        };
        assert_eq!(
            resolve(gate, KeyCode::KeyE, true),
            Some(KeyOutcome::CloseContainer)
        );
        // Escape is resolved by the arm *above* the container arm, so it pauses
        // (and `Pause`'s handler closes the menu on the way). If the container
        // arm were moved above it, this would be `CloseContainer` and Escape
        // would stop reaching the pause screen from an open inventory.
        assert_eq!(resolve(gate, KeyCode::Escape, true), Some(KeyOutcome::Pause));
        // A key release while a container is open does nothing at all — but must
        // also not fall through to the gameplay arms.
        assert_eq!(resolve(gate, KeyCode::KeyE, false), None);
        assert_eq!(resolve(gate, KeyCode::KeyW, false), None);
    }

    /// Issue #378 part 3. Vanilla's `1`–`9` **do not** change the selected hotbar
    /// slot while a container screen is open; they issue a `ContainerInput::SWAP`
    /// with that hotbar index against the hovered slot
    /// (`AbstractContainerScreen.checkHotbarKeyPressed`,
    /// `AbstractContainerScreen.java:506-522`, and the number keys are handled in
    /// `Minecraft.handleKeybinds` only when `screen == null`).
    ///
    /// Before this they fell into the container arm's swallow: they neither
    /// selected a slot — correct — nor swapped, which is the gap.
    #[test]
    fn the_number_keys_swap_with_the_hovered_slot_instead_of_selecting_one() {
        let gate = KeyGate {
            container_open: true,
            ..KeyGate::default()
        };
        let digits = [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
        ];
        for (i, code) in digits.into_iter().enumerate() {
            // The button number is the hotbar index, `0..=8` — vanilla passes the
            // loop counter straight through as `buttonNum`.
            assert_eq!(
                resolve(gate, code, true),
                Some(KeyOutcome::ContainerSwap { button: i as i32 }),
                "{code:?} must swap with hotbar index {i} while a container is open"
            );
            // -- the two controls -------------------------------------------
            // 1. The same key while *playing* still selects the slot. Without
            //    this, a resolver that had simply lost `SelectSlot` altogether
            //    would satisfy the assertion above.
            assert_eq!(
                resolve(playing(), code, true),
                Some(KeyOutcome::SelectSlot(i)),
                "control failed: {code:?} no longer selects a hotbar slot in the \
                 world either, so this is not a container-specific route"
            );
            // 2. A key *release* is not a swap. Vanilla acts on `keyPressed`
            //    only, and a swap on both edges would fire every action twice.
            assert_eq!(
                resolve(gate, code, false),
                None,
                "{code:?} released must do nothing"
            );
        }
        // And the outcome is genuinely distinct from selecting a slot: nothing in
        // the container arm may produce `SelectSlot`, or the hotbar would jump
        // under an open inventory.
        for code in digits {
            assert!(
                !matches!(resolve(gate, code, true), Some(KeyOutcome::SelectSlot(_))),
                "{code:?} must not change the selected slot behind a screen"
            );
        }
    }

    /// The off-hand key's container half (issues #378 part 3 / #382).
    ///
    /// `key.swapOffhand` defaults to `F` (`Options.java:663`, GLFW keysym 70).
    /// It could not be added while `key.lodestone.toggleFly` squatted on `F`;
    /// #382 deleted that binding, and this is the assertion that the freed key
    /// actually reaches `Click::offhand_swap` rather than merely existing in
    /// the table.
    #[test]
    fn the_offhand_key_swaps_with_slot_forty_while_a_container_is_open() {
        let gate = KeyGate {
            container_open: true,
            ..KeyGate::default()
        };
        assert_eq!(
            resolve(gate, KeyCode::KeyF, true),
            Some(KeyOutcome::ContainerSwap {
                button: OFFHAND_SWAP_BUTTON
            }),
            "F must issue a SWAP against the off-hand's native slot"
        );
        // -- three controls, each for a different way this could be hollow ---
        // 1. The button number is the off-hand's, not a hotbar index. `40` is
        //    outside `0..=8`, so a resolver that had fallen through to
        //    `hotbar_slot_for` cannot satisfy this.
        assert!(
            !(0..=8).contains(&OFFHAND_SWAP_BUTTON),
            "control failed: 40 overlaps the hotbar range, so the assertion \
             above cannot distinguish the two routes"
        );
        // 2. A release is not a swap — vanilla acts on `keyPressed` only.
        assert_eq!(resolve(gate, KeyCode::KeyF, false), None);
        // 3. **The gameplay half is a different outcome, not the same one.**
        //    This line used to assert `None` with a note saying that landing
        //    #378's gameplay half should come here and change it on purpose.
        //    Issue #385 is that landing, and this is the change: with no screen
        //    open the key must resolve to the *bare action*, never to a
        //    `ContainerSwap` — a resolver that reused `ContainerSwap` here would
        //    hit-test a slot that does not exist and silently do nothing.
        assert_eq!(
            resolve(playing(), KeyCode::KeyF, true),
            Some(KeyOutcome::SwapOffhand),
            "with no screen open the off-hand key is a ServerboundPlayerAction, \
             not a container click (#385)"
        );
        assert_ne!(
            resolve(playing(), KeyCode::KeyF, true),
            resolve(gate, KeyCode::KeyF, true),
            "the two routes must not collapse into one outcome — that is the \
             conflation #385 exists to prevent"
        );
    }

    /// Issue #385, the gameplay half: `F` in the world **reaches the wire** as
    /// `ClientAction::SwapItemWithOffhand`.
    ///
    /// Two hops, both asserted, because either alone is satisfiable by a dead
    /// chain: `resolve_key` producing the outcome proves nothing about the
    /// driver, and a `NetClient` that accepts an action proves nothing about the
    /// keybind. The `match` arm between them is the piece a compiler *cannot*
    /// check — an arm that resolved and then did nothing would be exactly the
    /// island `CLAUDE.md` §1 names.
    ///
    /// What this deliberately does not assert is the **bytes**. Those are pinned
    /// where they belong, against the jar's own declared layout, in
    /// `crates/protocol/v770/tests/interaction_actions.rs`
    /// (`swap_item_with_offhand_is_byte_exact_against_the_jars_enum_order`) —
    /// asserting them again here off our own encoder would be
    /// `decode(encode(x))` with extra steps.
    #[test]
    fn the_offhand_key_in_the_world_sends_the_swap_action_to_the_wire() {
        assert_eq!(
            resolve(playing(), KeyCode::KeyF, true),
            Some(KeyOutcome::SwapOffhand),
            "hop 1: the keybind must resolve"
        );

        // Hop 2: the driver's arm. `offhand_swap_action` is what it calls; the
        // loopback below is what proves an accepted action is observable.
        let (net, actions) = NetClient::loopback();
        let action = offhand_swap_action(Some(lodestone_client::GameMode::Survival))
            .expect("a survival player may swap");
        net.send_action(action);
        assert_eq!(
            actions.try_recv(),
            Ok(lodestone_model::ClientAction::SwapItemWithOffhand),
            "hop 2: the action must reach the outbound channel"
        );
        assert!(
            actions.try_recv().is_err(),
            "exactly one action per press — a doubled send would swap twice and \
             land back where it started, which looks identical to doing nothing"
        );
    }

    /// **The spectator control**, and the one guard vanilla actually applies
    /// (`Minecraft.java:1901`, re-checked server-side at
    /// `ServerGamePacketListenerImpl.java:1295`).
    ///
    /// Watched failing: with the `Spectator` arm removed,
    /// `offhand_swap_action(Spectator)` returns the action and the first
    /// assertion below reports `Some(SwapItemWithOffhand)`.
    ///
    /// The other three modes are the positive control. Without them this passes
    /// just as well against a function that returns `None` unconditionally — i.e.
    /// against the feature not existing at all, which is the state this issue
    /// found.
    #[test]
    fn a_spectator_does_not_send_the_offhand_swap_and_everyone_else_does() {
        use lodestone_client::GameMode;
        assert_eq!(
            offhand_swap_action(Some(GameMode::Spectator)),
            None,
            "a spectator has no inventory to swap; vanilla declines to send"
        );
        for mode in [
            GameMode::Survival,
            GameMode::Creative,
            GameMode::Adventure,
        ] {
            assert_eq!(
                offhand_swap_action(Some(mode)),
                Some(lodestone_model::ClientAction::SwapItemWithOffhand),
                "{mode:?} must still swap — otherwise the guard above is \
                 indistinguishable from the feature being absent"
            );
        }
        // Before login there is no mode. Sending is the better default: refusing
        // input until a mode arrives would make the key dead during the join
        // window, and the server re-checks anyway.
        assert_eq!(
            offhand_swap_action(None),
            Some(lodestone_model::ClientAction::SwapItemWithOffhand),
            "an unknown game mode must not read as spectator"
        );
    }

    // -- the drop key (`Q`), the two proven islands ------------------------
    //
    // `Click::drop_one`/`drop_stack`/`do_throw` (`lodestone-game`, #27) and
    // `ClientAction::DropSelectedItem`/`DropSelectedItemStack` were each built,
    // encoded and round-trip tested with zero producers before this. One
    // binding closes both — see `InputAction::Drop`'s and `KeyOutcome::
    // ContainerDrop`/`Drop`'s docs for the vanilla source this mirrors.

    /// The gameplay half, mirroring `the_offhand_key_swaps_with_slot_forty_
    /// while_a_container_is_open`'s shape: both resolve to a *different*
    /// outcome than the container half, and `ctrl` must reach the outcome
    /// unchanged from what `resolve_key` was handed.
    #[test]
    fn q_drops_one_while_playing_and_ctrl_q_drops_the_stack() {
        assert_eq!(
            resolve(playing(), KeyCode::KeyQ, true),
            Some(KeyOutcome::Drop { ctrl: false })
        );
        assert_eq!(
            resolve_ctrl(playing(), KeyCode::KeyQ, true),
            Some(KeyOutcome::Drop { ctrl: true })
        );
        // A release does nothing — vanilla's `keyDrop.consumeClick()` only
        // ever fires on the down edge.
        assert_eq!(resolve(playing(), KeyCode::KeyQ, false), None);
    }

    /// The container half — vanilla's `AbstractContainerScreen.keyPressed`
    /// (`:495-501`) reached through `resolve_key`'s `container_open` arm.
    #[test]
    fn q_issues_a_container_drop_while_a_container_is_open() {
        let gate = KeyGate {
            container_open: true,
            ..KeyGate::default()
        };
        assert_eq!(
            resolve(gate, KeyCode::KeyQ, true),
            Some(KeyOutcome::ContainerDrop { ctrl: false })
        );
        assert_eq!(
            resolve_ctrl(gate, KeyCode::KeyQ, true),
            Some(KeyOutcome::ContainerDrop { ctrl: true })
        );
        assert_eq!(resolve(gate, KeyCode::KeyQ, false), None);
        // -- the two-mechanisms control, same shape as the off-hand key's own --
        assert_ne!(
            resolve(playing(), KeyCode::KeyQ, true),
            resolve(gate, KeyCode::KeyQ, true),
            "the container and gameplay routes must not collapse into one \
             outcome, or the container click would fire in the world (no menu \
             to hit-test) or vice versa"
        );
    }

    /// `key.drop` must not have been swallowed as an unrecognised key behind
    /// an open container before this landed — the negative control for the
    /// island itself, run against the pre-fix shape by simulating what an
    /// unbound `InputAction::Drop` would have produced.
    #[test]
    fn an_unbound_drop_key_is_swallowed_behind_a_container_and_dead_in_the_world() {
        let mut binds = Keybinds::new();
        binds.set(InputAction::Drop, Binding::Unbound);
        let gate = KeyGate {
            container_open: true,
            ..KeyGate::default()
        };
        assert_eq!(
            resolve_key(&binds, gate, Some(KeyCode::KeyQ), true, false),
            None,
            "watched failing before this test existed: with the real binding \
             still assigned, this line reported Some(ContainerDrop {{ .. }})"
        );
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::KeyQ), true, false),
            None
        );
    }

    /// Hop 1 (`resolve_key`) and hop 2 (the driver's action, factored into
    /// [`drop_selected_action`] the same way `offhand_swap_action` is) for the
    /// gameplay half, mirroring `the_offhand_key_in_the_world_sends_the_swap_
    /// action_to_the_wire`.
    #[test]
    fn the_drop_key_in_the_world_sends_the_drop_action_to_the_wire() {
        assert_eq!(
            resolve(playing(), KeyCode::KeyQ, true),
            Some(KeyOutcome::Drop { ctrl: false }),
            "hop 1: the keybind must resolve"
        );

        let (net, actions) = NetClient::loopback();
        let action = drop_selected_action(Some(lodestone_client::GameMode::Survival), false)
            .expect("a survival player may drop");
        net.send_action(action.clone());
        assert_eq!(
            actions.try_recv(),
            Ok(lodestone_model::ClientAction::DropSelectedItem),
            "hop 2: the action must reach the outbound channel"
        );
        assert!(actions.try_recv().is_err(), "exactly one action per press");

        // And the `ctrl` axis selects the *other* wire action, not a flag on
        // the same one — `DropSelectedItem`/`DropSelectedItemStack` are two
        // separate `ClientAction` variants, not one with a bool field.
        let stack_action =
            drop_selected_action(Some(lodestone_client::GameMode::Survival), true)
                .expect("a survival player may drop the whole stack");
        assert_eq!(
            stack_action,
            lodestone_model::ClientAction::DropSelectedItemStack
        );
        assert_ne!(action, stack_action);
    }

    /// The spectator control, the one guard vanilla applies
    /// (`Minecraft.java:1908`) — same shape as `a_spectator_does_not_send_
    /// the_offhand_swap_and_everyone_else_does`, watched failing the same way:
    /// remove the `Spectator` arm from `drop_selected_action` and the first
    /// assertion below reports `Some(DropSelectedItem)`.
    #[test]
    fn a_spectator_does_not_send_the_drop_action_and_everyone_else_does() {
        use lodestone_client::GameMode;
        assert_eq!(
            drop_selected_action(Some(GameMode::Spectator), false),
            None,
            "a spectator has nothing to drop; vanilla declines to send"
        );
        assert_eq!(
            drop_selected_action(Some(GameMode::Spectator), true),
            None,
            "the ctrl axis must not bypass the spectator guard"
        );
        for mode in [
            GameMode::Survival,
            GameMode::Creative,
            GameMode::Adventure,
        ] {
            assert_eq!(
                drop_selected_action(Some(mode), false),
                Some(lodestone_model::ClientAction::DropSelectedItem),
                "{mode:?} must still drop — otherwise the guard above is \
                 indistinguishable from the feature being absent"
            );
        }
        // Before login there is no mode; sending is the better default, same
        // reasoning as `offhand_swap_action`'s own `None` case.
        assert_eq!(
            drop_selected_action(None, false),
            Some(lodestone_model::ClientAction::DropSelectedItem),
            "an unknown game mode must not read as spectator"
        );
    }

    #[test]
    fn an_open_chat_prompt_swallows_every_key_into_the_editor() {
        // `W` must type a `w`, not walk.
        let gate = KeyGate {
            chat_open: true,
            ..KeyGate::default()
        };
        for (code, _) in default_playing_expectations() {
            assert_eq!(
                resolve(gate, code, true),
                Some(KeyOutcome::Chat),
                "{code:?} should route to the chat editor"
            );
        }
        // Including keys nothing is bound to — the editor wants those too.
        assert_eq!(resolve(gate, KeyCode::KeyZ, true), Some(KeyOutcome::Chat));
        // And an unnameable physical key still reaches the editor, whose `text`
        // may be the only thing that identifies it.
        assert_eq!(
            resolve_key(&Keybinds::new(), gate, None, true, false),
            Some(KeyOutcome::Chat)
        );
    }

    #[test]
    fn a_menu_screen_outranks_the_chat_prompt_and_everything_below_it() {
        let gate = KeyGate {
            menu: true,
            ..KeyGate::default()
        };
        for (code, _) in default_playing_expectations() {
            assert_eq!(resolve(gate, code, true), Some(KeyOutcome::Menu));
        }
        // Both flags set: the menu wins. This is the documented order, and a
        // swapped pair would send the edit form's keystrokes to the chat buffer.
        let both = KeyGate {
            menu: true,
            chat_open: true,
            container_open: true,
            gameplay: true,
        };
        assert_eq!(resolve(both, KeyCode::KeyW, true), Some(KeyOutcome::Menu));
        assert_eq!(resolve(both, KeyCode::Escape, true), Some(KeyOutcome::Menu));
        // Chat outranks the container and gameplay in turn.
        let chat_over_container = KeyGate {
            chat_open: true,
            container_open: true,
            gameplay: true,
            ..KeyGate::default()
        };
        assert_eq!(
            resolve(chat_over_container, KeyCode::KeyE, true),
            Some(KeyOutcome::Chat)
        );
    }

    #[test]
    fn gameplay_bindings_are_inert_when_no_screen_accepts_gameplay_input() {
        // Every flag false: no menu, no chat, no container, and not playing —
        // e.g. the loading screen. Only the two ungated arms may still fire.
        let gate = KeyGate::default();
        for (code, _) in default_playing_expectations() {
            let got = resolve(gate, code, true);
            match code {
                // `Pause` is intentionally ungated: Escape must work on the
                // loading and error screens, which is how it did before.
                KeyCode::Escape => assert_eq!(got, Some(KeyOutcome::Pause)),
                // So is the debug overlay — it is an instrument, and gating it
                // on `Playing` would make it unavailable exactly when a stuck
                // connection is the thing being debugged.
                KeyCode::F3 => assert_eq!(got, Some(KeyOutcome::ToggleDebugOverlay)),
                _ => assert_eq!(got, None, "{code:?} fired outside gameplay"),
            }
        }
    }

    #[test]
    fn held_bindings_report_both_edges_and_one_shot_bindings_only_the_press() {
        // Movement and the player list are held states; the rest are one-shots.
        // A one-shot that fired on release would double-toggle perspective, and
        // a held binding gated on `pressed` would stick on forever.
        assert_eq!(
            resolve(playing(), KeyCode::KeyW, false),
            Some(KeyOutcome::Movement(Action::Forward, false))
        );
        assert_eq!(
            resolve(playing(), KeyCode::Tab, false),
            Some(KeyOutcome::PlayerList(false))
        );
        for one_shot in [
            KeyCode::KeyE,
            KeyCode::KeyT,
            KeyCode::Slash,
            KeyCode::KeyF,
            KeyCode::F5,
            KeyCode::F3,
            KeyCode::Escape,
            KeyCode::Digit1,
        ] {
            assert_eq!(
                resolve(playing(), one_shot, false),
                None,
                "{one_shot:?} must not fire on release"
            );
        }
    }

    #[test]
    fn a_rebind_moves_the_behaviour_to_the_new_key_and_off_the_old_one() {
        let mut binds = Keybinds::new();
        binds.set(InputAction::Inventory, Binding::Key(KeyCode::KeyI));
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::KeyI), true, false),
            Some(KeyOutcome::OpenContainer)
        );
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::KeyE), true, false),
            None,
            "the old default must stop opening the inventory"
        );
        // …and the rebound key also closes the container, because both sites ask
        // the table rather than naming `KeyE`.
        let gate = KeyGate {
            container_open: true,
            ..KeyGate::default()
        };
        assert_eq!(
            resolve_key(&binds, gate, Some(KeyCode::KeyI), true, false),
            Some(KeyOutcome::CloseContainer)
        );
        assert_eq!(
            resolve_key(&binds, gate, Some(KeyCode::KeyE), true, false),
            None
        );
    }

    #[test]
    fn unbinding_an_action_disables_it_without_disturbing_the_rest() {
        let mut binds = Keybinds::new();
        binds.set(InputAction::Jump, Binding::Unbound);
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::Space), true, false),
            None
        );
        // The neighbouring arms are untouched.
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::KeyW), true, false),
            Some(KeyOutcome::Movement(Action::Forward, true))
        );
    }

    #[test]
    fn attack_and_use_are_keyboard_dispatchable_once_rebound_off_the_mouse() {
        // Under the defaults these arms are dormant, because attack and use are
        // mouse-bound — assert that, so "it works" cannot be an accident of the
        // key path firing too.
        assert_eq!(resolve(playing(), KeyCode::KeyR, true), None);

        let mut binds = Keybinds::new();
        binds.set(InputAction::Attack, Binding::Key(KeyCode::KeyR));
        binds.set(InputAction::Use, Binding::Key(KeyCode::KeyV));
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::KeyR), true, false),
            Some(KeyOutcome::Attack(true))
        );
        // Hold-to-dig: the release edge must arrive, or mining never stops.
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::KeyR), false, false),
            Some(KeyOutcome::Attack(false))
        );
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::KeyV), true, false),
            Some(KeyOutcome::Use(true))
        );
        // The release edge must arrive too, or `ReleaseUseItem` never sends —
        // the exact bug this test's sibling assertions exist to catch (a bow
        // or shield cannot complete a use without it).
        assert_eq!(
            resolve_key(&binds, playing(), Some(KeyCode::KeyV), false, false),
            Some(KeyOutcome::Use(false))
        );
    }

    #[test]
    fn the_mouse_path_resolves_the_default_attack_and_use_buttons() {
        // The mouse half of dispatch, which is why `Binding` is not `KeyCode`.
        let binds = Keybinds::new();
        assert_eq!(
            mouse_action_for(&binds, MouseButton::Left),
            Some(InputAction::Attack)
        );
        assert_eq!(
            mouse_action_for(&binds, MouseButton::Right),
            Some(InputAction::Use)
        );
        // Middle **is** a gameplay binding now: `key.pickItem` defaults to
        // `Type.MOUSE, 2` (`Options.java:669`), so it is the primary route for
        // pick-item rather than a rebound one. This assertion previously read
        // `None`, which was correct only while pick-item did not exist — the
        // premise went stale when the binding landed, not the code.
        assert_eq!(
            mouse_action_for(&binds, MouseButton::Middle),
            Some(InputAction::PickItem)
        );

        // Swapping the two buttons is a supported rebind.
        let mut swapped = binds;
        swapped.set(InputAction::Attack, Binding::Mouse(MouseButton::Right));
        swapped.set(InputAction::Use, Binding::Mouse(MouseButton::Left));
        assert_eq!(
            mouse_action_for(&swapped, MouseButton::Right),
            Some(InputAction::Attack)
        );
        assert_eq!(
            mouse_action_for(&swapped, MouseButton::Left),
            Some(InputAction::Use)
        );
    }

    #[test]
    fn a_movement_action_can_be_driven_from_a_mouse_button() {
        // Not something vanilla offers, but it falls out of `Binding` covering
        // both input kinds — and the mouse handler routes it, so it is not an
        // island.
        let mut binds = Keybinds::new();
        binds.set(InputAction::Jump, Binding::Mouse(MouseButton::Middle));
        let action = mouse_action_for(&binds, MouseButton::Middle);
        assert_eq!(action, Some(InputAction::Jump));
        assert_eq!(action.and_then(InputAction::movement), Some(Action::Jump));
    }

    #[test]
    fn an_unnameable_physical_key_is_ignored_by_the_binding_chain() {
        // `PhysicalKey::Unidentified` reaches the menu and chat arms (tested
        // above) but must not match any binding — there is nothing to match on.
        assert_eq!(
            resolve_key(&Keybinds::new(), playing(), None, true, false),
            None
        );
    }

    /// **Pressing Play Selected World reaches a running integrated server**
    /// (issue #287).
    ///
    /// This is the anti-island gate for singleplayer, and it is the only test
    /// anywhere that crosses *every* seam of it in one go: the registry's
    /// serverbound lookup, the boxed `ServerProtocol`, the net thread, the
    /// in-memory duplex, `IntegratedServer`'s serving loop, the real v770 wire
    /// format, and the client's decode — ending at a `NetUpdate` the shell's own
    /// frame loop consumes.
    ///
    /// The button half is `menu::nav`'s
    /// `play_selected_world_asks_the_app_to_start_singleplayer`, which asserts the
    /// click produces `MenuAction::Singleplayer(None)`; `apply_menu_action`'s arm
    /// between the two is a single call this file can be read for. The seam
    /// *without* the shell is `crates/protocol/v770/tests/singleplayer_seam.rs`.
    ///
    /// **Chunks, not just login, is the load-bearing assertion.** Login is five
    /// `ServerProtocol` methods with no trait defaults, so it cannot silently fall
    /// through the box; terrain is where a half-wired server shows up, and it is
    /// also the only thing here that proves the *world* exists rather than just a
    /// handshake. A world that logs in and streams nothing is precisely the shape
    /// of the chunk-blackout failures `CLAUDE.md` records.
    ///
    /// `view_radius = 0` is one column: the bundled generator costs ~12 ms per
    /// column, and one is enough to prove terrain crosses the wire (its *content*
    /// is verified block-for-block in `lodestone-server`'s own tests, against a
    /// JVM oracle rather than against our encoder).
    #[test]
    fn pressing_play_reaches_a_running_integrated_server() {
        let protocol = Config::default().protocol;
        let seed = crate::menu::world_select::BUNDLED_WORLD.seed;
        let net = match launch_singleplayer(protocol, 0, None, seed) {
            Ok(net) => net,
            Err(e) => {
                // A build with no hostable family must *report*, which is the
                // `--no-default-features` contract. In the default build (`live`)
                // this is a failure, not a skip.
                assert!(
                    !cfg!(feature = "live"),
                    "the default build must be able to host singleplayer: {e}"
                );
                assert!(matches!(e, LaunchError::NoVersionFamily { .. }));
                return;
            }
        };

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut logged_in = false;
        let mut chunks = 0usize;
        let mut errors: Vec<String> = Vec::new();
        while Instant::now() < deadline && !(logged_in && chunks > 0) {
            for update in net.poll() {
                match update {
                    crate::net::NetUpdate::LoggedIn { .. } => logged_in = true,
                    crate::net::NetUpdate::Chunk { .. } => chunks += 1,
                    // Collected rather than ignored: an `Error`/`Disconnected`
                    // here is the actual diagnosis, and without it the failure
                    // message would only say "timed out".
                    crate::net::NetUpdate::Error(e) => errors.push(e),
                    crate::net::NetUpdate::Disconnected(reason) => {
                        errors.push(format!("disconnected: {reason:?}"));
                    }
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(
            logged_in,
            "the client never logged in to the integrated server; errors: {errors:?}"
        );
        assert!(
            chunks > 0,
            "logged in but no terrain arrived — the server is serving nothing; \
             errors: {errors:?}"
        );
        assert!(
            errors.is_empty(),
            "the session reported errors while starting: {errors:?}"
        );
    }

    /// **Issue #189's queued patch, exercised through production code.**
    ///
    /// `crate::menu::social::entries_from_tablist` was pure and unit-tested
    /// with **no caller anywhere in the shell** — `docs/social-interactions.md`'s
    /// own "Decorative" section. This does not call it a second time by hand
    /// (that would just be the existing unit test again, which proves
    /// nothing about production); it drives the actual chain: a real
    /// `WindowApp`, a `SessionTabList` folded through the same `NetIngest`
    /// schedule the net thread runs, and `drive_ui_from_session` itself —
    /// the method `redraw()` calls every frame.
    #[test]
    fn drive_ui_from_session_refreshes_the_social_roster_from_the_real_tab_list() {
        use crate::net::NetUpdate;
        use lodestone_client::{ClientEvent, GameMode, PlayerListEntry};
        use uuid::Uuid;

        let mut app = WindowApp::new(Config {
            mode: Mode::Headless,
            ..Config::default()
        });
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        app.sim.attach_net(net);
        // `drive_ui_from_session`'s refresh is guarded on `SessionPhase::Connected`
        // — reach it the same way `sim/tests.rs`'s own tab-list test does,
        // through a real `NetUpdate`, not by poking a private field.
        feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
        app.sim.step(1.0 / 20.0);
        assert_eq!(
            app.sim.session_phase(),
            crate::sim::SessionPhase::Connected,
            "precondition: the refresh guard reads this, so it must actually be live"
        );

        let alice = Uuid::from_u128(1);
        let bob = Uuid::from_u128(2);
        app.sim
            .net()
            .expect("net attached above")
            .ingest_session_event(ClientEvent::PlayerListUpdate {
                entries: vec![
                    PlayerListEntry {
                        uuid: bob,
                        name: Some("Bob".into()),
                        game_mode: Some(GameMode::Creative),
                        latency: Some(20),
                        display_name: None,
                        listed: Some(true),
                    },
                    PlayerListEntry {
                        uuid: alice,
                        name: Some("Alice".into()),
                        game_mode: Some(GameMode::Survival),
                        latency: Some(10),
                        display_name: None,
                        listed: Some(true),
                    },
                ],
            });

        // Precondition: nothing has refreshed the screen model yet — proves the
        // assertion below actually exercises `drive_ui_from_session`, not some
        // earlier call this test forgot about.
        assert!(
            app.nav.social().entries().is_empty(),
            "precondition: the roster must still be empty before the real call runs"
        );

        app.drive_ui_from_session();

        let names: Vec<&str> = app
            .nav
            .social()
            .entries()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["Alice", "Bob"],
            "the roster must reflect the real folded tab list, in vanilla's display order"
        );
    }

    /// Issue #192's last hop, exercised through production code exactly like
    /// the social-roster test above: `menu::UiState::show_credits` and
    /// `net::NetUpdate::WinGame` both already existed, individually tested,
    /// with **nothing calling either from the other** — the credits screen was
    /// reachable only from a test, and `WinGame` only reached a channel no
    /// one drained into UI state. This drives the real chain end to end: a
    /// real `WindowApp`, a real `NetUpdate::WinGame` through the loopback
    /// feed (the same seam `NetClient::run`'s background thread publishes
    /// into in production, once `net::forward` — separately proven by
    /// `forward_translates_win_game_into_the_credits_signal` — turns the real
    /// decoded `ClientEvent::WinGame` into it), `Sim::poll_net`'s real
    /// `WinGame` arm, and `drive_ui_from_session` itself.
    #[test]
    fn drive_ui_from_session_opens_credits_on_the_real_win_game_event() {
        use crate::net::NetUpdate;

        let mut app = WindowApp::new(Config {
            mode: Mode::Headless,
            ..Config::default()
        });
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        app.sim.attach_net(net);
        // Reach a live-gameplay screen the same way `on_credits` (`menu/
        // nav.rs`'s own test helper) does — `show_credits` only leaves from
        // `Playing | Chat | Container | Paused`, matching `die`'s guard.
        app.ui.enter_dev_world();
        assert_eq!(
            app.ui.screen(),
            crate::menu::Screen::Playing,
            "precondition: must be on a live-gameplay screen before WinGame arrives"
        );
        assert!(
            !app.sim.has_won(),
            "precondition: nothing has signalled a win yet"
        );

        feed.send(NetUpdate::WinGame).unwrap();
        app.sim.step(1.0 / 20.0);
        assert!(
            app.sim.has_won(),
            "Sim::poll_net's real WinGame arm must latch the win"
        );
        // Precondition restated after the poll but before the real call this
        // test exercises, so the assertion below cannot be explained by
        // something upstream having already moved the screen.
        assert_eq!(
            app.ui.screen(),
            crate::menu::Screen::Playing,
            "precondition: drive_ui_from_session has not run yet"
        );

        app.drive_ui_from_session();

        assert_eq!(
            app.ui.screen(),
            crate::menu::Screen::Credits,
            "the real WIN_GAME event (GAME_EVENT code 4, ClientPacketListener.java:1548) \
             must open the credits screen"
        );
    }

    /// Live gate for issue #25: `ShellWeatherProbe::precipitation` must reach
    /// a real per-column snow/rain decision now that the biome-climate lane
    /// is wired, not the `Rain` it answered unconditionally before this
    /// session (`app.rs`'s own history — see the #25 report).
    ///
    /// Connects directly through `ClientBuilder`, bypassing `NetClient`'s
    /// background thread so the raw event stream can be read here: the real
    /// `ClientEvent::BiomeClimates` is captured off it and folded into a
    /// `BiomeClimateCell` **by hand, with the same call** `net::forward`'s
    /// arm makes — proving the fold, not merely trusting it — while every
    /// other event is drained so the driver's bounded channel never blocks.
    /// Mirrors `net::tests::live_entity_light_at_distinguishes_loaded_from_unloaded`'s
    /// shape.
    ///
    /// The expected precipitation per sampled column is computed **here**,
    /// independently of both `ShellWeatherProbe` and `lodestone_render::
    /// weather` — the raw climate is pulled straight off the `BiomeClimateCell`
    /// and vanilla's own threshold is applied by hand, quoted from the
    /// decompiled source rather than from this crate's constant:
    /// `Biome.java:176`, `return this.getTemperature(pos, seaLevel) >= 0.15F;`
    /// (`warmEnoughToRain`, called from `getPrecipitationAt` at `:108`). A
    /// wrong threshold in either implementation would show up as a mismatch
    /// against this independently-computed expectation rather than agreeing
    /// with itself — the `decode(encode(x)) == x` trap `CLAUDE.md` warns
    /// about, avoided by never calling `precipitation_for_temperature`/
    /// `height_adjusted_temperature` from this test.
    ///
    /// ```text
    /// cargo test -p lodestone-shell --features live --lib \
    ///     app::tests::live_precipitation_matches_vanillas_own_threshold_for_real_biomes \
    ///     -- --ignored --nocapture
    /// ```
    #[cfg(feature = "live")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires the lodestone-survival server on 127.0.0.1:25565"]
    async fn live_precipitation_matches_vanillas_own_threshold_for_real_biomes() {
        use crate::net::BiomeClimateCell;
        use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
        use lodestone_render::WeatherProbe as _;
        use lodestone_testsupport::{poll_until, unique_username};

        let user = unique_username();
        let protocol = 776; // vanilla 26.2 — the `live` feature's compiled-in family
        let adapter = lodestone_registry::adapter_for_protocol(protocol)
            .expect("the `live` feature compiles a family in for protocol 776");
        let (handle, mut events) = ClientBuilder::new(
            ServerAddress {
                host: "127.0.0.1".into(),
                port: 25565,
            },
            LoginProfile {
                username: user.clone(),
                uuid: uuid::Uuid::new_v4(),
            },
            adapter,
        )
        .connect()
        .await
        .expect("connect to lodestone-survival on 127.0.0.1:25565");

        let climates = Arc::new(BiomeClimateCell::default());
        let climates_thread = Arc::clone(&climates);
        let drain = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let lodestone_model::ClientEvent::BiomeClimates {
                    temperatures,
                    downfall,
                    has_precipitation,
                } = event
                {
                    // The exact fold `net::forward`'s `BiomeClimates` arm
                    // makes — called here by hand since this test bypasses
                    // `forward` entirely to read the raw stream.
                    climates_thread.apply(&temperatures, &downfall, &has_precipitation);
                }
            }
        });

        assert!(
            poll_until(
                Duration::from_secs(30),
                Duration::from_millis(100),
                || async {
                    handle
                        .players()
                        .into_iter()
                        .find(|p| p.name.as_deref() == Some(user.as_str()))
                }
            )
            .await
            .is_some(),
            "player {user} never reached Play on the oracle"
        );

        let dims = poll_until(
            Duration::from_secs(10),
            Duration::from_millis(100),
            || async { handle.world_dimensions() },
        )
        .await
        .expect("world dimensions never arrived");

        let loaded = poll_until(
            Duration::from_secs(15),
            Duration::from_millis(200),
            || async {
                let chunks = handle.loaded_chunks();
                if chunks.is_empty() { None } else { Some(chunks) }
            },
        )
        .await
        .expect("no chunks streamed in within 15s of login");

        // The registry (and with it `BiomeClimates`) lands at `Login`, ahead
        // of chunk data, but poll rather than assume the ordering: this test
        // cares about the fold having happened, not about racing it.
        assert!(
            poll_until(Duration::from_secs(10), Duration::from_millis(100), || {
                let climates = Arc::clone(&climates);
                async move { climates.get(0).map(|_| ()) }
            })
            .await
            .is_some(),
            "ClientEvent::BiomeClimates never arrived — the climate table is still empty"
        );

        let handle = Arc::new(handle);
        let probe = ShellWeatherProbe {
            light: 1.0,
            sky_visible: true,
            handle: Some(Arc::clone(&handle)),
            biome_climates: Some(Arc::clone(&climates)),
        };

        // Sample a real column in the middle of a loaded chunk, at mid-build-
        // height. `checked` and `snow_seen`/`rain_seen` are reported in the
        // panic message so a failure names the real biome and climate
        // involved, not just "mismatch".
        let mut checked = 0usize;
        let mut mismatches: Vec<String> = Vec::new();
        for chunk in loaded.iter().take(16) {
            let y = dims.min_y + (dims.height as i32 / 2);
            let block_x = chunk.x * 16 + 8;
            let block_z = chunk.z * 16 + 8;
            let base_si = dims.min_y.div_euclid(16);
            let si = y.div_euclid(16) - base_si;
            if si < 0 || (si as usize) >= dims.section_count() {
                continue;
            }
            let Some(section) = handle.section_at(*chunk, si as usize) else {
                continue;
            };
            let biome = section.biome_at_block(8, y.rem_euclid(16) as usize, 8);
            let Some(climate) = climates.get(usize::try_from(biome).unwrap_or(usize::MAX)) else {
                continue;
            };
            let (Some(temperature), Some(has_precipitation)) =
                (climate.temperature, climate.has_precipitation)
            else {
                continue;
            };
            checked += 1;

            // Independent re-derivation, not a call to `lodestone_render::
            // weather`: vanilla's own height falloff
            // (`Biome.getHeightAdjustedTemperature`, `Biome.java:112-121`)
            // and its own rain/snow threshold (`Biome.java:176`, `0.15F`).
            let above = (y - crate::worldgen::SEA_LEVEL) as f32;
            let adjusted = if above > 0.0 {
                temperature - above * 0.05 / 40.0
            } else {
                temperature
            };
            let expected = if !has_precipitation {
                lodestone_render::Precipitation::None
            } else if adjusted >= 0.15 {
                lodestone_render::Precipitation::Rain
            } else {
                lodestone_render::Precipitation::Snow
            };

            let actual = probe.precipitation(block_x, y, block_z);
            println!(
                "chunk {chunk:?} biome {biome} temperature={temperature} \
                 has_precipitation={has_precipitation} adjusted={adjusted} -> {expected:?}"
            );
            if actual != expected {
                mismatches.push(format!(
                    "chunk {chunk:?} biome {biome} temperature={temperature} \
                     has_precipitation={has_precipitation} adjusted={adjusted}: \
                     expected {expected:?}, probe returned {actual:?}"
                ));
            }
        }

        assert!(
            checked > 0,
            "no loaded column resolved a section + biome + climate — the wiring \
             chain (section_at → biome_at_block → BiomeClimateCell) never \
             produced real data to check against"
        );
        assert!(
            mismatches.is_empty(),
            "{}/{checked} sampled columns disagreed with vanilla's own threshold: \
             {mismatches:#?}",
            mismatches.len()
        );

        drain.abort();
    }
}

/// Gates for the recipe-book panel wiring (issue #163).
///
/// The recipe-book UI landed fully built and unit-tested in `container.rs` and
/// `lodestone-game`, and reached **zero pixels** because the three call sites
/// that drive it live in `app.rs`/`sim.rs`/`hud.rs`. These gates measure the
/// wiring itself, not the geometry — `container.rs`'s own 75 tests already prove
/// the geometry is right, and every one of them passed while nothing drew.
#[cfg(test)]
mod recipe_book_wiring {
    use super::*;
    use lodestone_game::item::ItemStack;
    use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook, ShapedRecipe, TagResolver};
    use lodestone_model::Identifier;

    fn id(name: &str) -> Identifier {
        name.parse().expect("valid identifier")
    }

    fn stack(name: &str, count: i32) -> ItemStack {
        ItemStack::new(id(name), count)
    }

    /// A canvas big enough that the panel is *not* pushed against the
    /// `RECIPE_PANEL_MIN_X` clamp, so the layout under test is the ordinary one.
    const W: u32 = 1280;
    const H: u32 = 800;

    /// The torch: `1` wide, `2` tall — coal over stick.
    ///
    /// Chosen because its arithmetic is **falsifiable**. Laid row-major into a
    /// 3-wide grid the two ingredients occupy cells `0` and `3`, because the
    /// stride is the *grid's* width and not the shape's. A hand-count that used
    /// the shape's width predicts `0` and `1`, and that prediction is wrong —
    /// which is exactly why this recipe is the subject rather than a 1×1 one
    /// that cannot tell the two apart.
    fn torch() -> Recipe {
        Recipe::Shaped(ShapedRecipe::new(
            1,
            2,
            vec![
                Some(Ingredient::Item(id("minecraft:coal"))),
                Some(Ingredient::Item(id("minecraft:stick"))),
            ],
            stack("minecraft:torch", 4),
        ))
    }

    fn torch_book() -> RecipeBook {
        let mut book = RecipeBook::new();
        book.insert(id("minecraft:torch"), torch());
        book
    }

    // -- click-to-fill ---------------------------------------------------

    /// The dispatch loop's **resulting slot contents**, not merely that clicks
    /// were issued.
    ///
    /// This is the assertion that would have caught the plan this change was
    /// briefed with. "Two `ContainerClick`s per step — pick up from
    /// `source_slot`, place into `cell`" reads correctly and is wrong:
    /// `Click::left` on a slot places the **whole** carried stack, so a 5-coal
    /// stack would land entirely in cell 0. See [`auto_fill_clicks`].
    #[test]
    fn auto_fill_puts_exactly_one_item_in_each_grid_cell() {
        let mut menu = Menu::crafting(3, 3);
        menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
        menu.set_slot_item(20, Some(stack("minecraft:stick", 3)));
        let book = torch_book();
        let steps = menu
            .plan_recipe_auto_fill(book.get(&id("minecraft:torch")).expect("recipe"), book.tags())
            .expect("the plan must exist — both ingredients are in the inventory");

        // `craft.first_input == 1` for a crafting table, so grid cells 0 and 3
        // are menu slots 1 and 4.
        assert_eq!(
            steps.iter().map(|s| s.cell).collect::<Vec<_>>(),
            vec![1, 4],
            "row-major into a 3-wide grid: cells 0 and 3, offset by first_input"
        );

        for click in auto_fill_clicks(&steps) {
            click.apply(&mut menu, lodestone_game::click::PlayerCtx::survival());
        }

        assert_eq!(
            menu.slot_item(1).map(|s| (s.item().to_string(), s.count())),
            Some(("minecraft:coal".to_string(), 1)),
            "cell 0 must hold exactly ONE coal, not the whole stack"
        );
        assert_eq!(
            menu.slot_item(4).map(|s| (s.item().to_string(), s.count())),
            Some(("minecraft:stick".to_string(), 1)),
            "cell 3 must hold exactly one stick"
        );
        assert_eq!(
            menu.slot_item(2).map(|s| s.item().to_string()),
            None,
            "cell 1 must stay EMPTY — the 1x2 shape does not occupy it"
        );
        assert_eq!(
            menu.slot_item(12).map(|s| s.count()),
            Some(4),
            "the remainder must be returned to the source slot, not left on the cursor"
        );
        assert_eq!(
            menu.slot_item(20).map(|s| s.count()),
            Some(2),
            "same for the second source"
        );
        assert!(
            menu.carried().is_none(),
            "the cursor must end empty, or the next real click would misbehave"
        );
    }

    /// The negative control for the gate above, and it is **executed**, not
    /// described: the briefed "two clicks per step" sequence, run through the
    /// same menu, must fail the same assertion.
    ///
    /// Without this, "one item per cell" is satisfied by any plan that happens
    /// to place something, and the magnitude — *how many* — is never under test.
    #[test]
    fn two_clicks_per_step_would_dump_the_whole_stack_in_one_cell() {
        let mut menu = Menu::crafting(3, 3);
        menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
        menu.set_slot_item(20, Some(stack("minecraft:stick", 3)));
        let book = torch_book();
        let steps = menu
            .plan_recipe_auto_fill(book.get(&id("minecraft:torch")).expect("recipe"), book.tags())
            .expect("plan");

        // The rejected design: literally two clicks per step, both left.
        let ctx = lodestone_game::click::PlayerCtx::survival();
        for step in &steps {
            Click::left(step.source_slot).apply(&mut menu, ctx);
            Click::left(step.cell).apply(&mut menu, ctx);
        }

        assert_eq!(
            menu.slot_item(1).map(|s| s.count()),
            Some(5),
            "control must observe the WHOLE 5-coal stack in cell 0 — if this ever \
             reads 1, `Click::left` has changed meaning and the real gate above \
             is no longer measuring anything"
        );
        assert_ne!(
            menu.slot_item(1).map(|s| s.count()),
            Some(1),
            "and it must NOT satisfy the real gate's assertion"
        );
    }

    /// One source stack feeding several cells still leaves one item per cell —
    /// the case the "group by source" sequence exists for.
    #[test]
    fn one_source_stack_can_fill_several_cells() {
        let mut menu = Menu::crafting(3, 3);
        menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
        let book = {
            let mut b = RecipeBook::new();
            b.insert(
                id("test:three_coal"),
                Recipe::Shaped(ShapedRecipe::new(
                    3,
                    1,
                    vec![
                        Some(Ingredient::Item(id("minecraft:coal"))),
                        Some(Ingredient::Item(id("minecraft:coal"))),
                        Some(Ingredient::Item(id("minecraft:coal"))),
                    ],
                    stack("minecraft:coal_block", 1),
                )),
            );
            b
        };
        let steps = menu
            .plan_recipe_auto_fill(book.get(&id("test:three_coal")).expect("recipe"), book.tags())
            .expect("plan");
        for click in auto_fill_clicks(&steps) {
            click.apply(&mut menu, lodestone_game::click::PlayerCtx::survival());
        }
        for cell in [1usize, 2, 3] {
            assert_eq!(
                menu.slot_item(cell).map(|s| s.count()),
                Some(1),
                "cell {cell} must hold exactly one coal"
            );
        }
        assert_eq!(
            menu.slot_item(12).map(|s| s.count()),
            Some(2),
            "5 coal minus 3 placed = 2 returned to the source"
        );
    }

    /// The plan is all-or-nothing, so a missing ingredient must issue **no**
    /// clicks at all rather than half-filling the grid.
    #[test]
    fn a_missing_ingredient_issues_no_clicks() {
        let mut menu = Menu::crafting(3, 3);
        menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
        let book = torch_book();
        assert!(
            menu.plan_recipe_auto_fill(
                book.get(&id("minecraft:torch")).expect("recipe"),
                book.tags()
            )
            .is_none(),
            "no stick in the inventory, so there must be no plan"
        );
    }

    // -- the draw pass reaches the screen --------------------------------

    /// Rasterise a colour stream's triangles onto a `res × res` grid in NDC and
    /// report `(covered_cells, bounding_box)` restricted to `rect`, an
    /// `(x0, y0, x1, y1)` NDC box.
    ///
    /// A CPU rasteriser rather than a GPU gate on purpose: this measures whether
    /// the wiring puts geometry **where the panel is**, which is a property of
    /// the vertices, and it runs in every `cargo test` instead of behind an
    /// `#[ignore]`. The bounding box is returned because a bare fraction cannot
    /// tell a uniform-but-wrong frame from a localised blob.
    fn coverage(
        verts: &[f32],
        rect: (f32, f32, f32, f32),
        res: usize,
    ) -> (usize, Option<(f32, f32, f32, f32)>) {
        let (rx0, ry0, rx1, ry1) = rect;
        let mut covered = 0usize;
        let mut bbox: Option<(f32, f32, f32, f32)> = None;
        // Cell centres, in NDC.
        let to_ndc = |i: usize| -1.0 + 2.0 * (i as f32 + 0.5) / res as f32;
        for gy in 0..res {
            for gx in 0..res {
                let (px, py) = (to_ndc(gx), to_ndc(gy));
                if px < rx0 || px > rx1 || py < ry0 || py > ry1 {
                    continue;
                }
                let mut hit = false;
                for tri in verts.chunks_exact(6 * 3) {
                    let (ax, ay) = (tri[0], tri[1]);
                    let (bx, by) = (tri[6], tri[7]);
                    let (cx, cy) = (tri[12], tri[13]);
                    let d = (bx - ax) * (cy - ay) - (cx - ax) * (by - ay);
                    if d.abs() < f32::EPSILON {
                        continue;
                    }
                    let w0 = ((bx - px) * (cy - py) - (cx - px) * (by - py)) / d;
                    let w1 = ((cx - px) * (ay - py) - (ax - px) * (cy - py)) / d;
                    let w2 = 1.0 - w0 - w1;
                    if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                        hit = true;
                        break;
                    }
                }
                if hit {
                    covered += 1;
                    bbox = Some(match bbox {
                        None => (px, py, px, py),
                        Some((x0, y0, x1, y1)) => (x0.min(px), y0.min(py), x1.max(px), y1.max(py)),
                    });
                }
            }
        }
        (covered, bbox)
    }

    /// The panel's own rect in NDC, derived from **the same layout expression the
    /// draw uses** — never a restated constant.
    ///
    /// A HUD gate in this repo hardcoded a `cluster_top` the draw computed from a
    /// moving anchor and reported 0 px for a row that was rendering perfectly.
    /// This calls `recipe_panel_layout` exactly as `recipe_panel_geometry` does.
    fn panel_rect_ndc(panel: &RecipePanelState, menu: &Menu, tabs: usize, pages: usize) -> (f32, f32, f32, f32) {
        let layout = recipe_panel_layout(panel, menu, 1, W, H, tabs, pages);
        let (cw, ch) = crate::menu::render::logical_canvas(1, W, H);
        let r = layout.panel;
        (
            2.0 * r.x / cw - 1.0,
            1.0 - 2.0 * (r.y + r.h) / ch,
            2.0 * (r.x + r.w) / cw - 1.0,
            1.0 - 2.0 * r.y / ch,
        )
    }

    fn open_panel() -> RecipePanelState {
        RecipePanelState {
            open: true,
            ..RecipePanelState::default()
        }
    }

    /// **Every vertex the draw pass will submit must land inside the `[-1, 1]`
    /// NDC clip range.**
    ///
    /// This one sweep catches the entire "geometry exists, nothing is on screen"
    /// class, and it is the sweep that found both of the bugs the panel's own
    /// author hit: tabs at `bx - 30` going off-canvas, and a
    /// `Builder::new(1.0, 1.0, None)` placeholder putting every vertex far
    /// outside the visible range.
    #[test]
    fn every_panel_vertex_lands_inside_the_ndc_clip_range() {
        let menu = Menu::crafting(3, 3);
        let book = torch_book();
        let geo = recipe_panel_geometry(
            Some(&book),
            &open_panel(),
            &menu,
            1,
            None,
            None,
            W,
            H,
        )
        .expect("a crafting table has a recipe book");

        assert!(
            geo.vertex_count() > 0,
            "the open panel must emit geometry at all"
        );
        for (i, v) in geo.verts.chunks_exact(6).enumerate() {
            assert!(
                (-1.0..=1.0).contains(&v[0]) && (-1.0..=1.0).contains(&v[1]),
                "vertex {i} at ({}, {}) is outside the NDC clip range — the panel \
                 would have geometry and draw nothing",
                v[0],
                v[1]
            );
        }
    }

    /// The same sweep at a canvas narrow enough to hit the
    /// `RECIPE_PANEL_MIN_X` clamp, which is where the tabs previously escaped
    /// off-canvas to `x = -1.1218` NDC.
    #[test]
    fn panel_vertices_stay_on_canvas_at_the_min_x_clamp() {
        let menu = Menu::crafting(3, 3);
        let book = torch_book();
        let geo = recipe_panel_geometry(
            Some(&book),
            &open_panel(),
            &menu,
            1,
            None,
            None,
            420,
            400,
        )
        .expect("recipe book");
        for (i, v) in geo.verts.chunks_exact(6).enumerate() {
            assert!(
                (-1.0..=1.0).contains(&v[0]) && (-1.0..=1.0).contains(&v[1]),
                "vertex {i} at ({}, {}) escaped the canvas at the clamp",
                v[0],
                v[1]
            );
        }
    }

    /// **Coverage inside the recipe book's own screen rect.**
    ///
    /// The island this closes could not be seen by any test that only checked
    /// the geometry was *built*: `container.rs`'s 75 tests all passed while the
    /// panel drew nothing. This asserts the vertices actually cover the rect the
    /// layout puts the panel in.
    #[test]
    fn an_open_panel_covers_its_own_screen_rect() {
        let menu = Menu::crafting(3, 3);
        let book = torch_book();
        let panel = open_panel();
        let (tabs, pages, _) = recipe_panel_contents(
            Some(&book),
            &panel,
            lodestone_model::RecipeBookType::Crafting,
        );
        let rect = panel_rect_ndc(&panel, &menu, tabs, pages);
        let geo = recipe_panel_geometry(Some(&book), &panel, &menu, 1, None, None, W, H)
            .expect("recipe book");

        let res = 128;
        let (covered, bbox) = coverage(&geo.verts, rect, res);
        // Total grid cells whose centre falls inside the rect, so the fraction
        // below is "of the panel", not "of the screen".
        let inside = {
            let (mut n, to_ndc) = (0usize, |i: usize| -1.0 + 2.0 * (i as f32 + 0.5) / res as f32);
            for gy in 0..res {
                for gx in 0..res {
                    let (px, py) = (to_ndc(gx), to_ndc(gy));
                    if px >= rect.0 && px <= rect.2 && py >= rect.1 && py <= rect.3 {
                        n += 1;
                    }
                }
            }
            n
        };
        assert!(inside > 0, "the panel rect must contain sample points at all");
        let fraction = covered as f32 / inside as f32;
        assert!(
            fraction > 0.9,
            "an open panel must fill its own rect: covered {covered}/{inside} \
             ({fraction:.3}) inside rect {rect:?}, covered bbox {bbox:?}"
        );
    }

    /// The **executed** negative control for the coverage gate: a *closed*
    /// panel draws only its toggle button, which lives on the container's own
    /// chrome and is nowhere inside the book panel's rect. It must fail the same
    /// assertion.
    ///
    /// This is what distinguishes "the panel is drawn" from "something, anything,
    /// emitted vertices" — and note what else already paints here: nothing, since
    /// this measures the panel geometry's own stream in isolation rather than a
    /// composited frame.
    #[test]
    fn a_closed_panel_fails_the_coverage_assertion() {
        let menu = Menu::crafting(3, 3);
        let book = torch_book();
        let open = open_panel();
        let closed = RecipePanelState::default();
        let (tabs, pages, _) = recipe_panel_contents(
            Some(&book),
            &open,
            lodestone_model::RecipeBookType::Crafting,
        );
        // The *same* rect the positive gate measures — derived from the open
        // layout, so the control differs only in what was drawn.
        let rect = panel_rect_ndc(&open, &menu, tabs, pages);
        let geo = recipe_panel_geometry(Some(&book), &closed, &menu, 1, None, None, W, H)
            .expect("recipe book");

        let (covered, bbox) = coverage(&geo.verts, rect, 128);
        assert_eq!(
            covered, 0,
            "a closed panel must cover NONE of the book rect (bbox {bbox:?}) — if \
             this ever passes, the positive gate above is measuring something \
             other than the panel body"
        );
        assert!(
            geo.vertex_count() > 0,
            "but it must still emit the toggle button, or the control is vacuous \
             for a different reason: nothing drawn at all"
        );
    }

    /// A menu with no recipe book at all draws no panel — so a chest does not
    /// grow a recipe-book toggle.
    #[test]
    fn a_menu_without_a_recipe_book_draws_no_panel() {
        let chest = Menu::generic(27);
        assert!(
            recipe_book_type_for(&chest).is_none(),
            "a chest has no recipe book"
        );
        assert!(
            recipe_panel_geometry(None, &open_panel(), &chest, 1, None, None, W, H).is_none(),
            "and therefore emits no geometry"
        );
    }

    /// The furnace family maps to its own book, not the crafting one — the fork
    /// `Menu::plan_recipe_auto_fill` makes internally, kept in agreement.
    #[test]
    fn book_type_matches_the_menu() {
        assert_eq!(
            recipe_book_type_for(&Menu::crafting(3, 3)),
            Some(lodestone_model::RecipeBookType::Crafting)
        );
    }

    // -- the toast --------------------------------------------------------

    /// The toast reaches [`crate::hud::HudFrame`] from a queue with a real
    /// entry, at a timestamp inside the display window.
    ///
    /// Driven through a `RecipeToastQueue` the test fills itself. That is the
    /// **test-only injection point** this feature needs, and deliberately not a
    /// fake producer in production code: the live producer is the
    /// `recipe_book_add` decode, which does not exist yet.
    #[test]
    fn a_queued_unlock_becomes_a_toast_view() {
        let mut queue = lodestone_game::recipe::RecipeToastQueue::new();
        let now = 1_000_000u64;
        queue.push(id("minecraft:crafting_table"), id("minecraft:torch"), now);

        let view = recipe_toast_view(&queue, now + 100).expect("inside the 5000ms window");
        assert_eq!(view.station.item.to_string(), "minecraft:crafting_table");
        assert_eq!(view.unlocked.item.to_string(), "minecraft:torch");
        assert_eq!(view.visible_portion, 1.0);
    }

    /// The control for the gate above: past the 5000ms window there is no
    /// toast, and an empty queue never produces one. Both must fail the same
    /// `expect`.
    #[test]
    fn the_toast_expires_and_an_empty_queue_never_shows_one() {
        let mut queue = lodestone_game::recipe::RecipeToastQueue::new();
        let now = 1_000_000u64;
        assert!(
            recipe_toast_view(&queue, now).is_none(),
            "an empty queue must not produce a toast — this is the state every \
             real session is in until the decode lands"
        );
        queue.push(id("minecraft:crafting_table"), id("minecraft:torch"), now);
        assert!(
            recipe_toast_view(&queue, now + lodestone_game::recipe::RECIPE_TOAST_DISPLAY_MS).is_none(),
            "and it must expire exactly at DISPLAY_TIME"
        );
    }
}
