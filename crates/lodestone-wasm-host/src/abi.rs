//! The capability-gated ABI surface: the lift from `lodestone_model::ClientEvent` into
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
//! | intent components (`BreakIntent`, `PlaceIntent`, `SelectSlotIntent`, `MovementIntent`, `LookIntent`) and their outcome components | `set-look(option<look-intent>)`, `set-movement(option<movement-intent>)`, `set-break(option<break-intent>)`, `place-block(place-intent)`, `select-slot(hotbar-slot)`, and bounded outcome events | look owns an optional component; movement overrides the normal controller's copied input for one tick, then flows through the existing physics and egress consumers. Break owns a persistent target until explicit release, while the shell owns validation, prediction and sequence. Placement crosses as only a target and face, then the shell owns its one-shot lifecycle. Selection crosses as only the candidate slot; the shell owns range validation and its carried-item echo. See `docs/wasm-plugin-intents.md`. |
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
use crate::host::{
    Action, BlockBreakVerdict, BlockFace, BlockOffset, BlockPlaceVerdict, BlockPos, BreakOutcome,
    BreakRejection, BreakStatus, ChatKind, ChatMessage,
    CommandAnchor, CommandContext, CommandEntity, CommandExecution, CommandPosition,
    CommandRotation, EntityDamageVerdict, Event, Hand, Health, InventoryClickVerdict,
    PlaceOutcome, PlaceRejection, PlaceStatus, PlayerInteractVerdict, PlayerMoveVerdict,
    SectionBlocksChanged, SectionPos, VerdictContext,
};

/// An action that changes a local-player intent rather than queuing a protocol
/// action. The conductor applies it before the existing look consumer runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IntentAction {
    /// Install a copied look target, or remove it and return ownership to input.
    Look(Option<lodestone_ecs::player::LookIntent>),
    /// Override the normal controller's copied movement input for this tick.
    Movement(Option<MovementOverride>),
    /// Submit one placement to the existing local-player lifecycle.
    Place(lodestone_ecs::player::PlaceIntent),
    /// Install, retarget, or release the persistent mining intent.
    Break(Option<lodestone_ecs::player::BreakIntent>),
    /// Request one shell-owned selected-hotbar-slot change.
    SelectSlot(usize),
}

/// A guest movement request after the boundary has enforced finite, digital axes.
///
/// `using_item` deliberately is not representable: the native controller owns that
/// state and the conductor preserves it when it applies this copied input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovementOverride {
    pub forward: f32,
    pub strafe: f32,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
}

