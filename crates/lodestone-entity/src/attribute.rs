//! Vanilla's attribute system: a base value plus modifiers combined in a fixed
//! three-stage order.
//!
//! The arithmetic here is the part that is easy to get subtly wrong. Vanilla's
//! `AttributeInstance.calculateValue` is:
//!
//! ```text
//! base   = baseValue + Σ amount           (over ADD_VALUE)
//! result = base
//! result = result + Σ base * amount        (over ADD_MULTIPLIED_BASE)
//! result = result * Π (1 + amount)         (over ADD_MULTIPLIED_TOTAL)
//! value  = sanitize(result)                (clamp to [min, max], NaN -> min)
//! ```
//!
//! The three stages are not interchangeable: `ADD_MULTIPLIED_BASE` always
//! multiplies the *post-addition* base, while `ADD_MULTIPLIED_TOTAL` multiplies
//! the running total. Applying them in the wrong order — or folding the base
//! multipliers into the total multipliers — produces speeds that are close
//! enough to look right and wrong enough for a server's movement checks to
//! notice.
//!
//! [`Operation::AddMultipliedTotal`] with amount `0.3` is exactly the sprint
//! modifier `lodestone-physics` already models, so the two crates agree on the
//! convention by construction.

use lodestone_model::{EntityAttributeSnapshot, Identifier};
use std::collections::HashMap;
use std::str::FromStr;

/// How an [`AttributeModifier`] combines with the running value.
///
/// The discriminants match vanilla's wire ids (`ADD_VALUE = 0`,
/// `ADD_MULTIPLIED_BASE = 1`, `ADD_MULTIPLIED_TOTAL = 2`) so a version crate can
/// map a serialized operation id straight onto this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    /// Adds `amount` to the base value (the classic "addition" operation).
    AddValue,
    /// Adds `base * amount` to the running result, where `base` is the value
    /// after all [`AddValue`](Operation::AddValue) modifiers.
    AddMultipliedBase,
    /// Multiplies the running result by `1 + amount` (the classic
    /// "multiply total" operation, applied last).
    AddMultipliedTotal,
}

impl Operation {
    /// The vanilla wire id for this operation.
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Operation::AddValue => 0,
            Operation::AddMultipliedBase => 1,
            Operation::AddMultipliedTotal => 2,
        }
    }

    /// Maps a vanilla wire id back to an operation, if it is in range.
    #[must_use]
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Operation::AddValue),
            1 => Some(Operation::AddMultipliedBase),
            2 => Some(Operation::AddMultipliedTotal),
            _ => None,
        }
    }
}

/// A single modifier applied to an attribute, identified by a stable key so it
/// can be replaced or removed without duplicating.
#[derive(Debug, Clone, PartialEq)]
pub struct Modifier {
    /// Stable identity of the modifier (vanilla keys these by a namespaced id).
    pub id: Identifier,
    /// The modifier amount, interpreted per [`Operation`].
    pub amount: f64,
    /// How the amount combines with the running value.
    pub operation: Operation,
}

impl Modifier {
    /// Creates a modifier.
    #[must_use]
    pub fn new(id: Identifier, amount: f64, operation: Operation) -> Self {
        Self {
            id,
            amount,
            operation,
        }
    }
}

/// The static definition of an attribute: its default and its valid range.
///
/// Vanilla's `RangedAttribute` clamps every computed value into `[min, max]`
/// and maps `NaN` to `min`; [`AttributeDef::sanitize`] reproduces that exactly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttributeDef {
    /// The default base value when no explicit base is supplied.
    pub default: f64,
    /// Inclusive lower clamp bound.
    pub min: f64,
    /// Inclusive upper clamp bound.
    pub max: f64,
}

impl AttributeDef {
    /// Creates a definition.
    #[must_use]
    pub const fn new(default: f64, min: f64, max: f64) -> Self {
        Self { default, min, max }
    }

    /// Clamps `value` into `[min, max]`, mapping `NaN` to `min`, matching
    /// `RangedAttribute.sanitizeValue`.
    #[must_use]
    pub fn sanitize(&self, value: f64) -> f64 {
        if value.is_nan() {
            self.min
        } else {
            value.clamp(self.min, self.max)
        }
    }
}

/// A live attribute: a definition, a base value, and a set of modifiers keyed by
/// id.
///
/// The value is recomputed lazily and cached; any mutation invalidates the
/// cache. Modifier iteration order within an operation follows insertion order,
/// which is deterministic (vanilla uses an unordered map, so any order is a
/// faithful choice; the additive stages are order-independent and the
/// multiplicative stage is commutative up to floating-point rounding).
#[derive(Debug, Clone)]
pub struct AttributeInstance {
    def: AttributeDef,
    base: f64,
    order: Vec<Identifier>,
    modifiers: HashMap<Identifier, Modifier>,
    cache: std::cell::Cell<Option<f64>>,
}

impl AttributeInstance {
    /// Creates an instance seeded with the definition's default base value.
    #[must_use]
    pub fn new(def: AttributeDef) -> Self {
        Self {
            def,
            base: def.default,
            order: Vec::new(),
            modifiers: HashMap::new(),
            cache: std::cell::Cell::new(None),
        }
    }

    /// The definition backing this instance.
    #[must_use]
    pub fn def(&self) -> AttributeDef {
        self.def
    }

    /// The current base value (before modifiers).
    #[must_use]
    pub fn base_value(&self) -> f64 {
        self.base
    }

    /// Sets the base value, invalidating the cached result if it changed.
    pub fn set_base_value(&mut self, base: f64) {
        if base != self.base {
            self.base = base;
            self.cache.set(None);
        }
    }

    /// Adds or replaces a modifier by its id.
    pub fn add_or_update(&mut self, modifier: Modifier) {
        if !self.modifiers.contains_key(&modifier.id) {
            self.order.push(modifier.id.clone());
        }
        self.modifiers.insert(modifier.id.clone(), modifier);
        self.cache.set(None);
    }

    /// Removes a modifier by id; returns whether one was present.
    pub fn remove(&mut self, id: &Identifier) -> bool {
        if self.modifiers.remove(id).is_some() {
            self.order.retain(|existing| existing != id);
            self.cache.set(None);
            true
        } else {
            false
        }
    }

    /// The number of modifiers currently applied.
    #[must_use]
    pub fn modifier_count(&self) -> usize {
        self.modifiers.len()
    }

    fn iter_op(&self, op: Operation) -> impl Iterator<Item = &Modifier> {
        self.order
            .iter()
            .filter_map(move |id| self.modifiers.get(id))
            .filter(move |m| m.operation == op)
    }

    /// The final, sanitized value after applying all modifiers in vanilla's
    /// three-stage order. Cached until the next mutation.
    #[must_use]
    pub fn value(&self) -> f64 {
        if let Some(v) = self.cache.get() {
            return v;
        }
        let mut base = self.base;
        for m in self.iter_op(Operation::AddValue) {
            base += m.amount;
        }
        let mut result = base;
        for m in self.iter_op(Operation::AddMultipliedBase) {
            result += base * m.amount;
        }
        for m in self.iter_op(Operation::AddMultipliedTotal) {
            result *= 1.0 + m.amount;
        }
        let sanitized = self.def.sanitize(result);
        self.cache.set(Some(sanitized));
        sanitized
    }
}

/// A collection of attribute instances keyed by attribute id.
#[derive(Debug, Clone, Default)]
pub struct AttributeMap {
    instances: HashMap<Identifier, AttributeInstance>,
}

impl AttributeMap {
    /// Creates an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts (or replaces) an instance for `key`, seeded from the default
    /// registry definition. Returns a mutable reference to it.
    pub fn get_or_default(&mut self, key: &Identifier) -> &mut AttributeInstance {
        let def = default_def(key).unwrap_or(AttributeDef::new(0.0, f64::MIN, f64::MAX));
        self.instances
            .entry(key.clone())
            .or_insert_with(|| AttributeInstance::new(def))
    }

    /// Inserts an instance under `key`.
    pub fn insert(&mut self, key: Identifier, instance: AttributeInstance) {
        self.instances.insert(key, instance);
    }

    /// The instance for `key`, if present.
    #[must_use]
    pub fn get(&self, key: &Identifier) -> Option<&AttributeInstance> {
        self.instances.get(key)
    }

