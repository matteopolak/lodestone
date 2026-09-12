use super::*;
#[test]
fn session_phase_tracks_net_updates() {
    use crate::net::NetUpdate;
    use lodestone_model::Text;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    // Before any connection: purely local.
    assert_eq!(sim.session_phase(), SessionPhase::LocalOnly);

    // Attaching a live connection moves us to Connecting immediately, so the
    // menu shows a loading screen rather than a lie.
    sim.attach_net(net);
    assert_eq!(sim.session_phase(), SessionPhase::Connecting);

    // LoggedIn ⇒ Connected (the menu's "session_ready").
    feed.send(NetUpdate::LoggedIn { entity_id: 42 }).unwrap();
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected);

    // A mid-game disconnect ⇒ Ended with the reason preserved, which is what
    // drives the menu's Error screen. Assert the reason survives, so a
    // blank/again-Connected mapping can't pass. `"Server closed"` is a
    // synthetic, not-a-vanilla-key reason (see `NetUpdate::Disconnected`'s
    // doc comment), hence `Text::literal` rather than `Text::translate`;
    // the translation-key path is covered separately by
    // `disconnect_reason_is_translated_through_the_language_table`.
    feed.send(NetUpdate::Disconnected(Box::new(Text::literal(
        "Server closed",
    ))))
    .unwrap();
    sim.poll_net();
    match sim.session_phase() {
        SessionPhase::Ended(end) => {
            let reason = end.plain();
            assert!(reason.contains("Server closed"), "reason lost: {reason}");
            assert_eq!(
                end.kind,
                crate::sim::SessionEndKind::Disconnected,
                "a server-sent disconnect is not a client-side failure"
            );
            assert!(
                !reason.starts_with("disconnected: "),
                "the prefix was ours, not vanilla's, and it is gone: {reason}"
            );
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

/// Control for the two tests below: proves the "no raw key reaches the
/// screen" assertion can actually fail, i.e. it is discriminating rather
/// than vacuous (`CLAUDE.md`'s evidence standard). `test_config()` is
/// `Mode::Headless`, so `Sim::new` always takes the demo-palette path
/// (`BlockResources::load(false)`), which never loads a language table —
/// `sim.language` is deterministically `None` here regardless of the
/// environment. With no table, `resolve_text` still lowers the
/// `Translate` node (via `Text::resolve`), but with
/// nothing to translate it and no `fallback` set, it falls back to the
/// key itself — reproducing byte-for-byte the pre-#68 defect
/// (`net::forward` used to send `reason.to_plain_string()`, which hits
/// the same "no match, no fallback ⇒ render the key" path against its
/// own tiny built-in table). If this ever changed to also disappear the
/// key, the positive test below would no longer be proof of anything.
#[test]
fn disconnect_reason_without_a_language_table_falls_back_to_the_raw_key() {
    use crate::net::NetUpdate;
    use lodestone_model::Text;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    assert!(
        sim.language.is_none(),
        "control's premise requires no language table loaded"
    );
    sim.attach_net(net);
    feed.send(NetUpdate::Disconnected(Box::new(Text::translate(
        "multiplayer.disconnect.kicked",
        vec![],
    ))))
    .unwrap();
    sim.poll_net();
    match sim.session_phase() {
        SessionPhase::Ended(end) => {
            let reason = end.plain();
            assert!(
                reason.contains("multiplayer.disconnect.kicked"),
                "control failed to reproduce the raw-key defect: {reason}"
            );
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

/// The proof: a real translation key reaches `Screen::Error`
/// as the real English vanilla ships for it, not as the raw key. The
/// expected string is not this test's own formatter's output — it is
/// copied verbatim from the real vanilla `en_us.json`
/// (`.cache/mc/26.2/src/assets/minecraft/lang/en_us.json:5773`,
/// `"multiplayer.disconnect.kicked": "Kicked by an operator"`), i.e. a
/// hand-decoded spec example per `CLAUDE.md`'s evidence standard, so
/// this can't pass by agreeing with itself. The fixture below carries
/// only that one real entry rather than the whole ~500 KiB table so the
/// test stays hermetic and has no `client.jar`/`LODESTONE_ASSETS`
/// dependency that could go missing in CI — `Language::from_json_bytes`
/// is the same parser [`crate::resources::BlockResources::try_vanilla`]
/// feeds the real file through, so this is not a bespoke lookup path.
#[test]
fn disconnect_reason_is_translated_through_the_language_table() {
    use crate::net::NetUpdate;
    use lodestone_assets::Language;
    use lodestone_model::Text;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    let lang = Language::from_json_bytes(
        br#"{"multiplayer.disconnect.kicked": "Kicked by an operator"}"#,
    )
    .expect("valid language JSON");
    sim.language = Some(Arc::new(lang));
    sim.attach_net(net);
    feed.send(NetUpdate::Disconnected(Box::new(Text::translate(
        "multiplayer.disconnect.kicked",
        vec![],
    ))))
    .unwrap();
    sim.poll_net();
    match sim.session_phase() {
        SessionPhase::Ended(end) => {
            let reason = end.plain();
            assert!(
                reason.contains("Kicked by an operator"),
                "translated English missing: {reason}"
            );
            assert!(
                !reason.contains("multiplayer.disconnect.kicked"),
                "raw key leaked through the translator: {reason}"
            );
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

/// A compatibility proxy can carry an older JSON component as the literal
/// string inside a newer disconnect component. The outer protocol decode is
/// valid in that case, but the shell must give the embedded component one
/// bounded parse before drawing it instead of exposing its JSON source.
#[test]
fn disconnect_reason_recovers_embedded_json_and_its_styles() {
    use crate::net::NetUpdate;
    use lodestone_model::{Text, TextColor};

    let raw = r#"{"text":"-----","strikethrough":true,"color":"gray","extra":[{"text":" kicked","strikethrough":false,"color":"red"}]}"#;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::Disconnected(Box::new(Text::literal(raw))))
        .unwrap();
    sim.poll_net();

    let SessionPhase::Ended(end) = sim.session_phase() else {
        panic!("expected Ended");
    };
    assert_eq!(end.plain(), "----- kicked");
    let spans = end.reason.to_spans();
    assert_eq!(spans.len(), 2, "root text and extra child stay separate");
    assert_eq!(spans[0].style.color, Some(TextColor::Gray));
    assert_eq!(spans[0].style.strikethrough, Some(true));
    assert_eq!(spans[1].style.color, Some(TextColor::Red));
    assert_eq!(spans[1].style.strikethrough, Some(false));

    let control = Text::literal(raw).resolve(&|_| None);
    assert!(
        control.to_plain_string().starts_with('{'),
        "the literal control must reproduce the reported raw JSON"
    );
}

/// Compatibility proxies do not all put the serialized component directly in
/// the root literal. This shape has an empty root, a child containing a JSON
/// string, and only then the component object. A root-only, one-pass recovery
/// leaves the braces on screen.
#[test]
fn disconnect_reason_recovers_child_and_repeated_json_wrappers() {
    use crate::net::NetUpdate;
    use lodestone_model::{Text, TextColor};

    let raw = r#"{"text":"-----","strikethrough":true,"color":"gray"}"#;
    let quoted = serde_json::to_string(raw).expect("a string always serializes");
    let wrapped = Text {
        extra: vec![Text::literal(quoted)],
        ..Text::default()
    };
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::Disconnected(Box::new(wrapped)))
        .unwrap();
    sim.poll_net();

    let SessionPhase::Ended(end) = sim.session_phase() else {
        panic!("expected Ended");
    };
    assert_eq!(end.plain(), "-----");
    let spans = end.reason.to_spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].style.color, Some(TextColor::Gray));
    assert_eq!(spans[0].style.strikethrough, Some(true));
}

/// Mineplex's proxy path has been observed stripping a serialized JSON
/// string's outer quotes while retaining the backslash escapes inside it.
/// That is neither a component object nor a valid JSON string until the
/// missing boundary is restored at the disconnect-only compatibility seam.
#[test]
fn disconnect_reason_recovers_unquoted_escaped_component() {
    use crate::net::NetUpdate;
    use lodestone_model::{Text, TextColor};

    let raw = r#"{\"text\":\"-----\",\"strikethrough\":true,\"color\":\"gray\",\"extra\":[{\"text\":\"Mineplex!\",\"strikethrough\":false,\"color\":\"red\"}]}"#;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::Disconnected(Box::new(Text::literal(raw))))
        .unwrap();
    sim.poll_net();

    let SessionPhase::Ended(end) = sim.session_phase() else {
        panic!("expected Ended");
    };
    assert_eq!(end.plain(), "-----Mineplex!");
    let spans = end.reason.to_spans();
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].style.color, Some(TextColor::Gray));
    assert_eq!(spans[0].style.strikethrough, Some(true));
    assert_eq!(spans[1].style.color, Some(TextColor::Red));
    assert_eq!(spans[1].style.strikethrough, Some(false));
}

#[test]
fn session_phase_reports_net_error_as_ended() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::Error("connection refused".into()))
        .unwrap();
    sim.poll_net();
    match sim.session_phase() {
        SessionPhase::Ended(end) => {
            let reason = end.plain();
            assert!(reason.contains("connection refused"), "got {reason}");
            assert_eq!(
                end.kind,
                crate::sim::SessionEndKind::Failed,
                "a net error is a client-side failure, not a server disconnect — \
                 that distinction is what gives the screen the right title"
            );
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

/// The absence control for the two tests below: this is the exact defect
/// report ("if i open to lan, then open it a second time it kicks me and
/// says lan is already up") reproduced directly at the `Sim` boundary, using
/// the *old* wrong variant a second publish attempt used to be reported
/// through. It must still end the session — proving the detector the next
/// test relies on (`session_phase != Ended`) would actually have caught the
/// bug, rather than merely asserting the negative and hoping the mechanism
/// works.
#[test]
fn net_update_error_would_have_caught_the_old_already_published_kick() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected, "premise");

    // The literal message `net.rs` sent for this case before
    // `NetUpdate::LanPublishError` existed.
    feed.send(NetUpdate::Error(
        "open to LAN: this world is already published".into(),
    ))
    .unwrap();
    sim.poll_net();
    assert!(
        matches!(sim.session_phase(), SessionPhase::Ended(_)),
        "control failed: NetUpdate::Error must still end a session, or the \
         positive test below is not measuring anything"
    );
}

/// The fix ("kicks me" report): a second Open to LAN press —
/// `IntegratedServer::publish` returning `AlreadyExists` — must reach the
/// player as one more chat line on a session that is still alive, never as a
/// disconnect. See `NetUpdate::LanPublishError`'s own doc for the full
/// button → net thread → publish handler → error path trace, and the control
/// immediately above for evidence the assertion below would actually fail if
/// the old code path (`NetUpdate::Error`) were used instead.
#[test]
fn a_second_lan_publish_reports_a_chat_error_without_ending_the_session() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();
    assert_eq!(
        sim.session_phase(),
        SessionPhase::Connected,
        "premise: a live session to publish from"
    );

    // The first publish succeeds.
    feed.send(NetUpdate::LanOpened { port: 25565 }).unwrap();
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected);
    assert!(
        sim.is_lan_published(),
        "premise: the world really is published now, or the second call \
         below is not the scenario this test claims to cover"
    );

    // The second publish — a second press of the same button — fails
    // server-side, and must not disturb the session at all.
    feed.send(NetUpdate::LanPublishError(
        "open to LAN: this world is already published".into(),
    ))
    .unwrap();
    sim.poll_net();
    assert_eq!(
        sim.session_phase(),
        SessionPhase::Connected,
        "a second publish attempt must never disconnect an otherwise \
         healthy session — this is the discriminating assertion, not the \
         chat line below"
    );
    let chat = sim.recent_chat_spans(10);
    assert!(
        chat.iter()
            .any(|(spans, _)| crate::overlay::spans_text(spans).contains("already published")),
        "the failure must still reach the player, through the ordinary chat \
         path: {chat:?}"
    );
}

