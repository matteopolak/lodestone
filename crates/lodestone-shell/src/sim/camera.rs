//! `Sim`'s camera cluster: the fog helpers (`fog_for_render_distance`,
//! `water_fog`, `lava_fog`), `fog_settings`/`biome_sky_color`, and the
//! eye/render camera derivation (`interpolated_player`, `camera`,
//! `toggle_third_person`, `set_view_bobbing`, `bob_frame`, `render_camera`,
//! `spyglass_scoping`, `third_person_body_state`) plus the `NoCollision`
//! stand-in `render_camera`'s third-person pullback falls back to — seam 6
//! of the sim.rs decomposition sequence (seam 1 was the test module,
//! `sim/tests.rs`; seam 2 was placement prediction, `sim/placement.rs`;
//! seam 3 was the interaction/combat cluster, `sim/actions.rs`; seam 4 was
//! the net-apply cluster, `sim/net_apply.rs`; seam 5 was the audio cluster,
//! `sim/audio.rs`).
//!
//! `use super::*;` for the same reason every other seam file uses it:
//! `sim::camera` is a descendant of `sim` and already has the same
//! visibility into `Sim`'s private fields and `sim.rs`'s other private
//! helpers that the earlier seams have.
//!
//! `fog_for_render_distance` is `pub(crate)` here, same as it was in
//! `sim.rs`, but now needs a re-export: `app.rs` names it by its full path
//! (`crate::sim::fog_for_render_distance`), and `app.rs` is neither `sim`
//! nor a descendant of it, so the item has to be reachable *at* the `sim`
//! module boundary. `sim.rs` picks it back up with a plain (non-`pub`)
//! `use camera::fog_for_render_distance;` — sufficient for
//! `crate::sim::fog_for_render_distance` to resolve, and it also re-enters
//! `sim::tests`' `use super::*;` glob the same way `placement::is_air_state`
//! already does. `water_fog`/`lava_fog` need no such treatment: both are
//! called only from `fog_settings`, which moved here with them.
//!
//! Every other item here is an `impl Sim` method and needed no privacy
//! change: all were already `pub` (called from `app.rs`) or stay private
//! because their only callers moved into this same file (`biome_sky_color`
//! from `fog_settings`, `interpolated_player` from `camera`/
//! `third_person_body_state`, `spyglass_scoping` from `render_camera`).

use super::*;

/// Distance fog for a render distance of `render_distance` chunks.
///
/// Fog is what hides the render-distance edge — without it the loaded world
/// ends in a hard wall of geometry against the sky. It therefore has to track
/// the *configured* distance rather than a fixed default, or raising
/// `--render-distance` would fog out the very chunks it just loaded, making a
/// larger view look worse than a smaller one.
///
/// Free-standing so the relationship is testable without generating a world:
/// [`Sim::new`] at render distance 32 builds thousands of sections, which is a
/// minute of work to check a multiplication.
pub(crate) fn fog_for_render_distance(render_distance: u32) -> lodestone_render::fog::FogSettings {
    // `for_render_distance`, not `for_view_distance`: the latter deliberately does
    // **not** populate the environmental fog pair, so the live overworld was still
    // getting only the render-distance term after that fix landed. The Nether and
    // the End already had it, because `Sim::fog_settings` calls `FogSettings::nether`
    // /`the_end` directly — so the one dimension a player actually starts in was the
    // one the fix did not reach.
    //
    // The span is unchanged: `for_render_distance` is algebraically identical to the
    // fraction form across render distance 3..=40, which `gpu.rs`'s
    // `fog_start_fraction_matches_vanillas_span` pins. `gpu::FOG_START_FRACTION` is
    // still used by that test, so it does not become dead.
    lodestone_render::fog::FogSettings::for_render_distance(crate::gpu::SKY_COLOR, render_distance)
}

/// Short, near-eye distance fog for an eye submerged in water.
///
/// Vanilla water vision is only a few chunks, so the far edge is capped short
/// (and never past where chunks actually stop) and the ramp starts at the eye
/// (`start_fraction` 0) so terrain dissolves close rather than at the sky edge.
/// The colour is the default ocean underwater fog — the per-biome water fog
/// colour is not yet reachable from the shell, so this is the documented
/// fallback rather than a biome-correct tint.
fn water_fog(render_distance: u32) -> lodestone_render::fog::FogSettings {
    let far = 32.0_f32.min(render_distance as f32 * 16.0);
    lodestone_render::fog::FogSettings::for_view_distance([0.05, 0.19, 0.44], far, 0.0)
}