    /// A mutable reference to the instance for `key`, if present.
    pub fn get_mut(&mut self, key: &Identifier) -> Option<&mut AttributeInstance> {
        self.instances.get_mut(key)
    }

    /// The computed value for `key`, falling back to the registry default base
    /// value when the attribute is not present in this map.
    #[must_use]
    pub fn value(&self, key: &Identifier) -> Option<f64> {
        if let Some(instance) = self.instances.get(key) {
            Some(instance.value())
        } else {
            default_def(key).map(|d| d.default)
        }
    }

    /// Number of attributes tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.instances.len()
    }

    /// Whether the map holds no attributes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// Iterates the attribute keys currently tracked, in unspecified order.
    pub fn keys(&self) -> impl Iterator<Item = &Identifier> {
        self.instances.keys()
    }

    /// Iterates `(key, instance)` pairs currently tracked, in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = (&Identifier, &AttributeInstance)> {
        self.instances.iter()
    }
}

/// Looks up the vanilla default definition for a namespaced attribute id.
///
/// This table is the version-free semantic content: the *set* of attributes and
/// their default/range are supplied here for the modern game. A version whose
/// registry differs supplies its own base values via the packet adapter; this
/// table is the fallback and the source of clamp ranges.
#[must_use]
pub fn default_def(key: &Identifier) -> Option<AttributeDef> {
    if key.namespace() != "minecraft" {
        return None;
    }
    let d = |default, min, max| Some(AttributeDef::new(default, min, max));
    match key.path() {
        "air_drag_modifier" => d(1.0, 0.0, 2048.0),
        "armor" => d(0.0, 0.0, 30.0),
        "armor_toughness" => d(0.0, 0.0, 20.0),
        "attack_damage" => d(2.0, 0.0, 2048.0),
        "attack_knockback" => d(0.0, 0.0, 5.0),
        "attack_speed" => d(4.0, 0.0, 1024.0),
        "below_name_distance" => d(10.0, 0.0, 512.0),
        "block_break_speed" => d(1.0, 0.0, 1024.0),
        "block_interaction_range" => d(4.5, 0.0, 64.0),
        "bounciness" => d(0.0, 0.0, 1.0),
        "burning_time" => d(1.0, 0.0, 1024.0),
        "camera_distance" => d(4.0, 0.0, 32.0),
        "entity_interaction_range" => d(3.0, 0.0, 64.0),
        "explosion_knockback_resistance" => d(0.0, 0.0, 1.0),
        "fall_damage_multiplier" => d(1.0, 0.0, 100.0),
        "flying_speed" => d(0.4, 0.0, 1024.0),
        "follow_range" => d(32.0, 0.0, 2048.0),
        "friction_modifier" => d(1.0, 0.0, 2048.0),
        "gravity" => d(0.08, -1.0, 1.0),
        "jump_strength" => d(0.42, 0.0, 32.0),
        "knockback_resistance" => d(0.0, -2.0, 1.0),
        "luck" => d(0.0, -1024.0, 1024.0),
        "max_absorption" => d(0.0, 0.0, 2048.0),
        "max_health" => d(20.0, 1.0, 1024.0),
        "mining_efficiency" => d(0.0, 0.0, 1024.0),
        "movement_efficiency" => d(0.0, 0.0, 1.0),
        "movement_speed" => d(0.7, 0.0, 1024.0),
        "name_tag_distance" => d(64.0, 0.0, 512.0),
        "oxygen_bonus" => d(0.0, 0.0, 1024.0),
        "safe_fall_distance" => d(3.0, -1024.0, 1024.0),
        "scale" => d(1.0, 0.0625, 16.0),
        "sneaking_speed" => d(0.3, 0.0, 1.0),
        "spawn_reinforcements" => d(0.0, 0.0, 1.0),
        "step_height" => d(0.6, 0.0, 10.0),
        "submerged_mining_speed" => d(0.2, 0.0, 20.0),
        "sweeping_damage_ratio" => d(0.0, 0.0, 1.0),
        "tempt_range" => d(10.0, 0.0, 2048.0),
        "water_movement_efficiency" => d(0.0, 0.0, 1.0),
        "waypoint_transmit_range" => d(0.0, 0.0, 6.0e7),
        "waypoint_receive_range" => d(0.0, 0.0, 6.0e7),
        _ => None,
    }
}

/// The full set of modern attribute ids this crate knows a default for. Useful
/// for whole-corpus assertions against a registry report.
#[must_use]
pub fn known_attribute_paths() -> &'static [&'static str] {
    &[
        "air_drag_modifier",
        "armor",
        "armor_toughness",
        "attack_damage",
        "attack_knockback",
        "attack_speed",
        "below_name_distance",
        "block_break_speed",
        "block_interaction_range",
        "bounciness",
        "burning_time",
        "camera_distance",
        "entity_interaction_range",
        "explosion_knockback_resistance",
        "fall_damage_multiplier",
        "flying_speed",
        "follow_range",
        "friction_modifier",
        "gravity",
        "jump_strength",
        "knockback_resistance",
        "luck",
        "max_absorption",
        "max_health",
        "mining_efficiency",
        "movement_efficiency",
        "movement_speed",
        "name_tag_distance",
        "oxygen_bonus",
        "safe_fall_distance",
        "scale",
        "sneaking_speed",
        "spawn_reinforcements",
        "step_height",
        "submerged_mining_speed",
        "sweeping_damage_ratio",
        "tempt_range",
        "water_movement_efficiency",
        "waypoint_transmit_range",
        "waypoint_receive_range",
    ]
}

/// The canonical id of vanilla's `water_movement_efficiency` attribute — the
/// attribute Depth Strider modifies, and the one this module's wire-fold
/// ([`instance_from_snapshot`]/[`attribute_value`]) exists to make reachable.
/// A small convenience so a per-tick caller (e.g. the physics-tick system
/// that feeds [`PlayerState::with_water_movement_efficiency`](
/// https://docs.rs/lodestone-physics) — see `docs/swimming.md`) doesn't have
/// to hand-parse the literal every tick.
#[must_use]
pub fn water_movement_efficiency_key() -> Identifier {
    Identifier::from_str("minecraft:water_movement_efficiency").expect("valid built-in identifier")
}

/// The canonical id of vanilla's `movement_speed` attribute.
///
/// Reaches physics via [`PlayerState::with_movement_speed`](
/// https://docs.rs/lodestone-physics) the same way
/// [`water_movement_efficiency_key`] reaches
/// [`PlayerState::with_water_movement_efficiency`](
/// https://docs.rs/lodestone-physics) for Depth Strider — same shape of fold,
/// same seam, one attribute later. Folding the server-reported snapshot
/// through [`attribute_value`] already covers Speed/Slowness and soul speed
/// for free: vanilla applies those as `MOVEMENT_SPEED` `AttributeModifier`s
/// **server-side** (`LivingEntity.onEffectAdded`/`onEffectUpdated`, gated on
/// `!level().isClientSide()`) and syncs the resulting base+modifiers back over the
/// wire via `ClientboundUpdateAttributesPacket`
/// (`ServerEntity.sendPairingData` on initial tracking, `ServerEntity.sendDirtyEntityData`
/// on the per-tick resync `ServerEntity.sendChanges` drives);
/// the client never re-derives the modifier itself. The sprint bonus is the
/// same shape too — `LivingEntity.setSprinting` adds/removes a transient
/// `+0.3F ADD_MULTIPLIED_TOTAL` modifier keyed `minecraft:sprinting`
/// (`LivingEntity.setSprinting`, matching
/// [`sprint_modifier_matches_physics_convention`]'s worked example) — but a
/// caller reading this attribute client-side sees that modifier only once the
/// server has processed the corresponding `PlayerCommand` and resynced, which
/// lags the client's own locally-latched sprint key by a tick or more; that
/// latency is why `lodestone_ecs::player::player_physics` keeps its own
/// local sprint multiply on top of this attribute's folded value rather than
/// relying on the modifier alone (`docs/swimming.md`).
#[must_use]
pub fn movement_speed_key() -> Identifier {
    Identifier::from_str("minecraft:movement_speed").expect("valid built-in identifier")
}

