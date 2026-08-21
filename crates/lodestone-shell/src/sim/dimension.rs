//! `Sim`'s dimension cluster: **which dimension we are in, what changes when
//! that changes, and the portal-transition screen effect the trip itself
//! draws.**
//!
//! `use super::*;` for the same reason every other seam file in `sim/` uses it —
//! `sim::dimension` is a descendant of `sim` and already sees `Sim`'s private
//! fields.
//!
//! # There is exactly one source of truth, and it is not here
//!
//! "We are in dimension X" is owned by
//! [`lodestone_ecs::session::ServerDimension`] (and its sibling
//! `ServerDimensionType`), folded from `ClientEvent::Login` **and**
//! `ClientEvent::Respawned` — respawn being how a server reports a portal trip.
//! It surfaces as `PlayerSnapshot::dimension`. Everything in this module reads
//! that through the one accessor [`Sim::dimension`]; nothing here stores a
//! second copy of the identity, which is the defect
//! `docs/dimension-visuals.md` records at length.
//!
//! What this module *does* store is [`Sim::applied_dimension`] — "the dimension
//! whose one-time reset I have already run". That is an **edge detector**, the
//! same shape as `app.rs`'s `applied_fog`, and the distinction matters: an edge
//! detector that disagrees with the source of truth is at worst a missed or
//! repeated reset, whereas a second copy of the identity is a render path
//! reading the wrong dimension forever.
//!
//! # Why the reset needs an edge at all, when fog does not
//!
//! Per-frame reads need no edge — [`Sim::fog_settings`] and
//! [`Sim::sky_mode`] recompute from the source every frame and are always
//! right. The reset is different: dropping the entity index and the meshed
//! columns is a *transition*, not a state, and running it every frame would
//! delete the terrain as fast as it arrived.
//!
//! # A dimension change is not a respawn
//!
//! The two share one packet, so this is the split, and getting it backwards
//! empties the player's inventory on their first portal trip:
//!
//! | survives a dimension change | does not |
//! |---|---|
//! | inventory, XP, health, hunger, air | the entity index |
//! | the local player entity and every component on it | every meshed column |
//! | the session (tab list, scoreboard, chat log) | the chunk store's contents |
//!
//! So the reset here is deliberately **not** [`Sim::end_session`]'s reset with a
//! narrower name. It touches the world and the other entities, and nothing that
//! belongs to the player: no `reset_local_player`, no
//! `insert_session_component_sets`, no vitals. A death-respawn in the *same*
//! dimension runs none of it, which is what the `applied_dimension` comparison
//! buys.

use super::*;

/// Vanilla's `LocalPlayer.handlePortalTransitionEffect` ramp, per tick, while
/// the player stands in a nether portal: `step = 0.0125F`.
///
/// 80 ticks — four seconds — from nothing to full. Not a round `1.0 / 100.0` or
/// a "one second" fade: the number is read off the method, and the asymmetry
/// with [`PORTAL_EFFECT_DECAY_PER_TICK`] is the whole character of the effect.
const PORTAL_EFFECT_RAMP_PER_TICK: f32 = 0.0125;

/// The decay, per tick, once the player is out of the portal: `step = -0.05F`,
/// carried here as its magnitude.
///
/// **Four times faster than the ramp** (20 ticks against 80). That ratio is the
/// reason a linear fade looks wrong at both ends — the effect creeps in and
/// snaps out, and a symmetric curve reads as sluggish on exit and abrupt on
/// entry.
const PORTAL_EFFECT_DECAY_PER_TICK: f32 = 0.05;

impl Sim {
    /// The dimension the local player is in, or `None` before login.
    ///
    /// **The one read** for every consumer that needs the dimension *alone*:
    /// [`Sim::fog_settings`] and [`Sim::sky_mode`] come through here rather than
    /// each reaching into
    /// `net.shared_handle().get().and_then(|h| h.player().dimension)` on their
    /// own, which is how `fog_settings` was written and is how a third consumer
    /// ends up with a fourth subtly different fallback. `None` means "the server
    /// has not told us" and every caller must fall back explicitly — never to a
    /// plausible-looking overworld.
    ///
    /// `Sim::refresh_mesh_policy` deliberately does **not** call this: it needs
    /// the dimension *and* the dimension type, and takes one `player()` snapshot
    /// for both so the two cannot describe two different moments. Splitting it
    /// into this accessor plus a second snapshot would be a regression, not a
    /// tidy-up.
    #[must_use]
    pub fn dimension(&self) -> Option<lodestone_client::DimensionId> {
        self.net
            .as_ref()
            .and_then(|net| net.shared_handle().get().and_then(|h| h.player().dimension))
    }