#[test]
fn end_session_tears_down_and_a_fresh_connect_afterward_starts_clean() {
    // The real acceptance test for `Sim::end_session`: not just that it
    // clears fields, but that a *second* connect afterward behaves
    // exactly like the first, with nothing from the old session leaking
    // through.
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected);

    // Populate every read-model `end_session` is responsible for
    // clearing, so this test can actually observe the reset rather than
    // asserting on fields that were already empty. The vitals go in through
    // the *net thread's* fold (`ingest`) because that is now the only writer;
    // the chat log still arrives on the `NetUpdate` channel.
    feed.send(NetUpdate::Chat {
        text: lodestone_model::Text::literal("hello"),
        player: false,
        sender: None,
        // A system message carries no signature to check, so the driver's
        // verdict is `false` — see `NetUpdate::Chat::verified`, which is
        // "unproven", not "forged".
        verified: false,
    })
    .unwrap();
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::HealthChanged {
            health: 12.0,
            food: 8,
            saturation: 3.0,
        },
    );
    // A shared-fold component that is *not* a vital, to pin the other half of
    // the teardown invariant: `end_session` must clear the previous server's
    // sidebar as well as the vitals.
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::DisplayObjective {
            slot: lodestone_model::event::DisplaySlot::Sidebar,
            objective: Some("kills".into()),
        },
    );
    sim.poll_net();
    assert!(
        !sim.recent_chat_spans(10).is_empty(),
        "setup: chat must be populated before the teardown can be observed clearing it"
    );
    assert_eq!(sim.health(), Some(12.0), "setup: health must be populated");
    assert_eq!(
        sim.server_entity_id(),
        Some(7),
        "setup: entity id must be populated"
    );
    assert_eq!(
        displayed_sidebar(&sim).as_deref(),
        Some("kills"),
        "setup: the sidebar must be populated"
    );

    sim.end_session();

    assert!(sim.net().is_none(), "the connection must be dropped");
    assert_eq!(sim.session_phase(), SessionPhase::LocalOnly);
    assert!(sim.recent_chat_spans(10).is_empty(), "chat log must clear");
    assert_eq!(sim.health(), None, "health must clear");
    assert_eq!(sim.food(), None, "food must clear");
    assert_eq!(
        sim.server_entity_id(),
        None,
        "the local entity id must clear"
    );
    assert_eq!(
        displayed_sidebar(&sim),
        None,
        "the previous server's sidebar must clear too — §4.1(c) made this \
         reachable from `Sim.local`, so the old 'it goes away with `net`' \
         reasoning no longer holds"
    );

    // The negative control this test exists for: a fresh connect
    // afterward must reach `Connected` and must not carry the old
    // session's chat forward, proving the reset actually took rather
    // than merely reporting empty because nothing polled yet.
    let (net2, _actions2, feed2) = NetClient::loopback_with_feed();
    sim.attach_net(net2);
    assert_eq!(sim.session_phase(), SessionPhase::Connecting);
    feed2.send(NetUpdate::LoggedIn { entity_id: 9 }).unwrap();
    ingest(&mut sim, login_event(9));
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected);
    assert_eq!(sim.server_entity_id(), Some(9));
    assert!(
        sim.recent_chat_spans(10).is_empty(),
        "the new session must not inherit the old one's chat"
    );
}

