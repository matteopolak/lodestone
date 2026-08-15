//! `WindowApp::redraw`: per-frame HUD assembly and render orchestration.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

impl WindowApp {
    pub(super) fn redraw(&mut self) {
        // Issue #148: refresh the cached recipe corpus if a plugin registered
        // since the last frame. Revision-gated, so the ordinary frame pays one
        // `u64` comparison under a short read guard and nothing else.
        self.sync_recipe_book();
        // Reconcile the menu with the live session before we borrow GPU state.
        // (This also reconciles a server-initiated container close back to
        // `Screen::Playing` — see `drive_ui_from_session`'s own tail.)
        self.drive_ui_from_session();
        if self.sim.open_menu().is_some() && self.ui.is_playing() {
            self.ui.open_container();
            self.set_grab(false);
        }

        // Pace the frame and tick **before** the GPU-readiness guard. Simulation
        // must never be conditional on a swapchain image: keep-alives and the
        // per-tick movement packet ride this loop, and a client the server
        // considers stalled is sent no chunks at all. `step.dt` is already
        // clamped to vanilla's ten-tick catch-up budget, so a long stall is
        // dropped rather than replayed in a burst.
        let frame_start = Instant::now();
        let target_fps = self.current_target_fps(frame_start);
        let step = self.pacer.begin_frame(frame_start, target_fps);
        let dt = step.dt;
        // `Runner::Winit`: the host event loop drives this driver's `App`
        // itself, once per `RedrawRequested`, by calling `update()` directly
        // — no internal timer, so packet ingest is never gated on frame rate.
        self.ecs.update();
        // Issues #202/#203/#443/#444: pushed down before `step`, not after
        // like View Bobbing below — `step` is what actually reads them this
        // call (`apply_mouse`'s look-inversion, the toggle-mode and
        // sprint-window pushes into `InputState`, and the auto-jump gate in
        // the tick loop), so pushing them post-step would apply this frame's
        // option change one frame late.
        self.sim
            .set_mouse_invert(self.nav.invert_mouse_x(), self.nav.invert_mouse_y());
        self.sim.set_sensitivity(self.nav.sensitivity());
        self.sim.set_toggle_modes(
            self.nav.toggle_sneak(),
            self.nav.toggle_sprint(),
            self.nav.toggle_attack(),
            self.nav.toggle_use(),
        );
        self.sim
            .set_sprint_window_ticks(self.nav.sprint_window_ticks());
        self.sim.set_auto_jump(self.nav.auto_jump());
        // Render Distance, on vanilla's 600 ms delay rather than per frame —
        // `WindowApp::render_distance_apply_at`'s doc has the citation and the
        // reason. Before `step` like the pushes above, so the frame that commits
        // it also draws with it.
        self.tick_render_distance(frame_start);
        self.sim.step(dt);
        if !step.render {
            // Unfocused (throttled to ~30 fps) or occluded: skip presenting
            // only. `acquire()` is the call that stalls on a backgrounded
            // window, so it is precisely what must not run here.
            return;
        }

        // Vanilla's VSync option (`options.vsync`). Polled every presented
        // frame, before either draw path, for the same reason View Bobbing
        // just below is: `MenuNav` owns the pure `Options`, and
        // `set_present_mode`'s equality guard is what makes a per-frame poll
        // safe against a GPU setter — only the frame the option actually
        // flips pays for a swapchain rebuild.
        self.sync_vsync_present_mode();
        // Vanilla's View Bobbing option, pushed down before either draw path
        // because the toggle lives on a menu screen and should take effect while
        // that screen is still showing. Polled per frame rather than fired on the
        // toggle for the same reason the present-mode sync was: `MenuNav` owns the
        // `Options` and is pure, and `Sim` owns none.
        self.sim.set_view_bobbing(self.nav.view_bobbing());
        // The other half of vanilla's bob split, pushed down beside it and never
        // instead of it: View Bobbing gates the walk bob, Damage Tilt scales the
        // hurt tilt, and `renderLevel` applies the second whether or not the first
        // is on. Pushed every frame for `set_view_bobbing`'s reason.
        self.sim
            .set_damage_tilt_strength(self.nav.damage_tilt_strength());
        // Vanilla's See-Through Leaves option. Polled per frame like View
        // Bobbing above, but `Sim::set_cutout_leaves`'s own equality guard is
        // what keeps this affordable — see that method's doc.
        self.sim.set_cutout_leaves(self.nav.options().cutout_leaves);
        // Live resource-pack reload. Polled every frame like the options
        // above, but the equality guard here is
        // `crate::resources::pack_generation` rather than a value comparison
        // — `crate::menu::packs::commit`'s own doc used to name this as the
        // missing piece ("this client has no live reload"). By the time
        // `reload_resource_pack_atlas` returns `Some`, every loaded column
        // has *already* been re-meshed against the new atlas (its own doc);
        // this block only has to catch the GPU up: swap the terrain atlas's
        // bind groups in place (never a fifth bind group — see
        // `RenderState::reload_block_atlas`'s doc) and reattach the HUD's and
        // menu's own GUI atlases, which are separate stitches with their own
        // owners.
        if let Some(atlas) = self.sim.reload_resource_pack_atlas() {
            if let Some(gpu) = self.gpu.as_ref()
                && let Some(render) = self.render.as_mut()
            {
                render.reload_block_atlas(gpu.device(), gpu.queue(), &atlas);
            }
            let format = self.target.as_ref().map(lodestone_render::SurfaceTarget::format);
            if let Some(format) = format {
                if let Some(gpu) = self.gpu.as_ref()
                    && let Some(hud) = self.hud.as_mut()
                    && let Some(gui) = crate::resources::load_gui_atlas()
                {
                    hud.attach_gui(gpu.device(), gpu.queue(), format, gui);
                }
                if let Some(gpu) = self.gpu.as_ref()
                    && let Some(menu) = self.menu.as_mut()
                    && let Some(gui) = crate::resources::load_menu_gui_atlas()
                {
                    menu.attach_gui(gpu.device(), gpu.queue(), gui);
                }
            }
        }
        // Vanilla's eleven `soundSource.*` sliders, pushed beside View Bobbing and
        // **before** `draw_menu`'s early return on purpose: the sliders live on the
        // Sound settings page, so a player dragging Master must hear the menu music
        // change while that page is still the whole frame. One mixer lock per
        // frame carries all eleven — see `Sim::set_sound_volumes`.
        self.sim.set_sound_volumes(self.nav.options());
        // Vanilla's FOV option, in degrees. Pushed here rather than folded into
        // `Config` at launch the way `renderDistance` is, because vanilla's `fov`
        // takes the default `applyValueImmediately` and its does not — the slider
        // has to move the view while the settings page is open. `Sim::camera`
        // reads it; `camera_rig::build_camera` clamps it.
        self.sim
            .set_fov_y_degrees(self.nav.options().fov as f32);

        // A menu screen owns the whole frame — its pass clears, so there is no
        // world render behind it and none of the HUD state below is built.
        if self.draw_menu() {
            return;
        }

        // Resolved **before** the field-borrow split below, because both are
        // `&self` methods and everything past this point holds `&mut` borrows of
        // individual fields — the same constraint that makes
        // `recipe_panel_geometry` a free function.
        let creative_open = self.creative_screen_open();
        let creative_title = self.creative_frame_title().unwrap_or_default();
        let creative_menu = creative_open.then(|| self.sim.player_menu());
        // Advancements (#167). Same pre-split resolution, and the hover has to be
        // computed here too: `advancements_layout` centres a tab on first read, so
        // it needs `&mut self.nav` — which the geometry call below also needs, and
        // two `&mut` borrows in one expression will not do.
        let advancements_open = self.ui.is_advancements();
        let advancements_hover = advancements_open
            .then(|| {
                self.target
                    .as_ref()
                    .map(RenderTarget::size)
                    .map(|(w, h)| self.advancements_hover(w, h))
            })
            .flatten()
            .unwrap_or_default();
        // The live progress, read from `SessionAdvancements` and folded into the
        // toast queue — the join that makes #167's frames, progress readouts and
        // hidden-widget reveals real.
        let advancement_progress = self.advancement_progress();
        let toasted = self.advancement_toast(recipe_toast_now_ms());
        let advancement_toast = toasted.map(|a| {
            super::advancements_screen::advancement_toast_view(
                a,
                self.sim.translator().as_ref(),
            )
        });
        let advancements_title = advancements_open
            .then(|| {
                let translate = self.sim.translator();
                advancements_title(self.nav.advancements(), translate.as_ref())
            })
            .unwrap_or_default();

        let (Some(gpu), Some(target), Some(render), Some(hud), Some(container_renderer)) = (
            self.gpu.as_ref(),
            self.target.as_mut(),
            self.render.as_mut(),
            self.hud.as_mut(),
            self.container.as_mut(),
        ) else {
            return;
        };
        let device = gpu.device();
        let queue = gpu.queue();

        // Removals first, then uploads — the order is load-bearing since issue
        // #479 put chunk unloads on this path. The server's `ViewTracker`
        // recenter is a forget/**resend** cycle, so one poll can carry an unload
        // and a re-arrival for the same column, which puts the same `SectionKey`
        // in both drains. Uploading first would let the removal delete the mesh
        // that just arrived, leaving a permanent hole exactly where the player
        // is walking — the bug this fix exists to close, reintroduced by
        // sequencing. Draining removals first is also correct for the older
        // `SnapshotOutcome::Empty` path (a section that snapshots to nothing
        // produces no mesh, so it can never be in both) and it lowers peak
        // section-origin arena occupancy, since a freed slot is reusable by the
        // uploads below in the same frame.
        for key in self.sim.drain_removals() {
            render.remove_section(&key);
        }
        for meshed in self.sim.drain_meshes() {
            render.upload_section(device, queue, meshed.key, &meshed.mesh);
        }

        let (w, h) = target.size();
        let frame = match target.acquire() {
            Ok(frame) => frame,
            Err(e) => {
                if e.needs_reconfigure() {
                    target.reconfigure(device);
                    render.resize(device, w, h);
                }
                // Transient (timeout/occluded/validation): just skip this frame.
                return;
            }
        };

        let aspect = w as f32 / h as f32;
        // Recompute the targeted block from the interpolated camera each frame.
        self.sim.update_target(aspect);
        // The true first-person eye: block targeting and the audio listener
        // deliberately keep reading this one even in third person (see
        // `Sim::camera`'s doc) — only the actual draw call below wants the
        // pulled-back camera.
        let camera = self.sim.camera(aspect);
        // What the frame is actually drawn from: `camera` unmodified in first
        // person, or `camera` pulled back (collision-clamped) behind the
        // player in third person. Installing the third-person body source
        // every frame is cheap (one small `Option` clone, no live borrow of
        // `Sim` needed inside the closure) and keeps the two in lock-step —
        // see `RenderState::set_third_person_body_source`'s doc for why a
        // `None`/`Some` source *is* the camera-mode toggle.
        let render_camera = self.sim.render_camera(aspect);
        let body_state = self.sim.third_person_body_state();
        render.set_third_person_body_source(move || body_state.clone());
        // This frame's arm-swing progress, for the first-person arm pass. Sampled
        // here and moved into the closure rather than captured by reference, for
        // the same reason as `body_state` above: the source outlives this call and
        // must not borrow `Sim`.
        //
        // **Installed every frame, and it has to be** — the value is a partial-tick
        // interpolation, so a one-shot install at connect time would freeze the arm
        // at whatever the swing looked like the instant we joined. `body_state`
        // right above it has the identical requirement, which is why the two sit
        // together. Only the *reading* is per frame; the swing clock itself
        // advances on the 20 Hz tick inside `Sim::step`.
        let hand_swing = self.sim.hand_swing_progress();
        render.set_hand_swing_source(move || hand_swing);

        // The eating/drinking bob — `ItemInHandRenderer.applyEatTransform`. Installed
        // here, next to the swing, because it has the identical partial-tick
        // requirement: the value is `getUseItemRemainingTicks() - frameInterp + 1`,
        // so a one-shot install would freeze the item mid-bite forever. `None` off a
        // consume, which is the plain held-item pose.
        //
        // **Without this line the whole first-person half of eating is invisible** and
        // nothing looks broken: the pass still runs and the food still draws in the
        // hand, just without moving. That is the island shape, which is why this is
        // wired in the same change as the transform rather than left for a follow-up.
        let eating = self.sim.consume_usage_time();
        render.set_item_use_source(move || eating);

        // The hand needs its own copy of the view bob: vanilla applies `bobView`
        // a *second* time to a fresh pose stack seeded with the unbobbed
        // model-view (`GameRenderer.java`), rather than letting the hand
        // inherit the world's bobbed matrix. Without this the whole chain is an
        // island — `hand_view_proj` reads a source nothing installs, so the arm
        // stays rigid while the camera bobs, which is what the player reported.
        let hand_bob = self.sim.bob_frame();
        render.set_hand_bob_source(move || hand_bob);

        // Snapshot the player's nine hotbar slots into owned draw records.
        //
        // **Hoisted above the world render on purpose.** The HUD is the obvious
        // consumer, but `set_main_hand_source` below is read inside
        // `RenderState::render`, so this has to exist before that call. Doing it
        // once here rather than twice serves both from a single `Menu` clone —
        // `Sim::player_menu` clones all 46 slots, and a second call per frame is
        // exactly the cost the mining-freeze fix removed from the tick path.
        let player_menu = self.sim.player_menu();
        let hotbar_records: Vec<Option<HotbarSlot>> = (0..9)
            .map(|i| {
                player_menu.player_native(i).and_then(|st| {
                    let item = ResourceLocation::parse(&st.item().to_string()).ok()?;
                    let damage = st
                        .components()
                        .get_int(lodestone_game::item::DAMAGE_COMPONENT)
                        .and_then(|v| u32::try_from(v).ok());
                    let max_damage = st
                        .components()
                        .get_int(lodestone_game::item::MAX_DAMAGE_COMPONENT)
                        .and_then(|v| u32::try_from(v).ok());
                    Some(HotbarSlot {
                        item,
                        count: st.count().max(0) as u32,
                        damage,
                        max_damage,
                        enchanted: crate::hud::item_icon::stack_has_foil(st),
                        // Mirrors `container::builder::icon_record` — without these
                        // a dyed leather item or a mixed potion held in the hotbar
                        // drew its definition's plain default instead of the real
                        // colour.
                        dyed_color: st.dyed_color(),
                        potion_color: st.potion_color(),
                    })
                })
            })
            .collect();
        drop(player_menu);

        // What the player is holding, for the first-person hand pass. Vanilla's
        // `ItemInHandRenderer` forks on `isEmpty()` and draws *either* the item or
        // the bare arm, never both — `None` here is that empty hand, which is also
        // what the demo path and every headless test get.
        //
        // Installed every frame for the same reason as the swing above: the value
        // changes the instant the player scrolls the hotbar, so a one-shot install
        // would freeze slot 0 into the hand forever. Sampled and moved, because the
        // source outlives this call and must not borrow `Sim`.
        let held = hotbar_records
            .get(self.sim.selected_slot())
            .and_then(|record| record.as_ref())
            .map(|record| crate::gpu::MainHandItem {
                item: record.item.clone(),
                foil: record.enchanted,
                // Mirrors `container::builder::icon_record` and the `HotbarSlot`
                // built above — without these the first-person hand drew a dyed
                // leather item's or a mixed potion's plain default colour even
                // though the identical stack's hotbar icon showed the real one.
                dyed_color: record.dyed_color,
                potion_color: record.potion_color,
            });
        // The item id, re-derived rather than cloned: issue #154's spyglass
        // FOV/vignette needs the bare location further down in this function
        // (`ScreenEffects::scoping`), and the closure otherwise takes ownership
        // of the whole record for the render source's lifetime.
        let held_for_scoping = held.as_ref().map(|item| item.item.clone());
        render.set_main_hand_source(move || held.clone());

        // Block entities — chests (issue #23). **This install is what makes a
        // chest visible at all**: a 26.2 chest has no block model (its
        // `block/chest.json` declares only a particle texture, zero elements), so
        // without this the terrain mesher leaves a hole where every chest is.
        //
        // Installed every frame, like the swing and the held item above and for
        // the same reason: the closure captures this frame's partial tick and a
        // snapshot of the lid map, so a one-shot install at connect would draw
        // every lid frozen at the fraction of a tick we happened to join on.
        if let Some(f) = self.sim.block_entity_source() {
            render.set_block_entity_source(f);
        }

        // Skulls and heads. Same per-frame install as the chests above, though for
        // a weaker reason: none of the ported skull types animate, so there is no
        // partial tick to go stale. It is installed here anyway rather than once at
        // connect so the two block-entity sources cannot drift into different
        // lifetimes — a skull source that survived a disconnect would keep handing
        // out spawns from a dead world's handle.
        if let Some(f) = self.sim.skull_source() {
            render.set_skull_source(f);
        }

        // Signs. Same per-frame install as chests and skulls above; see
        // `Sim::sign_source` for why it captures no partial tick.
        if let Some(f) = self.sim.sign_source() {
            render.set_sign_source(f);
        }

        // Bells. Same per-frame install as the three above — the render pass,
        // the GPU-side wiring in `gpu.rs` and the CPU-side gather
        // (`Sim::bell_source`) were all already landed; this call site was
        // the one remaining hop before a live client draws a bell at all
        // (`docs/block-entity-renderers.md`'s Bell section).
        if let Some(f) = self.sim.bell_source() {
            render.set_bell_source(f);
        }

        // Shulker boxes. Same per-frame install as the four above, and the same
        // reason this call site matters as much as the geometry: a 26.2 shulker
        // box has **no block model**, so without it the terrain mesher leaves a
        // hole where every box is — the chest failure mode exactly.
        if let Some(f) = self.sim.shulker_source() {
            render.set_shulker_source(f);
        }

        // Banners. The pattern compositing, the mask atlas, the flag mesh, the
        // sway and the translucent layer pipeline were all landed with zero
        // consumers; this call site plus `prepare_block_entities`' arm is what
        // finally puts a banner on screen.
        if let Some(f) = self.sim.banner_source() {
            render.set_banner_source(f);
        }

        // Lectern books. Unlike chest/shulker/banner, an unset source here is not
        // a hole — a lectern's shelf and base are real block models — so the
        // failure mode is a lectern that is never holding a book, which is
        // exactly what a missing install looks like from a screenshot. Hence the
        // call site lands with the geometry rather than after it.
        if let Some(f) = self.sim.lectern_source() {
            render.set_lectern_source(f);
        }

        // Campfire cooking items. Installed beside the others but consumed by a
        // different pass: a campfire's renderer draws item models, not a cuboid
        // rig, so this reaches `prepare_item_geometry` rather than
        // `prepare_block_entities`. An unset source is not a hole — the fire and
        // logs are real block models — it is a campfire that never cooks anything.
        if let Some(f) = self.sim.campfire_source() {
            render.set_campfire_source(f);
        }

        // Enchanting-table books. The per-frame install matters more here than
        // anywhere else in this list: the closure captures a snapshot of the
        // animation fold *and* the partial tick, and none of the book's four
        // animated values is on the wire — so a one-shot install draws every book
        // frozen at the tick the session joined on, with no missing packet to
        // blame it on.
        if let Some(f) = self.sim.enchanting_table_source() {
            render.set_enchanting_table_source(f);
        }

        // Moving pistons. A third destination again: not `prepare_block_entities`
        // (no cuboid rig) and not the item path either, but the moving-block-model
        // seam falling blocks share. The per-frame install is the strictest in this
        // list — a whole push lasts two ticks, so a stale closure does not freeze
        // the animation, it pins `progress` at 0 and buries the head inside the
        // piston base.
        if let Some(f) = self.sim.moving_piston_source() {
            render.set_moving_piston_source(f);
        }

        // `GameRenderer.bobHurt` — the damage tilt and the death roll, as an
        // eye-space matrix multiplied into every world view-projection.
        //
        // This is the hop that had been missing, and the reason it was missing is
        // worth keeping: the maths and the option were ported and tested long
        // before this line existed, but `bobbed_camera` cannot carry roll, so
        // `Sim::render_camera` had nowhere to put a 14-degree tilt and passed a
        // hard `0.0`. Vanilla does not fold it into camera fields either — it does
        // `projectionMatrix.mul(bobStack)`, which is exactly this.
        //
        // A value rather than a closure, and installed every frame: the tilt
        // decays over ten ticks with a partial-tick term, so a one-shot install
        // would freeze the camera at whatever angle it was first handed.
        render.set_eye_bob_transform(self.sim.damage_tilt_eye_transform());
        // The hand applies `bobHurt` a *second* time, independently — vanilla's
        // `renderItemInHand` does the same — and it takes the strength rather than
        // the matrix because it composes both bob halves itself.
        render.set_damage_tilt_strength(self.sim.damage_tilt_strength());
        // Vanilla's Glint Speed and Glint Strength accessibility options. Both
        // were already parameters of `glint_clock` and `GlintUniform::new`; the two
        // shell call sites handing over `DEFAULT_SPEED`/`DEFAULT_STRENGTH` was all
        // that kept the rows inert. This covers the **world and hand** draws; the
        // 2-D GUI icon glint is the third site and is owned by the HUD and
        // container renderers (`IconRenderer::set_glint_options`), pushed on the
        // two lines below.
        let (glint_speed, glint_strength) = {
            let o = self.nav.options();
            (f64::from(o.glint_speed), o.glint_strength)
        };
        render.set_glint_options(glint_speed, glint_strength);
        // The third site, in both of its owners. All three read the same wall
        // clock, so a site that misses this push is not merely at the wrong rate —
        // it is out of phase with the other two, which is the visible symptom of a
        // partial push and the reason these lines sit against the one above rather
        // than near the HUD draw.
        hud.set_glint_options(glint_speed, glint_strength);
        container_renderer.set_glint_options(glint_speed, glint_strength);
        // Vanilla's Clouds option (off/fast/fancy). `SkyFrame::with_cloud_status`
        // had zero production callers, so every frame drew FANCY whatever the
        // player chose — the FAST quad path was pixel-gated and unreachable.
        render.set_cloud_status(self.nav.options().cloud_status);
        // The connected dimension's own skybox — vanilla's
        // `DimensionType.skybox()`, and the half of "the Nether renders under the
        // overworld sky" that a fog colour cannot fix. `set_fog`/`set_clear_color`
        // below already pick the Nether's red haze; this is what stops the sun, the
        // moon, the star field and the cloud deck drawing over it.
        //
        // Beside the Clouds push rather than inside the fog block, and
        // unconditional rather than change-detected: this is a per-frame read of
        // the one dimension source (`Sim::dimension`), a single enum compare in
        // `SkyRenderer::render`, and no uniform upload. A change-detected version
        // would need a second `applied_*` field for no measurable saving.
        render.set_sky_mode(self.sim.sky_mode());

        // Filled maps (issue #184). This is the hop that turns the `SessionMaps`
        // fold from an F3 readout into the picture itself — the palette, the
        // per-map texture and the held/framed quads were all landed with no live
        // producer, so without this call a map draws its blank inventory sprite.
        // Re-installed per frame because the closure snapshots the store.
        if let Some(f) = self.sim.map_source() {
            render.set_map_source(f);
        }

        // Reconcile fog with the player's bit-exact fluid state each frame,
        // re-uploading only when it changes (crossing a water/lava surface) so a
        // submerged eye dissolves terrain into short water/lava fog and the
        // surface restores the render-distance sky fog.
        //
        // Weather darkens *both* ends of the gradient before the change check, so
        // the storm reaches the sky disc's centre, its horizon, the terrain fog and
        // the below-horizon clear colour from one place. Doing it after would leave
        // the clear colour bright and put a hard clear-vs-fog seam at the horizon,
        // which is exactly what `set_clear_color`'s own doc warns about.
        //
        // A ramping rain level therefore re-uploads the fog uniform every tick
        // rather than only on a fluid crossing. That is intended: the ramp is
        // ±0.01/tick over ~100 ticks (`ServerLevel.java`), and a
        // change-detected upload that ignored it would render a storm at clear-sky
        // colours until the player happened to swim.
        let weather_state = self.weather.as_ref().map(|w| w.state());
        let desired_fog = {
            let base = self.sim.fog_settings();
            match &weather_state {
                Some(w) => {
                    let rain = w.rain_level();
                    let thunder = w.thunder_level();
                    let flashing = w.flashing();
                    // Vanilla's layer order: the flash tint is added by
                    // `ClientLevel.addEnvironmentAttributeLayers` and the weather
                    // darkening by `WeatherAttributes.addBuiltinLayers` on top, so
                    // a bolt during a storm brightens a sky that is *then* darkened
                    // — not the other way round, which would wash the flash out.
                    let sky = lodestone_render::weather_darken_linear(
                        lodestone_render::lightning_flash_linear(base.sky_color, flashing),
                        rain,
                        thunder,
                    );
                    let fog = lodestone_render::weather_darken_linear(
                        lodestone_render::lightning_flash_linear(base.color, flashing),
                        rain,
                        thunder,
                    );
                    lodestone_render::fog::FogSettings {
                        color: fog,
                        sky_color: sky,
                        ..base
                    }
                }
                None => base,
            }
        };
        if self.applied_fog != Some(desired_fog) {
            render.set_fog(desired_fog, self.config.render_distance);
            // The clear colour must never disagree with the fog colour it is
            // set alongside — see `RenderState::set_clear_color`'s doc and
            // `docs/dimension-visuals.md`'s wiring note. Piggybacking on the
            // same change-detected `if` this fog upload already used is free:
            // there is no separate "did the clear colour change" condition to
            // get out of sync with it.
            // `_tracked`: applies the same `FOG_COLOR` day/night track
            // `fog_with_clock` applies, so the clear colour and the terrain fog
            // cannot drift apart. `desired_fog.color` is the untracked day base
            // (weather-darkened, not clock-tracked), which is exactly what this
            // wants and what `set_fog` two lines up already receives — passing an
            // already-tracked colour would apply the track twice.
            render.set_clear_color_tracked(desired_fog.color);
            self.applied_fog = Some(desired_fog);
        }
        // Drive the audio listener from the exact camera we render, so what the
        // player hears is spatialised to match what they see. No-op when audio
        // is disabled.
        self.sim.set_audio_listener(&camera);
        // In-world music, beside the listener update because both are "audio
        // follows the frame we actually drew".
        //
        // The three inputs are the ones `Minecraft.java` uses, and two of
        // them are easy to get wrong: `creative` is `instabuild && mayfly` and not
        // a gamemode check (`Sim::music_creative`), and `underwater` is
        // water-specific rather than any fluid (`Sim::music_underwater`).
        //
        // `background_music` is the standing biome's own three-slot record, from
        // the 42-biome table, with a **dimension-specific** fallback — see
        // `Sim::background_music`. It is not the biome id: the biome only chooses
        // the record, and `BackgroundMusic::select` makes the pick.
        let background = self.sim.background_music();
        // The portable clock. `std::time::Instant::now()` traps on wasm32; on native
        // this is the identical type, because `web_time` re-exports `std::time`
        // there. See `crate::platform`.
        let now = crate::platform::Instant::now();
        self.sim.tick_music(
            now,
            &crate::audio::music::world_situation(
                &background,
                self.sim.music_creative(),
                self.sim.music_underwater(),
                self.sim.music_volume(),
            ),
        );
        // Cave ambience, the biome/dimension loop and the rain cadence, on the
        // same clock as the music for the same reason — all three are vanilla's
        // 20 Hz `BiomeAmbientSoundsHandler`/`tickWeatherEffects` bookkeeping, and
        // `ShellAmbience::advance` derives whole ticks from this instant rather
        // than running once per frame.
        self.sim.tick_ambience(now, weather_state.as_ref());
        let outline = self.sim.target().map(|hit| hit.block);
        let entity_draws = self.sim.entity_draws();
        // Remote players' skins, in two hops that must both be here. The first
        // starts a fetch for every skin URL in view; it is idempotent per URL, so
        // handing it the same list every frame costs one hash lookup per player.
        // The second turns whatever has landed into a bind group — a `&mut`
        // borrow, so it cannot happen inside the render pass.
        //
        // Without this pair the whole chain is an island: the properties decode,
        // the rig selection and the batch key are all in place and every remote
        // player still draws the pack's default sheet, which is also exactly what
        // an offline-mode server legitimately looks like. See
        // `crate::remote_skins`.
        crate::remote_skins::request_all(
            entity_draws
                .iter()
                .filter_map(|draw| draw.player_skin.as_ref().map(|skin| skin.url.as_str())),
        );
        render.install_pending_player_skins(device, queue);
        // Extraction lives in `Sim` because resolving each particle's light
        // needs the world; doing it here would hand out two borrows of `Sim`.
        let particle_frame = self.sim.extract_particles(&camera);
        render.prepare_particles(device, queue, &self.sim.particle_instances(), &camera);
        let tick = self.sim.tick_count();
        render.update_animation(queue, tick);

        // Precipitation columns. Inlined rather than a `self.` method because
        // `render` is a live `&mut` borrow of `self.render` for the rest of this
        // function, so any `&self` method call here is a second borrow; the pure
        // half lives in `weather_columns_for_frame` instead.
        //
        // Skipped entirely in clear weather — `extract_columns` returns empty on a
        // zero rain level, and the light sample below is the one world lock this
        // costs, so a clear frame pays nothing.
        {
            let (columns, rain_columns) = weather_state
                .as_ref()
                .filter(|w| w.any_precipitation())
                .map(|w| {
                    // ONE light sample per frame, at the eye, reused for every
                    // column — see `ShellWeatherProbe`'s doc for the three
                    // divergences that buys. The *biome* half is a real
                    // per-column lookup (issue #25) and is what used to cost
                    // 441 × 3 world locks a frame; `ShellWeatherProbe::memo`
                    // now takes one lock per chunk column instead, which is why
                    // the probe below **must** stay per-frame. `sky_darken()` is the
                    // weather-folded factor the terrain and entity passes are
                    // already using this frame, so the rain cannot be lit by a
                    // different sky than the blocks it falls past.
                    let packed = self
                        .sim
                        .net()
                        .map(|n| (n.shared_handle(), n.shared_sky_default()))
                        .and_then(|(h, policy)| {
                            crate::net::entity_light_at(
                                &h,
                                camera.position.x.floor() as i32,
                                camera.position.y.floor() as i32,
                                camera.position.z.floor() as i32,
                                // Load-bearing for `sky_visible` below, not just for
                                // brightness: absent sky data used to resolve to 0
                                // here, so `(p >> 4) & 0x0F > 0` was false and rain
                                // rendered **nowhere in open sky** — the one place a
                                // player is guaranteed to be looking at it.
                                policy.get(),
                            )
                        });
                    let probe = ShellWeatherProbe {
                        light: lodestone_render::light::light_term(
                            packed.unwrap_or(lodestone_render::ENTITY_FULLBRIGHT),
                            render.sky_darken(),
                        ),
                        // No sample at all is "world not loaded yet", which must
                        // read as open sky: a `false` here would make the very
                        // first rainy frames after a join silently empty, which is
                        // indistinguishable from the pass being unwired.
                        sky_visible: packed.is_none_or(|p| ((p >> 4) & 0x0F) > 0),
                        handle: self.sim.net().and_then(|n| n.shared_handle().get().cloned()),
                        biome_climates: self.sim.net().map(crate::net::NetClient::shared_biome_climates),
                        // Fresh every frame, by construction — see the field doc.
                        memo: Default::default(),
                    };
                    weather_columns_for_frame(w, &camera, tick, &probe)
                })
                .unwrap_or_default();
            render.prepare_weather(device, queue, &columns, rain_columns, &camera);
        }
        // The underwater/fire overlay pass's per-frame input (issues #108,
        // #112). `eye_in_water` is the *same* `PhysicsState` predicate the
        // submerged fog and the air-bubble row already read
        // (`docs/sky-and-air-bubbles.md`) — not a second derivation. `on_fire`
        // now comes from `PlayerSnapshot::on_fire`, folded by
        // `apply_local_player_on_fire`: the shared-flags byte reaches a generic
        // `EntityFlags` component for any entity, but the local player is
        // deliberately excluded from the generic entity-view path, so it needs a
        // session-scoped fold to arrive at all. `false` without a live
        // connection, which is also the pre-first-packet answer.
        let on_fire = self
            .sim
            .net()
            .and_then(|n| n.shared_handle().get().cloned())
            .is_some_and(|h| h.player().on_fire);
        let spectator = self
            .sim
            .net()
            .and_then(|n| n.shared_handle().get().cloned())
            .and_then(|h| h.game_mode())
            == Some(lodestone_client::GameMode::Spectator);
        // Native slot 39 is the head, per `Menu::player`'s own table (menu slots
        // `5..=8` are head/chest/legs/feet at native `39/38/37/36`, running
        // backwards feet-first) — the same indices `Sim::third_person_body_state`
        // reads for the armour layers.
        //
        // Matched on the item id rather than on
        // `minecraft:equippable.camera_overlay`, which is what vanilla actually
        // keys on (`Hud.extractCameraOverlays`, `Hud.java`). That is a
        // deliberate narrowing and it matches `ScreenEffects::wearing_pumpkin`'s
        // own doc: carved pumpkin is the only item shipping with that component
        // field set, so the general per-item lookup would have exactly one entry.
        // If a second item ever gains it, this is the line that has to become the
        // component read.
        const HEAD_NATIVE_SLOT: usize = 39;
        let wearing_pumpkin = self
            .sim
            .player_menu()
            .player_native(HEAD_NATIVE_SLOT)
            .is_some_and(|st| st.item().to_string() == "minecraft:carved_pumpkin");
        // The freeze overlay's per-frame input (issue #139). `PlayerState::
        // percent_frozen` is real, tested physics state (`update_freezing`,
        // issue #212, `lodestone-physics`) — not a stub. `Sim::player()`
        // already returns `PlayerState` by value, so this needs no new `Sim`
        // accessor. See `docs/screen-overlays.md`'s "Freeze" section.
        let freeze_percent = self.sim.player().percent_frozen();
        let screen_effects = crate::gpu::ScreenEffects {
            eye_in_water: self.sim.player().eye_in_water,
            on_fire,
            spectator,
            tick,
            wearing_pumpkin,
            freeze_percent,
            // `Player.isScoping()` is `isUsingItem() && getUseItem().is(Items.
            // SPYGLASS)` (`Player.java`). Both halves: `Sim::
            // using_item()` (the two-line accessor issue #154 was waiting
            // on) and `held_for_scoping`, the same item id already computed
            // above for the first-person hand pass.
            scoping: self.sim.using_item()
                && held_for_scoping
                    .as_ref()
                    .is_some_and(|loc| loc.namespace() == "minecraft" && loc.path() == "spyglass"),
            // No potion-effect-duration tracker exists anywhere in this
            // codebase yet, so `0.0` is still the honest answer for nausea —
            // a placeholder pretending to work would be worse. See
            // `docs/screen-overlays.md`'s "Confusion and portal" section.
            nausea_intensity: 0.0,
            // The portal overlay's alpha, live: `Sim::portal_effect_intensity`
            // is vanilla's `Mth.lerp(partialTicks, oPortalEffectIntensity,
            // portalEffectIntensity)`, ramped +0.0125/tick while the player's
            // bounding box overlaps a `nether_portal` cell and decayed
            // -0.05/tick after (`sim/dimension.rs`). This one scalar reaches
            // **two** effects with different shapes — the overlay's alpha
            // directly, and `max(portal, nausea)` plus a speed blend for the
            // world-projection warp — which is why the pass takes the raw
            // intensity rather than a pre-multiplied strength.
            portal_intensity: self.sim.portal_effect_intensity(),
        };
        // Route the progressive-mining crack overlay(s) (issue #410): the local
        // player's own dig plus one slot for every *other* player's overlay the
        // server has reported. `CrackPipeline`/`render_with_crack_and_effects`
        // accept any number of targets in one pass, and `Sim::crack_targets`
        // is the accessor that actually walks `SessionBlockDestruction`/
        // `BlockDestructionOverlays` via `crate::gpu::gather_crack_targets` —
        // the hop that was still missing when #410 was closed: the gather and
        // the pipeline were both proven in isolation, but nothing in
        // production called the gather, so only the local target ever reached
        // this vec.
        let cracks: Vec<crate::gpu::CrackTarget> = self.sim.crack_targets();
        let stats = render.render_with_crack_and_effects(
            device,
            queue,
            frame.view(),
            &render_camera,
            outline,
            &entity_draws,
            &cracks,
            screen_effects,
        );

        // Fold GPU counters + timing into the debug overlay.
        let frame_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
        // Counted, not derived from `1.0 / dt` — see
        // `FramePacer::record_presented_frame`'s doc for why a reciprocal of
        // the pacer's own scheduling `dt` reports the wrong quantity once a
        // framerate cap makes the event loop iterate far more often than it
        // presents. This call site is reached only after every early return
        // above it (occlusion, missing GPU state, a failed `acquire()`, a
        // menu screen owning the whole frame) — i.e. only when a frame really
        // was drawn and is about to be presented.
        self.pacer.record_presented_frame(frame_start);
        self.sim.stats.section_count = stats.sections_drawn;
        self.sim.stats.quads = stats.total_quads;
        self.sim.stats.vram_bytes = stats.vram_bytes;
        self.sim.stats.vram_reserved_bytes = stats.vram_reserved_bytes;
        self.sim.stats.entities_drawn = stats.entities_drawn;
        self.sim.stats.particles_alive = particle_frame.alive;
        self.sim.stats.particles_drawn = stats.particles_drawn;
        self.sim.stats.particles_unresolved = particle_frame.unresolved;
        // The occlusion-cull split (U3/U5). These five `RenderStats` fields were
        // populated and reached the test harness but no pixels — the same island
        // shape `stats.adapter` and `stats.difficulty` were fixed for, one field
        // set over. `occlusion_active` is the one that has to be on screen: every
        // failure mode of this cull draws *more*, so a zero cull count cannot
        // distinguish an open surface from a graph that refused to walk.
        self.sim.stats.occlusion_graph_sections = stats.occlusion_graph_sections;
        self.sim.stats.sections_culled_occlusion = stats.sections_culled_occlusion;
        self.sim.stats.sections_occlusion_shadow = stats.sections_occlusion_shadow;
        self.sim.stats.occlusion_active = stats.occlusion_active;
        self.sim.stats.occlusion_walks = stats.occlusion_walks;
        self.sim.stats.frame_ms = frame_ms;
        self.sim.stats.fps = self.pacer.fps() as f32;
        // `ServerDifficulty` reached a real, tested ECS fold but nothing in the
        // shell read it — this is that last hop, onto the F3 overlay's own
        // `Difficulty:` line (`hud.rs`'s `DebugStats::left_lines`).
        self.sim.stats.difficulty = self.sim.difficulty();
        // The F3+B / F3+G state, for the overlay's own `Debug overlays:` line —
        // vanilla's `Debug charts:` block, carrying the two toggles that exist
        // here. Copied from the `Arc<AtomicBool>`s the world-line source closure
        // reads (`install_debug_lines_source`), not from a shell-side mirror, so
        // the hint and the draw cannot disagree about whether boxes are on. This
        // write lives here rather than in `Sim::refresh_stats` because the atomics
        // are owned by `WindowApp`, not by `Sim`.
        {
            use std::sync::atomic::Ordering;
            self.sim.stats.hitboxes_shown = self.debug_hitboxes.load(Ordering::Relaxed);
            self.sim.stats.chunk_borders_shown = self.debug_chunk_borders.load(Ordering::Relaxed);
        }

        // The baked 3-D item geometry, shared by the container screen below and the
        // HUD hotbar further down. It borrows `self.sim`, so it cannot be hoisted
        // above the `self.sim.stats` writes just above — but it must exist before
        // the container overlay, which is the pass that was missing it.
        // Sound-subtitle captions (issue #198). Gated on the persisted
        // `showSubtitles` accessibility option. Collected **here**, above
        // `item_models`, and not beside the rest of the HUD frame: this needs
        // `&mut self.sim` (the caption queue purges as it is read) while
        // `item_models` holds an immutable borrow of `self.sim` all the way to the
        // hotbar draw.
        let sound_subtitles = if self.nav.options().show_subtitles {
            self.sim.sound_subtitles(&camera)
        } else {
            Vec::new()
        };

        let item_models = self.sim.vanilla_atlas().and_then(|a| a.models());

        // Assemble the HUD frame: debug overlay, chat log + prompt, tab list,
        // and the survival gauges. Locals are collected up-front so their
        // borrows outlive the frame struct.
        let chat_open = self.ui.is_chat_open();
        // Built up front (not down where `hud_frame.chat_options` used to
        // build its own copy) because the scroll window below needs
        // `rows_per_page` before `chat_lines` is even fetched. Reused
        // verbatim for `hud_frame.chat_options`, so the two cannot read two
        // different snapshots of the options mid-frame.
        // `chat_opts`, not `chat_opts_raw` — `menu::nav::tests::
        // app_rs_still_threads_every_chat_option_into_the_hud_frame` greps this
        // file's own source text for the literal `chat_opts.<field>` per chat
        // setting, so the wiring detector needs this exact local name to keep
        // seeing every field it checks.
        let chat_opts = self.nav.options();
        let chat_display_opts = crate::hud::ChatDisplayOptions {
            scale: chat_opts.chat_scale,
            width_pct: chat_opts.chat_width,
            height_pct_unfocused: chat_opts.chat_height_unfocused,
            height_pct_focused: chat_opts.chat_height_focused,
            line_spacing: chat_opts.chat_line_spacing,
            text_opacity: chat_opts.chat_opacity,
            background_opacity: chat_opts.chat_background_opacity,
            colors: chat_opts.chat_colors,
        };
        let chat_rows_per_page = crate::hud::chat_lines_per_page(
            chat_display_opts,
            crate::hud::chat_pose_scale(chat_display_opts),
            chat_open,
        );
        // Pull enough history for the HUD to fade/scroll; it caps and ages them.
        // Open, this needs the *full* history (capped at the feed's own
        // 100-entry limit — `lodestone_game::chat::ChatFeed`'s own cap) so the
        // scroll window and the new-arrival sync below have everything to
        // work with. Closed cannot be scrolled at all (there is no
        // `ChatScreen` to hold a position while the box is not up), so the
        // small fixed fetch from before this feature is untouched and
        // unaffected by anything below.
        // The feed hands back owned spans (the hex-carrying sibling of the
        // legacy-flattened `String` this used to read — `Session::recent_chat`
        // still exists for the other, untouched draw path); borrow them into
        // the `&[TextSpan]` slice the HUD frame takes, keeping both locals
        // alive for the frame's scope.
        let chat_spans_owned: Vec<(Vec<lodestone_model::text::TextSpan>, f32)> =
            self.sim.recent_chat_spans(if chat_open { 100 } else { 10 });
        // `ChatScroll::sync` only reads `history` on the open branch, so
        // building this unconditionally (rather than gating the clone on
        // `chat_open`) costs nothing extra on the closed path (10 short
        // strings) and keeps the call site to one line. Scrolling only needs
        // the visible text, not the colour, so each entry's spans are
        // concatenated back to a plain `String` here.
        let chat_history_strings: Vec<String> = chat_spans_owned
            .iter()
            .map(|(spans, _)| spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect();
        // Reproduces `ChatComponent.addMessageToDisplayQueue`'s "a new
        // message while scrolled does not jump the view" behaviour, and
        // `ChatScreen.removed`'s `resetChatScroll()` when closed — see
        // `ChatScroll::sync`'s own doc for why both live in one call here
        // rather than a separate close hook.
        self.chat_input
            .scroll_mut()
            .sync(chat_open, &chat_history_strings, chat_rows_per_page);
        let (chat_win_start, chat_win_end) = if chat_open {
            self.chat_input
                .scroll()
                .window_range(chat_spans_owned.len(), chat_rows_per_page)
        } else {
            (0, chat_spans_owned.len())
        };
        let chat_spans_lines: Vec<(&[lodestone_model::text::TextSpan], f32)> = chat_spans_owned
            [chat_win_start..chat_win_end]
            .iter()
            .map(|(spans, age)| (spans.as_slice(), *age))
            .collect();
        // `None` while closed — vanilla's own scrollbar draws only in the
        // `isForeground` (open) display mode.
        let chat_scrollbar_view = chat_open.then(|| crate::hud::ChatScrollbar {
            scrolled: self.chat_input.scroll().scrolled(),
            total: chat_spans_owned.len(),
            new_message_since_scroll: self.chat_input.scroll().new_message_since_scroll(),
        });
        // Rows, header and footer together — the whole `PlayerTabOverlay` frame.
        // Read only while the overlay is up, because it is a world clone.
        let tab_view = self.tab_held.then(|| self.sim.tab_list_view());
        let health = self.sim.health();
        let food = self.sim.food();
        // Vanilla's `canHurtPlayer()` — the single gate `extractPlayerHealth` sits
        // behind, so it hides the hearts, the hunger row and the air bubbles together,
        // and (through `hasExperience()`, whose body is identical) the XP bar too.
        // Read off the same `shared_handle` the `spectator` flag above uses; the
        // predicate itself lives in `crate::hud::can_hurt_player` so the
        // `isSurvival()`-not-`Creative` distinction is testable without a window.
        let can_hurt_player = crate::hud::can_hurt_player(
            self.sim
                .net()
                .and_then(|n| n.shared_handle().get().cloned())
                .and_then(|h| h.game_mode()),
        );
        // `HudState::MAX_AIR` — the same constant `PlayerSnapshot::air` fills
        // an unreported value with — rather than a second hardcoded `300`.
        let air = self
            .sim
            .air()
            .map(|a| (a, lodestone_game::player_state::HudState::MAX_AIR, self.sim.player().eye_in_water));
        let sidebar = self.sim.sidebar();
        let boss_bars = self.sim.boss_bars();
        // Two different questions, and they used to share one boolean named
        // `crosshair` — which is why the hotbar vanished behind the pause menu
        // and the inventory (issue #61). The crosshair is the aiming reticle and
        // belongs to *active* play; the hotbar belongs to the **world**, and
        // vanilla keeps it on screen behind every in-game screen.
        let crosshair = self.ui.is_playing();
        let world_hud = hud_follows_world(self.ui.screen());

        // The command-suggestion dropdown. Declared **before** `hud_frame`
        // because the frame borrows both, and a local declared after it would be
        // dropped first. `chat_open` gates it as well as the list's own
        // existence: closing the box does not clear the completion state (a
        // cancelled line is deliberately recoverable), so an ungated popup would
        // survive the close by one frame.
        let suggestion_ghost = chat_open
            .then(|| self.chat_input.suggestion_ghost())
            .flatten();
        let suggestion_list = chat_open.then(|| self.chat_input.suggestion_list()).flatten();

        let mut hud_frame = HudFrame::new(&self.sim.stats);
        hud_frame.show_debug = self.show_debug;
        hud_frame.crosshair = crosshair;
        // `chat_spans`, not `chat`: a non-empty `chat_spans` wins outright over
        // the legacy `&str` path (see `HudFrame::chat_spans`'s own doc), and is
        // the only one of the pair carrying a hex `TextColor::Rgb` past this
        // point. `chat_wrap_spans` is left `None` — no persisted spans cache
        // exists yet, so the visible log is re-wrapped every frame on this path
        // exactly as the legacy path was before issue #527 (a); `chat_wrap`
        // below still caches nothing for it since it caches `&str`, not spans.
        hud_frame.chat_spans = &chat_spans_lines;
        hud_frame.sound_subtitles = &sound_subtitles;
        // Persisted wrap results (issue #527 (a)): without this the whole
        // visible log is re-wrapped, quadratically, every frame. Retained for
        // the (now-dormant) legacy `chat` path; `chat_spans` above has no
        // persisted cache of its own yet.
        hud_frame.chat_wrap = Some(&self.chat_wrap);
        hud_frame.chat_input = chat_open.then(|| self.chat_input.as_str());
        // Vanilla blinks the text cursor on a 300 ms half-period:
        // `TextCursorUtils.CURSOR_BLINK_INTERVAL_MS == 300` and
        // `isCursorVisible(ms) == (ms / 300) % 2 == 0`
        // (`.cache/mc/26.2/client-src/.../TextCursorUtils.java`). The
        // phase has to come from wall time rather than the tick clock, because
        // the caret keeps blinking while the game is paused.
        // `crate::platform::epoch_duration`, not `SystemTime::now()`: the latter
        // compiles for wasm32 and TRAPS at runtime, and the caret blinks every
        // frame, so a browser tab would die on the first chat open.
        hud_frame.chat_caret_visible =
            (crate::platform::epoch_duration().as_millis() / 300) % 2 == 0;
        // Without these two lines the whole dropdown is an island: the state
        // machine in `chat.rs` runs, its unit tests pass, and zero pixels change.
        hud_frame.chat_suggestion_ghost = suggestion_ghost.as_deref();
        hud_frame.chat_suggestions = suggestion_list.map(|list| crate::hud::SuggestionPopup {
            line: self.chat_input.as_str(),
            start: list.start(),
            candidates: list.candidates(),
            selected: list.current(),
            offset: list.offset(),
            // The tooltip's anchor, and its gate: vanilla shows a candidate's
            // `Message` only while the pointer is over the list. `self.cursor`
            // is physical pixels; the popup's rect is logical-canvas ones.
            cursor: Some(crate::hud::HudRenderer::canvas_cursor(
                w,
                h,
                self.nav.gui_scale(),
                self.cursor,
            )),
        });
        // Without this the whole chat-option chain is an island: the fields are
        // persisted, `ChatDisplayOptions` is read by the draw, and the live
        // client would still show vanilla defaults forever.
        //
        // `chat_display_opts`, not a fresh `self.nav.options()` read — built
        // once, above, so the scroll-window math earlier in this function and
        // this draw-facing copy cannot disagree about the frame's own chat
        // settings.
        hud_frame.chat_options = chat_display_opts;
        hud_frame.chat_scrollbar = chat_scrollbar_view;
        hud_frame.players = tab_view.as_ref();
        hud_frame.sidebar = sidebar.as_ref();
        hud_frame.boss_bars = &boss_bars;
        hud_frame.can_hurt_player = can_hurt_player;
        hud_frame.health = health;
        // The armour row. `Sim::armour_value` is `floor(minecraft:armor)` off the
        // local player's folded attribute snapshot — vanilla's
        // `LivingEntity.getArmorValue()` — so equipment reaches the bar the way it
        // reaches any other attribute, as a server `update_attributes` push, and
        // there is no per-item table anywhere in the chain. Without this line the
        // whole row is an island: `HudFrame::armour` defaults to `None`, both draw
        // paths and both gates are correct, and zero pixels change.
        hud_frame.armour = self.sim.armour_value();
        hud_frame.food = food;
        // Without this the hunger wobble (issue #30) is computed correctly and
        // never fires: vanilla shakes the row only while saturation is
        // exhausted, so an unfed `saturation` reads as "always satisfied".
        hud_frame.saturation = self.sim.saturation();
        hud_frame.air = air;
        hud_frame.hotbar = world_hud.then(|| self.sim.selected_slot());
        hud_frame.hotbar_items = world_hud.then_some(hotbar_records.as_slice());
        hud_frame.xp = self.sim.xp();
        hud_frame.title = self.sim.title_overlay();
        hud_frame.action_bar = self.sim.action_bar_overlay();
        hud_frame.held_item = self.sim.held_item_overlay();
        hud_frame.recipe_stats = self
            .recipe_book
            .as_ref()
            .map(|book| (book.len(), book.tags().len()));
        // Issue #436's `SessionWorldBorder`/`SessionSpawnPoint` folds reaching
        // the screen. Both were folded, reset on quit-to-title and gated
        // through the real `SharedState::apply` path with **no reader
        // anywhere in the shell**; these two lines are the first. See
        // `HudFrame::border_debug` for why this is the debug overlay and not
        // yet the vignette tint vanilla draws.
        hud_frame.border_debug = self.sim.world_border_warning();
        hud_frame.spawn_debug = self.sim.spawn_point().pos();
        // Issue #184's `SessionMaps` fold reaching the screen. A diagnostic and
        // not the map's own picture — see `HudFrame::map_debug` for what is still
        // missing and why it is a texture job rather than a wiring one.
        hud_frame.map_debug = self.sim.map_debug();
        // The recipe-unlock toast (issue #163). `None` on every real session
        // today, because the queue's only possible producer is the
        // `recipe_book_add` decode that does not exist yet — see the field's own
        // doc. Wired here anyway so it lights up the moment that lands.
        hud_frame.recipe_toast = recipe_toast_view(&self.recipe_toasts, recipe_toast_now_ms());
        // The advancement-completion toast (issue #167), resolved above the
        // field-borrow split like every other `Sim`-derived view.
        hud_frame.advancement_toast = advancement_toast;
        // Always `Some`: `Sim::attack_strength_scale` is defined on both the
        // demo and live worlds (the ticker and the `attack_speed` attribute
        // default both exist before any server connection), unlike
        // `health`/`food`/`xp` which stay `None` until a server reports them.
        // `hud.rs`'s draw site is what actually gates this on
        // `frame.crosshair` — see that field's doc for why the crosshair
        // hides behind an open screen but the hotbar does not (issue #61).
        hud_frame.attack_cooldown = Some(self.sim.attack_strength_scale());
        // The 3-D block-item icons need the baked model set (for geometry) and a
        // depth attachment (so the near faces of the mini-block win over the far
        // ones). Both are `None` on the demo path, which degrades to flat sprites.
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            Some(render.depth_view()),
            &hud_frame,
            item_models,
            self.nav.gui_scale(),
            w,
            h,
        );
        // Status-effect overlay, composited over the HUD in its own Load pass.
        if let Some(effects) = self.effects.as_mut() {
            effects.render(device, queue, frame.view(), &self.sim.active_effects(), w, h);
        }

        // The container overlay draws **after** the HUD (issue #51/#61): vanilla's
        // `Gui.render` draws the HUD unconditionally behind any world-following
        // screen (`hud_follows_world` above), and the screen then paints its own
        // translucent background over it (`Screen.java`,
        // `AbstractContainerScreen::isInGameUi`) — the dim is draw order, not a
        // per-element alpha. Drawing this block before the HUD (as it used to)
        // meant the HUD painted back over the container's dim every frame and the
        // hotbar never actually looked dimmed behind an open chest. Both this pass
        // and the HUD's own model sub-pass independently clear the shared depth
        // buffer immediately before drawing their own GUI items, so swapping the
        // two relative to each other is safe — see `docs/container-screen.md`.
        // The creative-inventory screen (issue #158) **replaces** the player's
        // inventory screen rather than overlaying it, exactly as vanilla's
        // `Minecraft.openInventory` picks one screen or the other. So it is
        // resolved before the container block below and short-circuits it — two
        // panels drawn over each other is what an overlay would give.
        if creative_open {
            let geo = creative_panel_geometry(
                &self.creative,
                creative_menu.as_ref(),
                &creative_title,
                // The same cursor and the same tooltip option the ordinary container
                // frame is given below. Without these the creative screen draws no
                // hover highlight, no cursor stack and no tooltip — the three things
                // that made taking an item out of it feel broken.
                Some([self.cursor.0, self.cursor.1]),
                Some(self.nav.advanced_item_tooltips()),
                container_renderer,
                item_models,
                self.nav.gui_scale(),
                w,
                h,
            );
            container_renderer.render_geometry_scaled(
                device,
                queue,
                frame.view(),
                Some(render.depth_view()),
                &geo,
                self.nav.gui_scale(),
                w,
                h,
            );
        }
        let open_menu = self.sim.open_menu();
        // The open merchant's trade list (issue #245's UI half), read once
        // per frame and reused both for the composed title below and for
        // `ContainerFrame::with_trades` — see `Sim::trades`'s own doc for why
        // this is cheap and safe to read unconditionally (empty, never a
        // guard-needing `None`, off a live merchant screen).
        let trades = self.sim.trades();
        let player_menu;
        let (container_menu, container_title) = if let Some(open) = open_menu.as_ref() {
            // Through the language table, not `Text::to_plain_string` — the
            // server sends `translate("container.crafting")`, and the model's
            // stub table has no `container.*` key, so flattening it directly put
            // the raw key on screen (issue #52). See `container::menu_title`.
            //
            // The merchant screen composes a level badge into the title
            // itself (`MerchantScreen.extractLabels`) rather than merely
            // moving the anchor — `container::merchant_title` is the whole
            // reason `menu_type_title_anchor` no longer excludes it. Keyed
            // off `open.menu.special_layout()`, not the wire `menu_type`
            // string: if the server ever sends a `merchant` menu whose size
            // does not match `MerchantMenu`'s three slots, `Menus::build_menu`
            // has already fallen back to a plain generic container, and this
            // must agree with that fallback rather than re-deriving it.
            let title = if open.menu.special_layout() == Some(lodestone_game::menu::SpecialLayout::Merchant)
            {
                crate::container::merchant_title(
                    &open.title,
                    trades.villager_level(),
                    trades.show_progress(),
                    self.sim.translator().as_ref(),
                )
            } else {
                crate::container::menu_title(&open.title, self.sim.translator().as_ref())
            };
            (Some(&open.menu), title)
        } else if self.ui.is_container_open() {
            player_menu = self.sim.player_menu();
            // **"Crafting"**, not "Inventory" (issue #370). `InventoryScreen`
            // passes `translatable("container.crafting")` as its title
            // (`InventoryScreen.java`) — it names the 2x2 grid — and the
            // literal `"Inventory"` that used to sit here was wrong twice: wrong
            // word, and, going in as the *title*, drawn at the title anchor,
            // which on this one screen is `x = 97`. The word "Inventory" does
            // exist in vanilla, as the *second* label, which this screen is the
            // only one to omit; see `container::label_layout`.
            (
                Some(&player_menu),
                crate::container::player_inventory_title(self.sim.translator().as_ref()),
            )
        } else {
            (None, String::new())
        };
        if container_menu.is_some() && !creative_open {
            // `playerInventoryTitle` through the same language table. A local
            // constant here is not the #52 defect class repeating: vanilla reads
            // it from `Inventory.getDisplayName()`, itself the client-side
            // constant `translatable("container.inventory")`
            // (`Inventory.java`), so there is no server component to resolve.
            let inventory_label =
                crate::container::player_inventory_label(self.sim.translator().as_ref());
            // `merchant.trades` — "Trades", the merchant screen's second label
            // (issue #245's UI half). Computed unconditionally like
            // `inventory_label` above; `ContainerFrame`'s own draw path is
            // what gates it on the screen actually being a merchant.
            let trades_label = crate::container::merchant_trades_label(self.sim.translator().as_ref());
            // The anvil rename box's current value (issue #603).
            // `AnvilRenameState::sync` is vanilla's `slotChanged`: it resets
            // `self.anvil_rename` to the input slot's own hover name whenever
            // that slot's identity changes, and otherwise leaves whatever the
            // player has typed alone (`KeyOutcome::AnvilRename` in
            // `app/lifecycle.rs` is what edits it) — see that module's own
            // doc. `None` off any non-anvil screen, matching every other
            // special-layout-only field below; the state itself is not
            // cleared on leaving the screen, only its value stops being read
            // (see `WindowApp::anvil_rename`'s own doc).
            let anvil_name = container_menu.and_then(|menu| {
                if menu.special_layout() != Some(lodestone_game::menu::SpecialLayout::Anvil) {
                    return None;
                }
                let item = menu.slot_item(0).map(|stack| {
                    (
                        stack.custom_name().is_some(),
                        lodestone_game::item::styled_hover_name(stack, self.sim.translator().as_ref()),
                    )
                });
                self.anvil_rename.sync(
                    item.as_ref()
                        .map(|(has_custom_name, name)| (*has_custom_name, name.as_str())),
                );
                Some(self.anvil_rename.value.clone())
            });
            // Does the recipe-book panel own the pointer this frame? The *click*
            // path has consulted this predicate before the container's own hit
            // test since the panel landed (`handle_recipe_panel_click`); the draw
            // had no equivalent, so the hovered-slot highlight and the tooltip
            // resolved to whatever slot sat under the book. Same predicate, same
            // shared layout — see `recipe_panel_pointer_hit`.
            let hover_blocked = container_menu.is_some_and(|menu| {
                recipe_panel_pointer_hit(
                    self.recipe_book.as_ref(),
                    &self.recipe_panel,
                    menu,
                    self.nav.gui_scale(),
                    self.cursor,
                    w,
                    h,
                )
                .is_some()
            });
            // The carried stack follows the pointer, so the frame needs the cursor
            // in physical pixels — the same space `hit_test` and the menu layout
            // use (see the `cursor` field). Without this the stack is built but
            // never positioned, and nothing draws.
            // The live drag preview (issue #378 part 2). `drag_paint` is the
            // *same* paint set `MenuInput::release` will turn into the
            // QUICK_CRAFT sequence, and the counts drawn from it come out of
            // `Menu::quick_craft_plan`, which is what distributes them — so the
            // preview cannot show a split the release will not produce.
            let container_frame = ContainerFrame::new(container_menu, &container_title)
                .with_inventory_label(&inventory_label)
                // The trade list and which row is selected (issue #245's UI
                // half) — `Sim::trades` returns an empty (never absent) store
                // off a non-merchant screen, and `draw_merchant_trades` only
                // ever draws when `menu.special_layout()` is `Merchant`, so
                // this is unconditional like `with_menu_type` below rather
                // than guarded on the screen kind here.
                .with_trades(Some(&trades), self.merchant_selected)
                .with_trades_label(&trades_label)
                .with_cursor(Some([self.cursor.0, self.cursor.1]))
                // The hovered slot's tooltip. This is the *only* caller that
                // enables it, which is deliberate — see `ContainerFrame::tooltips`
                // — and the flag it passes is vanilla's persisted
                // `advancedItemTooltips`, toggled by F3+H.
                .with_tooltips(self.nav.advanced_item_tooltips())
                // An open recipe book moves the panel right (vanilla's
                // `updateScreenPosition`). `container_input`'s hit-test passes the
                // same flag through `hit_test_with_book` — the two must agree.
                .with_book_open(self.recipe_panel.open)
                // …and the panel **consumes the pointer** over itself, so no slot
                // highlights or tooltips under it. Deliberately not expressed by
                // withholding the cursor: the carried stack must keep following
                // the pointer across the book. See `ContainerFrame::hover_blocked`.
                .with_hover_blocked(hover_blocked)
                .with_drag(self.menu_input.drag_paint())
                // The wire `menu_type`, which is what `menu_type_title_anchor`
                // keys on. Without this line the nine per-screen title anchors
                // are correct and **unfed**, so a furnace or an anvil silently
                // falls back to the generic `(8, 6)` — the same class of gap as
                // a source installed but never set.
                .with_menu_type(open_menu.as_ref().map(|open| &open.menu_type))
                .with_recipe_book(self.recipe_book.as_ref())
                // The inventory avatar's **live pose**. Vanilla poses the real
                // render state, so a player who opens their inventory during the
                // tail of a swing sees that swing. Without this line
                // `gui_entity_anim`'s `base` argument — which exists for exactly
                // this — is fed `AnimInput::REST` forever and the avatar is a
                // mannequin.
                //
                // **The walk cycle now arrives too.** This used to be a two-field
                // literal over `AnimInput::REST` with a note that
                // `limb_swing`/`limb_swing_amount` were unreachable: the walk state
                // lives on `Sim::body_pose`, whose only public reader was
                // `third_person_body_state`, and that returns `None` in first
                // person — the only camera mode the inventory screen is ever open
                // in. The obstacle was that early return, not the private field, so
                // `Sim::local_body_anim` is the same construction without it, and
                // the avatar now walks, crouches and pitches its head exactly as
                // the third-person body does.
                //
                // `hand_swing_progress`/`tick_count` are gone from here on purpose:
                // `local_body_anim` reads `attack_anim` and `age_ticks` off the
                // *same* `body_pose.render(partial_tick)` call as the limb swing, so
                // the swing and the walk cannot drift by a frame.
                .with_avatar_pose(self.sim.local_body_anim())
                // The local player's own uuid (issue #646), so the
                // inventory avatar's *default* skin resolves through the
                // same `default_skin_for_uuid` call the world side already
                // uses for every other player with no declared skin —
                // see `ContainerFrame::avatar_uuid`'s doc. `None` off a
                // live session, which keeps the pre-login bootstrap default.
                .with_avatar_uuid(self.sim.local_uuid())
                // The anvil's XP cost and the enchanting table's three level
                // costs (`docs/container-cost-screens.md`'s "What is not yet
                // wired" gap). `&[]` on the player-inventory screen (no
                // `open_menu`), which draws neither cost — correct, since
                // neither special layout is ever the player's own inventory.
                .with_cost_context(
                    open_menu.as_ref().map_or(&[][..], |open| open.data.as_slice()),
                    self.sim.has_infinite_materials(),
                    self.sim.xp().map_or(0, |(level, _)| level),
                )
                .with_anvil_name(anvil_name.as_deref());
            // `render_with_icons_scaled`, **not** `render_scaled`: the latter
            // hardcodes `depth: None, models: None`, so `want_models` was always
            // false and `push_item_model` returned early. Flat sprite icons still
            // drew (they need only `attach_items`), which is exactly why the symptom
            // read as "block items render *flat*" rather than "nothing renders" —
            // and why it survived as an island with `attach_items` *and*
            // `attach_item_models` both already wired.
            //
            // The `_scaled` variant is required: the plain one lays out against
            // `AUTO_GUI_SCALE` and would disagree with `hit_test_with_scale` about
            // where the slots are.
            //
            // `_between_strata`, and **not** a plain call followed by the panel
            // draw: the recipe book belongs to the stratum between the slots and
            // the carried stack, so the panel goes *into* this call rather than
            // after it. That method's own doc carries the vanilla order and the
            // reported symptom; the short version is that the container renderer
            // draws the carried stack and the hovered-slot tooltip itself, so
            // anything submitted after the whole call covers both.
            container_renderer.render_with_icons_scaled_between_strata(
                device,
                queue,
                frame.view(),
                Some(render.depth_view()),
                &container_frame,
                item_models,
                self.nav.gui_scale(),
                w,
                h,
                || {
                    // The recipe-book panel (issue #163), **over** the container
                    // panel it belongs to and **under** the cursor stack — the
                    // toggle button sits on the container's own chrome and the
                    // book body overlaps its left edge at narrow canvases
                    // (`container.rs`'s documented clamp), so drawing it before
                    // the slots would bury both.
                    //
                    // This call is what stops the whole
                    // `recipe_book_panel_layout`/`_hit_test`/`_geometry` family
                    // being an island: it was built and unit-tested with 75 tests
                    // and reached zero pixels because nothing composited the
                    // vertices.
                    let Some(menu) = container_menu else {
                        return;
                    };
                    let items = hud.item_atlas();
                    // The search box's *text* needs the same vanilla font the
                    // container's own labels use — without it the box drew as an
                    // empty well, which is why it read as missing entirely.
                    let font = hud.font();
                    if let Some(geo) = recipe_panel_geometry(
                        self.recipe_book.as_ref(),
                        &self.recipe_panel,
                        menu,
                        self.nav.gui_scale(),
                        items.as_deref(),
                        item_models,
                        font.as_deref(),
                        w,
                        h,
                        // The hover tooltip vanilla draws over a recipe button
                        // (`RecipeBookPage.extractTooltip`). The same cursor and
                        // the same persisted `advancedItemTooltips` flag the
                        // container's own slot tooltip above uses, so the two can
                        // never disagree about which lines an identical stack
                        // shows — and `hover_blocked` above already stops the
                        // container drawing a second tooltip for whatever slot
                        // sits under the book.
                        //
                        // **One residual divergence, recorded rather than
                        // fixed.** Vanilla's tooltips are deferred
                        // (`setTooltipForNextFrame`) and composited after
                        // everything, so `recipeBookComponent.extractTooltip`
                        // lands above the carried stack too. Ours rides the tail
                        // of this one geometry blob, which has no tooltip split
                        // marker, so a player *carrying* a stack while hovering a
                        // recipe button sees the stack over the tooltip. The two
                        // *slot* tooltips cannot conflict — `hover_blocked` makes
                        // them mutually exclusive — so this is the only case, and
                        // closing it means giving
                        // `RecipeBookPanelGeometry` a tooltip range the way
                        // `chrome_vertex_count` already splits its chrome.
                        crate::container::RecipeTooltipContext {
                            cursor: Some([self.cursor.0, self.cursor.1]),
                            advanced: self.nav.advanced_item_tooltips(),
                        },
                    ) {
                        hud.render_recipe_book_panel(
                            device,
                            queue,
                            frame.view(),
                            Some(render.depth_view()),
                            &geo,
                            self.nav.gui_scale(),
                            w,
                            h,
                        );
                    }
                },
            );
        }

        // The pause overlay draws *over* the world/HUD/container passes above
        // rather than replacing them — see `Screen::Paused`'s doc comment and
        // `menu::render::owns_frame`'s, which is deliberately why `Paused` is
        // not in that set: adding it there would route this screen through
        // `draw_menu`'s `Clear` pass instead and stop the world rendering
        // behind it for as long as the game is paused.
        if self.ui.is_paused()
            && let Some(menu) = self.menu.as_mut()
        {
            let pause_frame = crate::menu::render::pause_frame(&self.nav);
            menu.render_overlay(device, queue, frame.view(), &pause_frame, w, h);
        }

        // The Advancements screen (issue #167), drawn over the still-rendering
        // paused world for the same reason the pause overlay above is: it is
        // reached from the pause menu and vanilla keeps the world behind it.
        //
        // Through `ContainerRenderer` rather than `MenuRenderer` — see
        // `crate::menu::advancements`' module doc. That is also why
        // `menu::render::owns_frame` deliberately excludes it and
        // `frame_for` returns `None`: without this block that `None` would mean
        // "invisible", the same trap in-world Settings and the command block
        // editor were both caught by.
        let gui_scale_now = self.nav.gui_scale();
        if advancements_open
            && let Some(geo) = advancements_panel_geometry(
                self.nav.advancements_mut(),
                &advancements_hover,
                &advancement_progress,
                &advancements_title,
                container_renderer,
                item_models,
                gui_scale_now,
                w,
                h,
            )
        {
            container_renderer.render_geometry_scaled(
                device,
                queue,
                frame.view(),
                Some(render.depth_view()),
                &geo,
                gui_scale_now,
                w,
                h,
            );
        }

        // The death screen (issue #103) follows exactly the same overlay
        // shape as pause, for the same reason: a live server holds a dead
        // player with no chunk stream until it respawns, so this must draw
        // over the still-rendering, still-ticking world rather than replace
        // it — see `Screen::Death`'s doc comment.
        if self.ui.is_death()
            && let Some(menu) = self.menu.as_mut()
        {
            let death_frame = crate::menu::render::death_frame(&self.nav, self.sim.death_message());
            menu.render_overlay(device, queue, frame.view(), &death_frame, w, h);
        }

        // The resource-pack prompt (a server's own `ClientboundResourcePackPushPacket`,
        // not a menu button) follows the same overlay shape as Death/Paused
        // immediately above, for the same reason: it must not itself stop the
        // world from rendering or the session from ticking. It differs from
        // both in *when* it can be up — `Screen::ResourcePackPrompt` is
        // reachable from `Screen::Connecting` (a push can arrive during
        // Configuration, before Play), where `frame_for` already returns
        // `None` and `draw_menu` already falls through to this world path —
        // so no extra branch is needed for that case; the world render
        // still clears and presents a real (if chunk-less) frame underneath.
        if let Some(prompt_frame) =
            crate::menu::nav::resource_pack_prompt_overlay_frame(&self.ui, &self.nav)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.render_overlay(device, queue, frame.view(), &prompt_frame, w, h);
        }

