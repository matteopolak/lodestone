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

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

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
}