/// The driver's signature verdict decides a player message's `MessageTrust`,
/// and both answers are asserted — the discriminating pair, because a stamp
/// that ignores the flag agrees with one arm by construction.
///
/// This is the gate for a defect of the "a correct consumer fed a constant by
/// its producer" shape: `MessageTrust` had three variants and a real signature
/// check ran in the client driver, while `net.rs`'s router matched
/// `ClientEvent::Chat` with `..` and dropped `ack`, so `net_apply` stamped
/// **every** player message `NotSecure`. Under that code the `verified: true`
/// arm below reads `NotSecure` and fails; nothing about the `false` arm would
/// have noticed.
///
/// It drives the real `NetUpdate::Chat` through the real `Sim::poll_net` and
/// reads the stored `ChatEntry` back off the real `SessionChat` component, so
/// it covers the fold rather than a restatement of it. It does **not** cover
/// `net.rs`'s `forward` — that maps `ClientEvent` to `NetUpdate` one layer up.
#[test]
fn a_verified_player_message_is_stored_secure_and_an_unverified_one_is_not() {
    use crate::net::NetUpdate;
    use lodestone_game::chat::{ChatEntry, MessageTrust};

    let mut seen = Vec::new();
    for (verified, expected) in [(true, MessageTrust::Secure), (false, MessageTrust::NotSecure)] {
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::Chat {
            text: lodestone_model::Text::literal("hi"),
            player: true,
            sender: None,
            verified,
        })
        .unwrap();
        sim.poll_net();

        let local = sim.local_entity();
        let trust = sim.read(|w| {
            w.get::<lodestone_ecs::session::SessionChat>(local)
                .and_then(|chat| match chat.0.feed().iter().next_back() {
                    Some(ChatEntry::Player { trust, .. }) => Some(*trust),
                    _ => None,
                })
        });
        seen.push((verified, trust));
        assert_eq!(
            trust,
            Some(expected),
            "verified = {verified} must store {expected:?}"
        );
    }
    // Collected and asserted on the collection so a regression reports both
    // arms rather than aborting on the first: the claim is that the two
    // *differ*, which one arm alone cannot express.
    assert_ne!(
        seen[0].1, seen[1].1,
        "the two arms must not coincide, or the flag is being ignored: {seen:?}"
    );
}

