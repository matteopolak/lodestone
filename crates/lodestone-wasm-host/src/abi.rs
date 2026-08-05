//! The ABI surface (issue #173): the lift from `lodestone_model::ClientEvent` into
//! a WIT `event`, the lower from a WIT `action` back into
//! `lodestone_model::ClientAction`, and the capability that gates each direction.
//!
//! # This is the intent doctrine, serialised — and where it is not
//!
//! `docs/plugin-api.md`'s doctrine was designed to keep plugins out of other
//! systems' machines, and a surface that never hands out a machine is exactly a
//! surface that serialises. So the WIT vocabulary is not a second dialect invented
//! for wasm; it is the same `GameEvent(ClientEvent)` a native plugin reads and the
//! same `ClientAction` a native plugin pushes onto `ActionQueue`, with a copy
//! boundary in the middle.
//!
//! **Where it genuinely does not fit, stated rather than papered over:**
//!
//! | native tier has | this tier has | why |
//! |---|---|---|
//! | `Text`, the styled component tree | a plain `String` | `Text` is recursive, with translation keys, hover/click events and per-node style. Lifting it faithfully means a recursive WIT variant plus a translation table the guest cannot resolve anyway. So [`lift_event`] flattens with `Text::to_plain_string`, and this is **lossy**: a guest cannot see colour, cannot see a translation key, and cannot distinguish a translated message from a literal one that happens to render the same. |
//! | `ChatAckInfo` on `ClientEvent::Chat` | dropped | signed-chat acknowledgement is the *driver's* bookkeeping — a guest that echoed an `offset` back would fork a sequence counter the driver owns, which is clause 2 of the doctrine. Deliberately unreachable. |
//! | intent components (`BreakIntent`, `PlaceIntent`, `MovementIntent`, `LookIntent`) and their outcome components | **nothing yet** | these are the *other half* of the doctrine and they are not in the v0.1 world. They are install/remove-shaped rather than value-shaped, so they need paired ABI calls the host mirrors into real component inserts and removes, plus an outcome poll — a bigger surface than the one-crossing tick this world defines. See `docs/wasm-plugin-host.md` §"What is not in the world yet". A guest can chat and swing; it cannot yet mine, place or steer. |
//! | ~110 `ClientEvent` variants | 3 | the curated subset. Not an oversight and not a TODO: a full mirror is the staleness factory `lodestone_ecs::events`'s own module doc refuses. |
//!
//! # The staleness question, and the honest answer to it
//!
//! A curated subset raises the obvious objection: what stops it silently falling
//! behind `ClientEvent`? Nothing *automatic*, and it is worth being precise about
//! why rather than claiming a guard that does not exist.
//!
//! `ClientEvent` is `#[non_exhaustive]`, so [`lift_event`]'s `match` needs a
//! wildcard arm and no compiler error can fire when a variant is added. What
//! replaces that guard is the **subscription model**: a guest names the event kinds
//! it observes in its `plugin.toml`, and a kind this world does not define is a
//! loud manifest rejection at load time rather than a silent absence. So a plugin
//! author who wants an unlifted event finds out immediately, by name, at the point
//! they ask for it — which is the failure mode that matters. The host operator
//! learns nothing, and that is the residual gap.

use lodestone_model::{ClientAction, ClientEvent};

use crate::capability::{Capability, CapabilitySet};
use crate::host::{Action, BlockOffset, ChatKind, ChatMessage, Event, Hand, Health, SectionBlocksChanged, SectionPos};

