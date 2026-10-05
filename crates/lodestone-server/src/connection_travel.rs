use super::*;
use std::{future::Future, pin::Pin, task::{Context, Poll}};

#[derive(Clone)]
pub(super) enum Destination {
    Home,
    Dimension(Arc<dyn ChunkSource>),
}

enum PreparationSource<'a> {
    Borrowed(&'a dyn ChunkSource),
    Owned(Arc<dyn ChunkSource>),
}

impl<'a> PreparationSource<'a> {
    fn home<S: ChunkSource + 'static>(source: SourceRef<'a, S>) -> Self {
        match source.shared_arc() {
            Some(source) => Self::Owned(source),
            None => Self::Borrowed(source.get()),
        }
    }

    fn get(&self) -> &dyn ChunkSource {
        match self {
            Self::Borrowed(source) => *source,
            Self::Owned(source) => source.as_ref(),
        }
    }

    async fn admit(&self, coordinates: Vec<(i32, i32)>) -> Result<(), ChunkEncodeError> {
        match self {
            Self::Owned(source) => {
                crate::join_scheduler::admit_owned_columns(Arc::clone(source), coordinates).await
            }
            Self::Borrowed(source) => {
                #[cfg(not(target_arch = "wasm32"))]
                let _ = generate_columns_parallel(*source, &coordinates);
                #[cfg(target_arch = "wasm32")]
                let _ = generate_columns_borrowed(*source, &coordinates).await;
                Ok(())
            }
        }
    }

    async fn query<R: Send + 'static>(
        &self,
        query: impl FnOnce(&dyn ChunkSource) -> R + Send + 'static,
    ) -> R {
        match self {
            Self::Borrowed(source) => query(*source),
            Self::Owned(source) => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let source = Arc::clone(source);
                    crate::spawn::spawn_worldgen(move || query(source.as_ref())).await
                }
                #[cfg(target_arch = "wasm32")]
                {
                    query(source.as_ref())
                }
            }
        }
    }
}

pub(super) enum PreparedTravel {
    Dimension { destination: Destination, dimension: crate::dimension::Dimension, position: Vec3 },
    Gateway(Vec3),
}

#[cfg(not(target_arch = "wasm32"))]
type Preparation<'a> = Pin<Box<dyn Future<Output = Result<Option<PreparedTravel>, ChunkEncodeError>> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
type Preparation<'a> = Pin<Box<dyn Future<Output = Result<Option<PreparedTravel>, ChunkEncodeError>> + 'a>>;

struct PendingTravel<'a> {
    epoch: u64,
    contact: BlockPos,
    future: Preparation<'a>,
}

pub(super) struct TravelController<'a> {
    portal: crate::portal::PortalTracker,
    gateway_cooldown: u8,
    end_exit_contact: bool,
    active: Destination,
    next: Option<Destination>,
    epoch: u64,
    pending: Option<PendingTravel<'a>>,
}

impl<'a> TravelController<'a> {
    pub(super) fn new<S: ChunkSource + 'static>(source: SourceRef<'_, S>) -> Self {
        Self {
            portal: crate::portal::PortalTracker::new(),
            gateway_cooldown: 0,
            end_exit_contact: false,
            active: match source {
                SourceRef::Dimension(source) => Destination::Dimension(Arc::clone(source)),
                SourceRef::Borrowed(_) | SourceRef::Shared(_) => Destination::Home,
            },
            next: None,
            epoch: 0,
            pending: None,
        }
    }

    pub(super) fn source(&self) -> Option<Arc<dyn ChunkSource>> {
        match &self.active {
            Destination::Home => None,
            Destination::Dimension(source) => Some(Arc::clone(source)),
        }
    }

    pub(super) fn promote(&mut self) {
        if let Some(destination) = self.next.take() {
            self.active = destination;
        }
    }

    pub(super) fn stage(&mut self, destination: Destination) {
        self.epoch = self.epoch.wrapping_add(1);
        self.pending = None;
        self.next = Some(destination);
    }

    pub(super) fn is_preparing(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn poll_prepared(&mut self, cx: &mut Context<'_>, position: Option<(f64, f64, f64)>) -> Poll<Result<Option<PreparedTravel>, ChunkEncodeError>> {
        self.cancel_if_left(position);
        let Some(pending) = self.pending.as_mut() else { return Poll::Pending; };
        let Poll::Ready(result) = pending.future.as_mut().poll(cx) else { return Poll::Pending; };
        let pending = self.pending.take().expect("completed travel owns its request");
        if pending.epoch != self.epoch {
            return Poll::Ready(Ok(None));
        }
        Poll::Ready(result)
    }

    fn current_source<S: ChunkSource + 'static>(&self, home: SourceRef<'a, S>) -> PreparationSource<'a> {
        match self.source() {
            Some(source) => PreparationSource::Owned(source),
            None => PreparationSource::home(home),
        }
    }

    fn start(&mut self, contact: BlockPos, future: Preparation<'a>) {
        self.pending = Some(PendingTravel { epoch: self.epoch, contact, future });
    }

    fn cancel_if_left(&mut self, position: Option<(f64, f64, f64)>) {
        let feet = position.map(|(x, y, z)| BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32));
        if self.pending.as_ref().is_some_and(|pending| Some(pending.contact) != feet) {
            self.epoch = self.epoch.wrapping_add(1);
            self.pending = None;
        }
    }

    pub(super) fn tick<S: ChunkSource + 'static>(
        &mut self,
        home: SourceRef<'a, S>,
        current: SourceRef<'_, S>,
        block_entities: &BlockEntityHandle,
        mobs: &MobHandle,
        player_entity_id: i32,
        position: Option<(f64, f64, f64)>,
        game_mode: GameMode,
        world: &crate::world_state::WorldStateHandle,
    ) {
        self.gateway_cooldown = self.gateway_cooldown.saturating_sub(1);
        let feet = position.map(|(x, y, z)| BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32));
        self.cancel_if_left(position);
        if self.is_preparing() { return; }
        let Some(feet) = feet else { self.portal.tick(None, 0); return; };
        let Some(state) = resident_block_state(current.get(), feet.x, feet.y, feet.z) else {
            let source = self.current_source(home);
            self.start(feet, Box::pin(async move {
                source.admit(vec![(feet.x.div_euclid(16), feet.z.div_euclid(16))]).await?;
                Ok(None)
            }));
            return;
        };
        if self.gateway_cooldown == 0
            && end_gateway_contact_allowed(current.dimension(), true)
            && !player_has_mount(mobs, player_entity_id)
            && let Some((exit, exact)) = end_gateway_exit_resident(current.get(), block_entities, feet)
        {
            let source = self.current_source(home);
            self.start(feet, Box::pin(async move {
                source.admit(crate::portal::end_gateway_required_columns(exit)).await?;
                let position = crate::portal::end_gateway_arrival_in_resident_world(source.get(), exit, exact);
                Ok(position.map(PreparedTravel::Gateway))
            }));
            return;
        }
        let end = crate::portal::is_end_portal(state);
        let contact = (end || crate::portal::is_portal(state)).then_some(feet);
        if !end && contact.is_some() && let Some(index) = current.get().portal_index() {
            index.insert(current.dimension(), feet);
        }
        let rules = world.rules();
        let delay = if end { 0 } else if Abilities::for_mode(game_mode).invulnerable {
            rules.players_nether_portal_creative_delay()
        } else {
            rules.players_nether_portal_default_delay()
        }.max(0);
        if self.portal.tick(contact, delay).is_none() { return; }
        let from = current.dimension();
        if end {
            if from == crate::dimension::Dimension::End {
                self.end_exit_contact = true;
                return;
            }
            let Some(destination) = home.get().sibling(crate::dimension::Dimension::End) else { return; };
            let mobs = if world.dimension_runtime(home.dimension()).is_some() {
                world.ensure_dimension_runtime(crate::dimension::Dimension::End).mobs().clone()
            } else {
                mobs.clone()
            };
            self.start(feet, Box::pin(prepare_end(destination, mobs)));
        } else if rules.allow_entering_nether_using_portals() || from == crate::dimension::Dimension::Nether {
            let to = from.nether_portal_destination();
            let (destination, source) = if to == home.dimension() {
                (Destination::Home, PreparationSource::home(home))
            } else {
                let Some(source) = home.get().sibling(to) else { return; };
                (Destination::Dimension(Arc::clone(&source)), PreparationSource::Owned(source))
            };
            let index = current.get().portal_index().cloned();
            let axis = crate::portal::Axis::from_state(state);
            self.start(feet, Box::pin(prepare_nether(source, destination, from, to, position.unwrap(), axis, index)));
        }
    }

    /// Whether the player touched the End's exit portal since the last call.
    pub(super) fn take_end_exit_contact(&mut self) -> bool {
        std::mem::take(&mut self.end_exit_contact)
    }

    pub(super) fn arrived(&mut self, dimension_changed: bool) {
        if dimension_changed { self.portal.begin_cooldown(); }
        else { self.gateway_cooldown = END_GATEWAY_CONTACT_COOLDOWN; }
    }
}

