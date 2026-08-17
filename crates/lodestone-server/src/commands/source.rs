//! [`CommandSource`] — vanilla's `CommandSourceStack`, minus the server handle
//! — and the resolution of an [`EntitySelector`] against a player roster.
//!
//! # What is here and what is deliberately not
//!
//! `CommandSourceStack` carries a `MinecraftServer`, a `ServerLevel` and an
//! `Entity`. None of those can enter this crate: `lodestone-server` depends on
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
//! The split mirrors vanilla's own: `EntitySelectorParser` produces the AST,
//! `EntitySelector.findEntities(CommandSourceStack)` resolves it. Resolution
//! needs a roster and a caller position, which the grammar crate must not know
//! about — and keeping the AST inert is what lets [`resolve_players`] be tested
//! against a hand-built roster with no world at all.

use lodestone_command_mc::{EntitySelector, SelectorOrder, SelectorPredicate};
use lodestone_model::{GameMode, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

/// Which point on an entity a `^`-local coordinate or a `facing` resolves from
/// (`EntityAnchorArgument.Anchor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntityAnchor {
    /// `feet` — the entity's own position. Vanilla's default.
    #[default]
    Feet,
    /// `eyes` — the position plus the eye height.
    Eyes,
}

/// The entity a command is running as, when there is one.
///
/// `None` on [`CommandSource`] is the console (RCON): a command source with a
/// position and a permission level but no body, which is why `/gamemode
/// creative` with no target fails for RCON exactly as it does in vanilla
/// (`getPlayerOrException`).
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
    /// `Player.experienceLevel`, the same "republished on every mutation"
    /// mirror `game_mode` already is — see [`crate::players::PlayerRegistry::set_experience`].
    pub xp_level: i32,
    /// Points *within the current level* — `Mth.floor(experienceProgress *
    /// getXpNeededForNextLevel())`, `ExperienceCommand.java`'s own `POINTS`
    /// query formula, **not** the lifetime total
    /// [`crate::experience::PlayerExperience::total`] tracks. `/xp query …
    /// points` is the only reader.
    pub xp_points: i32,
}

/// Why a selector matched nothing, or could not be resolved.
///
/// Distinguished because vanilla distinguishes them: `NO_PLAYERS_FOUND`
/// ("argument.entity.notfound.player") is a different message from
/// `getPlayerOrException`'s "you must be a player", and a command that reported
/// "no players found" to the console would be actively misleading.
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

