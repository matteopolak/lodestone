//! Tests for gameplay key bindings, inventory actions, debug chords, and mouse routing.

use super::*;

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
        (KeyCode::KeyO, KeyOutcome::OpenFriends),
        (KeyCode::F5, KeyOutcome::TogglePerspective),
        // F3 is the debug *modifier*, reporting both edges; the
        // overlay toggle happens on the release when no chord fired (see
        // `resolve_key`, and vanilla's own keyboard handling).
        (KeyCode::F3, KeyOutcome::DebugModifier(true)),
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
fn friends_hotkey_is_rebindable_and_only_resolves_during_gameplay() {
    let mut binds = Keybinds::new();
    binds.set(InputAction::Friends, Binding::Key(KeyCode::KeyY.into()));

    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyY), true, false, None),
        Some(KeyOutcome::OpenFriends)
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyO), true, false, None),
        None,
        "the default key must stop opening Friends after a rebind"
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyY), false, false, None),
        None,
        "releasing the binding must not reopen Friends"
    );
    assert_eq!(
        resolve_key(&binds, KeyGate::default(), Some(KeyCode::KeyY), true, false, None),
        None,
        "the hotkey must not open Friends away from gameplay"
    );
    assert_eq!(
        resolve_key(
            &binds,
            KeyGate { menu: true, gameplay: true, ..KeyGate::default() },
            Some(KeyCode::KeyY),
            true,
            false,
            None,
        ),
        Some(KeyOutcome::Menu),
        "an open menu must retain keyboard focus"
    );
}

#[test]
fn friends_hotkey_effect_opens_the_real_overlay_and_returns_to_pause() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.begin(crate::menu::SessionKind::Multiplayer);
    app.ui.session_ready();

    app.apply_key_outcome(Some(KeyOutcome::OpenFriends), true, Some(KeyCode::KeyO), None);

    assert_eq!(app.ui.screen(), Screen::Friends);
    app.ui.close_friends();
    assert_eq!(app.ui.screen(), Screen::Paused);
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

    // …and the prefix follows the *command binding*, not the physical
    // slash key. Rebinding chat and command to other keys must carry the
    // distinction with them.
    let mut binds = Keybinds::new();
    binds.set(InputAction::Command, Binding::Key(KeyCode::Backquote.into()));
    binds.set(InputAction::Chat, Binding::Key(KeyCode::KeyY.into()));
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::Backquote), true, false, None),
        Some(KeyOutcome::OpenChat { command: true })
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyY), true, false, None),
        Some(KeyOutcome::OpenChat { command: false })
    );
    // The old keys stop opening chat at all.
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::Slash), true, false, None),
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
            // The nine number keys also issue a
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

/// The number keys `1`–`9` **do not** change the selected hotbar
/// slot while a container screen is open; they issue a `ContainerInput::SWAP`
/// with that hotbar index against the hovered slot
/// (the container-screen hotbar-swap key handling,
/// and the number keys are handled in
/// the client-side key handling only when no screen is open).
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
        // loop counter straight through as the button index.
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
        // 2. A key *release* is not a swap. The input handler acts on presses
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

/// The off-hand key's container half.
///
/// The off-hand binding defaults to `F`. It must remain distinct from the
/// container's other bindings, and this assertion checks that the key
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
    // 2. A release is not a swap — the input handler acts on presses only.
    assert_eq!(resolve(gate, KeyCode::KeyF, false), None);
    // 3. **The gameplay half is a different outcome, not the same one.**
    //    This line intentionally exercises the gameplay half: with no screen
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