/// Near-opaque, few-block distance fog for an eye submerged in lava: submerging
/// in lava blinds fast in vanilla, so the range is very short and the colour a
/// hot orange.
fn lava_fog() -> lodestone_render::fog::FogSettings {
    lodestone_render::fog::FogSettings::for_view_distance([0.6, 0.1, 0.0], 3.0, 0.0)
}

impl Sim {
    /// Distance fog for this frame: sized to the configured render distance
    /// normally (further specialised by the connected *dimension* — the
    /// Nether's fixed dense red haze, the End's near-black edge fade — when
    /// neither override below applies), and swapped for a short, dense
    /// water/lava fog while the player's eye is submerged.
    ///
    /// Selected from the bit-exact eye-in-fluid state (`FluidState`) the physics
    /// producer computes each tick, so the fog matches vanilla's submerged view
    /// rather than a locally-guessed boolean. Lava is checked before water,
    /// matching vanilla's lava-first submersion order, and both take priority
    /// over the dimension fog: standing in lava in the Nether still gets lava
    /// fog, not Nether fog.
    ///
    /// The dimension is read the same way `refresh_mesh_policy` reads it
    /// for `SkyDefault` — `net.shared_handle().get().and_then(|h|
    /// h.player().dimension)` — which is `None` before login and (per
    /// `docs/dimension-visuals.md`) stale after a portal trip until
    /// `lodestone-client`'s `Inner::apply` gets a `Respawned` arm; that staleness
    /// is a pre-existing condition of the dimension field itself; this reads it
    /// the same way every other dimension-conditioned decision in this crate
    /// does, no better and no worse.
    #[must_use]
    pub fn fog_settings(&self) -> lodestone_render::fog::FogSettings {
        let fluid = self.fluid_state();
        if fluid.under_lava() {
            return lava_fog();
        }
        if fluid.under_water() {
            return water_fog(self.config.render_distance);
        }
        let dimension = self
            .net
            .as_ref()
            .and_then(|net| net.shared_handle().get().and_then(|h| h.player().dimension));
        match dimension {
            Some(d) if d.namespace() == "minecraft" && d.path() == "the_nether" => {
                lodestone_render::fog::FogSettings::nether(self.config.render_distance)
            }
            Some(d) if d.namespace() == "minecraft" && d.path() == "the_end" => {
                lodestone_render::fog::FogSettings::the_end(
                    self.config.render_distance,
                    crate::gpu::FOG_START_FRACTION,
                )
            }
            _ => fog_for_render_distance(self.config.render_distance),
        }
        .with_biome_sky_color(self.biome_sky_color())
    }