/// The game-event id that opens the credits.
const WIN_GAME_EVENT: u8 = 4;
/// Its parameter. The current client shows the credits whatever it says; older
/// clients show them for `1.0` and answer `0.0` with an immediate respawn
/// request, so `1.0` is right for both.
const WIN_GAME_SHOW_CREDITS: f32 = 1.0;

/// Per-connection state of leaving the End through the exit portal.
///
/// A player who has not yet seen the credits gets a two-step handshake: the
/// server announces the win, and the client answers with its perform-respawn
/// command once the credits are dismissed. `won` bridges the two so a player
/// standing in the portal does not re-announce every tick, and so a
/// perform-respawn from a living player who is not leaving the End stays
/// ignored. A player who has seen them is sent straight home like any other
/// portal traveller, with no announcement.
pub(super) struct EndExit {
    credits_seen: bool,
    won: bool,
    respawn_requested: bool,
}

impl EndExit {
    pub(super) fn new(credits_seen: bool) -> Self {
        Self { credits_seen, won: false, respawn_requested: false }
    }

    /// Starts an exit. `Some(true)` means the win must be announced, `Some(false)`
    /// that the player goes straight home, and `None` that an exit is already
    /// waiting on the client.
    fn begin(&mut self) -> Option<bool> {
        if self.won { return None; }
        if self.credits_seen {
            self.respawn_requested = true;
            return Some(false);
        }
        self.won = true;
        self.credits_seen = true;
        Some(true)
    }

    pub(super) fn is_won(&self) -> bool { self.won }

    /// Records the client's perform-respawn answer to the announcement.
    pub(super) fn request_respawn(&mut self) {
        if self.won { self.respawn_requested = true; }
    }

    /// Takes a pending move home, ending the exit.
    pub(super) fn take_respawn(&mut self) -> bool {
        let requested = std::mem::take(&mut self.respawn_requested);
        if requested { self.won = false; }
        requested
    }
}

/// The player-data key recording that the credits were shown to this player.
pub(super) const SEEN_CREDITS_FIELD: &str = "seenCredits";

pub(super) fn credits_seen_in(preserved: &[(String, lodestone_core::Nbt)]) -> bool {
    preserved.iter().any(|(key, value)| {
        key == SEEN_CREDITS_FIELD && matches!(value, lodestone_core::Nbt::Byte(flag) if *flag != 0)
    })
}

/// Starts an exit: announces the win and records the credits as seen (in
/// `preserved`, so the save carries it), or, for a player who has seen them,
/// queues the move home.
pub(super) async fn begin_end_exit<T: Transport, P: ServerProtocol>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    exit: &mut EndExit,
    preserved: &mut Vec<(String, lodestone_core::Nbt)>,
) -> Result<(), ServerError> {
    let Some(announce) = exit.begin() else { return Ok(()); };
    if !announce { return Ok(()); }
    apply(conn, state, proto.encode_game_event(WIN_GAME_EVENT, WIN_GAME_SHOW_CREDITS)).await?;
    if !credits_seen_in(preserved) {
        preserved.retain(|(key, _)| key != SEEN_CREDITS_FIELD);
        preserved.push((SEEN_CREDITS_FIELD.to_owned(), lodestone_core::Nbt::Byte(1)));
    }
    Ok(())
}

/// A request, made by a respawn, for the connection loop to rebuild its
/// dimension view around a new position.
#[derive(Debug, Clone)]
pub(super) struct DimensionReset {
    /// Where the player's feet go.
    pub target: Vec3,
    /// Which source the view is rebuilt in.
    pub route: crate::respawn::Route,
}

