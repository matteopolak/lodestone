//! [`CommandSource`] — the real command-source-stack shape, minus the server
//! handle — and the resolution of an [`EntitySelector`] against a roster of
//! players and world entities.
//!
//! # What is here and what is deliberately not
//!
//! The real command source carries a server handle, a level, and an entity.
//! None of those can enter this crate: `lodestone-server` depends on
//! neither `lodestone-ecs` nor a version crate (see [`crate::command`]'s module
//! doc for the measured cost), and the browser bundle links this crate.
//!
//! What survives that cut is exactly what a command *reads*: who is running it,
//! where they are, which way they face, which dimension, what permission level,
//! and which anchor (`feet`/`eyes`) `^`-local coordinates resolve from. That is
//! [`CommandSource`], and it is enough for `/gamemode`, `/give`, `/tp`,
//! `/summon` and — the reason the shape matters — for `/execute`, whose whole
//! job is to *rewrite* one of these and re-dispatch.
//!
//! # Selector resolution lives here, not in `lodestone-command-mc`
//!
//! The split mirrors the real one: a selector parser produces the AST, and
//! resolving it against a command source is a separate step. Resolution
//! needs a roster and a caller position, which the grammar crate must not know
//! about — and keeping the AST inert is what lets [`resolve_subjects`] be tested
//! against a hand-built roster with no world at all.

use lodestone_command_mc::{EntitySelector, SelectorOrder, SelectorPredicate};
use lodestone_model::{GameMode, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

/// Which point on an entity a `^`-local coordinate or a `facing` resolves
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntityAnchor {
    /// `feet` — the entity's own position. The real default.
    #[default]
    Feet,
    /// `eyes` — the position plus the eye height.
    Eyes,
}

/// The entity a command is running as, when there is one.
///
/// `None` on [`CommandSource`] is the console (RCON): a command source with a
/// position and a permission level but no body, which is why `/gamemode
/// creative` with no target fails for RCON exactly as it does for the real
/// command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntity {
    /// The profile uuid — the outbox key, and the identity the wire echoed at
    /// login.
    pub uuid: Uuid,
    /// The network entity id, for a directive that addresses the entity itself.
    pub entity_id: i32,
    /// The login name.
    pub username: String,
}

/// Everything a built-in command knows about who is running it.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandSource {
    /// The entity, or `None` for the console.
    pub entity: Option<SourceEntity>,
    /// The display name used in feedback — a username, or `"Rcon"`.
    pub name: String,
    pub position: Vec3,
    pub rotation: Rotation,
    pub dimension: ResourceKey,
    /// 0–4, matching 26.2's `HasCommandLevel` and
    /// [`crate::AccessLists::permission_level`]. RCON is 4.
    pub permission_level: u8,
    pub anchor: EntityAnchor,
}

impl CommandSource {
    /// A source with no entity — the console.
    #[must_use]
    pub fn console(name: impl Into<String>, dimension: ResourceKey, permission_level: u8) -> Self {
        Self {
            entity: None,
            name: name.into(),
            position: Vec3::new(0.0, 0.0, 0.0),
            rotation: Rotation { yaw: 0.0, pitch: 0.0 },
            dimension,
            permission_level,
            anchor: EntityAnchor::Feet,
        }
    }

    /// A source that *is* a player.
    #[must_use]
    pub fn player(
        uuid: Uuid,
        entity_id: i32,
        username: impl Into<String>,
        position: Vec3,
        rotation: Rotation,
        dimension: ResourceKey,
        permission_level: u8,
    ) -> Self {
        let username = username.into();
        Self {
            entity: Some(SourceEntity { uuid, entity_id, username: username.clone() }),
            name: username,
            position,
            rotation,
            dimension,
            permission_level,
            anchor: EntityAnchor::Feet,
        }
    }

    /// This source's uuid, if it has a body.
    #[must_use]
    pub fn uuid(&self) -> Option<Uuid> {
        self.entity.as_ref().map(|e| e.uuid)
    }