#[test]
fn inbound_chat_is_logged_and_typed_lines_route_to_the_action_seam() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientAction;
    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    // Inbound server chat must surface in the HUD log (not merely logged).
    feed.send(NetUpdate::Chat {
        text: lodestone_model::Text::literal("hello world"),
        player: false,
        sender: None,
        // A system message carries no signature to check, so the driver's
        // verdict is `false` — see `NetUpdate::Chat::verified`, which is
        // "unproven", not "forged".
        verified: false,
    })
    .unwrap();
    sim.poll_net();
    let lines: Vec<String> = sim
        .recent_chat_spans(10)
        .into_iter()
        .map(|(spans, _)| crate::overlay::spans_text(&spans))
        .collect();
    assert_eq!(
        lines,
        vec!["hello world".to_string()],
        "inbound chat must reach the display log"
    );

    // Typed lines route through the one outbound action seam: a leading '/'
    // is a command (slash stripped), otherwise a chat message.
    assert!(sim.send_chat("/say hi"), "a command line must send");
    assert!(sim.send_chat("plain message"), "a chat line must send");
    // Anti-vacuity: a blank line must send *nothing*, so "everything sends"
    // can't pass — and neither can "nothing sends", guarded by the two above.
    assert!(!sim.send_chat("   "), "blank input must not send");

    // Outbound routing does not rewrite `/givedebug`; the command and its
    // validation belong to the server. The client forwards the typed line
    // through the ordinary command path.
    let before = sim.recent_chat_spans(10).len();
    assert!(
        sim.send_chat("/givedebug minecraft:diamond_pickaxe 1"),
        "a /givedebug line is now an ordinary command and must reach the wire"
    );
    assert!(
        sim.send_chat("/givedebug"),
        "even the malformed form goes to the server; nothing absorbs it locally"
    );
    assert_eq!(
        sim.recent_chat_spans(10).len(),
        before,
        "no local echo and no local error line — that was the wrapper's job"
    );

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert_eq!(
        sent,
        vec![
            ClientAction::SendCommand {
                command: "say hi".into()
            },
            ClientAction::SendChat {
                text: "plain message".into()
            },
            // Verbatim, *not* rewritten to `give @s minecraft:diamond_pickaxe 1`
            // — which is the whole assertion.
            ClientAction::SendCommand {
                command: "givedebug minecraft:diamond_pickaxe 1".into()
            },
            ClientAction::SendCommand {
                command: "givedebug".into()
            },
        ],
        "exactly the four non-blank lines route, with the command slash stripped"
    );
}

#[test]
fn chat_lines_age_as_the_clock_advances() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    feed.send(NetUpdate::Chat {
        text: lodestone_model::Text::literal("aged line"),
        player: false,
        sender: None,
        // A system message carries no signature to check, so the driver's
        // verdict is `false` — see `NetUpdate::Chat::verified`, which is
        // "unproven", not "forged".
        verified: false,
    })
    .unwrap();
    sim.poll_net();
    // Freshly received: age is ~0.
    assert!(
        sim.recent_chat_spans(1)[0].1 < 0.001,
        "a just-received line is young"
    );

    // Advancing the sim clock ages the line by real elapsed time.
    sim.step(2.5);
    let age = sim.recent_chat_spans(1)[0].1;
    assert!(
        (2.4..=2.6).contains(&age),
        "line age must track the sim clock, got {age}"
    );
}

/// The HUD's health/food accessors must reflect the **net thread's** fold.
///
/// This used to feed `NetUpdate::Health` and assert the shell's own arm folded
/// it. That arm was the duplicate the vitals collapse deleted, so the test now
/// drives `ClientEvent::HealthChanged` through the one remaining fold — the
/// `NetIngest` schedule inside this `Sim`'s own `World`, which is exactly what
/// production does — and asserts the same accessors. Sharper, not weaker: the
/// old version could have passed with the production fold missing entirely.
#[test]
fn server_health_and_food_reach_the_hud_accessors() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    // Off a live server there is no survival state, so the HUD draws no bars.
    assert_eq!(sim.health(), None);
    assert_eq!(sim.food(), None);

    ingest(
        &mut sim,
        lodestone_client::ClientEvent::HealthChanged {
            health: 14.0,
            food: 17,
            saturation: 2.5,
        },
    );
    // Both fields must land — a one-sided store would leave the other None.
    assert_eq!(sim.health(), Some(14.0));
    assert_eq!(sim.food(), Some(17));
}

/// The negative control for the two tests above: enqueueing without running
/// the schedule must change nothing, so "the accessor reports 14" is evidence
/// the *fold* ran and not merely that the event was constructed.
#[test]
fn queueing_health_without_running_net_ingest_folds_nothing() {
    let mut sim = Sim::new(test_config());
    let local = sim.local;
    sim.write(|w| {
        w.resource_mut::<lodestone_ecs::ingest::IngestQueue>().push(
            lodestone_client::ClientEvent::HealthChanged {
                health: 14.0,
                food: 17,
                saturation: 2.5,
            },
        );
    });
    assert_eq!(
        sim.health(),
        None,
        "pushing must not fold; only NetIngest folds"
    );
    // …and the local player really is the entity the fold would write, so the
    // assertion above is not passing because it is looking at the wrong one.
    assert!(
        sim.read(|w| w.get::<Vitals>(local).is_some()),
        "the local player must carry Vitals for this control to mean anything"
    );
}

#[test]
fn server_experience_reaches_the_hud_accessor() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    // Off a live server (or before the first packet) there is no real XP
    // value, so the HUD must not draw a faked bar.
    assert_eq!(sim.experience(), None);

    ingest(
        &mut sim,
        lodestone_client::ClientEvent::ExperienceChanged {
            progress: 0.6,
            level: 30,
            total: 1395,
        },
    );
    assert_eq!(sim.experience(), Some((0.6, 30, 1395)));
}

#[test]
fn title_events_fold_into_the_title_overlay() {
    use crate::net::NetUpdate;
    use lodestone_model::{ClientEvent, Text};

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    // No title yet → nothing to draw.
    assert!(sim.title_overlay().is_none());

    feed.send(NetUpdate::TitleEvent(ClientEvent::TitleText {
        text: Text::literal("Welcome"),
    }))
    .unwrap();
    feed.send(NetUpdate::TitleEvent(ClientEvent::SubtitleText {
        text: Text::literal("to the server"),
    }))
    .unwrap();
    sim.poll_net();

    let (title, subtitle, _alpha) = sim
        .title_overlay()
        .expect("a server-sent title must reach the HUD accessor");
    assert_eq!(crate::overlay::spans_text(&title), "Welcome");
    assert_eq!(
        subtitle.as_deref().map(crate::overlay::spans_text),
        Some("to the server".to_owned())
    );

    // A clear packet must empty the overlay again.
    feed.send(NetUpdate::TitleEvent(ClientEvent::TitlesCleared {
        reset_times: false,
    }))
    .unwrap();
    sim.poll_net();
    assert!(sim.title_overlay().is_none());
}

