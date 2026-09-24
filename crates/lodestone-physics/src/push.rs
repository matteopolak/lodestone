//! Entity-versus-entity interaction: the **soft push** (vanilla's own entity
//! push, driven by its crowd-push pass) and the **hard-collision** half of
//! vanilla's own no-collision check (querying entity collisions from the
//! level).
//!
//! These are two different mechanisms gated by two different predicates, and the
//! most consequential thing to know before touching this file is which entities
//! each one actually applies to in 26.2.
//!
//! # "is pushable" and "can be collided with" are not two spellings of one idea
//!
//! | | predicate | reached through | default | living entity |
//! |---|---|---|---|---|
//! | **push** (soft, velocity) | vanilla's own "is pushable" check | vanilla's own pushable-by selector → the level's pushable-entities query → the crowd-push pass | `false` | **overridden**: `isAlive() && !isSpectator() && !onClimbable()` |
//! | **collide** (hard, blocks movement) | vanilla's own "can be collided with" check | vanilla's own "can collide with" → the level's entity-collisions query → collide / no-collision | `false` | **not overridden** |
//!
//! So a player or a mob is *pushable* and **not** *collidable*. Two players walk
//! through each other and shove each other apart; they never clip. Exhaustively,
//! the only overrides of "can be collided with" in the whole 26.2 tree are:
//!
//! * the boat family — `true` unconditionally, and it also overrides "can
//!   collide with" to admit a *pushable* entity as a collider even when that
//!   entity is not itself collidable — that is the "you stand on a boat"
//!   case, and it is exactly the asymmetry that makes these two predicates
//!   impossible to merge;
//! * the shulker — alive;
//! * the happy ghast — a state machine over baby/vehicle/still-timeout, with
//!   a client-only clause admitting a player standing on its back.
//!
//! **This corrects a common framing.** "Players and mobs pass straight through
//! each other" is not a Lodestone defect; it is vanilla. What is missing here is
//! the *push*, which is a real and visible divergence, plus the collision half for
//! the three entity families above.
//!
//! # Who moves, and how vanilla dodges the ordering problem
//!
//! Vanilla's own entity push is **symmetric**: it computes one horizontal
//! vector from the two positions and hands `-v` to `this` and `+v` to the
//! other entity, each gated independently on `!isVehicle() && isPushable()`.
//! A ridden entity (vanilla's own "is vehicle" check) absorbs the shove and passes it to
//! nobody.
//!
//! Naive pairwise separation has a real ordering dependency, and vanilla's answer
//! is not a tie-break rule — it is that **nothing moves during the pass**. The
//! push is added to velocity, positions are read and never written, and the
//! crowd-push pass runs at the *end* of the per-tick player/mob update —
//! after travel has already moved everything for this tick. So the impulses
//! a given entity receives in one tick are a **set computed from a frozen
//! snapshot of positions**, and summing a set is order-independent. There
//! is no relaxation loop, no penetration-depth solve, and no "iterate in
//! entity-id order".
//!
//! The residue is that `+` on `f64` is not associative, so a crowd of two or more
//! pushers can land on a different last bit depending on tick order — a bound of
//! about one ulp of the velocity, five orders of magnitude below the 0.25-block
//! rubber-band threshold, and *not* something vanilla itself pins (the server's
//! entity iteration order is not observable from a client). Do not chase it.
//!
//! # There is no per-entity push cap
//!
//! Worth stating because it is widely believed: nothing limits how many entities
//! push one entity in a tick. Vanilla's own crowd-push pass iterates the
//! whole list. What the crowd-damage game rule does is deal **6.0 cramming
//! damage**, server-side only, with a random one-in-four probability gate —
//! damage, not a movement clamp, and invisible to a client's physics. Crowd
//! behaviour is limited by the `0.05F` per-pair magnitude and by drag, not
//! by a counter.
//!
//! # What a client actually experiences
//!
//! Vanilla's own pushable-by selector has a clause that changes the whole
//! shape of the port: the pusher admits the pushee only when the pusher's
//! level is *not* client-side, **or** the pushee is a player and specifically
//! the local player — any other case (in particular, a remote entity trying
//! to push a remote player on a client) returns false outright.
//!
//! `entity` is the *pusher*, `input` the *pushee*. On a client, the only admissible
//! pushee is the local player. Therefore:
//!
//! * the local player's own crowd-push pass finds **nothing** (the
//!   candidate list excludes itself, and no other candidate is the local
//!   player), so a vanilla client never initiates a push;
//! * every push the local player feels arrives from some *other* entity's
//!   per-tick update — which the client does run, unconditionally, for
//!   remote entities (the travel call inside is gated on being
//!   effectively-AI-controlled, but the crowd-push call is not). Vanilla's
//!   own remote-player entity makes this unmistakable: its per-tick update
//!   override discards the entire living-entity body and keeps
//!   interpolation, swing/bob timers and **the crowd-push call itself** —
//!   on the client, a remote player's whole physics contribution *is*
//!   shoving the local player;
//! * because vanilla's own entity push is symmetric, iterating *our*
//!   neighbours and applying the receive-half to ourselves reproduces that
//!   exactly — the pair test (`box.intersects(box)`) and the magnitude are
//!   both symmetric.
//!
//! One measured consequence to record rather than to "fix": the **server applies
//! the impulse twice per pair per tick and the client once**. On a server both
//! sides' crowd-push passes run and each calls the symmetric push, so the
//! player is shoved by its own pass *and* by the mob's; on a client only the mob's
//! pass qualifies. That is vanilla client behaviour, so a client that models `2×`
//! would be the one out of step with its peers. It also does not itself trip the
//! rubber-band check, which re-runs the *claimed* delta through collision rather
//! than re-deriving velocity (see [`crate::player::maybe_back_off_from_edge`]).
//!
//! # What this module deliberately does not do
//!
//! It does not fetch a list of nearby entities — [`CollisionView`] answers *block*
//! geometry and gains no method here. Entity data is a different concern with a
//! different lifetime (a per-tick snapshot, not a spatial query the physics engine
//! can repeat), so it arrives as a caller-owned `&[NearbyEntity]` slice. See
//! `docs/entity-push.md` for the exact shell-side contract.