    /// Whether this source holds at least `level`.
    #[must_use]
    pub fn has_level(&self, level: u8) -> bool {
        self.permission_level >= level
    }
}

/// One player the resolver may return.
///
/// A flattened view of the registry rather than a borrow of it, so resolution
/// happens outside the registry's lock — the same reason
/// [`lodestone_command::SuggestionProvider`]'s doc gives for snapshotting names.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerCandidate {
    pub uuid: Uuid,
    pub entity_id: i32,
    pub username: String,
    pub position: Vec3,
    /// Body/head rotation, as `crate::players::PlayerRegistry` last recorded
    /// it — the field `/execute at`'s rotation transfer and `rotated as`
    /// (`crate::commands::execute`) both need and, until now, had nowhere to
    /// read from.
    pub rotation: Rotation,
    pub game_mode: GameMode,
    /// The player's experience level, the same "republished on every mutation"
    /// mirror `game_mode` already is — see [`crate::players::PlayerRegistry::set_experience`].
    pub xp_level: i32,
    /// Points *within the current level* — `floor(experienceProgress *
    /// xp_needed_for_next_level)`, the real `/xp query … points` query
    /// formula, **not** the lifetime total
    /// [`crate::experience::PlayerExperience::total`] tracks. `/xp query …
    /// points` is the only reader.
    pub xp_points: i32,
    /// The player's full save-shaped NBT (inventory, health, ...), when the
    /// connection that owns it supplied one. `/data get entity` and `nbt=` read
    /// it; absent, they fall back to a view built from the fields above.
    pub nbt: Option<std::sync::Arc<lodestone_core::Nbt>>,
}

/// What a selector needs to know about an entity to match it.
pub trait SelectorSubject {
    /// The persistent identity.
    fn uuid(&self) -> Uuid;
    /// The plain-text name `name=` compares against: a player's login name, or
    /// the empty string for an entity without a custom name.
    fn name(&self) -> &str;
    /// The canonical entity-type id, e.g. `minecraft:player`.
    fn entity_type(&self) -> &str;
    /// The entity's position.
    fn position(&self) -> Vec3;
    /// Body rotation, for `x_rotation=` / `y_rotation=`.
    fn rotation(&self) -> Rotation;
    /// The game mode, for a player; `None` for anything else.
    fn game_mode(&self) -> Option<GameMode>;
    /// The scoreboard holder name: a player's login name, an entity's uuid text.
    fn score_holder(&self) -> String;
}

impl SelectorSubject for PlayerCandidate {
    fn uuid(&self) -> Uuid {
        self.uuid
    }
    fn name(&self) -> &str {
        &self.username
    }
    fn entity_type(&self) -> &str {
        lodestone_command_mc::entity::PLAYER_TYPE
    }
    fn position(&self) -> Vec3 {
        self.position
    }
    fn rotation(&self) -> Rotation {
        self.rotation
    }
    fn game_mode(&self) -> Option<GameMode> {
        Some(self.game_mode)
    }
    fn score_holder(&self) -> String {
        self.username.clone()
    }
}

/// A non-player entity the selectors can match, snapshotted from the mob
/// simulation when a command runs.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldEntityCandidate {
    pub uuid: Uuid,
    pub entity_id: i32,
    /// The canonical entity-type id, e.g. `minecraft:item_frame`.
    pub entity_type: String,
    pub position: Vec3,
    pub rotation: Rotation,
    /// The custom name as plain text, `""` when unnamed.
    pub name: String,
    /// The dimension the entity is in (the one the mob simulation serves).
    pub dimension: ResourceKey,
}

impl SelectorSubject for WorldEntityCandidate {
    fn uuid(&self) -> Uuid {
        self.uuid
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn entity_type(&self) -> &str {
        &self.entity_type
    }
    fn position(&self) -> Vec3 {
        self.position
    }
    fn rotation(&self) -> Rotation {
        self.rotation
    }
    fn game_mode(&self) -> Option<GameMode> {
        None
    }
    fn score_holder(&self) -> String {
        self.uuid.to_string()
    }
}

/// Anything a selector can resolve to.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandTarget {
    Player(PlayerCandidate),
    World(WorldEntityCandidate),
}

