//! Memory modules: the shared blackboard a brain's behaviours read and write.
//!
//! Where the [`GoalSelector`](crate::ai::GoalSelector) coordinates goals through
//! mutually-exclusive flags, the Brain system coordinates behaviours through
//! **memory**. A behaviour declares which memories it needs present or absent to
//! run, produces or consumes values, and the emergent mutual exclusion (only one
//! behaviour writes `WALK_TARGET` at a time, and `MoveToTargetSink` refuses to
//! start while a path exists) replaces the flag machinery entirely.
//!
//! This is a faithful reproduction of vanilla's `MemorySlot` /
//! `MemoryModuleType` / `MemoryStatus`, including the two details that are easy
//! to get wrong:
//!
//! * A memory can only be *set* if it was first **registered** (vanilla
//!   registers a slot for every memory any behaviour requires). Setting an
//!   unregistered memory is silently a no-op — see [`Memories::set`].
//! * Expiry is checked *before* the decrement: a value set with a time-to-live
//!   of `n` survives `n` ticks and is cleared on the `n + 1`-th
//!   ([`Memories::tick`]).

use lodestone_model::Vec3;
use std::collections::HashMap;

/// A version-free key identifying a kind of memory.
///
/// Vanilla's `MemoryModuleType<T>` is a registry entry carrying its value type;
/// here the key is a stable string and the value type is enforced dynamically by
/// [`MemoryValue`]. The core vanilla modules are provided as associated
/// constants; a version crate may mint additional keys with [`MemoryModuleType::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MemoryModuleType(&'static str);

impl MemoryModuleType {
    /// Mints a memory key from a stable name.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The stable name of this memory module.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.0
    }

    /// Where the mob wants to walk (a [`WalkTarget`]).
    pub const WALK_TARGET: Self = Self("walk_target");
    /// Where the mob wants to look (a world position).
    pub const LOOK_TARGET: Self = Self("look_target");
    /// The current attack target's entity id.
    pub const ATTACK_TARGET: Self = Self("attack_target");
    /// The nearest visible player's position.
    pub const NEAREST_VISIBLE_PLAYER: Self = Self("nearest_visible_player");
    /// All nearby visible living entities (their ids).
    pub const NEAREST_VISIBLE_LIVING_ENTITIES: Self = Self("nearest_visible_living_entities");
    /// A marker present while the mob is panicking.
    pub const IS_PANICKING: Self = Self("is_panicking");
    /// The position of whatever last hurt the mob.
    pub const HURT_BY: Self = Self("hurt_by");
    /// The nearest hostile entity's id, from
    /// [`super::sensor::NearestHostileSensor`] — vanilla's `NEAREST_HOSTILE`,
    /// what `VillagerPanicTrigger.hasHostile` (and, more generally, any
    /// target-acquisition behaviour that needs "what is the closest thing I
    /// should be worried about") reads.
    pub const NEAREST_HOSTILE: Self = Self("nearest_hostile");
    /// Registered-but-usually-empty marker used by `MoveToTargetSink` to record
    /// how long a walk target has been unreachable.
    pub const CANT_REACH_WALK_TARGET_SINCE: Self = Self("cant_reach_walk_target_since");
    /// Present while the mob holds a computed path (a marker here).
    pub const PATH: Self = Self("path");
    /// The point a goat's ram-attack charges toward — `GoatAi.RAM`'s
    /// `RamTarget`/`PrepareRamNearestTarget` share this exact memory, the
    /// former reading it as its walk destination and the latter writing it
    /// once the prepare timer elapses.
    pub const RAM_TARGET: Self = Self("ram_target");
    /// A presence-only marker (with a TTL) blocking a new ram attempt —
    /// vanilla's `RAM_COOLDOWN_TICKS`, an `Integer` counted down every tick by
    /// a `CountDownCooldownTicks` core behaviour there. This crate's
    /// [`Memories`] TTL mechanism ([`Memories::set_with_expiry`]) already
    /// performs that countdown-and-auto-clear, so no separate ticking
    /// behaviour is needed to reproduce it.
    pub const RAM_COOLDOWN_TICKS: Self = Self("ram_cooldown_ticks");
}

/// The presence requirement a behaviour places on a memory.
///
/// Faithful to vanilla `MemoryStatus`. Note that *all* checks are `false` when
/// the slot is unregistered, so even `ValueAbsent` requires the slot to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryStatus {
    /// The slot exists (registered), regardless of whether it holds a value.
    Registered,
    /// The slot exists and holds a value.
    ValuePresent,
    /// The slot exists and is empty.
    ValueAbsent,
}