use crate::collision::{CollisionView, no_collision};
use crate::geometry::{Aabb, Vec3d};
use crate::mth;

/// Vanilla's own entity-push separation gate: `dd >= 0.01F` — a `float`
/// literal compared against a `double`, so the real threshold is the widened
/// `0.01f`, `0.009999999776482582…`, and **not** `0.01`.
const MIN_SEPARATION: f64 = 0.01_f32 as f64;

/// Vanilla's own entity-push scale: `xa *= 0.05F` — likewise a widened
/// `float`, so the real scale is `0.05000000074505806…`. Writing `0.05` here
/// is a silent last-bits divergence on every push.
const PUSH_SCALE: f64 = 0.05_f32 as f64;

/// Vanilla's own scoreboard-team collision rule — the team gate on pushing.
///
/// An entity with no team resolves to [`Self::Always`], which is why that is the
/// [`Default`]: a vanilla server with no scoreboard teams gives every pair
/// `(Always, Always, not allied)` and the gate is transparent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CollisionRule {
    /// Vanilla's own default — and the resolved value for a team-less entity.
    #[default]
    Always,
    /// Vetoes the pair from either side.
    Never,
    /// Pushes allies only.
    PushOwnTeam,
    /// Pushes non-allies only.
    PushOtherTeams,
}

/// The team half of vanilla's own pushable-by selector, with `own` the
/// *pusher*'s rule and `theirs` the *pushee*'s.
///
/// Ported literally, including Java's operator precedence in the final line
/// (`a != X && b != X || sameTeam` groups as `(a != X && b != X) || sameTeam`) and
/// the fact that a push-own-team rule on **either** side vetoes an allied
/// pair while a push-other-teams rule on either side vetoes a non-allied
/// one. The two rules are not mirror images of each other, which is easy to
/// "simplify" wrongly.
#[must_use]
pub fn team_allows_push(own: CollisionRule, theirs: CollisionRule, same_team: bool) -> bool {
    if own == CollisionRule::Never || theirs == CollisionRule::Never {
        return false;
    }
    if (own == CollisionRule::PushOwnTeam || theirs == CollisionRule::PushOwnTeam) && same_team {
        return false;
    }
    (own != CollisionRule::PushOtherTeams && theirs != CollisionRule::PushOtherTeams) || same_team
}

/// One nearby entity, as a caller must hand it to this module — a **snapshot**,
/// valid for exactly the tick it was built for.
///
/// Every field is one named vanilla call, so a producer can be checked against the
/// source field by field rather than by intent. Nothing here is derived from
/// anything else here: in particular [`Self::position`] is vanilla's own feet
/// position and is *not* recoverable from [`Self::bounding_box`] — the push
/// direction reads vanilla's own X/Z accessors while the pair test reads the box, and
/// conflating them would be a guess about an entity whose box is offset from
/// its position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NearbyEntity {
    /// Vanilla's own feet-centre position. Only `x` and `z` are read (the
    /// push is horizontal-only); `y` is carried so a caller need not strip it.
    pub position: Vec3d,
    /// Vanilla's own world-space bounding box. Drives the pair test and, for
    /// a collidable entity, *is* the collider handed to the movement sweep.
    pub bounding_box: Aabb,
    /// Vanilla's own "is pushable" check. For a living entity that is
    /// `isAlive() && !isSpectator() && !onClimbable()` — note the **ladder
    /// veto**: a mob on a ladder neither pushes nor is pushed. `false` for
    /// the base entity type, so an arrow, an item or an armour stand never
    /// participates.
    pub pushable: bool,
    /// Whether this entity is a producer for vanilla's own crowd-push pass.
    ///
    /// This differs from [`Self::pushable`], which is the *receiver*
    /// predicate. A boat is neither a crowd-push producer nor a pushable
    /// living entity, but it can still be present in this mixed snapshot
    /// because [`Self::collidable`] is true.
    pub pushes_players: bool,
    /// Vanilla's own "can be collided with" check — hard collision. `false`
    /// for players and every mob; `true` only for boats, shulkers and
    /// (conditionally) happy ghasts. See this module's header table.
    pub collidable: bool,
    /// Vanilla's own "is vehicle" check — has at least one passenger. Vetoes
    /// *receiving* a push but not being a collider.
    pub is_vehicle: bool,
    /// Vanilla's own "no physics" flag. Vetoes the whole pair from either
    /// side. A spectating player has it set every tick.
    pub no_physics: bool,
    /// Vanilla's own "is spectator" check — applied to the pushee by the
    /// pushable-by selector and to the collider by the entity-collisions
    /// query.
    pub spectator: bool,
    /// Vanilla's own "is passenger of same vehicle" check — two passengers
    /// of one boat neither push nor collide.
    pub same_vehicle: bool,
    /// This entity's team `CollisionRule`; [`CollisionRule::Always`] when it has
    /// no team.
    pub collision_rule: CollisionRule,
    /// `ownTeam.isAlliedTo(theirTeam)` — `false` whenever *either* side is
    /// team-less, because vanilla guards it with `ownTeam != null`.
    pub allied: bool,
}

