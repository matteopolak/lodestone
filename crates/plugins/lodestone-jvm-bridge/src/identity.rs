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
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    /// Reverse index, so re-exposing an object a plugin already holds yields
    /// the **same** handle rather than a second one. Bukkit plugins compare
    /// entity references for identity, and two live handles to one entity would
    /// break `equals` in a way that presents as a plugin bug.
    by_payload: HashMap<T, ObjectRef>,
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
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            by_payload: HashMap::new(),
        }
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
        if let Some(existing) = self.by_payload.get(&payload) {
            return *existing;
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
        self.by_payload.insert(payload, handle);
        handle
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
        let Some(handle) = self.by_payload.remove(payload) else {
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
}