/// Lift one `ClientEvent` into the guest vocabulary, or `None`.
///
/// `None` has two distinct causes and the caller cannot tell them apart, on
/// purpose: the variant is not in the curated subset, or it is but `granted` does
/// not include its `observe:` capability. Both mean "this guest does not see this
/// event", and distinguishing them at the call site would invite a caller to route
/// around the capability check.
#[must_use]
pub fn lift_event(event: &ClientEvent, granted: &CapabilitySet) -> Option<Event> {
    match event {
        ClientEvent::Chat { text, kind, .. } if granted.contains(Capability::ObserveChat) => {
            Some(Event::Chat(ChatMessage {
                // Lossy, deliberately — see this module's table.
                text: text.to_plain_string(),
                kind: match kind {
                    lodestone_model::event::ChatKind::Chat => ChatKind::Chat,
                    lodestone_model::event::ChatKind::System => ChatKind::System,
                    lodestone_model::event::ChatKind::GameInfo => ChatKind::GameInfo,
                },
            }))
        }
        ClientEvent::HealthChanged {
            health,
            food,
            saturation,
        } if granted.contains(Capability::ObserveHealth) => Some(Event::HealthChanged(Health {
            health: *health,
            food: *food,
            saturation: *saturation,
        })),
        ClientEvent::SectionBlocksChanged { section, blocks }
            if granted.contains(Capability::ObserveBlocks) =>
        {
            Some(Event::BlocksChanged(SectionBlocksChanged {
                section: SectionPos {
                    x: section.x,
                    y: section.y,
                    z: section.z,
                },
                blocks: blocks
                    .iter()
                    .map(|[x, y, z]| BlockOffset {
                        x: *x,
                        y: *y,
                        z: *z,
                    })
                    .collect(),
            }))
        }
        // Not an island-factory wildcard in the sense `CLAUDE.md` warns about: the
        // routers it names drop events that *should* have reached somewhere, whereas
        // this one is the definition of the curated subset. A guest cannot ask for a
        // kind that lands here — the manifest rejects the name.
        _ => None,
    }
}

/// Which capability an action needs, as a total function over the WIT `action`
/// variants.
///
/// Split out of [`lower_action`] so that a new variant added to the world forces a
/// decision *here* — this `match` has no wildcard, and `Action` is a generated type
/// that is not `#[non_exhaustive]`, so adding an arm to the `.wit` and forgetting to
/// gate it is a **compile error**. That is the one place in this crate where the
/// compiler really does hold the line, and it is deliberate.
#[must_use]
pub fn capability_for(action: &Action) -> Capability {
    match action {
        Action::SendChat(_) | Action::SendCommand(_) => Capability::ActChat,
        Action::SwingArm(_) => Capability::ActInteract,
    }
}