/// The gameplay half: `F` in the world **reaches the wire** as
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
/// (vanilla's own client-side check, re-checked server-side too).
///
/// Watched failing: with the `Spectator` arm removed,
/// `offhand_swap_action(Spectator)` returns the action and the first
/// assertion below reports `Some(SwapItemWithOffhand)`.
///
/// The other three modes are the positive control. Without them this passes
/// just as well against a function that returns `None` unconditionally — i.e.
/// against the feature not existing at all, which is the state an absent
/// feature would produce.
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
// `Click::drop_one`/`drop_stack`/`do_throw` (`lodestone-game`) and
// `ClientAction::DropSelectedItem`/`DropSelectedItemStack` were each built,
// encoded and round-trip tested with zero producers before this. One
// binding closes both — see `InputAction::Drop`'s and `KeyOutcome::
// ContainerDrop`/`Drop`'s docs for the source behavior this mirrors.

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
    // A release does nothing — vanilla's own key-click consumption only
    // ever fires on the down edge.
    assert_eq!(resolve(playing(), KeyCode::KeyQ, false), None);
}

/// The container half — vanilla's own container-screen key handling
/// reached through `resolve_key`'s `container_open` arm.
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

/// The drop action must not be swallowed as an unrecognised key behind an open
/// container. This negative control simulates an unbound `InputAction::Drop`
/// and verifies the corresponding gameplay path remains distinct.
#[test]
fn an_unbound_drop_key_is_swallowed_behind_a_container_and_dead_in_the_world() {
    let mut binds = Keybinds::new();
    binds.set(InputAction::Drop, Binding::Unbound);
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve_key(&binds, gate, Some(KeyCode::KeyQ), true, false, None),
        None,
        "watched failing before this test existed: with the real binding \
         still assigned, this line reported Some(ContainerDrop {{ .. }})"
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyQ), true, false, None),
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
/// — same shape as `a_spectator_does_not_send_
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
        resolve_key(&Keybinds::new(), gate, None, true, false, None),
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
        debug_held: true,
        recipe_search: true,
        creative_search: true,
        anvil_rename_active: true,
        spectator: false,
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
            KeyCode::F3 => assert_eq!(got, Some(KeyOutcome::DebugModifier(true))),
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
        KeyCode::Escape,
        KeyCode::Digit1,
    ] {
        assert_eq!(
            resolve(playing(), one_shot, false),
            None,
            "{one_shot:?} must not fire on release"
        );
    }
    // F3 is deliberately *not* in that list any more: it is the
    // debug modifier, so it reports both edges, and the driver toggles the
    // overlay on the release when no chord fired.
    assert_eq!(
        resolve(playing(), KeyCode::F3, false),
        Some(KeyOutcome::DebugModifier(false))
    );
}

/// F3+B and F3+G resolve to their sub-modes only while the modifier is held, and
/// a plain B or G is untouched.
///
/// The negative half is the point: `B` and `G` are unbound in the default table,
/// so if the chord arms ignored `debug_held` they would fire on every press and
/// the assertion below would catch it.
#[test]
fn the_debug_chords_need_the_modifier_held() {
    let held = KeyGate {
        gameplay: true,
        debug_held: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(held, KeyCode::KeyB, true),
        Some(KeyOutcome::ToggleHitboxes)
    );
    assert_eq!(
        resolve(held, KeyCode::KeyG, true),
        Some(KeyOutcome::ToggleChunkBorders)
    );
    // Release is not a chord — a chord that fired on both edges would toggle
    // twice per keystroke and appear to do nothing.
    assert_eq!(resolve(held, KeyCode::KeyB, false), None);

    // Without the modifier, neither key means anything.
    assert_eq!(resolve(playing(), KeyCode::KeyB, true), None);
    assert_eq!(resolve(playing(), KeyCode::KeyG, true), None);
}

