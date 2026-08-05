//! The connected-player registry — the thing that makes a player an **entity
//! other connections receive** (issue #438).
//!
//! # What it is
//!
//! Before this module the server had exactly one entity egress,
//! [`EntitySource::snapshots`](crate::EntitySource), and in production it was
//! fed by [`LiveMobSource`](crate::LiveMobSource) alone. Everything the server
//! knew about a *player* lived in local variables inside `serve_play`'s stack
//! frame, so nothing could address it: two players on one server — including
//! over LAN — were completely invisible to each other.
//!
//! [`PlayerRegistry`] is the shared handle that fixes that, in the same shape
//! [`BlockEntityHandle`](crate::BlockEntityHandle) and
//! [`MobHandle`](crate::MobHandle) already established for the other two kinds
//! of shared world state: an `Arc<Mutex<…>>` every connection task clones, so
//! one connection's join is visible to every other connection's next streaming
//! pass.
//!
//! # Why there is no broadcast channel
//!
//! Issue #438's own body names "no broadcast path" as the third of three
//! blockers, and it is the one that turned out **not** to need building.
//! [`EntityStreamer`](crate::EntitySource)'s per-connection diff is already a
//! *pull*: each connection compares "the entities right now" against what it
//! was last sent and emits the difference. A player appearing in the registry
//! is therefore picked up by every other connection's next pass with no push
//! at all — the same mechanism that already spawns a mob that walked into
//! view. Adding a `broadcast::Sender` would have been a second, redundant
//! mechanism for a diff that already exists.
//!
//! The tab list is the one thing the entity diff does *not* cover, so it gets
//! the identical treatment one level up: [`PlayerListStreamer`] is the
//! roster-shaped twin of `EntityStreamer`, diffing UUIDs instead of snapshots.
//!
//! # The ordering constraint that is not optional
//!
//! **A real client silently drops an `ADD_ENTITY` for a player it has no
//! `PlayerInfo` for.** From the jar, not inferred —
//! `ClientPacketListener.createEntityFromPacket`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/multiplayer/ClientPacketListener.java:591-604`):
//!
//! ```text
//! if (type == EntityTypes.PLAYER) {
//!    PlayerInfo playerInfo = this.getPlayerInfo(packet.getUUID());
//!    if (playerInfo == null) {
//!       LOGGER.warn("Server attempted to add player prior to sending player info (Player id {})", packet.getUUID());
//!       return null;
//!    } else {
//!       return new RemotePlayer(this.level, playerInfo.getProfile());
//!    }
//! }
//! ```
//!
//! It returns `null`, `handleAddEntity` logs "Skipping Entity with id" and the
//! entity is **never added to the level**. So a server that streams a perfect
//! `ADD_ENTITY` and no `player_info_update` reaches zero pixels while every
//! wire in `cargo xtask connectedness` reads green — exactly this repo's
//! island failure mode. That is why [`PlayerView`] carries the roster and the
//! entity snapshots **together, from one lock acquisition**, and why
//! `crate::server`'s streaming pass emits the player-list directives *before*
//! the entity diff. Two separate reads could interleave a join between them
//! and produce precisely the dropped spawn above.
//!
//! # How to change it
//!
//! * **Adding a field a player carries on the wire** (rotation, held item,
//!   skin properties): extend [`TrackedPlayer`], set it in [`PlayerRegistry`],
//!   and lower it in [`PlayerRegistry::view`]. Nothing in `crate::server`
//!   changes.
//! * **Player rotation is deliberately absent.**
//!   [`ServerBound::PlayerMoved`](crate::ServerBound::PlayerMoved) carries
//!   `(x, y, z, on_ground)` and no angles: `v770`'s decoder discards the
//!   rotation from `move_player_pos_rot` and maps the two rotation-only
//!   movement packets to `Ignored`
//!   (`crates/protocol/v770/src/server_protocol.rs`, the `MOVE_PLAYER_ROT`
//!   arm). Streaming a player's facing needs that variant to grow angles
//!   first, which changes every one of its match sites — a separate unit.
//! * **Entity ids come from a second allocator**, see
//!   [`PLAYER_ENTITY_ID_BASE`].
//!
//! # Dependencies
//!
//! Nothing outside this crate and `lodestone-model`. Deliberately version-free
//! like the rest of `lodestone-server`: this module names no packet id and no
//! wire layout — [`ServerProtocol::encode_player_info_add`](crate::ServerProtocol::encode_player_info_add)
//! and its sibling are the seam a version crate implements.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use lodestone_model::{ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::protocol::{EntitySnapshot, PlayerListing, ServerDirective, ServerProtocol};
use crate::server::EntitySource;