/// Lower one guest action into a `ClientAction`, or refuse it.
///
/// `Err` carries the capability that was missing, so a host can log *which* grant a
/// plugin needed rather than "an action was dropped". A refusal is counted and
/// logged by the conductor, never silent: a plugin whose actions vanish with no
/// explanation is the single most confusing thing a plugin API can do.
pub fn lower_action(action: Action, granted: &CapabilitySet) -> Result<ClientAction, Capability> {
    let needed = capability_for(&action);
    if !granted.contains(needed) {
        return Err(needed);
    }
    Ok(match action {
        Action::SendChat(text) => ClientAction::SendChat { text },
        Action::SendCommand(command) => ClientAction::SendCommand { command },
        Action::SwingArm(hand) => ClientAction::SwingArm {
            hand: match hand {
                Hand::Main => lodestone_model::common::Hand::Main,
                Hand::Off => lodestone_model::common::Hand::Off,
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use lodestone_model::Text;

    use super::*;

    fn chat_event(text: &str) -> ClientEvent {
        ClientEvent::Chat {
            text: Text::literal(text),
            kind: lodestone_model::event::ChatKind::Chat,
            sender: None,
            ack: None,
        }
    }

    /// The lift carries the text and the kind through unchanged.
    #[test]
    fn a_chat_event_lifts_with_its_text_and_kind() {
        let lifted = lift_event(
            &chat_event("hello"),
            &CapabilitySet::from_iter([Capability::ObserveChat]),
        )
        .expect("must lift");
        assert_eq!(
            lifted,
            Event::Chat(ChatMessage {
                text: "hello".to_owned(),
                kind: ChatKind::Chat,
            })
        );
    }

    /// **The data-flow capability, enforced.** Without `observe:chat` the same
    /// event does not lift — and the control immediately above proves it otherwise
    /// would, so this is a decision rather than a lift that never works.
    #[test]
    fn a_chat_event_does_not_lift_without_the_observe_capability() {
        assert_eq!(lift_event(&chat_event("hello"), &CapabilitySet::empty()), None);
        assert_eq!(
            lift_event(
                &chat_event("hello"),
                &CapabilitySet::from_iter([Capability::ObserveHealth, Capability::ActChat])
            ),
            None,
            "a *different* capability must not open the chat door"
        );
    }

    /// Each `observe:` capability opens exactly its own event and no other. The
    /// cross product, because a `granted.contains(…)` typo that named the wrong
    /// variant would pass any single-capability test.
    #[test]
    fn each_observe_capability_opens_only_its_own_event() {
        let events = [
            (Capability::ObserveChat, chat_event("x")),
            (
                Capability::ObserveHealth,
                ClientEvent::HealthChanged {
                    health: 20.0,
                    food: 20,
                    saturation: 5.0,
                },
            ),
            (
                Capability::ObserveBlocks,
                ClientEvent::SectionBlocksChanged {
                    section: lodestone_model::SectionPos { x: 1, y: 2, z: 3 },
                    blocks: vec![[1, 2, 3]],
                },
            ),
        ];
        for (cap, event) in &events {
            let only = CapabilitySet::from_iter([*cap]);
            assert!(
                lift_event(event, &only).is_some(),
                "{cap} must lift its own event"
            );
            for (other, _) in &events {
                if other == cap {
                    continue;
                }
                assert!(
                    lift_event(event, &CapabilitySet::from_iter([*other])).is_none(),
                    "{other} must not lift {cap}'s event"
                );
            }
        }
    }

    /// An event outside the curated subset does not lift even under the permissive
    /// policy — so "not lifted" is about the subset, not about a capability.
    #[test]
    fn an_unlifted_event_kind_yields_none_even_when_everything_is_granted() {
        assert_eq!(
            lift_event(
                &ClientEvent::Ping { id: 7 },
                &CapabilitySet::permissive()
            ),
            None
        );
    }

    /// The lower produces the real `ClientAction` the driver sends.
    #[test]
    fn actions_lower_onto_the_real_client_action_vocabulary() {
        let granted = CapabilitySet::permissive();
        assert_eq!(
            lower_action(Action::SendChat("hi".to_owned()), &granted),
            Ok(ClientAction::SendChat {
                text: "hi".to_owned()
            })
        );
        assert_eq!(
            lower_action(Action::SendCommand("time set day".to_owned()), &granted),
            Ok(ClientAction::SendCommand {
                command: "time set day".to_owned()
            })
        );
        assert_eq!(
            lower_action(Action::SwingArm(Hand::Off), &granted),
            Ok(ClientAction::SwingArm {
                hand: lodestone_model::common::Hand::Off
            })
        );
    }

    /// **The act-side capability, enforced, with the missing grant named.** The
    /// `Err` payload is what lets the conductor log something actionable.
    #[test]
    fn an_action_without_its_capability_is_refused_and_names_what_was_missing() {
        assert_eq!(
            lower_action(Action::SendChat("hi".to_owned()), &CapabilitySet::empty()),
            Err(Capability::ActChat)
        );
        assert_eq!(
            lower_action(Action::SwingArm(Hand::Main), &CapabilitySet::empty()),
            Err(Capability::ActInteract)
        );
        // `act:chat` does not buy `act:interact` — the two are separate powers.
        assert_eq!(
            lower_action(
                Action::SwingArm(Hand::Main),
                &CapabilitySet::from_iter([Capability::ActChat])
            ),
            Err(Capability::ActInteract)
        );
    }

    /// Every action variant is gated by something. Trivial-looking, and it is the
    /// test that fails if a new arm is added to `capability_for` returning a
    /// capability that `default_policy` happens to grant unconditionally.
    #[test]
    fn every_action_variant_needs_a_capability_that_the_empty_set_lacks() {
        for action in [
            Action::SendChat(String::new()),
            Action::SendCommand(String::new()),
            Action::SwingArm(Hand::Main),
        ] {
            assert!(
                lower_action(action.clone(), &CapabilitySet::empty()).is_err(),
                "{action:?} must be refused by an empty capability set"
            );
        }
    }
}