impl CommandTarget {
    /// The network entity id.
    #[must_use]
    pub fn entity_id(&self) -> i32 {
        match self {
            Self::Player(p) => p.entity_id,
            Self::World(w) => w.entity_id,
        }
    }

    /// The name feedback uses: a player's login name, an entity's custom name,
    /// or its type id when unnamed.
    #[must_use]
    pub fn display_name(&self) -> String {
        match self {
            Self::Player(p) => p.username.clone(),
            Self::World(w) if w.name.is_empty() => w.entity_type.clone(),
            Self::World(w) => w.name.clone(),
        }
    }

    /// The entity as a [`SourceEntity`], for `execute as`.
    #[must_use]
    pub fn source_entity(&self) -> SourceEntity {
        SourceEntity { uuid: self.uuid(), entity_id: self.entity_id(), username: self.display_name() }
    }

    /// The player behind this target, if it is one.
    #[must_use]
    pub fn as_player(&self) -> Option<&PlayerCandidate> {
        match self {
            Self::Player(p) => Some(p),
            Self::World(_) => None,
        }
    }
}

impl SelectorSubject for CommandTarget {
    fn uuid(&self) -> Uuid {
        match self {
            Self::Player(p) => p.uuid(),
            Self::World(w) => w.uuid(),
        }
    }
    fn name(&self) -> &str {
        match self {
            Self::Player(p) => SelectorSubject::name(p),
            Self::World(w) => w.name(),
        }
    }
    fn entity_type(&self) -> &str {
        match self {
            Self::Player(p) => p.entity_type(),
            Self::World(w) => w.entity_type(),
        }
    }
    fn position(&self) -> Vec3 {
        match self {
            Self::Player(p) => SelectorSubject::position(p),
            Self::World(w) => w.position(),
        }
    }
    fn rotation(&self) -> Rotation {
        match self {
            Self::Player(p) => p.rotation,
            Self::World(w) => w.rotation,
        }
    }
    fn game_mode(&self) -> Option<GameMode> {
        match self {
            Self::Player(p) => p.game_mode(),
            Self::World(_) => None,
        }
    }
    fn score_holder(&self) -> String {
        match self {
            Self::Player(p) => p.score_holder(),
            Self::World(w) => w.score_holder(),
        }
    }
}

/// Why a selector matched nothing, or could not be resolved.
///
/// Distinguished because the real command distinguishes them: "no players
/// found" is a different message from "you must be a player", and a command
/// that reported "no players found" to the console would be actively
/// misleading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorError {
    /// `@s` (or an implicit self target) from a source with no entity.
    NotAnEntity,
    /// The selector is well-formed and matched nobody.
    NoPlayersFound,
    /// The selector needs a feature this server does not have yet.
    Unsupported(&'static str),
}

impl std::fmt::Display for SelectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnEntity => f.write_str("That command can only be used by a player"),
            Self::NoPlayersFound => f.write_str("No player was found"),
            Self::Unsupported(what) => write!(f, "{what} is not supported yet"),
        }
    }
}