        // Issue #449's terrain half. `Screen::Connecting` covers the
        // handshake/configuration phase as a full frame (see `frame_for`); this
        // block covers the moments after login while the player's own chunk is
        // still streaming in. Drawn as an overlay over the still-rendering
        // world rather than replacing it, for the same reason Paused/Death are
        // overlays: chunks must keep meshing and uploading behind the text —
        // the very thing a full-frame `owns_frame` screen would stop. The
        // predicate is vanilla's own `DownloadingTerrainScreen` rule (the chunk
        // column under the player is loaded), so the text clears the moment the
        // ground the player is standing on arrives.
        //
        // The bar and the count line only appear when there is a real
        // denominator to divide by (the session declared a view radius); with
        // none, this stays the bare phase label it always was, because a
        // progress bar wired to nothing is worse than no bar.
        if self.ui.is_playing()
            && self.sim.terrain_loading()
            && let Some(menu) = self.menu.as_mut()
        {
            let label = crate::menu::loading::ConnectPhase::LoadingTerrain.label();
            let loading_frame = match self.sim.terrain_progress() {
                Some(progress) => crate::menu::render::loading_frame_with_progress_and_grid(
                    label,
                    progress,
                    // Issue #568: vanilla's `LevelLoadingScreen` grid, one real
                    // square per column in the current view. `None` until a
                    // view radius is declared — the same precondition
                    // `progress` above already gates on — so the grid can
                    // never draw ahead of the bar it sits beside.
                    self.sim.terrain_chunk_grid(),
                ),
                None => crate::menu::render::loading_frame(label),
            };
            menu.render_overlay(device, queue, frame.view(), &loading_frame, w, h);
        }

