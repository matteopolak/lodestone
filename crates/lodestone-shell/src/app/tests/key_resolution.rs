//! Tests for key precedence, capture, and binding resolution.

use super::*;

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
    resolve_key(&Keybinds::new(), gate, Some(code), pressed, false, None)
}

/// A plugin's `Consume` claim on a physical key wins over
/// gameplay when nothing else has first claim on the keyboard — the
/// positive half of the precedence-rank doc on `resolve_key`.
#[test]
fn a_plugin_consume_claim_wins_over_an_unbound_key_during_gameplay() {
    let binds = Keybinds::new();
    // `KeyCode::F13` is not bound to anything in the default table, so
    // absent the plugin claim this would resolve to `None` — proof the
    // outcome came from the claim, not from an incidental keybind.
    let outcome = resolve_key(
        &binds,
        playing(),
        Some(KeyCode::F13),
        true,
        false,
        Some(lodestone_ecs::KeyInterceptMode::Consume),
    );
    assert_eq!(outcome, Some(KeyOutcome::PluginConsumed));
}

/// Both edges reach `PluginConsumed`, not just the press — the same
/// both-edges requirement `Attack`/`Use` have, and the reason the arm has no
/// `&& pressed` guard.
#[test]
fn a_plugin_consume_claim_fires_on_release_too() {
    let binds = Keybinds::new();
    let outcome = resolve_key(
        &binds,
        playing(),
        Some(KeyCode::F13),
        false,
        false,
        Some(lodestone_ecs::KeyInterceptMode::Consume),
    );
    assert_eq!(outcome, Some(KeyOutcome::PluginConsumed));
}

/// The precedence-rank claim itself: a plugin's `Consume` claim on a key that
/// is *also* bound to a real gameplay action (here, forward movement) still
/// wins — a plugin hotkey cannot be shadowed by a coincidental rebind onto
/// the same physical key, matching `resolve_key`'s own doc.
#[test]
fn a_plugin_consume_claim_outranks_a_real_gameplay_binding_on_the_same_key() {
    let binds = Keybinds::new();
    let outcome = resolve_key(
        &binds,
        playing(),
        Some(KeyCode::KeyW), // bound to `InputAction::Forward` by default
        true,
        false,
        Some(lodestone_ecs::KeyInterceptMode::Consume),
    );
    assert_eq!(outcome, Some(KeyOutcome::PluginConsumed));
}

/// The negative control this whole design turns on: `Observe` mode must
/// change nothing about resolution — the plugin is told about the key
/// elsewhere (see the driver call site), but `resolve_key` itself must
/// resolve exactly as if no plugin existed.
#[test]
fn a_plugin_observe_claim_does_not_change_resolution() {
    let binds = Keybinds::new();
    let with_observe = resolve_key(
        &binds,
        playing(),
        Some(KeyCode::KeyW),
        true,
        false,
        Some(lodestone_ecs::KeyInterceptMode::Observe),
    );
    let with_no_plugin = resolve_key(&binds, playing(), Some(KeyCode::KeyW), true, false, None);
    assert_eq!(with_observe, with_no_plugin);
    // Sharpen the assertion: this must be the real movement outcome, not two
    // fixtures that coincidentally agree by both being `None`.
    assert!(matches!(with_observe, Some(KeyOutcome::Movement(_, true))));
}

/// A container screen keeps first claim over a plugin's `Consume` mode —
/// `resolve_key`'s own doc states this ranking, and this is the control that
/// actually exercises it rather than trusting the doc comment.
#[test]
fn an_open_container_still_outranks_a_plugin_consume_claim() {
    let binds = Keybinds::new();
    let gate = KeyGate {
        container_open: true,
        gameplay: true,
        ..KeyGate::default()
    };
    // The inventory-close binding still fires, exactly as it would with no
    // plugin involved at all — the container arm's own catch-all runs
    // first and the plugin arm below it is never reached.
    let outcome = resolve_key(
        &binds,
        gate,
        Some(KeyCode::KeyE), // `InputAction::Inventory`'s default binding
        true,
        false,
        Some(lodestone_ecs::KeyInterceptMode::Consume),
    );
    assert_eq!(outcome, Some(KeyOutcome::CloseContainer));
}

/// The function-key path: an F-key has no printable `text`, so it is
/// exactly the case `menu_key_for` drops and `capture_key_for` must not.
/// `F1` (not `F5`, which `resolve_key`'s own default table already binds
/// to `TogglePerspective` — picking a bound key here would prove nothing
/// about the *unbound*, no-text case a real Controls-menu rebind targets)
/// persists as the standard function-key identifier.
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