/// A concrete value stored in a memory slot.
///
/// Vanilla's memory map is heterogeneous (`MemoryModuleType<T> -> T`); Rust has
/// no such map, so the value types a brain actually uses are enumerated here.
/// This keeps memory fully inspectable and comparable in tests — which the
/// project's testing philosophy prizes over an opaque `Any`.
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryValue {
    /// A registered-present marker with no payload (e.g. `IS_PANICKING`).
    Unit,
    /// A boolean flag.
    Bool(bool),
    /// An integer (timestamps, counters).
    Int(i64),
    /// A world position (`LOOK_TARGET`, `NEAREST_VISIBLE_PLAYER`, `HURT_BY`).
    Pos(Vec3),
    /// An entity id (`ATTACK_TARGET`).
    Entity(i32),
    /// A walk destination with a speed and stop distance.
    WalkTarget(WalkTarget),
    /// A list of entity ids (`NEAREST_VISIBLE_LIVING_ENTITIES`). An empty list
    /// is treated as no value, matching vanilla's `isEmptyCollection`.
    Entities(Vec<i32>),
}

impl MemoryValue {
    /// Whether this value is an empty collection, which vanilla coerces to "no
    /// value" on set.
    fn is_empty_collection(&self) -> bool {
        matches!(self, MemoryValue::Entities(v) if v.is_empty())
    }
}

/// A walk destination: where to go, how fast, and how close counts as arrived.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalkTarget {
    /// The destination in world space.
    pub pos: Vec3,
    /// The speed modifier applied to the mob's movement speed.
    pub speed: f32,
    /// The block distance within which the target is considered reached.
    pub close_enough: i32,
}

impl WalkTarget {
    /// Builds a walk target.
    #[must_use]
    pub const fn new(pos: Vec3, speed: f32, close_enough: i32) -> Self {
        Self {
            pos,
            speed,
            close_enough,
        }
    }
}

/// A single memory slot: an optional value with an optional time-to-live.
#[derive(Debug, Clone)]
struct MemorySlot {
    value: Option<MemoryValue>,
    /// Ticks remaining; [`i64::MAX`] means the value never expires.
    ttl: i64,
}

impl MemorySlot {
    const NEVER: i64 = i64::MAX;

    fn create() -> Self {
        Self {
            value: None,
            ttl: Self::NEVER,
        }
    }

    fn has_value(&self) -> bool {
        self.value.is_some()
    }

    fn can_expire(&self) -> bool {
        self.ttl != Self::NEVER
    }

    fn has_expired(&self) -> bool {
        self.ttl <= 0
    }

    fn set(&mut self, value: MemoryValue, ttl: i64) {
        self.value = Some(value);
        self.ttl = ttl;
    }

    fn clear(&mut self) {
        self.value = None;
        self.ttl = Self::NEVER;
    }

    fn tick(&mut self) {
        if self.has_value() && self.can_expire() {
            if self.has_expired() {
                self.clear();
            } else {
                self.ttl -= 1;
            }
        }
    }
}

/// The mob's memory map: the set of registered slots and their values.
///
/// A behaviour receives `&mut Memories` and interacts only through this type,
/// never touching another behaviour directly. That indirection is the whole
/// point of the architecture.
#[derive(Debug, Clone, Default)]
pub struct Memories {
    slots: HashMap<MemoryModuleType, MemorySlot>,
}

impl Memories {
    /// A fresh, empty memory map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a slot for `ty` if one does not already exist. Only registered
    /// memories can hold values.
    pub fn register(&mut self, ty: MemoryModuleType) {
        self.slots.entry(ty).or_insert_with(MemorySlot::create);
    }

    /// Whether a slot exists for `ty`.
    #[must_use]
    pub fn is_registered(&self, ty: MemoryModuleType) -> bool {
        self.slots.contains_key(&ty)
    }

    /// Sets `ty` to `value` with no expiry. **No-op if `ty` is unregistered**,
    /// exactly as vanilla's `setMemoryInternal`. An empty-collection value
    /// clears the slot.
    pub fn set(&mut self, ty: MemoryModuleType, value: MemoryValue) {
        self.set_with_expiry_opt(ty, value, MemorySlot::NEVER);
    }

    /// Sets `ty` to `value`, cleared automatically after `ttl` ticks (survives
    /// `ttl` ticks, cleared on the `ttl + 1`-th). No-op if unregistered.
    pub fn set_with_expiry(&mut self, ty: MemoryModuleType, value: MemoryValue, ttl: i64) {
        self.set_with_expiry_opt(ty, value, ttl);
    }

    fn set_with_expiry_opt(&mut self, ty: MemoryModuleType, value: MemoryValue, ttl: i64) {
        if let Some(slot) = self.slots.get_mut(&ty) {
            if value.is_empty_collection() {
                slot.clear();
            } else {
                slot.set(value, ttl);
            }
        }
    }

    /// Sets `ty` from an optional value, clearing it when `None` (vanilla's
    /// `setOrErase`). No-op if unregistered.
    pub fn set_or_erase(&mut self, ty: MemoryModuleType, value: Option<MemoryValue>) {
        match value {
            Some(v) => self.set(ty, v),
            None => self.erase(ty),
        }
    }

