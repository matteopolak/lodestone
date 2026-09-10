//! Typed moving-entity ownership hand-off across tick-region boundaries.
//!
//! A region owner may finish a moving entity in a different chunk from the
//! chunk that owned its tick-start state. The source must stop before a
//! destination can accept that state; otherwise two owners can both advance
//! one entity or a delayed completion can overwrite a newer transfer. This
//! module keeps that ordering as a small, bounded token protocol. It does not
//! start workers and it does not retain entity state: the caller owns the
//! payload and calls the two transitions around its central state update.

use std::collections::BTreeMap;

use crate::tick_region::TickOwner;

/// The typed identity of one moving-entity hand-off.
///
/// `epoch` is the producer's monotonically increasing tick-start plan. The
/// entity id and serial slot prevent a completion from being applied to a
/// different entity or publication position, while source and destination
/// make the ownership transition explicit rather than deriving it again from
/// a mutable position after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityHandoffToken {
    /// The network identity that remains stable across the transfer.
    pub entity_id: i32,
    /// The entity's tick-start position in the central publication sequence.
    pub serial: usize,
    /// The tick-start owner-plan identity.
    pub epoch: u64,
    /// The owner that has finished its last mutation of this entity.
    pub source: TickOwner,
    /// The owner that may begin the next mutation of this entity.
    pub destination: TickOwner,
}

impl EntityHandoffToken {
    fn valid_fields(epoch: u64, source: TickOwner, destination: TickOwner) -> bool {
        epoch != 0
            && source != destination
            && matches!(source, TickOwner::Chunk { .. })
            && matches!(destination, TickOwner::Chunk { .. })
    }

    /// Builds a token for a real cross-owner transfer.
    ///
    /// A zero epoch is not a usable plan identity, so callers get `None`
    /// rather than manufacturing an ambiguous first-generation token.
    #[must_use]
    pub fn new(
        entity_id: i32,
        serial: usize,
        epoch: u64,
        source: TickOwner,
        destination: TickOwner,
    ) -> Option<Self> {
        Self::valid_fields(epoch, source, destination).then_some(Self {
            entity_id,
            serial,
            epoch,
            source,
            destination,
        })
    }
}

/// Why a moving-entity hand-off was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityHandoffError {
    /// A transfer must name a nonzero source and destination epoch.
    InvalidToken,
    /// An entity already has a source stop waiting for destination admission.
    ConcurrentTransfer,
    /// The same source completion was presented twice.
    DuplicateSourceStop,
    /// A completion is older than one already accepted for this entity.
    StaleEpoch,
    /// The destination was asked to start before its source stopped.
    DestinationBeforeSource,
    /// The destination completion was presented twice.
    DuplicateDestinationStart,
    /// The token does not match the active source-to-destination route.
    MismatchedDestination,
}

/// A bounded source-stop/destination-start barrier for moving entities.
///
/// Active state is retained only between the two calls for a live transfer.
/// The completed epoch is retained per live entity so delayed replies are
/// rejected without an unbounded history; callers should call
/// [`Self::forget_entity`] when the entity is removed.
#[derive(Debug, Default)]
pub struct EntityOwnershipHandoff {
    active: BTreeMap<(i32, u64), EntityHandoffToken>,
    completed_epoch: BTreeMap<i32, u64>,
}

impl EntityOwnershipHandoff {
    /// Records that the source owner has stopped mutating `token.entity_id`.
    ///
    /// No destination can start until this succeeds. A second transfer for
    /// the same entity is refused while the first source-to-destination route
    /// is still active, so a worker cannot race its own previous completion.
    pub fn stop_source(&mut self, token: EntityHandoffToken) -> Result<(), EntityHandoffError> {
        if !EntityHandoffToken::valid_fields(token.epoch, token.source, token.destination) {
            return Err(EntityHandoffError::InvalidToken);
        }
        if self
            .completed_epoch
            .get(&token.entity_id)
            .is_some_and(|&epoch| token.epoch <= epoch)
        {
            return Err(EntityHandoffError::StaleEpoch);
        }
        let key = (token.entity_id, token.epoch);
        if self.active.contains_key(&key) {
            return Err(EntityHandoffError::DuplicateSourceStop);
        }
        if self
            .active
            .keys()
            .any(|&(entity_id, _)| entity_id == token.entity_id)
        {
            return Err(EntityHandoffError::ConcurrentTransfer);
        }
        self.active.insert(key, token);
        Ok(())
    }