/// Shift+F3 (the profiler pie chart toggle) and its own F3+number navigation
/// resolve only while the modifier is held — the same shape
/// [`the_debug_chords_need_the_modifier_held`] checks for F3+B/F3+G, and for
/// the same reason: the number row is the (rebindable) hotbar selector, so a
/// chord arm that ignored `debug_held` would fire on every ordinary hotbar
/// press and the negative half below would catch it.
#[test]
fn the_profiler_chart_chords_need_the_modifier_held() {
    let held = KeyGate {
        gameplay: true,
        debug_held: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(held, KeyCode::ShiftLeft, true),
        Some(KeyOutcome::ToggleProfilerChart)
    );
    assert_eq!(
        resolve(held, KeyCode::ShiftRight, true),
        Some(KeyOutcome::ToggleProfilerChart)
    );
    // Release is not a chord, matching every other F3 chord — unlike B/G
    // (unbound by default), Shift is also the sneak binding, so its release still
    // falls through to an ordinary (harmless, since sneak was never pressed
    // through this path) `Movement` release rather than to `None`.
    assert_ne!(
        resolve(held, KeyCode::ShiftLeft, false),
        Some(KeyOutcome::ToggleProfilerChart)
    );

    // Digit1..Digit8 drill into wedges 0..8; Digit0 returns to the root.
    assert_eq!(
        resolve(held, KeyCode::Digit1, true),
        Some(KeyOutcome::ProfilerChartSelect(Some(0)))
    );
    assert_eq!(
        resolve(held, KeyCode::Digit8, true),
        Some(KeyOutcome::ProfilerChartSelect(Some(7)))
    );
    assert_eq!(
        resolve(held, KeyCode::Digit0, true),
        Some(KeyOutcome::ProfilerChartSelect(None))
    );
    // Digit9 is not a profiler-chart key (only eight phases exist), so it
    // falls through to whatever it would otherwise resolve to — here, the
    // ordinary (default-bound) hotbar slot 9 selection, since F3 held only
    // intercepts the specific keys it lists, exactly like every other
    // debug-held chord.
    assert_eq!(
        resolve(held, KeyCode::Digit9, true),
        Some(KeyOutcome::SelectSlot(8))
    );

    // Without the modifier, Shift is sneak (`Movement`) and the digits select
    // hotbar slots — both remain ordinary gameplay bindings.
    assert_ne!(
        resolve(playing(), KeyCode::ShiftLeft, true),
        Some(KeyOutcome::ToggleProfilerChart)
    );
    assert_ne!(
        resolve(playing(), KeyCode::Digit1, true),
        Some(KeyOutcome::ProfilerChartSelect(Some(0)))
    );
}

/// F3+P (pause on lost focus) and F3+C (copy location) — the same
/// modifier-gated shape [`the_debug_chords_need_the_modifier_held`] checks
/// for F3+B/F3+G, extended to the two chords covered here. `P` and `C`
/// are unbound in the default table (like `B`/`G`), so the negative half is
/// real: a chord that ignored `debug_held` would fire on every plain press.
#[test]
fn the_pause_and_copy_location_chords_need_the_debug_modifier() {
    let held = KeyGate {
        gameplay: true,
        debug_held: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(held, KeyCode::KeyP, true),
        Some(KeyOutcome::TogglePauseOnLostFocus)
    );
    assert_eq!(
        resolve(held, KeyCode::KeyC, true),
        Some(KeyOutcome::CopyLocation)
    );
    // Release is not a chord, same reason as F3+B/F3+G.
    assert_eq!(resolve(held, KeyCode::KeyP, false), None);
    assert_eq!(resolve(held, KeyCode::KeyC, false), None);

    // Without the modifier, neither key means anything.
    assert_eq!(resolve(playing(), KeyCode::KeyP, true), None);
    assert_eq!(resolve(playing(), KeyCode::KeyC, true), None);
}

