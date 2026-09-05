//! Generational handles for objects a Java plugin holds across ticks.
//!
//! # What it is
//!
//! A slot map. [`ObjectRegistry`] hands out [`ObjectRef`]s — a slot index plus
//! a generation — and resolves them back to whatever the host stored (an ECS
//! `Entity`, a chunk key, a block position). A ref whose subject has gone away
//! resolves to [`ResolveError::Stale`] and never to somebody else's object.
//!
//! # The problem this exists for
//!
//! Bukkit plugins keep `Player`, `World` and `Block` references in fields, in
//! maps, in scheduled tasks, and dereference them arbitrarily later. The entity
//! behind one can despawn, disconnect or unload in between. Three things must
//! all be true:
//!
//! - a stale reference must **fail**, not dangle;
//! - it must fail **distinguishably**, so the bridge can raise the exception
//!   Bukkit semantics call for rather than returning a plausible wrong answer;
//! - and a slot reused by a later object must **not** silently answer for the
//!   old one.
//!
//! The third is the one a plain index gets wrong, and it gets it wrong in the
//! worst possible way: a plugin holding a ref to a logged-out player would
//! start operating on whoever next occupied that slot — a correctness bug that
//! looks like a permissions bug. The generation counter is what closes it.
//! [`ObjectRegistry::release`] bumps the slot's generation, so every ref minted
//! before the release compares unequal to every ref minted after it, and the
//! comparison is a `u32` rather than a lookup.
//!
//! # How it works
//!
//! Slots are reused (a busy server churns entities, and growing forever is not
//! an option), but a slot's generation only ever increases. A resolve checks
//! the ref's generation against the slot's; a mismatch is [`ResolveError::Stale`]
//! and an empty slot is the same error, because from a plugin's point of view
//! they are the same event.
//!
//! `generation` is deliberately `u32` and **saturating**, not wrapping. At
//! wrap-around a wrapped generation would start matching ancient refs again —
//! reintroducing exactly the bug this type exists to prevent, but only after
//! four billion reuses of one slot, which is precisely the kind of defect that
//! ships. A saturated slot is retired instead ([`Slot::retired`]): it is never
//! handed out again, costing one slot rather than correctness.
//!
//! # How to change it
//!
//! - **Do not make `ObjectRef` constructible outside this module.** Its fields
//!   are private so a plugin cannot forge one by arithmetic; a JNI-side handle
//!   is an opaque `jlong` obtained from [`ObjectRef::to_bits`] and returned
//!   through [`ObjectRef::from_bits`], which round-trips but proves nothing
//!   about validity — resolution does that.
//! - **Do not silently return `None` for a stale ref where Bukkit throws.**
//!   The distinction between "the entity is gone" and "you passed nonsense" is
//!   the difference between the exception a plugin expects and one it does not.
//!
//! # Dependencies
//!
//! `std` only. Generic over the payload so the host decides what a ref points
//! at; [`ObjectKind`] is carried alongside so a `Player` handle used where a
//! `Block` is expected is caught at the boundary rather than by a type
//! confusion deeper in.

use std::collections::HashMap;

/// Default maximum number of live object handles in one registry.
///
/// The default is deliberately finite: a plugin must not be able to turn a
/// long-running server into an unbounded host-side allocation. Hosts with a
/// smaller, measured budget can use [`ObjectRegistry::with_capacity`].
pub const DEFAULT_OBJECT_REGISTRY_CAPACITY: usize = 1024;

/// What a handle refers to.
///
/// Carried inside the ref itself so that a mixed-up handle is rejected at the
/// JNI boundary. Java's own type system cannot help here — every handle
/// crosses as a `jlong`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectKind {
    /// A player, live or not.
    Player,
    /// A non-player entity.
    Entity,
    /// A world/dimension.
    World,
    /// A block position within a world.
    Block,
    /// A block entity (chest, sign, furnace).
    BlockEntity,
    /// An inventory or container view.
    Inventory,
}

/// An opaque, forgery-resistant handle to a host object.
///
/// Crosses the JNI boundary as a `jlong` via [`Self::to_bits`] /
/// [`Self::from_bits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectRef {
    index: u32,
    generation: u32,
    kind: ObjectKind,
}

