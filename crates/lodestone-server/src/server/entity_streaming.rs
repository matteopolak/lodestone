//! The entity-visibility source trait and the per-connection streamer that spawns, moves and despawns entities for one client.

use super::*;

/// A read-only view of the entities in the world right now, supplied by the
/// caller that owns the simulation and its tick.
///
/// [`serve_connection`] reads snapshots each streaming pass and diffs them
/// against what *this* connection was last sent; it never ticks the simulation
/// itself, so one shared world can feed many connections without double-ticking.
pub trait EntitySource: Send + Sync {
    /// The entities that should currently be visible to the client.
    fn snapshots(&self) -> Vec<EntitySnapshot>;

    /// Returns a publication revision and its snapshots, or `None` when that
    /// connection has already consumed the publication. `None` as the input
    /// requests an initial snapshot, including an empty one.
    ///
    /// Sources with a publication counter must read the counter and snapshots
    /// atomically. The default always reads snapshots, ignoring the cursor,
    /// so sources mutated directly retain their per-pass visibility. Revisions
    /// are local to one source and a connection must not reuse them across sources.
    fn snapshots_if_changed(
        &self,
        _previous_revision: Option<u64>,
    ) -> Option<(u64, Vec<EntitySnapshot>)> {
        Some((0, self.snapshots()))
    }

    /// The registry of connected **players**, if this source tracks them
    /// (the player registry is optional).
    ///
    /// This is the one conduit by which a connection reaches the players
    /// sharing its world, and it exists as a defaulted method on this trait
    /// rather than a new parameter for a specific reason:
    /// [`serve_connection`] and its five sibling entry points are called
    /// directly from `crates/protocol/v770/tests/*`, and adding an argument
    /// would have churned every one of those call sites for a feature none of
    /// them uses. Every pre-existing [`EntitySource`] — [`NoEntities`],
    /// [`crate::LiveMobSource`], [`MobHandle`], and every test double —
    /// inherits `None` and keeps its exact previous behaviour.
    ///
    /// Returning `Some` is what turns on player streaming for a connection:
    /// [`crate::players::PlayerAwareSource`] is the composition production
    /// uses, pairing the mob source with the registry. Note that players are
    /// deliberately **not** reachable through [`snapshots`](Self::snapshots) —
    /// that call has no viewer, and a viewer-less player list is exactly how a
    /// connection would be sent its own entity. See
    /// [`crate::players::PlayerRegistry::view`].
    fn players(&self) -> Option<&PlayerRegistry> {
        None
    }

    /// The boss bars a client should currently hold — the dragon fight's own
    /// health bar, and any future producer this trait gains. Defaulted to
    /// empty for the identical reason [`players`](Self::players) is: adding a
    /// method here must not force every existing implementor (including the
    /// version-crate test doubles) to grow one.
    ///
    /// [`crate::mobs::MobSim::boss_bars`] is today's one real producer, reached
    /// through [`crate::mobs::LiveMobSource`]/[`crate::mobs::MobHandle`].
    fn boss_bars(&self) -> Vec<BossBarSnapshot> {
        Vec::new()
    }
}

pub(super) struct ActiveEntities<'a, E> {
    pub(super) fallback: &'a E,
    pub(super) runtime: Option<Arc<crate::dimension_runtime::DimensionRuntime>>,
    pub(super) players: Option<&'a PlayerRegistry>,
}

impl<'a, E: EntitySource> ActiveEntities<'a, E> {
    pub(super) fn new(world: &'a crate::world_state::WorldStateHandle, fallback: &'a E, dimension: crate::dimension::Dimension) -> Self {
        let runtime = world.dimension_runtime(dimension);
        let players = if runtime.is_some() { Some(world.player_registry()) } else { fallback.players() };
        Self { fallback, runtime, players }
    }
}

impl<E: EntitySource> EntitySource for ActiveEntities<'_, E> {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        match &self.runtime {
            Some(runtime) => runtime.entities().snapshots(),
            None => self.fallback.snapshots(),
        }
    }

    fn snapshots_if_changed(&self, previous_revision: Option<u64>) -> Option<(u64, Vec<EntitySnapshot>)> {
        match &self.runtime {
            Some(runtime) => runtime.entities().snapshots_if_changed(previous_revision),
            None => self.fallback.snapshots_if_changed(previous_revision),
        }
    }

    fn players(&self) -> Option<&PlayerRegistry> { self.players }

    fn boss_bars(&self) -> Vec<BossBarSnapshot> {
        match &self.runtime {
            Some(runtime) => runtime.entities().boss_bars(),
            None => self.fallback.boss_bars(),
        }
    }
}