        // In-world Options, from a player report: settings opened mid-game used
        // to draw the *panorama* behind itself, which belongs to the main menu
        // only. `menu::render::frame_for` now returns `None` for
        // `Screen::Settings` when `ui.settings_in_world()`, so the frame has to
        // be drawn here as an overlay over the still-rendering paused world —
        // the same shape as pause and death above. Without this block that
        // `None` means the screen draws *nothing*, which is worse than the
        // panorama it replaced, so the two halves must stay together.
        //
        // The frame comes from `nav::settings_overlay_frame` rather than from a
        // `settings_frame` call written out here — the same rule the command
        // block block below follows, and for a measured reason. This site used to
        // build it raw, which skipped the canvas facts `frame_for` stamps on
        // every full-screen frame, so in-world Options had no `MenuFrame::list`
        // and therefore none of the band chrome the main-menu copy has (a
        // 2026-08-09 player report), no hover cursor, and a `Panorama` backdrop
        // over the paused world. One expression, two consumers: `on_screen_frame`
        // hit-tests the same frame this draws.
        if let Some(settings_frame) =
            crate::menu::nav::settings_overlay_frame(&self.ui, &self.nav)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.render_overlay(device, queue, frame.view(), &settings_frame, w, h);
        }

        // Issue #474: the command block edit screen was drawn **nowhere**.
        // `menu::render::frame_for` correctly has no arm — it is an overlay, not
        // a full screen — but neither did `on_screen_frame`, and neither did
        // this function, so `command_block_frame` had no production caller at
        // all and the screen's clicks never hit-tested. Right-clicking a command
        // block opened a screen that rendered nothing.
        //
        // Exactly the shape of the in-world Settings block above, and of
        // `0d0ae93`'s fix: a screen whose `frame_for` answers `None` for a
        // correct reason still needs someone to draw it, or the `None` means
        // "invisible" rather than "overlay".
        //
        // `command_tree()` is passed rather than `None`: the suggestion popup on
        // this screen is fed by the real server's tree now that #470 decodes it
        // and #471 routes it to the shell. `None` draws no popup at all, which
        // is the honest fallback before a tree arrives.
        //
        // The frame comes from `nav::command_block_overlay_frame` rather than
        // from a `render::command_block_frame` call written out here, because
        // `nav::on_screen_frame` — what the *mouse* hit-tests against — calls
        // the same function. Two constructions of one screen's geometry is a
        // click landing on a row the draw put elsewhere, and it is invisible in
        // a screenshot. One expression, two consumers.
        if let Some(command_block_frame) =
            crate::menu::nav::command_block_overlay_frame(&self.ui, &self.nav)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.render_overlay(device, queue, frame.view(), &command_block_frame, w, h);
        }

        // The sign-editing screen — the fifth overlay, same shape as the
        // command block block immediately above and for the same reason:
        // `menu::render::frame_for` has no arm for it (it is an overlay, not a
        // full screen), so without a draw call here the screen would open,
        // hit-test correctly (`nav::on_screen_frame` already calls
        // `sign_edit_overlay_frame`), and render nothing.
        if let Some(sign_edit_frame) = crate::menu::nav::sign_edit_overlay_frame(&self.ui, &self.nav)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.render_overlay(device, queue, frame.view(), &sign_edit_frame, w, h);
        }

        // `key.screenshot` (issue #16), and **this position is the whole
        // correctness argument**: every pass above — world, HUD, container, and
        // the three overlay blocks — has now written into `frame.view()`, and
        // `present` below consumes `frame` by value. Capturing right after
        // `acquire()` (the plan this replaced) would copy out a swapchain image
        // with no defined content yet. See `docs/keybindings.md`'s "Screenshot".
        //
        // A failure is logged and dropped: a screenshot must never take the
        // frame loop down. The flag is cleared either way, so a target that
        // structurally cannot be captured (headless — `texture()` is `None`
        // there) does not retry forever.
        if self.pending_screenshot {
            self.pending_screenshot = false;
            if let Some(texture) = frame.texture() {
                // Fully qualified rather than imported: this module's only namespace
                // is `use super::*`, so bringing `SystemTime` in would mean adding an
                // import to `app.rs` — a file several agents edit concurrently — for
                // one call site.
                // `UNIX_EPOCH + epoch_duration()` rather than `SystemTime::now()`,
                // which traps on wasm32. Identical value on native, and it keeps
                // `capture`'s signature (which only ever *reads* the instant handed
                // to it — it never calls `now()`) unchanged.
                match crate::screenshot::capture(
                    device,
                    queue,
                    texture,
                    std::time::UNIX_EPOCH + crate::platform::epoch_duration(),
                ) {
                    Ok(path) => println!("Saved screenshot as {}", path.display()),
                    Err(e) => tracing::warn!(target: "screenshot", "capture failed: {e}"),
                }
            } else {
                tracing::warn!(
                    target: "screenshot",
                    "this render target has no presentable texture to capture"
                );
            }
        }

        if let Some(window) = &self.window {
            window.pre_present_notify();
        }
        frame.present(queue);

        if self.last_log.elapsed() >= Duration::from_secs(1) {
            self.last_log = Instant::now();
            println!("{}", self.sim.stats.one_line());
        }
    }

    /// This frame's `app::pacing::effective_target_fps` — the live
    /// `framerateLimit`/`inactivityFpsLimit` folded with how long since the
    /// last real input, at `now`. `None` means "no cap": a focused window
    /// lets vsync/the compositor pace it, exactly as before either option
    /// existed.
    ///
    /// Called from both [`Self::redraw`] (to decide whether to render) and
    /// `about_to_wait` (to decide how the event loop should wait) — see
    /// `app::pacing::FramePacer::control_flow`'s doc for why the second call
    /// is what keeps a low cap from becoming a busy-wait.
    pub(super) fn current_target_fps(&self, now: Instant) -> Option<u32> {
        crate::app::pacing::effective_target_fps(
            self.nav.options().framerate_limit,
            self.nav.options().inactivity_fps_limit,
            self.pacer.idle_secs(now),
        )
    }

    /// Vanilla's `options.vsync` (`Options.java`, default `true`).
    /// Polled every presented frame rather than pushed on toggle — see
    /// `docs/frame-pacing.md` and the deleted `unlock_framerate` debug knob
    /// this reuses the exact reasoning (and the exact `SurfaceTarget` API) of.
    ///
    /// `true` restores the adapter's **remembered** default
    /// (`SurfaceTarget::default_present_mode`, almost always `Fifo`) rather
    /// than `wgpu::PresentMode::AutoVsync` — those are not the same:
    /// `AutoVsync` resolves to `FifoRelaxed` wherever it exists, which
    /// permits tearing on a late frame, while the default config vanilla
    /// picks is plain `Fifo`. `false` is `AutoNoVsync`, never a concrete
    /// `Immediate`/`Mailbox` an adapter might not advertise — `AutoNoVsync`
    /// degrades to `Fifo` and simply stays capped, which is the safe failure
    /// mode for an option a player can flip on a GPU nobody has tested.
    fn sync_vsync_present_mode(&mut self) {
        let (Some(gpu), Some(target)) = (self.gpu.as_ref(), self.target.as_mut()) else {
            return;
        };
        let mode = if self.nav.options().enable_vsync {
            target.default_present_mode()
        } else {
            wgpu::PresentMode::AutoNoVsync
        };
        target.set_present_mode(gpu.device(), mode);
    }
}