/// A **hex** colour survives `title_overlay` and `action_bar_overlay`.
///
/// These two accessors flattened with `Text::to_legacy_string()`, and that call was
/// where a modern server's title colour died: the sixteen named colours have `§`
/// codes and the font layer applies codes at draw time, so they survived a
/// `String` — `TextColor::Rgb` has none. Hex is therefore the *only* input on which
/// "flattens to a legacy string" and "hands over spans" differ, which is why a
/// named colour here would be the coincident-input species of vacuous test.
///
/// The three values are pairwise distinct so a title/subtitle/action-bar mix-up
/// cannot pass, and the mismatches are collected so one bad arm does not hide the
/// other two.
#[test]
fn a_hex_colour_survives_the_title_and_action_bar_accessors() {
    use crate::net::NetUpdate;
    use lodestone_model::{ClientEvent, Text, TextColor, TextStyle};

    // Not multiples of 0x11, not near a named colour, all different.
    const TITLE: u32 = 0x001f_2e3d;
    const SUBTITLE: u32 = 0x004a_6b8c;
    const ACTION: u32 = 0x00c4_7b19;

    let hex = |text: &str, rgb: u32| Text {
        style: TextStyle {
            font: None,
            color: Some(TextColor::Rgb(rgb)),
            ..TextStyle::default()
        },
        ..Text::literal(text)
    };

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::TitleEvent(ClientEvent::TitleText {
        text: hex("T", TITLE),
    }))
    .unwrap();
    feed.send(NetUpdate::TitleEvent(ClientEvent::SubtitleText {
        text: hex("S", SUBTITLE),
    }))
    .unwrap();
    feed.send(NetUpdate::ActionBar(hex("A", ACTION))).unwrap();
    sim.poll_net();

    let (title, subtitle, _) = sim
        .title_overlay()
        .expect("a server-sent title must reach the HUD accessor");
    let (action, _) = sim
        .action_bar_overlay()
        .expect("a GameInfo message must reach the action-bar accessor");
    let subtitle = subtitle.expect("the subtitle must reach the accessor too");

    let mut wrong = Vec::new();
    for (name, spans, want) in [
        ("title", &title, TITLE),
        ("subtitle", &subtitle, SUBTITLE),
        ("action_bar", &action, ACTION),
    ] {
        let got: Vec<Option<TextColor>> = spans.iter().map(|s| s.style.color).collect();
        if got != vec![Some(TextColor::Rgb(want))] {
            wrong.push(format!("{name}: want Rgb(#{want:06x}) throughout, got {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:?}");
}

#[test]
fn game_info_chat_folds_into_the_action_bar_not_the_feed() {
    use crate::net::NetUpdate;
    use lodestone_model::Text;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    assert!(sim.action_bar_overlay().is_none());

    feed.send(NetUpdate::ActionBar(Text::literal("Boss incoming")))
        .unwrap();
    sim.poll_net();

    let (text, alpha) = sim
        .action_bar_overlay()
        .expect("a GameInfo message must reach the action-bar accessor");
    assert_eq!(crate::overlay::spans_text(&text), "Boss incoming");
    assert!(alpha > 0.0, "a fresh action-bar message is fully opaque");
    // It must not have leaked into the chat scrollback.
    assert!(
        sim.recent_chat_spans(10).is_empty(),
        "GameInfo is the action bar, not chat — it must not enter the feed"
    );
}

/// The held-item name highlight end to end: selecting an
/// item's name reaches [`Sim::held_item_overlay`] — the accessor
/// `app.rs`'s `hud_frame.held_item = self.sim.held_item_overlay()` reads
/// every frame — and, the property `docs/held-item-name-tooltip.md`
/// calls out as the one non-obvious constraint, switching between two
/// hotbar slots that hold the **same** item does not retrigger it.
#[test]
fn held_item_overlay_reaches_pixels_and_keys_on_identity_not_slot() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert_eq!(
        sim.held_item_overlay(),
        None,
        "control: nothing selected at spawn must show no overlay"
    );

    // Identical dirt in both hotbar slot 0 (selected by default) and
    // slot 1.
    give_main_hand_item(&mut sim, "minecraft:dirt");
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 1,
                item: Some(lodestone_model::ItemStack::new(
                    "minecraft:dirt".parse().expect("valid item id"),
                    1,
                )),
            });
        }
    });

    sim.step(1.0 / 20.0);
    let (name, alpha) = sim
        .held_item_overlay()
        .expect("selecting an item must show its name — the pixel this feature draws");
    assert_eq!(name, "Dirt");
    assert_eq!(
        alpha, 1.0,
        "vanilla's own hud rendering: a freshly triggered highlight is at full opacity, no fade-in"
    );

    // Run past the hold phase into the fade so alpha is measurably below
    // 1.0 before the slot switch below — otherwise a retrigger bug could
    // hide behind "alpha was already 1.0 anyway".
    for _ in 0..35 {
        sim.step(1.0 / 20.0);
    }
    let faded_alpha = sim
        .held_item_overlay()
        .map(|(_, a)| a)
        .expect("control: must still be showing (fading, not yet expired)");
    assert!(
        (0.0..1.0).contains(&faded_alpha),
        "control: must be mid-fade before the slot switch, got {faded_alpha}"
    );

    // The subject: selecting slot 1, which holds the identical item,
    // must not restart the timer.
    sim.select_slot(1);
    sim.step(1.0 / 20.0);
    let after_switch = sim
        .held_item_overlay()
        .map(|(_, a)| a)
        .expect("still showing: the countdown continues, it does not vanish");
    assert!(
        after_switch <= faded_alpha,
        "switching between two slots holding the same item must not restart the \
         timer (vanilla's own hud rendering's item-and-hover-name identity check, not slot \
         equality) — alpha went from {faded_alpha} to {after_switch}, which only \
         happens if it retriggered"
    );
}