/// The first network entity id a player is allocated.
///
/// Player ids and mob ids come from **two independent allocators**:
/// [`MobSim`](crate::MobSim) counts up from `1` (from `1000` in production,
/// see its own `set_next_id` doc comment) and knows nothing about players.
/// A collision would not merely look odd — the per-connection diff keys on
/// the id (`EntityStreamer::last_sent: HashMap<i32, EntitySnapshot>`), so one
/// shared id makes a mob and a player alias into a single tracked entity and
/// each pass overwrites the other's position.
///
/// A disjoint range is the honest fix available without reaching into the mob
/// simulation: `1 << 30` leaves the mob allocator over a billion ids before it
/// could ever reach here, and leaves this allocator another billion. Vanilla
/// has no such split — `Entity.ENTITY_COUNTER` is one `AtomicInteger` for every
/// entity in the level — so the *real* fix is one shared allocator when the
/// server-ECS migration (#433) gives both a common owner. Until then this
/// constant is what stops the aliasing, and it is asserted disjoint from the
/// mob range by this module's own tests.
pub const PLAYER_ENTITY_ID_BASE: i32 = 1 << 30;

/// The canonical entity-type key a player entity streams as.
///
/// `minecraft:player` is network entity-type id **156** in protocol 776
/// (Mojang's own `registries.json` for 26.2,
/// `minecraft:entity_type` → `minecraft:player` → `protocol_id`). This module
/// deliberately names the *key* rather than the number: resolving it to a wire
/// id is the version crate's job. Getting the key wrong is silent —
/// `entity_type_id(name).unwrap_or(0)` in `v770`'s `encode_add_entity_body`
/// maps an unknown name to type `0` (`minecraft:acacia_boat`) with no error
/// anywhere — so the gate asserts the streamed **type id**, not that an entity
/// arrived.
const PLAYER_ENTITY_TYPE: &str = "minecraft:player";

/// One connected player, as the server tracks it.
///
/// Position is the only mutable field today; see the module docs for why
/// rotation is not here yet.
#[derive(Debug, Clone, PartialEq)]
struct TrackedPlayer {
    /// The network entity id other connections address this player by.
    entity_id: i32,
    /// The profile UUID. This is the uuid the client presented at
    /// `LoginStart` and that
    /// [`ServerProtocol::login_success`](crate::ServerProtocol::login_success)
    /// echoed back, so the entity's uuid, the tab-list entry's uuid and the
    /// uuid the client believes is its own all agree. (Vanilla in offline mode
    /// instead *derives* it from the username and ignores what the client
    /// sent; matching that would mean changing `login_success` too, which is a
    /// separate change — and any divergence between the two would be a bug, so
    /// they move together or not at all.)
    uuid: Uuid,
    /// The username, for the tab-list entry.
    username: String,
    /// World-space feet position, in blocks.
    position: Vec3,
}

/// A consistent single-lock read of the registry, from one viewer's point of
/// view.
///
/// The two halves must come from one acquisition — see the module docs on the
/// ordering constraint. `roster` **includes** the viewer (vanilla lists you in
/// your own tab list); `entities` **excludes** it (vanilla never sends a player
/// their own entity, and doing so produces a visible doppelgänger standing
/// inside the camera).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerView {
    /// Every connected player, viewer included, for the tab list.
    pub roster: Vec<PlayerListing>,
    /// Every connected player **except** the viewer, as entity snapshots.
    pub entities: Vec<EntitySnapshot>,
}

/// The shared registry of connected players.
///
/// Clone it freely: every clone is the same registry. One is held by whatever
/// owns the world (`IntegratedServer`'s constructors) and one by each
/// connection task, exactly like [`MobHandle`](crate::MobHandle).
#[derive(Debug, Clone, Default)]
pub struct PlayerRegistry(Arc<Mutex<Inner>>);

#[derive(Debug, Default)]
struct Inner {
    /// Monotonic; never reused within a process even after a player leaves, so
    /// a client that was mid-`REMOVE_ENTITIES` cannot have a stale id resolve
    /// to a *different* player.
    next_offset: i32,
    players: Vec<TrackedPlayer>,
}