    /// Which sky this dimension draws — vanilla's `DimensionType.skybox()`.
    ///
    /// Pushed to the renderer once per frame by `app/redraw.rs`; see
    /// [`lodestone_render::SkyMode`] for what each mode suppresses and why the
    /// End is deliberately still sky-drawing here.
    ///
    /// # Why this matches the level *name* rather than the dimension type
    ///
    /// Unlike `sky_default_for_dimension`, which reads the server's own
    /// `has_skylight`, there is nothing better to read: the dimension type's
    /// `skybox` field is present in the captured `registry_data` NBT and
    /// **dropped by today's decode** (`docs/dimension-visuals.md`'s
    /// registry-decode section). The name match is the documented fallback, not
    /// a shortcut, and it becomes a one-line change the moment that field is
    /// carried. It is also not derivable from what *is* decoded: `has_skylight`
    /// is `false` in the Nether and `true` in the End, while their skyboxes are
    /// `none` and `end` — two different axes.
    ///
    /// No connection (the offline fixture world) is the overworld.
    #[must_use]
    pub fn sky_mode(&self) -> lodestone_render::SkyMode {
        match self.dimension() {
            Some(d) => lodestone_render::SkyMode::for_dimension_name(d.namespace(), d.path()),
            None => lodestone_render::SkyMode::Overworld,
        }
    }

    /// The portal overlay's strength this frame, `0.0..=1.0` — vanilla's
    /// `Mth.lerp(partialTicks, oPortalEffectIntensity, portalEffectIntensity)`.
    ///
    /// Interpolated rather than sampled, because the ramp advances at 20 Hz and
    /// the overlay is drawn at the frame rate: reading the raw tick value paints
    /// a visible 20-step staircase over the four-second ramp-in.
    ///
    /// # This one scalar drives two different things
    ///
    /// Both consumers exist already and they use it differently, which is why a
    /// single "portal effect strength" that fed one of them would look wrong:
    ///
    /// - the **overlay alpha** is this value directly, and portal takes
    ///   *priority* over nausea rather than blending with it (`Hud`'s own
    ///   `if (portalIntensity > 0) … else if (nauseaIntensity > 0) …`, which
    ///   `RenderState::render_inner` reproduces as an `if`/`else if`);
    /// - the **world-projection warp** takes `max(portal, nausea)` for its
    ///   amount and a *speed* blend `(portal * 20 + nausea * 7) / (portal +
    ///   nausea)` for its rotation rate — so a portal spins nearly three times
    ///   faster than nausea at the same amount.
    #[must_use]
    pub fn portal_effect_intensity(&self) -> f32 {
        // `FrameClock::interp_alpha` — the same partial tick every other
        // interpolated per-frame read in this crate uses (chest lids, the bell
        // shake, the player's own body pose), never a second clock.
        let alpha = self.clock().interp_alpha;
        let from = self.prev_portal_effect_intensity;
        let to = self.portal_effect_intensity;
        (from + (to - from) * alpha).clamp(0.0, 1.0)
    }

    /// One tick of vanilla's `LocalPlayer.handlePortalTransitionEffect`.
    ///
    /// `+0.0125` while the player is inside a nether portal, `-0.05` otherwise,
    /// clamped to `0.0..=1.0`, with the previous value kept for the frame lerp.
    /// The sound, the screen-closing and the `portalProcess` bookkeeping in the
    /// same vanilla method are server- or UI-side and are not modelled here.
    ///
    /// Called unconditionally from [`Sim::step`]'s tick body: the decay arm has
    /// to keep running after the player leaves the portal (and after they arrive
    /// in the Nether, where the effect fades out over its 20 ticks), so gating
    /// this on "in a portal" would freeze the overlay on screen at full
    /// strength.
    pub(crate) fn tick_portal_effect(&mut self) {
        self.prev_portal_effect_intensity = self.portal_effect_intensity;
        let step = if self.eye_or_body_in_nether_portal() {
            PORTAL_EFFECT_RAMP_PER_TICK
        } else if self.portal_effect_intensity > 0.0 {
            -PORTAL_EFFECT_DECAY_PER_TICK
        } else {
            // Already at rest. Vanilla's own third branch: `step` stays `0.0F`,
            // so the clamp below is a no-op and nothing is written.
            0.0
        };
        self.portal_effect_intensity = (self.portal_effect_intensity + step).clamp(0.0, 1.0);
    }

