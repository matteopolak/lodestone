//! Tests for platform shortcut translation into menu editing events.

use super::*;

use crate::app::menus::shortcut_modifier_held;

/// **The macOS mapping specifically.** `shortcut_modifier_held` takes
/// `is_macos` as a parameter rather than reading `cfg!(target_os =
/// "macos")` inline precisely so this is assertable on any machine the
/// suite happens to run on — the bug this test targets is invisible on
/// Linux/Windows by construction (Ctrl already worked there), so a test
/// that only ever exercised whichever OS runs CI would not have caught
/// it.
#[test]
fn macos_shortcut_modifier_is_cmd_not_ctrl() {
    assert!(shortcut_modifier_held(ModifiersState::SUPER, true));
    assert!(!shortcut_modifier_held(ModifiersState::CONTROL, true));
}

#[test]
fn non_macos_shortcut_modifier_is_ctrl_not_cmd() {
    assert!(shortcut_modifier_held(ModifiersState::CONTROL, false));
    assert!(!shortcut_modifier_held(ModifiersState::SUPER, false));
}

/// The two platform splits, each paired with the modifier that platform
/// actually uses for edit shortcuts.
///
/// Every gate below runs against **both**, through
/// `menu_key_for_platform`, rather than against `ModifiersState::SUPER`
/// alone through `menu_key_for`. The old form read `cfg!(target_os =
/// "macos")` inside the function, so `SUPER` meant "shortcut" only when
/// the suite happened to be running on a Mac: these five gates passed on
/// the dev machines and failed on every Linux CI runner, which reads as
/// an environment fault and is really the test asserting a property of
/// the host. Driving both splits is also strictly stronger — the
/// non-macOS mapping was previously asserted by nothing but
/// `non_macos_shortcut_modifier_is_ctrl_not_cmd`, one layer below this
/// translation.
const SHORTCUT_PLATFORMS: [(bool, ModifiersState); 2] = [
    (true, ModifiersState::SUPER),
    (false, ModifiersState::CONTROL),
];

fn platform_name(is_macos: bool) -> &'static str {
    if is_macos { "macOS/Cmd" } else { "non-macOS/Ctrl" }
}

/// The literal reported symptom: with the shortcut modifier held, `A`
/// must produce `MenuKey::SelectAll`, never `MenuKey::Char('a')` — and
/// critically, `text` is non-empty here (winit still reports `Some("a")`
/// alongside the physical key), which is exactly what made the old
/// modifier-blind `menu_key_for` insert the letter.
#[test]
fn cmd_a_selects_all_and_never_types_a() {
    // Collected rather than asserted inside the loop: an `assert!` there
    // stops at the first split and leaves the other unmeasured, so a
    // failure would name one platform when both may be wrong.
    let mut mismatches = Vec::new();
    for (is_macos, modifier) in SHORTCUT_PLATFORMS {
        let key = WindowApp::menu_key_for_platform(
            PhysicalKey::Code(KeyCode::KeyA),
            Some("a"),
            modifier,
            is_macos,
        );
        if key != Some(MenuKey::SelectAll) {
            mismatches.push(format!(
                "{}: got {key:?}, expected SelectAll (and never Char('a'))",
                platform_name(is_macos)
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Same shape for paste — "it just inserts a v".
#[test]
fn cmd_v_pastes_and_never_types_v() {
    let mut mismatches = Vec::new();
    for (is_macos, modifier) in SHORTCUT_PLATFORMS {
        let key = WindowApp::menu_key_for_platform(
            PhysicalKey::Code(KeyCode::KeyV),
            Some("v"),
            modifier,
            is_macos,
        );
        if key != Some(MenuKey::Paste) {
            mismatches.push(format!(
                "{}: got {key:?}, expected Paste (and never Char('v'))",
                platform_name(is_macos)
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn cmd_c_copies_and_cmd_x_cuts() {
    let mut mismatches = Vec::new();
    for (is_macos, modifier) in SHORTCUT_PLATFORMS {
        for (code, letter, want) in [
            (KeyCode::KeyC, "c", MenuKey::Copy),
            (KeyCode::KeyX, "x", MenuKey::Cut),
        ] {
            let key = WindowApp::menu_key_for_platform(
                PhysicalKey::Code(code),
                Some(letter),
                modifier,
                is_macos,
            );
            if key != Some(want) {
                mismatches.push(format!(
                    "{} + {letter}: got {key:?}, expected {want:?}",
                    platform_name(is_macos)
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// A plain, unmodified `a` must still type — the negative control proving
/// the suppression above is conditional on the modifier and not a
/// blanket regression that breaks ordinary typing.
#[test]
fn plain_a_still_types() {
    assert_eq!(
        WindowApp::menu_key_for(
            PhysicalKey::Code(KeyCode::KeyA),
            Some("a"),
            ModifiersState::empty()
        ),
        Some(MenuKey::Char('a'))
    );
}

/// Cmd+Shift+A is not select-all in vanilla either
/// (vanilla's own select-all check requires shift to be up) — and must
/// not fall through to typing `a` alongside doing nothing, matching
/// `focus::KeyEvent::is_edit_shortcut`'s own guard.
#[test]
fn cmd_shift_a_is_neither_select_all_nor_text() {
    let mut mismatches = Vec::new();
    for (is_macos, modifier) in SHORTCUT_PLATFORMS {
        let key = WindowApp::menu_key_for_platform(
            PhysicalKey::Code(KeyCode::KeyA),
            Some("A"),
            modifier | ModifiersState::SHIFT,
            is_macos,
        );
        if key.is_some() {
            mismatches.push(format!(
                "{}: got {key:?}, expected None (neither SelectAll nor Char('A'))",
                platform_name(is_macos)
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// An unrecognised chord (the modifier held, but not one of A/C/X/V) must
/// still suppress the letter rather than falling through to it — the
/// "shipping only modifier tracking turns 'types a v' into 'does
/// nothing', which reads as a new bug" case the test guards against, but for
/// a key with no dedicated shortcut this *is* the correct behaviour:
/// vanilla does not type `b` while Cmd is held either.
#[test]
fn unrecognised_cmd_chord_suppresses_the_letter() {
    let mut mismatches = Vec::new();
    for (is_macos, modifier) in SHORTCUT_PLATFORMS {
        let key = WindowApp::menu_key_for_platform(
            PhysicalKey::Code(KeyCode::KeyB),
            Some("b"),
            modifier,
            is_macos,
        );
        if key.is_some() {
            mismatches.push(format!(
                "{}: got {key:?}, expected None (the letter must not fall through)",
                platform_name(is_macos)
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// `from_menu_key` is the other half of the fix — the `MenuKey` produced
/// above has to reach `EditBox::handle_key` as a real
/// `is_select_all()`/`is_paste()` event, or select-all/paste would be
/// silently declined despite being detected correctly here.
#[test]
fn select_all_and_paste_reach_editbox_as_real_shortcut_events() {
    use crate::menu::focus::KeyEvent;

    let select_all = KeyEvent::from_menu_key(MenuKey::SelectAll).unwrap();
    assert!(select_all.is_select_all());

    let paste = KeyEvent::from_menu_key(MenuKey::Paste).unwrap();
    assert!(paste.is_paste());

    let copy = KeyEvent::from_menu_key(MenuKey::Copy).unwrap();
    assert!(copy.is_copy());

    let cut = KeyEvent::from_menu_key(MenuKey::Cut).unwrap();
    assert!(cut.is_cut());
}
