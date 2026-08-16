//! Server debug feeds and NBT query replies (issue #26).
//!
//! ## What it is
//!
//! The client-side mirror of vanilla's debug renderers: the values the server
//! pushes on a `minecraft:debug_subscription` the client asked for, plus the
//! two one-shot debug replies (`debug_sample`, `tag_query`) and the game-test
//! overlay positions.
//!
//! ## How it works
//!
//! **The server sends nothing here until asked.** Every `debug_*_value` and
//! `debug_event` packet exists only in response to
//! `ClientAction::SubscribeDebug`, so this store and that action are two halves
//! of one loop — a store with no subscriber stays empty forever and is not
//! broken, and a subscriber with no store would throw the values away. That is
//! the reason both landed together rather than in whichever order was easier.
//!
//! Values are keyed by `(subscription, subject)` and are **last-write-wins**:
//! vanilla's own renderers hold one current value per subject, and the wire's
//! `Optional` on the three `*_value` packets is how the server *clears* a key.
//! So `Some(bytes)` inserts, `None` removes — treating `None` as "empty payload"
//! would leave stale overlays on screen after the server stopped tracking them.
//!
//! `debug_event` has no `Optional` wrapper, because an event is always present;
//! it appends to a bounded ring instead of replacing a key.
//!
//! ## Payloads are opaque, on purpose
//!
//! A subscription's value codec is chosen per registry entry and the seventeen
//! registered ones share no shape at all — `DebugBeeInfo`, `DebugBrainDump`,
//! `List<BlockPos>`, `Unit` (zero bytes), and one (`dedicated_server_tick_time`)
//! whose value codec is `null` and throws if it is ever sent as a value. Seventeen
//! decoders for a debug overlay is the wrong trade, so this store carries bytes
//! and a renderer decodes the one or two feeds it actually draws.
//!
//! ## How to change it
//!
//! To draw a feed, read [`DebugFeedStore::block_values`] (or the chunk/entity
//! equivalent) filtered to your subscription key and parse the bytes there. To
//! *receive* one, add its key to the client's subscription set — nothing here
//! subscribes on its own.
//!
//! [`EVENT_RING`] bounds the event ring; a debug feed on a busy server is
//! unbounded otherwise and this store lives for the whole session.
//!
//! ## Dependencies
//!
//! [`lodestone_model::event::ClientEvent`] only.

use std::collections::BTreeMap;
use std::collections::VecDeque;

use lodestone_model::event::{ClientEvent, DebugSampleKind};
use lodestone_model::{BlockPos, Identifier, Text};

/// How many `debug_event`s are retained. Vanilla's own renderers keep a
/// comparable short history; the number matters only as a bound.
pub const EVENT_RING: usize = 256;

/// How many `debug_sample` batches are retained, for the same reason.
pub const SAMPLE_RING: usize = 64;

/// One received debug event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugFeedEvent {
    /// The subscription it arrived on.
    pub subscription: Identifier,
    /// Opaque per-subscription payload.
    pub value: Vec<u8>,
}

/// One received sample batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugSampleBatch {
    /// The samples, nanoseconds for [`DebugSampleKind::TickTime`].
    pub sample: Vec<i64>,
    /// Which series.
    pub kind: DebugSampleKind,
}

/// A game-test highlight the server asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameTestHighlight {
    /// Absolute world position.
    pub absolute: BlockPos,
    /// Position relative to the test's origin.
    pub relative: BlockPos,
}

/// A test instance block's last reported status.
#[derive(Debug, Clone, PartialEq)]
pub struct TestInstanceStatusReport {
    /// Human-readable status line.
    pub status: Text,
    /// Detected region size, when the server has one.
    pub size: Option<(i32, i32, i32)>,
}