/// The real selector-resolution rule — the AST plus candidates and a caller —
/// over any [`SelectorSubject`]: players alone, or players with the world's
/// entities when the selector reaches entities.
///
/// The order of operations is the real rule's and each step matters:
///
/// 1. `@s` short-circuits to the caller, still subject to the predicates —
///    `@s[gamemode=creative]` legitimately matches nobody.
/// 2. A bare name or uuid is an exact lookup, not a filter.
/// 3. Otherwise: every candidate, filtered by the predicates, then by
///    `distance`/`dx dy dz` around the (possibly `x`/`y`/`z`-overridden) origin,
///    then **sorted**, then truncated to `max_results`.
///
/// Sorting before truncating is the whole point of `sort=nearest,limit=3`: the
/// other order gives three arbitrary candidates sorted among themselves.
pub fn resolve_subjects<T: SelectorSubject + Clone>(
    selector: &EntitySelector,
    source: &CommandSource,
    candidates: &[T],
    // A deterministic index permutation for `sort=random`, supplied by the
    // caller. Passed in rather than generated here because this crate is
    // synchronous and wasm-safe and must not reach for a clock, and because a
    // test needs `@r` to be reproducible.
    shuffle: &dyn Fn(usize) -> Vec<usize>,
    // `scores=` resolution: `(holder, objective) -> score`, `None` for either
    // an unknown objective or a holder with no score on a known one — both
    // read as "the predicate fails", matching the real `scores=` resolution.
    // A closure rather than a `&ScoreboardHandle` because this crate must stay
    // ignorant of the scoreboard's storage shape (the module doc's "grammar
    // here, resolution there" split), and so a test can supply a hand-built
    // table with no scoreboard at all.
    score: &dyn Fn(&str, &str) -> Option<i32>,
    // `team=` resolution: `holder -> team name`, `""` for a holder on no team
    // — see `lodestone_command_mc::SelectorPredicate::Team`'s own doc for why
    // there is no `Option` here. Same closure-over-handle shape as `score`,
    // for the identical reason: this crate must stay ignorant of
    // `crate::commands::team_store`'s storage shape.
    team: &dyn Fn(&str) -> String,
    // `tag=` resolution: the tags on an entity, by uuid.
    tags: &dyn Fn(Uuid) -> Vec<String>,
    // `nbt=` resolution: whether the entity's NBT contains the compound.
    nbt: &dyn Fn(Uuid, &[(String, lodestone_command_mc::SnbtValue)]) -> bool,
) -> Result<Vec<T>, SelectorError> {
    if selector.current_entity {
        let Some(entity) = source.entity.as_ref() else {
            return Err(SelectorError::NotAnEntity);
        };
        let Some(me) = candidates.iter().find(|c| c.uuid() == entity.uuid) else {
            return Err(SelectorError::NoPlayersFound);
        };
        return if matches_predicates(me, selector, score, team, tags, nbt) {
            Ok(vec![me.clone()])
        } else {
            Err(SelectorError::NoPlayersFound)
        };
    }

    if let Some(name) = selector.player_name.as_deref() {
        return candidates
            .iter()
            .find(|c| c.name() == name && c.entity_type() == lodestone_command_mc::entity::PLAYER_TYPE)
            .cloned()
            .map(|c| vec![c])
            .ok_or(SelectorError::NoPlayersFound);
    }

    if let Some(uuid) = selector.entity_uuid {
        return candidates
            .iter()
            .find(|c| c.uuid() == uuid)
            .cloned()
            .map(|c| vec![c])
            .ok_or(SelectorError::NoPlayersFound);
    }

    let origin = (
        selector.position.x.unwrap_or(source.position.x),
        selector.position.y.unwrap_or(source.position.y),
        selector.position.z.unwrap_or(source.position.z),
    );

    let mut matched: Vec<T> = candidates
        .iter()
        .filter(|c| matches_predicates(*c, selector, score, team, tags, nbt))
        .filter(|c| matches_region(*c, selector, origin))
        .cloned()
        .collect();

    match selector.order {
        SelectorOrder::Arbitrary => {}
        SelectorOrder::Nearest => matched.sort_by(|a, b| {
            distance_sqr(a, origin)
                .partial_cmp(&distance_sqr(b, origin))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        SelectorOrder::Furthest => matched.sort_by(|a, b| {
            distance_sqr(b, origin)
                .partial_cmp(&distance_sqr(a, origin))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        SelectorOrder::Random => {
            let permutation = shuffle(matched.len());
            // Guarded rather than trusted: a caller-supplied permutation that is
            // not one leaves the order alone instead of dropping or duplicating
            // players, which is what an unchecked `swap` loop would do.
            if permutation.len() == matched.len()
                && permutation.iter().all(|&i| i < matched.len())
            {
                let reordered: Vec<T> =
                    permutation.iter().map(|&i| matched[i].clone()).collect();
                matched = reordered;
            }
        }
    }

    if matched.len() > selector.max_results {
        matched.truncate(selector.max_results);
    }
    if matched.is_empty() {
        return Err(SelectorError::NoPlayersFound);
    }
    Ok(matched)
}

fn distance_sqr<T: SelectorSubject>(candidate: &T, origin: (f64, f64, f64)) -> f64 {
    let position = candidate.position();
    let (dx, dy, dz) = (position.x - origin.0, position.y - origin.1, position.z - origin.2);
    dx * dx + dy * dy + dz * dz
}

fn matches_predicates<T: SelectorSubject>(
    candidate: &T,
    selector: &EntitySelector,
    score: &dyn Fn(&str, &str) -> Option<i32>,
    team: &dyn Fn(&str) -> String,
    tags: &dyn Fn(Uuid) -> Vec<String>,
    nbt: &dyn Fn(Uuid, &[(String, lodestone_command_mc::SnbtValue)]) -> bool,
) -> bool {
    // A positive `type=` narrows to that type alone: `@e[type=cow]` cannot match
    // a player, and saying so here is what keeps `/give @e[type=cow] …` from
    // silently hitting everybody.
    if let Some(kind) = selector.limit_to_type.as_deref() {
        if kind != candidate.entity_type() {
            return false;
        }
    }
    selector.predicates.iter().all(|predicate| match predicate {
        SelectorPredicate::Name { name, inverted } => (candidate.name() == name) != *inverted,
        // Only a player has a game mode; anything else fails, inverted or not.
        SelectorPredicate::GameMode { mode, inverted } => {
            candidate.game_mode().is_some_and(|actual| (actual == *mode) != *inverted)
        }
        SelectorPredicate::EntityType { id, inverted } => (id == candidate.entity_type()) != *inverted,
        // A roster holds live players only, so the real is-alive check is always
        // satisfied.
        // Stated rather than skipped so a future roster that carries the dead
        // (which is a real possibility — a dead player stays connected on the
        // death screen) has one place to change.
        SelectorPredicate::Alive => true,
        // The real `scores=` predicate: every named objective must resolve
        // to a score for this holder, and that score must fall in range. A
        // holder with no score on a known objective, or an objective this
        // scoreboard has never seen, both come back `None` from `score` and
        // both refuse the match — the real rule does not distinguish them either.
        SelectorPredicate::Scores(entries) => entries.iter().all(|(objective, range)| {
            score(&candidate.score_holder(), objective).is_some_and(|value| range.matches(value))
        }),
        // The real `team=` predicate: compare the holder's team
        // name (`""` when on no team) against `name` directly — see
        // `SelectorPredicate::Team`'s own doc for why there is no `Option`
        // three-way to model here.
        SelectorPredicate::Team { name, inverted } => (team(&candidate.score_holder()) == *name) != *inverted,
        SelectorPredicate::Tag { tag, inverted } => {
            let held = tags(candidate.uuid());
            let has = if tag.is_empty() { held.is_empty() } else { held.iter().any(|t| t == tag) };
            has != *inverted
        }
        SelectorPredicate::EntityTypeTag { tag, inverted } => {
            let member = lodestone_data::entity_type_tags::entity_type_tag_members(tag)
                .is_some_and(|members| members.contains(&candidate.entity_type()));
            member != *inverted
        }
        SelectorPredicate::XRotation(bounds) => {
            lodestone_command_mc::entity::rotation_in_bounds(bounds, candidate.rotation().pitch)
        }
        SelectorPredicate::YRotation(bounds) => {
            lodestone_command_mc::entity::rotation_in_bounds(bounds, candidate.rotation().yaw)
        }
        SelectorPredicate::Nbt { nbt: pattern, inverted } => nbt(candidate.uuid(), pattern) != *inverted,
    })
}

/// `distance=` and `dx`/`dy`/`dz`.
///
/// The two are different shapes, not two spellings of one: `distance` is a
/// radius in blocks and `dx dy dz` is an axis-aligned **box** whose far corner
/// is `+1.0` past the delta, which is why `dx=0` still selects the caller's
/// own block column rather than nothing.
fn matches_region<T: SelectorSubject>(
    candidate: &T,
    selector: &EntitySelector,
    origin: (f64, f64, f64),
) -> bool {
    if let Some(bounds) = selector.distance {
        if !bounds.matches(distance_sqr(candidate, origin).sqrt()) {
            return false;
        }
    }
    if let Some([dx, dy, dz]) = selector.volume {
        let axis = |value: f64, base: f64, delta: f64| {
            let (low, high) = if delta < 0.0 { (delta, 0.0) } else { (0.0, delta) };
            value >= base + low && value <= base + high + 1.0
        };
        let position = candidate.position();
        if !axis(position.x, origin.0, dx) || !axis(position.y, origin.1, dy) || !axis(position.z, origin.2, dz)
        {
            return false;
        }
    }
    true
}

/// The identity permutation — `sort=random` with no shuffling, for a caller
/// (and a test) that wants determinism.
#[must_use]
#[cfg(test)]
pub fn no_shuffle(len: usize) -> Vec<usize> {
    (0..len).collect()
}

/// A real `sort=random` permutation, Fisher-Yates over `0..len` seeded from
/// `seed`. [`crate::mob_spawn::SpawnRng`] rather than `rand::thread_rng()`
/// because this crate must stay wasm-safe (`docs/browser-shell-port.md`'s
/// clock-hazard census: nothing here may reach for a wall clock), and a fresh
/// `SpawnRng` per call is exactly the "no persistent RNG state threaded
/// through the command dispatch surface" shape [`no_shuffle`]'s own doc
/// describes — the caller supplies a seed that varies per call instead.
///
/// `crate::commands::registrar::Ctx::resolve` is the one production caller,
/// seeding from `WorldTime::game_time` — see its own call site for why that
/// is "changes every real tick" rather than "changes every call", and what
/// that does and does not guarantee.
#[must_use]
pub fn seeded_shuffle(seed: u64) -> impl Fn(usize) -> Vec<usize> {
    move |len| {
        let mut permutation: Vec<usize> = (0..len).collect();
        let mut rng = crate::mob_spawn::SpawnRng::new(seed);
        // Fisher-Yates, back to front: for each index from the end, swap in a
        // uniformly-chosen element from the still-unshuffled prefix (inclusive
        // of itself), matching `java.util.Collections.shuffle`'s own algorithm.
        for i in (1..permutation.len()).rev() {
            let j = rng.next_int(i as i32 + 1) as usize;
            permutation.swap(i, j);
        }
        permutation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A permutation, not a partial or repeating draw — the property Fisher-
    /// Yates guarantees regardless of seed, and the one a broken swap range
    /// (an off-by-one on the `1..len` bound, say) would violate first.
    #[test]
    fn seeded_shuffle_always_returns_a_real_permutation() {
        for seed in [0u64, 1, 42, u64::MAX] {
            let mut permutation = seeded_shuffle(seed)(20);
            permutation.sort_unstable();
            assert_eq!(permutation, (0..20).collect::<Vec<usize>>(), "seed {seed}");
        }
    }

    /// The actual bug this replaces: `Ctx::resolve` used to pass `no_shuffle`
    /// in production, so `sort=random` was the identity permutation forever,
    /// not merely within one tick. At `len = 20`, a real shuffle landing on
    /// the identity by chance is astronomically unlikely (`1/20!`), so
    /// disagreeing with `no_shuffle` here is a real, discriminating check —
    /// not a coin flip that could pass for the wrong reason.
    #[test]
    fn seeded_shuffle_is_not_the_identity_permutation() {
        assert_ne!(seeded_shuffle(1)(20), no_shuffle(20));
    }

    /// Different seeds must draw different permutations — the property that
    /// makes `game_time`-seeding at the call site actually vary the result
    /// tick to tick, rather than every seed collapsing onto one fixed order.
    #[test]
    fn different_seeds_draw_different_permutations() {
        assert_ne!(seeded_shuffle(1)(20), seeded_shuffle(2)(20));
    }
}