    /// The standing biome's `minecraft:visual/sky_color` in **linear** RGB, or
    /// `None` when there is nothing better than the dimension default to draw
    /// (issue #96).
    ///
    /// # The chain, and the one hop that is not a lookup
    ///
    /// The colour table arrives whole, indexed by biome holder id, on
    /// `ClientEvent::BiomeVisuals` and reaches here as
    /// `PlayerSnapshot::biome_sky_colors`. **The biome itself is not on the
    /// network at all** — it lives in the chunk section's biome palette, so this
    /// is the hop that has to happen at the camera every frame, and it is the
    /// reason the whole table travels rather than one resolved colour.
    ///
    /// # Why it scans downward for a section
    ///
    /// `sections_at` elides an empty section to `None`, and the section holding
    /// the player's own feet is very often empty — standing on a plain at `y=64`
    /// puts the eye in section `64..80` while the ground is the last block of
    /// `48..64`. Sampling only the eye's section would therefore leave the sky
    /// untinted over open ground, which is precisely where a sky is visible.
    /// Biomes are all but columnar (one cell is 4×4×4 blocks, and vanilla's own
    /// biome sources vary far more horizontally than vertically), so the first
    /// present section at or below the eye is the right answer, not an
    /// approximation worth a second mechanism.
    ///
    /// The `None`s are all deliberate and all mean the same thing: *the server
    /// has not told us*. Pre-login, a server that sent no biome registry, a
    /// column that has not streamed in, a biome with no `sky_color` (the ten
    /// Nether/End biomes) — each falls back to the dimension colour the caller
    /// already computed, which is the same explicit-fallback shape #34 was filed
    /// over. Never a plausible-looking overworld blue.
    #[must_use]
    fn biome_sky_color(&self) -> Option<[f32; 3]> {
        let net = self.net.as_ref()?;
        let table = net.shared_handle().get()?.player().biome_sky_colors;
        if table.is_empty() {
            return None;
        }
        let dims = net.world_dimensions()?;
        let section_count = dims.section_count();

        let position = self.player().position;
        let block_x = position.x.floor() as i32;
        let block_y = position.y.floor() as i32;
        let block_z = position.z.floor() as i32;
        let chunk = lodestone_client::ChunkPos {
            x: block_x.div_euclid(16),
            z: block_z.div_euclid(16),
        };
        let base_si = dims.min_y.div_euclid(16);
        let eye_si = block_y.div_euclid(16) - base_si;
        // Clamp rather than reject: an eye above the build limit still stands in
        // a biome, and the topmost section is the one that holds it.
        let top = eye_si.clamp(0, i32::try_from(section_count).unwrap_or(0).saturating_sub(1));
        if section_count == 0 {
            return None;
        }

        // Top-down: one lock acquisition for the whole column, then the highest
        // present section at or below the eye.
        let requests: Vec<(lodestone_client::ChunkPos, usize)> = (0..=top)
            .rev()
            .map(|si| (chunk, usize::try_from(si).unwrap_or(0)))
            .collect();
        let (section, si) = net
            .sections_at(&requests)
            .into_iter()
            .zip(requests.iter().map(|(_, si)| *si))
            .find_map(|(section, si)| section.map(|s| (s, si)))?;

        // The sampled `y` is the eye's own within its section, or the top of
        // whichever lower section answered.
        let local_y = if si == usize::try_from(top).unwrap_or(0) {
            block_y.rem_euclid(16) as usize
        } else {
            15
        };
        let biome = section.biome_at_block(
            block_x.rem_euclid(16) as usize,
            local_y,
            block_z.rem_euclid(16) as usize,
        );
        let packed = (*table.get(usize::try_from(biome).ok()?)?)?;
        // sRGB bytes → linear, exactly as `FogSettings::nether`/`the_end` do with
        // their own hex constants. The *day/night* multiply stays in gamma space
        // inside the sky pass (`SkyFrame`); this is only the transfer function
        // for the base colour, which every colour handed to the renderer gets.
        Some(lodestone_render::fog::srgb_u8_to_linear([
            ((packed >> 16) & 0xFF) as u8,
            ((packed >> 8) & 0xFF) as u8,
            (packed & 0xFF) as u8,
        ]))
    }

    /// The player's physics state with `position` replaced by the feet
    /// interpolated between the last two physics ticks — the "drawn" position
    /// every per-frame consumer of the player's own placement wants, rather
    /// than the raw tick-boundary value [`Self::player`] returns. Shared by
    /// [`Self::camera`] and [`Self::third_person_body_state`] so the eye and
    /// the third-person body it stands next to never disagree about where
    /// "here" is.
    #[must_use]
    fn interpolated_player(&self) -> PlayerState {
        let a = f64::from(self.clock().interp_alpha);
        let mut interp = self.player();
        let prev = self.prev_position();
        interp.position = Vec3d::new(
            prev.x + (interp.position.x - prev.x) * a,
            prev.y + (interp.position.y - prev.y) * a,
            prev.z + (interp.position.z - prev.z) * a,
        );
        interp
    }