/// The exact vanilla wording `debug_shown_feedback`/`debug_enabled_feedback`
/// produce, predicted from vanilla's own translated-debug-feedback
/// call sites and the `en_us.json` strings they resolve
/// (`debug.show_hitboxes.on`/`.off`, `debug.chunk_boundaries.on`/`.off`,
/// `debug.advanced_tooltips.on`/`.off`, `debug.pause_focus.on`/`.off`) —
/// not the round number, the exact byte string including the legacy `§`
/// codes vanilla's own debug feedback decoration applies (`§e` yellow, `§l` bold, `§r`
/// reset before the un-styled body).
#[test]
fn debug_feedback_helpers_match_vanillas_exact_wording_and_legacy_codes() {
    assert_eq!(debug_feedback("hi"), "§e§l[Debug]:§r hi");
    assert_eq!(
        debug_shown_feedback("Hitboxes", true),
        "§e§l[Debug]:§r Hitboxes: shown"
    );
    assert_eq!(
        debug_shown_feedback("Hitboxes", false),
        "§e§l[Debug]:§r Hitboxes: hidden"
    );
    assert_eq!(
        debug_shown_feedback("Chunk borders", true),
        "§e§l[Debug]:§r Chunk borders: shown"
    );
    assert_eq!(
        debug_shown_feedback("Advanced tooltips", false),
        "§e§l[Debug]:§r Advanced tooltips: hidden"
    );
    assert_eq!(
        debug_enabled_feedback("Pause on lost focus", true),
        "§e§l[Debug]:§r Pause on lost focus: enabled"
    );
    assert_eq!(
        debug_enabled_feedback("Pause on lost focus", false),
        "§e§l[Debug]:§r Pause on lost focus: disabled"
    );
}

/// The colour actually reaches a vertex without a hex span or the legacy
/// string path (`Text::to_legacy_string`) touching this at all — the exact
/// concern the brief names, because `to_legacy_string` cannot carry an RGB
/// colour and this repo already has a defect class where a coloured message
/// silently lost its colour through it.
///
/// `Text::literal(debug_feedback(msg)).to_spans()` is production's own
/// expansion path (`Text::to_spans`'s own doc: "`from_legacy` consumes every
/// `§`+code pair"), the same one `ChatLog::recent_ages_spans` uses for the
/// HUD's real chat draw — not a hand-rolled parser this test invented.
///
/// **Negative control, in the same assertion set:** the body span carries no
/// colour and no bold, so a version of `debug_feedback` that coloured the
/// *whole* line (an easy way to get this "working" by accident) fails here.
#[test]
fn debug_feedback_expands_to_a_bold_yellow_prefix_span_and_a_plain_body_span() {
    use lodestone_model::text::{Text, TextColor};

    let spans = Text::literal(debug_shown_feedback("Hitboxes", true)).resolve(&|_| None).to_spans();
    assert_eq!(spans.len(), 2, "a coloured prefix run and a plain body run: {spans:?}");

    assert_eq!(spans[0].text, "[Debug]:");
    assert_eq!(spans[0].style.color, Some(TextColor::Yellow));
    assert_eq!(spans[0].style.bold, Some(true));

    assert_eq!(spans[1].text, " Hitboxes: shown");
    assert_eq!(
        spans[1].style.color, None,
        "the body must not inherit or carry a colour of its own"
    );
    assert_eq!(
        spans[1].style.bold, None,
        "§r resets bold before the body, so it must not read as bold"
    );
}

/// `KeyOutcome::CopyLocation`'s exact wire format, predicted from
/// vanilla's own debug copy-location format string ("/execute in %s run
/// tp @s %.2f %.2f %.2f %.2f %.2f") — not the round number, and every
/// numeric field pairwise-distinct (`CLAUDE.md`'s transposition rule) so a
/// swapped x/y/z or yaw/pitch fails here rather than round-tripping silently.
#[test]
fn copy_location_command_matches_vanillas_execute_format_with_distinct_fields() {
    assert_eq!(
        copy_location_command("minecraft:the_nether", [11.5, 64.25, -8.125], 91.5, -12.75),
        "/execute in minecraft:the_nether run tp @s 11.50 64.25 -8.12 91.50 -12.75"
    );
}