/// Runs one full streaming pass for a connection: tab-list diff first, then the
/// entity diff over the mob source **and** every other connected player.
///
/// The order is load-bearing and the reason this is one function rather than
/// two call sites. A client that receives an `ADD_ENTITY` of type
/// `minecraft:player` before it holds a `PlayerInfo` for that uuid **discards
/// the spawn** — vanilla's own client-side "create entity from packet" step
/// returns `null`
/// and logs "Server attempted to add player prior to sending player info".
/// So the roster adds must precede the
/// spawn, in the same pass, and both must come from
/// [`crate::players::PlayerRegistry::view`]'s single lock acquisition — two
/// separate reads could interleave a join between them and produce precisely
/// the dropped spawn.
///
/// `ticket` is this connection's own player registration; its id is what gets
/// excluded, so a connection never receives itself.
pub(super) fn stream_pass<P, E>(
    proto: &P,
    entities: &E,
    streamer: &mut EntityStreamer,
    player_list: &mut PlayerListStreamer,
    ticket: Option<&PlayerTicket>,
) -> Vec<ServerDirective>
where
    P: ServerProtocol,
    E: EntitySource,
{
    let mut directives = Vec::new();
    if let Some(registry) = entities.players() {
        let publication = entities.snapshots_if_changed(streamer.last_publication);
        let view = registry.view(ticket.map(PlayerTicket::entity_id));
        directives.extend(player_list.sync(proto, &view.roster));
        directives.extend(streamer.sync_with_players(
            proto,
            publication
                .as_ref()
                .map(|(_, snapshots)| snapshots.as_slice()),
            &view.entities,
        ));
        if let Some((revision, _)) = publication {
            streamer.last_publication = Some(revision);
        }
    } else if let Some((revision, snapshots)) =
        entities.snapshots_if_changed(streamer.last_publication)
    {
        directives.extend(streamer.sync(proto, &snapshots));
        streamer.last_publication = Some(revision);
    }
    directives.extend(streamer.sync_boss_bars(proto, &entities.boss_bars()));
    directives
}

/// An [`EntitySource`] carrying no entities — for callers that only stream
/// terrain (the existing chunk-only behaviour).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEntities;

impl EntitySource for NoEntities {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        Vec::new()
    }
}

/// Per-connection bookkeeping that turns "the entities right now" into the
/// spawn / update / remove directives this client still needs, given what it was
/// already sent.
///
/// The server owns the lifecycle; [`ServerProtocol`] encodes individual packets.
/// Each entity's last-sent snapshot lives in exactly one of the source maps.
/// Player state can therefore advance without recapturing a mob publication
/// or interpreting its absent snapshot list as an entity removal.
#[derive(Debug, Default)]
pub(super) struct EntityStreamer {
    pub(super) last_sent: HashMap<i32, EntitySnapshot>,
    pub(super) players_last_sent: HashMap<i32, EntitySnapshot>,
    pub(super) last_publication: Option<u64>,
    /// The boss bars this connection has been sent `ADD` for and not yet
    /// `REMOVE` — the same last-sent-state shape [`last_sent`](Self::last_sent)
    /// keeps for entities, one level simpler (a bar has no spawn/update split
    /// on the wire, only add/update-progress/remove). See
    /// [`sync_boss_bars`](Self::sync_boss_bars).
    pub(super) boss_bars_sent: HashMap<uuid::Uuid, BossBarSnapshot>,
}

impl EntityStreamer {
    pub(super) fn reset_dimension<P: ServerProtocol>(&mut self, proto: &P) -> Vec<ServerDirective> {
        let removed_bars = self.sync_boss_bars(proto, &[]);
        self.last_sent.clear();
        self.players_last_sent.clear();
        self.last_publication = None;
        self.boss_bars_sent.clear();
        removed_bars
    }

    /// Produces the directives that bring the client from its last-sent state to
    /// `current`, updating the bookkeeping to match.
    pub(super) fn sync<P: ServerProtocol>(
        &mut self,
        proto: &P,
        current: &[EntitySnapshot],
    ) -> Vec<ServerDirective> {
        self.sync_with_players(proto, Some(current), &[])
    }

    /// An absent publication leaves its entities untouched. Player snapshots
    /// are always current, and removals from both sources share one packet.
    pub(super) fn sync_with_players<P: ServerProtocol>(
        &mut self,
        proto: &P,
        current: Option<&[EntitySnapshot]>,
        players: &[EntitySnapshot],
    ) -> Vec<ServerDirective> {
        let mut directives = Vec::new();
        let mut removed = current
            .map(|snapshots| Self::remove_vanished(&mut self.last_sent, snapshots))
            .unwrap_or_default();
        removed.extend(Self::remove_vanished(&mut self.players_last_sent, players));
        if !removed.is_empty() {
            directives.push(proto.encode_remove_entity(&removed));
        }
        if let Some(current) = current {
            Self::sync_updates(proto, &mut self.last_sent, current, &mut directives);
        }
        Self::sync_updates(proto, &mut self.players_last_sent, players, &mut directives);
        directives
    }