impl NearbyEntity {
    /// An ordinary living, unridden, un-teamed, non-spectating mob or player at
    /// `position` with the given box: pushable, not collidable — the shape the
    /// overwhelming majority of entities take.
    #[must_use]
    pub fn living(position: Vec3d, bounding_box: Aabb) -> Self {
        Self {
            position,
            bounding_box,
            pushable: true,
            pushes_players: true,
            collidable: false,
            is_vehicle: false,
            no_physics: false,
            spectator: false,
            same_vehicle: false,
            collision_rule: CollisionRule::Always,
            allied: false,
        }
    }
}

/// Our own side of the push gates — the handful of `this.*` calls vanilla's
/// own entity push and pushable-by selector make on the entity being pushed.
///
/// Deliberately **not** `Default`: `alive` must be `true` for an ordinary player
/// and `bool::default()` is `false`, so a derived `Default` would silently produce
/// a corpse that no push can move. Use [`Self::LIVING_PLAYER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushSelf {
    /// Vanilla's own "is alive" check.
    pub alive: bool,
    /// Vanilla's own "is spectator" check. Vetoes its own "is pushable"
    /// check *and*, via vanilla's own per-tick player update setting no-physics to match
    /// spectator state, the whole of vanilla's own entity push.
    pub spectator: bool,
    /// Vanilla's own "is vehicle" check — we are being ridden, so we absorb
    /// rather than receive.
    pub is_vehicle: bool,
    /// Our team's `CollisionRule`.
    pub collision_rule: CollisionRule,
}

impl PushSelf {
    /// An alive, non-spectating, unridden, un-teamed player — what a single-player
    /// client is on essentially every tick.
    pub const LIVING_PLAYER: Self = Self {
        alive: true,
        spectator: false,
        is_vehicle: false,
        collision_rule: CollisionRule::Always,
    };
}

/// Vanilla's own entity-push horizontal vector, computed once per pair and
/// *before* either receive gate. `None` is vanilla's `dd < 0.01F` early
/// exit — the pair is too concentric to have a direction.
///
/// `self_pos` is `this.position()`, `other_pos` is `entity.position()`, and the
/// returned vector points **from us toward them**: we receive its negation, they
/// receive it unchanged.
///
/// # What the arithmetic actually computes, which is not what it looks like
///
/// Writing `m = absMax(dx, dz)`, the five lines of division and multiplication
/// collapse exactly to
///
/// ```text
/// push = (dx/m, dz/m) * 0.05f * min(sqrt(m), 1.0)
/// ```
///
/// because `pow = 1/sqrt(m)` cancels one of the two `sqrt(m)` divisions whenever
/// the `if (pow > 1.0)` clamp does *not* bind. `(dx/m, dz/m)` is the separation
/// normalised by the **Chebyshev** norm, so its dominant component is exactly `±1`
/// and its length is between `1` and `√2`.
///
/// Three consequences, each of which contradicts a reading you would arrive at from
/// the shape of the source:
///
/// 1. **The normaliser is `sqrt(abs_max)`, not the vector length.** `abs_max` is the
///    larger component magnitude (see [`mth::abs_max`]). For `(0.15, 0.08)` the two
///    differ by 6%, on both axes, on every tick.
/// 2. **There is no distance falloff.** The magnitude is `0.05f × min(√m, 1)` times
///    a factor in `[1, √2]`: it *rises* with separation up to `m = 1` and is then
///    **flat** at `0.05f` forever. Two entities one block apart shove each other
///    exactly as hard as two entities five blocks apart — which is unobservable for
///    same-sized mobs, whose boxes stop overlapping at `m = width`, and quite
///    observable inside a happy ghast.
/// 3. **The `pow > 1.0` clamp is a soft-start near contact, not a cap on a
///    blow-up.** Removing it would make the magnitude a constant `0.05f`; with it,
///    a pair `0.05` apart is pushed by `0.05f × √0.05 ≈ 0.011` — a *quarter* of the
///    force. Nearly-concentric entities separate slowly and then accelerate, which
///    is exactly the pile-settling behaviour a naive "push apart by penetration
///    depth" rule gets backwards.
///
/// The literal transcription is kept rather than the collapsed form, because the
/// two are not bit-identical (`dx/√m·pow·0.05` rounds three times, `dx/m·0.05·√m`
/// rounds differently) and this crate is judged by bits.
#[must_use]
pub fn pair_push_vector(self_pos: Vec3d, other_pos: Vec3d) -> Option<Vec3d> {
    let mut xa = other_pos.x - self_pos.x;
    let mut za = other_pos.z - self_pos.z;
    let mut dd = mth::abs_max(xa, za);
    // Vanilla is `if (dd >= 0.01F) { … }` with no else, so the reject branch is the
    // *negation* of a `>=` and not a `<`. On `NaN` the two disagree: `!(NaN >= x)`
    // rejects (as Java does, since `NaN >= x` is false and the block is skipped)
    // while `NaN < x` is also false and would fall through into the push. Keep the
    // negated form.
    #[expect(
        clippy::neg_cmp_op_on_partial_ord,
        reason = "NaN must reject, which `dd < MIN_SEPARATION` would not do"
    )]
    if !(dd >= MIN_SEPARATION) {
        return None;
    }
    dd = dd.sqrt();
    xa /= dd;
    za /= dd;
    let mut pow = 1.0 / dd;
    if pow > 1.0 {
        pow = 1.0;
    }
    xa *= pow;
    za *= pow;
    xa *= PUSH_SCALE;
    za *= PUSH_SCALE;
    Some(Vec3d::new(xa, 0.0, za))
}