    /// Build the **true first-person eye** camera for the given viewport
    /// aspect ratio, with the feet position interpolated between the last two
    /// physics ticks so motion stays smooth even though physics runs at a
    /// fixed 20 Hz. View angles are current (mouse-look is per-frame, matching
    /// vanilla).
    ///
    /// The pose's eye height is passed to [`build_camera`] explicitly, so the
    /// position handed to it is the player's real interpolated feet in every pose
    /// (`Avatar.java:22-36`: `0.4` swimming, `1.27` crouching, `1.62` standing).
    /// It used to be folded into the feet Y as a bias instead — arithmetically the
    /// same, but the argument was then not the feet whenever a non-standing pose
    /// was active. See `camera_rig.rs`'s module docs.
    ///
    /// This is also the ray origin for [`update_target`](Self::update_target)
    /// and the audio listener ([`Self::set_audio_listener`]'s caller in
    /// `app.rs`), **deliberately unmodified by third-person mode**: block
    /// interaction and hearing both originate from the real eye in vanilla,
    /// not from wherever a pulled-back camera happens to be. Only the actual
    /// render pass wants the third-person offset — see [`Self::render_camera`].
    #[must_use]
    pub fn camera(&self, aspect: f32) -> Camera {
        let interp = self.interpolated_player();
        build_camera(
            &interp,
            // The *camera's* eased eye, not `interp.eye_height` — see the field's
            // doc. Interpolating the entity's eye height would still snap, because
            // the value being interpolated between two ticks is itself the
            // post-snap one.
            self.eye_height_smoother.lerp(self.clock().interp_alpha),
            aspect,
            self.config.render_distance,
        )
    }

    /// Flips the camera mode (vanilla's `F5`): first person ↔ third person.
    ///
    /// This one bool is the entire "camera mode" state in this shell —
    /// [`RenderState::set_third_person_body_source`](crate::gpu::RenderState::set_third_person_body_source)'s
    /// own doc says the closure's `None`/`Some` split *is* the camera-mode
    /// toggle by design, and [`Self::render_camera`] /
    /// [`Self::third_person_body_state`] are exactly that closure's two
    /// halves: the same flag decides both, so they can never disagree about
    /// which mode is active this frame.
    pub fn toggle_third_person(&mut self) {
        self.third_person = !self.third_person;
    }

    /// The camera the frame is actually **drawn** from: [`Self::camera`]
    /// unmodified in first person, or that same eye pulled straight backward
    /// along its own view direction in third person — vanilla's real
    /// "back" algorithm, not a stand-in for it — clamped against live
    /// collision geometry so it never clips through a wall (see
    /// [`crate::camera_rig::collision_pullback`]).
    ///
    /// Reads whichever collision adapter [`Self::update_target`] would use
    /// (`LiveCollision` on a server, `WorldCollision` on the offline fixture),
    /// so a third-person camera respects the exact same geometry the player
    /// collides against. A live session whose own column has not streamed in
    /// yet (`Self::live_collision` returning `None`) has nothing real to
    /// clamp against, so this falls back to the desired distance unclamped
    /// rather than jamming the camera into the eye.
    /// Push vanilla's View Bobbing option down from the menu layer. Cheap and
    /// idempotent; `app.rs` calls it once per presented frame rather than on the
    /// toggle, for the same reason the deleted present-mode poll did — the menu
    /// is pure and owns the `Options`, and `Sim` owns none.
    pub fn set_view_bobbing(&mut self, on: bool) {
        self.view_bobbing = on;
    }

    /// The interpolated walk bob this frame, or an all-zero frame when the option
    /// is off. Exposed so a gate can assert the *input* to the camera fold
    /// separately from the fold itself.
    #[must_use]
    pub fn bob_frame(&self) -> crate::camera_rig::BobFrame {
        if !self.view_bobbing {
            return crate::camera_rig::BobFrame::default();
        }
        self.view_bob.frame(self.clock().interp_alpha)
    }