fn axis(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// One capability-authorized guest result.
#[derive(Debug, Clone, PartialEq)]
pub enum LoweredAction {
    /// A normal client action owned by `ActionQueue`.
    Client(ClientAction),
    /// A copied intent consumed by the local-player ECS path.
    Intent(IntentAction),
}

/// Copy a command dispatcher source into the guest's value-only command context.
///
/// Permission checking is intentionally not repeated here: the native registry
/// filters the root and its subtree before it calls the guest handler. The guest
/// gets the result of that decision, never an authority it could use to bypass it.
#[must_use]
pub fn lift_command_context(source: &lodestone_ecs::commands::CommandSource) -> CommandContext {
    let execution = source.execution.as_ref().map(|execution| CommandExecution {
        entity: execution.entity.as_ref().map(|entity| CommandEntity {
            entity_id: entity.entity_id,
            username: entity.username.clone(),
        }),
        position: CommandPosition {
            x: execution.position.x,
            y: execution.position.y,
            z: execution.position.z,
        },
        rotation: CommandRotation {
            yaw: execution.rotation.yaw,
            pitch: execution.rotation.pitch,
        },
        dimension: execution.dimension.to_string(),
        anchor: match execution.anchor {
            lodestone_ecs::commands::CommandAnchor::Feet => CommandAnchor::Feet,
            lodestone_ecs::commands::CommandAnchor::Eyes => CommandAnchor::Eyes,
        },
        permission_level: execution.permission_level,
    });
    CommandContext {
        sender_name: source.name.clone(),
        execution,
    }
}

/// Copy a native action-veto context into the synchronous guest vocabulary.
///
/// This is deliberately total over the six native typed ask sites. No world
/// borrow crosses the boundary, so calling it under the tick-owned world guard
/// cannot provide a route back into that guard.
#[must_use]
pub fn lift_verdict_context(
    context: &lodestone_ecs::veto::VerbContext,
) -> Option<VerdictContext> {
    use lodestone_ecs::veto::VerbContext as Native;

    let block_pos = |pos: lodestone_model::BlockPos| BlockPos {
        x: pos.x,
        y: pos.y,
        z: pos.z,
    };
    match context {
        Native::BlockBreak { pos, state_id } => Some(VerdictContext::BlockBreak(BlockBreakVerdict {
            pos: block_pos(*pos),
            state_id: *state_id,
        })),
        Native::BlockPlace { pos } => Some(VerdictContext::BlockPlace(BlockPlaceVerdict {
            pos: block_pos(*pos),
        })),
        Native::EntityDamage { target_entity_id } => Some(VerdictContext::EntityDamage(EntityDamageVerdict {
            target_entity_id: *target_entity_id,
        })),
        Native::InventoryClick {
            window_id,
            slot,
            button,
        } => Some(VerdictContext::InventoryClick(InventoryClickVerdict {
            window_id: *window_id,
            slot: *slot,
            button: *button,
        })),
        Native::PlayerMove {
            moving,
            jumping,
            sprinting,
        } => Some(VerdictContext::PlayerMove(PlayerMoveVerdict {
            moving: *moving,
            jumping: *jumping,
            sprinting: *sprinting,
        })),
        Native::PlayerInteract {
            pos,
            target_entity_id,
        } => Some(VerdictContext::PlayerInteract(PlayerInteractVerdict {
            pos: pos.map(block_pos),
            target_entity_id: *target_entity_id,
        })),
        // A future native context cannot silently cross an ABI that has no
        // representable copy of it. The host turns this into a fail-closed ask.
        _ => None,
    }
}

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

/// Copy one resolved local-player placement into the bounded guest vocabulary.
///
/// `Idle` is deliberately absent: it does not advance the native generation and
/// is not an outcome. The conductor compares generations per observing guest, so
/// each non-idle answer is delivered once rather than repeated every tick.
#[must_use]
pub fn lift_place_outcome(
    outcome: &lodestone_ecs::player::PlaceOutcome,
) -> Option<PlaceOutcome> {
    let status = match outcome.status {
        lodestone_ecs::player::PlaceStatus::Idle => return None,
        lodestone_ecs::player::PlaceStatus::Predicted => PlaceStatus::Predicted,
        lodestone_ecs::player::PlaceStatus::SentUnpredicted => PlaceStatus::SentUnpredicted,
        lodestone_ecs::player::PlaceStatus::Rejected(rejection) => {
            PlaceStatus::Rejected(match rejection {
                lodestone_ecs::player::PlaceRejection::Dead => PlaceRejection::Dead,
                lodestone_ecs::player::PlaceRejection::UnreachableOrObstructed => {
                    PlaceRejection::UnreachableOrObstructed
                }
                lodestone_ecs::player::PlaceRejection::NoWorldData => PlaceRejection::NoWorldData,
                lodestone_ecs::player::PlaceRejection::NothingPlaceableHeld => {
                    PlaceRejection::NothingPlaceableHeld
                }
                lodestone_ecs::player::PlaceRejection::IntersectsPlayer => {
                    PlaceRejection::IntersectsPlayer
                }
                lodestone_ecs::player::PlaceRejection::Vetoed => PlaceRejection::Vetoed,
            })
        }
    };
    Some(PlaceOutcome {
        status,
        generation: outcome.generation,
    })
}

/// Copy a changed local-player mining state into the bounded guest vocabulary.
/// `Idle` is meaningful here: it tells a guest that its explicit release has
/// reached the lifecycle. The conductor suppresses repeated equal statuses.
#[must_use]
pub fn lift_break_outcome(outcome: &lodestone_ecs::player::BreakOutcome) -> BreakOutcome {
    let status = match outcome.0 {
        lodestone_ecs::player::BreakStatus::Idle => BreakStatus::Idle,
        lodestone_ecs::player::BreakStatus::Progressing => BreakStatus::Progressing,
        lodestone_ecs::player::BreakStatus::Rejected(rejection) => {
            BreakStatus::Rejected(match rejection {
                lodestone_ecs::player::BreakRejection::Dead => BreakRejection::Dead,
                lodestone_ecs::player::BreakRejection::UnreachableOrObstructed => {
                    BreakRejection::UnreachableOrObstructed
                }
                lodestone_ecs::player::BreakRejection::NoWorldData => BreakRejection::NoWorldData,
                lodestone_ecs::player::BreakRejection::UnknownBlockState => {
                    BreakRejection::UnknownBlockState
                }
                lodestone_ecs::player::BreakRejection::Vetoed => BreakRejection::Vetoed,
            })
        }
    };
    BreakOutcome { status }
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
        Action::SetLook(_) => Capability::ActLook,
        Action::SetMovement(_) => Capability::ActMovement,
        Action::SetBreak(_) => Capability::ActBreak,
        Action::PlaceBlock(_) => Capability::ActPlace,
        Action::SelectSlot(_) => Capability::ActSelectSlot,
    }
}