/// Whether a pair is admitted at all — the conjunction of vanilla's own
/// pushable-by selector applied to us as the pushee, the box test vanilla's
/// own entity query performs, and vanilla's own entity-push's two guards.
///
/// `self_pushable` is our resolved "is pushable" check (including the
/// "on climbable" term, which needs a world view and so is resolved by the caller — see
/// [`apply_entity_push`]).
fn pair_admitted(
    self_box: Aabb,
    self_no_physics: bool,
    self_pushable: bool,
    self_rule: CollisionRule,
    other: &NearbyEntity,
) -> bool {
    // Vanilla's own entity query: strict box intersection with **no epsilon
    // inflation**. The entity-collisions query inflates its query by 1.0E-7
    // and this one does not; keep them distinct.
    if !self_box.intersects(&other.bounding_box) {
        return false;
    }
    // This mixed snapshot also carries hard colliders. Only entities whose
    // own tick runs vanilla's own crowd-push pass may enter this crowd pass.
    if !other.pushes_players {
        return false;
    }
    // Vanilla's own "no spectators" filter on the pushee (us) and, via
    // vanilla's own "no physics" flag, on a spectating pusher.
    if self_no_physics || other.no_physics {
        return false;
    }
    // Vanilla's own entity push: `!this.isPassengerOfSameVehicle(entity)`.
    if other.same_vehicle {
        return false;
    }
    // Vanilla's own pushable-by selector: `if (!input.isPushable()) return
    // false;` — the *pushee*'s pushability is a list-membership condition,
    // not just a receive gate. It is therefore checked twice in vanilla and
    // the second check is redundant; kept here so the two call sites read
    // like the source.
    if !self_pushable {
        return false;
    }
    team_allows_push(other.collision_rule, self_rule, other.allied)
}

/// The shared crowd pass, accumulating **onto** `velocity` one push at a time.
///
/// Vanilla adds each push separately, so summing the impulses first and
/// adding once would be a different `f64` rounding in a crowd of two or
/// more. Both public entry points route through here so there is exactly
/// one accumulation order in
/// the crate: [`apply_entity_push`] hands it the real velocity, and
/// [`entity_push_impulse`] hands it a zero, which reproduces the same sequence of
/// adds with an offset of zero.
fn accumulate_pushes(
    velocity: &mut Vec3d,
    self_pos: Vec3d,
    self_box: Aabb,
    self_flags: PushSelf,
    self_pushable: bool,
    nearby: &[NearbyEntity],
) {
    // Vanilla's own per-tick player update: `noPhysics = isSpectator()`.
    let self_no_physics = self_flags.spectator;
    for other in nearby {
        if !pair_admitted(
            self_box,
            self_no_physics,
            self_pushable,
            self_flags.collision_rule,
            other,
        ) {
            continue;
        }
        let Some(v) = pair_push_vector(self_pos, other.position) else {
            continue;
        };
        // `if (!this.isVehicle() && this.isPushable()) this.push(-xa, 0.0, -za);`
        if self_flags.is_vehicle || !self_pushable {
            continue;
        }
        // Vanilla's own entity push drops a non-finite impulse whole, all
        // three components together.
        if !v.x.is_finite() || !v.z.is_finite() {
            continue;
        }
        *velocity = velocity.add(Vec3d::new(-v.x, 0.0, -v.z));
    }
}

/// The total impulse the local entity receives this tick from vanilla's own
/// crowd-push pass — vanilla's whole crowd pass, from the pushee's point of
/// view.
///
/// `self_pushable` is the resolved "is pushable" check; `self_box` is our
/// own bounding box; `nearby` is every entity whose box could overlap ours.
/// Candidates that fail any gate contribute nothing, so a caller may pass a
/// generously-sized neighbourhood.
///
/// Accumulated in slice order. See the module header on why that is
/// order-independent in exact arithmetic and within about one ulp in `f64`. Prefer
/// [`apply_entity_push`] when you hold the velocity: adding this total to a
/// velocity is one extra `f64` add compared with vanilla's sequence.
#[must_use]
pub fn entity_push_impulse(
    self_pos: Vec3d,
    self_box: Aabb,
    self_flags: PushSelf,
    self_pushable: bool,
    nearby: &[NearbyEntity],
) -> Vec3d {
    let mut impulse = Vec3d::ZERO;
    accumulate_pushes(
        &mut impulse,
        self_pos,
        self_box,
        self_flags,
        self_pushable,
        nearby,
    );
    impulse
}

/// The **other** half of the same symmetric pair: the impulse `other`
/// receives from vanilla's own entity push.
///
/// A client does not own remote entities' velocities — the server does, and their
/// positions arrive interpolated — so this exists for a caller that *simulates* an
/// entity (a local mob loop, a prediction of a boat we are riding). Applying it to
/// a server-driven remote entity is cosmetic at best and fights interpolation at
/// worst.
#[must_use]
pub fn reciprocal_push_impulse(
    self_pos: Vec3d,
    self_box: Aabb,
    self_flags: PushSelf,
    self_pushable: bool,
    other: &NearbyEntity,
) -> Vec3d {
    if !pair_admitted(
        self_box,
        self_flags.spectator,
        self_pushable,
        self_flags.collision_rule,
        other,
    ) {
        return Vec3d::ZERO;
    }
    let Some(v) = pair_push_vector(self_pos, other.position) else {
        return Vec3d::ZERO;
    };
    if other.is_vehicle || !other.pushable {
        return Vec3d::ZERO;
    }
    if !v.x.is_finite() || !v.z.is_finite() {
        return Vec3d::ZERO;
    }
    v
}

