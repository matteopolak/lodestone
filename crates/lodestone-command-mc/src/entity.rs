//! `minecraft:entity` — the `@p`/`@a`/`@r`/`@s`/`@e`/`@n` selector grammar, its
//! AST, and the argument type that produces one.
//!
//! # The split, restated because it is the design
//!
//! Vanilla has `EntitySelectorParser` (text → `EntitySelector`) and
//! `EntitySelector.findEntities(CommandSourceStack)` (AST + world → entities).
//! This module is the first half only. [`EntitySelector`] is plain data with no
//! world access, and the server resolves it — see
//! `lodestone_server::commands::source` for the resolver.
//!
//! # What v1 parses
//!
//! `@p @a @r @s @e @n`, a bare player name, a bare uuid, and the options
//! `type`, `name`, `distance`, `limit`, `sort`, `gamemode`, `x`/`y`/`z`,
//! `dx`/`dy`/`dz`, `scores`, each with vanilla's `!` inversion where vanilla
//! has it.
//!
//! `nbt`, `advancements`, `predicate`, `tag`, `team`, `level` and the two
//! `*_rotation` options are **not** parsed, and each is refused by name rather
//! than silently ignored — a selector that quietly matched more players than
//! the author asked for is the worst available failure. Every one of them
//! needs infrastructure this server does not have (entity NBT, an advancement
//! predicate engine, teams).
//!
//! **None of that is visible on the wire.** `minecraft:entity`'s network
//! payload is one flags byte — `single` and `players_only` — with no option
//! list, so the transmitted node for `@a[distance=..8]` and for a selector this
//! crate cannot parse at all are byte-identical. Deferring options therefore
//! cannot break tree parity, and cannot make the client autocomplete something
//! we reject *at the node level*; it can only produce a parse error on a value
//! the client had no opinion about anyway.

use std::fmt;

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::GameMode;
use lodestone_model::command_tree::ArgumentParser;
use uuid::Uuid;

use crate::McArg;

/// `MinMaxBounds`: an inclusive range with either end optional.
///
/// `5` parses as `min == max == 5`; `1..3` as both; `1..` and `..3` as one.
/// Both ends absent is an error (`MinMaxBounds.ERROR_EMPTY`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Bounds<T> {
    pub min: Option<T>,
    pub max: Option<T>,
}

impl<T: PartialOrd + Copy> Bounds<T> {
    /// Whether `value` is within both present ends.
    #[must_use]
    pub fn matches(&self, value: T) -> bool {
        if let Some(min) = self.min {
            if value < min {
                return false;
            }
        }
        if let Some(max) = self.max {
            if value > max {
                return false;
            }
        }
        true
    }
}

/// `MinMaxBounds.Bounds.fromReader` for `f64`.
///
/// The one subtle rule is `isAllowedInputChar`: a `.` is part of the number
/// **unless** it begins a `..`, which is what lets `1..5` tokenize without
/// backtracking and `1.5..` still read `1.5`.
fn read_bounds_f64(reader: &mut StringReader) -> Result<Bounds<f64>, ParseError> {
    let start = reader.cursor();
    if !reader.can_read() {
        return Err(ParseError::new(start, ParseErrorKind::ExpectedDouble));
    }
    let min = read_bounds_number(reader);
    let max = if reader.can_read_n(2) && reader.peek() == Some('.') && peek_at(reader, 1) == Some('.')
    {
        reader.skip();
        reader.skip();
        read_bounds_number(reader)
    } else {
        min.clone()
    };
    if min.is_none() && max.is_none() {
        reader.set_cursor(start);
        return Err(ParseError::new(start, ParseErrorKind::ExpectedDouble));
    }
    let parse = |text: Option<String>| -> Result<Option<f64>, ParseError> {
        match text {
            None => Ok(None),
            Some(text) => text
                .parse::<f64>()
                .map(Some)
                .map_err(|_| ParseError::new(start, ParseErrorKind::InvalidDouble(text))),
        }
    };
    let bounds = Bounds { min: parse(min)?, max: parse(max)? };
    // `MinMaxBounds.Doubles.fromReader`'s `areSwapped` check.
    if let (Some(min), Some(max)) = (bounds.min, bounds.max) {
        if min > max {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::DoubleTooLow { found: max, min }));
        }
    }
    Ok(bounds)
}

/// One end of a bounds expression: the run of `[0-9-]` plus any `.` that is not
/// the first character of a `..`.
fn read_bounds_number(reader: &mut StringReader) -> Option<String> {
    let start = reader.cursor();
    while reader.can_read() {
        match reader.peek() {
            Some(c) if c.is_ascii_digit() || c == '-' => reader.skip(),
            Some('.') if peek_at(reader, 1) != Some('.') => reader.skip(),
            _ => break,
        }
    }
    if reader.cursor() == start {
        return None;
    }
    let source = reader.source();
    Some(source.chars().skip(start).take(reader.cursor() - start).collect())
}