/// The whole chain, through the real `ChatLog` production code pushes
/// through (`Sim::push_local_chat`/`Sim::recent_chat_spans`) rather than a
/// hand-built `Text` — a plain literal string carrying `§` codes really does
/// survive a round trip through the same feed the HUD reads.
///
/// **Negative control:** a plain message pushed alongside it (no `§` codes)
/// comes back as one unstyled span, proving the expansion is conditional on
/// the codes actually being present rather than every chat line silently
/// gaining a colour.
#[test]
fn pushing_debug_feedback_through_the_real_chat_log_survives_as_a_bold_yellow_span() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.sim.push_local_chat("plain status line");
    app.sim
        .push_local_chat(debug_shown_feedback("Chunk borders", false));

    let recent = app.sim.recent_chat_spans(2);
    assert_eq!(recent.len(), 2, "both lines must be retained: {recent:?}");

    let (plain_spans, _) = &recent[0];
    assert_eq!(plain_spans.len(), 1);
    assert_eq!(plain_spans[0].text, "plain status line");
    assert_eq!(plain_spans[0].style.color, None);

    let (debug_spans, _) = &recent[1];
    assert_eq!(debug_spans.len(), 2, "{debug_spans:?}");
    assert_eq!(debug_spans[0].text, "[Debug]:");
    assert_eq!(
        debug_spans[0].style.color,
        Some(lodestone_model::text::TextColor::Yellow)
    );
    assert_eq!(debug_spans[1].text, " Chunk borders: hidden");
}

/// The end-to-end gap `resolve_key`'s own tests cannot see: the owner
/// reported F3+B/F3+G producing *no chat feedback at all* — not thin lines,
/// nothing — while F3+H, driven the same way, worked. A resolver-level
/// assertion (`the_debug_chords_need_the_modifier_held`) already proves all
/// three `KeyOutcome`s are *produced* correctly; this drives them through
/// [`WindowApp::apply_key_outcome`] — the real effect half of
/// `handle_keyboard_input`, split out because winit's `KeyEvent` cannot be
/// constructed outside winit itself (a private `platform_specific` field),
/// which is exactly why nothing before this test reached past the resolver —
/// and asserts on the *real* atomics and the *real* chat log, side by side
/// with F3+H as the owner's own working control.
#[test]
fn f3_b_and_f3_g_flip_their_atomic_and_push_chat_through_the_real_key_path() {
    use std::sync::atomic::Ordering;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });

    // F3 down — the same `DebugModifier(true)` outcome a real F3 keydown
    // resolves to, driven through the real effect path.
    app.apply_key_outcome(Some(KeyOutcome::DebugModifier(true)), true, Some(KeyCode::F3), None);
    assert!(app.debug_held, "F3 down must set debug_held through the real path");

    app.apply_key_outcome(Some(KeyOutcome::ToggleHitboxes), true, Some(KeyCode::KeyB), None);
    assert!(
        app.debug_hitboxes.load(Ordering::Relaxed),
        "F3+B must flip the hitboxes atomic through the real path"
    );

    app.apply_key_outcome(Some(KeyOutcome::ToggleChunkBorders), true, Some(KeyCode::KeyG), None);
    assert!(
        app.debug_chunk_borders.load(Ordering::Relaxed),
        "F3+G must flip the chunk-borders atomic through the real path"
    );

    let tooltips_before = app.nav.advanced_item_tooltips();
    app.apply_key_outcome(Some(KeyOutcome::ToggleAdvancedTooltips), true, Some(KeyCode::KeyH), None);
    assert_ne!(
        app.nav.advanced_item_tooltips(),
        tooltips_before,
        "F3+H must flip the tooltip option through the real path (the owner's own working control)"
    );

    let recent = app.sim.recent_chat_spans(3);
    assert_eq!(
        recent.len(),
        3,
        "all three chords must each push exactly one chat line through the real path, got {recent:?}"
    );
    let text_of = |spans: &[lodestone_model::text::TextSpan]| -> String {
        spans.iter().map(|s| s.text.clone()).collect::<String>()
    };
    assert!(
        text_of(&recent[0].0).contains("Hitboxes"),
        "F3+B's chat line is missing or wrong: {:?}",
        recent[0]
    );
    assert!(
        text_of(&recent[1].0).contains("Chunk borders"),
        "F3+G's chat line is missing or wrong: {:?}",
        recent[1]
    );
    assert!(
        text_of(&recent[2].0).contains("Advanced tooltips"),
        "F3+H's chat line is missing or wrong: {:?}",
        recent[2]
    );
}