/// Lower one guest action into a `ClientAction`, or refuse it.
///
/// `Err` carries the capability that was missing, so a host can log *which* grant a
/// plugin needed rather than "an action was dropped". A refusal is counted and
/// logged by the conductor, never silent: a plugin whose actions vanish with no
/// explanation is the single most confusing thing a plugin API can do.
pub fn lower_action(action: Action, granted: &CapabilitySet) -> Result<LoweredAction, Capability> {
    let needed = capability_for(&action);
    if !granted.contains(needed) {
        return Err(needed);
    }
    Ok(match action {
        Action::SendChat(text) => LoweredAction::Client(ClientAction::SendChat { text }),
        Action::SendCommand(command) => LoweredAction::Client(ClientAction::SendCommand { command }),
        Action::SwingArm(hand) => LoweredAction::Client(ClientAction::SwingArm {
            hand: match hand {
                Hand::Main => lodestone_model::common::Hand::Main,
                Hand::Off => lodestone_model::common::Hand::Off,
            },
        }),
        Action::SetLook(look) => LoweredAction::Intent(IntentAction::Look(look.map(|look| {
            lodestone_ecs::player::LookIntent {
                yaw: look.yaw,
                pitch: look.pitch,
            }
        }))),
        Action::SetMovement(movement) => {
            LoweredAction::Intent(IntentAction::Movement(movement.map(|movement| {
                MovementOverride {
                    forward: axis(movement.forward),
                    strafe: axis(movement.strafe),
                    jump: movement.jump,
                    sneak: movement.sneak,
                    sprint: movement.sprint,
                }
            })))
        }
        Action::SetBreak(intent) => LoweredAction::Intent(IntentAction::Break(intent.map(|intent| {
            lodestone_ecs::player::BreakIntent {
                pos: lodestone_model::BlockPos::new(intent.pos.x, intent.pos.y, intent.pos.z),
                face: match intent.face {
                    BlockFace::Down => lodestone_model::BlockFace::Down,
                    BlockFace::Up => lodestone_model::BlockFace::Up,
                    BlockFace::North => lodestone_model::BlockFace::North,
                    BlockFace::South => lodestone_model::BlockFace::South,
                    BlockFace::West => lodestone_model::BlockFace::West,
                    BlockFace::East => lodestone_model::BlockFace::East,
                },
            }
        }))),
        Action::PlaceBlock(intent) => LoweredAction::Intent(IntentAction::Place(
            lodestone_ecs::player::PlaceIntent {
                pos: lodestone_model::BlockPos::new(intent.pos.x, intent.pos.y, intent.pos.z),
                face: match intent.face {
                    BlockFace::Down => lodestone_model::BlockFace::Down,
                    BlockFace::Up => lodestone_model::BlockFace::Up,
                    BlockFace::North => lodestone_model::BlockFace::North,
                    BlockFace::South => lodestone_model::BlockFace::South,
                    BlockFace::West => lodestone_model::BlockFace::West,
                    BlockFace::East => lodestone_model::BlockFace::East,
                },
            },
        )),
        Action::SelectSlot(slot) => LoweredAction::Intent(IntentAction::SelectSlot(usize::from(slot))),
    })
}