    #[must_use]
    pub fn render_camera(&self, aspect: f32) -> Camera {
        // The bob lands **here and not in `Self::camera`**, which is deliberate
        // and is the difference between a wobbling camera and a wobbling *game*:
        // `Self::camera` is also the block-targeting ray origin and the audio
        // listener, and vanilla bobs neither. `GameRenderer.renderLevel` folds the
        // bob into the *projection matrix* (`:539`), so `Camera`'s own position
        // and rotation — what `getPickRay` and the listener read — never see it.
        //
        // Not gated on `third_person`: 26.2's `renderLevel` applies `bobView`
        // whenever `optionsRenderState.bobView` is set, with no camera-type check
        // (`GameRenderer.java:534-536`), and `bobView` itself only tests
        // `isPlayer`. Older versions did suppress it in third person and issue
        // #58's body says so; re-read against `.cache/mc/26.2/client-src`, that is
        // no longer true.
        let eye = bobbed_camera(
            self.camera(aspect),
            self.bob_frame(),
            // `bobHurt` is deliberately **not** driven from here yet: it is almost
            // entirely a roll, and `bobbed_camera` cannot carry roll, so wiring it
            // would produce a visibly wrong tilt rather than a slightly imprecise
            // one. `ViewBob::hurt` and `BobFrame::hurt_roll_degrees` are
            // implemented and tested against vanilla; see `docs/view-bobbing.md`
            // for what the last hop needs.
            0.0,
        );
        if !self.third_person {
            // Issue #154: vanilla's FOV zoom is gated on `firstPerson &&
            // isScoping()` (`AbstractClientPlayer.getFieldOfViewModifier`,
            // `AbstractClientPlayer.java:92-114`) — a third-person camera
            // never zooms, so this composition only runs on the early
            // first-person return, not the two third-person branches below.
            return apply_spyglass_fov(eye, self.spyglass_scoping());
        }
        if self.is_live() {
            match self.live_collision() {
                Some(view) => third_person_camera(eye, true, &view),
                None => third_person_camera(eye, true, &NoCollision),
            }
        } else {
            let store = self.chunk_world();
            let world = store.read();
            let view = WorldCollision::new(&world);
            third_person_camera(eye, true, &view)
        }
    }

    /// Vanilla's `Player.isScoping()` (issue #154):
    /// `isUsingItem() && getUseItem().is(Items.SPYGLASS)`
    /// (`Player.java:1936-1938`), computed entirely from `Sim`'s own state so
    /// [`Self::render_camera`] needs no new parameter — `app.rs` computes the
    /// same condition independently for `ScreenEffects::scoping` (it already
    /// has the held item at hand for the first-person render source), and
    /// the two are expected to agree rather than share a call, the same way
    /// `wearing_pumpkin` is computed locally in `app.rs` rather than exposed
    /// from here.
    #[must_use]
    fn spyglass_scoping(&self) -> bool {
        self.using_item()
            && self
                .player_menu()
                .player_native(self.selected_slot())
                .is_some_and(|st| st.item().to_string() == "minecraft:spyglass")
    }