    /// Whether the local player's bounding box overlaps a `minecraft:nether_portal`
    /// cell.
    ///
    /// # This is vanilla's own "inside block" test, not a proximity guess
    ///
    /// `Entity.checkInsideBlocks` walks the cells its bounding box (deflated by
    /// `1.0E-5`) intersects and calls `state.entityInside(...)` on each;
    /// `NetherPortalBlock.entityInside` is what sets the portal process. The
    /// deflation is what stops a player standing flush against a portal from
    /// registering, and it is why this is not a bare `floor`/`ceil` range.
    ///
    /// `insideBlock` there is `intersectShape == Shapes.block() || …` and
    /// `NetherPortalBlock` does not override `getEntityInsideCollisionShape`, so
    /// its inside-shape is the **full cube** even though its collision shape is a
    /// thin slab. A cell test is therefore exact for this block, not an
    /// approximation of one — which is the whole reason this can be a cell scan
    /// rather than a shape intersection.
    fn eye_or_body_in_nether_portal(&self) -> bool {
        // No connection means the offline fixture world, which has no portals.
        if self.net.is_none() {
            return false;
        }
        let bb = self
            .player()
            .bounding_box(&self.profile())
            .inflate(-Self::INSIDE_BLOCK_DEFLATE);
        // `floor`/`ceil` on the deflated box: `min` floors to the first cell it
        // reaches into and `max` ceils to one past the last, so a box exactly on a
        // cell boundary contributes only the cells it genuinely overlaps.
        let (x0, x1) = (bb.min_x.floor() as i32, bb.max_x.floor() as i32);
        let (y0, y1) = (bb.min_y.floor() as i32, bb.max_y.floor() as i32);
        let (z0, z1) = (bb.min_z.floor() as i32, bb.max_z.floor() as i32);
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    if is_nether_portal_state(self.block_at_world([x, y, z])) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Vanilla's `Entity.checkInsideBlocks` deflation, `1.0E-5F`.
    const INSIDE_BLOCK_DEFLATE: f64 = 1.0e-5;

    /// React to a server-reported respawn, which is **also** how the server
    /// reports a portal trip (`ClientEvent::Respawned` carries the destination
    /// dimension).
    ///
    /// Returns `true` when this was a dimension *change* and the reset ran.
    ///
    /// # The comparison is the whole safety argument
    ///
    /// A death-respawn in the same dimension reports the same
    /// [`lodestone_client::DimensionId`], so it takes the `false` path and the
    /// world is left entirely alone — which is what stops this from turning
    /// every death into a full terrain reload. `None`
    /// (a family whose adapter reports no dimension) is likewise treated as "no
    /// change": a reset we cannot justify is worse than one we skip, because the
    /// skipped case is the pre-existing behaviour and the unjustified one deletes
    /// terrain the server will not resend.
    pub(crate) fn apply_respawn(&mut self, dimension: Option<lodestone_client::DimensionId>) -> bool {
        let changed = match (&self.applied_dimension, &dimension) {
            (Some(before), Some(after)) => before != after,
            // Pre-login we have no baseline. The join path's own reset already
            // ran, so record and do nothing.
            (None, Some(_)) => false,
            (_, None) => false,
        };
        if let Some(after) = dimension {
            self.applied_dimension = Some(after);
        }
        if !changed {
            return false;
        }
        self.reset_for_dimension_change();
        true
    }

    /// Record the dimension a fresh session logged into, without running any
    /// reset.
    ///
    /// The baseline for [`Sim::apply_respawn`]'s comparison. Separate from the
    /// respawn path because a login is not a transition *within* a session:
    /// `attach_net` has already built a clean `Sim` and there is nothing of a
    /// previous dimension to drop. Without this the first portal trip of a
    /// session would compare against `None` and skip its reset.
    pub(crate) fn record_login_dimension(&mut self) {
        self.applied_dimension = self.dimension();
    }

    /// Everything the client owns that belongs to the dimension we just left.
    ///
    /// Three drops, and each is the *only* thing that clears its own store — see
    /// this module's doc for the survives/does-not table, and note that nothing
    /// player-scoped appears below.
    ///
    /// # The chunk **store** is still not cleared here, and now it is cleared
    /// elsewhere
    ///
    /// The renderer's sections are dropped below; the client's decoded chunk store
    /// is not, and that remains deliberate. The reason is ordering, not oversight:
    /// the store is written by the **net thread** as packets decode, while this runs
    /// on the render thread when the shell next drains `NetClient::poll`. Columns
    /// for the *new* dimension can already be in the store by then, and a bulk clear
    /// here would delete terrain no server will resend — trading leftover geometry
    /// for a permanent hole.
    ///
    /// Dropping the meshed columns is safe in exactly the way clearing the store
    /// is not: a column still in the store is re-meshed the moment anything
    /// dirties it, so an over-eager mesh drop costs a re-mesh, and an over-eager
    /// store clear costs the chunk.
    ///
    /// The store clear therefore lives at the one point where the ordering is
    /// safe — `lodestone_client::Driver::forget_previous_dimension`, called from
    /// `Driver::emit`'s `Respawned` arm, on the net thread, after that packet's
    /// world-write guard has been dropped and before the next packet decodes. It
    /// compares the destination against the dimension it last recorded, so a
    /// death-respawn touches nothing. Against our own integrated server the store
    /// was already emptied in the right order by its `forget_chunk` sweep; that
    /// clear is what makes a **vanilla** server correct too. See
    /// `docs/nether-portals.md`.
    pub(crate) fn reset_for_dimension_change(&mut self) {
        // The other entities. Vanilla builds a whole new `ClientLevel` on
        // `handleRespawn`, which drops every entity in the old one; this is the
        // same call `end_session` uses, and it exempts the local player for the
        // reason its own doc gives (the driver holds that `Entity` across the
        // reset).
        self.write(|w| {
            crate::entities::reset_entity_tracks(w);
            lodestone_ecs::ingest::reset_ingest_entities(w);
        });
        // Every GPU section this dimension ever uploaded, plus the in-flight mesh
        // jobs that would otherwise land in the new dimension carrying the old
        // one's geometry. `TerrainMesh::end_session` is named for its first
        // caller, not for a session: it is the mesh-side reset, and a dimension
        // change needs precisely it.
        self.terrain_mut(TerrainMesh::end_session);
        // The interpolator's own accumulator, for the reason `end_session`
        // records: leaving it phased against the player's accumulator re-phases
        // the two clocks arbitrarily across the transition.
        self.write(|w| w.resource_mut::<FrameClock>().reset_accumulator());
        // The portal effect is decaying at this point (the player has left the
        // portal by arriving), so it is deliberately **not** zeroed: vanilla
        // carries `portalEffectIntensity` across `handleRespawn` explicitly
        // (`ClientPacketListener.handleRespawn` copies both it and its previous
        // value onto the new player), so the overlay fades out in the
        // destination rather than vanishing at the seam.
        self.status = "changed dimension".into();
    }

    /// The reset [`Sim::end_session`] needs from this cluster, so the two paths
    /// cannot drift apart on the fields this module owns.
    pub(crate) fn reset_dimension_state(&mut self) {
        self.applied_dimension = None;
        self.portal_effect_intensity = 0.0;
        self.prev_portal_effect_intensity = 0.0;
    }
}

/// Whether a block state id is a nether portal.
///
/// Keyed on the *block* name rather than on a state id range, because
/// `nether_portal` has two states (`axis=x`/`axis=z`) and matching one would
/// make the effect fire in half the portals — the kind of half-working that
/// reads as a flaky bug rather than a wrong branch.
fn is_nether_portal_state(state: u32) -> bool {
    lodestone_data::block_states::block_name(state) == Some("minecraft:nether_portal")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless_config() -> crate::config::Config {
        crate::config::Config {
            mode: crate::config::Mode::Headless,
            render_distance: 2,
            ..crate::config::Config::default()
        }
    }

    fn dim(id: &str) -> lodestone_client::DimensionId {
        id.parse().expect("valid dimension id")
    }

    /// Seed one indexed non-local entity, the way ingest would, and return how
    /// many the index holds.
    fn seed_indexed_entity(sim: &mut Sim, entity_id: i32) -> usize {
        sim.write(|w| {
            let entity = w.spawn(lodestone_ecs::entity::MinecraftEntityId(entity_id)).id();
            w.resource_mut::<lodestone_ecs::entity::EntityIndex>()
                .insert(entity_id, entity);
            w.resource::<lodestone_ecs::entity::EntityIndex>().len()
        })
    }

    fn indexed_count(sim: &Sim) -> usize {
        sim.read(|w| w.resource::<lodestone_ecs::entity::EntityIndex>().len())
    }

    /// **The split.** A death-respawn in the same dimension must leave the world
    /// entirely alone; a portal trip must drop the other dimension's entities.
    ///
    /// The same-dimension arm is the one that matters, and it is not decoration:
    /// with the comparison dropped from `apply_respawn`, every death in the game
    /// would empty the entity index and throw away every meshed column — a much
    /// worse bug than the leftover overworld mobs this exists to fix, and one that
    /// a test asserting only the portal arm would certify as working.
    ///
    /// The two dimension ids are deliberately *both* `minecraft:` and differ only
    /// in path, so a comparison that accidentally compared namespaces would pass
    /// the same-dimension arm and fail here rather than the other way round.
    #[test]
    fn a_same_dimension_respawn_keeps_the_entities_and_a_portal_trip_drops_them() {
        let mut sim = Sim::with_demo_world(headless_config());
        // Baseline: the first `Respawned` of a session has nothing to compare
        // against, so it records and resets nothing.
        assert!(
            !sim.apply_respawn(Some(dim("minecraft:overworld"))),
            "with no recorded dimension there is no change to act on"
        );
        assert_eq!(seed_indexed_entity(&mut sim, 77), 1);

        assert!(
            !sim.apply_respawn(Some(dim("minecraft:overworld"))),
            "a death-respawn in the same dimension is not a dimension change"
        );
        assert_eq!(
            indexed_count(&sim),
            1,
            "a same-dimension respawn must not touch the entity index — every \
             death would otherwise wipe the world"
        );

        assert!(
            sim.apply_respawn(Some(dim("minecraft:the_nether"))),
            "a different dimension on the same packet is a portal trip"
        );
        assert_eq!(
            indexed_count(&sim),
            0,
            "arriving in the Nether must drop the overworld's entities — vanilla \
             builds a whole new ClientLevel"
        );

        // And the trip is not repeatable: a second `Respawned` naming the Nether
        // (a death in the Nether) must not reset again.
        assert_eq!(seed_indexed_entity(&mut sim, 78), 1);
        assert!(
            !sim.apply_respawn(Some(dim("minecraft:the_nether"))),
            "the edge must be consumed — a Nether death is not a second trip"
        );
        assert_eq!(indexed_count(&sim), 1);
    }

    /// The other half of "a dimension change is not a respawn": the reset must not
    /// touch anything player-scoped.
    ///
    /// An emptied inventory on the first portal trip is the failure this guards,
    /// and it is much worse than a wrong sky — so the assertion is on the concrete
    /// contents, not on "the component still exists". `Sim::with_demo_world`
    /// hands out an empty hotbar, so the slot is *filled here* first: asserting
    /// against the fixture's default would pass with the inventory wiped.
    #[test]
    fn a_dimension_change_leaves_the_inventory_xp_and_health_alone() {
        let mut sim = Sim::with_demo_world(headless_config());
        assert!(!sim.apply_respawn(Some(dim("minecraft:overworld"))));

        // Pairwise-distinct values, and none of them a default: `7.5` health is
        // not full, `11` food is not `20`, and the XP triple's three fields are
        // three different numbers so a transposition inside `Xp` cannot survive.
        let local = sim.local;
        sim.write(|w| {
            let mut vitals = w
                .get_mut::<lodestone_ecs::session::Vitals>(local)
                .expect("the local player has vitals");
            vitals.health = Some(7.5);
            vitals.food = Some(11);
        });
        sim.write(|w| {
            let mut xp = w.get_mut::<lodestone_ecs::session::Xp>(local).expect("xp");
            xp.0 = Some((0.25, 23, 900));
        });

        assert!(sim.apply_respawn(Some(dim("minecraft:the_nether"))));

        let (health, food) = sim.read(|w| {
            let v = w
                .get::<lodestone_ecs::session::Vitals>(local)
                .expect("vitals survive a dimension change");
            (v.health, v.food)
        });
        assert_eq!(
            (health, food),
            (Some(7.5), Some(11)),
            "health/hunger must cross a portal"
        );
        let xp = sim.read(|w| {
            w.get::<lodestone_ecs::session::Xp>(local)
                .expect("xp survives a dimension change")
                .0
        });
        assert_eq!(
            xp,
            Some((0.25, 23, 900)),
            "XP must cross a portal — this is the reset-too-much failure"
        );
    }

    /// With no connection there is no dimension, and the fallback is a *drawn*
    /// sky rather than the Nether's blank one.
    ///
    /// The `None` direction is the one that fails invisibly: an offline fixture
    /// world resolving to `SkyMode::None` renders a black void overhead with
    /// nothing red anywhere and no error.
    #[test]
    fn no_connection_falls_back_to_a_drawn_sky() {
        let sim = Sim::with_demo_world(headless_config());
        assert!(sim.dimension().is_none(), "the fixture has no server");
        assert_eq!(sim.sky_mode(), lodestone_render::SkyMode::Overworld);
        assert!(
            sim.sky_mode().draws_sky_geometry(),
            "the offline world must still draw a sky"
        );
    }

    /// The curve, against the two constants read off
    /// `LocalPlayer.handlePortalTransitionEffect` — and the **asymmetry** is what
    /// is asserted, because a symmetric fade is the plausible wrong answer that a
    /// direction-only check cannot separate from the real one.
    ///
    /// Both counts are derived from the steps rather than guessed at a round
    /// number: `1.0 / 0.0125 = 80` and `1.0 / 0.05 = 20`.
    #[test]
    fn the_portal_curve_ramps_over_eighty_ticks_and_decays_over_twenty() {
        assert_eq!(
            (1.0_f32 / PORTAL_EFFECT_RAMP_PER_TICK).round() as i32,
            80,
            "vanilla's +0.0125F/tick is four seconds to full"
        );
        assert_eq!(
            (1.0_f32 / PORTAL_EFFECT_DECAY_PER_TICK).round() as i32,
            20,
            "vanilla's -0.05F/tick is one second to nothing"
        );
        // The ratio, stated as its own claim: a 1:1 fade would pass both of the
        // assertions above if either constant were mistyped to match the other.
        assert!(
            (PORTAL_EFFECT_DECAY_PER_TICK / PORTAL_EFFECT_RAMP_PER_TICK - 4.0).abs() < 1e-6,
            "the decay is exactly 4x the ramp: {} vs {}",
            PORTAL_EFFECT_DECAY_PER_TICK,
            PORTAL_EFFECT_RAMP_PER_TICK
        );
    }

    /// The state → "is a portal" test must accept **both** axes.
    ///
    /// `nether_portal` has an `axis` property with two values, so a state-id
    /// equality check against one of them fires in exactly half the portals in
    /// the world, depending on which way the frame was built. The two ids are
    /// asserted to be distinct first, or this test would pass against a
    /// single-state block.
    #[test]
    fn both_portal_axes_count_and_neighbouring_blocks_do_not() {
        let ids: Vec<u32> = (0..u32::try_from(lodestone_data::block_states::STATE_COUNT).unwrap())
            .filter(|&id| is_nether_portal_state(id))
            .collect();
        assert_eq!(
            ids.len(),
            2,
            "nether_portal has axis=x and axis=z; got {ids:?}"
        );
        assert_ne!(ids[0], ids[1]);
        // The blocks a portal is actually made of and stood in must not count —
        // obsidian is the frame and air is what a player walks through to reach
        // it, so either one matching would make the effect fire permanently.
        for name in ["minecraft:obsidian", "minecraft:air", "minecraft:end_portal"] {
            let hit = (0..u32::try_from(lodestone_data::block_states::STATE_COUNT).unwrap())
                .filter(|&id| is_nether_portal_state(id))
                .any(|id| lodestone_data::block_states::block_name(id) == Some(name));
            assert!(!hit, "{name} must not read as a nether portal");
        }
    }
}