/// `EntitySelector.findPlayers` — the AST plus a roster and a caller, to a
/// player list.
///
/// The order of operations is vanilla's and each step matters:
///
/// 1. `@s` short-circuits to the caller (`currentEntity`), still subject to the
///    predicates — `@s[gamemode=creative]` legitimately matches nobody.
/// 2. A bare name or uuid is an exact lookup, not a filter.
/// 3. Otherwise: every candidate, filtered by the predicates, then by
///    `distance`/`dx dy dz` around the (possibly `x`/`y`/`z`-overridden) origin,
///    then **sorted**, then truncated to `max_results`.
///
/// Sorting before truncating is the whole point of `sort=nearest,limit=3`: the
/// other order gives three arbitrary players sorted among themselves.
pub fn resolve_players(
    selector: &EntitySelector,
    source: &CommandSource,
    candidates: &[PlayerCandidate],
    // A deterministic index permutation for `sort=random`, supplied by the
    // caller. Passed in rather than generated here because this crate is
    // synchronous and wasm-safe and must not reach for a clock, and because a
    // test needs `@r` to be reproducible.
    shuffle: &dyn Fn(usize) -> Vec<usize>,
    // `scores=` resolution: `(holder, objective) -> score`, `None` for either
    // an unknown objective or a holder with no score on a known one — both
    // read as "the predicate fails", matching `EntitySelectorOptions.SCORES`.
    // A closure rather than a `&ScoreboardHandle` because this crate must stay
    // ignorant of the scoreboard's storage shape (the module doc's "grammar
    // here, resolution there" split), and so a test can supply a hand-built
    // table with no scoreboard at all.
    score: &dyn Fn(&str, &str) -> Option<i32>,
) -> Result<Vec<PlayerCandidate>, SelectorError> {
    if selector.current_entity {
        let Some(entity) = source.entity.as_ref() else {
            return Err(SelectorError::NotAnEntity);
        };
        let Some(me) = candidates.iter().find(|c| c.uuid == entity.uuid) else {
            return Err(SelectorError::NoPlayersFound);
        };
        return if matches_predicates(me, selector, score) {
            Ok(vec![me.clone()])
        } else {
            Err(SelectorError::NoPlayersFound)
        };
    }

    if let Some(name) = selector.player_name.as_deref() {
        return candidates
            .iter()
            .find(|c| c.username == name)
            .cloned()
            .map(|c| vec![c])
            .ok_or(SelectorError::NoPlayersFound);
    }

    if let Some(uuid) = selector.entity_uuid {
        return candidates
            .iter()
            .find(|c| c.uuid == uuid)
            .cloned()
            .map(|c| vec![c])
            .ok_or(SelectorError::NoPlayersFound);
    }

    let origin = (
        selector.position.x.unwrap_or(source.position.x),
        selector.position.y.unwrap_or(source.position.y),
        selector.position.z.unwrap_or(source.position.z),
    );

    let mut matched: Vec<PlayerCandidate> = candidates
        .iter()
        .filter(|c| matches_predicates(c, selector, score))
        .filter(|c| matches_region(c, selector, origin))
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
                let reordered: Vec<PlayerCandidate> =
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

fn distance_sqr(candidate: &PlayerCandidate, origin: (f64, f64, f64)) -> f64 {
    let (dx, dy, dz) = (
        candidate.position.x - origin.0,
        candidate.position.y - origin.1,
        candidate.position.z - origin.2,
    );
    dx * dx + dy * dy + dz * dz
}

fn matches_predicates(
    candidate: &PlayerCandidate,
    selector: &EntitySelector,
    score: &dyn Fn(&str, &str) -> Option<i32>,
) -> bool {
    // `limit_to_type` is `minecraft:player` for every player-shaped selector,
    // and a roster only holds players, so it is satisfied by construction. A
    // *different* positive type narrows to nothing — `@e[type=cow]` cannot match
    // a player — and saying so here is what keeps `/give @e[type=cow] …` from
    // silently hitting everybody.
    if let Some(kind) = selector.limit_to_type.as_deref() {
        if kind != lodestone_command_mc::entity::PLAYER_TYPE {
            return false;
        }
    }
    selector.predicates.iter().all(|predicate| match predicate {
        SelectorPredicate::Name { name, inverted } => (&candidate.username == name) != *inverted,
        SelectorPredicate::GameMode { mode, inverted } => (candidate.game_mode == *mode) != *inverted,
        SelectorPredicate::EntityType { id, inverted } => {
            (id == lodestone_command_mc::entity::PLAYER_TYPE) != *inverted
        }
        // A roster holds live players only, so `isAlive` is always satisfied.
        // Stated rather than skipped so a future roster that carries the dead
        // (which is a real possibility — a dead player stays connected on the
        // death screen) has one place to change.
        SelectorPredicate::Alive => true,
        // `EntitySelectorOptions.SCORES`: every named objective must resolve
        // to a score for this holder, and that score must fall in range. A
        // holder with no score on a known objective, or an objective this
        // scoreboard has never seen, both come back `None` from `score` and
        // both refuse the match — vanilla does not distinguish them either.
        SelectorPredicate::Scores(entries) => entries.iter().all(|(objective, range)| {
            score(&candidate.username, objective).is_some_and(|value| range.matches(value))
        }),
    })
}

/// `distance=` and `dx`/`dy`/`dz`.
///
/// The two are different shapes, not two spellings of one: `distance` is a
/// radius in blocks and `dx dy dz` is an axis-aligned **box** whose far corner is
/// `+1.0` past the delta (`EntitySelectorParser.createAabb`), which is why
/// `dx=0` still selects the caller's own block column rather than nothing.
fn matches_region(
    candidate: &PlayerCandidate,
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
        if !axis(candidate.position.x, origin.0, dx)
            || !axis(candidate.position.y, origin.1, dy)
            || !axis(candidate.position.z, origin.2, dz)
        {
            return false;
        }
    }
    true
}

/// The identity permutation — `sort=random` with no shuffling, for a caller
/// (and a test) that wants determinism.
#[must_use]
pub fn no_shuffle(len: usize) -> Vec<usize> {
    (0..len).collect()
}

/// No scoreboard at all — every `scores=` lookup misses, matching an unknown
/// objective. For a caller (or a test) with no scores to offer.
#[must_use]
pub fn no_scores(_holder: &str, _objective: &str) -> Option<i32> {
    None
}