/// Everything the server has told us on a debug channel this session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DebugFeedStore {
    block_values: BTreeMap<(Identifier, (i32, i32, i32)), Vec<u8>>,
    chunk_values: BTreeMap<(Identifier, (i32, i32)), Vec<u8>>,
    entity_values: BTreeMap<(Identifier, i32), Vec<u8>>,
    events: VecDeque<DebugFeedEvent>,
    samples: VecDeque<DebugSampleBatch>,
    highlights: Vec<GameTestHighlight>,
    test_instance_status: Option<TestInstanceStatusReport>,
    nbt_replies: BTreeMap<i32, Option<Vec<u8>>>,
}

impl DebugFeedStore {
    /// An empty store — the correct state for every session in which nothing
    /// subscribed to a debug feed, which is all of them until something does.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The retained debug events, oldest first.
    #[must_use]
    pub fn events(&self) -> impl Iterator<Item = &DebugFeedEvent> {
        self.events.iter()
    }

    /// The retained sample batches, oldest first.
    #[must_use]
    pub fn samples(&self) -> impl Iterator<Item = &DebugSampleBatch> {
        self.samples.iter()
    }

    /// Game-test highlights the server has asked for, in arrival order.
    #[must_use]
    pub fn highlights(&self) -> &[GameTestHighlight] {
        &self.highlights
    }

    /// The last test instance block status, if any.
    #[must_use]
    pub fn test_instance_status(&self) -> Option<&TestInstanceStatusReport> {
        self.test_instance_status.as_ref()
    }

    /// The reply to NBT query `transaction_id`, if one has arrived.
    ///
    /// The outer `Option` is "did a reply arrive"; the inner one is "did the
    /// server have anything". Those are genuinely different and collapsing them
    /// would make a refused query indistinguishable from a lost one.
    #[must_use]
    pub fn nbt_reply(&self, transaction_id: i32) -> Option<&Option<Vec<u8>>> {
        self.nbt_replies.get(&transaction_id)
    }

    /// Takes the reply to `transaction_id`, so a requester consumes it once.
    pub fn take_nbt_reply(&mut self, transaction_id: i32) -> Option<Option<Vec<u8>>> {
        self.nbt_replies.remove(&transaction_id)
    }

    /// How many values across all three subject kinds are held. The cheap thing
    /// for a test to assert that is a fact about traversal rather than about
    /// construction.
    #[must_use]
    pub fn value_count(&self) -> usize {
        self.block_values.len() + self.chunk_values.len() + self.entity_values.len()
    }