/// `StringReader.peek(offset)`, which `lodestone-command`'s reader does not
/// expose. Implemented by cursor arithmetic rather than by widening that crate's
/// API: this is the only caller, and `peek_at` is exactly the shape a future
/// `peek_n` would take if a second one appears.
fn peek_at(reader: &StringReader, offset: usize) -> Option<char> {
    let index = reader.cursor() + offset;
    reader.source().chars().nth(index)
}

/// The order a selector's candidates are put in before `limit` truncates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectorOrder {
    /// `EntitySelector.ORDER_ARBITRARY` — insertion order, which for this
    /// server is the player registry's own order.
    #[default]
    Arbitrary,
    /// `EntitySelectorParser.ORDER_NEAREST`.
    Nearest,
    /// `EntitySelectorParser.ORDER_FURTHEST`.
    Furthest,
    /// `EntitySelectorParser.ORDER_RANDOM`.
    Random,
}

/// A per-candidate predicate the *resolver* applies.
///
/// Deliberately data rather than a boxed closure (which is what vanilla builds):
/// a closure would need an entity type from a crate this one must not depend on,
/// and an AST that is `PartialEq` is what makes a grammar test able to state its
/// expected value.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectorPredicate {
    /// `name=` / `name=!` — exact match against the entity's plain-text name.
    Name { name: String, inverted: bool },
    /// `type=` / `type=!` — a canonical `minecraft:*` entity-type id, validated
    /// against `lodestone_data::entity_types` at parse time.
    EntityType { id: String, inverted: bool },
    /// `gamemode=` / `gamemode=!` — matches players only; a non-player
    /// candidate always fails, inverted or not (`EntitySelectorOptions`'s
    /// `instanceof ServerPlayer` guard returns `false`, it does not negate).
    GameMode { mode: GameMode, inverted: bool },
    /// The entity must be alive. Added implicitly by `@e` and `@n`, exactly as
    /// `parseSelector`'s `yield true` branches do.
    Alive,
    /// `scores={obj1=1..5,obj2=10}` — every named objective must have a score
    /// recorded for this holder, within its range. `EntitySelectorOptions.
    /// SCORES`'s own predicate returns `false`, not "skip", for either an
    /// unknown objective or a holder with no score on a known one — both are
    /// modelled the same way here: the lookup returning `None`.
    Scores(Vec<(String, crate::scoreboard::IntRange)>),
}

/// The `x`/`y`/`z` overrides: which components of the caller's position the
/// selector replaces. Absent components keep the caller's own.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SelectorPosition {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
}

/// A parsed entity selector — vanilla's `EntitySelector` record, minus the two
/// fields that are closures over world state.
#[derive(Debug, Clone, PartialEq)]
pub struct EntitySelector {
    /// `maxResults`. `usize::MAX` means unbounded (`@a`, `@e`).
    pub max_results: usize,
    /// `includesEntities`: `false` restricts the result to players. `@a`, `@p`
    /// and `@r` set it false by construction; `gamemode=` and `level=` also
    /// force it false.
    pub includes_entities: bool,
    /// `worldLimited` — set by any positional option. Carried for parity; this
    /// server has one dimension of players, so nothing reads it yet.
    pub world_limited: bool,
    /// `currentEntity` — `@s`. The caller itself, subject to the predicates.
    pub current_entity: bool,
    /// A bare player name rather than a selector (`/gamemode creative Steve`).
    pub player_name: Option<String>,
    /// A bare uuid rather than a selector.
    pub entity_uuid: Option<Uuid>,
    pub order: SelectorOrder,
    pub predicates: Vec<SelectorPredicate>,
    /// `distance=`, in blocks from the (possibly overridden) origin.
    pub distance: Option<Bounds<f64>>,
    pub position: SelectorPosition,
    /// `dx`/`dy`/`dz` — a box volume rather than a radius. Absent components
    /// are `0.0` once any one is present, matching `createAabb`'s own
    /// `deltaX == null ? 0.0 : deltaX`.
    pub volume: Option<[f64; 3]>,
    /// Whether an `@`-selector was used at all, as opposed to a bare name or
    /// uuid. Vanilla gates this on a permission; see [`EntityArg`].
    pub uses_selectors: bool,
    /// The single entity type `@a`/`@p`/`@r` (or a positive `type=`) narrowed
    /// to, for the resolver's fast path.
    pub limit_to_type: Option<String>,
}

impl Default for EntitySelector {
    fn default() -> Self {
        Self {
            max_results: 0,
            includes_entities: false,
            world_limited: false,
            current_entity: false,
            player_name: None,
            entity_uuid: None,
            order: SelectorOrder::Arbitrary,
            predicates: Vec::new(),
            distance: None,
            position: SelectorPosition::default(),
            volume: None,
            uses_selectors: false,
            limit_to_type: None,
        }
    }
}