/// F3+P's toggle+persist half, through the real `MenuNav` — the same shape
/// `toggle_advanced_item_tooltips` already has no dedicated test for, closed
/// here because it exercises the option's in-memory toggle. Persistence (writing no
/// key when untouched, degrading a garbled value to vanilla's `true`) is
/// covered by `config.rs`'s
/// `pause_on_lost_focus_defaults_on_and_only_writes_a_key_when_turned_off`;
/// this is the in-memory toggle the F3+P driver arm actually calls.
#[test]
fn toggle_pause_on_lost_focus_flips_the_option_both_ways() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    assert!(
        app.nav.pause_on_lost_focus(),
        "vanilla's own default is on"
    );
    app.nav.toggle_pause_on_lost_focus();
    assert!(!app.nav.pause_on_lost_focus());
    app.nav.toggle_pause_on_lost_focus();
    assert!(app.nav.pause_on_lost_focus());
}

#[test]
fn a_rebind_moves_the_behaviour_to_the_new_key_and_off_the_old_one() {
    let mut binds = Keybinds::new();
    binds.set(InputAction::Inventory, Binding::Key(KeyCode::KeyI.into()));
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyI), true, false, None),
        Some(KeyOutcome::OpenContainer)
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyE), true, false, None),
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
        resolve_key(&binds, gate, Some(KeyCode::KeyI), true, false, None),
        Some(KeyOutcome::CloseContainer)
    );
    assert_eq!(
        resolve_key(&binds, gate, Some(KeyCode::KeyE), true, false, None),
        None
    );
}

#[test]
fn unbinding_an_action_disables_it_without_disturbing_the_rest() {
    let mut binds = Keybinds::new();
    binds.set(InputAction::Jump, Binding::Unbound);
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::Space), true, false, None),
        None
    );
    // The neighbouring arms are untouched.
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyW), true, false, None),
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
    binds.set(InputAction::Attack, Binding::Key(KeyCode::KeyR.into()));
    binds.set(InputAction::Use, Binding::Key(KeyCode::KeyV.into()));
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyR), true, false, None),
        Some(KeyOutcome::Attack(true))
    );
    // Hold-to-dig: the release edge must arrive, or mining never stops.
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyR), false, false, None),
        Some(KeyOutcome::Attack(false))
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyV), true, false, None),
        Some(KeyOutcome::Use(true))
    );
    // The release edge must arrive too, or `ReleaseUseItem` never sends —
    // the exact bug this test's sibling assertions exist to catch (a bow
    // or shield cannot complete a use without it).
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyV), false, false, None),
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
    // Middle **is** a gameplay binding now: the pick-item action defaults to
    // the middle mouse button, so it is the primary route for
    // pick-item rather than a rebound one. This assertion previously read
    // `None`, which was correct only while pick-item did not exist — the
    // premise went stale when the binding landed, not the code.
    assert_eq!(
        mouse_action_for(&binds, MouseButton::Middle),
        Some(InputAction::PickItem)
    );

    // Swapping the two buttons is a supported rebind.
    let mut swapped = binds;
    swapped.set(InputAction::Attack, Binding::Mouse(MouseButton::Right.into()));
    swapped.set(InputAction::Use, Binding::Mouse(MouseButton::Left.into()));
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
    binds.set(InputAction::Jump, Binding::Mouse(MouseButton::Middle.into()));
    let action = mouse_action_for(&binds, MouseButton::Middle);
    assert_eq!(action, Some(InputAction::Jump));
    assert_eq!(action.and_then(InputAction::movement), Some(Action::Jump));
}

#[test]
fn an_unnameable_physical_key_is_ignored_by_the_binding_chain() {
    // `PhysicalKey::Unidentified` reaches the menu and chat arms (tested
    // above) but must not match any binding — there is nothing to match on.
    assert_eq!(
        resolve_key(&Keybinds::new(), playing(), None, true, false, None),
        None
    );
}