impl ObjectRef {
    /// What this handle refers to.
    #[must_use]
    pub const fn kind(self) -> ObjectKind {
        self.kind
    }

    /// Pack into the `jlong` a JNI call carries.
    ///
    /// The kind is *not* packed in: it is recovered from the registry's slot on
    /// resolution, so a plugin cannot change a handle's kind by editing the
    /// bits it was given. Packing it would make the kind claim
    /// caller-controlled, which defeats the check it exists for.
    #[must_use]
    pub const fn to_bits(self) -> i64 {
        ((self.index as i64) << 32) | (self.generation as i64 & 0xFFFF_FFFF)
    }

    /// Unpack a handle received from Java.
    ///
    /// Round-trips [`Self::to_bits`] and **proves nothing else** — an arbitrary
    /// `jlong` produces a well-formed `ObjectRef` that resolution will reject.
    /// `kind` is what the *call site* expects; resolution checks it against
    /// what the registry actually holds.
    #[must_use]
    pub const fn from_bits(bits: i64, kind: ObjectKind) -> Self {
        // Both truncations are intended: the two `u32` halves are packed into
        // one `i64` by `to_bits`, and masking back out is the inverse.
        let index = (bits >> 32) as u32;
        let generation = (bits & 0xFFFF_FFFF) as u32;
        Self {
            index,
            generation,
            kind,
        }
    }
}

/// Why an [`ObjectRef`] did not resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// The object is gone: despawned, disconnected, unloaded — or the slot was
    /// reused by something newer. A plugin holding this ref should see the
    /// exception Bukkit raises for a dead reference.
    Stale,
    /// The handle resolved, but to a different kind of object than the call
    /// site expected — a `Block` handle passed where a `Player` was wanted.
    /// Distinct from [`Self::Stale`] because it is a *plugin* bug rather than
    /// an ordinary lifetime event, and reporting it as staleness would send
    /// the author looking in the wrong place.
    KindMismatch {
        /// What the call site expected.
        expected: ObjectKind,
        /// What the registry actually holds.
        actual: ObjectKind,
    },
    /// The index is past the end of the registry — a forged or corrupted
    /// handle rather than an expired one.
    OutOfRange,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stale => f.write_str("the referenced object no longer exists"),
            Self::KindMismatch { expected, actual } => {
                write!(f, "expected a {expected:?} handle, got a {actual:?} handle")
            }
            Self::OutOfRange => f.write_str("handle does not name a slot in this registry"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Why a new object handle could not be minted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectRegistryError {
    /// The registry has reached its configured live-object bound.
    CapacityExceeded {
        /// The maximum number of live objects this registry accepts.
        capacity: usize,
    },
}

impl ObjectRegistryError {
    /// The configured live-object bound that was reached.
    #[must_use]
    pub const fn capacity(self) -> usize {
        match self {
            Self::CapacityExceeded { capacity } => capacity,
        }
    }
}

impl std::fmt::Display for ObjectRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapacityExceeded { capacity } => {
                write!(f, "object handle capacity {capacity} exceeded")
            }
        }
    }
}

impl std::error::Error for ObjectRegistryError {}

/// Backwards-compatible short name for [`ObjectRegistryError`].
pub type RegistryError = ObjectRegistryError;

/// One registry slot.
#[derive(Debug)]
struct Slot<T> {
    generation: u32,
    /// `None` once released; the generation stays, so old refs keep failing.
    occupant: Option<(ObjectKind, T)>,
}

impl<T> Slot<T> {
    /// A slot whose generation has saturated is never reissued — see the module
    /// doc on why wrapping would be worse than losing a slot.
    const fn retired(&self) -> bool {
        self.generation == u32::MAX
    }
}

/// Hands out [`ObjectRef`]s and resolves them.
///
/// Generic over the payload: the host decides whether a `Player` ref points at
/// an ECS `Entity`, a connection id, or something else, and this type stays
/// testable without an ECS.
#[derive(Debug)]
pub struct ObjectRegistry<T> {
    capacity: usize,
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    /// Reverse index, so re-exposing an object a plugin already holds yields
    /// the **same** handle rather than a second one. Bukkit plugins compare
    /// entity references for identity, and two live handles to one entity would
    /// break `equals` in a way that presents as a plugin bug.
    by_payload: HashMap<(ObjectKind, T), ObjectRef>,
}