/// Vanilla's transient sprint modifier on `minecraft:movement_speed` —
/// `LivingEntity.SPRINTING_MODIFIER_ID` and its `SPEED_MODIFIER_SPRINTING`
/// constant, `+0.3` `ADD_MULTIPLIED_TOTAL`.
///
/// Exposed so a client that predicts sprint locally can tell whether the
/// server's own modifier has arrived yet. `LivingEntity.setSprinting` adds and
/// removes this on the entity's `AttributeMap`, and
/// `ClientboundUpdateAttributesPacket` carries the modifier list — so once the
/// packet lands the folded value **already includes** the sprint bonus, and
/// anything applying a second local multiply on top would compound it
/// (~1.69x rather than 1.3x). See `lodestone_ecs::player::player_physics`.
#[must_use]
pub fn sprinting_modifier_id() -> Identifier {
    Identifier::from_str("minecraft:sprinting").expect("valid built-in identifier")
}

/// Builds a foldable [`AttributeInstance`] from a wire-shaped
/// [`EntityAttributeSnapshot`] — the shape `ClientboundUpdateAttributesPacket`
/// decodes to (see `lodestone_v770::packets::metadata::read_update_attributes`).
///
/// The wire snapshot carries only `base` and `modifiers`; it has no min/max
/// range (vanilla never sends `RangedAttribute`'s bounds over the network —
/// the client is expected to already know them from its own registry). Those
/// clamp bounds are filled in from [`default_def`], falling back to an
/// unranged definition for an attribute id this crate's table does not know
/// (an unknown-but-syncable attribute from a future version should still
/// fold to *something* rather than being unrepresentable).
///
/// A modifier whose wire `operation` byte is not `0`/`1`/`2` is dropped
/// rather than rejecting the whole snapshot, matching the adapter's
/// per-packet error tolerance one layer down
/// (`v770::adapter::handle_update_attributes`'s "swallow and skip" policy) —
/// there is no such id in a real 26.2 server, but a malformed one should not
/// panic a client that already decoded the packet successfully.
#[must_use]
pub fn instance_from_snapshot(snapshot: &EntityAttributeSnapshot) -> AttributeInstance {
    let def = default_def(&snapshot.attribute).unwrap_or(AttributeDef::new(0.0, f64::MIN, f64::MAX));
    let mut instance = AttributeInstance::new(def);
    instance.set_base_value(snapshot.base);
    for modifier in &snapshot.modifiers {
        if let Some(operation) = Operation::from_id(modifier.operation) {
            instance.add_or_update(Modifier::new(modifier.id.clone(), modifier.amount, operation));
        }
    }
    instance
}

/// Folds a wire-reported attribute list ([`EntityView::attributes`](
/// https://docs.rs/lodestone-client) / `NetClient::local_player_attributes`'s
/// return shape) down to one attribute's computed value, per vanilla's
/// three-stage `AttributeInstance.calculateValue`
/// ([`AttributeInstance::value`]).
///
/// Falls back to the registry default when `key` is absent from `snapshots`
/// — the server only ever sends an attribute once something makes it worth
/// syncing (a base override, or a modifier), so a fresh entity with no
/// enchantments and no effects legitimately never gets an explicit
/// `water_movement_efficiency` entry. Absence must read as "still the
/// default", not "zero forever" or "unknown". Returns `0.0` if `key` has no
/// known default either (an attribute this crate's registry table has never
/// heard of).
#[must_use]
pub fn attribute_value(snapshots: &[EntityAttributeSnapshot], key: &Identifier) -> f64 {
    match snapshots.iter().find(|s| &s.attribute == key) {
        Some(snapshot) => instance_from_snapshot(snapshot).value(),
        None => default_def(key).map_or(0.0, |d| d.default),
    }
}

/// The base-class attribute template a concrete entity type is built on,
/// mirroring vanilla's `createLivingAttributes` → `createMobAttributes` →
/// `createMonsterAttributes` / `createAnimalAttributes` chain.
///
/// All three variants extend `Mob.createMobAttributes()` (living +
/// `follow_range` 16), which is the shared prefix in
/// [`template_bases`]; they differ only in the one attribute the subclass
/// builder adds.
///
/// # Why the third variant is `Mob` and not `AbstractGolem`
///
/// A snow golem is an `AbstractGolem`, so the obvious reading is that it needs
/// an `AbstractGolem` variant. It does not: **`AbstractGolem` declares no
/// `createAttributes` at all**, and `SnowGolem.createAttributes()` calls
/// `Mob.createMobAttributes()` directly. The same is true of a ghast, which
/// is `extends Mob implements Enemy` — hostile by interface, but with none of
/// `Monster`'s `attack_damage` (`Ghast.createAttributes`).
///
/// So this enum keys on the **attribute-supplier chain**, not the class
/// hierarchy, and the two diverge. Adding a variant per superclass would have
/// produced an `AbstractGolem` arm that is byte-identical to `Mob` and a
/// standing invitation to give it attributes vanilla never gives it. The rule
/// for a new species is therefore: read which `create*Attributes()` its own
/// builder calls, and pick that — never its `extends` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseTemplate {
    /// `Mob.createMobAttributes()` on its own: living + `follow_range` 16, with
    /// no subclass addition. A ghast and a snow golem build on this directly.
    Mob,
    /// `Monster.createMonsterAttributes()`: mob + `attack_damage`.
    Monster,
    /// `Animal.createAnimalAttributes()`: mob + `tempt_range` 10.
    Animal,
}

/// The attribute set contributed by `LivingEntity.createLivingAttributes()`,
/// every entry seeded at its `RangedAttribute` default. Ordered as vanilla adds
/// them so a built map iterates deterministically.
const LIVING_PATHS: &[&str] = &[
    "max_health",
    "knockback_resistance",
    "movement_speed",
    "armor",
    "armor_toughness",
    "max_absorption",
    "step_height",
    "scale",
    "gravity",
    "safe_fall_distance",
    "fall_damage_multiplier",
    "jump_strength",
    "entity_interaction_range",
    "oxygen_bonus",
    "burning_time",
    "explosion_knockback_resistance",
    "water_movement_efficiency",
    "movement_efficiency",
    "attack_knockback",
    "camera_distance",
    "waypoint_transmit_range",
    "bounciness",
    "air_drag_modifier",
    "friction_modifier",
    "name_tag_distance",
    "below_name_distance",
];

/// A concrete entity type's base-value overrides: the explicit `.add(ATTR, v)`
/// calls in its `createAttributes()`, layered on top of its [`BaseTemplate`].
struct TypeSpec {
    template: BaseTemplate,
    /// `(attribute path, base value)` overrides. An entry whose path is not in
    /// the template's set *adds* that attribute (e.g. a zombie's
    /// `spawn_reinforcements`); an entry that is present *replaces* the base.
    overrides: &'static [(&'static str, f64)],
}