/// Performs a respawn: resolves the player's respawn point ([`crate::respawn::plan`]),
/// tells the client, and returns the dimension reset the connection loop must
/// run when the player did not stay in the view they were already in.
///
/// `keep_data` marks the End exit, which keeps everything the player carries:
/// no anchor charge is spent and no depletion sound plays. Both that and a
/// death clear a point that turns out to be unusable and send the client the
/// no-respawn-block event first, as the respawn packet follows it.
pub(super) async fn perform_respawn<T: Transport, P: ServerProtocol>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    home: &dyn ChunkSource,
    current: &dyn ChunkSource,
    respawn: &mut Option<crate::world_spawn::RespawnPoint>,
    world_spawn: Vec3,
    game_mode: GameMode,
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
    block_ticks: &crate::tick::BlockTickFeed,
    keep_data: bool,
) -> Result<Option<DimensionReset>, ServerError> {
    use crate::respawn::Route;
    let can_travel = proto.supports_dimension_change();
    let plan = crate::respawn::plan(home, current, *respawn, world_spawn, !keep_data, can_travel);
    let current_is_home = current.dimension() == home.dimension();
    let in_current_view = match plan.route {
        Route::Home => current_is_home,
        Route::Current => true,
        Route::Sibling(_) => false,
    };
    if plan.missing_block {
        *respawn = None;
        apply(conn, state, proto.encode_game_event(NO_RESPAWN_BLOCK_GAME_EVENT, 0.0)).await?;
    }
    if let Some((at, before, after)) = plan.spent {
        // The block feed serves the dimension being viewed. A charge spent in
        // another dimension reaches its viewers through that dimension's own
        // chunks. In view, the neighbours are notified so a comparator reading
        // the anchor follows the new charge.
        if in_current_view {
            block_ticks.publish_change(at.x, at.y, at.z, before, after);
            let viewed = if current_is_home { home } else { current };
            let (changed, scheduled) = super::propagate_placement_with_entities(viewed, at, None);
            block_ticks.request_scheduled_ticks(scheduled);
            for (pos, new_state) in changed {
                block_ticks.publish(pos.x, pos.y, pos.z, new_state);
            }
        }
    }
    // A death that lands at home is an ordinary respawn frame; every other
    // landing is a dimension change, which makes the client drop its chunks.
    let respawn_frame = !keep_data && matches!(plan.route, Route::Home);
    let frames = if respawn_frame {
        proto.encode_respawn_with_teleport_id(issue_teleport_id(teleport_acknowledgements), plan.feet)
    } else {
        proto.encode_dimension_change_with_teleport_id(
            issue_teleport_id(teleport_acknowledgements), plan.dimension.key(), plan.feet, game_mode,
        )
    };
    if frames.is_empty() { return Ok(None); }
    for directive in frames { apply(conn, state, directive).await?; }
    if let (Some(anchor), false) = (plan.anchor, keep_data) {
        // Only the respawning player hears the depletion.
        apply(conn, state, proto.encode_world_effect(&crate::respawn_anchor::deplete_sound(anchor))).await?;
    }
    let rebuild = !respawn_frame || !in_current_view;
    Ok(rebuild.then_some(DimensionReset { target: plan.feet, route: plan.route }))
}

/// The game event telling the client its respawn block was missing or
/// obstructed.
const NO_RESPAWN_BLOCK_GAME_EVENT: u8 = 0;

struct ResidentQuery<'a> {
    source: &'a dyn ChunkSource,
    unavailable: std::sync::atomic::AtomicU8,
}

impl ResidentQuery<'_> {
    fn new(source: &dyn ChunkSource) -> ResidentQuery<'_> {
        ResidentQuery { source, unavailable: std::sync::atomic::AtomicU8::new(0) }
    }

    fn complete(&self) -> bool { self.unavailable.load(Ordering::Relaxed) == 0 }
}

impl ChunkSource for ResidentQuery<'_> {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        match resident_column(self.source, cx, cz) {
            Some(column) => column,
            None => {
                self.unavailable.fetch_or(1, Ordering::Relaxed);
                let dimension = self.source.dimension().unwrap_or(crate::dimension::Dimension::Overworld);
                ChunkColumn::new(dimension.min_y(), dimension.height())
            }
        }
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        let dimension = self.source.dimension().unwrap_or(crate::dimension::Dimension::Overworld);
        if y < dimension.min_y() || y >= dimension.min_y() + dimension.height() { return StateId::AIR; }
        let state = match self.source.try_resident_block_state_id(x, y, z) {
            Some(crate::chunk_store::TryResident::Present(state)) => Some(state),
            Some(crate::chunk_store::TryResident::Busy) => {
                self.unavailable.fetch_or(2, Ordering::Relaxed);
                return StateId::AIR;
            }
            Some(crate::chunk_store::TryResident::Absent) => None,
            None => self.source.resident_block_state_id(x, y, z),
        };
        state.unwrap_or_else(|| {
            self.unavailable.fetch_or(1, Ordering::Relaxed);
            StateId::AIR
        })
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String { crate::chunk::DEFAULT_BIOME.to_owned() }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) { unreachable!("travel query is read-only"); }
    fn is_column_resident(&self, cx: i32, cz: i32) -> bool {
        match self.source.try_resident_column_presence(cx, cz) {
            Some(crate::chunk_store::TryResident::Present(())) => true,
            Some(crate::chunk_store::TryResident::Busy) => {
                self.unavailable.fetch_or(2, Ordering::Relaxed);
                false
            }
            Some(crate::chunk_store::TryResident::Absent) => {
                self.unavailable.fetch_or(1, Ordering::Relaxed);
                false
            }
            None => self.source.is_column_resident(cx, cz),
        }
    }
    fn dimension(&self) -> Option<crate::dimension::Dimension> { self.source.dimension() }
}

fn rectangle_columns(x_lo: i32, x_hi: i32, z_lo: i32, z_hi: i32) -> Vec<(i32, i32)> {
    (x_lo.div_euclid(16)..=x_hi.div_euclid(16))
        .flat_map(|cx| (z_lo.div_euclid(16)..=z_hi.div_euclid(16)).map(move |cz| (cx, cz)))
        .collect()
}

fn creation_columns(origin: BlockPos, axis: crate::portal::Axis) -> Vec<(i32, i32)> {
    let (x_hi, z_hi) = match axis { crate::portal::Axis::X => (18, 17), crate::portal::Axis::Z => (17, 18) };
    rectangle_columns(origin.x - 17, origin.x + x_hi, origin.z - 17, origin.z + z_hi)
}

async fn wait_for_resident_write() {
    #[cfg(not(target_arch = "wasm32"))]
    tokio::time::sleep(Duration::from_millis(1)).await;
    #[cfg(target_arch = "wasm32")]
    lodestone_time::browser_sleep(Duration::from_millis(1)).await;
}