#[cfg(test)]
mod tests {
    use lodestone_model::Text;

    use crate::host::PlaceIntent;

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

    /// The protocol-facing arms lower onto the real `ClientAction` vocabulary.
    #[test]
    fn actions_lower_onto_the_real_client_action_vocabulary() {
        let granted = CapabilitySet::permissive();
        assert_eq!(
            lower_action(Action::SendChat("hi".to_owned()), &granted),
            Ok(LoweredAction::Client(ClientAction::SendChat {
                text: "hi".to_owned()
            }))
        );
        assert_eq!(
            lower_action(Action::SendCommand("time set day".to_owned()), &granted),
            Ok(LoweredAction::Client(ClientAction::SendCommand {
                command: "time set day".to_owned()
            }))
        );
        assert_eq!(
            lower_action(Action::SwingArm(Hand::Off), &granted),
            Ok(LoweredAction::Client(ClientAction::SwingArm {
                hand: lodestone_model::common::Hand::Off
            }))
        );
    }

    /// Selection crosses as an intent, never as a hand-built carried-item
    /// packet. The shell keeps both its range gate and the ordered echo.
    #[test]
    fn a_selected_slot_lowers_onto_the_shell_owned_intent() {
        let granted = CapabilitySet::from_iter([Capability::ActSelectSlot]);
        assert_eq!(
            lower_action(Action::SelectSlot(6), &granted),
            Ok(LoweredAction::Intent(IntentAction::SelectSlot(6)))
        );
    }

    /// Look ownership crosses as a copied intent, not a fabricated movement
    /// packet. The existing local-player systems own clamping, physics, and send.
    #[test]
    fn look_actions_lower_onto_the_local_player_intent_vocabulary() {
        let granted = CapabilitySet::from_iter([Capability::ActLook]);
        assert_eq!(
            lower_action(
                Action::SetLook(Some(crate::host::LookIntent { yaw: 37.5, pitch: -12.0 })),
                &granted,
            ),
            Ok(LoweredAction::Intent(IntentAction::Look(Some(
                lodestone_ecs::player::LookIntent { yaw: 37.5, pitch: -12.0 }
            ))))
        );
        assert_eq!(
            lower_action(Action::SetLook(None), &granted),
            Ok(LoweredAction::Intent(IntentAction::Look(None)))
        );
    }

    /// Movement crosses as copied axes, not as a position or a protocol action.
    /// The numeric control makes the boundary reject both a non-digital finite
    /// axis and a NaN before either can reach physics.
    #[test]
    fn movement_actions_lower_onto_a_finite_digital_local_player_intent() {
        let granted = CapabilitySet::from_iter([Capability::ActMovement]);
        assert_eq!(
            lower_action(
                Action::SetMovement(Some(crate::host::MovementIntent {
                    forward: 4.0,
                    strafe: f32::NAN,
                    jump: true,
                    sneak: true,
                    sprint: true,
                })),
                &granted,
            ),
            Ok(LoweredAction::Intent(IntentAction::Movement(Some(
                MovementOverride {
                    forward: 1.0,
                    strafe: 0.0,
                    jump: true,
                    sneak: true,
                    sprint: true,
                }
            ))))
        );
        assert_eq!(
            lower_action(Action::SetMovement(None), &granted),
            Ok(LoweredAction::Intent(IntentAction::Movement(None)))
        );
    }

    /// Placement crosses as the two copied facts a ray hit contains. The guest
    /// cannot fabricate the held item, prediction sequence, block state, or a
    /// protocol action; the shell-owned placement lifecycle receives this intent.
    #[test]
    fn placement_actions_lower_onto_the_local_player_placement_lifecycle() {
        let granted = CapabilitySet::from_iter([Capability::ActPlace]);
        assert_eq!(
            lower_action(
                Action::PlaceBlock(PlaceIntent {
                    pos: BlockPos { x: -7, y: 64, z: 19 },
                    face: BlockFace::West,
                }),
                &granted,
            ),
            Ok(LoweredAction::Intent(IntentAction::Place(
                lodestone_ecs::player::PlaceIntent {
                    pos: lodestone_model::BlockPos::new(-7, 64, 19),
                    face: lodestone_model::BlockFace::West,
                }
            )))
        );
    }