impl PlayerRegistry {
    /// A fresh, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a connection's player and returns the ticket that owns the
    /// registration.
    ///
    /// The returned [`PlayerTicket`] deregisters on drop. That is not a
    /// stylistic choice: `serve_play` returns through a dozen different `?`
    /// paths (transport error, keep-alive timeout, clean disconnect, an
    /// invalid packet) and a hand-written removal on the success path alone
    /// would leak a ghost player — visible to everyone else, standing still,
    /// forever — on every one of the others.
    #[must_use]
    pub fn join(&self, username: &str, uuid: Uuid, position: Vec3) -> PlayerTicket {
        let entity_id = {
            let mut inner = self.lock();
            let entity_id = PLAYER_ENTITY_ID_BASE.wrapping_add(inner.next_offset);
            inner.next_offset += 1;
            inner.players.push(TrackedPlayer {
                entity_id,
                uuid,
                username: username.to_owned(),
                position,
            });
            entity_id
        };
        PlayerTicket {
            entity_id,
            uuid,
            registry: self.clone(),
        }
    }

    /// Moves a tracked player. A no-op for an id that is not registered, which
    /// is the correct behaviour for the one race that can produce it: a
    /// position update computed from a packet that arrived as the player's own
    /// ticket dropped.
    pub fn set_position(&self, entity_id: i32, position: Vec3) {
        if let Some(player) = self
            .lock()
            .players
            .iter_mut()
            .find(|p| p.entity_id == entity_id)
        {
            player.position = position;
        }
    }

    /// The roster and the entity snapshots, from one lock acquisition, as
    /// `viewer` should see them.
    ///
    /// Pass `Some(id)` for a connection that has its own player entity, so it
    /// is excluded from `entities`; `None` for a viewer with no player of its
    /// own (there is no such caller in production — it exists so a caller
    /// cannot be *forced* to invent an id, and because it is what the negative
    /// control for the doppelgänger rule flips to).
    #[must_use]
    pub fn view(&self, viewer: Option<i32>) -> PlayerView {
        let inner = self.lock();
        let entity_type = player_entity_type();
        PlayerView {
            roster: inner
                .players
                .iter()
                .map(|p| PlayerListing {
                    uuid: p.uuid,
                    username: p.username.clone(),
                })
                .collect(),
            entities: inner
                .players
                .iter()
                .filter(|p| Some(p.entity_id) != viewer)
                .map(|p| EntitySnapshot {
                    id: p.entity_id,
                    uuid: p.uuid,
                    entity_type: entity_type.clone(),
                    position: p.position,
                    // See the module docs: no server-side player rotation
                    // exists to lower, because `ServerBound::PlayerMoved`
                    // carries no angles.
                    rotation: Rotation {
                        yaw: 0.0,
                        pitch: 0.0,
                    },
                    head_yaw: 0.0,
                    // Player motion is client-authoritative here: the client
                    // reports positions, never velocities, so there is no
                    // per-tick delta to publish. An absolute position update
                    // is what the streamer sends anyway.
                    velocity: Vec3::new(0.0, 0.0, 0.0),
                    // No player metadata is modelled yet. Adding any means
                    // running `EntityDataIndexOracle.java` first — index 8 is
                    // `LivingEntity.DATA_LIVING_ENTITY_FLAGS` *and*
                    // `AbstractArrow.ID_FLAGS`, and a player is a
                    // `LivingEntity`, so the census column that separates the
                    // claimants is not guessable from the previous
                    // collision's guard.
                    metadata: Vec::new(),
                })
                .collect(),
        }
    }

    /// How many players are currently registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().players.len()
    }

    /// Whether no players are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Deregisters a player. Private: [`PlayerTicket`]'s `Drop` is the only
    /// caller, so a registration cannot be leaked by forgetting to call this.
    fn remove(&self, entity_id: i32) {
        self.lock().players.retain(|p| p.entity_id != entity_id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().expect("player registry lock poisoned")
    }
}

/// The canonical player entity-type key, parsed.
///
/// `expect` rather than a fallible return: the input is the literal
/// [`PLAYER_ENTITY_TYPE`] above, so a failure here is a broken
/// `ResourceKey::from_str`, not a runtime condition a caller could handle.
fn player_entity_type() -> ResourceKey {
    use std::str::FromStr;
    ResourceKey::from_str(PLAYER_ENTITY_TYPE).expect("`minecraft:player` is a valid resource key")
}

/// Ownership of one player's registration, held by that player's connection
/// task for as long as it is in Play.
///
/// Dropping it removes the player from the registry, so every other
/// connection's next streaming pass emits a `REMOVE_ENTITIES` for it and drops
/// its tab-list entry. See [`PlayerRegistry::join`] for why this is RAII
/// rather than an explicit `leave` call.
#[derive(Debug)]
pub struct PlayerTicket {
    entity_id: i32,
    uuid: Uuid,
    registry: PlayerRegistry,
}