async fn apply_writes(
    source: &PreparationSource<'_>,
    mut writes: impl Iterator<Item = (BlockPos, StateId)> + Clone + Send,
    stop_when_fight_started: bool,
) -> Result<bool, ChunkEncodeError> {
    let mut completed = 0usize;
    while let Some((pos, state)) = writes.next() {
        loop {
            if stop_when_fight_started && source.get().dragon_fight_started() == Some(true) { return Ok(false); }
            let mutation = match source.get().try_resident_block_state_id(pos.x, pos.y, pos.z) {
                Some(crate::chunk_store::TryResident::Present(current)) if current == state => break,
                Some(crate::chunk_store::TryResident::Busy) => crate::chunk_store::TryBlockMutation::Busy,
                Some(crate::chunk_store::TryResident::Absent) => crate::chunk_store::TryBlockMutation::Absent,
                _ => match source.get().try_set_block(pos.x, pos.y, pos.z, state) {
                    Some(mutation) => mutation,
                    None => {
                        if source.get().resident_block_state_id(pos.x, pos.y, pos.z) != Some(state) {
                            source.get().set_block(pos.x, pos.y, pos.z, state);
                        }
                        break;
                    }
                },
            };
            match mutation {
                crate::chunk_store::TryBlockMutation::Applied => break,
                crate::chunk_store::TryBlockMutation::Busy => wait_for_resident_write().await,
                crate::chunk_store::TryBlockMutation::Absent => {
                    let mut columns = vec![(pos.x.div_euclid(16), pos.z.div_euclid(16))];
                    for (remaining, _) in writes.clone().take(TICK_BLOCK_UPDATE_BATCH) {
                        let coordinate = (remaining.x.div_euclid(16), remaining.z.div_euclid(16));
                        if !columns.contains(&coordinate) { columns.push(coordinate); }
                    }
                    source.admit(columns).await?;
                    wait_for_resident_write().await;
                }
                crate::chunk_store::TryBlockMutation::Unsupported => {
                    return Err(ChunkEncodeError::new(format!("portal destination does not support resident mutation at {pos:?}")));
                }
            }
        }
        completed += 1;
        if completed.is_multiple_of(TICK_BLOCK_UPDATE_BATCH) {
            #[cfg(not(target_arch = "wasm32"))]
            tokio::task::yield_now().await;
            #[cfg(target_arch = "wasm32")]
            lodestone_time::browser_yield().await;
        }
    }
    Ok(true)
}

async fn prepare_nether(
    source: PreparationSource<'_>,
    destination: Destination,
    from: crate::dimension::Dimension,
    to: crate::dimension::Dimension,
    position: (f64, f64, f64),
    axis: crate::portal::Axis,
    index: Option<crate::portal::PortalIndex>,
) -> Result<Option<PreparedTravel>, ChunkEncodeError> {
    let Some((x, y, z)) = crate::dimension::scaled_destination(from, to, position.0, position.1, position.2) else { return Ok(None); };
    let approximate = BlockPos::new(x, y, z);
    source.admit(crate::portal::find_exit_portal_required_columns(to, index.as_ref(), approximate)).await?;
    let search_index = index.clone();
    let existing = source.query(move |source| {
        let query = ResidentQuery::new(source);
        let existing = crate::portal::find_exit_portal(&query, to, search_index.as_ref(), approximate);
        query.complete().then_some(existing)
    }).await;
    let Some(existing) = existing else { return Ok(None); };
    let resolved = if let Some(existing) = existing {
        let Some(state) = resident_block_state(source.get(), existing.x, existing.y, existing.z) else { return Ok(None); };
        let exit_axis = crate::portal::Axis::from_state(state);
        let reach = crate::portal::MAX_WIDTH;
        let columns = match exit_axis {
            crate::portal::Axis::X => rectangle_columns(existing.x - reach, existing.x + reach, existing.z, existing.z),
            crate::portal::Axis::Z => rectangle_columns(existing.x, existing.x, existing.z - reach, existing.z + reach),
        };
        source.admit(columns).await?;
        source.query(move |source| {
            let query = ResidentQuery::new(source);
            if !crate::portal::is_portal(query.block_state_id(existing.x, existing.y, existing.z)) { return None; }
            let (corner, _, _) = crate::portal::largest_rectangle_around(&query, existing, exit_axis);
            query.complete().then_some(crate::portal::PortalDestination {
                position: Vec3::new(f64::from(corner.x) + 0.5, f64::from(corner.y), f64::from(corner.z) + 0.5),
                created: None,
                dimension: to,
            })
        }).await
    } else {
        source.admit(creation_columns(approximate, axis)).await?;
        source.query(move |source| {
            let query = ResidentQuery::new(source);
            let created = crate::portal::create_portal(&query, to, approximate, axis)?;
            query.complete().then_some(crate::portal::PortalDestination {
                position: Vec3::new(f64::from(created.origin.x) + 0.5, f64::from(created.origin.y), f64::from(created.origin.z) + 0.5),
                created: Some(created),
                dimension: to,
            })
        }).await
    };
    let Some(resolved) = resolved else { return Ok(None); };
    if let Some(created) = resolved.created {
        apply_writes(&source, created.blocks.iter().copied(), false).await?;
        if let Some(index) = index { index.extend(to, created.portal_cells); }
    }
    Ok(Some(PreparedTravel::Dimension { destination, dimension: to, position: resolved.position }))
}

async fn prepare_end(source: Arc<dyn ChunkSource>, mobs: MobHandle) -> Result<Option<PreparedTravel>, ChunkEncodeError> {
    let destination = Destination::Dimension(Arc::clone(&source));
    let source = PreparationSource::Owned(source);
    let (origin, position) = crate::portal::end_portal_arrival();
    source.admit(crate::portal::end_platform_required_columns(origin)).await?;
    let platform = crate::portal::end_platform_writes(origin);
    apply_writes(&source, platform.iter().copied(), false).await?;
    if source.get().dragon_fight_started() != Some(true) {
        let seed = crate::worldgen_data::active_world_seed();
        let arena_origin = Vec3::new(0.0, 64.0, 0.0);
        let blocks = crate::mobs::MobSim::end_dragon_fight_block_writes(
            seed, arena_origin, crate::dimension::Dimension::End.min_y(),
        );
        let mut admission = HashSet::new();
        for write in &blocks {
            admission.extend(column_admission_footprint(write.x.div_euclid(16), write.z.div_euclid(16), 1));
        }
        source.admit(admission.into_iter().collect()).await?;
        let complete = apply_writes(&source, blocks.iter().map(|write| {
            (BlockPos::new(write.x, write.y, write.z), write.state)
        }), true).await?;
        if complete && source.get().claim_dragon_fight_start() {
            mobs.with(|sim| { sim.spawn_end_dragon_fight(seed, arena_origin); });
        }
    }
    Ok(Some(PreparedTravel::Dimension { destination, dimension: crate::dimension::Dimension::End, position }))
}