impl<T> Default for ObjectRegistry<T>
where
    T: Eq + std::hash::Hash + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T> ObjectRegistry<T>
where
    T: Eq + std::hash::Hash + Clone,
{
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_OBJECT_REGISTRY_CAPACITY)
    }

    /// An empty registry with an explicit live-object capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            slots: Vec::new(),
            free: Vec::new(),
            by_payload: HashMap::new(),
        }
    }

    /// Alias for [`Self::with_capacity`] for callers that prefer a constructor
    /// name which distinguishes the bounded form from [`Self::new`].
    #[must_use]
    pub fn new_with_capacity(capacity: usize) -> Self {
        Self::with_capacity(capacity)
    }

    /// The maximum number of live object handles this registry accepts.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many live objects the registry holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_payload.len()
    }

    /// Whether the registry holds nothing live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_payload.is_empty()
    }

    /// Get the handle for `payload`, minting one if it is not already exposed.
    ///
    /// Idempotent by design: calling twice for one entity returns the same
    /// [`ObjectRef`] both times, so Java-side reference equality behaves the
    /// way a plugin author expects.
    pub fn handle_for(&mut self, kind: ObjectKind, payload: T) -> ObjectRef {
        self.try_handle_for(kind, payload)
            .expect("object registry capacity must be sized for its caller")
    }

    /// Get or mint a handle, reporting capacity exhaustion instead of
    /// panicking. Native callback boundaries should use this method so a
    /// misbehaving plugin receives a bounded Java error.
    pub fn try_handle_for(
        &mut self,
        kind: ObjectKind,
        payload: T,
    ) -> Result<ObjectRef, ObjectRegistryError> {
        let key = (kind, payload.clone());
        if let Some(existing) = self.by_payload.get(&key) {
            return Ok(*existing);
        }
        if self.by_payload.len() >= self.capacity {
            return Err(ObjectRegistryError::CapacityExceeded {
                capacity: self.capacity,
            });
        }
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                let index = u32::try_from(self.slots.len())
                    .expect("object registry exceeded u32::MAX slots");
                self.slots.push(Slot {
                    generation: 0,
                    occupant: None,
                });
                index
            }
        };
        let slot = &mut self.slots[index as usize];
        slot.occupant = Some((kind, payload.clone()));
        let handle = ObjectRef {
            index,
            generation: slot.generation,
            kind,
        };
        self.by_payload.insert(key, handle);
        Ok(handle)
    }

    /// Resolve a handle the way a JNI call site needs it: checked for
    /// staleness and for kind.
    ///
    /// # Errors
    ///
    /// See [`ResolveError`]. Every variant is a *reported* failure; none of
    /// them can return another object's payload.
    pub fn resolve(&self, handle: ObjectRef, expected: ObjectKind) -> Result<&T, ResolveError> {
        let slot = self
            .slots
            .get(handle.index as usize)
            .ok_or(ResolveError::OutOfRange)?;
        if slot.generation != handle.generation {
            return Err(ResolveError::Stale);
        }
        let Some((kind, payload)) = slot.occupant.as_ref() else {
            return Err(ResolveError::Stale);
        };
        if *kind != expected {
            return Err(ResolveError::KindMismatch {
                expected,
                actual: *kind,
            });
        }
        Ok(payload)
    }

    /// Retire every handle to `payload` — the entity despawned, the player
    /// disconnected, the chunk unloaded.
    ///
    /// Returns whether anything was live. Bumping the generation here is what
    /// makes every outstanding ref fail from this moment on.
    pub fn release(&mut self, payload: &T) -> bool {
        let keys: Vec<_> = self
            .by_payload
            .keys()
            .filter(|(_, candidate)| candidate == payload)
            .cloned()
            .collect();
        keys.into_iter()
            .map(|(kind, payload)| self.release_kind(kind, &payload))
            .fold(false, |released, current| released || current)
    }

    /// Release one object of one kind.
    pub fn release_kind(&mut self, kind: ObjectKind, payload: &T) -> bool {
        let Some(handle) = self.by_payload.remove(&(kind, payload.clone())) else {
            return false;
        };
        let Some(slot) = self.slots.get_mut(handle.index as usize) else {
            return false;
        };
        slot.occupant = None;
        // Saturating, not wrapping: see the module doc. A wrapped generation
        // would make ancient handles valid again.
        slot.generation = slot.generation.saturating_add(1);
        if !slot.retired() {
            self.free.push(handle.index);
        }
        true
    }

    /// Release every live object matching `predicate`.
    ///
    /// This is the lifecycle cleanup hook: an owner can invalidate all of its
    /// handles without learning or storing slot indices. Every invalidated
    /// slot advances its generation before it can be reused.
    pub fn release_matching(
        &mut self,
        mut predicate: impl FnMut(ObjectKind, &T) -> bool,
    ) -> usize {
        let keys: Vec<_> = self
            .by_payload
            .keys()
            .filter(|(kind, payload)| predicate(*kind, payload))
            .cloned()
            .collect();
        keys.into_iter()
            .filter(|(kind, payload)| self.release_kind(*kind, payload))
            .count()
    }

    /// Release every live object and leave the registry reusable.
    pub fn clear(&mut self) -> usize {
        let keys: Vec<_> = self.by_payload.keys().cloned().collect();
        keys.into_iter()
            .filter(|(kind, payload)| self.release_kind(*kind, payload))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_is_fallible_and_released_slots_are_reusable() {
        let mut registry = ObjectRegistry::with_capacity(1);
        let first = registry
            .try_handle_for(ObjectKind::World, 11_u64)
            .expect("the first object fits");
        assert_eq!(
            registry.try_handle_for(ObjectKind::Player, 22),
            Err(ObjectRegistryError::CapacityExceeded { capacity: 1 }),
        );
        assert_eq!(
            registry
                .try_handle_for(ObjectKind::World, 11)
                .expect("re-exposing a live object does not consume capacity"),
            first,
        );
        assert!(registry.release_kind(ObjectKind::World, &11));
        let replacement = registry
            .try_handle_for(ObjectKind::Player, 22)
            .expect("released capacity is reusable");
        assert_eq!(registry.resolve(first, ObjectKind::World), Err(ResolveError::Stale));
        assert_eq!(registry.resolve(replacement, ObjectKind::Player), Ok(&22));
    }

    #[test]
    fn same_payload_in_different_kinds_has_independent_identity() {
        let mut registry = ObjectRegistry::with_capacity(2);
        let world = registry.handle_for(ObjectKind::World, 7_u8);
        let player = registry.handle_for(ObjectKind::Player, 7_u8);
        assert_ne!(world, player);
        assert_eq!(registry.resolve(world, ObjectKind::World), Ok(&7));
        assert_eq!(registry.resolve(player, ObjectKind::Player), Ok(&7));
        assert_eq!(registry.release(&7), true);
        assert_eq!(registry.resolve(world, ObjectKind::World), Err(ResolveError::Stale));
        assert_eq!(registry.resolve(player, ObjectKind::Player), Err(ResolveError::Stale));
        assert!(registry.is_empty());
    }

    #[test]
    fn clear_invalidates_every_live_handle_before_reuse() {
        let mut registry = ObjectRegistry::with_capacity(2);
        let old_world = registry.handle_for(ObjectKind::World, 1_u8);
        let old_block = registry.handle_for(ObjectKind::Block, 2_u8);
        assert_eq!(registry.clear(), 2);
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.resolve(old_world, ObjectKind::World), Err(ResolveError::Stale));
        assert_eq!(registry.resolve(old_block, ObjectKind::Block), Err(ResolveError::Stale));
        let new_world = registry.handle_for(ObjectKind::World, 3_u8);
        assert_ne!(new_world, old_world);
        assert_eq!(registry.resolve(old_world, ObjectKind::World), Err(ResolveError::Stale));
    }
}