    /// Clears the value in `ty`'s slot, leaving the slot registered. No-op if
    /// unregistered.
    pub fn erase(&mut self, ty: MemoryModuleType) {
        if let Some(slot) = self.slots.get_mut(&ty) {
            slot.clear();
        }
    }

    /// The current value of `ty`, if the slot exists and holds one.
    #[must_use]
    pub fn get(&self, ty: MemoryModuleType) -> Option<&MemoryValue> {
        self.slots.get(&ty).and_then(|s| s.value.as_ref())
    }

    /// Whether `ty` holds a value right now.
    #[must_use]
    pub fn has_value(&self, ty: MemoryModuleType) -> bool {
        self.check(ty, MemoryStatus::ValuePresent)
    }

    /// Ticks remaining before `ty` expires, or [`i64::MAX`] if it never will.
    /// Zero if unregistered.
    #[must_use]
    pub fn time_until_expiry(&self, ty: MemoryModuleType) -> i64 {
        self.slots.get(&ty).map_or(0, |s| s.ttl)
    }

    /// Evaluates a `(memory, status)` requirement. Always `false` for an
    /// unregistered slot, even for [`MemoryStatus::ValueAbsent`].
    #[must_use]
    pub fn check(&self, ty: MemoryModuleType, status: MemoryStatus) -> bool {
        match self.slots.get(&ty) {
            None => false,
            Some(slot) => match status {
                MemoryStatus::Registered => true,
                MemoryStatus::ValuePresent => slot.has_value(),
                MemoryStatus::ValueAbsent => !slot.has_value(),
            },
        }
    }

    /// Advances every slot's expiry by one tick (vanilla's
    /// `forgetOutdatedMemories`). Run first, before sensors and behaviours.
    pub fn tick(&mut self) {
        for slot in self.slots.values_mut() {
            slot.tick();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_is_a_noop_when_unregistered() {
        let mut m = Memories::new();
        m.set(MemoryModuleType::IS_PANICKING, MemoryValue::Unit);
        assert!(!m.has_value(MemoryModuleType::IS_PANICKING));
        assert!(!m.check(MemoryModuleType::IS_PANICKING, MemoryStatus::Registered));
    }

    #[test]
    fn value_absent_is_false_until_registered() {
        let mut m = Memories::new();
        // Unregistered: even ValueAbsent is false.
        assert!(!m.check(MemoryModuleType::WALK_TARGET, MemoryStatus::ValueAbsent));
        m.register(MemoryModuleType::WALK_TARGET);
        // Registered and empty: now ValueAbsent holds.
        assert!(m.check(MemoryModuleType::WALK_TARGET, MemoryStatus::ValueAbsent));
        assert!(!m.check(MemoryModuleType::WALK_TARGET, MemoryStatus::ValuePresent));
    }

    #[test]
    fn expiry_clears_on_the_tick_after_reaching_zero() {
        let mut m = Memories::new();
        m.register(MemoryModuleType::LOOK_TARGET);
        m.set_with_expiry(
            MemoryModuleType::LOOK_TARGET,
            MemoryValue::Pos(Vec3::new(1.0, 2.0, 3.0)),
            2,
        );
        // ttl=2 -> tick -> 1 (still present)
        m.tick();
        assert!(m.has_value(MemoryModuleType::LOOK_TARGET));
        assert_eq!(m.time_until_expiry(MemoryModuleType::LOOK_TARGET), 1);
        // ttl=1 -> tick -> 0 (still present)
        m.tick();
        assert!(m.has_value(MemoryModuleType::LOOK_TARGET));
        assert_eq!(m.time_until_expiry(MemoryModuleType::LOOK_TARGET), 0);
        // ttl=0 -> tick -> expired, cleared
        m.tick();
        assert!(!m.has_value(MemoryModuleType::LOOK_TARGET));
    }

    #[test]
    fn ttl_zero_is_cleared_on_first_tick() {
        let mut m = Memories::new();
        m.register(MemoryModuleType::LOOK_TARGET);
        m.set_with_expiry(
            MemoryModuleType::LOOK_TARGET,
            MemoryValue::Pos(Vec3::default()),
            0,
        );
        assert!(m.has_value(MemoryModuleType::LOOK_TARGET));
        m.tick();
        assert!(!m.has_value(MemoryModuleType::LOOK_TARGET));
    }

    #[test]
    fn empty_collection_coerces_to_no_value() {
        let mut m = Memories::new();
        m.register(MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES);
        m.set(
            MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES,
            MemoryValue::Entities(vec![]),
        );
        assert!(!m.has_value(MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES));
        m.set(
            MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES,
            MemoryValue::Entities(vec![7]),
        );
        assert!(m.has_value(MemoryModuleType::NEAREST_VISIBLE_LIVING_ENTITIES));
    }
}