pub(super) async fn reset_stream<T: Transport, P: ServerProtocol, S: ChunkSource + 'static>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    position: Vec3,
    view: &mut ViewTracker,
    stream: &mut crate::join_scheduler::JoinChunkStream<S>,
    encodes: &mut PendingJoinEncodes<'_>,
    batches: &mut VecDeque<PendingChunkBatch>,
    awaiting_ack: &mut bool,
    relights: &mut PendingRelights,
    tick_updates: &mut VecDeque<crate::tick::TickBlockChange>,
) -> Result<(), ServerError> {
    encodes.clear();
    batches.clear();
    *awaiting_ack = false;
    relights.clear();
    tick_updates.clear();
    for &(cx, cz) in &view.loaded { apply(conn, state, proto.encode_forget_chunk(cx, cz)).await?; }
    let center = ((position.x / 16.0).floor() as i32, (position.z / 16.0).floor() as i32);
    apply(conn, state, proto.encode_chunk_cache_center(center.0, center.1)).await?;
    view.reset(center);
    let rings = join_view_rings(view.radius).into_iter()
        .map(|ring| ring.into_iter().map(|(dx, dz)| (center.0 + dx, center.1 + dz)).collect())
        .collect();
    *stream = crate::join_scheduler::JoinChunkStream::ringed(rings);
    Ok(())
}

pub(super) fn reset_player(
    position: Vec3,
    dimension: crate::dimension::Dimension,
    player_pos: &mut Option<(f64, f64, f64)>,
    movement: &mut ClientMovement,
    fall: &mut FallTracker,
    client_loaded: &mut bool,
    // Whether the client must report loaded again; a protocol with no such
    // packet stays loaded across the reset.
    reload_required: bool,
    world: &crate::world_state::WorldStateHandle,
    players: Option<&PlayerRegistry>,
    player_entity_id: i32,
) {
    *player_pos = Some((position.x, position.y, position.z));
    *movement = ClientMovement::default();
    fall.reset();
    *client_loaded = !reload_required;
    publish_presence(world, players, player_entity_id, dimension, position);
}

fn publish_presence(
    world: &crate::world_state::WorldStateHandle,
    players: Option<&PlayerRegistry>,
    player_entity_id: i32,
    dimension: crate::dimension::Dimension,
    position: Vec3,
) {
    if let Some(players) = players { players.set_presence(player_entity_id, dimension, position); }
    if world.dimension_runtime(dimension).is_some() { return; }
    world.tick_anchors().publish(players.map_or_else(|| vec![crate::tick_area::TickAnchor {
        dimension,
        cx: (position.x / 16.0).floor() as i32,
        cz: (position.z / 16.0).floor() as i32,
    }], PlayerRegistry::tick_anchors));
}

pub(super) struct TravelArrival {
    pub(super) dimension: crate::dimension::Dimension,
}

pub(super) fn ticket_store_for_source(
    source: &dyn ChunkSource,
    home: &dyn ChunkSource,
    compatibility: &TicketStoreHandle,
) -> Result<TicketStoreHandle, ChunkEncodeError> {
    match source.ticket_store() {
        Some(store) => {
            if let (Some(dimension), Some(home_dimension), Some(home_store)) =
                (source.dimension(), home.dimension(), home.ticket_store())
                && dimension != home_dimension && store.same_store(&home_store)
            {
                return Err(ChunkEncodeError::new("different dimensions require different ticket stores"));
            }
            Ok(store)
        }
        None if home.ticket_store().is_some() => Err(ChunkEncodeError::new(
            "a ticket-backed world requires a destination ticket store",
        )),
        None => Ok(compatibility.clone()),
    }
}

pub(super) fn prepare_ticket_transfer(
    ticket: &PlayerTicketGuard,
    home: &dyn ChunkSource,
    destination: &dyn ChunkSource,
    position: Vec3,
    radius: i32,
) -> Result<crate::ticket::PlayerTicketTransfer, ChunkEncodeError> {
    let store = ticket_store_for_source(destination, home, ticket.compatibility_store())?;
    let transfer = ticket.prepare_transfer(&store,
        ((position.x / 16.0).floor() as i32, (position.z / 16.0).floor() as i32),
        radius, radius.clamp(0, crate::chunk_store::CONCURRENT_TICK_RADIUS));
    if transfer.changes_store() { destination.reconcile_ticket_residency(); }
    Ok(transfer)
}

pub(super) fn finish_ticket_transfer(
    ticket: &mut PlayerTicketGuard,
    transfer: crate::ticket::PlayerTicketTransfer,
    origin: &dyn ChunkSource,
    destination: &dyn ChunkSource,
) {
    let changed = transfer.changes_store();
    ticket.commit_transfer(transfer);
    destination.reconcile_ticket_residency();
    if changed { origin.reconcile_ticket_residency(); }
}