    /// Folds one event, returning whether it belonged to this store.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::DebugBlockValue {
                pos,
                subscription,
                value,
            } => {
                let key = (subscription.clone(), (pos.x, pos.y, pos.z));
                match value {
                    Some(bytes) => {
                        self.block_values.insert(key, bytes.clone());
                    }
                    // An absent value is the server *clearing* this key, not an
                    // empty payload. See the module doc.
                    None => {
                        self.block_values.remove(&key);
                    }
                }
                true
            }
            ClientEvent::DebugChunkValue {
                chunk,
                subscription,
                value,
            } => {
                let key = (subscription.clone(), (chunk.x, chunk.z));
                match value {
                    Some(bytes) => {
                        self.chunk_values.insert(key, bytes.clone());
                    }
                    None => {
                        self.chunk_values.remove(&key);
                    }
                }
                true
            }
            ClientEvent::DebugEntityValue {
                entity_id,
                subscription,
                value,
            } => {
                let key = (subscription.clone(), *entity_id);
                match value {
                    Some(bytes) => {
                        self.entity_values.insert(key, bytes.clone());
                    }
                    None => {
                        self.entity_values.remove(&key);
                    }
                }
                true
            }
            ClientEvent::DebugEvent {
                subscription,
                value,
            } => {
                if self.events.len() == EVENT_RING {
                    self.events.pop_front();
                }
                self.events.push_back(DebugFeedEvent {
                    subscription: subscription.clone(),
                    value: value.clone(),
                });
                true
            }
            ClientEvent::DebugSample { sample, kind } => {
                if self.samples.len() == SAMPLE_RING {
                    self.samples.pop_front();
                }
                self.samples.push_back(DebugSampleBatch {
                    sample: sample.clone(),
                    kind: *kind,
                });
                true
            }
            ClientEvent::GameTestHighlightPos { absolute, relative } => {
                self.highlights.push(GameTestHighlight {
                    absolute: *absolute,
                    relative: *relative,
                });
                true
            }
            ClientEvent::TestInstanceBlockStatus { status, size } => {
                self.test_instance_status = Some(TestInstanceStatusReport {
                    status: status.clone(),
                    size: *size,
                });
                true
            }
            ClientEvent::TagQueryResponse {
                transaction_id,
                tag,
            } => {
                self.nbt_replies.insert(*transaction_id, tag.clone());
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DebugFeedStore;
    use lodestone_model::event::ClientEvent;
    use lodestone_model::BlockPos;

    fn key(name: &str) -> lodestone_model::Identifier {
        name.parse().expect("test key parses")
    }

    /// The load-bearing behaviour: `None` clears rather than storing empty.
    #[test]
    fn an_absent_value_clears_the_key_rather_than_storing_an_empty_payload() {
        let mut store = DebugFeedStore::new();
        let pos = BlockPos { x: 1, y: 2, z: 3 };
        assert!(store.apply(&ClientEvent::DebugBlockValue {
            pos,
            subscription: key("minecraft:neighbor_updates"),
            value: Some(vec![1, 2, 3]),
        }));
        assert_eq!(store.value_count(), 1);

        assert!(store.apply(&ClientEvent::DebugBlockValue {
            pos,
            subscription: key("minecraft:neighbor_updates"),
            value: None,
        }));
        assert_eq!(
            store.value_count(),
            0,
            "a cleared key must leave no entry -- a stale overlay would keep drawing"
        );
    }

    /// Distinct subscriptions on the same subject must not collide.
    #[test]
    fn the_subscription_is_part_of_the_key() {
        let mut store = DebugFeedStore::new();
        let pos = BlockPos { x: 0, y: 0, z: 0 };
        store.apply(&ClientEvent::DebugBlockValue {
            pos,
            subscription: key("minecraft:neighbor_updates"),
            value: Some(vec![1]),
        });
        store.apply(&ClientEvent::DebugBlockValue {
            pos,
            subscription: key("minecraft:redstone_wire_orientations"),
            value: Some(vec![2]),
        });
        assert_eq!(store.value_count(), 2);
    }

    /// "No reply yet" and "replied with nothing" are different states.
    #[test]
    fn a_null_nbt_reply_is_still_a_reply() {
        let mut store = DebugFeedStore::new();
        assert!(store.nbt_reply(1).is_none(), "nothing has arrived yet");
        store.apply(&ClientEvent::TagQueryResponse {
            transaction_id: 1,
            tag: None,
        });
        assert_eq!(
            store.nbt_reply(1),
            Some(&None),
            "an arrived-but-empty reply must be distinguishable from no reply"
        );
        assert_eq!(store.take_nbt_reply(1), Some(None));
        assert!(store.nbt_reply(1).is_none(), "taking consumes it");
    }

    #[test]
    fn the_event_ring_is_bounded() {
        let mut store = DebugFeedStore::new();
        for index in 0..(super::EVENT_RING + 10) {
            store.apply(&ClientEvent::DebugEvent {
                subscription: key("minecraft:game_events"),
                value: vec![u8::try_from(index % 256).unwrap()],
            });
        }
        assert_eq!(store.events().count(), super::EVENT_RING);
    }

    #[test]
    fn an_unrelated_event_is_rejected() {
        let mut store = DebugFeedStore::new();
        assert!(!store.apply(&ClientEvent::KeepAlive { id: 1 }));
    }
}