/// Resolves a modern entity type key to its vanilla base-attribute spec.
///
/// This is version-free semantic content read from 26.2's per-mob
/// `createAttributes()` builders: it is the *set* of attributes a type has plus
/// its base-value overrides, independent of any wire index. A version whose
/// registry differs would supply its own; this covers the mobs the client
/// currently renders plus their close variants.
fn type_spec(path: &str) -> Option<TypeSpec> {
    // Zombie family shares `Zombie.createAttributes()`.
    const ZOMBIE: &[(&str, f64)] = &[
        ("follow_range", 35.0),
        ("movement_speed", 0.23),
        ("attack_damage", 3.0),
        ("armor", 2.0),
        ("spawn_reinforcements", 0.0),
    ];
    // `Drowned.createAttributes()` is `Zombie`'s plus `STEP_HEIGHT 1.0`
    // — spelled out rather than derived
    // from `ZOMBIE`, because `overrides` is a `&'static [_]` and there is no
    // const concatenation. Keep the first five rows in sync with `ZOMBIE`;
    // `zombie_family_variants_share_their_parents_bases` pins that.
    const DROWNED: &[(&str, f64)] = &[
        ("follow_range", 35.0),
        ("movement_speed", 0.23),
        ("attack_damage", 3.0),
        ("armor", 2.0),
        ("spawn_reinforcements", 0.0),
        ("step_height", 1.0),
    ];
    let spec = match path {
        // `ZombieVillager` declares no `createAttributes`, so it shares
        // `Zombie.createAttributes()` — checked per
        // class, since `Drowned` in the same family does override.
        "zombie" | "husk" | "zombie_villager" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: ZOMBIE,
        },
        "drowned" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: DROWNED,
        },
        "skeleton" | "stray" | "wither_skeleton" | "bogged" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[("movement_speed", 0.25)],
        },
        // `Parched.createAttributes()` is `AbstractSkeleton`'s plus
        // `MAX_HEALTH 16.0`. A 26.2 variant not otherwise covered here.
        "parched" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[("movement_speed", 0.25), ("max_health", 16.0)],
        },
        "creeper" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[("movement_speed", 0.25)],
        },
        // `Witch.createAttributes()`: the monster
        // base plus `MAX_HEALTH 26.0` and `MOVEMENT_SPEED 0.25`. **26, not 20** —
        // the witch is one of the few monsters that is not on the generic health,
        // and inheriting the base here would have made it a third easier to kill
        // than vanilla's.
        "witch" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[("max_health", 26.0), ("movement_speed", 0.25)],
        },
        // `Pillager.createAttributes()`:
        // `MOVEMENT_SPEED 0.35`, `MAX_HEALTH 24.0`, `ATTACK_DAMAGE 5.0`,
        // `FOLLOW_RANGE 32.0`.
        //
        // The `follow_range` is the one worth naming: `Mob.createMobAttributes`
        // overrides the registry's 32.0 down to 16.0 for *every* mob, and the
        // pillager puts it back to 32.0 — so this is a real override that happens to
        // equal the registry default, and dropping it as redundant would halve the
        // pillager's acquisition range. The `attack_damage` of 5.0 is its melee
        // value; the crossbow bolt's damage comes from the projectile, not this.
        "pillager" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[
                ("movement_speed", 0.35),
                ("max_health", 24.0),
                ("attack_damage", 5.0),
                ("follow_range", 32.0),
            ],
        },
        // `ZombifiedPiglin.createAttributes()` is `Zombie`'s with
        // `SPAWN_REINFORCEMENTS_CHANCE` re-added as 0.0 (already 0.0 in
        // `ZOMBIE`, so a no-op), `MOVEMENT_SPEED` re-added as 0.23 (also a
        // no-op) and `ATTACK_DAMAGE` **raised to 5.0**. Two of its three
        // `add` calls restate the parent's value; only the damage differs, and
        // `zombie_family_variants_share_their_parents_bases` pins that split.
        "zombified_piglin" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[
                ("follow_range", 35.0),
                ("movement_speed", 0.23),
                ("attack_damage", 5.0),
                ("armor", 2.0),
                ("spawn_reinforcements", 0.0),
            ],
        },
        // `Guardian.createAttributes()`.
        "guardian" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[
                ("attack_damage", 6.0),
                ("movement_speed", 0.5),
                ("max_health", 30.0),
            ],
        },
        // `ElderGuardian.createAttributes()` is `Guardian.createAttributes()`
        // with all three of its values re-`add`ed — vanilla's `add` replaces,
        // so the elder keeps *none* of the guardian's numbers
        // Note the elder is the
        // **slower** of the two (0.3 against 0.5); a table derived from "elder
        // is the bigger one" would have got that backwards.
        "elder_guardian" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[
                ("attack_damage", 8.0),
                ("movement_speed", 0.3),
                ("max_health", 80.0),
            ],
        },
        // `Blaze.createAttributes()`. No
        // `max_health` override, so it keeps the living default of 20.
        "blaze" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[
                ("attack_damage", 6.0),
                ("movement_speed", 0.23),
                ("follow_range", 48.0),
            ],
        },
        // `Warden.createAttributes()`: `MAX_HEALTH 500.0`,
        // `MOVEMENT_SPEED 0.3`, `KNOCKBACK_RESISTANCE 1.0`,
        // `ATTACK_KNOCKBACK 1.5`, `ATTACK_DAMAGE 30.0`, `FOLLOW_RANGE 24.0`.
        // The warden is by far the highest-health, highest-damage entry in
        // this table, and `knockback_resistance` at the attribute's own `1.0`
        // ceiling means the melee/sonic-boom knockback formulas that scale by
        // `1.0 - knockback_resistance` are always zero against it — real,
        // not a bug in whatever consumes the attribute.
        "warden" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[
                ("max_health", 500.0),
                ("movement_speed", 0.3),
                ("knockback_resistance", 1.0),
                ("attack_knockback", 1.5),
                ("attack_damage", 30.0),
                ("follow_range", 24.0),
            ],
        },
        // `EnderMan.createAttributes()`.
        // `follow_range` 64 is the widest in this table and feeds
        // `MobSim::spawn_species`'s A* budget directly.
        "enderman" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[
                ("max_health", 40.0),
                ("movement_speed", 0.3),
                ("attack_damage", 7.0),
                ("follow_range", 64.0),
                ("step_height", 1.0),
            ],
        },
        // `Ghast.createAttributes()` — a
        // **bare `Mob`** builder despite `Ghast implements Enemy`, so it has
        // no `attack_damage` at all and no `movement_speed` override (it is
        // flight-driven; the 0.06 is `flying_speed`). Its `follow_range` 100
        // is the largest of any mob here.
        "ghast" => TypeSpec {
            template: BaseTemplate::Mob,
            overrides: &[
                ("max_health", 10.0),
                ("follow_range", 100.0),
                ("camera_distance", 8.0),
                ("flying_speed", 0.06),
            ],
        },
        // `SnowGolem.createAttributes()`
        // — also a bare `Mob` builder, not an `Animal` one: a snow golem has
        // no `tempt_range` and cannot be led by food.
        "snow_golem" => TypeSpec {
            template: BaseTemplate::Mob,
            overrides: &[("max_health", 4.0), ("movement_speed", 0.2)],
        },
        // `IronGolem.createAttributes()`
        // — also a bare `Mob` builder. `knockback_resistance` **1.0** is the
        // one to notice: a golem cannot be knocked back at all, unlike every
        // other mob in this table, and `step_height` **1.0** is a full block
        // rather than the usual 0.6, so it walks straight over a single-block
        // rise instead of needing to jump it.
        "iron_golem" => TypeSpec {
            template: BaseTemplate::Mob,
            overrides: &[
                ("max_health", 100.0),
                ("movement_speed", 0.25),
                ("knockback_resistance", 1.0),
                ("attack_damage", 15.0),
                ("step_height", 1.0),
            ],
        },
        // `Bee.createAttributes()`. An
        // `Animal` that also carries `ATTACK_DAMAGE`, like the rabbit below.
        "bee" => TypeSpec {
            template: BaseTemplate::Animal,
            overrides: &[
                ("max_health", 10.0),
                ("flying_speed", 0.6),
                ("movement_speed", 0.3),
                ("attack_damage", 2.0),
            ],
        },
        // `Wolf.createAttributes()`. A
        // `TamableAnimal`, but its builder calls `Animal.createAnimalAttributes()`
        // and `TamableAnimal` declares no `createAttributes` of its own — the
        // supplier chain, not the class chain (see [`BaseTemplate`]).
        "wolf" => TypeSpec {
            template: BaseTemplate::Animal,
            overrides: &[
                ("movement_speed", 0.3),
                ("max_health", 8.0),
                ("attack_damage", 4.0),
            ],
        },
        // `Villager.createAttributes()` —
        // the only override `AbstractVillager`'s hierarchy declares.
        // `WanderingTrader` has no `createAttributes` of its own, so it
        // inherits this: `max_health`/`attack_damage`/`armor` stay at the
        // generic `Mob` defaults, and only `movement_speed` differs from
        // this sim's own `DEFAULT_FOLLOW_RANGE`-style fallback.
        "wandering_trader" => TypeSpec {
            template: BaseTemplate::Mob,
            overrides: &[("movement_speed", 0.5)],
        },
        "spider" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[("max_health", 16.0), ("movement_speed", 0.3)],
        },
        // `CaveSpider.createCaveSpider()` is `Spider`'s with `MAX_HEALTH`
        // re-`add`ed as 12.0, so it keeps
        // the 0.3 speed and loses 4 health. Written flat rather than as
        // "spider's, overridden", because `add` in vanilla replaces and the
        // flat form is what `default_attributes` applies in order anyway.
        "cave_spider" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[("max_health", 12.0), ("movement_speed", 0.3)],
        },
        "pig" => TypeSpec {
            template: BaseTemplate::Animal,
            overrides: &[("max_health", 10.0), ("movement_speed", 0.25)],
        },
        "cow" | "mooshroom" => TypeSpec {
            template: BaseTemplate::Animal,
            overrides: &[("max_health", 10.0), ("movement_speed", 0.2)],
        },
        "sheep" => TypeSpec {
            template: BaseTemplate::Animal,
            overrides: &[("max_health", 8.0), ("movement_speed", 0.23)],
        },
        "chicken" => TypeSpec {
            template: BaseTemplate::Animal,
            overrides: &[("max_health", 4.0), ("movement_speed", 0.25)],
        },
        // `Rabbit.createAttributes()`.
        // The only `Animal` here that carries `ATTACK_DAMAGE`: the killer
        // bunny uses it, and vanilla puts it on every rabbit's supplier rather
        // than on that variant, so it belongs in the base set and not behind
        // `setRabbitType`. Its 0.3 speed is also why an unlisted rabbit was
        // the clearest symptom of this bug — the registry default is 0.7.
        "rabbit" => TypeSpec {
            template: BaseTemplate::Animal,
            overrides: &[
                ("max_health", 3.0),
                ("movement_speed", 0.3),
                ("attack_damage", 3.0),
            ],
        },
        // `Cat.createAttributes()`. A
        // `TamableAnimal`, same reason the wolf's arm above is: the supplier
        // chain calls `Animal.createAnimalAttributes()` directly, since
        // `TamableAnimal` declares no `createAttributes` of its own.
        "cat" => TypeSpec {
            template: BaseTemplate::Animal,
            overrides: &[
                ("max_health", 10.0),
                ("movement_speed", 0.3),
                ("attack_damage", 3.0),
            ],
        },
        // `Parrot.createAttributes()`.
        // The only species in this table with both `flying_speed` and
        // `attack_damage` set alongside a sub-default `movement_speed` — a
        // parrot walks slowly (`0.2`, against the registry default `0.7`)
        // but flies at a comparable clip (`0.4`).
        "parrot" => TypeSpec {
            template: BaseTemplate::Animal,
            overrides: &[
                ("max_health", 6.0),
                ("flying_speed", 0.4),
                ("movement_speed", 0.2),
                ("attack_damage", 3.0),
            ],
        },
        _ => return None,
    };
    Some(spec)
}