pub(super) async fn commit<T: Transport, P: ServerProtocol, S: ChunkSource + 'static>(
    travel: &mut TravelController<'_>,
    prepared: PreparedTravel,
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    home: SourceRef<'_, S>,
    state: &mut State,
    view: &mut ViewTracker,
    stream: &mut crate::join_scheduler::JoinChunkStream<S>,
    encodes: &mut PendingJoinEncodes<'_>,
    batches: &mut VecDeque<PendingChunkBatch>,
    awaiting_ack: &mut bool,
    relights: &mut PendingRelights,
    tick_updates: &mut VecDeque<crate::tick::TickBlockChange>,
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
    player_pos: &mut Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    movement: &mut ClientMovement,
    fall: &mut FallTracker,
    client_loaded: &mut bool,
    game_mode: GameMode,
    world: &crate::world_state::WorldStateHandle,
    ticket: &mut PlayerTicketGuard,
    streamer: &mut EntityStreamer,
    players: Option<&PlayerRegistry>,
    player_entity_id: i32,
) -> Result<Option<TravelArrival>, ServerError> {
    match prepared {
        PreparedTravel::Dimension { destination, dimension, position } => {
            let change = proto.encode_dimension_change_with_teleport_id(
                issue_teleport_id(teleport_acknowledgements), dimension.key(), position, game_mode,
            );
            if change.is_empty() { return Ok(None); }
            let destination_source = match &destination {
                Destination::Home => home.get(),
                Destination::Dimension(source) => source.as_ref(),
            };
            let transfer = prepare_ticket_transfer(ticket, home.get(), destination_source, position, view.radius)?;
            for directive in streamer.reset_dimension(proto) { apply(conn, state, directive).await?; }
            for directive in change { apply(conn, state, directive).await?; }
            reset_stream(conn, proto, state, position, view, stream, encodes, batches,
                awaiting_ack, relights, tick_updates).await?;
            finish_ticket_transfer(ticket, transfer, source.get(), destination_source);
            reset_player(position, dimension, player_pos, movement, fall, client_loaded, proto.sends_player_loaded(), world, players, player_entity_id);
            travel.stage(destination);
            travel.arrived(true);
            Ok(Some(TravelArrival { dimension }))
        }
        PreparedTravel::Gateway(position) => {
            let rotation = player_rot.unwrap_or_default();
            apply(conn, state, proto.encode_teleport_with_id(
                issue_teleport_id(teleport_acknowledgements), position.x, position.y, position.z,
                rotation.yaw, rotation.pitch,
            )).await?;
            *player_pos = Some((position.x, position.y, position.z));
            *movement = ClientMovement::default();
            fall.reset();
            let previous_center = view.center;
            let update = view.recenter(proto, (position.x / 16.0).floor() as i32,
                (position.z / 16.0).floor() as i32, player_rot.map(|rotation| rotation.yaw));
            if view.center != previous_center {
                ticket.move_to_with_simulation_radius(view.center, view.radius,
                    view.radius.clamp(0, crate::chunk_store::CONCURRENT_TICK_RADIUS));
                source.get().reconcile_ticket_residency();
            }
            send_view_update(conn, proto, source, Some(stream), state, view, update,
                awaiting_ack, batches).await?;
            publish_presence(world, players, player_entity_id, source.dimension(), position);
            travel.arrived(false);
            Ok(Some(TravelArrival { dimension: source.dimension() }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dimension::Dimension;

    struct TravelWorld {
        dimension: Dimension,
        tickets: Option<TicketStoreHandle>,
        blocks: Mutex<HashMap<BlockPos, StateId>>,
        admissions: Mutex<Vec<(i32, i32)>>,
        availability: std::sync::atomic::AtomicU8,
        mutation_budget: std::sync::atomic::AtomicUsize,
        mutations: std::sync::atomic::AtomicUsize,
        mutation_blocked: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        fight_started: std::sync::atomic::AtomicBool,
        fight_claims: std::sync::atomic::AtomicUsize,
    }

    impl TravelWorld {
        fn new(dimension: Dimension) -> Self {
            Self { dimension, tickets: None, blocks: Mutex::new(HashMap::new()), admissions: Mutex::new(Vec::new()),
                availability: std::sync::atomic::AtomicU8::new(0),
                mutation_budget: std::sync::atomic::AtomicUsize::new(usize::MAX),
                mutations: std::sync::atomic::AtomicUsize::new(0), mutation_blocked: Mutex::new(None),
                fight_started: std::sync::atomic::AtomicBool::new(false),
                fight_claims: std::sync::atomic::AtomicUsize::new(0) }
        }

        fn portal(&self, origin: BlockPos) {
            for x in 0..2 { for y in 0..3 {
                self.set_block(origin.x + x, origin.y + y, origin.z, crate::portal::portal_state(crate::portal::Axis::X));
            } }
        }
    }

    impl ChunkSource for TravelWorld {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            self.admissions.lock().unwrap().push((cx, cz));
            let mut column = ChunkColumn::new(self.dimension.min_y(), self.dimension.height());
            let marker = if self.dimension == Dimension::Nether { Block::Netherrack } else { Block::Stone };
            column.set_block_id(0, 0, 0, marker.default_state());
            for (pos, state) in self.blocks.lock().unwrap().iter() {
                if (pos.x.div_euclid(16), pos.z.div_euclid(16)) == (cx, cz) {
                    column.set_block_id(pos.x.rem_euclid(16), pos.y, pos.z.rem_euclid(16), *state);
                }
            }
            column
        }
        fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
            self.blocks.lock().unwrap().get(&BlockPos::new(x, y, z)).copied().unwrap_or(StateId::AIR)
        }
        fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
            Some(self.block_state_id(x, y, z))
        }
        fn try_resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<crate::chunk_store::TryResident<StateId>> {
            Some(match self.availability.load(Ordering::Relaxed) {
                1 => crate::chunk_store::TryResident::Absent,
                2 => crate::chunk_store::TryResident::Busy,
                _ => crate::chunk_store::TryResident::Present(self.block_state_id(x, y, z)),
            })
        }
        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String { crate::chunk::DEFAULT_BIOME.to_owned() }
        fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
            self.blocks.lock().unwrap().insert(BlockPos::new(x, y, z), state);
        }
        fn try_set_block(&self, x: i32, y: i32, z: i32, state: StateId) -> Option<crate::chunk_store::TryBlockMutation> {
            if self.availability.load(Ordering::Relaxed) == 3 {
                return Some(crate::chunk_store::TryBlockMutation::Unsupported);
            }
            if self.mutations.load(Ordering::Relaxed) >= self.mutation_budget.load(Ordering::Relaxed) {
                if let Some(sender) = self.mutation_blocked.lock().unwrap().take() { let _ = sender.send(()); }
                return Some(crate::chunk_store::TryBlockMutation::Busy);
            }
            self.set_block(x, y, z, state);
            self.mutations.fetch_add(1, Ordering::Relaxed);
            Some(crate::chunk_store::TryBlockMutation::Applied)
        }
        fn dragon_fight_started(&self) -> Option<bool> { Some(self.fight_started.load(Ordering::Acquire)) }
        fn claim_dragon_fight_start(&self) -> bool {
            self.fight_claims.fetch_add(1, Ordering::Relaxed);
            !self.fight_started.swap(true, Ordering::AcqRel)
        }
        fn dimension(&self) -> Option<Dimension> { Some(self.dimension) }
        fn ticket_store(&self) -> Option<TicketStoreHandle> { self.tickets.clone() }
        fn sibling(&self, dimension: Dimension) -> Option<Arc<dyn ChunkSource>> { Some(Arc::new(Self::new(dimension))) }
    }

    #[test]
    fn ticket_resolution_preserves_no_store_sources_but_rejects_a_missing_production_sibling() {
        let mut home = TravelWorld::new(Dimension::Overworld);
        let mut destination = TravelWorld::new(Dimension::End);
        let compatibility = TicketStoreHandle::new();
        assert!(ticket_store_for_source(&destination, &home, &compatibility).unwrap().same_store(&compatibility));
        home.tickets = Some(TicketStoreHandle::new());
        assert!(ticket_store_for_source(&destination, &home, &compatibility).is_err());
        let resolved = ticket_store_for_source(&home, &home, &compatibility).unwrap();
        assert!(resolved.same_store(home.tickets.as_ref().unwrap()));
        assert!(!resolved.same_store(&compatibility));
        destination.tickets = Some(resolved);
        assert!(ticket_store_for_source(&destination, &home, &compatibility).is_err());
        destination.tickets = Some(TicketStoreHandle::new());
        assert!(ticket_store_for_source(&destination, &home, &compatibility).unwrap().same_store(destination.tickets.as_ref().unwrap()));
    }

    #[test]
    fn resident_query_distinguishes_busy_from_absent_and_rejects_incomplete_air() {
        let source = TravelWorld::new(Dimension::Overworld);
        for (availability, flags) in [(1, 1), (2, 2)] {
            source.availability.store(availability, Ordering::Relaxed);
            let query = ResidentQuery::new(&source);
            assert_eq!(query.block_state_id(19, 70, -8), StateId::AIR);
            assert!(!query.complete());
            assert_eq!(query.unavailable.load(Ordering::Relaxed), flags);
            assert!(source.admissions.lock().unwrap().is_empty());
        }
        source.availability.store(0, Ordering::Relaxed);
        let query = ResidentQuery::new(&source);
        assert_eq!(query.block_state_id(19, 70, -8), StateId::AIR);
        assert!(query.complete(), "available air is distinct from an unavailable resident");
    }

    #[tokio::test]
    async fn destination_writes_defer_busy_readmit_absent_and_decline_unsupported() {
        let source = TravelWorld::new(Dimension::End);
        let preparation = PreparationSource::Borrowed(&source);
        let mut cx = Context::from_waker(std::task::Waker::noop());
        let writes = [(BlockPos::new(31, 63, -1), Block::Obsidian.default_state()),
            (BlockPos::new(32, 63, -17), Block::Obsidian.default_state())];
        source.availability.store(2, Ordering::Relaxed);
        let mut busy = Box::pin(apply_writes(&preparation, writes.iter().copied(), false));
        assert!(busy.as_mut().poll(&mut cx).is_pending());
        assert!(source.admissions.lock().unwrap().is_empty(), "busy is not a missing-column request");
        source.availability.store(0, Ordering::Relaxed);
        assert!(busy.await.unwrap());
        source.blocks.lock().unwrap().clear();
        source.availability.store(1, Ordering::Relaxed);
        let mut absent = Box::pin(apply_writes(&preparation, writes.iter().copied(), false));
        assert!(absent.as_mut().poll(&mut cx).is_pending());
        let admissions = source.admissions.lock().unwrap();
        assert_eq!(admissions.len(), 2);
        assert!(admissions.contains(&(1, -1)) && admissions.contains(&(2, -2)));
        drop(admissions);
        source.availability.store(0, Ordering::Relaxed);
        assert!(absent.await.unwrap());
        source.blocks.lock().unwrap().clear();
        source.availability.store(3, Ordering::Relaxed);
        assert!(apply_writes(&preparation, writes.iter().copied(), false).await.is_err());
        assert!(source.blocks.lock().unwrap().is_empty(), "unsupported cannot fall through to a generating write");
    }

    #[tokio::test]
    async fn cancelling_end_arena_preparation_preserves_edits_without_consuming_the_claim() {
        let home = TravelWorld::new(Dimension::Overworld);
        let end = Arc::new(TravelWorld::new(Dimension::End));
        end.mutation_budget.store(25, Ordering::Relaxed);
        let (sender, blocked) = tokio::sync::oneshot::channel();
        *end.mutation_blocked.lock().unwrap() = Some(sender);
        let mut travel = TravelController::new(SourceRef::Borrowed(&home));
        let contact = BlockPos::new(4, 70, -8);
        travel.start(contact, Box::pin(prepare_end(end.clone(), MobHandle::default())));
        {
            let preparation = std::future::poll_fn(|cx| travel.poll_prepared(cx, Some((4.5, 70.0, -7.5))));
            tokio::pin!(preparation);
            tokio::select! {
                _ = &mut preparation => panic!("arena writes must suspend at the fixture gate"),
                result = blocked => result.expect("the arena mutation gate was reached"),
            }
        }
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(travel.poll_prepared(&mut cx, Some((5.5, 70.0, -7.5))).is_pending());
        assert!(!travel.is_preparing());
        assert_eq!(end.fight_claims.load(Ordering::Relaxed), 0);
        assert_eq!(end.mutations.load(Ordering::Relaxed), 25);
        assert_eq!(end.blocks.lock().unwrap().len(), 25, "completed platform edits are retained, not rolled back");
        end.fight_started.store(true, Ordering::Release);
        assert!(prepare_end(end.clone(), MobHandle::default()).await.unwrap().is_some());
        assert_eq!(end.mutations.load(Ordering::Relaxed), 25, "an initialized fight does not rewrite the arena");
        assert_eq!(end.fight_claims.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn shared_contacts_keep_rule_delay_decay_and_arrival_cooldown() {
        let home = TravelWorld::new(Dimension::Overworld);
        home.portal(BlockPos::new(0, 70, -1));
        let source = SourceRef::Borrowed(&home);
        let world = crate::world_state::WorldStateHandle::new();
        let registry = BlockEntityHandle::new();
        let mobs = MobHandle::default();
        let inside = Some((0.5, 70.0, -0.5));
        let mut travel = TravelController::new(source);
        for tick in 1..=80 {
            travel.tick(source, source, &registry, &mobs, 1, inside, GameMode::Survival, &world);
            assert!(!travel.is_preparing(), "survival tick {tick} precedes tick 81");
        }
        travel.tick(source, source, &registry, &mobs, 1, inside, GameMode::Survival, &world);
        assert!(travel.is_preparing());
        let mut creative = TravelController::new(source);
        creative.tick(source, source, &registry, &mobs, 1, inside, GameMode::Creative, &world);
        assert!(creative.is_preparing(), "creative delay zero triggers on the first tick");
        let mut decaying = TravelController::new(source);
        for _ in 0..11 { decaying.tick(source, source, &registry, &mobs, 1, inside, GameMode::Survival, &world); }
        decaying.tick(source, source, &registry, &mobs, 1, None, GameMode::Survival, &world);
        assert_eq!(decaying.portal.ticks(), 7);
        world.set_rule("players_nether_portal_default_delay", "7").unwrap();
        decaying.tick(source, source, &registry, &mobs, 1, inside, GameMode::Survival, &world);
        assert!(decaying.is_preparing(), "an updated delay applies to accumulated exposure");
        decaying.stage(Destination::Home);
        decaying.arrived(true);
        for _ in 0..40 {
            decaying.tick(source, source, &registry, &mobs, 1, inside, GameMode::Creative, &world);
            assert!(!decaying.is_preparing());
        }
    }

    #[test]
    fn leaving_contact_cancels_preparation_and_the_disabled_control_delivers_stale_work() {
        let home = TravelWorld::new(Dimension::Overworld);
        let mut travel = TravelController::new(SourceRef::Borrowed(&home));
        let origin = BlockPos::new(5, 71, -8);
        let mut cx = Context::from_waker(std::task::Waker::noop());
        let ready = || Box::pin(async { Ok(Some(PreparedTravel::Gateway(Vec3::new(100.5, 51.0, 0.5)))) }) as Preparation<'_>;
        travel.start(origin, ready());
        assert!(travel.poll_prepared(&mut cx, Some((5.5, 71.0, -6.5))).is_pending());
        assert!(!travel.is_preparing());
        travel.start(origin, ready());
        let control = travel.pending.as_mut().unwrap().future.as_mut().poll(&mut cx);
        assert!(matches!(control, Poll::Ready(Ok(Some(PreparedTravel::Gateway(_))))),
            "bypassing the contact fence must expose the obsolete arrival");
        travel.stage(Destination::Home);
        assert!(!travel.is_preparing());
    }

    #[tokio::test]
    async fn shared_preparation_admits_home_on_the_fractional_negative_return_leg() {
        let home = TravelWorld::new(Dimension::Overworld);
        let nether = TravelWorld::new(Dimension::Nether);
        let index = crate::portal::PortalIndex::new();
        let home_portal = BlockPos::new(1720, 96, -524);
        let nether_portal = BlockPos::new(215, 32, -66);
        home.portal(home_portal);
        nether.portal(nether_portal);
        index.insert(Dimension::Overworld, home_portal);
        index.insert(Dimension::Nether, nether_portal);
        let outbound = prepare_nether(PreparationSource::Borrowed(&nether), Destination::Home,
            Dimension::Overworld, Dimension::Nether, (1720.5, 96.0, -523.25),
            crate::portal::Axis::X, Some(index.clone())).await.unwrap();
        let Some(PreparedTravel::Dimension { position, .. }) = outbound else { panic!("outbound destination required"); };
        assert_eq!(position, Vec3::new(215.5, 32.0, -65.5));
        let outbound_admissions = nether.admissions.lock().unwrap().len();
        let returned = prepare_nether(PreparationSource::Borrowed(&home), Destination::Home,
            Dimension::Nether, Dimension::Overworld, (position.x, position.y, position.z),
            crate::portal::Axis::X, Some(index)).await.unwrap();
        let Some(PreparedTravel::Dimension { destination: Destination::Home, position, .. }) = returned else { panic!("home destination required"); };
        assert_eq!(position, Vec3::new(1720.5, 96.0, -523.5));
        assert!(home.admissions.lock().unwrap().contains(&(107, -33)));
        assert_eq!(nether.admissions.lock().unwrap().len(), outbound_admissions);
        assert_eq!(home.column(0, 0).block_state_id(0, 0, 0), Block::Stone.default_state());
        assert_eq!(nether.column(0, 0).block_state_id(0, 0, 0), Block::Netherrack.default_state());
    }

    #[test]
    fn dimension_and_death_arrival_require_fresh_readiness_without_pausing_the_world() {
        let home = TravelWorld::new(Dimension::Overworld);
        let mut travel = TravelController::new(SourceRef::Borrowed(&home));
        travel.stage(Destination::Dimension(Arc::new(TravelWorld::new(Dimension::Nether))));
        travel.promote();
        assert_eq!(travel.source().unwrap().dimension(), Some(Dimension::Nether));
        let world = crate::world_state::WorldStateHandle::new();
        world.resume_initial_ticks();
        let mut position = Some((199.0, 120.0, 5.0));
        let mut movement = ClientMovement { delta: Vec3::new(4.0, -9.0, 3.0), on_ground: false, received_this_tick: true };
        let mut fall = FallTracker::default();
        let mut loaded = true;
        reset_player(Vec3::new(-31.5, 71.0, 48.5), Dimension::Overworld,
            &mut position, &mut movement, &mut fall, &mut loaded, true, &world, None, LOCAL_PLAYER_ENTITY_ID);
        travel.stage(Destination::Home);
        travel.promote();
        assert!(travel.source().is_none());
        assert!(!loaded);
        assert_eq!(movement, ClientMovement::default());
        assert_eq!(world.tick_anchors().snapshot(), vec![crate::tick_area::TickAnchor { dimension: Dimension::Overworld, cx: -2, cz: 3 }]);
        assert!(!world.initial_ticks_paused());
        assert!(!player_tick_ready(&world, loaded));
        assert!(player_tick_ready(&world, true));
    }

    #[test]
    fn a_first_exit_announces_and_waits_for_the_clients_respawn() {
        let mut exit = EndExit::new(false);
        assert_eq!(exit.begin(), Some(true), "unseen credits are announced");
        assert_eq!(exit.begin(), None, "standing in the portal does not re-announce");
        assert!(!exit.take_respawn(), "nothing moves until the client answers");
        exit.request_respawn();
        assert!(exit.take_respawn());
        assert!(!exit.is_won(), "the exit is over once the player is sent home");
        assert_eq!(exit.begin(), Some(false), "the credits are seen from now on");
    }

    #[test]
    fn a_perform_respawn_with_no_exit_pending_is_ignored() {
        let mut exit = EndExit::new(false);
        exit.request_respawn();
        assert!(!exit.take_respawn());
    }

    #[test]
    fn seen_credits_send_the_player_straight_home() {
        let mut exit = EndExit::new(true);
        assert_eq!(exit.begin(), Some(false));
        assert!(exit.take_respawn(), "no handshake: the move home is queued at once");
        assert!(!exit.is_won());
    }

    #[test]
    fn the_seen_flag_round_trips_through_the_preserved_player_fields() {
        use lodestone_core::Nbt;
        assert!(!credits_seen_in(&[]));
        assert!(!credits_seen_in(&[(SEEN_CREDITS_FIELD.to_owned(), Nbt::Byte(0))]));
        assert!(credits_seen_in(&[(SEEN_CREDITS_FIELD.to_owned(), Nbt::Byte(1))]));
        assert!(!credits_seen_in(&[("other".to_owned(), Nbt::Byte(1))]));
    }
}