    pub(super) fn remove_vanished(
        last_sent: &mut HashMap<i32, EntitySnapshot>,
        current: &[EntitySnapshot],
    ) -> Vec<i32> {
        let live: HashSet<i32> = current.iter().map(|e| e.id).collect();
        let removed: Vec<i32> = last_sent
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        for id in &removed {
            last_sent.remove(id);
        }
        removed
    }

    pub(super) fn sync_updates<P: ServerProtocol>(
        proto: &P,
        last_sent: &mut HashMap<i32, EntitySnapshot>,
        current: &[EntitySnapshot],
        directives: &mut Vec<ServerDirective>,
    ) {
        // Spawns and updates, in the source's iteration order.
        for entity in current {
            match last_sent.get(&entity.id) {
                None => {
                    directives.push(proto.encode_add_entity(entity));
                    // The spawn frame carries no metadata, so send a second
                    // `SET_ENTITY_DATA` directive whenever the snapshot has
                    // non-default values. `encode_add_entity` returns exactly
                    // one `ServerDirective`, so the metadata stays separate.
                    if !entity.metadata.is_empty() {
                        directives.push(proto.encode_set_entity_data(entity.id, &entity.metadata));
                    }
                    // A snapshot with a leash link needs both the spawn and link
                    // frames, so a client whose first visible snapshot contains
                    // `leash_link` renders the rope immediately.
                    if entity.leash_link.is_some() {
                        directives.push(proto.encode_set_entity_link(entity.id, entity.leash_link));
                    }
                    last_sent.insert(entity.id, entity.clone());
                }
                Some(prev) if prev != entity => {
                    directives.extend(proto.encode_entity_update(Some(prev), entity));
                    // A metadata-only change (e.g. a creeper's
                    // `swell_dir` climbing while it stands still) still
                    // takes this branch — `EntitySnapshot`'s `PartialEq`
                    // covers `metadata` too — so this check is independent
                    // of whether position/rotation also changed this tick.
                    if prev.metadata != entity.metadata {
                        directives.push(proto.encode_set_entity_data(entity.id, &entity.metadata));
                    }
                    // A leash-link transition covers both attachment (`None` →
                    // `Some`) and detachment (`Some` → `None`); the encoder
                    // chooses the wire representation for an absent holder.
                    if prev.leash_link != entity.leash_link {
                        directives.push(proto.encode_set_entity_link(entity.id, entity.leash_link));
                    }
                    last_sent.insert(entity.id, entity.clone());
                }
                Some(_) => {}
            }
        }
    }

    /// The `BOSS_EVENT` twin of [`sync`](Self::sync) — diffs `current` against
    /// what this connection was last sent and returns the add/update/remove
    /// directives that close the gap.
    ///
    /// The boss-event packet carries no "visible" bit of its own
    /// (see [`BossBarSnapshot`]'s own doc): a bar this pass reports
    /// `visible: false` is therefore removed (or, if it was never added,
    /// simply never added) rather than sent with a false flag, and a bar
    /// whose id vanished from `current` entirely — the boss despawned — is
    /// removed the same way an entity id vanishing from [`sync`](Self::sync)'s
    /// `current` triggers `REMOVE_ENTITIES`.
    pub(super) fn sync_boss_bars<P: ServerProtocol>(
        &mut self,
        proto: &P,
        current: &[BossBarSnapshot],
    ) -> Vec<ServerDirective> {
        let mut directives = Vec::new();

        let live: HashSet<uuid::Uuid> = current.iter().map(|b| b.id).collect();
        let vanished: Vec<uuid::Uuid> = self
            .boss_bars_sent
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        for id in vanished {
            self.boss_bars_sent.remove(&id);
            directives.push(proto.encode_boss_event_remove(id));
        }

        for bar in current {
            match self.boss_bars_sent.get(&bar.id) {
                None if bar.visible => {
                    directives.push(proto.encode_boss_event_add(bar.id, &bar.name, bar.progress));
                    self.boss_bars_sent.insert(bar.id, bar.clone());
                }
                // Never added and still not visible: nothing to do, and
                // nothing to remember — matches vanilla never broadcasting an
                // invisible `ServerBossEvent` to a player in the first place.
                None => {}
                Some(_) if !bar.visible => {
                    directives.push(proto.encode_boss_event_remove(bar.id));
                    self.boss_bars_sent.remove(&bar.id);
                }
                Some(prev) if prev.progress != bar.progress => {
                    directives.push(proto.encode_boss_event_update_progress(bar.id, bar.progress));
                    self.boss_bars_sent.insert(bar.id, bar.clone());
                }
                Some(_) => {}
            }
        }

        directives
    }
}