impl EntitySelector {
    /// Whether this selector can ever match more than one candidate.
    #[must_use]
    pub fn is_single(&self) -> bool {
        self.max_results == 1
    }
}

/// `minecraft:player` — the type `@a`/`@p`/`@r` narrow to
/// (`parseSelector`'s `limitToType(EntityTypes.PLAYER)`).
pub const PLAYER_TYPE: &str = "minecraft:player";

/// `EntityArgument` — `entity()`, `entities()`, `player()`, `players()`.
///
/// The two booleans are exactly the two bits `EntityArgument.Info` puts on the
/// wire, and they are also the two constraints this parser enforces, which is
/// the point of [`McArg`]: `players()` sends `players_only` **and** rejects
/// `@e`, from one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityArg {
    /// At most one result (`entity()`, `player()`).
    pub single: bool,
    /// Players only (`player()`, `players()`).
    pub players_only: bool,
}

impl EntityArg {
    /// `EntityArgument.entity()` — one entity.
    #[must_use]
    pub const fn entity() -> Self {
        Self { single: true, players_only: false }
    }

    /// `EntityArgument.entities()` — any number of entities.
    #[must_use]
    pub const fn entities() -> Self {
        Self { single: false, players_only: false }
    }

    /// `EntityArgument.player()` — one player.
    #[must_use]
    pub const fn player() -> Self {
        Self { single: true, players_only: true }
    }

    /// `EntityArgument.players()` — any number of players. What `/gamemode`'s
    /// `<target>` and `/give`'s `<targets>` both use.
    #[must_use]
    pub const fn players() -> Self {
        Self { single: false, players_only: true }
    }
}

impl ArgumentType for EntityArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        match parse_selector(reader, *self) {
            Ok(selector) => Ok(ParsedValue::dynamic(selector)),
            Err(e) => {
                reader.set_cursor(start);
                Err(e)
            }
        }
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        // `fillSelectorSuggestions`' own order (`EntitySelectorParser.java`),
        // narrowed by `players_only` the way `EntityArgument.listSuggestions`
        // narrows it. `CommandTree::suggest` applies the prefix filter, so these
        // are offered unfiltered exactly as vanilla's builder does.
        //
        // Live player *names* are not here and cannot be: this type has no
        // access to a roster. The server adds them; see
        // `lodestone_server::commands::ServerCommands::suggest_with_players`.
        let mut out = vec!["@p".to_string(), "@a".to_string(), "@r".to_string(), "@s".to_string()];
        if !self.players_only {
            out.push("@e".to_string());
            out.push("@n".to_string());
        }
        out
    }
}

impl McArg for EntityArg {
    type Value = EntitySelector;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Entity { single: self.single, players_only: self.players_only }
    }
}

/// The error kind used for every selector-grammar refusal.
///
/// `ParseErrorKind::InvalidBool` carries the offending text and renders as
/// "invalid …, expected …". Reusing it keeps this crate from minting its own
/// error dialect: `lodestone-command`'s `ParseErrorKind` is deliberately
/// Brigadier's own set plus the one addition permissions forced, and a new
/// variant per Minecraft argument type would end that.
fn refuse(position: usize, message: impl Into<String>) -> ParseError {
    ParseError::new(position, ParseErrorKind::InvalidBool(message.into()))
}

pub(crate) fn parse_selector(reader: &mut StringReader, arg: EntityArg) -> Result<EntitySelector, ParseError> {
    if reader.peek() == Some('@') {
        reader.skip();
        let mut selector = parse_at_selector(reader)?;
        selector.uses_selectors = true;
        if reader.peek() == Some('[') {
            reader.skip();
            parse_options(reader, &mut selector)?;
        }
        enforce(reader, &selector, arg)?;
        Ok(selector)
    } else {
        let selector = parse_name_or_uuid(reader)?;
        enforce(reader, &selector, arg)?;
        Ok(selector)
    }
}