/// Vanilla's own "is pushable" check for the local entity:
/// `isAlive() && !isSpectator() && !onClimbable()`.
///
/// The "on climbable" term is why this needs a [`CollisionView`]: it is
/// vanilla's own "on climbable" check, the climbable tag at the entity's
/// **feet block position**, the same query [`crate::entity::travel_in_air`]
/// already makes. A player on a ladder is immovable — hold a ladder in a
/// mob crush and nothing shoves you off it.
#[must_use]
pub fn self_is_pushable(flags: PushSelf, position: Vec3d, view: &dyn CollisionView) -> bool {
    flags.alive
        && !flags.spectator
        && !view.is_climbable(
            mth::floor(position.x),
            mth::floor(position.y),
            mth::floor(position.z),
        )
}

/// Applies one tick of vanilla's own crowd-push pass to a
/// [`crate::player::PlayerState`].
///
/// **Call this at the very end of a tick, after [`crate::player::tick`].**
/// Vanilla runs its crowd-push pass at the end of its per-tick player
/// update, after travel, so the impulse lands on the velocity that the
/// *next* tick integrates and never on this tick's movement. Calling it
/// before the travel dispatch advances the push by a full tick and is
/// observable within two ticks.
pub fn apply_entity_push(
    state: &mut crate::player::PlayerState,
    view: &dyn CollisionView,
    profile: &crate::profile::PhysicsProfile,
    nearby: &[NearbyEntity],
    self_flags: PushSelf,
) {
    if nearby.is_empty() {
        return;
    }
    let pushable = self_is_pushable(self_flags, state.position, view);
    let self_box = state.bounding_box(profile);
    let self_pos = state.position;
    accumulate_pushes(
        &mut state.velocity,
        self_pos,
        self_box,
        self_flags,
        pushable,
        nearby,
    );
}

/// Vanilla's own entity-collisions query — the entity boxes that
/// participate in a movement sweep, appended to `out`.
///
/// Three details are ported rather than tidied:
///
/// * the degenerate-box bail is on vanilla's own box "get size", the **mean
///   edge length**, not a volume — `< 1.0E-7` returns nothing at all;
/// * the query box is `testArea.inflate(1.0E-7)`, an inflation the *push* pair test
///   pointedly does not have;
/// * the predicate is vanilla's own "no spectators, and can collide with"
///   check, i.e. `other.canBeCollidedWith(us) && !us.isPassengerOfSameVehicle(other)`.
///   It is [`NearbyEntity::collidable`] and **not** [`NearbyEntity::pushable`];
///   a mob contributes nothing here no matter how solid it looks.
///
/// The boxes are appended verbatim — vanilla's own single-box shape
/// construction over an entity's bounding box, so the per-box sweep in
/// [`crate::collision`] is exact for it with no voxel-shape machinery.
pub fn entity_collision_boxes(test_area: Aabb, nearby: &[NearbyEntity], out: &mut Vec<Aabb>) {
    if test_area.size() < 1.0E-7 {
        return;
    }
    let query = test_area.inflate(1.0E-7);
    for other in nearby {
        if other.spectator || !other.collidable || other.same_vehicle {
            continue;
        }
        if query.intersects(&other.bounding_box) {
            out.push(other.bounding_box);
        }
    }
}

/// Vanilla's own "no entity collision" check —
/// `getEntityCollisions(...).isEmpty()`.
#[must_use]
pub fn no_entity_collision(test_area: Aabb, nearby: &[NearbyEntity]) -> bool {
    let mut boxes = Vec::new();
    entity_collision_boxes(test_area, nearby, &mut boxes);
    boxes.is_empty()
}