/// The held-item span seam: [`Sim::held_item_overlay_spans`] —
/// which `app/redraw.rs`'s `hud_frame.held_item_spans =
/// self.sim.held_item_overlay_spans()` reads every frame, mirroring
/// `hud_frame.held_item = self.sim.held_item_overlay()` right above it —
/// must carry a held item's hex-coloured custom name all the way to a HUD
/// **vertex colour**, not merely return spans from the accessor.
///
/// Builds the item through the real production path a live server's
/// `set_container_slot`/`set_container_content` would drive
/// (`ClientEvent::InventorySlotChanged` → `Menus::apply` →
/// `lodestone_ecs::session::tick_hud_overlays` →
/// `HeldItemHighlight::set_spans`, from `styled_hover_name_spans`), then
/// feeds the accessor's own output into `HudFrame`/`HudGeometry::build`
/// exactly the shape `app/redraw.rs` assembles, and checks three
/// pairwise-distinct vertex RGBs (hex, an inline `§` code and a named
/// colour — the same three-clause shape `hud::tests::
/// held_item_spans_carry_hex_named_and_inline_legacy_colour_to_distinct_vertices`
/// uses, so a fixture with only named colours cannot hide the hex-drop bug).
///
/// The control repeats the *same* real item through the legacy
/// `Sim::held_item_overlay`/`HudFrame::held_item` path
/// (`styled_hover_name`'s `Text::to_legacy_string`) and requires the hex
/// colour to be lost there — proving the positive assertion above is
/// measuring the seam this test exists for, not a coincidence.
#[test]
fn held_item_overlay_spans_carry_hex_colour_from_a_real_item_to_a_vertex() {
    use lodestone_model::text::{Text, TextColor, TextContent, TextStyle};
    use lodestone_model::{Identifier, ItemComponents, ItemStack as ModelItemStack};

    let hex = Text {
        content: TextContent::Literal("Hex".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Rgb(0x1a_2b3c)),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    // The inline convention: a server-authored custom name whose colour
    // lives inside the literal text as a `§c` code rather than as a
    // component-level style.
    let inline_legacy = Text::literal("\u{00a7}cRed");
    let named = Text {
        content: TextContent::Literal("Gray".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Gray),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    let custom_name = Text {
        extra: vec![hex, inline_legacy, named],
        ..Text::default()
    };

    let item_id: Identifier = "minecraft:diamond_sword".parse().expect("valid item id");
    let mut components = ItemComponents::default();
    components.custom_name = Some(custom_name);
    let wire_stack = ModelItemStack {
        item: item_id,
        count: 1,
        components,
    };

    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(wire_stack),
            });
        }
    });
    sim.step(1.0 / 20.0);

    let (spans, alpha) = sim
        .held_item_overlay_spans()
        .expect("a hex-coloured custom name must still show a held-item overlay");
    assert!(alpha > 0.0, "a freshly triggered highlight is at full opacity");

    let stats = DebugStats::default();
    let geo = crate::hud::HudGeometry::build(
        &crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            held_item_spans: Some((spans, alpha)),
            ..crate::hud::HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        geo.vertex_count() > 0,
        "sanity: the label must draw something at all"
    );

    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let has_colour = |verts: &[f32], rgb: (u8, u8, u8)| {
        verts
            .chunks_exact(6)
            .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
    };
    let expected = [
        ("hex", (0x1a_u8, 0x2b_u8, 0x3c_u8)),
        ("inline §c", (0xff_u8, 0x55_u8, 0x55_u8)),
        ("named gray", (0xaa_u8, 0xaa_u8, 0xaa_u8)),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|(_, rgb)| !has_colour(&geo.verts, *rgb))
        .map(|(name, _)| *name)
        .collect();
    assert!(
        missing.is_empty(),
        "these colours never reached a vertex: {missing:?} (full expected set: {expected:?})"
    );

    // Control: the same real held item, but through the lossy
    // `Sim::held_item_overlay`/`HudFrame::held_item` path — must lose the
    // hex colour, or the assertion above proves nothing about which field
    // actually carries it.
    let (legacy_name, legacy_alpha) = sim
        .held_item_overlay()
        .expect("control: the legacy accessor must also see the same held item");
    let legacy_geo = crate::hud::HudGeometry::build(
        &crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            held_item: Some((legacy_name, legacy_alpha)),
            ..crate::hud::HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        !has_colour(&legacy_geo.verts, (0x1a, 0x2b, 0x3c)),
        "control failed: the legacy `held_item` path was expected to lose the hex \
         colour (that is the bug #656 tracks), but it drew it anyway — this test's \
         premise is wrong"
    );
}

/// `app/redraw.rs`'s per-frame render loop is built on live GPU/window
/// state, so no unit test in this crate can call it directly — the same
/// constraint `menu::nav::tests::
/// app_rs_still_threads_every_chat_option_into_the_hud_frame` and
/// `redraw_rs_still_pushes_the_glint_options_to_all_three_sites` work around
/// by grepping that file's own source text instead. This is the same
/// technique for held-item seam: `Sim::held_item_overlay_spans`
/// (`sim/session.rs`) reaching `HudFrame::held_item_spans` at the app-wiring
/// layer, mirroring the pre-existing legacy `held_item_overlay` →
/// `HudFrame::held_item` line right above it.
///
/// This lives in `sim/tests.rs` rather than beside the line it checks —
/// `include_str!`-ing a file from *within itself* would make the assertion
/// tautological, since the search string would always be present as the
/// literal argument to this very `.contains()` call. Living in a different
/// file (as the `chat_opts`/glint-options precedents already do) is what
/// keeps the check meaningful.
///
/// `held_item_overlay_spans_carry_hex_colour_from_a_real_item_to_a_vertex`
/// (just above) proves the accessor's own output reaches a vertex colour;
/// this proves the line that hands that output to `HudFrame` in the first
/// place is still present, so the two together cover the whole seam.
#[test]
fn redraw_rs_still_forwards_held_item_overlay_spans_to_the_hud_frame() {
    let src = include_str!("../app/redraw.rs");
    assert!(
        src.contains("hud_frame.held_item_spans = self.sim.held_item_overlay_spans();"),
        "app/redraw.rs no longer forwards `Sim::held_item_overlay_spans` into \
         `HudFrame::held_item_spans` — the held-item label is back to losing hex \
         colours, with nothing else in this crate able to see it because the real \
         draw loop cannot be unit tested"
    );
}