/// The ordered `(path, base value)` pairs a [`BaseTemplate`] contributes before
/// a type's own overrides, mirroring the `createMobAttributes` chain.
fn template_bases(template: BaseTemplate) -> Vec<(&'static str, f64)> {
    let mut bases: Vec<(&'static str, f64)> =
        LIVING_PATHS.iter().map(|p| (*p, default_path(p))).collect();
    // Every mob overrides the generic follow_range default (32) with 16.
    bases.push(("follow_range", 16.0));
    match template {
        // `Mob.createMobAttributes()` adds nothing beyond the `follow_range`
        // above — see `BaseTemplate::Mob`.
        BaseTemplate::Mob => {}
        BaseTemplate::Monster => bases.push(("attack_damage", default_path("attack_damage"))),
        BaseTemplate::Animal => bases.push(("tempt_range", default_path("tempt_range"))),
    }
    bases
}

/// The `RangedAttribute` default for a bare path, or `0.0` if unknown (unknown
/// paths never appear in the hand-verified templates above).
fn default_path(path: &str) -> f64 {
    // `Identifier::from_str` on a bare path yields the `minecraft` namespace.
    Identifier::from_str(&format!("minecraft:{path}"))
        .ok()
        .and_then(|id| default_def(&id))
        .map(|d| d.default)
        .unwrap_or(0.0)
}

/// Builds the fully-seeded [`AttributeMap`] for a modern entity type, matching
/// what vanilla's `DefaultAttributes.getSupplier(type)` produces: the type's
/// attribute set, each seeded with its per-type base value (or the generic
/// `RangedAttribute` default where the type does not override it).
///
/// Returns `None` for a type this crate has no template for. The map holds only
/// base values — no modifiers — so `map.value(key)` equals the base until a
/// live `update_attributes` or an equipment/effect modifier is folded in.
///
/// This is the input the physics movement seam consumes: `movement_speed` here
/// is the real per-type base (a zombie's `0.23`, a spider's `0.3`), not a
/// hand-picked constant.
#[must_use]
pub fn default_attributes(entity_type: &Identifier) -> Option<AttributeMap> {
    if entity_type.namespace() != "minecraft" {
        return None;
    }
    let spec = type_spec(entity_type.path())?;
    let mut map = AttributeMap::new();
    // Seed the template set, then apply the type's overrides in order. Using
    // `get_or_default` keeps each instance's clamp range from the registry.
    for (path, base) in template_bases(spec.template)
        .into_iter()
        .chain(spec.overrides.iter().copied())
    {
        if let Ok(key) = Identifier::from_str(&format!("minecraft:{path}")) {
            map.get_or_default(&key).set_base_value(base);
        }
    }
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Identifier {
        Identifier::from_str(s).unwrap()
    }

    #[test]
    fn base_value_only() {
        let inst = AttributeInstance::new(AttributeDef::new(0.1, 0.0, 1024.0));
        assert_eq!(inst.value(), 0.1);
    }

    #[test]
    fn three_stage_order_is_not_commutative_with_folding() {
        // base 0.1; +0.05 add_value -> 0.15; +100% add_multiplied_base (base*1.0)
        // -> 0.15 + 0.15 = 0.30; then *1.3 total -> 0.39.
        let mut inst = AttributeInstance::new(AttributeDef::new(0.1, 0.0, 1024.0));
        inst.add_or_update(Modifier::new(id("test:add"), 0.05, Operation::AddValue));
        inst.add_or_update(Modifier::new(
            id("test:mulbase"),
            1.0,
            Operation::AddMultipliedBase,
        ));
        inst.add_or_update(Modifier::new(
            id("test:multotal"),
            0.3,
            Operation::AddMultipliedTotal,
        ));
        let v = inst.value();
        assert!((v - 0.39).abs() < 1e-12, "got {v}");

        // Folding mul_base into mul_total would give 0.15 * 2.0 * 1.3 = 0.39 here
        // by coincidence; use a second add_value to break that. base after
        // add_value: 0.1 + 0.05 + 0.05 = 0.2; mul_base 100%: 0.2 + 0.2 = 0.4;
        // total *1.3 = 0.52. A folded computation using the *original* base 0.1
        // for mul_base would give 0.2 + 0.1 = 0.3, then *1.3 = 0.39 — different.
        inst.add_or_update(Modifier::new(id("test:add2"), 0.05, Operation::AddValue));
        let v2 = inst.value();
        assert!((v2 - 0.52).abs() < 1e-12, "got {v2}");
    }

    #[test]
    fn sprint_modifier_matches_physics_convention() {
        // movement_speed base 0.1, sprint is +0.3 add_multiplied_total -> 0.13.
        let mut inst = AttributeInstance::new(AttributeDef::new(0.1, 0.0, 1024.0));
        inst.add_or_update(Modifier::new(
            id("minecraft:sprinting"),
            0.3,
            Operation::AddMultipliedTotal,
        ));
        assert!((inst.value() - 0.13).abs() < 1e-12);
    }

    #[test]
    fn sanitize_clamps_and_maps_nan() {
        let def = AttributeDef::new(0.0, 0.0, 1.0);
        assert_eq!(def.sanitize(2.0), 1.0);
        assert_eq!(def.sanitize(-1.0), 0.0);
        assert_eq!(def.sanitize(f64::NAN), 0.0);
    }

    #[test]
    fn knockback_resistance_clamped_to_range() {
        let mut inst =
            AttributeInstance::new(default_def(&id("minecraft:knockback_resistance")).unwrap());
        inst.set_base_value(5.0);
        assert_eq!(inst.value(), 1.0); // max is 1.0
    }

    #[test]
    fn remove_modifier_invalidates_cache() {
        let mut inst = AttributeInstance::new(AttributeDef::new(1.0, 0.0, 100.0));
        inst.add_or_update(Modifier::new(id("t:m"), 1.0, Operation::AddMultipliedTotal));
        assert_eq!(inst.value(), 2.0);
        assert!(inst.remove(&id("t:m")));
        assert_eq!(inst.value(), 1.0);
    }

    #[test]
    fn operation_id_roundtrip() {
        for op in [
            Operation::AddValue,
            Operation::AddMultipliedBase,
            Operation::AddMultipliedTotal,
        ] {
            assert_eq!(Operation::from_id(op.id()), Some(op));
        }
        assert_eq!(Operation::from_id(3), None);
    }

    #[test]
    fn zombie_base_attributes_match_vanilla() {
        let map = default_attributes(&id("minecraft:zombie")).expect("zombie has a spec");
        // Explicit per-mob overrides.
        assert_eq!(map.value(&id("minecraft:movement_speed")), Some(0.23));
        assert_eq!(map.value(&id("minecraft:follow_range")), Some(35.0));
        assert_eq!(map.value(&id("minecraft:attack_damage")), Some(3.0));
        assert_eq!(map.value(&id("minecraft:armor")), Some(2.0));
        // Inherited living defaults the zombie does NOT override.
        assert_eq!(map.value(&id("minecraft:max_health")), Some(20.0));
        assert_eq!(map.value(&id("minecraft:step_height")), Some(0.6));
        assert_eq!(map.value(&id("minecraft:knockback_resistance")), Some(0.0));
    }

    #[test]
    fn pig_is_an_animal_without_attack_damage() {
        let map = default_attributes(&id("minecraft:pig")).expect("pig has a spec");
        assert_eq!(map.value(&id("minecraft:movement_speed")), Some(0.25));
        assert_eq!(map.value(&id("minecraft:max_health")), Some(10.0));
        // Animals get the mob follow_range (16), not the generic 32 default.
        assert_eq!(map.value(&id("minecraft:follow_range")), Some(16.0));
        assert_eq!(map.value(&id("minecraft:tempt_range")), Some(10.0));
        // Only monsters have attack_damage in their set.
        assert!(map.get(&id("minecraft:attack_damage")).is_none());
    }

    #[test]
    fn spider_and_animal_speed_overrides() {
        let spider = default_attributes(&id("minecraft:spider")).unwrap();
        assert_eq!(spider.value(&id("minecraft:max_health")), Some(16.0));
        assert_eq!(spider.value(&id("minecraft:movement_speed")), Some(0.3));

        for (ty, speed, hp) in [
            ("minecraft:cow", 0.2, 10.0),
            ("minecraft:sheep", 0.23, 8.0),
            ("minecraft:chicken", 0.25, 4.0),
            ("minecraft:creeper", 0.25, 20.0),
            ("minecraft:skeleton", 0.25, 20.0),
        ] {
            let map = default_attributes(&id(ty)).unwrap();
            assert_eq!(
                map.value(&id("minecraft:movement_speed")),
                Some(speed),
                "{ty} speed"
            );
            assert_eq!(map.value(&id("minecraft:max_health")), Some(hp), "{ty} hp");
        }
    }

    #[test]
    fn a_folded_sprint_modifier_uses_the_real_base() {
        // The physics seam consumes movement_speed; folding a sprint modifier
        // onto the real zombie base 0.23 gives 0.23 * 1.3 = 0.299, not the
        // generic-default 0.7 * 1.3.
        let mut map = default_attributes(&id("minecraft:zombie")).unwrap();
        map.get_mut(&id("minecraft:movement_speed"))
            .unwrap()
            .add_or_update(Modifier::new(
                id("minecraft:sprinting"),
                0.3,
                Operation::AddMultipliedTotal,
            ));
        let v = map.value(&id("minecraft:movement_speed")).unwrap();
        assert!((v - 0.299).abs() < 1e-9, "got {v}");
    }

    #[test]
    fn unknown_type_has_no_supplier() {
        assert!(default_attributes(&id("minecraft:item")).is_none());
        assert!(default_attributes(&id("modded:thing")).is_none());
    }

    /// Five species with landed, jar-cited goal rosters had **no**
    /// `type_spec` arm, so `default_attributes` returned `None` and every
    /// consumer fell through to `AttributeMap::value`'s registry default.
    ///
    /// **The symptom ran the wrong way from the obvious guess**, which is why
    /// this asserts exact values rather than plausibility: `movement_speed`'s
    /// registry default is **0.7** (`attribute.rs`'s `default_def` table), not
    /// zero and not a combat fallback. So an unlisted rabbit ran at 0.7 against
    /// its jar 0.3 — more than **twice too fast**, not sluggish. A mob moving
    /// at 0.7 reads as a pathfinding or interpolation defect, which is how this
    /// would have cost someone a day.
    ///
    /// Every number below is read from that species' own `createAttributes()`
    /// in `.cache/mc/26.2/`, not inferred from a sibling — the roster work
    /// found this family non-uniform in three separate ways, and hand-written
    /// tables have been wrong three times in this repo.
    #[test]
    fn species_with_rosters_have_jar_exact_bases_not_registry_defaults() {
        // `movement_speed`'s registry default, and therefore the value every one
        // of these species had before this fix. **Measured here rather than
        // asserted from a comment**: a bare `AttributeMap` is exactly what a
        // consumer holds when `default_attributes` answers `None`, so this is
        // the wrong value the fix removes, read from the same table the
        // production fallback reads. Writing `0.7` as a bare constant would
        // make the check below a claim about a number nothing verifies.
        let registry_default_speed = AttributeMap::new()
            .value(&id("minecraft:movement_speed"))
            .expect("movement_speed is a registry attribute, so a bare map answers its default");
        assert!(
            (registry_default_speed - 0.7).abs() < 1e-9,
            "the fallback this fix removes should be 0.7; got {registry_default_speed}. \
             If the registry default changed, the numbers below still stand — but the \
             claim about what the bug *was* needs rewriting."
        );

        // (type, jar movement_speed, jar max_health, jar cite)
        let cases: &[(&str, f64, f64, &str)] = &[
            ("rabbit", 0.3, 3.0, "animal/rabbit/Rabbit.java:292"),
            ("drowned", 0.23, 20.0, "monster/zombie/Drowned.java:81"),
            ("cave_spider", 0.3, 12.0, "monster/spider/CaveSpider.java:26"),
            ("zombie_villager", 0.23, 20.0, "monster/zombie/Zombie.java:131"),
            ("parched", 0.25, 16.0, "monster/skeleton/Parched.java:32"),
            // Second batch. Every one of these overrides
            // `movement_speed`, so all of them are separated from the 0.7
            // fallback by the assertion below. The one species that does
            // **not** override it — the ghast — is deliberately absent, and
            // has its own test: see `a_ghast_legitimately_moves_at_the_registry_default`.
            ("guardian", 0.5, 30.0, "monster/Guardian.java:85"),
            ("elder_guardian", 0.3, 80.0, "monster/ElderGuardian.java:35"),
            ("blaze", 0.23, 20.0, "monster/Blaze.java:54"),
            ("enderman", 0.3, 40.0, "monster/EnderMan.java:113"),
            ("snow_golem", 0.2, 4.0, "animal/golem/SnowGolem.java:63"),
            (
                "zombified_piglin",
                0.23,
                20.0,
                "monster/zombie/ZombifiedPiglin.java:80",
            ),
            ("bee", 0.3, 10.0, "animal/bee/Bee.java:528"),
            ("wolf", 0.3, 8.0, "animal/wolf/Wolf.java:216"),
        ];

        for &(ty, speed, health, cite) in cases {
            let map = default_attributes(&id(&format!("minecraft:{ty}")))
                .unwrap_or_else(|| panic!("{ty} must have a type_spec arm (#457)"));

            let got = map.value(&id("minecraft:movement_speed")).unwrap();
            assert!(
                (got - speed).abs() < 1e-9,
                "{ty} must move at {speed} per {cite}, got {got}"
            );
            assert!(
                (got - registry_default_speed).abs() > 1e-9,
                "{ty} measured exactly the {registry_default_speed} registry \
                 default, so it is still falling through type_spec rather than \
                 reading {cite}"
            );

            let got_health = map.value(&id("minecraft:max_health")).unwrap();
            assert!(
                (got_health - health).abs() < 1e-9,
                "{ty} must have {health} max health per {cite}, got {got_health}"
            );
        }
    }

    /// The `DROWNED` const duplicates `ZOMBIE`'s five rows because `overrides`
    /// is a `&'static [_]` with no const concatenation. This pins the two
    /// against each other so the copy cannot drift, and pins the one row that
    /// is *meant* to differ: a drowned wades, so vanilla gives it
    /// `STEP_HEIGHT 1.0` where every other mob inherits `0.6`.
    #[test]
    fn zombie_family_variants_share_their_parents_bases() {
        let zombie = default_attributes(&id("minecraft:zombie")).unwrap();
        let drowned = default_attributes(&id("minecraft:drowned")).unwrap();
        let villager = default_attributes(&id("minecraft:zombie_villager")).unwrap();

        for path in ["movement_speed", "follow_range", "attack_damage", "armor"] {
            let key = id(&format!("minecraft:{path}"));
            assert_eq!(
                drowned.value(&key),
                zombie.value(&key),
                "drowned's {path} must match zombie's — DROWNED has drifted from ZOMBIE"
            );
            assert_eq!(
                villager.value(&key),
                zombie.value(&key),
                "zombie_villager declares no createAttributes, so its {path} must match zombie's"
            );
        }

        let step = id("minecraft:step_height");
        assert_eq!(drowned.value(&step), Some(1.0), "Drowned.java:82");
        assert_eq!(zombie.value(&step), Some(0.6), "the inherited living default");

        // A zombified piglin is also `Zombie.createAttributes()`-derived, but
        // it is the one variant that genuinely *diverges*: two of its three
        // `add` calls restate the parent's numbers and only `ATTACK_DAMAGE`
        // changes. Pinned here rather than in
        // the loop above precisely because it must **not** match.
        let piglin = default_attributes(&id("minecraft:zombified_piglin")).unwrap();
        let damage = id("minecraft:attack_damage");
        assert_eq!(piglin.value(&damage), Some(5.0), "ZombifiedPiglin.java:84");
        assert_eq!(zombie.value(&damage), Some(3.0), "Zombie.java:134");
        for path in ["movement_speed", "follow_range", "armor"] {
            let key = id(&format!("minecraft:{path}"));
            assert_eq!(
                piglin.value(&key),
                zombie.value(&key),
                "a zombified piglin's {path} restates the parent's value, so it must match zombie's"
            );
        }
    }

    /// The structural half of the jar-exact-bases guarantee, and the one gate here that does not
    /// restate a name list: **every species any roster family claims must
    /// resolve to a `type_spec` arm.**
    ///
    /// A per-species case table (the test above) can only check the species
    /// somebody remembered to add to it, which is exactly how `type_spec` came
    /// to be missing fourteen arms while every test it had was green. This one
    /// is driven from `roster::*::SPECIES` — the same lists `goals_for`
    /// dispatches on — so adding a species to a family and forgetting its
    /// attributes fails here rather than shipping a mob that sprints at the
    /// 0.7 registry default.
    ///
    /// The control is the `is_empty` assertion: if the roster ever exports no
    /// species at all, this test would otherwise pass by iterating nothing.
    #[test]
    fn every_rostered_species_has_a_type_spec_arm() {
        use crate::ai::roster;

        let all: Vec<&str> = roster::hostile_melee::SPECIES
            .iter()
            .chain(roster::ranged::SPECIES)
            .chain(roster::passive::SPECIES)
            .chain(roster::neutral::SPECIES)
            .chain(roster::specialist::SPECIES)
            .copied()
            .collect();
        assert!(
            !all.is_empty(),
            "the roster exported no species, so this gate measured nothing"
        );

        let missing: Vec<&str> = all
            .iter()
            .copied()
            .filter(|s| default_attributes(&id(&format!("minecraft:{s}"))).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "these rostered species have no type_spec arm, so every attribute \
             they read falls back to the registry default (#457): {missing:?}"
        );
    }

    /// The one species in the table for which `movement_speed` **is** the
    /// registry default — and therefore the one that
    /// `species_with_rosters_have_jar_exact_bases_not_registry_defaults`
    /// structurally cannot cover, because its "still falling through"
    /// assertion is exactly "not 0.7".
    ///
    /// `Ghast.createAttributes()` overrides
    /// `flying_speed` and never `movement_speed`, because a ghast does not
    /// walk. So the fact that its ground speed reads 0.7 is *correct* and not
    /// a missing-arm bug — the two are distinguished by whether the whole spec
    /// resolves, which is what `default_attributes(...).is_some()` below
    /// measures. Without this test, giving a ghast an arm and giving it no arm
    /// would be indistinguishable on the attribute the other gate reads.
    #[test]
    fn a_ghast_legitimately_moves_at_the_registry_default() {
        let ghast = default_attributes(&id("minecraft:ghast"))
            .expect("ghast must have a type_spec arm (#457)");
        let registry_default_speed = AttributeMap::new()
            .value(&id("minecraft:movement_speed"))
            .unwrap();

        // `get`, not `value`: a ghast's `movement_speed` must be **present and
        // seeded at 0.7** by `createLivingAttributes`, not absent-and-answered
        // by `value`'s registry fallback. Those two are indistinguishable
        // through `value` — which is the whole reason a missing `type_spec`
        // arm was invisible — so the distinction has to be made here.
        let speed = ghast
            .get(&id("minecraft:movement_speed"))
            .expect("LivingEntity.createLivingAttributes adds MOVEMENT_SPEED to every mob");
        assert_eq!(
            speed.value(),
            registry_default_speed,
            "Ghast.createAttributes never adds MOVEMENT_SPEED, so it keeps the \
             living default — if this stops matching, either the registry table \
             changed or somebody invented a walk speed for a ghast"
        );
        // The values it *does* override, none of which a fall-through produces.
        assert_eq!(ghast.value(&id("minecraft:follow_range")), Some(100.0));
        assert_eq!(ghast.value(&id("minecraft:flying_speed")), Some(0.06));
        assert_eq!(ghast.value(&id("minecraft:camera_distance")), Some(8.0));
        assert_eq!(ghast.value(&id("minecraft:max_health")), Some(10.0));
    }

    /// `BaseTemplate::Mob` must contribute **neither** `attack_damage` nor
    /// `tempt_range` — the whole reason it is a third variant rather than a
    /// reuse of `Monster` (for the ghast, which `implements Enemy`) or
    /// `Animal` (for the snow golem, which is an `AbstractGolem`).
    ///
    /// This is the assertion that fails if someone later "tidies" a ghast into
    /// `BaseTemplate::Monster` on the strength of its `Enemy` interface: an
    /// `attack_damage` would appear that vanilla's ghast does not have.
    ///
    /// **Absence is measured with [`AttributeMap::get`], never
    /// [`AttributeMap::value`].** `value` falls back to
    /// [`default_def`]`(key).default` for any registry-known attribute, so a
    /// ghast with no `attack_damage` in its set still answers `Some(2.0)`
    /// through it. The first draft of this test asserted `value(...) == None`
    /// and failed for exactly that reason — the same fallback that made a
    /// missing `type_spec` arm invisible in the first place, met a second time one layer up. Anything
    /// asking "does this type *have* this attribute" has to go through `get`.
    #[test]
    fn a_bare_mob_template_adds_neither_attack_damage_nor_tempt_range() {
        let damage = id("minecraft:attack_damage");
        let tempt = id("minecraft:tempt_range");

        for ty in ["ghast", "snow_golem"] {
            let map = default_attributes(&id(&format!("minecraft:{ty}"))).unwrap();
            assert!(
                map.get(&damage).is_none(),
                "{ty} builds on Mob.createMobAttributes(), which never adds ATTACK_DAMAGE"
            );
            assert!(
                map.get(&tempt).is_none(),
                "{ty} builds on Mob.createMobAttributes(), which never adds TEMPT_RANGE"
            );
        }

        // Controls that the two absences above are a property of the template
        // and not of `get` answering `None` for everything: the same two keys,
        // read the same way, must be present on the templates that do add them.
        let monster = default_attributes(&id("minecraft:blaze")).unwrap();
        assert!(
            monster.get(&damage).is_some(),
            "a Monster template must carry attack_damage — if this is None the \
             absences above prove nothing"
        );
        let animal = default_attributes(&id("minecraft:cow")).unwrap();
        assert!(
            animal.get(&tempt).is_some(),
            "an Animal template must carry tempt_range"
        );
    }

    /// The Depth Strider path, end to end through the wire-shaped conversion:
    /// a `water_movement_efficiency` snapshot carrying a `Depth Strider III`
    /// -style `AddValue` modifier (`+1.0`, from three stacked
    /// `0.33333334`-per-level modifiers, matching
    /// `data/minecraft/enchantment/depth_strider.json`'s
    /// `per_level_above_first` — see `.cache/mc/26.2/src`) alongside a
    /// multiplied-base and a multiplied-total modifier, chosen so the two
    /// multiplicative stages cannot coincide (two `AddMultipliedBase`
    /// modifiers whose amounts have a nonzero product against one
    /// `AddMultipliedTotal`, per the task's evidence standard: a single
    /// modifier in each multiplicative stage is a genuine trap here, because
    /// `base*(1+a)*(1+b)` is the same product regardless of which stage `a`
    /// and `b` are assigned to).
    ///
    /// Hand-computed (also cross-checked in Python, not just by inspection):
    /// `base = 0.0 + 0.33333334 + 0.1 = 0.43333334` (the second `add_value`
    /// modifier is a synthetic stand-in for some other effect that also
    /// touches this attribute, not a real vanilla source — its only job is
    /// to make `base` an odd enough number that a stage-order bug cannot
    /// hide behind a round one).
    /// `mulbase stage: 0.43333334 * (1 + 0.5 + 0.25) = 0.43333334 * 1.75
    /// = 0.7583333450`.
    /// `multotal stage: 0.7583333450 * 1.2 = 0.9100000140`.
    ///
    /// A build that assigns the wire operation bytes to the wrong enum
    /// variant (e.g. `from_id` swapping `1`/`2`) would instead run the one
    /// `AddMultipliedTotal`-intended modifier through the mulbase stage and
    /// the two `AddMultipliedBase`-intended modifiers through the multotal
    /// stage: `0.43333334 * 1.2 = 0.5200000080`, then
    /// `* 1.5 * 1.25 = 0.9750000150` — a different, still-in-range number
    /// (`0.975` vs `0.910`), so this test can tell the two apart.
    #[test]
    fn water_movement_efficiency_folds_through_the_wire_snapshot() {
        use lodestone_model::EntityAttributeModifier;

        let snapshot = EntityAttributeSnapshot {
            attribute: id("minecraft:water_movement_efficiency"),
            base: 0.0,
            modifiers: vec![
                // Depth Strider III: three `+0.33333334` AddValue stacks.
                EntityAttributeModifier {
                    id: id("minecraft:enchantment.depth_strider"),
                    amount: 0.33333334,
                    operation: Operation::AddValue.id(),
                },
                // Synthetic second add_value so `base` isn't a round number
                // a stage-order bug could coincidentally still match.
                EntityAttributeModifier {
                    id: id("test:synthetic_add"),
                    amount: 0.1,
                    operation: Operation::AddValue.id(),
                },
                EntityAttributeModifier {
                    id: id("test:mulbase1"),
                    amount: 0.5,
                    operation: Operation::AddMultipliedBase.id(),
                },
                EntityAttributeModifier {
                    id: id("test:mulbase2"),
                    amount: 0.25,
                    operation: Operation::AddMultipliedBase.id(),
                },
                EntityAttributeModifier {
                    id: id("test:multotal"),
                    amount: 0.2,
                    operation: Operation::AddMultipliedTotal.id(),
                },
            ],
        };

        let v = attribute_value(&[snapshot], &id("minecraft:water_movement_efficiency"));
        assert!((v - 0.9100000140).abs() < 1e-6, "got {v}");
        // The would-be-swapped-order value must not be what we got.
        assert!((v - 0.9750000150).abs() > 1e-3, "got {v}, indistinguishable from the swapped-order bug");
    }

    #[test]
    fn attribute_value_falls_back_to_registry_default_when_absent() {
        // No boots, no enchantment: the server has never sent an explicit
        // `water_movement_efficiency` snapshot for this entity, and absence
        // must read as "still the default", matching `RangedAttribute`'s own
        // default (`Attributes.WATER_MOVEMENT_EFFICIENCY`'s registration, `0.0`),
        // not as a hard zero baked into the caller.
        let v = attribute_value(&[], &id("minecraft:water_movement_efficiency"));
        assert_eq!(v, 0.0);

        // An unknown attribute id has no registry default either.
        let v = attribute_value(&[], &id("minecraft:not_a_real_attribute"));
        assert_eq!(v, 0.0);
    }

    #[test]
    fn instance_from_snapshot_drops_unrecognized_operation_bytes() {
        use lodestone_model::EntityAttributeModifier;

        let snapshot = EntityAttributeSnapshot {
            attribute: id("minecraft:water_movement_efficiency"),
            base: 0.2,
            modifiers: vec![EntityAttributeModifier {
                id: id("test:garbage"),
                amount: 99.0,
                operation: 200,
            }],
        };
        // Doesn't panic, and the unrecognized modifier contributes nothing.
        let instance = instance_from_snapshot(&snapshot);
        assert_eq!(instance.value(), 0.2);
    }

    #[test]
    fn water_movement_efficiency_key_matches_the_registry_id() {
        assert_eq!(
            water_movement_efficiency_key(),
            id("minecraft:water_movement_efficiency")
        );
        assert!(default_def(&water_movement_efficiency_key()).is_some());
    }

    #[test]
    fn movement_speed_key_matches_the_registry_id() {
        assert_eq!(movement_speed_key(), id("minecraft:movement_speed"));
        // The *generic* `RangedAttribute` default (0.7, `Attributes.MOVEMENT_SPEED`'s
        // registration) — a player's own base is 0.1 (`Player.createAttributes()`),
        // supplied by the wire snapshot once the server sends one, not by this
        // table. This table's job is the fallback + clamp range only.
        assert_eq!(default_def(&movement_speed_key()).unwrap().default, 0.7);
    }

    /// The `movement_speed` counterpart of `water_movement_efficiency_folds_
    /// through_the_wire_snapshot`: a Speed-II-shaped `ADD_MULTIPLIED_TOTAL`
    /// modifier on a player's own `0.1` base folds to `0.1 * 1.4 = 0.14`,
    /// through the same wire-shaped conversion Depth Strider uses. This is the
    /// value `lodestone_ecs::player::player_physics` would hand to
    /// `PlayerState::with_movement_speed` (combined with the local sprint
    /// multiply on top, per this module's own `movement_speed_key` docs) —
    /// proving the fold this crate owns produces the right number before any
    /// caller outside this crate touches it.
    #[test]
    fn movement_speed_folds_a_speed_ii_modifier_onto_the_player_base() {
        use lodestone_model::EntityAttributeModifier;

        let snapshot = EntityAttributeSnapshot {
            attribute: id("minecraft:movement_speed"),
            base: 0.1, // Player.createAttributes().add(MOVEMENT_SPEED, 0.1F)
            modifiers: vec![EntityAttributeModifier {
                id: id("minecraft:effect.speed"),
                // MobEffects.SPEED: +0.2F per level, amplifier 1 = Speed II.
                amount: f64::from(0.2f32) * 2.0,
                operation: Operation::AddMultipliedTotal.id(),
            }],
        };

        let v = attribute_value(&[snapshot], &id("minecraft:movement_speed"));
        assert!((v - 0.14).abs() < 1e-9, "got {v}");
    }

    #[test]
    fn instance_from_snapshot_uses_the_reported_base_not_the_registry_default() {
        use lodestone_model::EntityAttributeModifier;

        // The wire base (0.5) must win over the registry default (0.0) —
        // the snapshot is authoritative once the server has sent one.
        let snapshot = EntityAttributeSnapshot {
            attribute: id("minecraft:water_movement_efficiency"),
            base: 0.5,
            modifiers: Vec::<EntityAttributeModifier>::new(),
        };
        assert_eq!(instance_from_snapshot(&snapshot).value(), 0.5);
    }
}