/// Vanilla's own "no collision" check —
/// `noBlockCollision && noEntityCollision && noBorderCollision`.
///
/// This is the predicate vanilla's own "can player fit within blocks and
/// entities" check applies to a `deflate(1.0E-7)`d pose box, and the one
/// vanilla's own "can fall at least" check applies to its downward probe.
/// [`no_collision`] remains the block-only form for callers with no entity
/// snapshot.
///
/// **The world-border collision check is still unmodelled** — this engine has no world border,
/// so a box the border would block reads as free. That is a pre-existing gap, now
/// the *only* remaining term of the three.
#[must_use]
pub fn no_collision_among_entities(
    view: &dyn CollisionView,
    box_: Aabb,
    nearby: &[NearbyEntity],
) -> bool {
    no_collision(view, box_) && no_entity_collision(box_, nearby)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 0.6 × 1.8 player-shaped box with its feet centre at `(x, y, z)`.
    fn body(x: f64, y: f64, z: f64) -> Aabb {
        Aabb::new(x - 0.3, y, z - 0.3, x + 0.3, y + 1.8, z + 0.3)
    }

    /// A `w` × `h` box with its feet centre at `(x, y, z)`.
    fn wide_body(x: f64, y: f64, z: f64, w: f64, h: f64) -> Aabb {
        Aabb::new(x - w / 2.0, y, z - w / 2.0, x + w / 2.0, y + h, z + w / 2.0)
    }

    struct Empty;
    impl CollisionView for Empty {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
    }

    struct Ladders;
    impl CollisionView for Ladders {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        fn is_climbable(&self, _x: i32, _y: i32, _z: i32) -> bool {
            true
        }
    }

    #[test]
    fn the_normaliser_is_sqrt_of_abs_max_not_sqrt_of_the_squared_length() {
        // Hand-decoded from the jar. dx and dz are the *literal*
        // subtractions, not the decimals they look like: 0.65 - 0.5 is
        // 0.15000000000000002, and using 0.15 here would make this test wrong in the
        // last bits while still passing every inequality below.
        let dx = 0.65_f64 - 0.5;
        let dz = 0.58_f64 - 0.5;
        let dd = mth::abs_max(dx, dz).sqrt();
        assert_eq!(dd, dx.sqrt(), "x is the dominant axis here");
        //   pow = 1/dd = 2.582… > 1.0 -> clamped to 1.0
        let expect_x = dx / dd * 1.0 * PUSH_SCALE;
        let expect_z = dz / dd * 1.0 * PUSH_SCALE;
        let v = pair_push_vector(Vec3d::new(0.5, 1.0, 0.5), Vec3d::new(0.65, 1.0, 0.58)).unwrap();
        assert_eq!(v.x.to_bits(), expect_x.to_bits());
        assert_eq!(v.z.to_bits(), expect_z.to_bits());
        assert_eq!(v.y, 0.0, "the push is horizontal-only");

        // The control that makes the above an assertion about `abs_max` rather than
        // about arithmetic in general: normalising by the vector length instead
        // would be a visibly different answer, not a last-bits one.
        let by_length = (dx * dx + dz * dz).sqrt();
        let wrong_x = dx / by_length * PUSH_SCALE;
        assert!(
            (v.x - wrong_x).abs() > 1.0e-4,
            "sqrt(absMax) and 1/length must not coincide here: {} vs {wrong_x}",
            v.x
        );
    }

    #[test]
    fn the_magnitude_rises_to_a_plateau_and_never_falls_off() {
        // The measured shape of vanilla's own entity push, which is NOT a
        // distance falloff:
        //   |push| = 0.05f * min(sqrt(absMax), 1) * |d / absMax|,  |d/absMax| in [1, √2]
        // On a single axis that is exactly `0.05f * min(sqrt(m), 1)` — increasing in
        // m up to 1.0, then flat forever.
        let mag = |m: f64| {
            pair_push_vector(Vec3d::ZERO, Vec3d::new(m, 0.0, 0.0))
                .unwrap()
                .x
        };

        // Monotone rise below one block.
        let ramp = [0.01, 0.05, 0.15, 0.3, 0.6, 0.9];
        for w in ramp.windows(2) {
            assert!(
                mag(w[0]) < mag(w[1]),
                "closer must be *weaker*: {} at m={} vs {} at m={}",
                mag(w[0]),
                w[0],
                mag(w[1]),
                w[1]
            );
        }
        // The soft start is severe: at 0.05 apart the push is a quarter of full.
        assert!(mag(0.05) < 0.26 * PUSH_SCALE && mag(0.05) > 0.2 * PUSH_SCALE);

        // Flat plateau at the widened 0.05f from one block outward — the
        // `pow > 1.0` clamp releases and the two sqrt(m) terms cancel. Exact at
        // m = 1.0 (sqrt(1) is exact); a few ulps off beyond it, because the two
        // divisions by an inexact sqrt round twice and the cancellation is only
        // algebraic. That residue is itself the reason the collapsed closed form is
        // documented but not implemented.
        assert_eq!(mag(1.0).to_bits(), PUSH_SCALE.to_bits());
        for m in [1.2, 3.0, 40.0] {
            let err = (mag(m) - PUSH_SCALE).abs();
            assert!(
                err <= 2.0 * f64::EPSILON * PUSH_SCALE,
                "single-axis magnitude must plateau at 0.05f, m = {m}, got {} (err {err:e})",
                mag(m)
            );
        }
        // …and the plateau is genuinely the un-clamped branch, not the clamp still
        // binding: assert the branch condition directly.
        assert!(1.0 / 1.2_f64.sqrt() < 1.0);
        assert!(1.0 / 0.9_f64.sqrt() > 1.0);

        // The literal transcription and the collapsed closed form agree to within a
        // rounding, and are NOT bit-identical — which is why the source order is
        // kept. (Control for the doc comment's claim.)
        let m: f64 = 1.05;
        let collapsed = PUSH_SCALE * m.sqrt().min(1.0);
        assert!((mag(m) - collapsed).abs() < 1.0e-16);
    }

    #[test]
    fn the_separation_floor_is_the_widened_float_and_is_checked_on_abs_max() {
        // Exactly at the widened 0.01f the pair is admitted; one ulp below it is not.
        let at = 0.01_f32 as f64;
        assert!(pair_push_vector(Vec3d::ZERO, Vec3d::new(at, 0.0, 0.0)).is_some());
        let below = f64::from_bits(at.to_bits() - 1);
        assert!(pair_push_vector(Vec3d::ZERO, Vec3d::new(below, 0.0, 0.0)).is_none());
        // The control for "0.01f is not 0.01": a separation between the two decides
        // differently under each reading.
        assert!(at < 0.01, "0.01f widens *below* 0.01");
        let between = f64::from_bits(0.01_f64.to_bits() - 1);
        assert!(
            pair_push_vector(Vec3d::ZERO, Vec3d::new(between, 0.0, 0.0)).is_some(),
            "a naive `>= 0.01` would reject this pair"
        );
        // absMax, not length: a pair separated only diagonally by less than the
        // floor on both axes is rejected even though its length clears it.
        assert!(pair_push_vector(Vec3d::ZERO, Vec3d::new(0.009, 0.0, 0.009)).is_none());
    }

    #[test]
    fn a_non_finite_separation_is_rejected_the_way_java_rejects_it() {
        // `if (dd >= 0.01F)` skips its block on NaN. A `dd < 0.01F` reject would
        // *not* — `NaN < x` is false — and would emit a NaN impulse, which then
        // poisons the position permanently. Vanilla's own "abs max" propagates
        // the NaN because Java's own `max` does; Rust's `f64::max` does not,
        // hence `mth::java_max_f64`.
        assert!(mth::abs_max(f64::NAN, 0.5).is_nan());
        assert!(pair_push_vector(Vec3d::ZERO, Vec3d::new(f64::NAN, 0.0, 0.5)).is_none());
        assert!(pair_push_vector(Vec3d::ZERO, Vec3d::new(0.5, 0.0, f64::NAN)).is_none());
        // Infinities do clear the gate (`inf >= 0.01f`), and vanilla's own
        // entity push's `Double.isFinite` guard is what drops the resulting
        // impulse.
        let us = Vec3d::new(0.5, 1.0, 0.5);
        let mut huge =
            NearbyEntity::living(Vec3d::new(f64::INFINITY, 1.0, 0.5), body(0.65, 1.0, 0.5));
        huge.bounding_box = body(0.65, 1.0, 0.5);
        assert_eq!(
            entity_push_impulse(
                us,
                body(0.5, 1.0, 0.5),
                PushSelf::LIVING_PLAYER,
                true,
                &[huge],
            ),
            Vec3d::ZERO,
            "a non-finite impulse must be dropped whole, not partially applied"
        );
    }

    #[test]
    fn the_push_is_symmetric_and_points_the_two_bodies_apart() {
        let us = Vec3d::new(0.5, 1.0, 0.5);
        let them = NearbyEntity::living(Vec3d::new(0.65, 1.0, 0.5), body(0.65, 1.0, 0.5));
        let ours = entity_push_impulse(
            us,
            body(0.5, 1.0, 0.5),
            PushSelf::LIVING_PLAYER,
            true,
            &[them],
        );
        let theirs = reciprocal_push_impulse(
            us,
            body(0.5, 1.0, 0.5),
            PushSelf::LIVING_PLAYER,
            true,
            &them,
        );
        assert!(ours.x < 0.0, "we are shoved away from them (toward -x)");
        assert!(theirs.x > 0.0, "they are shoved away from us (toward +x)");
        assert_eq!(ours.x.to_bits(), (-theirs.x).to_bits(), "equal magnitudes");
        assert_eq!(ours.y, 0.0);
        assert_eq!(theirs.y, 0.0);
    }

    #[test]
    fn a_ridden_entity_absorbs_the_shove_and_a_rider_is_never_pushed_by_its_vehicle() {
        let us = Vec3d::new(0.5, 1.0, 0.5);
        let self_box = body(0.5, 1.0, 0.5);
        let mut them = NearbyEntity::living(Vec3d::new(0.65, 1.0, 0.5), body(0.65, 1.0, 0.5));

        // `!entity.isVehicle()` gates only *their* half; ours still lands.
        them.is_vehicle = true;
        let ours = entity_push_impulse(us, self_box, PushSelf::LIVING_PLAYER, true, &[them]);
        assert!(ours.x < 0.0);
        assert_eq!(
            reciprocal_push_impulse(us, self_box, PushSelf::LIVING_PLAYER, true, &them),
            Vec3d::ZERO
        );

        // `!this.isVehicle()` gates ours; theirs still lands.
        them.is_vehicle = false;
        let ridden = PushSelf {
            is_vehicle: true,
            ..PushSelf::LIVING_PLAYER
        };
        assert_eq!(
            entity_push_impulse(us, self_box, ridden, true, &[them]),
            Vec3d::ZERO
        );
        assert_ne!(
            reciprocal_push_impulse(us, self_box, ridden, true, &them),
            Vec3d::ZERO
        );

        // Two passengers of one vehicle: the pair is dropped from both sides.
        them.same_vehicle = true;
        assert_eq!(
            entity_push_impulse(us, self_box, PushSelf::LIVING_PLAYER, true, &[them]),
            Vec3d::ZERO
        );
        assert_eq!(
            reciprocal_push_impulse(us, self_box, PushSelf::LIVING_PLAYER, true, &them),
            Vec3d::ZERO
        );
    }

    #[test]
    fn the_pair_test_is_strict_box_intersection_with_no_inflation() {
        let self_box = body(0.5, 1.0, 0.5); // x in [0.2, 0.8]
        let us = Vec3d::new(0.5, 1.0, 0.5);
        // Flush at x = 0.8: `min < max` is false, so no push at all — even though
        // the separation (0.6) is far above the floor.
        let flush = NearbyEntity::living(Vec3d::new(1.1, 1.0, 0.5), body(1.1, 1.0, 0.5));
        assert_eq!(
            entity_push_impulse(us, self_box, PushSelf::LIVING_PLAYER, true, &[flush]),
            Vec3d::ZERO
        );
        // One ulp of overlap is enough.
        let nudged = f64::from_bits(1.1_f64.to_bits() - 8);
        let touching = NearbyEntity::living(Vec3d::new(nudged, 1.0, 0.5), body(nudged, 1.0, 0.5));
        assert_ne!(
            entity_push_impulse(us, self_box, PushSelf::LIVING_PLAYER, true, &[touching]),
            Vec3d::ZERO
        );
        // Vertical separation counts too: standing on someone's head is not a push.
        let above = NearbyEntity::living(Vec3d::new(0.5, 2.8, 0.5), body(0.5, 2.8, 0.5));
        assert_eq!(
            entity_push_impulse(us, self_box, PushSelf::LIVING_PLAYER, true, &[above]),
            Vec3d::ZERO
        );
    }

    #[test]
    fn a_ladder_makes_us_unpushable_and_the_control_shows_the_fixture_would_push() {
        let flags = PushSelf::LIVING_PLAYER;
        let pos = Vec3d::new(0.5, 1.0, 0.5);
        assert!(self_is_pushable(flags, pos, &Empty));
        assert!(
            !self_is_pushable(flags, pos, &Ladders),
            "a living entity's pushable check vetoes while it is on a climbable block"
        );
        let dead = PushSelf {
            alive: false,
            ..flags
        };
        assert!(!self_is_pushable(dead, pos, &Empty));
        let ghost = PushSelf {
            spectator: true,
            ..flags
        };
        assert!(!self_is_pushable(ghost, pos, &Empty));
    }

    #[test]
    fn crowd_impulses_accumulate_with_no_cap() {
        // There is no cramming-damage-style clamp on the movement side: eight
        // pushers deliver eight impulses. Arranged symmetrically in z so the x sum
        // is a clean multiple and the z terms cancel in pairs.
        let us = Vec3d::new(0.5, 1.0, 0.5);
        let self_box = body(0.5, 1.0, 0.5);
        let one = [NearbyEntity::living(
            Vec3d::new(0.65, 1.0, 0.5),
            body(0.65, 1.0, 0.5),
        )];
        let single = entity_push_impulse(us, self_box, PushSelf::LIVING_PLAYER, true, &one);
        let crowd: Vec<NearbyEntity> = (0..8).map(|_| one[0]).collect();
        let many = entity_push_impulse(us, self_box, PushSelf::LIVING_PLAYER, true, &crowd);
        assert!(
            (many.x - single.x * 8.0).abs() < 1.0e-15,
            "8 identical pushers must deliver 8x the impulse, got {} vs {}",
            many.x,
            single.x * 8.0
        );
    }

    #[test]
    fn team_rules_match_the_source_truth_table() {
        use CollisionRule::{Always, Never, PushOtherTeams, PushOwnTeam};
        // NEVER vetoes from either side, allied or not.
        for allied in [false, true] {
            assert!(!team_allows_push(Never, Always, allied));
            assert!(!team_allows_push(Always, Never, allied));
        }
        // PUSH_OWN_TEAM on either side vetoes an *allied* pair and admits a
        // non-allied one — which reads backwards until you notice vanilla names the
        // rule for who it *does* push and then negates it in this branch.
        assert!(!team_allows_push(PushOwnTeam, Always, true));
        assert!(!team_allows_push(Always, PushOwnTeam, true));
        assert!(team_allows_push(PushOwnTeam, Always, false));
        // PUSH_OTHER_TEAMS on either side vetoes a non-allied pair.
        assert!(!team_allows_push(PushOtherTeams, Always, false));
        assert!(!team_allows_push(Always, PushOtherTeams, false));
        assert!(team_allows_push(PushOtherTeams, Always, true));
        // The no-teams default is transparent.
        assert!(team_allows_push(Always, Always, false));
    }

    #[test]
    fn collision_and_push_are_gated_by_different_predicates() {
        let probe = body(0.5, 1.0, 0.5);
        // A mob: pushable, not collidable. It shoves and it does not block.
        let mob = NearbyEntity::living(Vec3d::new(0.65, 1.0, 0.5), body(0.65, 1.0, 0.5));
        assert!(
            no_entity_collision(probe, &[mob]),
            "a mob is not a collider — this is vanilla, not a gap"
        );
        assert_ne!(
            entity_push_impulse(
                Vec3d::new(0.5, 1.0, 0.5),
                probe,
                PushSelf::LIVING_PLAYER,
                true,
                &[mob],
            ),
            Vec3d::ZERO
        );

        // A boat: vanilla's own "can be collided with" check true *and* its own "is pushable" check true — both halves.
        let mut boat = mob;
        boat.collidable = true;
        assert!(!no_entity_collision(probe, &[boat]));

        // A shulker-shaped case: collidable but NOT pushable. It blocks and it
        // never shoves — the inverse asymmetry, and the reason one boolean cannot
        // serve both questions.
        let mut shulker = mob;
        shulker.collidable = true;
        shulker.pushable = false;
        assert!(!no_entity_collision(probe, &[shulker]));
        assert_eq!(
            reciprocal_push_impulse(
                Vec3d::new(0.5, 1.0, 0.5),
                probe,
                PushSelf::LIVING_PLAYER,
                true,
                &shulker,
            ),
            Vec3d::ZERO
        );
    }

    #[test]
    fn entity_collision_boxes_ports_the_inflation_the_size_bail_and_the_spectator_filter() {
        let mut boat = NearbyEntity::living(
            Vec3d::new(2.0, 1.0, 0.5),
            wide_body(2.0, 1.0, 0.5, 1.375, 0.5625),
        );
        boat.collidable = true;

        // The 1.0E-7 inflation: a probe flush against the boat's face collides here
        // where the *push* pair test (uninflated, strict) would not.
        let flush = Aabb::new(
            boat.bounding_box.max_x,
            1.0,
            0.2,
            boat.bounding_box.max_x + 0.6,
            2.8,
            0.8,
        );
        let mut out = Vec::new();
        entity_collision_boxes(flush, &[boat], &mut out);
        assert_eq!(out.len(), 1, "inflate(1.0E-7) admits a flush contact");
        assert!(
            !flush.intersects(&boat.bounding_box),
            "…and the uninflated test would not — that is the whole point"
        );

        // The degenerate bail is on the *mean edge length*.
        let degenerate = Aabb::new(2.0, 1.0, 0.5, 2.0, 1.0, 0.5);
        assert!(degenerate.size() < 1.0E-7);
        out.clear();
        entity_collision_boxes(degenerate, &[boat], &mut out);
        assert!(out.is_empty());

        // A spectator contributes nothing even if flagged collidable.
        let mut ghost = boat;
        ghost.spectator = true;
        out.clear();
        entity_collision_boxes(flush, &[ghost], &mut out);
        assert!(out.is_empty());
    }
}