/// `EntitySelectorParser.parseSelector`'s switch, including which kinds add the
/// implicit `Entity::isAlive` predicate (`@e` and `@n` only — the player kinds
/// deliberately do not, because a dead player is still in the roster).
fn parse_at_selector(reader: &mut StringReader) -> Result<EntitySelector, ParseError> {
    let position = reader.cursor();
    let Some(kind) = reader.read() else {
        return Err(refuse(position, "@"));
    };
    let mut selector = EntitySelector::default();
    let alive = match kind {
        'a' => {
            selector.max_results = usize::MAX;
            selector.includes_entities = false;
            selector.limit_to_type = Some(PLAYER_TYPE.to_string());
            false
        }
        'e' => {
            selector.max_results = usize::MAX;
            selector.includes_entities = true;
            true
        }
        'n' => {
            selector.max_results = 1;
            selector.includes_entities = true;
            selector.order = SelectorOrder::Nearest;
            true
        }
        'p' => {
            selector.max_results = 1;
            selector.includes_entities = false;
            selector.order = SelectorOrder::Nearest;
            selector.limit_to_type = Some(PLAYER_TYPE.to_string());
            false
        }
        'r' => {
            selector.max_results = 1;
            selector.includes_entities = false;
            selector.order = SelectorOrder::Random;
            selector.limit_to_type = Some(PLAYER_TYPE.to_string());
            false
        }
        's' => {
            selector.max_results = 1;
            selector.includes_entities = true;
            selector.current_entity = true;
            false
        }
        other => {
            reader.set_cursor(position);
            return Err(refuse(position, format!("@{other}")));
        }
    };
    if alive {
        selector.predicates.push(SelectorPredicate::Alive);
    }
    Ok(selector)
}

/// `EntitySelectorParser.parseNameOrUUID`.
///
/// A uuid-shaped token is a uuid and includes non-player entities; anything else
/// is a player name, which must be 1..=16 characters. `max_results` is 1 either
/// way.
fn parse_name_or_uuid(reader: &mut StringReader) -> Result<EntitySelector, ParseError> {
    let position = reader.cursor();
    let text = reader.read_string()?;
    let mut selector = EntitySelector { max_results: 1, ..EntitySelector::default() };
    if let Ok(uuid) = text.parse::<Uuid>() {
        selector.entity_uuid = Some(uuid);
        selector.includes_entities = true;
    } else {
        if text.is_empty() || text.chars().count() > 16 {
            reader.set_cursor(position);
            return Err(refuse(position, text));
        }
        selector.includes_entities = false;
        selector.player_name = Some(text);
    }
    Ok(selector)
}

/// The `single`/`players_only` constraints the argument type declares, applied
/// after the whole selector is known.
///
/// This is `EntityArgument.parse`'s own post-check
/// (`EntityArgument.java:106-124`), and it has two details worth stating.
///
/// **It runs after the options.** `@e[limit=1]` *is* a legal single-entity
/// selector, and a check placed in `parse_at_selector`'s switch would reject it.
///
/// **`@s` is exempt from `players_only`.** The vanilla condition is
/// `includesEntities() && playersOnly && !isSelfSelector()`. `@s` sets
/// `includesEntities = true` (the caller might not be a player), so without the
/// exemption `/gamemode creative @s` — a `players()` argument — is refused. That
/// is exactly what the first run of `the_six_selector_kinds…` caught.
fn enforce(
    reader: &StringReader,
    selector: &EntitySelector,
    arg: EntityArg,
) -> Result<(), ParseError> {
    let position = reader.cursor();
    if arg.single && selector.max_results != 1 {
        return Err(refuse(position, "selector must match at most one entity"));
    }
    if arg.players_only && selector.includes_entities && !selector.current_entity {
        return Err(refuse(position, "selector must match only players"));
    }
    Ok(())
}

/// `EntitySelectorParser.parseOptions` — the `[a=b,c=d]` loop.
fn parse_options(reader: &mut StringReader, selector: &mut EntitySelector) -> Result<(), ParseError> {
    skip_whitespace(reader);
    while reader.can_read() && reader.peek() != Some(']') {
        skip_whitespace(reader);
        let key_position = reader.cursor();
        let key = reader.read_string()?;
        skip_whitespace(reader);
        if reader.peek() != Some('=') {
            reader.set_cursor(key_position);
            return Err(refuse(key_position, format!("selector option '{key}' has no value")));
        }
        reader.skip();
        skip_whitespace(reader);
        parse_option(reader, selector, &key, key_position)?;
        skip_whitespace(reader);
        if reader.can_read() {
            if reader.peek() != Some(',') {
                if reader.peek() != Some(']') {
                    return Err(refuse(reader.cursor(), "expected end of selector options"));
                }
                break;
            }
            reader.skip();
        }
    }
    if reader.can_read() {
        reader.skip();
        Ok(())
    } else {
        Err(refuse(reader.cursor(), "unterminated selector options"))
    }
}

