//! Client-side body-yaw tracking for remote player entities.

use super::*;

/// The client-simulated body yaw for a **player** entity — vanilla's own
/// body-yaw field, run locally because nothing ever sends it.
///
/// A player's own `Rotation`/`HeadYaw` arrive over the wire **equal**:
/// vanilla's own entity-changes broadcast packs the entity's own yaw accessor
/// as the move/rotation packet's angle and its own head-yaw accessor
/// as the head packet's, and vanilla's own player AI-step forces `this.yHeadRot =
/// this.getYRot()` every tick — a player has no second, independently-aimed
/// value the way a `Mob`'s `LookControl` gives its head. So feeding the
/// reported [`Rotation`] yaw straight into [`EntityFacts::yaw`], which is
/// exactly right for a mob (whose body and AI-aimed head genuinely diverge
/// on the wire already), makes a *player's* body and head numerically
/// identical forever — the "turns as one rigid block, head never moves"
/// report this component exists to fix.
///
/// Real vanilla clients never receive a body yaw for another player either:
/// every receiving client's own `RemotePlayer` puppet runs
/// vanilla's own living-entity tick's generic head-turn lag **locally**, deriving
/// its rendered body yaw from the received look yaw and that puppet's own
/// per-tick movement. [`tick_remote_body_yaw`] is that same simulation,
/// reusing [`crate::sim::step::body_yaw_target`]/[`crate::sim::step::tick_head_turn`]
/// — the identical port `sim/step.rs` already wrote for the local
/// third-person body — so the rule has one implementation, not two.
///
/// Lives on the **ingest** entity, beside `Rotation`/`HeadYaw`/`Position`,
/// not the render track: [`resolve_entity_facts`] reads it directly the same
/// way it reads those.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct BodyYawState {
    /// Vanilla's own body-yaw field.
    pub(super) yaw: f32,
    /// This entity's `Position` as of last tick — vanilla's own previous-position
    /// pair, the reference [`crate::sim::step::body_yaw_target`]'s `(dx, dz)` is
    /// measured against.
    pub(super) last_feet: lodestone_model::Vec3,
}

/// `GameTick`/`TickSet::Animate`: advances [`BodyYawState`] for every tracked
/// **player** entity, once per tick — the client-side half of
/// vanilla's own living-entity head-turn tick real vanilla runs for every other player's
/// puppet on every receiving client. See [`BodyYawState`]'s doc for why a
/// player needs this simulation and a mob (whose `Rotation` is already an
/// independent, AI-driven body yaw) must not get it — gated here on
/// `EntityKind::path` `== "player"`, exactly the check `resolve_entity_facts`
/// already uses for `is_player`.
///
/// Two passes over disjoint archetypes (`Without<BodyYawState>` for the
/// lazy-insert half) rather than one `Option<&mut BodyYawState>` query,
/// because a fresh player needs [`Commands`] to gain the component at all —
/// initialised to its own reported yaw, matching vanilla's spawn-time
/// `this.yBodyRot = this.getYRot()`, so a fresh join starts rigid for
/// exactly the one tick nothing has told it otherwise yet, not eased in from
/// a guessed value.
///
/// `attacking` reads [`AttackSwing::attack_anim`] the same tick
/// `lodestone_ecs::ingest::tick_entity_swing` ages it — one tick "behind"
/// vanilla's own ordering, the same way `sim/step.rs`'s identical call site
/// already documents itself as being, for the identical reason.
///
/// The `50.0` `max_head_rotation` is vanilla's un-narrowed
/// max-head-rotation-relative-to-body accessor. `Player`'s own `15.0`
/// override while blocking with a shield is **not** modelled here: it needs
/// a decoded remote block/use-item state this crate does not carry yet, so a
/// blocking remote player's body currently drags at the wider, un-narrowed
/// angle rather than vanilla's tighter one — a narrower gap than the "rigid
/// block" bug this fixes, and recorded rather than silently assumed away.
pub fn tick_remote_body_yaw(
    mut existing: Query<(
        &lodestone_ecs::entity::EntityKind,
        &lodestone_ecs::entity::Position,
        &lodestone_ecs::entity::Rotation,
        Option<&AttackSwing>,
        &mut BodyYawState,
    )>,
    missing: Query<
        (
            Entity,
            &lodestone_ecs::entity::EntityKind,
            &lodestone_ecs::entity::Position,
            &lodestone_ecs::entity::Rotation,
        ),
        Without<BodyYawState>,
    >,
    mut commands: Commands,
) {
    const MAX_HEAD_ROTATION_DEG: f32 = 50.0;
    for (kind, position, rotation, swing, mut state) in &mut existing {
        if kind.0.path() != "player" {
            continue;
        }
        let dx = position.0.x - state.last_feet.x;
        let dz = position.0.z - state.last_feet.z;
        let attacking = swing.is_some_and(|swing| swing.attack_anim > 0.0);
        let target =
            crate::sim::step::body_yaw_target(state.yaw, rotation.0.yaw, dx, dz, attacking);
        state.yaw = crate::sim::step::tick_head_turn(
            state.yaw,
            rotation.0.yaw,
            target,
            MAX_HEAD_ROTATION_DEG,
        );
        state.last_feet = position.0;
    }
    for (entity, kind, position, rotation) in &missing {
        if kind.0.path() != "player" {
            continue;
        }
        commands.entity(entity).insert(BodyYawState {
            yaw: rotation.0.yaw,
            last_feet: position.0,
        });
    }
}
