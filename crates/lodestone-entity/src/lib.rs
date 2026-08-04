//! Version-free entity layer: tracking, metadata, attributes, mob AI and
//! navigation.
//!
//! `lodestone-entity` sits above the world and physics crates and below the
//! client. Like its peers it is **version-free**: every version-specific
//! decision (metadata indices, entity registry ids, per-mob dimensions, block
//! semantics for pathfinding) enters through a trait or schema seam that a
//! version crate fills in, exactly as `lodestone-physics` takes a
//! `CollisionView`-style seam and `lodestone-world` takes a `LongArrayFraming`
//! knob.
//!
//! # Layout
//!
//! * [`entity`] — the [`EntityTracker`] store and packet-driven lifecycle.
//! * [`interpolation`] — the 20 Hz-to-render blending seam.
//! * [`metadata`] — version-free metadata values plus the version schema seam.
//! * [`attribute`] — vanilla's attribute arithmetic and default table.
//! * [`pathfinding`] — vanilla's A* (`PathFinder`, `WalkNodeEvaluator`, `Path`)
//!   over the [`PathWorld`](pathfinding::PathWorld) seam.
//! * [`ai`] — the [`GoalSelector`](ai::GoalSelector) scheduler and a
//!   representative set of goals.
//! * [`brain`] — vanilla's *other* mob-AI system: the memory-driven,
//!   activity-scheduled [`Brain`](brain::Brain) used by newer mobs.
//! * [`spawn`] — version-free mob-category rules and vanilla's despawn decision.
//! * [`pose`] — per-entity pose/animation render state (the renderer seam).
//! * [`projectile`] — ballistic (non-mob) projectile trajectories, plus
//!   [`ProjectileRegistry`](projectile::ProjectileRegistry), the per-tick
//!   driver a server owns to advance many of them at once.
//! * [`item_entity`] — dropped-item fall dynamics and lifecycle
//!   (age/pickup/merge), plus
//!   [`ItemEntityRegistry`](item_entity::ItemEntityRegistry), the per-tick
//!   driver a server owns to advance despawn/merge across many of them.
//! * [`damage`] — the damage-reduction pipeline and invulnerability-frame gate.
//! * [`explosion`] — ray-sampled blast exposure, damage and knockback power.

pub mod ai;
pub mod attribute;
pub mod brain;
pub mod damage;
pub mod entity;
pub mod explosion;
pub mod interpolation;
pub mod item_entity;
pub mod metadata;
pub mod pathfinding;
pub mod pose;
pub mod projectile;
pub mod spawn;

pub use attribute::{AttributeDef, AttributeInstance, AttributeMap, Modifier, Operation};
pub use brain::{
    Activity, Behavior, BehaviorControl, Brain, BrainMob, GateBehavior, Memories, MemoryModuleType,
    MemoryStatus, MemoryValue, OrderPolicy, RunningPolicy, Sensor, WalkTarget,
};
pub use damage::{
    DamageFlags, DamageOutcome, Defenses, HurtCooldown, HurtDecision, apply_reductions,
};
pub use entity::{EntityKind, EntityState, EntityTracker};
pub use explosion::{Aabb, OpenAir, RayView, entity_damage, knockback_power, seen_percent};
pub use interpolation::Interpolated;
pub use item_entity::{ItemEntityRegistry, ItemLifecycle, ItemMotion, TrackedItem, try_merge};
pub use metadata::{EntityMetadata, MetadataSchema, MetadataValue, SharedEntityFlags};
pub use pose::{EntityPose, RenderPose, WalkAnimation};
pub use projectile::{DragProfile, IntegrationOrder, Projectile, ProjectileRegistry, TrackedProjectile};
pub use spawn::{
    DespawnCtx, DespawnDecision, MobCategory, SpawnConditions, check_despawn, mob_cap,
};