/// The v1 option set. An unknown key and a *known but deferred* key are
/// deliberately different errors — the second tells the author the option
/// exists and this server cannot honour it yet, which is the difference between
/// a typo and a missing feature.
fn parse_option(
    reader: &mut StringReader,
    selector: &mut EntitySelector,
    key: &str,
    key_position: usize,
) -> Result<(), ParseError> {
    match key {
        "name" => {
            let inverted = read_inversion(reader);
            let name = reader.read_string()?;
            selector.predicates.push(SelectorPredicate::Name { name, inverted });
        }
        "distance" => {
            let position = reader.cursor();
            let bounds = read_bounds_f64(reader)?;
            if bounds.min.is_some_and(|v| v < 0.0) || bounds.max.is_some_and(|v| v < 0.0) {
                return Err(refuse(position, "distance cannot be negative"));
            }
            selector.distance = Some(bounds);
            selector.world_limited = true;
        }
        "limit" => {
            let position = reader.cursor();
            // `@s` refuses `limit` outright (`!s.isCurrentEntity()`), rather
            // than accepting a redundant `limit=1`.
            if selector.current_entity {
                return Err(refuse(position, "limit is not applicable to @s"));
            }
            let count = reader.read_int()?;
            if count < 1 {
                return Err(refuse(position, "limit must be at least 1"));
            }
            selector.max_results = count as usize;
        }
        "sort" => {
            let position = reader.cursor();
            if selector.current_entity {
                return Err(refuse(position, "sort is not applicable to @s"));
            }
            let name = reader.read_unquoted_string();
            selector.order = match name.as_str() {
                "nearest" => SelectorOrder::Nearest,
                "furthest" => SelectorOrder::Furthest,
                "random" => SelectorOrder::Random,
                "arbitrary" => SelectorOrder::Arbitrary,
                _ => return Err(refuse(position, name)),
            };
        }
        "gamemode" => {
            let position = reader.cursor();
            let inverted = read_inversion(reader);
            let name = reader.read_unquoted_string();
            let Some((_, mode)) = crate::game_mode::GAME_MODE_NAMES
                .iter()
                .find(|(candidate, _)| *candidate == name)
            else {
                return Err(refuse(position, name));
            };
            selector.includes_entities = false;
            selector.predicates.push(SelectorPredicate::GameMode { mode: *mode, inverted });
        }
        "type" => {
            let position = reader.cursor();
            let inverted = read_inversion(reader);
            if reader.peek() == Some('#') {
                return Err(refuse(position, "entity-type tags are not supported yet"));
            }
            let id = read_resource_key(reader, position)?;
            if lodestone_data::entity_types::entity_type_id(&id).is_none() {
                return Err(refuse(position, id));
            }
            if id == PLAYER_TYPE && !inverted {
                selector.includes_entities = false;
            }
            if !inverted {
                selector.limit_to_type = Some(id.clone());
            }
            selector.predicates.push(SelectorPredicate::EntityType { id, inverted });
        }
        "x" | "y" | "z" => {
            let value = reader.read_double()?;
            selector.world_limited = true;
            match key {
                "x" => selector.position.x = Some(value),
                "y" => selector.position.y = Some(value),
                _ => selector.position.z = Some(value),
            }
        }
        "dx" | "dy" | "dz" => {
            let value = reader.read_double()?;
            selector.world_limited = true;
            let volume = selector.volume.get_or_insert([0.0, 0.0, 0.0]);
            match key {
                "dx" => volume[0] = value,
                "dy" => volume[1] = value,
                _ => volume[2] = value,
            }
        }
        "scores" => {
            let entries = read_scores_map(reader)?;
            selector.predicates.push(SelectorPredicate::Scores(entries));
        }
        // Known to vanilla, not implemented here, and named so the author knows
        // which it is. Each needs a subsystem that does not exist: entity NBT
        // (`nbt`), the advancement predicate engine (`advancements`,
        // `predicate`), entity tags (`tag`), teams (`team`), experience levels
        // (`level`), or per-entity rotation tracking (`*_rotation`).
        "nbt" | "advancements" | "predicate" | "tag" | "team" | "level" | "x_rotation"
        | "y_rotation" => {
            Err(refuse(key_position, format!("selector option '{key}' is not supported yet")))?;
        }
        _ => Err(refuse(key_position, key.to_string()))?,
    }
    Ok(())
}

/// `EntitySelectorOptions.SCORES`'s map literal: `{obj1=1..5,obj2=10}`. Each
/// range reuses [`crate::scoreboard::IntRangeArg`] rather than a second
/// hand-written reader, so the two syntaxes (`/execute if score … matches`
/// and this) cannot drift apart.
fn read_scores_map(
    reader: &mut StringReader,
) -> Result<Vec<(String, crate::scoreboard::IntRange)>, ParseError> {
    let open = reader.cursor();
    if reader.peek() != Some('{') {
        return Err(refuse(open, "expected '{'"));
    }
    reader.skip();
    let mut entries = Vec::new();
    skip_whitespace(reader);
    if reader.peek() != Some('}') {
        loop {
            skip_whitespace(reader);
            let name_position = reader.cursor();
            let objective = reader.read_unquoted_string();
            if objective.is_empty() {
                return Err(refuse(name_position, "expected an objective name"));
            }
            skip_whitespace(reader);
            if reader.peek() != Some('=') {
                return Err(refuse(reader.cursor(), format!("objective '{objective}' has no value")));
            }
            reader.skip();
            skip_whitespace(reader);
            let range_position = reader.cursor();
            let range_value = crate::scoreboard::IntRangeArg
                .parse(reader)
                .map_err(|_| refuse(range_position, "expected an integer range"))?;
            let range = *range_value
                .downcast_ref::<crate::scoreboard::IntRange>()
                .expect("IntRangeArg produces an IntRange");
            entries.push((objective, range));
            skip_whitespace(reader);
            if reader.peek() == Some(',') {
                reader.skip();
                continue;
            }
            break;
        }
    }
    skip_whitespace(reader);
    if reader.peek() != Some('}') {
        return Err(refuse(reader.cursor(), "expected '}'"));
    }
    reader.skip();
    Ok(entries)
}