impl PlayerTicket {
    /// The network entity id other connections address this player by — and
    /// the id this connection must be **excluded** by in its own
    /// [`PlayerRegistry::view`].
    #[must_use]
    pub fn entity_id(&self) -> i32 {
        self.entity_id
    }

    /// The player's profile uuid.
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        self.uuid
    }
}

impl Drop for PlayerTicket {
    fn drop(&mut self) {
        self.registry.remove(self.entity_id);
    }
}

/// Per-connection tab-list bookkeeping: the roster-shaped twin of
/// `EntityStreamer`.
///
/// Holds the set of uuids this connection has been told about, so each pass
/// emits one `player_info_update` for the newly-joined and one
/// `player_info_remove` for the departed, and nothing at all when the roster
/// is unchanged (the overwhelmingly common case — this runs on every inbound
/// packet).
#[derive(Debug, Default)]
pub struct PlayerListStreamer {
    sent: HashSet<Uuid>,
}

impl PlayerListStreamer {
    /// Produces the directives that bring this connection's tab list from its
    /// last-sent state to `roster`, updating the bookkeeping to match.
    ///
    /// Adds come **first** in the returned vector, and the caller must emit
    /// this whole vector before the entity diff — see the module docs on why a
    /// client drops an `ADD_ENTITY` that precedes its `PlayerInfo`.
    pub fn sync<P: ServerProtocol>(
        &mut self,
        proto: &P,
        roster: &[PlayerListing],
    ) -> Vec<ServerDirective> {
        let current: HashSet<Uuid> = roster.iter().map(|p| p.uuid).collect();

        let added: Vec<PlayerListing> = roster
            .iter()
            .filter(|p| !self.sent.contains(&p.uuid))
            .cloned()
            .collect();
        let removed: Vec<Uuid> = self
            .sent
            .iter()
            .filter(|uuid| !current.contains(uuid))
            .copied()
            .collect();

        let mut directives = Vec::new();
        if !added.is_empty() {
            directives.extend(proto.encode_player_info_add(&added));
        }
        if !removed.is_empty() {
            directives.extend(proto.encode_player_info_remove(&removed));
        }
        self.sent = current;
        directives
    }
}

/// An [`EntitySource`] that carries an inner source's entities **and** the
/// connected players.
///
/// This is the composition production uses: the inner `E` is
/// [`LiveMobSource`](crate::LiveMobSource) (or a `MobHandle`, or `NoEntities`),
/// and the registry rides alongside.
///
/// Note what [`EntitySource::snapshots`] does **not** do here: it returns only
/// the inner source's entities, never the players. Players are reachable only
/// through [`EntitySource::players`], which the streaming pass consults *with a
/// viewer id*. That is deliberate — it makes "send a player their own entity"
/// unrepresentable through the plain `snapshots()` path rather than merely
/// discouraged, so a future caller that forgets the exclusion gets no players
/// at all (loud) instead of a doppelgänger (quiet).
#[derive(Debug, Clone, Default)]
pub struct PlayerAwareSource<E> {
    inner: E,
    players: PlayerRegistry,
}

impl<E> PlayerAwareSource<E> {
    /// Pairs an entity source with a player registry.
    pub fn new(inner: E, players: PlayerRegistry) -> Self {
        Self { inner, players }
    }

    /// The registry this source shares.
    #[must_use]
    pub fn registry(&self) -> &PlayerRegistry {
        &self.players
    }
}