    /// A break request owns only the copied target until the guest explicitly
    /// releases it; the shell owns every packet/progress detail beneath it.
    #[test]
    fn break_actions_lower_onto_the_persistent_local_player_mining_lifecycle() {
        let granted = CapabilitySet::from_iter([Capability::ActBreak]);
        assert_eq!(
            lower_action(
                Action::SetBreak(Some(crate::host::BreakIntent {
                    pos: BlockPos { x: -7, y: 64, z: 19 },
                    face: BlockFace::West,
                })),
                &granted,
            ),
            Ok(LoweredAction::Intent(IntentAction::Break(Some(
                lodestone_ecs::player::BreakIntent {
                    pos: lodestone_model::BlockPos::new(-7, 64, 19),
                    face: lodestone_model::BlockFace::West,
                }
            ))))
        );
        assert_eq!(
            lower_action(Action::SetBreak(None), &granted),
            Ok(LoweredAction::Intent(IntentAction::Break(None)))
        );
    }

    /// The continuous lifecycle still crosses only a finite state vocabulary;
    /// the conductor, not this lift, suppresses repeated equal states.
    #[test]
    fn break_outcomes_lift_without_world_handles_or_error_strings() {
        assert_eq!(
            lift_break_outcome(&lodestone_ecs::player::BreakOutcome(
                lodestone_ecs::player::BreakStatus::Rejected(
                    lodestone_ecs::player::BreakRejection::NoWorldData,
                ),
            )),
            crate::host::BreakOutcome {
                status: crate::host::BreakStatus::Rejected(
                    crate::host::BreakRejection::NoWorldData,
                ),
            }
        );
    }

    /// The outcome vocabulary is finite and preserves the generation that makes
    /// a one-shot placement result distinguishable from an older result.
    #[test]
    fn placement_outcomes_lift_without_an_idle_poll_or_unbounded_error() {
        assert_eq!(
            lift_place_outcome(&lodestone_ecs::player::PlaceOutcome {
                status: lodestone_ecs::player::PlaceStatus::Rejected(
                    lodestone_ecs::player::PlaceRejection::NoWorldData,
                ),
                generation: 17,
            }),
            Some(crate::host::PlaceOutcome {
                status: crate::host::PlaceStatus::Rejected(crate::host::PlaceRejection::NoWorldData),
                generation: 17,
            })
        );
        assert_eq!(
            lift_place_outcome(&lodestone_ecs::player::PlaceOutcome::default()),
            None,
            "idle is not an outcome and must not become an every-tick guest event"
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
        assert_eq!(
            lower_action(Action::SetLook(None), &CapabilitySet::empty()),
            Err(Capability::ActLook)
        );
        assert_eq!(
            lower_action(Action::SetMovement(None), &CapabilitySet::empty()),
            Err(Capability::ActMovement)
        );
        assert_eq!(
            lower_action(
                Action::PlaceBlock(PlaceIntent {
                    pos: BlockPos { x: 0, y: 0, z: 0 },
                    face: BlockFace::Up,
                }),
                &CapabilitySet::empty(),
            ),
            Err(Capability::ActPlace)
        );
        assert_eq!(
            lower_action(Action::SetBreak(None), &CapabilitySet::empty()),
            Err(Capability::ActBreak)
        );
        assert_eq!(
            lower_action(Action::SelectSlot(6), &CapabilitySet::empty()),
            Err(Capability::ActSelectSlot)
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
            Action::SetLook(None),
            Action::SetMovement(None),
            Action::SetBreak(None),
            Action::PlaceBlock(PlaceIntent {
                pos: BlockPos { x: 0, y: 0, z: 0 },
                face: BlockFace::Up,
            }),
            Action::SelectSlot(6),
        ] {
            assert!(
                lower_action(action.clone(), &CapabilitySet::empty()).is_err(),
                "{action:?} must be refused by an empty capability set"
            );
        }
    }
}