    /// The local player's own third-person body for this frame, or `None` in
    /// first person — exactly the value `app.rs` hands
    /// [`RenderState::set_third_person_body_source`](crate::gpu::RenderState::set_third_person_body_source)'s
    /// closure every frame.
    ///
    /// The walk cycle, **arm swing** and idle age come from [`Self::body_pose`],
    /// ticked once per physics tick the same way `entities.rs`'s `render_anim`
    /// drives one for a tracked network entity, and interpolated here for the
    /// current sub-tick alpha. Facing does **not** come from that pose,
    /// though: `body_yaw_deg`/`head_pitch_deg` are read straight off the
    /// interpolated player instead, so the avatar's own facing tracks the
    /// camera with no per-tick lag — the lag `EntityPose`'s body-yaw smoothing
    /// exists to model is a *third-party observer's* view of a remote entity,
    /// which does not apply to your own body.
    ///
    /// Two gaps, both left exactly where the equivalent gap already is
    /// elsewhere in this codebase rather than guessed at:
    /// * **Head yaw never diverges from body yaw** (`head_yaw_deg` is always
    ///   `0`): vanilla's independent head-turn-then-body-catches-up
    ///   (`LivingEntity.tickHeadTurn`) is not modelled for the local player
    ///   anywhere in this engine.
    /// * **`slim`/skin data**: [`ThirdPersonBodyState::slim`]'s own doc
    ///   already records that no real skin-model bit exists yet; `false`
    ///   reproduces the first-person arm's existing default.
    /// * **Equipment covers main hand, off hand, and all four armour
    ///   slots.** Main hand is the selected hotbar slot; off hand is native
    ///   inventory index `40`; the armour slots are native indices
    ///   `39/38/37/36` for head/chest/legs/feet (`lodestone_game::menu`'s own
    ///   table, `Menu::player`).
    #[must_use]
    pub fn third_person_body_state(&self) -> Option<ThirdPersonBodyState> {
        if !self.third_person {
            return None;
        }
        let partial_tick = self.clock().interp_alpha;
        let interp = self.interpolated_player();
        let feet = glam::Vec3::new(
            interp.position.x as f32,
            interp.position.y as f32,
            interp.position.z as f32,
        );
        let walk = self.body_pose.render(partial_tick);
        /// Native player-inventory index of the off-hand slot
        /// (`lodestone_game::menu`'s doc table: hotbar `0..=8`, off-hand `40`).
        const OFFHAND_NATIVE_INDEX: usize = 40;
        let menu = self.player_menu();
        let mut equipment = Vec::new();
        if let Some(loc) = menu
            .player_native(self.selected_slot())
            .and_then(|st| ResourceLocation::parse(&st.item().to_string()).ok())
        {
            equipment.push((EquipmentSlot::MainHand, loc));
        }
        if let Some(loc) = menu
            .player_native(OFFHAND_NATIVE_INDEX)
            .and_then(|st| ResourceLocation::parse(&st.item().to_string()).ok())
        {
            equipment.push((EquipmentSlot::OffHand, loc));
        }
        // Native player-inventory indices of the four armour slots
        // (`lodestone_game::menu::Menu::player`'s own table: menu slots
        // `5..=8` are head/chest/legs/feet at native indices `39/38/37/36` —
        // the native indices run backwards, feet-first).
        const ARMOUR_NATIVE_SLOTS: [(usize, EquipmentSlot); 4] = [
            (39, EquipmentSlot::Head),
            (38, EquipmentSlot::Chest),
            (37, EquipmentSlot::Legs),
            (36, EquipmentSlot::Feet),
        ];
        for (native, eq) in ARMOUR_NATIVE_SLOTS {
            if let Some(loc) = menu
                .player_native(native)
                .and_then(|st| ResourceLocation::parse(&st.item().to_string()).ok())
            {
                equipment.push((eq, loc));
            }
        }
        Some(ThirdPersonBodyState {
            feet,
            body_yaw_deg: interp.yaw,
            anim: AnimInput {
                head_yaw_deg: 0.0,
                head_pitch_deg: interp.pitch,
                limb_swing: walk.limb_swing,
                limb_swing_amount: walk.limb_swing_amount,
                // The self-avatar's *body* half of the swing:
                // `HumanoidModel.setupAttackAnimation`, via
                // `lodestone_render::entity_anim::Skeleton::pose`. The same scalar
                // the first-person arm pass polls through
                // `Sim::hand_swing_progress`, but a completely different pose
                // function — see `ThirdPersonBodyState`'s docs on why the two must
                // never share one.
                //
                // `walk.attack_anim` rather than `self.hand_swing_progress()`: both
                // are `body_pose.attack_anim_lerp(partial_tick)`, and this one is
                // already in hand from the `render` call above at the *same*
                // partial tick, so the arm and the body cannot drift by a frame.
                attack_anim: walk.attack_anim,
                age_ticks: walk.age,
                aggressive: false,
                // **Not wired for the local player yet (issue #57).** Remote
                // entities get their bow/crossbow pose from
                // `entities::arm_pose_for`, driven by the `ItemUse` component that
                // `ingest::apply_entity_item_use` folds off the living-flags byte.
                // The local player cannot use that path: it has no `EntityKind`/
                // `Position`/`Rotation`/`HeadYaw` (deliberately — that absence is
                // what keeps a self-model off `ClientHandle::entities()`), so
                // `entity_view()`'s early `?` returns before the flags are read,
                // exactly as it does for `Vitals::on_fire`. Reaching it needs a
                // session-scoped fold and a `PlayerSnapshot` field, the same shape
                // `apply_local_player_on_fire` has. Left explicit rather than
                // spread with `..AnimInput::REST` so the gap is visible here.
                arm_pose: lodestone_render::ArmPose::Empty,
                arm_pose_left_hand: false,
            },
            scale: 1.0,
            slim: false,
            equipment,
        })
    }
}

/// A [`CollisionView`] with no geometry at all, for
/// [`Sim::render_camera`]'s third-person pullback when no live collision
/// snapshot exists yet (the player's own column has not streamed in): there
/// is nothing real to clamp against, so the camera pulls back the full
/// desired distance rather than treating "no data" as "solid".
struct NoCollision;

impl CollisionView for NoCollision {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<lodestone_physics::Aabb>) {}
}