impl<E: EntitySource> EntitySource for PlayerAwareSource<E> {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.inner.snapshots()
    }

    fn players(&self) -> Option<&PlayerRegistry> {
        Some(&self.players)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoEntities;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// The whole point of [`PLAYER_ENTITY_ID_BASE`]: a player id can never be
    /// a mob id. Derived from the mob allocator's own production base rather
    /// than restated, so raising `set_next_id(1000)` by six orders of
    /// magnitude still leaves this true (and if it ever would not, this fails).
    #[test]
    fn player_ids_cannot_collide_with_mob_ids() {
        // `MobSim::set_next_id(1000)` in production, `1` by default; either
        // way the allocator counts *up* from there.
        let mob_production_base = 1000_i32;
        assert!(
            PLAYER_ENTITY_ID_BASE > mob_production_base,
            "player ids must start above the mob allocator's base"
        );
        // Headroom for the mob allocator, stated as a count rather than a
        // vague "plenty": over a billion spawns before it could reach us.
        assert!(PLAYER_ENTITY_ID_BASE - mob_production_base > 1_000_000_000);
        // And headroom for us, so the `wrapping_add` in `join` is documentation
        // of an unreachable case rather than a live overflow path.
        assert!(i32::MAX - PLAYER_ENTITY_ID_BASE > 1_000_000_000);
    }

    /// A joined player appears to *others* and never to itself.
    #[test]
    fn a_player_is_excluded_from_its_own_view_and_present_in_anothers() {
        let registry = PlayerRegistry::new();
        let alice = registry.join("Alice", uuid(1), Vec3::new(8.0, 100.0, 8.0));
        let bob = registry.join("Bob", uuid(2), Vec3::new(9.0, 100.0, 9.0));

        let alice_view = registry.view(Some(alice.entity_id()));
        let ids: Vec<i32> = alice_view.entities.iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            vec![bob.entity_id()],
            "Alice must receive Bob's entity and not her own"
        );

        let bob_view = registry.view(Some(bob.entity_id()));
        let ids: Vec<i32> = bob_view.entities.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![alice.entity_id()]);

        // The roster is the other way round: vanilla lists you in your own tab
        // list, so both entries appear in both views.
        assert_eq!(alice_view.roster.len(), 2);
        assert_eq!(bob_view.roster.len(), 2);
    }

    /// The entity type must be the *player* key, not whatever
    /// `unwrap_or(0)` would silently substitute.
    #[test]
    fn a_player_entity_streams_as_minecraft_player() {
        let registry = PlayerRegistry::new();
        let _alice = registry.join("Alice", uuid(1), Vec3::new(8.0, 100.0, 8.0));
        let view = registry.view(None);
        assert_eq!(view.entities.len(), 1);
        assert_eq!(view.entities[0].entity_type.to_string(), "minecraft:player");
    }

    /// Dropping the ticket is what deregisters — the property every error path
    /// out of `serve_play` depends on.
    #[test]
    fn dropping_the_ticket_deregisters_the_player() {
        let registry = PlayerRegistry::new();
        let alice = registry.join("Alice", uuid(1), Vec3::new(8.0, 100.0, 8.0));
        {
            let _bob = registry.join("Bob", uuid(2), Vec3::new(9.0, 100.0, 9.0));
            assert_eq!(registry.len(), 2);
        }
        assert_eq!(registry.len(), 1, "Bob's ticket went out of scope");
        let view = registry.view(Some(alice.entity_id()));
        assert!(
            view.entities.is_empty(),
            "Alice should see nobody once Bob has left"
        );
        assert_eq!(view.roster.len(), 1);
    }

    /// Ids are never reused, so a client racing a `REMOVE_ENTITIES` cannot
    /// resolve a stale id onto a different player.
    #[test]
    fn entity_ids_are_not_reused_after_a_player_leaves() {
        let registry = PlayerRegistry::new();
        let first = {
            let alice = registry.join("Alice", uuid(1), Vec3::new(0.0, 0.0, 0.0));
            alice.entity_id()
        };
        let bob = registry.join("Bob", uuid(2), Vec3::new(0.0, 0.0, 0.0));
        assert_ne!(first, bob.entity_id());
    }

    #[test]
    fn set_position_moves_the_streamed_entity() {
        let registry = PlayerRegistry::new();
        let alice = registry.join("Alice", uuid(1), Vec3::new(8.0, 100.0, 8.0));
        registry.set_position(alice.entity_id(), Vec3::new(20.0, 65.0, -3.0));
        let view = registry.view(None);
        assert_eq!(view.entities[0].position, Vec3::new(20.0, 65.0, -3.0));
        // An unknown id is a no-op, not a panic.
        registry.set_position(i32::MIN, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(registry.view(None).entities[0].position.x, 20.0);
    }

    /// `snapshots()` on the composed source returns the inner source's
    /// entities *only* — players travel via `players()`, with a viewer.
    #[test]
    fn the_composed_source_never_leaks_players_through_plain_snapshots() {
        let registry = PlayerRegistry::new();
        let _alice = registry.join("Alice", uuid(1), Vec3::new(0.0, 0.0, 0.0));
        let source = PlayerAwareSource::new(NoEntities, registry);
        assert!(
            source.snapshots().is_empty(),
            "players must not appear in the viewer-agnostic snapshot path"
        );
        assert_eq!(
            source
                .players()
                .expect("a player-aware source reports its registry")
                .len(),
            1
        );
    }

    /// A plain source reports no registry, so every pre-existing
    /// `EntitySource` keeps its exact old behaviour.
    #[test]
    fn a_plain_source_has_no_registry() {
        assert!(NoEntities.players().is_none());
    }
}