    /// Starts the destination owner after the matching source stop.
    ///
    /// Consuming the active token is the destination barrier: a replay after
    /// this call cannot mutate the entity again, and a stale token cannot
    /// replace a newer destination state.
    pub fn start_destination(
        &mut self,
        token: EntityHandoffToken,
    ) -> Result<(), EntityHandoffError> {
        if !EntityHandoffToken::valid_fields(token.epoch, token.source, token.destination) {
            return Err(EntityHandoffError::InvalidToken);
        }
        if self
            .completed_epoch
            .get(&token.entity_id)
            .is_some_and(|&epoch| token.epoch <= epoch)
        {
            return Err(EntityHandoffError::DuplicateDestinationStart);
        }
        let Some(active) = self.active.remove(&(token.entity_id, token.epoch)) else {
            return Err(EntityHandoffError::DestinationBeforeSource);
        };
        if active != token {
            self.active.insert((active.entity_id, active.epoch), active);
            return Err(EntityHandoffError::MismatchedDestination);
        }
        self.completed_epoch.insert(token.entity_id, token.epoch);
        Ok(())
    }

    /// Drops coordination state for an entity that was removed before its
    /// next tick. This keeps the bounded barrier aligned with the live entity
    /// set instead of becoming a process-wide acknowledgement history.
    pub fn forget_entity(&mut self, entity_id: i32) {
        self.active.retain(|&(id, _), _| id != entity_id);
        self.completed_epoch.remove(&entity_id);
    }