/// `EntitySelectorParser.shouldInvertValue`.
fn read_inversion(reader: &mut StringReader) -> bool {
    skip_whitespace(reader);
    if reader.peek() == Some('!') {
        reader.skip();
        skip_whitespace(reader);
        true
    } else {
        false
    }
}

/// `Identifier.read`: `[a-z0-9_.-]*:[a-z0-9_./-]*`, defaulting the namespace to
/// `minecraft` when there is no colon.
fn read_resource_key(reader: &mut StringReader, position: usize) -> Result<String, ParseError> {
    let start = reader.cursor();
    while reader.can_read() {
        match reader.peek() {
            Some(c)
                if c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, '_' | ':' | '/' | '.' | '-') =>
            {
                reader.skip();
            }
            _ => break,
        }
    }
    let source = reader.source();
    let text: String = source.chars().skip(start).take(reader.cursor() - start).collect();
    if text.is_empty() {
        reader.set_cursor(start);
        return Err(refuse(position, "expected a resource key"));
    }
    Ok(if text.contains(':') { text } else { format!("minecraft:{text}") })
}

/// `StringReader.skipWhitespace`.
fn skip_whitespace(reader: &mut StringReader) {
    while reader.peek().is_some_and(char::is_whitespace) {
        reader.skip();
    }
}

