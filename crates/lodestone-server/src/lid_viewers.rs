//! Which players have a shulker box open, and the lid animation that implies.
//!
//! # What it is
//!
//! A map from player to the shulker box they are looking into. The lid opens
//! when the first viewer arrives and closes when the last one leaves; every
//! change in the count is announced as a block event so each client animates
//! the lid itself.
//!
//! # How it works
//!
//! The map lives on [`crate::block_entities::BlockEntityRegistry`], so it is
//! shared by every connection. [`LidViewers::open`] and [`LidViewers::release`]
//! return the [`LidChange`]s they caused; [`LidChange::effects`] turns one
//! into the block event and, on the first open and last close, the sound.
//! [`LidGuard`] releases a player's lid when its connection task ends by any
//! route, so an error or a cancelled task cannot leave a lid open.
//!
//! # How to change it
//!
//! Another animated container (a chest lid, a barrel) would add its viewer
//! here and its block-event parameters in [`LidChange::effects`]. The sound
//! pitch is the midpoint of the reference's random range because a guard
//! dropping on a dead connection has no random source.
//!
//! # Dependencies
//!
//! [`crate::effects::WorldEffect`] and [`crate::tick::BlockTickFeed`] to publish.

use std::collections::HashMap;

use lodestone_model::BlockPos;
use uuid::Uuid;

use crate::block_entities::BlockEntityHandle;
use crate::effects::WorldEffect;
use crate::tick::BlockTickFeed;

/// Block-event kind that carries a lid's viewer count.
const LID_EVENT: u8 = 1;

/// The count at one box before and after a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LidChange {
    pub(crate) pos: BlockPos,
    /// The box's block registry name, which the block event is addressed to.
    pub(crate) block: String,
    pub(crate) before: usize,
    pub(crate) after: usize,
}

impl LidChange {
    /// The block event, plus the open or close sound when the lid starts or
    /// finishes moving.
    #[must_use]
    pub(crate) fn effects(&self) -> Vec<WorldEffect> {
        let mut out = vec![WorldEffect::BlockEvent {
            pos: self.pos,
            block: self.block.clone(),
            kind: LID_EVENT,
            value: u8::try_from(self.after).unwrap_or(u8::MAX),
        }];
        let sound = if self.before == 0 && self.after > 0 {
            Some("minecraft:block.shulker_box.open")
        } else if self.before > 0 && self.after == 0 {
            Some("minecraft:block.shulker_box.close")
        } else {
            None
        };
        out.extend(sound.and_then(|name| crate::effects::block_sound(name, self.pos, 0.5, 0.95, 0)));
        out
    }
}

/// Players with a shulker box open, by player.
#[derive(Debug, Default)]
pub(crate) struct LidViewers {
    viewing: HashMap<Uuid, (BlockPos, String)>,
}

impl LidViewers {
    fn count(&self, pos: BlockPos) -> usize {
        self.viewing.values().filter(|(at, _)| *at == pos).count()
    }

    /// Whether anyone is looking into the box at `pos`.
    pub(crate) fn is_open(&self, pos: BlockPos) -> bool {
        self.count(pos) > 0
    }

    /// Records `who` as looking into the box at `pos`, first dropping whatever
    /// box they were looking into.
    pub(crate) fn open(&mut self, who: Uuid, pos: BlockPos, block: &str) -> Vec<LidChange> {
        let mut changes = self.release(who);
        let before = self.count(pos);
        self.viewing.insert(who, (pos, block.to_owned()));
        changes.push(LidChange { pos, block: block.to_owned(), before, after: before + 1 });
        changes
    }

    /// Ends `who`'s view, if they had one.
    pub(crate) fn release(&mut self, who: Uuid) -> Vec<LidChange> {
        let Some((pos, block)) = self.viewing.remove(&who) else { return Vec::new() };
        let after = self.count(pos);
        vec![LidChange { pos, block, before: after + 1, after }]
    }

    /// Forgets every view of the box at `pos` without announcing it: the box
    /// is gone, so there is no lid left to animate.
    pub(crate) fn forget(&mut self, pos: BlockPos) {
        self.viewing.retain(|_, (at, _)| *at != pos);
    }
}

/// Releases a player's lid when dropped.
pub(crate) struct LidGuard {
    block_entities: BlockEntityHandle,
    feed: BlockTickFeed,
    who: Uuid,
}

impl LidGuard {
    #[must_use]
    pub(crate) fn new(block_entities: BlockEntityHandle, feed: BlockTickFeed, who: Uuid) -> Self {
        Self { block_entities, feed, who }
    }
}

impl Drop for LidGuard {
    fn drop(&mut self) {
        release_and_publish(&self.block_entities, &self.feed, self.who);
    }
}

/// Ends `who`'s view and publishes the resulting lid effects.
pub(crate) fn release_and_publish(block_entities: &BlockEntityHandle, feed: &BlockTickFeed, who: Uuid) {
    let changes = block_entities.with(|reg| reg.lid_viewers.release(who));
    for effect in changes.iter().flat_map(LidChange::effects) {
        feed.publish_effect(effect);
    }
}