/// The read-through the shell now depends on: it folds nothing itself, so
/// the rows must come out of the **client's** one `SessionTabList`.
///
/// `ingest_session_event` runs the same `lodestone_ecs::session` systems the
/// real net thread runs (see `NetClient::session`); what this pins is the
/// chain `component → NetClient::tab_list → Sim::tab_list_view`, which is
/// exactly what the deleted `NetUpdate::TabListEvent` fold used to short.
#[test]
fn tab_overlay_rows_read_the_clients_one_folded_tab_list() {
    use lodestone_model::{ClientEvent, GameMode, PlayerListEntry, Text};
    use uuid::Uuid;

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    let alice = Uuid::from_u128(1);
    let bob = Uuid::from_u128(2);
    let ingest = |sim: &Sim, event: ClientEvent| {
        sim.net().expect("net attached").ingest_session_event(event);
    };
    ingest(
        &sim,
        ClientEvent::PlayerListUpdate {
            entries: vec![
                PlayerListEntry {
                    uuid: Some(bob),
                    name: Some("Bob".into()),
                    game_mode: Some(GameMode::Spectator),
                    latency: Some(30),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
                PlayerListEntry {
                    uuid: Some(alice),
                    name: Some("Alice".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(12),
                    display_name: Some(Text::literal("Alice the Brave")),
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
            ],
        },
    );

    // The whole row, not just the name: the projection now carries the game mode
    // and the latency *band*, and asserting only the names would not notice
    // either being dropped on the way through — which is exactly what the
    // pre-`TabListView` flattening did.
    let rows = |sim: &Sim| -> Vec<(String, &'static str, bool)> {
        sim.tab_list_view()
            .rows
            .iter()
            .map(|row| {
                (
                    crate::overlay::spans_text(&row.name),
                    row.ping_sprite,
                    row.spectator,
                )
            })
            .collect()
    };
    assert_eq!(
        rows(&sim),
        vec![
            ("Alice the Brave".to_string(), "icon/ping_5", false),
            // Spectators sort last and draw dimmed; both facts are in the row.
            ("Bob".to_string(), "icon/ping_5", true),
        ],
        "tab overlay rows must come from the client's folded TabList state"
    );

    ingest(
        &sim,
        ClientEvent::PlayerListRemove {
            profile_ids: vec![alice],
        },
    );
    assert_eq!(rows(&sim), vec![("Bob".to_string(), "icon/ping_5", true)]);
}

/// The missing production hop: `crate::gpu::gather_crack_targets` and
/// `BlockDestructionOverlays::iter` were both proven in `gpu/outline.rs`'s
/// own gate, but production must call the gather — `app.rs` only ever passed
/// `Sim::crack_target()`'s single local
/// dig through. This proves `Sim::crack_targets()` actually walks
/// `SessionBlockDestruction` for two *different* breaking entities, not just
/// the local target the pipeline gate already covers in isolation.
#[test]
fn crack_targets_reaches_every_other_players_overlay_not_just_the_local_dig() {
    use lodestone_model::ClientEvent;

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    let ingest = |sim: &Sim, event: ClientEvent| {
        sim.net().expect("net attached").ingest_session_event(event);
    };
    ingest(
        &sim,
        ClientEvent::BlockDestruction {
            entity_id: 301,
            pos: BlockPos::new(10, 64, 20),
            progress: 3,
        },
    );
    ingest(
        &sim,
        ClientEvent::BlockDestruction {
            entity_id: 402,
            pos: BlockPos::new(-5, 70, 8),
            progress: 7,
        },
    );

    let targets = sim.crack_targets();
    assert_eq!(
        targets.len(),
        2,
        "no local dig is in progress, so this must be exactly the two \
         other-player overlays reaching pixels — not one, not zero"
    );
    assert!(
        targets
            .iter()
            .any(|t| t.block == [10, 64, 20] && t.stage == 3),
        "entity 301's overlay must reach Sim::crack_targets: {targets:?}"
    );
    assert!(
        targets
            .iter()
            .any(|t| t.block == [-5, 70, 8] && t.stage == 7),
        "entity 402's overlay must reach Sim::crack_targets: {targets:?}"
    );
}

/// The negative control for the pair above: with no connection there is no
/// session `World` to read, so both projections must be empty rather than
/// falling back to some shell-local copy — which is the assertion that
/// `Sim` really holds neither aggregate any more.
#[test]
fn without_a_connection_the_shell_has_no_session_state_of_its_own() {
    let sim = Sim::new(test_config());
    assert!(sim.tab_list_view().is_empty());
    assert!(sim.sidebar().is_none());
    assert!(sim.boss_bars().is_empty());
}

/// The scoreboard twin of the tab-list read-through above.
#[test]
fn sidebar_rows_read_the_clients_one_folded_scoreboard() {
    use lodestone_model::event::{DisplaySlot, ObjectiveMode, ObjectiveRenderType};
    use lodestone_model::{ClientEvent, Text};

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    for event in [
        ClientEvent::ObjectiveUpdate {
            name: "kills".into(),
            mode: ObjectiveMode::Add,
            display_name: Some(Text::literal("Kills")),
            render_type: Some(ObjectiveRenderType::Integer),
            number_format: None,
        },
        ClientEvent::DisplayObjective {
            slot: DisplaySlot::Sidebar,
            objective: Some("kills".into()),
        },
        ClientEvent::ScoreUpdate {
            holder: "Alice".into(),
            objective: "kills".into(),
            value: 7,
            display: Some(Text::literal("Alice the Brave")),
            number_format: None,
        },
        ClientEvent::ScoreUpdate {
            holder: "Bob".into(),
            objective: "kills".into(),
            value: 3,
            display: None,
            number_format: None,
        },
    ] {
        sim.net().expect("net attached").ingest_session_event(event);
    }

    let sidebar = sim.sidebar().expect("sidebar objective should be visible");
    assert_eq!(crate::overlay::spans_text(&sidebar.title), "Kills");
    let rows: Vec<(String, String)> = sidebar
        .lines
        .iter()
        .map(|line| {
            (
                crate::overlay::spans_text(&line.label),
                crate::overlay::spans_text(&line.score),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("Alice the Brave".to_string(), "7".to_string()),
            ("Bob".to_string(), "3".to_string())
        ],
        "sidebar rows must come from the client's folded Scoreboard state"
    );
}

/// `Sim::tick_nearby_entities` must resolve a neighbour's real scoreboard team
/// into its `NearbyEntity::collision_rule`/`allied`, not leave every neighbour
/// at [`lodestone_physics::push::NearbyEntity::living`]'s `Always`/`false`
/// default forever — the defect the owner reported (a vanilla client refusing
/// to be pushed while ours accepted it) traces to exactly this: production had
/// zero readers of a non-default `CollisionRule` before this test, because the
/// only constructor `tick_nearby_entities` ever called was `::living`.
///
/// Two neighbours, not one, so the gate can be told from "every neighbour
/// reads as `Never`" — a team-name holder that never resolves would default
/// every neighbour to `Always` (see `tick_nearby_entities`'s own comment on
/// why an unresolved holder keeps the safe default), and a broken resolver
/// that always returns `Never` would be caught by Carol having no team and
/// still reading `Always`.
///
/// This does not exercise [`NearbyEntities::self_collision_rule`] (the local
/// player's *own* team) — that resolution goes through `NetClient::local_uuid`,
/// which `NetClient::loopback_with_feed` never publishes (it is a private
/// field this test cannot reach without touching `net.rs`), so
/// `self_collision_rule` reads its safe `Always` default here. The neighbour
/// half is the half the owner's report is actually about: a real remote
/// player pushing *us*.
#[test]
fn tick_nearby_entities_resolves_a_neighbours_scoreboard_team() {
    use lodestone_model::event::{TeamAction, TeamParameters, Visibility};
    use lodestone_model::{ClientEvent, GameMode, PlayerListEntry, Text};
    use uuid::Uuid;

    let mut sim = Sim::new(test_config());
    let feet = sim.player().position;

    let bob = Uuid::from_u128(101);
    let carol = Uuid::from_u128(102);

    // The tab list carries the account name a scoreboard team's member list
    // actually keys on (vanilla's own player scoreboard-name override) — see
    // `crate::sim::collide::scoreboard_holder`.
    ingest(
        &mut sim,
        ClientEvent::PlayerListUpdate {
            entries: vec![
                PlayerListEntry {
                    uuid: Some(bob),
                    name: Some("Bob".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(20),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
                PlayerListEntry {
                    uuid: Some(carol),
                    name: Some("Carol".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(20),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
            ],
        },
    );

    // A real server's `/team add red` + `/scoreboard teams option red
    // collisionRule never` + `/team join red Bob` — Bob is on a team whose
    // rule forbids the push outright; Carol has no team at all, exactly the
    // discriminating pair the task needs: one forbidden, one allowed, so a
    // resolver that ignores the team entirely (leaving everyone `Always`)
    // fails on Bob, and a resolver that is simply broken in the other
    // direction (everyone reads `Never`) fails on Carol.
    ingest(
        &mut sim,
        ClientEvent::TeamUpdate {
            name: "red".into(),
            action: TeamAction::Create {
                params: Box::new(TeamParameters {
                    display_name: Text::literal("Red"),
                    prefix: Text::literal(""),
                    suffix: Text::literal(""),
                    name_tag_visibility: Visibility::Always,
                    collision_rule: lodestone_model::CollisionRule::Never,
                    color: None,
                    friendly_fire: true,
                    see_friendly_invisibles: true,
                }),
                members: vec!["Bob".into()],
            },
        },
    );

    for (entity_id, uuid) in [(9001, bob), (9002, carol)] {
        ingest(
            &mut sim,
            ClientEvent::EntitySpawned {
                entity_id,
                uuid: Some(uuid),
                entity_type: "minecraft:player".parse().expect("valid entity type key"),
                pos: lodestone_model::Vec3::new(feet.x + 1.0, feet.y, feet.z),
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            },
        );
    }

    let nearby = sim.tick_nearby_entities();
    assert_eq!(
        nearby.list.len(),
        2,
        "both Bob and Carol must be in range and pass the push census"
    );

    let never_count = nearby
        .list
        .iter()
        .filter(|n| n.collision_rule == lodestone_physics::push::CollisionRule::Never)
        .count();
    let always_count = nearby
        .list
        .iter()
        .filter(|n| n.collision_rule == lodestone_physics::push::CollisionRule::Always)
        .count();
    assert_eq!(
        never_count, 1,
        "exactly Bob's NearbyEntity must carry his team's Never rule"
    );
    assert_eq!(
        always_count, 1,
        "Carol has no team, so hers must keep the Always default — proving \
         the resolver is not just returning Never unconditionally"
    );
    assert!(
        nearby.list.iter().all(|n| !n.allied),
        "the local player has no team in this harness, so `ownTeam != null` \
         must veto `allied` for every neighbour regardless of their own team"
    );
}

#[test]
fn tick_nearby_entities_keeps_a_boat_as_a_hard_collider_without_making_it_a_crowd_pusher() {
    let mut sim = Sim::new(test_config());
    let feet = sim.player().position;
    ingest(
        &mut sim,
        lodestone_model::ClientEvent::EntitySpawned {
            entity_id: 9010,
            uuid: None,
            entity_type: "minecraft:oak_boat".parse().expect("valid boat key"),
            pos: lodestone_model::Vec3::new(feet.x + 1.0, feet.y, feet.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );

    let nearby = sim.tick_nearby_entities();
    assert_eq!(nearby.list.len(), 1, "the non-pushing boat must not be filtered out");
    assert!(nearby.list[0].collidable);
    assert!(!nearby.list[0].pushes_players);
}

// -----------------------------------------------------------------------
// Local placement prediction
// -----------------------------------------------------------------------

/// The state ids below are transcribed from
/// `.cache/mc/26.2/generated/reports/blocks.json` — the registry generator's
/// output — and **not** from this code's own resolution, so
/// they are an external oracle rather than a round trip through
/// `state_for_placement`. Each is the state whose properties the reference
/// placement rules produce for that block.
///
/// A 26.2 data bump shifts every id, and this failing is the point: it says
/// the census moved under the resolver, which is exactly when the property
/// rules deserve a re-read.