impl fmt::Display for SelectorOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Arbitrary => "arbitrary",
            Self::Nearest => "nearest",
            Self::Furthest => "furthest",
            Self::Random => "random",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arg: EntityArg, text: &str) -> Result<EntitySelector, ParseError> {
        let mut reader = StringReader::new(text);
        arg.parse(&mut reader).map(|value| {
            value.downcast_ref::<EntitySelector>().expect("EntityArg produces an EntitySelector").clone()
        })
    }

    fn players(text: &str) -> EntitySelector {
        parse(EntityArg::players(), text).unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    /// The six selector kinds, against `parseSelector`'s own switch read this
    /// session. Predicted values, not "it parsed": `max_results`,
    /// `includes_entities`, the order, and whether the implicit `isAlive`
    /// predicate is added all differ per kind and all three have been got wrong
    /// in ports of this switch.
    #[test]
    fn the_six_selector_kinds_set_the_fields_parse_selector_sets() {
        let all = players("@a");
        assert_eq!(all.max_results, usize::MAX);
        assert!(!all.includes_entities);
        assert_eq!(all.order, SelectorOrder::Arbitrary);
        assert_eq!(all.limit_to_type.as_deref(), Some(PLAYER_TYPE));
        assert!(all.predicates.is_empty(), "@a adds no isAlive predicate");

        let nearest = players("@p");
        assert_eq!(nearest.max_results, 1);
        assert_eq!(nearest.order, SelectorOrder::Nearest);

        let random = players("@r");
        assert_eq!(random.order, SelectorOrder::Random);
        assert_eq!(random.max_results, 1);

        let me = players("@s");
        assert!(me.current_entity);
        assert!(me.includes_entities, "@s does not restrict to players");
        assert_eq!(me.max_results, 1);
        assert!(me.predicates.is_empty(), "@s adds no isAlive predicate");

        let entities = parse(EntityArg::entities(), "@e").expect("@e is legal for entities()");
        assert_eq!(entities.max_results, usize::MAX);
        assert!(entities.includes_entities);
        assert_eq!(
            entities.predicates,
            [SelectorPredicate::Alive],
            "@e adds Entity::isAlive"
        );

        let nearest_entity = parse(EntityArg::entity(), "@n").expect("@n is legal for entity()");
        assert_eq!(nearest_entity.order, SelectorOrder::Nearest);
        assert_eq!(nearest_entity.predicates, [SelectorPredicate::Alive]);
    }

    /// `players()` refuses `@e` and `entity()` refuses `@a` — and the control
    /// that both are legal for the *other* argument type, so the refusals are
    /// about the constraint and not about the grammar.
    #[test]
    fn the_single_and_players_only_constraints_are_enforced_after_the_options() {
        assert!(parse(EntityArg::players(), "@e").is_err());
        assert!(parse(EntityArg::entities(), "@e").is_ok());

        assert!(parse(EntityArg::entity(), "@a").is_err());
        assert!(parse(EntityArg::entities(), "@a").is_ok());

        // `@s` is exempt from `players_only` — `!isSelfSelector()` in vanilla's
        // own condition. Without it `/gamemode creative @s` is refused, which is
        // the single most-used form of the command.
        assert!(
            parse(EntityArg::players(), "@s").is_ok(),
            "@s must be accepted by a players() argument despite includes_entities"
        );

        // The load-bearing case: `@e[limit=1]` *is* single, and a constraint
        // check that ran before the options were read would wrongly reject it.
        assert!(
            parse(EntityArg::entity(), "@e[limit=1]").is_ok(),
            "@e[limit=1] is a single-entity selector"
        );
        assert!(parse(EntityArg::entity(), "@e[limit=2]").is_err());
    }

    #[test]
    fn a_bare_name_and_a_bare_uuid_are_both_selectors() {
        let name = players("Steve");
        assert_eq!(name.player_name.as_deref(), Some("Steve"));
        assert_eq!(name.max_results, 1);
        assert!(!name.uses_selectors);
        assert!(!name.includes_entities);

        let uuid = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
        let by_uuid = parse(EntityArg::entity(), uuid).expect("a uuid is a legal entity selector");
        assert_eq!(by_uuid.entity_uuid, Some(uuid.parse().unwrap()));
        assert!(by_uuid.includes_entities);

        // A 17-character name is refused (`parseNameOrUUID`'s `length() > 16`).
        assert!(parse(EntityArg::players(), "AbcdefghijklmnopQ").is_err());
        assert!(parse(EntityArg::players(), "Abcdefghijklmnop").is_ok(), "16 is legal");
    }

    #[test]
    fn the_v1_options_parse_to_the_values_they_name() {
        let s = players("@a[distance=..8]");
        assert_eq!(s.distance, Some(Bounds { min: None, max: Some(8.0) }));
        assert!(s.world_limited);

        let s = players("@a[distance=2..4.5]");
        assert_eq!(s.distance, Some(Bounds { min: Some(2.0), max: Some(4.5) }));

        // An exact value sets *both* ends — `fromReader`'s `max = min` branch.
        let s = players("@a[distance=3]");
        assert_eq!(s.distance, Some(Bounds { min: Some(3.0), max: Some(3.0) }));

        let s = players("@a[limit=3,sort=furthest]");
        assert_eq!(s.max_results, 3);
        assert_eq!(s.order, SelectorOrder::Furthest);

        let s = players("@a[gamemode=!creative]");
        assert_eq!(
            s.predicates,
            [SelectorPredicate::GameMode { mode: GameMode::Creative, inverted: true }]
        );

        let s = players("@a[name=Steve]");
        assert_eq!(
            s.predicates,
            [SelectorPredicate::Name { name: "Steve".to_string(), inverted: false }]
        );

        let s = players("@a[x=1.5,y=-2,z=3,dx=4,dz=6]");
        assert_eq!(
            s.position,
            SelectorPosition { x: Some(1.5), y: Some(-2.0), z: Some(3.0) }
        );
        assert_eq!(s.volume, Some([4.0, 0.0, 6.0]), "an absent delta component is 0.0");

        // `type=` resolves the default namespace and validates against the real
        // 26.2 census, so a typo fails at parse.
        let s = parse(EntityArg::entities(), "@e[type=cow]").expect("cow is an entity type");
        assert_eq!(
            s.predicates,
            [
                SelectorPredicate::Alive,
                SelectorPredicate::EntityType { id: "minecraft:cow".to_string(), inverted: false }
            ]
        );
        assert_eq!(s.limit_to_type.as_deref(), Some("minecraft:cow"));
        assert!(parse(EntityArg::entities(), "@e[type=coww]").is_err());

        // `type=player` forces players-only, which is what makes
        // `@e[type=player]` legal for a `players()` argument.
        assert!(parse(EntityArg::players(), "@e[type=player]").is_ok());
        assert!(parse(EntityArg::players(), "@e[type=cow]").is_err());
    }

    /// `scores={obj=range,...}` — one, two and inverted-shape ranges, and the
    /// three malformed forms a hand-rolled `{}` reader is likely to get wrong
    /// (missing brace, missing value, trailing comma).
    #[test]
    fn scores_parses_a_map_of_objective_to_int_range() {
        use crate::scoreboard::IntRange;

        let s = players("@a[scores={foo=1..5}]");
        assert_eq!(
            s.predicates,
            [SelectorPredicate::Scores(vec![(
                "foo".to_string(),
                IntRange { min: Some(1), max: Some(5) }
            )])]
        );

        // An exact value sets both ends, same as `distance`.
        let s = players("@a[scores={foo=10}]");
        assert_eq!(
            s.predicates,
            [SelectorPredicate::Scores(vec![("foo".to_string(), IntRange { min: Some(10), max: Some(10) })])]
        );

        // Two entries, pairwise-distinct ranges so a transposition would show.
        let s = players("@a[scores={foo=1..5,bar=..20}]");
        assert_eq!(
            s.predicates,
            [SelectorPredicate::Scores(vec![
                ("foo".to_string(), IntRange { min: Some(1), max: Some(5) }),
                ("bar".to_string(), IntRange { min: None, max: Some(20) }),
            ])]
        );

        // The empty map is legal — `scores={}` matches trivially, same as
        // vanilla's empty-map short-circuit.
        let s = players("@a[scores={}]");
        assert_eq!(s.predicates, [SelectorPredicate::Scores(vec![])]);

        for bad in [
            "@a[scores=]",
            "@a[scores={foo}]",
            "@a[scores={foo=}]",
            "@a[scores={foo=1]",
            "@a[scores={foo=1,}]",
        ] {
            assert!(parse(EntityArg::players(), bad).is_err(), "{bad:?} must not parse");
        }
    }

    /// A deferred option is refused by name, and an unknown one is refused as
    /// itself. Both must fail — the danger is an ignored option silently
    /// widening the match.
    #[test]
    fn deferred_and_unknown_options_are_both_refused_and_say_which() {
        let deferred = parse(EntityArg::players(), "@a[tag=foo]").expect_err("tag is deferred");
        assert!(
            deferred.to_string().contains("not supported yet"),
            "a deferred option must say so: {deferred}"
        );

        let unknown = parse(EntityArg::players(), "@a[wibble=1]").expect_err("wibble is not an option");
        assert!(
            !unknown.to_string().contains("not supported yet"),
            "an unknown option must not claim to be a deferred one: {unknown}"
        );

        // The control: the same bracket syntax with a *supported* option parses,
        // so the two refusals above are about the option and not the brackets.
        assert!(parse(EntityArg::players(), "@a[limit=1]").is_ok());
    }

    #[test]
    fn malformed_option_syntax_is_an_error_rather_than_a_silent_stop() {
        for bad in ["@a[", "@a[limit]", "@a[limit=1", "@a[limit=1 sort=nearest]", "@a[limit=0]"] {
            assert!(
                parse(EntityArg::players(), bad).is_err(),
                "{bad:?} must not parse"
            );
        }
    }

    /// A failed parse leaves the cursor untouched, so a sibling argument node
    /// tried after this one starts from the same place.
    #[test]
    fn a_failed_parse_rewinds_the_cursor() {
        let mut reader = StringReader::new("@z rest");
        assert!(EntityArg::players().parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0);
    }

    /// The wire flags are the constructor's own two booleans, and the four
    /// constructors cover the four combinations vanilla has.
    #[test]
    fn the_wire_flags_are_the_two_constraints() {
        assert_eq!(
            EntityArg::players().wire(),
            ArgumentParser::Entity { single: false, players_only: true }
        );
        assert_eq!(
            EntityArg::player().wire(),
            ArgumentParser::Entity { single: true, players_only: true }
        );
        assert_eq!(
            EntityArg::entities().wire(),
            ArgumentParser::Entity { single: false, players_only: false }
        );
        assert_eq!(
            EntityArg::entity().wire(),
            ArgumentParser::Entity { single: true, players_only: false }
        );
    }

    #[test]
    fn a_players_only_argument_does_not_suggest_the_entity_selectors() {
        let suggestions = EntityArg::players().suggest("");
        assert!(suggestions.contains(&"@a".to_string()));
        assert!(!suggestions.contains(&"@e".to_string()));
        assert!(EntityArg::entities().suggest("").contains(&"@e".to_string()));
    }

    /// `distance=5..1` is swapped and refused (`MinMaxBounds.Doubles.fromReader`'s
    /// `areSwapped`), and the `Bounds::matches` predicate the resolver uses is
    /// checked against hand-computed values rather than against itself.
    #[test]
    fn bounds_reject_a_swapped_range_and_match_inclusively() {
        assert!(parse(EntityArg::players(), "@a[distance=5..1]").is_err());
        assert!(parse(EntityArg::players(), "@a[distance=-1]").is_err());

        let bounds = Bounds { min: Some(2.0), max: Some(4.0) };
        assert!(!bounds.matches(1.99));
        assert!(bounds.matches(2.0), "inclusive at the minimum");
        assert!(bounds.matches(4.0), "inclusive at the maximum");
        assert!(!bounds.matches(4.01));

        let open = Bounds { min: None, max: Some(8.0) };
        assert!(open.matches(-100.0));
        assert!(open.matches(8.0));
        assert!(!open.matches(8.5));
    }
}
