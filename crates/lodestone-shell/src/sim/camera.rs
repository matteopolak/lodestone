//! `Sim`'s camera cluster: the fog helpers (`fog_for_render_distance`,
//! `water_fog`, `lava_fog`), `fog_settings`/`biome_sky_color`, and the
//! eye/render camera derivation (`interpolated_player`, `camera`,
//! `cycle_camera_type`, `set_view_bobbing`, `bob_frame`, `render_camera`,
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
    /// The dimension comes from [`Sim::dimension`], the one accessor every
    /// dimension-conditioned decision in this crate goes through — `None` before
    /// login, and correct across a portal trip because
    /// `lodestone_ecs::session::ServerDimension`'s fold handles `Respawned` as
    /// well as `Login`. **This doc used to record that read as stale after a
    /// portal trip**; it was, and the fix is described in
    /// `docs/dimension-visuals.md`.
    ///
    /// Fog colour is only half of "the Nether looks like the Nether": the sky
    /// *pass* is gated separately by [`Sim::sky_mode`], because a colour cannot
    /// express "draw no sun".
    #[must_use]
    pub fn fog_settings(&self) -> lodestone_render::fog::FogSettings {
        let fluid = self.fluid_state();
        if fluid.under_lava() {
            return lava_fog();
        }
        if fluid.under_water() {
            return water_fog(self.config.render_distance);
        }
        match self.dimension() {
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
    /// `None` when there is nothing better than the dimension default to draw.
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
    /// already computed, which is the same explicit-fallback shape that fix was filed
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
    /// (`Avatar.java`: `0.4` swimming, `1.27` crouching, `1.62` standing).
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
            // Vanilla's FOV option, not the module constant `build_camera` used
            // to write itself — see [`Self::set_fov_y_degrees`].
            self.fov_y_degrees,
        )
    }

    /// Push vanilla's **FOV** option ([`crate::config::Options::fov`]) down from
    /// the menu layer in degrees, exactly as [`Self::set_view_bobbing`] does for
    /// View Bobbing, and polled per frame for the same reason.
    ///
    /// Per frame rather than at launch because vanilla applies this one
    /// immediately: its `IntRange(30, 110)` takes the default
    /// `applyValueImmediately`, unlike `renderDistance`'s explicit `false`. So the
    /// FOV slider must move the view while the settings page is still open, which
    /// is why this is a `Sim` field and not a `Config::resolve_persisted` fold like
    /// `render_distance`.
    ///
    /// Clamping lives in [`build_camera`], which is the one place that can see
    /// every producer — the setter storing a raw value keeps this from being a
    /// second, drifting copy of vanilla's range.
    pub fn set_fov_y_degrees(&mut self, degrees: f32) {
        self.fov_y_degrees = degrees;
    }

    /// The FOV in degrees this frame, before the spyglass zoom. Exposed so a gate
    /// can assert the pushed value separately from the projection it produces.
    #[must_use]
    pub fn fov_y_degrees(&self) -> f32 {
        self.fov_y_degrees
    }

    /// Advances the camera mode one step (vanilla's `F5`, i.e.
    /// `CameraType.cycle()`): first person → third person back → third person
    /// **front** → first person.
    ///
    /// [`Self::render_camera`] and [`Self::third_person_body_state`] are the two
    /// halves of
    /// [`RenderState::set_third_person_body_source`](crate::gpu::RenderState::set_third_person_body_source)'s
    /// closure, and both read this one field, so they can never disagree about
    /// which mode is active this frame. They ask *different questions* of it,
    /// though: the body state (and hence every screen-overlay and first-person-arm
    /// gate downstream of `RenderStats::third_person_body_drawn`) is keyed on
    /// `is_first_person`, and only the camera itself distinguishes back from
    /// front.
    pub fn cycle_camera_type(&mut self) {
        self.camera_type = self.camera_type.cycle();
    }

    /// The camera mode this frame — all three of vanilla's states.
    #[must_use]
    pub fn camera_type(&self) -> crate::camera_rig::CameraType {
        self.camera_type
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

    /// Push vanilla's **Damage Tilt** accessibility option down from the menu
    /// layer, exactly as [`Self::set_view_bobbing`] does for View Bobbing.
    ///
    /// The two are the halves of one vanilla split and must be pushed together:
    /// `GameRenderer.renderLevel` applies `bobHurt` *outside* the `bobView` check,
    /// so turning View Bobbing off must not take the damage tilt with it.
    ///
    /// Clamped here as well as on load, because this is the value a matrix is
    /// built from and a stray negative would roll the camera the wrong way.
    pub fn set_damage_tilt_strength(&mut self, strength: f32) {
        self.damage_tilt_strength = strength.clamp(0.0, 1.0);
    }

    /// The Damage Tilt strength this frame — for the two consumers that build a
    /// matrix from it, [`Self::damage_tilt_eye_transform`] and the first-person
    /// hand pass.
    #[must_use]
    pub fn damage_tilt_strength(&self) -> f32 {
        self.damage_tilt_strength
    }

    /// This frame's `bobHurt` as an **eye-space** matrix — the damage tilt swung
    /// onto the direction the hit came from, plus the death roll.
    ///
    /// # This is the hop that was missing, and why it is not a `Camera`
    ///
    /// `bobHurt` is almost entirely a **roll**, and [`bobbed_camera`] cannot carry
    /// one: `Camera` is `position`/`yaw`/`pitch`, two angles, so a decomposed
    /// orientation has two degrees of freedom where the bob matrix has three. That
    /// — not an unverified formula, and not a missing packet decode — is why
    /// `render_camera` passed a hard `0.0` for so long. The maths was ported and
    /// tested the whole time; there was no seam it could reach the matrix through.
    ///
    /// `RenderState::set_eye_bob_transform` is that seam, and it is vanilla's own
    /// `projectionMatrix.mul(bobStack.last().pose())` — the bob multiplied into the
    /// world view-projection in eye space rather than folded into camera fields.
    /// `Self::camera` is deliberately untouched by it, so the block-targeting ray
    /// and the audio listener still do not bob.
    ///
    /// Returns the identity when the player has not been hit recently, which is
    /// almost every frame.
    #[must_use]
    pub fn damage_tilt_eye_transform(&self) -> glam::Mat4 {
        self.bob_frame().hurt_transform(self.damage_tilt_strength)
    }

    /// The interpolated walk bob this frame, or a frame with the walk terms
    /// zeroed when the option is off. Exposed so a gate can assert the *input*
    /// to the camera fold separately from the fold itself.
    ///
    /// The option zeroes **only the walk terms**, never the hurt half of the
    /// frame: vanilla's `bobHurt` is unconditional — `GameRenderer.renderLevel`
    /// applies it outside the `optionsRenderState.bobView` check
    /// (`GameRenderer.java`) — so the damage tilt must survive View
    /// Bobbing being off. A player who has not been hit recently is unaffected
    /// either way (`frame.hurt` is negative when the countdown has lapsed, and
    /// `BobFrame::hurt_roll_degrees` already returns `0` for that), so this
    /// differs from the old whole-frame `BobFrame::default()` only in the ten
    /// ticks after a hit.
    #[must_use]
    pub fn bob_frame(&self) -> crate::camera_rig::BobFrame {
        let frame = self.view_bob.frame(self.clock().interp_alpha);
        if self.view_bobbing {
            frame
        } else {
            crate::camera_rig::BobFrame {
                walk_phase: 0.0,
                bob: 0.0,
                ..frame
            }
        }
    }

    /// The local player was hurt: start the damage tilt (`Player.animateHurt`,
    /// which records `hurtDir = yaw` after `LivingEntity.animateHurt` resets the
    /// ten-tick countdown). The wire `yaw` is
    /// `ClientboundHurtAnimationPacket.yaw`, already decoded onto
    /// `ClientEvent::EntityHurtAnimation`; the server computes it as
    /// `atan2(damage) - playerYaw` (`ServerPlayer.indicateDamage`), so a hit from
    /// straight ahead is `0` — the pure-roll case.
    ///
    /// The camera-side half of the `bobHurt` wiring: `ViewBob` owns the
    /// countdown and direction here, and the ECS `HurtTime` component
    /// (`lodestone_ecs::entity::HurtTime`, folded by
    /// `apply_entity_hurt_animation`) is a separate consumer that exists for
    /// the red hurt-flash overlay. Called by the net-apply layer when
    /// `EntityHurtAnimation` names the local player's own id — see
    /// `docs/view-bobbing.md` for that hop.
    pub fn on_local_player_hurt(&mut self, yaw_degrees: f32) {
        self.view_bob.hurt(yaw_degrees);
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
        // (`GameRenderer.java`), and `bobView` itself only tests
        // `isPlayer`. Older versions did suppress it in third person and issue
        // That fix's body says so; re-read against `.cache/mc/26.2/client-src`, that is
        // no longer true.
        let eye = bobbed_camera(
            self.camera(aspect),
            self.bob_frame(),
            // **Still `0.0` here, and that is now a routing decision rather than
            // a hold.** `bobHurt` is almost entirely a roll, and this fold cannot
            // carry one — `Camera` has `position`/`yaw`/`pitch`, two angles,
            // against the bob matrix's three degrees of freedom. Passing a real
            // strength here would not tilt the camera; it would smear the roll
            // into yaw and pitch, which is worse than dropping it.
            //
            // So the hurt half takes the *other* route, the one vanilla itself
            // uses: `Self::damage_tilt_eye_transform` hands it to
            // `RenderState::set_eye_bob_transform`, which multiplies it into the
            // world view-projection in eye space — `projectionMatrix.mul(bobStack)`.
            // This fold keeps `bobView` only, whose own roll term is under `0.3°`.
            //
            // Both halves are therefore live; nothing about the damage tilt is
            // held off any more. See `docs/view-bobbing.md`.
            0.0,
        );
        // `is_first_person`, not "is it the back view": vanilla's own predicate
        // here is `!getCameraType().isFirstPerson()` (`Camera.java`, the
        // `detached` assignment), so the front view takes the *same* pullback
        // path as the back view and differs only by the mirror inside
        // `third_person_camera`.
        if self.camera_type.is_first_person() {
            // Vanilla's FOV zoom is gated on `firstPerson &&
            // isScoping()` (`AbstractClientPlayer.getFieldOfViewModifier`,
            // `AbstractClientPlayer.java`) — a third-person camera
            // never zooms, so this composition only runs on the early
            // first-person return, not the two third-person branches below.
            return apply_spyglass_fov(eye, self.spyglass_scoping());
        }
        let camera_type = self.camera_type;
        if self.is_live() {
            match self.live_collision() {
                Some(view) => third_person_camera(eye, camera_type, &view),
                None => third_person_camera(eye, camera_type, &NoCollision),
            }
        } else {
            let store = self.chunk_world();
            let world = store.read();
            let view = WorldCollision::new(&world);
            third_person_camera(eye, camera_type, &view)
        }
    }

    /// Vanilla's `Player.isScoping()`:
    /// `isUsingItem() && getUseItem().is(Items.SPYGLASS)`
    /// (`Player.java`), computed entirely from `Sim`'s own state so
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
    /// * **`slim`/skin data**: the rig comes from
    ///   [`crate::skin_fetch::current_model`] — the same signed-in-profile
    ///   fetch that already reaches the inventory avatar
    ///   (`container::player_preview`), read here instead of drained from
    ///   its one-shot pending slot so this body sees it on every frame, not
    ///   only the one after a container last opened. The **texture** is a
    ///   separate, still-open gap (`player_skin: None` below;
    ///   `docs/player-skins.md`) — only the rig shape is fixed here.
    /// * **Equipment covers main hand, off hand, and all four armour
    ///   slots.** Main hand is the selected hotbar slot; off hand is native
    ///   inventory index `40`; the armour slots are native indices
    ///   `39/38/37/36` for head/chest/legs/feet (`lodestone_game::menu`'s own
    ///   table, `Menu::player`).
    /// The local player's own animation state for this frame — the walk cycle,
    /// the arm swing, the head pitch, the crouch — with **no camera-mode gate**.
    ///
    /// [`Self::third_person_body_state`] is this plus placement and equipment, and
    /// its `None`-in-first-person early return is a *drawing* decision: the body
    /// must not be drawn when the camera is inside its head. The pose itself is
    /// camera-independent, and one consumer needs it precisely when the camera is
    /// first-person — the **inventory avatar**, which is only ever opened in first
    /// person. That is the whole reason this exists as its own method: the
    /// obstacle was never access to `Sim`'s private `body_pose`, it was that early
    /// return.
    ///
    /// Fed to `ContainerFrame::with_avatar_pose` → `gui_entity_anim`'s `base` in
    /// `app/redraw.rs`; see `docs/inventory-player-preview.md`.
    ///
    /// **`attack_anim` here is a phase, not a fraction.** `1.0` is the rest pose
    /// again, because `HumanoidModel.setupAttackAnimation` drives it through sines
    /// and `sin(π) == 0` — so a consumer that substitutes `1.0` for "fully
    /// swung" measures no movement at all and reads as unwired.
    #[must_use]
    pub fn local_body_anim(&self) -> AnimInput {
        let partial_tick = self.clock().interp_alpha;
        self.body_anim(&self.interpolated_player(), &self.body_pose.render(partial_tick))
    }

    #[must_use]
    pub fn third_person_body_state(&self) -> Option<ThirdPersonBodyState> {
        // `isFirstPerson()`, so the body draws in **both** detached modes. This
        // is also what suppresses the first-person arm and the pumpkin/underwater
        // overlays in the front view: `gpu/frame.rs` derives both from
        // `RenderStats::third_person_body_drawn`, which is this `Option`'s
        // `is_some()`. Asking "is the camera behind me" here instead would put
        // the arm back on screen in front view.
        if self.camera_type.is_first_person() {
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
            anim: self.body_anim(&interp, &walk),
            scale: 1.0,
            slim: crate::skin_fetch::current_model().is_slim(),
            equipment,
        })
    }

    /// The `AnimInput` half of [`Self::third_person_body_state`], taking the two
    /// values that caller already has in hand so nothing is interpolated twice at
    /// a different partial tick.
    ///
    /// Split out for [`Self::local_body_anim`] — see its doc for why.
    #[must_use]
    fn body_anim(
        &self,
        interp: &PlayerState,
        walk: &lodestone_entity::pose::RenderPose,
    ) -> AnimInput {
        AnimInput {
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
            // **Not wired for the local player yet.** Remote
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
            // `Entity.isCrouching()` is `hasPose(Pose.CROUCHING)` — the
            // *pose*, not the shift-key flag (`Entity.java`), and
            // the two genuinely differ: holding shift in a one-block gap
            // leaves you shift-key-down and `SWIMMING`. For the local player
            // the pose is already authoritative and already fit-gated —
            // `lodestone_physics::pose::update_player_pose` writes
            // `PlayerState::pose` as the tail of every tick — so this reads
            // it directly rather than re-deriving a crouch from input.
            crouching: interp.pose == lodestone_physics::pose::Pose::Crouching,
        }
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