    /// Number of source stops waiting for destination admission.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.active.len()
    }

    /// Reports whether a chunk owner can be released while an entity transfer
    /// is in flight.
    ///
    /// A source-stop token remains active until the central writer admits its
    /// destination. The chunk lifecycle/save owner must keep that source
    /// resident for the interval; otherwise a delayed completion could outlive
    /// the state that made it safe to apply. This is a query rather than a
    /// reservation: callers still need the ordinary chunk-lifecycle gate when
    /// they actually start an unload.
    #[must_use]
    pub fn source_unload_ready(&self, source: TickOwner) -> bool {
        !self
            .active
            .values()
            .any(|token| token.source == source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(epoch: u64) -> EntityHandoffToken {
        EntityHandoffToken::new(
            17,
            3,
            epoch,
            TickOwner::Chunk { cx: -1, cz: 0 },
            TickOwner::Chunk { cx: 0, cz: 0 },
        )
        .expect("test token is a real cross-owner transfer")
    }

    #[test]
    fn destination_cannot_start_before_source_stops() {
        let mut handoff = EntityOwnershipHandoff::default();
        assert_eq!(
            handoff.start_destination(token(1)),
            Err(EntityHandoffError::DestinationBeforeSource)
        );
        assert_eq!(handoff.pending(), 0);
        handoff.stop_source(token(1)).expect("source stop");
        handoff
            .start_destination(token(1))
            .expect("source stop opens destination");
        assert_eq!(handoff.pending(), 0);
    }

    #[test]
    fn stale_and_duplicate_delivery_is_rejected_by_epoch_and_entity_id() {
        let mut handoff = EntityOwnershipHandoff::default();
        handoff.stop_source(token(4)).expect("first source stop");
        assert_eq!(
            handoff.stop_source(token(4)),
            Err(EntityHandoffError::DuplicateSourceStop)
        );
        handoff
            .start_destination(token(4))
            .expect("first destination start");
        assert_eq!(
            handoff.start_destination(token(4)),
            Err(EntityHandoffError::DuplicateDestinationStart)
        );
        assert_eq!(
            handoff.stop_source(token(3)),
            Err(EntityHandoffError::StaleEpoch)
        );
        handoff.stop_source(token(5)).expect("newer source stop");
        assert_eq!(handoff.pending(), 1);
    }

    #[test]
    fn concurrent_same_entity_transfer_is_rejected_but_other_entities_progress() {
        let mut handoff = EntityOwnershipHandoff::default();
        handoff.stop_source(token(8)).expect("first entity stop");
        let other = EntityHandoffToken {
            entity_id: 18,
            serial: 4,
            ..token(8)
        };
        assert_eq!(
            handoff.stop_source(EntityHandoffToken {
                epoch: 9,
                ..token(8)
            }),
            Err(EntityHandoffError::ConcurrentTransfer)
        );
        handoff.stop_source(other).expect("independent entity stop");
        assert_eq!(handoff.pending(), 2);
    }

    #[test]
    fn invalid_and_mismatched_routes_fail_closed() {
        assert_eq!(
            EntityHandoffToken::new(
                1,
                0,
                0,
                TickOwner::Chunk { cx: 0, cz: 0 },
                TickOwner::Chunk { cx: 1, cz: 0 },
            ),
            None
        );
        let mut handoff = EntityOwnershipHandoff::default();
        handoff.stop_source(token(10)).expect("source stop");
        let mismatched = EntityHandoffToken {
            destination: TickOwner::Chunk { cx: 1, cz: 0 },
            ..token(10)
        };
        assert_eq!(
            handoff.start_destination(mismatched),
            Err(EntityHandoffError::MismatchedDestination)
        );
        assert_eq!(
            handoff.pending(),
            1,
            "a mismatched reply cannot consume the route"
        );
    }

    #[test]
    fn global_owners_cannot_enter_a_chunk_transfer_and_source_unload_waits() {
        assert_eq!(
            EntityHandoffToken::new(
                1,
                0,
                1,
                TickOwner::Global,
                TickOwner::Chunk { cx: 0, cz: 0 },
            ),
            None
        );
        assert_eq!(
            EntityHandoffToken::new(
                1,
                0,
                1,
                TickOwner::Chunk { cx: 0, cz: 0 },
                TickOwner::Global,
            ),
            None
        );

        let mut handoff = EntityOwnershipHandoff::default();
        let forged = EntityHandoffToken {
            entity_id: 1,
            serial: 0,
            epoch: 1,
            source: TickOwner::Global,
            destination: TickOwner::Chunk { cx: 0, cz: 0 },
        };
        assert_eq!(
            handoff.stop_source(forged),
            Err(EntityHandoffError::InvalidToken),
            "public token fields must still be validated at the barrier"
        );
        let transfer = token(1);
        let source = transfer.source;
        let destination = transfer.destination;
        assert!(handoff.source_unload_ready(source));
        handoff.stop_source(transfer).expect("source stop");
        assert!(!handoff.source_unload_ready(source));
        assert!(handoff.source_unload_ready(destination));
        handoff
            .start_destination(transfer)
            .expect("destination admission");
        assert!(handoff.source_unload_ready(source));
    }

    #[test]
    fn forgetting_an_entity_releases_pending_and_completed_state() {
        let mut handoff = EntityOwnershipHandoff::default();
        handoff.stop_source(token(11)).expect("source stop");
        handoff.forget_entity(17);
        assert_eq!(handoff.pending(), 0);
        handoff
            .stop_source(token(1))
            .expect("a live id can start fresh");
        handoff
            .start_destination(token(1))
            .expect("destination admission records the completed epoch");
        handoff.forget_entity(17);
        handoff
            .stop_source(token(1))
            .expect("forgetting a removed id clears completed epochs");
    }
}
