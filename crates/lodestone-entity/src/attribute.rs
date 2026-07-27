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

use lodestone_model::Identifier;
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

    /// Whether a modifier with `id` is present.
    #[must_use]
    pub fn has_modifier(&self, id: &Identifier) -> bool {
        self.modifiers.contains_key(id)
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

/// The base-class attribute template a concrete entity type is built on,
/// mirroring vanilla's `createLivingAttributes` → `createMobAttributes` →
/// `createMonsterAttributes` / `createAnimalAttributes` chain.
///
/// Both variants extend `Mob.createMobAttributes()` (living + `follow_range`
/// 16), which is the shared prefix in [`template_bases`]; they differ only in
/// the one attribute their subclass adds. (A bare-`Mob` type such as a bat or
/// squid would need a third variant; none of the currently-rendered mobs are
/// bare mobs.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseTemplate {
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
    let spec = match path {
        "zombie" | "husk" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: ZOMBIE,
        },
        "skeleton" | "stray" | "wither_skeleton" | "bogged" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[("movement_speed", 0.25)],
        },
        "creeper" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[("movement_speed", 0.25)],
        },
        "spider" => TypeSpec {
            template: BaseTemplate::Monster,
            overrides: &[("max_health", 16.0), ("movement_speed", 0.3)],
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
}
