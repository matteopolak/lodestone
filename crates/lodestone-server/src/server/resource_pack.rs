//! Resource-pack push: the feed a host uses to offer a pack to connected clients, and the recorded client responses.

use super::*;

/// A shared feed of server-initiated resource pack pushes — the
/// exact idiom [`BlockTickFeed`]/[`ExplosionFeed`]/[`WeatherFeed`] establish
/// for block changes, detonations and weather transitions, applied to a
/// resource pack push instead. A host publishes one [`ResourcePackPush`] per
/// push; `serve_play`'s `container_sync_tick` arm drains it into a real
/// clientbound `resource_pack_push` frame, on the same timer the three feeds
/// above ride.
///
/// Same single-consumer caveat as all three, and the same resolution:
/// singleplayer (`crate::IntegratedServer::open_in_memory_with_mobs`) spawns
/// exactly one connection task per feed instance. A push is broadcast-shaped
/// in vanilla (every connection must receive it), so this is the documented
/// limitation the other single-consumer feeds share, not a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePackResponseRecord {
    /// Id of the resource pack this response concerns.
    pub id: uuid::Uuid,
    /// Outcome reported by the client.
    pub response: ResourcePackResponseKind,
}

#[derive(Debug, Clone, Default)]
pub struct ResourcePackPushFeed(
    Arc<Mutex<Vec<ResourcePackPush>>>,
    Arc<Mutex<Vec<ResourcePackResponseRecord>>>,
);

impl ResourcePackPushFeed {
    /// Records one push for every consumer to learn about on their next
    /// [`drain_all`](Self::drain_all).
    pub fn publish(&self, push: ResourcePackPush) {
        self.0
            .lock()
            .expect("resource pack feed lock poisoned")
            .push(push);
    }

    /// Drains and returns every push published since the last call — see the
    /// struct doc comment for why this is safe only for exactly one consumer.
    pub fn drain_all(&self) -> Vec<ResourcePackPush> {
        std::mem::take(&mut *self.0.lock().expect("resource pack feed lock poisoned"))
    }

    /// Records a response received from a client for host-side policy or
    /// telemetry. Recording does not enforce acceptance or disconnect on any
    /// particular outcome.
    pub fn record_response(&self, response: ResourcePackResponseRecord) {
        self.1
            .lock()
            .expect("resource pack response feed lock poisoned")
            .push(response);
    }

    /// Drains responses received since the last call.
    pub fn drain_responses(&self) -> Vec<ResourcePackResponseRecord> {
        std::mem::take(
            &mut *self
                .1
                .lock()
                .expect("resource pack response feed lock poisoned"),
        )
    }
}
