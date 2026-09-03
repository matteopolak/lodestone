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
        let connected = self.sim.session_phase() == crate::sim::SessionPhase::Connected;
        let benchmark_frame = self.benchmark.as_mut().map(|driver| {
            let intent = driver.update(frame_start, connected);
            (
                intent,
                driver.label(intent.segment),
                driver.workload().name(),
                driver.elapsed(frame_start).unwrap_or_default(),
            )
        });
        if let Some((intent, label, workload, elapsed)) = benchmark_frame {
            // Bind the label before `begin_frame`: the profiler snapshots it
            // for the new pending row, whose interval becomes known and is
            // written at the following call.
            self.frame_profile.set_segment(Some(label));
            if self.benchmark_segment != Some(intent.segment) {
                let [x, y, z] = self.sim.stats.position;
                tracing::info!(
                    target: "frame_benchmark",
                    workload,
                    segment = label,
                    elapsed_seconds = elapsed.as_secs_f64(),
                    player_x = x,
                    player_y = y,
                    player_z = z,
                    loaded_columns = self.sim.stats.live_columns,
                    rss_bytes = crate::hud::process_rss_bytes(),
                    "benchmark segment transition"
                );
                self.benchmark_segment = Some(intent.segment);
                if intent.complete {
                    tracing::info!(
                        target: "frame_benchmark",
                        workload,
                        segment = label,
                        "benchmark complete"
                    );
                    self.ui.request_quit();
                }
            }
        } else {
            self.frame_profile.set_segment(None);
        }
        // Starts this frame's per-phase CPU timing (`app::frame_profile`) and
        // finalises whatever the *previous* call left pending — see that
        // module's doc for why finalisation lives here rather than in every
        // early `return` below.
        self.frame_profile.begin_frame(frame_start);
        // Issue #613's `PingRequest` remainder — see `WindowApp::last_ping_request`'s
        // own doc for why this is gated on F3 rather than sent every tick the
        // way vanilla's own ungated debug-overlay ping monitor is. `send_ping_request`
        // itself is best-effort (a closed session drops it silently), so this
        // needs no separate "are we connected" check.
        if should_send_ping_request(self.show_debug, self.last_ping_request, frame_start) {
            let time_ms = i64::try_from(crate::platform::epoch_duration().as_millis()).unwrap_or(i64::MAX);
            self.sim.send_ping_request(time_ms);
            self.last_ping_request = Some(frame_start);
        }
        let target_fps = self.current_target_fps(frame_start);
        let step = self.pacer.begin_frame(frame_start, target_fps);
        let dt = step.dt;
        // Issues #202/#203/#443/#444: pushed down before `step`, not after
        // like View Bobbing below — `step` is what actually reads them this
        // call (`apply_mouse`'s look-inversion, the toggle-mode and
        // sprint-window pushes into `InputState`, and the auto-jump gate in
        // the tick loop), so pushing them post-step would apply this frame's
        // option change one frame late.
        if self.benchmark.is_some() {
            // The showcase orbit's raw-pixel integration is calibrated at
            // vanilla's 0.5 sensitivity. Inversion/toggle/autojump options
            // would change the choreography, so the opt-in benchmark owns
            // these input semantics for its process lifetime.
            self.sim.set_mouse_invert(false, false);
            self.sim.set_sensitivity(0.5);
            self.sim.set_toggle_modes(false, false, false, false);
            self.sim.set_sprint_window_ticks(0);
            self.sim.set_auto_jump(false);
        } else {
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
        }
        // Render Distance, on vanilla's 600 ms delay rather than per frame —
        // `WindowApp::render_distance_apply_at`'s doc has the citation and the
        // reason. Before `step` like the pushes above, so the frame that commits
        // it also draws with it.
        self.tick_render_distance(frame_start);
        if let Some((intent, _, _, _)) = benchmark_frame {
            self.sim.input_mut(|input| {
                input.set(Action::Forward, intent.forward);
                input.set(Action::Sprint, intent.sprint);
                input.set(Action::Jump, intent.jump);
                if intent.mouse_dx != 0.0 {
                    input.add_mouse(intent.mouse_dx, 0.0);
                }
            });
        }
        self.frame_profile.mark(FramePhase::Setup, Instant::now());
        self.sim.step(dt);
        self.frame_profile
            .record_relight_workload(self.sim.take_relight_workload());
        self.frame_profile.mark(FramePhase::SimTick, Instant::now());
        if !step.render {
            // Unfocused (throttled to ~30 fps) or occluded: skip presenting
            // only. `acquire()` is the call that stalls on a backgrounded
            // window, so it is precisely what must not run here. Every phase
            // from here on is simply never marked this frame — a normal,
            // frequent outcome the profiler counts as a skip, not a zero.
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
        // Vanilla's Particles option. Polled per frame beside See-Through
        // Leaves rather than pushed on the settings write, because `Sim` is
        // rebuilt on every session start and a one-shot push would be lost
        // across a reconnect. A plain enum write, so no equality guard.
        self.sim.set_particle_level(self.nav.options().particles);
        // Vanilla's Biome Blend option. Polled per frame beside See-Through
        // Leaves, and for the identical reason it needs the same treatment: a
        // real change re-meshes every loaded column, so
        // `TerrainMesh::set_blend_radius`'s equality guard — not this call
        // site — is what stops that happening every frame.
        self.sim
            .set_blend_radius(self.nav.options().biome_blend_radius);
        // Vanilla's Entity Shadows option (owner report: "entity shadows are
        // missing"). Polled per frame like the options above; a plain bool
        // write, so no equality guard is needed the way `set_cutout_leaves`'s
        // own doc explains one is for a remesh-triggering setter.
        if let Some(render) = self.render.as_mut() {
            render.set_entity_shadows_enabled(self.nav.options().entity_shadows);
        }
        // Live resource-pack reload. Polled every frame like the options
        // above, but the equality guard here is
        // `crate::resources::pack_generation` rather than a value comparison
        // — `crate::menu::packs::commit`'s own doc used to name this as the
        // missing piece ("this client has no live reload"). By the time
        // `reload_resource_pack_atlas` returns `Some`, every loaded column
        // has *already* been re-meshed against the new atlas (its own doc);
        // this block only has to catch the GPU up: swap the terrain atlas's
        // bind groups in place (never a fifth bind group — see
        // `RenderState::reload_block_atlas`'s doc) and reattach every surface
        // that is a *separate* stitch with its own owner — the HUD's and menu's
        // GUI atlases, the flat item atlas and its glint sheet, and the 3-D
        // block-item pass, which is the one that merely *borrows* the world
        // renderer's atlas/palette/anim objects and is therefore left holding
        // dropped ones. Each of the four blocks below carries its own reason.
        if let Some(atlas) = self.sim.reload_resource_pack_atlas() {
            if let Some(gpu) = self.gpu.as_ref()
                && let Some(render) = self.render.as_mut()
            {
                render.reload_block_atlas(gpu.device(), gpu.queue(), &atlas);
            }
            // The 3-D block-item pass is the one surface that borrows the
            // *world* renderer's GPU objects rather than owning them: its
            // atlas view, tint palette and animation buffers all come from
            // `RenderState`'s `ModelRenderer`, and `reload_block_atlas`
            // above replaced all three with new objects while re-baking
            // every sprite's UVs. A bind group built at bring-up keeps the
            // *old* texture alive (wgpu resources are `Arc`-backed), so
            // nothing errors: the hotbar and every container slot go on
            // sampling a dropped atlas with freshly repacked coordinates,
            // which reads as block items rendering wrong — or, wherever the
            // new UVs land on atlas padding, as not rendering at all —
            // while the flat sprites below stay right because their atlas
            // and their UVs are replaced together. `attach_item_models`
            // rebuilds the whole `ModelIcons`, the same "rebuild the GPU
            // object, overwrite the field" shape `reload_block_atlas` uses
            // for its own bind groups.
            //
            // This is the **only** icon surface that belongs inside this
            // `if let`, and that is the whole point of the split below it:
            // it re-binds objects `reload_block_atlas` has just replaced, so
            // it is meaningless on a frame where that did not run.
            let format = self.target.as_ref().map(lodestone_render::SurfaceTarget::format);
            if let Some(format) = format
                && let Some(gpu) = self.gpu.as_ref()
                && let Some(render) = self.render.as_ref()
                && let (Some(view), Some(sampler), Some(palette), Some(anim)) = (
                    render.model_atlas_view(),
                    render.model_atlas_sampler(),
                    render.model_palette_buffer(),
                    render.model_anim_buffer(),
                )
            {
                if let Some(hud) = self.hud.as_mut() {
                    hud.attach_item_models(gpu.device(), format, view, sampler, palette, anim);
                }
                if let Some(container) = self.container.as_mut() {
                    container.attach_item_models(
                        gpu.device(),
                        format,
                        view,
                        sampler,
                        palette,
                        anim,
                    );
                }
            }
        }
        // Every icon surface that is its **own** stitch gets its **own**
        // generation latch, deliberately outside the block-atlas reload above.
        //
        // These were inside it, and that stranded them for the whole process.
        // `Sim::reload_resource_pack_atlas` advances its own
        // `last_pack_generation` *before* its remaining guards and then returns
        // `None` on three of them — no `net` (the demo world), no vanilla atlas
        // (a jar-less run), or its own `BlockResources::load(true)` falling back
        // to the demo palette. Each of those consumes the generation, the
        // counter only moves forward, and nothing retries: one such frame and
        // the flat item sprites, the two GUI atlases and the special-renderer
        // icon sheets keep the *previous* pack until the process exits. That is
        // the reported "the server's texture pack applied to fonts but not to
        // my items — held, slot, or on the ground": `vanilla_font` re-resolves
        // lazily per generation and so is immune, while every surface here is
        // pushed exactly once from a consumed edge.
        //
        // None of these reads the block atlas, so gating them on a *block*
        // reload was never right on its own terms either.
        let icon_pack_generation = crate::resources::pack_generation();
        if icon_pack_generation != self.last_icon_pack_generation {
            self.last_icon_pack_generation = icon_pack_generation;
            if let Some(gpu) = self.gpu.as_ref()
                && let Some(render) = self.render.as_mut()
            {
                // The world block-entity pass owns the Steve sheet a failed
                // custom-head skin fetch falls back to. Rebind it here rather
                // than leaving the owner on the pack state from startup.
                render.reload_block_entity_textures(gpu.device(), gpu.queue());
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
                // Container sheets are loose `textures/gui/container/**` art,
                // not entries in either GUI-sprite atlas. Rebuild their separate
                // owner on the same generation edge as the menu atlas so a
                // server pack's custom container screen is not stranded at the
                // startup version while its buttons already change.
                if let Some(gpu) = self.gpu.as_ref()
                    && let Some(container) = self.container.as_mut()
                    && let Some(background) = crate::resources::load_container_background()
                {
                    container.attach_background(gpu.device(), gpu.queue(), format, background);
                }
                // The flat item sprites are their own stitch with their own
                // owner, exactly like the two GUI atlases above — so a pack's
                // `textures/item/*.png` never reached a slot without this.
                // Rebuilt rather than cloned: `load_item_atlas` reads the
                // current pack stack on every call (there is no cache), which
                // is what makes the icons and the world agree about which pack
                // won. The glint sheet rides along, as at bring-up.
                let item_atlas = crate::resources::load_item_atlas();
                let glint_sheet = item_atlas
                    .as_ref()
                    .and_then(|_| crate::resources::load_glint_texture());
                if let Some(gpu) = self.gpu.as_ref()
                    && let Some(items) = item_atlas.clone()
                {
                    if let Some(hud) = self.hud.as_mut() {
                        hud.attach_items(gpu.device(), gpu.queue(), format, items.clone());
                        if let Some(img) = &glint_sheet {
                            hud.attach_glint(gpu.device(), gpu.queue(), format, img);
                        }
                    }
                    if let Some(container) = self.container.as_mut() {
                        container.attach_items(gpu.device(), gpu.queue(), format, items);
                        if let Some(img) = &glint_sheet {
                            container.attach_glint(gpu.device(), gpu.queue(), format, img);
                        }
                    }
                }
            }
            // The **special-renderer** icon pass (chest, shulker box, banner,
            // shield, skull, player head) — the third icon stream, and the one
            // the blocks above do not reach. It is not re-attached, it is
            // *dropped*: unlike the flat and 3-D streams it borrows nothing and
            // owns its own block-entity sheets, decoded from the pack stack that
            // was live on the frame it first built itself. So the failure here is
            // not "sampling a dropped atlas" — it is a perfectly valid sheet that
            // belongs to the *previous* pack, so a pack restyling a chest, a
            // shulker box, a banner or a skull never reached an inventory slot.
            //
            // The world's own block-entity pass reads the same loader and is
            // built once in `RenderState::new`, which `reload_block_atlas` does
            // not rebuild — so it has this defect too. That one is not fixed
            // here: it is a different owner and needs its sheets rebound rather
            // than a latch cleared.
            //
            // It sits outside the `format`/`gpu` guards above because it needs
            // neither: the rebuild happens lazily on the next frame carrying a
            // special icon, and that is also what makes a *failed* first build
            // recoverable — `special_tried` latched on that one attempt and
            // nothing else ever cleared it, so a stream that came up dark stayed
            // dark for the whole process.
            //
            // Both screens keep their own `IconRenderer`, so both are reached.
            if let Some(hud) = self.hud.as_mut() {
                hud.reload_special_icons();
            }
            if let Some(container) = self.container.as_mut() {
                container.reload_special_icons();
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
        self.frame_profile.mark(FramePhase::MeshUpload, Instant::now());

        let (w, h) = target.size();
        let frame = match target.acquire() {
            Ok(frame) => frame,
            Err(e) => {
                if e.needs_reconfigure() {
                    target.reconfigure(device);
                    render.resize(device, w, h);
                }
                // Transient (timeout/occluded/validation): just skip this
                // frame. `Acquire` is left unmarked — see the module doc: a
                // stall or a transient failure here is exactly the case this
                // phase exists to separate from ordinary pacing cost, so it
                // must show as a skip too, never a fabricated zero.
                return;
            }
        };
        self.frame_profile.mark(FramePhase::Acquire, Instant::now());

        // The menu background blur reads the pixels already drawn into this
        // frame, so it needs the texture behind `frame.view()` *before* anything
        // draws into it — captured once here rather than at each of the several
        // `render_overlay` sites below. Without this call `MenuRenderer`'s own
        // frame texture stays `None` forever and the whole blur pass is a silent
        // no-op: built, tested, reaching no pixels.
        // The live `options.menuBackgroundBlurriness`, read before the borrow
        // below rather than inside it. Polled per presented frame beside
        // `begin_frame` — one site covers every `render_overlay` call in this
        // function, the same reason `begin_frame` itself is one call.
        let blur_radius = self.nav.options().menu_background_blurriness as f32;
        if let Some(menu) = self.menu.as_mut() {
            menu.begin_frame(frame.colour_texture().clone());
            menu.set_blur_radius(blur_radius);
        }

        // A second, non-colour-managed view of the same swapchain texture —
        // vanilla's own 2-D GUI blending is not colour-managed at all, so the
        // HUD's flat-colour pass (text, stack counts, durability bars) needs
        // this instead of `frame.view()`'s (sRGB) one to match it byte-for-byte.
        // Captured once here, the same pattern as `frame.colour_texture()`
        // above, since every HUD draw call this frame wants the identical view.
        //
        // Asked of the renderer rather than derived from `target`: the view's
        // format has to agree with the format `HudRenderer::new` compiled its
        // flat-colour pipeline against, and those two facts used to live in two
        // files. `flat_colour_view` is that agreement made structural.
        //
        // **This was attempted once before and reverted**, because at the time
        // `HudRenderer` drew its flat-colour verts in the *same* pass as the
        // sprite/glint/model pipelines: those keep the corrected (sRGB) format,
        // a wgpu pass fixes one attachment format for every pipeline drawn into
        // it, and so the item pipelines could not draw at all — inventory
        // icons and air bubbles disappeared. The renderer now gives the
        // flat-colour stream its own pass in both entry points
        // (`hud-colour-pass`, and the recipe panel's chrome/count passes),
        // which is precisely what lets the raw view come back without taking
        // the textured passes with it.
        let hud_raw_view = hud.flat_colour_view(&frame);

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

        // The eating/drinking bob — vanilla's own held-item eat-transform. Installed
        // here, next to the swing, because it has the identical partial-tick
        // requirement: the value is the remaining use ticks minus the frame's
        // partial-tick interpolation plus one,
        // so a one-shot install would freeze the item mid-bite forever. `None` off a
        // consume, which is the plain held-item pose.
        //
        // **Without this line the whole first-person half of eating is invisible** and
        // nothing looks broken: the pass still runs and the food still draws in the
        // hand, just without moving. That is the island shape, which is why this is
        // wired in the same change as the transform rather than left for a follow-up.
        let item_use = self.sim.item_use_render_state();
        render.set_item_use_source(move || item_use);

        // The hand needs its own copy of the view bob: vanilla applies `bobView`
        // a *second* time to a fresh pose stack seeded with the unbobbed
        // model-view, rather than letting the hand
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
                    // Modern servers can retain a vanilla gameplay item id while
                    // replacing only its client-side item-definition lookup.
                    let item = st.item_model().unwrap_or_else(|| st.item().clone());
                    let item = ResourceLocation::parse(&item.to_string()).ok()?;
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
                        custom_model_data: st.custom_model_data(),
                        // Mirrors `container::builder::icon_record` — without these
                        // a dyed leather item or a mixed potion held in the hotbar
                        // drew its definition's plain default instead of the real
                        // colour.
                        dyed_color: st.dyed_color(),
                        potion_color: st.potion_color(),
                        // Same crate-boundary loss as the dye/potion pair above,
                        // for a banner's loom patterns rather than its colour —
                        // without this a banner in the hotbar drew its base
                        // colour only, never its pattern.
                        banner_patterns: st.banner_patterns().to_vec(),
                        // Same crate-boundary loss as the pattern line above,
                        // for a shield's own dye tint rather than its loom
                        // patterns.
                        base_color: st.base_color().map(str::to_owned),
                        // And the same again for a custom head's own skin: a
                        // decorative head carried in the hotbar drew the default
                        // skull sheet while the identical head placed in the
                        // world drew its real face. `stack_skin_url` also starts
                        // the fetch; see its doc.
                        skin: crate::hud::item_icon::stack_skin_url(st),
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
                custom_model_data: record.custom_model_data,
                // Mirrors `container::builder::icon_record` and the `HotbarSlot`
                // built above — without these the first-person hand drew a dyed
                // leather item's or a mixed potion's plain default colour even
                // though the identical stack's hotbar icon showed the real one.
                dyed_color: record.dyed_color,
                potion_color: record.potion_color,
                // Same crate-boundary loss as the dye/potion pair above: without
                // this a held banner drew its base colour but never its own
                // loom patterns, even though the identical stack's hotbar icon
                // now does.
                banner_patterns: record.banner_patterns.clone(),
                // Same crate-boundary loss as the pattern line above, for a
                // held shield's own dye tint — without this a held shield
                // combined with a banner drew no base tint even though the
                // identical stack's hotbar icon now does.
                base_color: record.base_color.clone(),
                // And the same again for a held custom head's own skin. Cloned
                // off the record rather than re-resolved from the stack, so the
                // hand and the hotbar icon cannot disagree about which head this
                // is — and so `stack_skin_url`'s fetch is requested once.
                skin: record.skin.clone(),
            });
        // The item id, re-derived rather than cloned: spyglass
        // FOV/vignette needs the bare location further down in this function
        // (`ScreenEffects::scoping`), and the closure otherwise takes ownership
        // of the whole record for the render source's lifetime.
        let held_for_scoping = held.as_ref().map(|item| item.item.clone());
        render.set_main_hand_source(move || held.clone());

        // One immutable camera-scoped state/light gather feeds every
        // state-driven block-entity source below. NBT-driven sources retain
        // their specialised gathers; they cannot use this compact record
        // without cloning arbitrary NBT trees.
        let block_entity_snapshot = self
            .sim
            .block_entity_frame_snapshot(render_camera.position);

        // Block entities — chests. **This install is what makes a
        // chest visible at all**: a 26.2 chest has no block model (its
        // `block/chest.json` declares only a particle texture, zero elements), so
        // without this the terrain mesher leaves a hole where every chest is.
        //
        // Installed every frame, like the swing and the held item above and for
        // the same reason: the closure captures this frame's partial tick and a
        // snapshot of the lid map, so a one-shot install at connect would draw
        // every lid frozen at the fraction of a tick we happened to join on.
        if let Some(snapshot) = &block_entity_snapshot {
            render.set_block_entity_source(
                self.sim
                    .block_entity_source_from_snapshot(std::sync::Arc::clone(snapshot)),
            );
        }

        // Skulls and heads. Same per-frame install as the chests above, though for
        // a weaker reason: no skull carries a partial tick that could go stale,
        // because the one animation they have is driven by a redstone-powered
        // counter this client does not track. It is installed here anyway rather than once at
        // connect so the two block-entity sources cannot drift into different
        // lifetimes — a skull source that survived a disconnect would keep handing
        // out spawns from a dead world's handle.
        if let Some(f) = self.sim.skull_source() {
            render.set_skull_source(f);
        }

        // Copper golem statues. Same per-frame install and the same
        // hole-in-the-world failure mode as chests/skulls:
        // `copper_golem_statue.json` has no block model of its own.
        if let Some(snapshot) = &block_entity_snapshot {
            render.set_copper_golem_statue_source(
                self.sim
                    .copper_golem_statue_source_from_snapshot(std::sync::Arc::clone(snapshot)),
            );
        }

        // Signs. Same per-frame install as chests and skulls above; see
        // `Sim::sign_source` for why it captures no partial tick.
        if let Some(f) = self.sim.sign_source() {
            render.set_sign_source(f);
        }

        // Beacon beams. Same per-frame install as the two above, for the
        // reason `Sim::beacon_source` documents: the closure captures the
        // game tick and the partial tick the beam's scroll and spin animate
        // against, so a stale install freezes it. Unlike bell/conduit below,
        // it carries no cloned tracker alongside those clocks — a beacon's
        // `levels`/`beamSections` are recomputed fresh from current world
        // state every call, the same client-side block-entity ticker vanilla
        // itself runs.
        if let Some(f) = self.sim.beacon_source() {
            render.set_beacon_source(f);
        }

        // `Display`-family entities (`text_display`/`item_display`/
        // `block_display`). Unlike the sources above this is a plain value,
        // not a closure — `Sim::display_draws` already resolved everything
        // this frame (`crate::display_entities::extract_display_draws`,
        // an `Extract`-schedule system, has already run by the time `redraw`
        // reaches here) — so there is nothing left to poll, only to hand
        // over. This was the missing hop: `RenderState::set_display_draws`
        // existed with real consumers below it and nothing above it ever
        // called it, so no `text_display` could ever draw regardless of how
        // correctly everything else in the chain was wired.
        render.set_display_draws(self.sim.display_draws());

        // End portals / end gateways. Same per-frame install as the sources
        // above, for uniformity — neither closure actually needs
        // reinstalling every frame (`Sim::end_portal_source`'s doc: no clock
        // captured at all), but installing alongside every other
        // block-entity source keeps this call site's shape simple to audit.
        if let Some(f) = self.sim.end_portal_source() {
            render.set_end_portal_source(f);
        }
        if let Some(f) = self.sim.end_gateway_source() {
            render.set_end_gateway_source(f);
        }
        render.set_end_portal_game_time(self.sim.game_time_for_shaders());

        // End gateway teleport beams. **Must** be re-installed every frame,
        // unlike the two sources just above: the closure captures the
        // `teleportCooldown` tracker plus the game/partial tick, so a stale
        // install freezes an in-progress countdown.
        if let Some(f) = self.sim.end_gateway_beam_source() {
            render.set_end_gateway_beam_source(f);
        }

        // Bells. Same per-frame install as the three above — the render pass,
        // the GPU-side wiring in `gpu.rs` and the CPU-side gather
        // (`Sim::bell_source`) were all already landed; this call site was
        // the one remaining hop before a live client draws a bell at all
        // (`docs/block-entity-renderers.md`'s Bell section).
        if let Some(snapshot) = &block_entity_snapshot {
            render.set_bell_source(
                self.sim
                    .bell_source_from_snapshot(std::sync::Arc::clone(snapshot)),
            );
        }

        // Shulker boxes. Same per-frame install as the four above, and the same
        // reason this call site matters as much as the geometry: a 26.2 shulker
        // box has **no block model**, so without it the terrain mesher leaves a
        // hole where every box is — the chest failure mode exactly.
        if let Some(snapshot) = &block_entity_snapshot {
            render.set_shulker_source(
                self.sim
                    .shulker_source_from_snapshot(std::sync::Arc::clone(snapshot)),
            );
        }

        // Decorated pots. Same per-frame install and the same failure mode as
        // shulker boxes: a 26.2 decorated pot has no block model, so without
        // this call site every pot is a hole in the terrain mesh, not a
        // missing decoration.
        if let Some(f) = self.sim.decorated_pot_source() {
            render.set_decorated_pot_source(f);
        }

        // Conduits. Same hole-in-the-world failure mode as shulker boxes and
        // decorated pots, plus the per-frame staleness hazard `bell_source`
        // documents: the closure this installs reads `Sim::conduit_ticks`,
        // advanced once per tick in `Sim::step`, so a stale install both
        // leaves a hole *and* freezes whichever conduits were already tracked.
        if let Some(snapshot) = &block_entity_snapshot {
            render.set_conduit_source(
                self.sim
                    .conduit_source_from_snapshot(std::sync::Arc::clone(snapshot)),
            );
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
        if let Some(snapshot) = &block_entity_snapshot {
            render.set_lectern_source(
                self.sim
                    .lectern_source_from_snapshot(std::sync::Arc::clone(snapshot)),
            );
        }

        // Campfire cooking items. Installed beside the others but consumed by a
        // different pass: a campfire's renderer draws item models, not a cuboid
        // rig, so this reaches `prepare_item_geometry` rather than
        // `prepare_block_entities`. An unset source is not a hole — the fire and
        // logs are real block models — it is a campfire that never cooks anything.
        if let Some(f) = self.sim.campfire_source() {
            render.set_campfire_source(f);
        }

        // Brushable-block revealed items. Same odd-one-out shape as
        // campfire's: consumed by `prepare_item_geometry`, not
        // `prepare_block_entities`, since the suspicious sand/gravel a
        // player sees is entirely a real block model. An unset source is a
        // correctly-dusted block with no item floating above it, never a
        // hole.
        if let Some(f) = self.sim.brushable_source() {
            render.set_brushable_source(f);
        }

        // Shelved items. Same odd-one-out shape as campfire/brushable's:
        // consumed by `prepare_item_geometry`, not `prepare_block_entities`,
        // since a shelf's board/back/sides are entirely real block-model
        // geometry. An unset source is a complete, empty shelf, never a hole.
        if let Some(f) = self.sim.shelf_source() {
            render.set_shelf_source(f);
        }

        // Vault display-item clusters. Same odd-one-out shape as campfire's:
        // consumed by `prepare_item_geometry`, not `prepare_block_entities`,
        // since a vault's cage/door/base are real block-model geometry and
        // this closure only supplies the floating reward. **Must** be
        // re-installed every frame like beacon's — the spin advances every
        // tick and a stale closure freezes it rather than merely being
        // absent. An unset source is a vault showing no reward, never a hole.
        if let Some(f) = self.sim.vault_source() {
            render.set_vault_source(f);
        }

        // Enchanting-table books. The per-frame install matters more here than
        // anywhere else in this list: the closure captures a snapshot of the
        // animation fold *and* the partial tick, and none of the book's four
        // animated values is on the wire — so a one-shot install draws every book
        // frozen at the tick the session joined on, with no missing packet to
        // blame it on.
        if let Some(snapshot) = &block_entity_snapshot {
            render.set_enchanting_table_source(
                self.sim
                    .enchanting_table_source_from_snapshot(std::sync::Arc::clone(snapshot)),
            );
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

        // Mob-spawner/trial-spawner display mobs. A fourth destination: not
        // `prepare_block_entities` (this draws through the ordinary mob
        // pipeline, not a `BlockEntityModelSet` rig) and not the moving-block
        // seam either — see `gpu/spawner_mobs.rs`. Same per-frame staleness
        // hazard as bell/conduit: the closure reads `Sim::spawner_spins`,
        // advanced once per tick in `Sim::step`, so a stale install freezes
        // every cage's spin at whatever partial tick it was installed on.
        // Leaving this call site out is not a hole in the world the way the
        // block-entity sources above are — both spawner block types have
        // real cage geometry drawn by the terrain mesher regardless — it is
        // an empty cage where a mob should be spinning.
        if let Some(f) = self.sim.spawner_source() {
            render.set_spawner_source(f);
        }

        // Vanilla's own damage-bob transform — the damage tilt and the death roll, as an
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
        // The connected dimension's own skybox — vanilla's own
        // per-dimension skybox selector, and the half of "the Nether renders under the
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
        // Beside `set_sky_mode` because it is the same shape: a per-frame read
        // of the connected dimension with no edge detector. See
        // `Sim::void_fog` for the two constants this replaced.
        render.set_void_fog(self.sim.void_fog());

        // Filled maps. This is the hop that turns the `SessionMaps`
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
        // ±0.01/tick over ~100 ticks, and a
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
                    // its own environment-attribute layering and the weather
                    // darkening on top of that, so
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
        // The three inputs are the ones vanilla's own client uses, and two of
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
        // Cape URLs ride the exact same request/install pair, one call below —
        // `remote_skins::request`/`drain_ready` are generic over "a texture
        // URL", so a cape is just a second URL per player rather than a
        // second pipeline. See `remote_skins::RemoteSkin::cape`'s doc.
        crate::remote_skins::request_all(
            entity_draws
                .iter()
                .filter_map(|draw| draw.player_skin.as_ref().map(|skin| skin.url.as_str()))
                .chain(
                    entity_draws
                        .iter()
                        .filter_map(|draw| draw.player_skin.as_ref()?.cape.as_deref()),
                ),
        );
        render.install_pending_player_skins(device, queue);
        // Extraction lives in `Sim` because resolving each particle's light
        // needs the world; doing it here would hand out two borrows of `Sim`.
        // Particles are world-space billboards, so both their extraction
        // culling and their camera uniform must use the camera that renders
        // the frame. Passing the first-person eye here left a third-person
        // smoke plume projected as though it were still attached to the eye.
        let particle_frame = self.sim.extract_particles(&render_camera);
        render.prepare_particles(
            device,
            queue,
            &self.sim.particle_instances(),
            &render_camera,
        );
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
                    // column — see `ShellWeatherProbe`'s doc for the one
                    // divergence that buys. The *height* and *biome* halves are
                    // real per-column lookups and are what used to cost 441 × 3
                    // world locks a frame; `ShellWeatherProbe::memo` now takes
                    // one lock per chunk column instead, which is why the probe
                    // below **must** stay per-frame. `sky_darken()` is the
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
                                policy.get(),
                            )
                        });
                    let probe = ShellWeatherProbe {
                        light: lodestone_render::light::light_term(
                            packed.unwrap_or(lodestone_render::ENTITY_FULLBRIGHT),
                            render.sky_darken(),
                        ),
                        // The per-column `column_top` heightmap clamp is what
                        // now keeps rain out of a room or a cave (see
                        // `ShellWeatherProbe`'s doc) — there is deliberately no
                        // camera-level "can I see the sky" gate here any more,
                        // matching vanilla's own `WeatherEffectRenderer`.
                        handle: self.sim.net().and_then(|n| n.shared_handle().get().cloned()),
                        biome_climates: self.sim.net().map(crate::net::NetClient::shared_biome_climates),
                        // Fresh every frame, by construction — see the field doc.
                        memo: Default::default(),
                    };
                    // The radius is the live `options.weatherRadius`, polled
                    // here rather than pushed on change like every other video
                    // option this function reads — the call already took a
                    // radius and was handed
                    // `lodestone_render::DEFAULT_WEATHER_RADIUS`.
                    weather_columns_for_frame(
                        w,
                        &camera,
                        tick,
                        self.sim.interp_alpha(),
                        self.nav.options().weather_radius,
                        &probe,
                    )
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
        // keys on in its own camera-overlay extraction. That is a
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
        // The freeze overlay's per-frame input. `PlayerState::
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
            // Vanilla's own scoping check is "using an item" and "that item is a
            // spyglass". Both halves: `Sim::
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
        // Route the progressive-mining crack overlay(s): the local
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
        // Hand the world's three flat-colour text passes — nametags, sign text
        // and `text_display` — this frame's **raw** (non-sRGB) view of the same
        // swapchain image. Vanilla composites text and its background plate on
        // gamma bytes and this shell's native swapchain is viewed as sRGB, so
        // without this the hardware blends them in linear light and a plate
        // reads too weak against a bright backdrop. `RenderState` derives both
        // the format and the view here, so the two cannot be decided apart —
        // the same structural fix `HudRenderer::flat_colour_view` records for
        // the HUD's own flat-colour stream. Per frame, before the render call:
        // a swapchain image is presented at the end of the frame and its view
        // must not survive into the next one.
        render.set_world_text_view(device, &frame);
        self.frame_profile.mark(FramePhase::Prepare, Instant::now());
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
        // Record **and** submit, fused — see `app::frame_profile`'s module
        // doc for why this shell has no seam between them to time separately.
        self.frame_profile.mark(FramePhase::WorldEncodeSubmit, Instant::now());
        // Counts for the `hud_ui_encode_submit` sub-phase breakdown
        // (`app::frame_profile::HudSubphaseCounts`), recorded together at the
        // end of the phase. A count next to each duration, per this repo's
        // evidence standard: "2 ms of chat gather" means one thing at 10 lines
        // and another at 100, and the duration alone cannot separate them.
        let mut menu_overlays_drawn = 0usize;
        let mut debug_lines_built = 0usize;

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
        // Four counters that reached `RenderStats` and stopped there — the
        // draw-call total and two of the three cull buckets (the occlusion one
        // was already copied below), plus the terrain camera bind-group count.
        // Every copy between the two structs happens here, so a field the
        // overlay cannot reach is always a missing line in this block.
        self.sim.stats.draw_calls = stats.draw_calls;
        self.sim.stats.sections_culled_distance = stats.sections_culled_distance;
        self.sim.stats.sections_culled_frustum = stats.sections_culled_frustum;
        self.sim.stats.terrain_camera_bind_group_switches =
            stats.terrain_camera_bind_group_switches;
        self.sim.stats.vram_bytes = stats.vram_bytes;
        self.sim.stats.vram_reserved_bytes = stats.vram_reserved_bytes;
        self.sim.stats.entities_drawn = stats.entities_drawn;
        self.sim.stats.particles_alive = particle_frame.alive;
        self.sim.stats.particles_drawn = stats.particles_drawn;
        self.sim.stats.particles_unresolved = particle_frame.unresolved;
        // Columns the weather pass uploaded this frame — see
        // `RenderState::weather_columns`'s own doc for why this is exposed
        // alongside `sky_darken`: "the rain is not drawing" has the same two
        // indistinguishable-on-screen causes the sky-darken line exists for.
        self.sim.stats.weather_columns = render.weather_columns();
        self.sim.stats.weather_rain_columns = render.weather_rain_columns();
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
        // Frame-profiling lines for the F3 overlay — see
        // `docs/frame-profiling.md`. Built only while the overlay is actually
        // showing: `summary()` sorts a ring buffer per phase, which is wasted
        // work for a screen nobody is looking at.
        if self.show_debug {
            let mut lines: Vec<String> = self.frame_profile.summary().map(|s| s.line()).collect();
            debug_lines_built = lines.len();
            // `render` (not `self.render`): the destructure above already
            // holds `&mut RenderState` for the rest of this function, and a
            // fresh `self.render.as_ref()` here would conflict with it.
            if render.gpu_timing_available() {
                for (name, ms) in render.gpu_timing_report() {
                    lines.push(match ms {
                        Some(ms) => format!("gpu {name}: {ms:.2} ms"),
                        None => format!("gpu {name}: <no reading yet>"),
                    });
                }
                // The CPU-bound-or-GPU-bound line — built from the two
                // **real** passes this timer can see, and labelled as the
                // lower bound it is.
                //
                // This used to add the `world_total` and `hud_total`
                // bracketing spans instead, on the grounds that between them
                // they cover every pass the shell submits. They do, in
                // submission terms, and the number was still not usable:
                // `benches/frame_profile.rs` measures those spans reading
                // **below the block pass they enclose** at two of four camera
                // waypoints on a quiet machine (0.059 ms of span against a
                // 0.234 ms pass at one of them). A span shorter than its own
                // contents is a different quantity, not a noisy one, so
                // showing its sum here as "gpu frame total" put a fabricated
                // figure on the overlay. The bracket segments are still
                // listed individually by the loop above, where they read as
                // what they are; they are just no longer summed into a claim.
                //
                // A sum of per-pass GPU times is **not** a frame's GPU time
                // either — passes overlap on a tile-based deferred GPU — so
                // this is stated as a floor rather than a total. It is enough
                // for the question it exists to answer: a floor already above
                // the frame budget settles GPU-bound outright.
                //
                // Denominator is the pacer's counted frame rate, not the
                // `frame_ms` figure a few lines above: that one is measured
                // from `frame_start` to just after the world render, so it
                // stops before the HUD and before `present` and would
                // overstate the GPU's share of the frame.
                let report = render.gpu_timing_report();
                let span = |name: &str| {
                    report.iter().find(|(n, _)| *n == name).and_then(|(_, ms)| *ms)
                };
                if let (Some(block), Some(hand)) = (span("world"), span("first_person")) {
                    let floor_ms = block + hand;
                    let fps = self.pacer.fps() as f32;
                    // A share is only meaningful once the pacer has actually
                    // counted a presented frame; before that, report the
                    // figure alone rather than dividing by zero into a
                    // fabricated percentage.
                    if fps > 0.0 {
                        let budget_ms = 1000.0 / fps;
                        lines.push(format!(
                            "gpu measured passes: {floor_ms:.2} ms >= {:.0}% of the {budget_ms:.2} ms \
                             presented interval (block+hand only, a floor; readback lags a few frames)",
                            100.0 * floor_ms / budget_ms,
                        ));
                    } else {
                        lines.push(format!(
                            "gpu measured passes: {floor_ms:.2} ms (block+hand only, a floor; no \
                             presented-frame interval yet)"
                        ));
                    }
                }
                let stalled = render.gpu_timing_stalled_frames();
                if stalled > 0 {
                    lines.push(format!("gpu timer stalled_frames: {stalled}"));
                }
            } else {
                lines.push("gpu timing: unavailable (device lacks Features::TIMESTAMP_QUERY)".to_string());
            }
            self.sim.stats.frame_profile = lines;

            // The pie chart's own render-ready snapshot — see
            // `docs/frame-profiling.md`'s "Pie chart" section. Built from the
            // exact same `summary()`/GPU report as the text lines just above,
            // so the two can never disagree about a frame's numbers, and
            // gated on `show_profiler_chart` (Shift+F3) in addition to
            // `show_debug`: the F3 text overlay can be up with the chart off.
            self.sim.stats.profiler_chart = if self.show_profiler_chart {
                let slices: Vec<crate::hud::ProfilerChartSlice> = self
                    .frame_profile
                    .summary()
                    .map(|s| crate::hud::ProfilerChartSlice {
                        name: s.phase.name(),
                        mean_ms: s.mean_ms,
                        p95_ms: s.p95_ms,
                        p99_ms: s.p99_ms,
                        samples: s.samples,
                        window: s.window,
                        skipped: s.skipped,
                    })
                    .collect();
                let (gpu, gpu_unavailable) = if render.gpu_timing_available() {
                    (render.gpu_timing_report(), false)
                } else {
                    (Vec::new(), true)
                };
                Some(crate::hud::ProfilerChart {
                    slices,
                    selected: self.profiler_chart_selected,
                    gpu,
                    gpu_unavailable,
                    gpu_stalled_frames: render.gpu_timing_stalled_frames(),
                })
            } else {
                None
            };
        } else {
            self.sim.stats.frame_profile.clear();
            self.sim.stats.profiler_chart = None;
        }
        // `hud_ui_encode_submit`'s first sub-phase closes here: this
        // instrument's own cost, and the only one of the six that is zero
        // whenever F3 is closed. See `HudSubphase::DebugGather`.
        self.frame_profile.mark_hud(HudSubphase::DebugGather, Instant::now());

        // The baked 3-D item geometry, shared by the container screen below and the
        // HUD hotbar further down. It borrows `self.sim`, so it cannot be hoisted
        // above the `self.sim.stats` writes just above — but it must exist before
        // the container overlay, which is the pass that was missing it.
        // Sound-subtitle captions. Gated on the persisted
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
        // One binding for the line count, used by BOTH reads below. The two
        // must ask for the same `n` or the parallel trust list stops being
        // index-aligned with the spans — a misalignment that would badge the
        // wrong message and could not be caught by anything downstream, since
        // both lists are the right length and the wrong pairing is invisible.
        let chat_line_count = if chat_open { 100 } else { 10 };
        let chat_spans_owned: Vec<(Vec<lodestone_model::text::TextSpan>, f32)> =
            self.sim.recent_chat_spans(chat_line_count);
        // The message-trust badge's source, walked over the same feed with the
        // same `n` (see above) and windowed by the same slice below.
        let chat_trust_owned: Vec<Option<lodestone_game::chat::MessageTrust>> =
            self.sim.recent_chat_trust(chat_line_count);
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
        // Reproduces vanilla's own chat-message-queue "a new
        // message while scrolled does not jump the view" behaviour, and
        // its own chat-screen close handling resetting the scroll when closed — see
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
        // Windowed by the *same* range as the spans above. Clamped rather than
        // indexed blind: the two lists come from two reads of the feed, and a
        // message arriving between them would make the trust list one longer,
        // which would panic here on the closed path where the window is the
        // whole list. An empty slice is the honest fallback — the frame's own
        // doc requires a consumer to draw nothing for a row it does not cover,
        // never a fabricated verdict.
        let chat_trust_lines: &[Option<lodestone_game::chat::MessageTrust>] =
            if chat_win_end <= chat_trust_owned.len() {
                &chat_trust_owned[chat_win_start..chat_win_end]
            } else {
                &[]
            };
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
        // The locator bar's dots, from the exact eye camera the
        // 3-D pass above renders with — vanilla resolves the locator bar
        // against `gameRenderer.mainCamera()`, not any third-person offset.
        let locator_dots = self.sim.locator_dots(camera.position, camera.yaw);
        // Two different questions, and they used to share one boolean named
        // `crosshair` — which is why the hotbar vanished behind the pause menu
        // and the inventory. The crosshair is the aiming reticle and
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
        // The hover half of chat interactivity (`docs/chat.md`'s
        // "Interactivity": the hit-test already finds a `hover_event`, but
        // nothing drew a tooltip from it). Gated on `chat_open` the same way
        // `dispatch_chat_click_under_cursor`'s own call site gates clicks —
        // vanilla has no interactive text at all on the passive, fading
        // closed-chat overlay, only inside the open `ChatScreen`.
        //
        // Not routed through `Self::chat_interaction`: that method takes
        // `&self` as a whole and `self.render.as_mut()` is already a live
        // exclusive borrow for the rest of this function (`render`, bound a
        // few lines above), so a whole-`self` call here does not borrow-check.
        // `hud` and `chat_display_opts` are the exact two inputs that method
        // would have re-derived from `self.hud`/`self.nav` anyway, so this
        // calls the same `HudRenderer::chat_interaction_at` directly off the
        // already-borrowed local instead of duplicating its logic.
        //
        // `to_spans`, not `to_legacy_string` — tooltip-title fix
        // gave `draw_chat_hover_tooltip` a span-carrying `wrap_spans`/
        // `text_spans` pair to draw with (the same primitives
        // `container::builder::Builder::shadowed_label_spans` already
        // reaches for), so a hex-coloured `show_text` hover no longer has to
        // flatten through `to_legacy_string`'s sixteen-code ceiling the way
        // `SuggestionLayer::Tooltip`'s plain-string popup still does.
        //
        // `hud::hover_tooltip_spans`, not a `resolve_text(..).to_spans()` on a
        // single payload field: an item hover's body is the same line-gather
        // an inventory slot's tooltip uses and an entity hover's is three
        // composed lines, so all three actions come from that one function.
        // Without this the typed payloads reach the hit-test and paint
        // nothing — the `show_item` half of chat hover would still be an
        // island. `advanced_item_tooltips` is the player's own option, read
        // from the same place the container tooltip reads it, so the two
        // surfaces cannot disagree about whether the id and durability lines
        // show.
        let chat_hover_tooltip_spans: Option<Vec<lodestone_model::TextSpan>> = chat_open
            .then(|| {
                let entries = self.sim.recent_chat_interactive(100);
                hud.chat_interaction_at(
                    w,
                    h,
                    self.nav.gui_scale(),
                    chat_display_opts,
                    chat_open,
                    &entries,
                    self.chat_input.scroll().scrolled(),
                    self.cursor,
                )
            })
            .flatten()
            .and_then(|hit| hit.hover)
            .map(|hover| {
                crate::hud::hover_tooltip_spans(
                    &hover,
                    self.sim.translator().as_ref(),
                    self.nav.options().advanced_item_tooltips,
                )
            });

        // The top-right status-effect overlay's own list, resolved before the
        // frame is built because the draw borrows it as a slice.
        //
        // `screen_shows_active_effects` is `Hud.extractEffects`' own
        // `screen() == null || !screen().showsActiveEffects()` guard: with a
        // screen up that draws the effects itself, the overlay must not paint,
        // or the top-right icons and the inventory column show the same effects
        // at once. It is resolved here rather than at the old overlay's call
        // site further down because the HUD now draws this widget, and the HUD
        // renders before that point.
        let hud_effect_icons = crate::effects::hud_icons(&self.sim.active_effects());
        let screen_shows_effects = screen_shows_active_effects(
            &self.sim,
            self.ui.is_container_open(),
            creative_open,
            self.recipe_panel.open,
            self.nav.gui_scale(),
            w,
            h,
        );

        let mut hud_frame = HudFrame::new(&self.sim.stats);
        hud_frame.effects =
            (!screen_shows_effects && world_hud).then_some(hud_effect_icons.as_slice());
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
        hud_frame.chat_trust = chat_trust_lines;
        hud_frame.sound_subtitles = &sound_subtitles;
        // Persisted wrap results (issue #527 (a)): without this the whole
        // visible log is re-wrapped, quadratically, every frame. Retained for
        // the (now-dormant) legacy `chat` path; `chat_spans` above has no
        // persisted cache of its own yet.
        hud_frame.chat_wrap = Some(&self.chat_wrap);
        hud_frame.chat_input = chat_open.then(|| self.chat_input.as_str());
        hud_frame.chat_selection = chat_open.then(|| self.chat_input.selection()).flatten();
        // Without this line the caret indicator is an island in the other
        // direction: `ChatInput` moves its insertion point on Left/Right/Home/
        // End and every one of its own tests passes, while the drawn indicator
        // is placed from the width of the *whole* line and so never moves. The
        // draw also picks the underscore-vs-bar shape from this, so an unset
        // field would keep both halves of that bug.
        hud_frame.chat_cursor = chat_open.then(|| self.chat_input.cursor_position());
        // Vanilla blinks the text cursor on a 300 ms half-period: visible
        // whenever the wall-clock millisecond count divided by 300 is even. The
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
        // Without this line the hit-test finds the hover exactly as well as it
        // did before this fix, and zero pixels change -- see this local's own
        // doc for the `chat_open` gate.
        hud_frame.chat_hover_tooltip =
            chat_hover_tooltip_spans.as_deref().map(|spans| crate::hud::ChatHoverTooltip {
                spans,
                cursor: crate::hud::HudRenderer::canvas_cursor(w, h, self.nav.gui_scale(), self.cursor),
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
        // local player's folded attribute snapshot — matching vanilla's own
        // armor-value accessor — so equipment reaches the bar the way it
        // reaches any other attribute, as a server `update_attributes` push, and
        // there is no per-item table anywhere in the chain. Without this line the
        // whole row is an island: `HudFrame::armour` defaults to `None`, both draw
        // paths and both gates are correct, and zero pixels change.
        hud_frame.armour = self.sim.armour_value();
        hud_frame.food = food;
        // Without this the hunger wobble is computed correctly and
        // never fires: vanilla shakes the row only while saturation is
        // exhausted, so an unfed `saturation` reads as "always satisfied".
        hud_frame.saturation = self.sim.saturation();
        hud_frame.air = air;
        hud_frame.hotbar = world_hud.then(|| self.sim.selected_slot());
        hud_frame.hotbar_items = world_hud.then_some(hotbar_records.as_slice());
        hud_frame.xp = self.sim.xp();
        hud_frame.locator = &locator_dots;
        hud_frame.title = self.sim.title_overlay();
        hud_frame.action_bar = self.sim.action_bar_overlay();
        hud_frame.held_item = self.sim.held_item_overlay();
        hud_frame.held_item_spans = self.sim.held_item_overlay_spans();
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
        hud_frame.map_debug = map_debug_when_visible(self.show_debug, || self.sim.map_debug());
        // The recipe-unlock toast. `None` on every real session
        // today, because the queue's only possible producer is the
        // `recipe_book_add` decode that does not exist yet — see the field's own
        // doc. Wired here anyway so it lights up the moment that lands.
        hud_frame.recipe_toast = recipe_toast_view(&self.recipe_toasts, recipe_toast_now_ms());
        // The advancement-completion toast, resolved above the
        // field-borrow split like every other `Sim`-derived view.
        hud_frame.advancement_toast = advancement_toast;
        // Always `Some`: `Sim::attack_strength_scale` is defined on both the
        // demo and live worlds (the ticker and the `attack_speed` attribute
        // default both exist before any server connection), unlike
        // `health`/`food`/`xp` which stay `None` until a server reports them.
        // `hud.rs`'s draw site is what actually gates this on
        // `frame.crosshair` — see that field's doc for why the crosshair
        // hides behind an open screen but the hotbar does not.
        hud_frame.attack_cooldown = Some(self.sim.attack_strength_scale());
        // Which of vanilla's three placements draws that value — `options.
        // attackIndicator`. Copied per frame off `MenuNav::options`, the same
        // poll shape the eight chat options use, so the two `hud.rs` draw sites
        // pick it up the frame after the settings row is cycled.
        hud_frame.attack_indicator = self.nav.options().attack_indicator;
        // The 3-D block-item icons need the baked model set (for geometry) and a
        // depth attachment (so the near faces of the mini-block win over the far
        // ones). Both are `None` on the demo path, which degrades to flat sprites.
        // Everything from the debug block down to here is pure CPU state
        // gather — no encoder exists yet. See `HudSubphase::FrameGather`.
        self.frame_profile.mark_hud(HudSubphase::FrameGather, Instant::now());
        hud.render_with_item_models(
            device,
            queue,
            frame.view(),
            &hud_raw_view,
            Some(render.depth_view()),
            &hud_frame,
            item_models,
            self.nav.gui_scale(),
            w,
            h,
        );
        self.frame_profile.mark_hud(HudSubphase::HudDraw, Instant::now());
        // The container overlay draws **after** the HUD (issue #51/#61): vanilla's
        // own HUD draw pass draws the HUD unconditionally behind any world-following
        // screen (`hud_follows_world` above), and the screen then paints its own
        // translucent background over it (vanilla's own in-game-UI check) — the dim is draw order, not a
        // per-element alpha. Drawing this block before the HUD (as it used to)
        // meant the HUD painted back over the container's dim every frame and the
        // hotbar never actually looked dimmed behind an open chest. Both this pass
        // and the HUD's own model sub-pass independently clear the shared depth
        // buffer immediately before drawing their own GUI items, so swapping the
        // two relative to each other is safe — see `docs/container-screen.md`.
        // The creative-inventory screen **replaces** the player's
        // inventory screen rather than overlaying it, exactly as vanilla's
        // own open-inventory logic picks one screen or the other. So it is
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
        // The open merchant's trade list (UI half), read once
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
            // the raw key on screen. See `container::menu_title`.
            //
            // The merchant screen composes a level badge into the title
            // itself (vanilla's own merchant-screen label extraction) rather than merely
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
            // **"Crafting"**, not "Inventory". `InventoryScreen`
            // passes `translatable("container.crafting")` as its title
            // — it names the 2x2 grid — and the
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
            // The player-inventory title through the same language table. A local
            // constant here is not the #52 defect class repeating: vanilla reads
            // it from its own inventory display-name accessor, itself the client-side
            // constant `translatable("container.inventory")`
            //, so there is no server component to resolve.
            let inventory_label =
                crate::container::player_inventory_label(self.sim.translator().as_ref());
            // `merchant.trades` — "Trades", the merchant screen's second label
            // (UI half). Computed unconditionally like
            // `inventory_label` above; `ContainerFrame`'s own draw path is
            // what gates it on the screen actually being a merchant.
            let trades_label = crate::container::merchant_trades_label(self.sim.translator().as_ref());
            // The anvil rename box's current value.
            // `AnvilRenameState::sync` is vanilla's own slot-changed handling: it resets
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
                // `plain_hover_name`, not `styled_hover_name`: this seeds an
                // *editable* text field and is later compared for equality
                // against the item's own name to decide whether a rename is
                // real (`AnvilRenameState::resolve_rename`) — vanilla's own
                // anvil-screen name-change handling seeds and compares against
                // the item's plain-text hover name, the plain-text
                // accessor with no `§` codes at all, not the legacy-coded
                // string `styled_hover_name` would give an edit box nowhere
                // to render.
                let item = menu.slot_item(0).map(|stack| {
                    (
                        stack.custom_name().is_some(),
                        lodestone_game::item::plain_hover_name(stack, self.sim.translator().as_ref()),
                    )
                });
                self.anvil_rename.sync(
                    item.as_ref()
                        .map(|(has_custom_name, name)| (*has_custom_name, name.as_str())),
                );
                Some(self.anvil_rename.value.clone())
            });
            // The beacon screen's pending primary/secondary power selection
            // (`SetBeaconEffects` remainder).
            // `BeaconSelection::sync` is vanilla's own
            // `ContainerListener::dataChanged` — it re-derives the local
            // selection from `container_data` properties `1`/`2` exactly
            // when that pair changes (menu open, and every successful
            // confirm), and otherwise leaves a pending local click alone.
            // `None` off any non-beacon screen, matching `anvil_name` above;
            // the state itself is not cleared on leaving the screen, only
            // its value stops being read (see `WindowApp::beacon_selection`'s
            // own doc).
            if container_menu.is_some_and(|menu| {
                menu.special_layout() == Some(lodestone_game::menu::SpecialLayout::Beacon)
            }) && let Some(open) = open_menu.as_ref()
            {
                let primary_id = open.data.iter().find(|(p, _)| *p == 1).map_or(0, |(_, v)| *v);
                let secondary_id = open.data.iter().find(|(p, _)| *p == 2).map_or(0, |(_, v)| *v);
                self.beacon_selection.sync(primary_id, secondary_id);
            }
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
            // The status-effect column beside the panel (vanilla's
            // effects-in-inventory display).
            // Only the local player's own inventory shows it — vanilla builds
            // this column in its own inventory screen and
            // creative-inventory screen only, and every other screen
            // declines to show active effects; a chest or furnace
            // menu resolves a different `MenuKind` and gets an empty slice.
            //
            // Resolved **here**, where the language table is, rather than at
            // the draw site: `Sim::translator` is what turns
            // `effect.minecraft.speed` into "Speed", and the widget drew the
            // raw registry path for as long as nothing handed it one.
            let effect_rows = if container_menu
                .is_some_and(|menu| menu.kind() == lodestone_game::menu::MenuKind::Player)
            {
                crate::effects::inventory_rows(
                    &self.sim.active_effects(),
                    self.sim.translator().as_ref(),
                )
            } else {
                Vec::new()
            };
            // The server's own ghost preview for whichever window is on
            // screen right now: resolved to a drawable stack here, where
            // `lodestone_data`'s item table lives, and filtered to this exact
            // window — the same "no window id of its own" contract
            // `bundle_selection` below already established, because
            // `ContainerFrame` itself has nothing to compare `window_id`
            // against. `known_recipes()` is a cheap clone of the session's
            // own recipe-sync store (`Sim::known_recipes`'s own doc), so
            // reading it unconditionally here costs nothing when no ghost is
            // showing.
            let ghost_window_id = open_menu.as_ref().map_or(0, |open| open.window_id);
            let recipe_ghost_stack = self
                .sim
                .known_recipes()
                .ghost()
                .filter(|ghost| ghost.window_id == ghost_window_id)
                .and_then(recipe_panel::ghost_result_stack);
            // The live drag preview (issue #378 part 2). `drag_paint` is the
            // *same* paint set `MenuInput::release` will turn into the
            // QUICK_CRAFT sequence, and the counts drawn from it come out of
            // `Menu::quick_craft_plan`, which is what distributes them — so the
            // preview cannot show a split the release will not produce.
            let container_frame = ContainerFrame::new(container_menu, &container_title)
                .with_inventory_label(&inventory_label)
                .with_effects(&effect_rows)
                // The trade list and which row is selected (UI
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
                // — and the flag it passes is vanilla's own persisted
                // advanced-item-tooltips option, toggled by F3+H.
                .with_tooltips(self.nav.advanced_item_tooltips())
                // An open recipe book moves the panel right (vanilla's
                // own screen-position update). `container_input`'s hit-test passes the
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
                .with_recipe_ghost(recipe_ghost_stack.as_ref())
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
                // The local player's own uuid, so the
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
                .with_anvil_name(anvil_name.as_deref())
                .with_beacon_selection(
                    self.beacon_selection.primary.as_ref(),
                    self.beacon_selection.secondary.as_ref(),
                )
                // The bundle scroll-selection highlight (issue #616's
                // `BUNDLE_ITEM_SELECTED`/#613's `SelectBundleItem` remainder,
                // the tooltip's own consumer half — see
                // `crate::container::bundle`'s module doc). Filtered to the
                // *currently open* window here rather than inside
                // `ContainerFrame` itself, which carries no window id of its
                // own: a selection tracked against a bundle in a screen that
                // has since closed (or a different one that has since
                // opened) must not paint a highlight in the new screen just
                // because a slot index happens to coincide.
                .with_bundle_selection(self.bundle_selection.filter(|selection| {
                    selection.window_id == open_menu.as_ref().map_or(0, |open| open.window_id)
                }));
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
                    // The recipe-book panel, **over** the container
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
                        // (vanilla's own recipe-book-page tooltip extraction). The same cursor and
                        // the same persisted advanced-item-tooltips flag the
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
                            &hud_raw_view,
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
        // The creative/container/recipe-book renderers, if one was open. On an
        // ordinary playing frame this closes immediately — see
        // `HudSubphase::ContainerDraw`.
        self.frame_profile.mark_hud(HudSubphase::ContainerDraw, Instant::now());

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
            menu_overlays_drawn += 1;
        }

        // The Advancements screen, drawn over the still-rendering
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
            menu_overlays_drawn += 1;
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

        // The death screen follows exactly the same overlay
        // shape as pause, for the same reason: a live server holds a dead
        // player with no chunk stream until it respawns, so this must draw
        // over the still-rendering, still-ticking world rather than replace
        // it — see `Screen::Death`'s doc comment.
        if self.ui.is_death()
            && let Some(menu) = self.menu.as_mut()
        {
            let death_frame = crate::menu::render::death_frame(&self.nav, self.sim.death_message());
            menu.render_overlay(device, queue, frame.view(), &death_frame, w, h);
            menu_overlays_drawn += 1;
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
            menu_overlays_drawn += 1;
        }

        // Issue #449's terrain half. `Screen::Connecting` covers the
        // handshake/configuration phase as a full frame (see `frame_for`); this
        // block covers the moments after login while the player's own chunk is
        // still streaming in. Drawn as an overlay over the still-rendering
        // world rather than replacing it, for the same reason Paused/Death are
        // overlays: chunks must keep meshing and uploading behind the text —
        // the very thing a full-frame `owns_frame` screen would stop. It is
        // nonetheless opaque: `loading_frame`'s backdrop is `Panorama`, whose
        // no-panorama fallback is a flat opaque fill, so nothing of the world
        // shows through while this is up.
        //
        // The predicate is `Sim::world_wait`, which is **two** rules and not
        // one. The terrain half is vanilla's own `LevelLoadTracker`
        // readiness rule (the chunk column under the player is loaded); the
        // asset half holds the screen while a server-pushed resource pack is
        // still downloading or has not yet been applied to the block atlas.
        //
        // The second half is why this block sits *below*
        // `Sim::reload_resource_pack_atlas` in this same function rather than
        // above it, and the ordering is the whole point. On the frame a pack
        // lands, the reload up there rebuilds the atlas and re-meshes every
        // loaded column — the visible second-long hitch — and only then does
        // this predicate go quiet. So the last frame the player actually saw
        // before the hitch is this screen, and the first world frame presented
        // after it already wears the new pack. Moving this above the reload
        // would present one frame of the old atlas and reinstate the flash.
        //
        // The bar and the count line only appear when the terrain half is what
        // is holding *and* there is a real denominator to divide by (the
        // session declared a view radius); with neither, this stays the bare
        // phase label it always was, because a progress bar wired to nothing is
        // worse than no bar.
        if self.ui.is_playing()
            && let Some(wait) = self.sim.world_wait()
            && let Some(menu) = self.menu.as_mut()
        {
            let label = wait.label();
            // `has_terrain_progress` is false for the pack wait, which draws the
            // bare label: nothing here observes a download's byte count, so a bar
            // would be the synthesised progress `menu::loading`'s own doc forbids.
            let loading_frame = match self
                .sim
                .terrain_progress()
                .filter(|_| wait.has_terrain_progress())
            {
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
            menu_overlays_drawn += 1;
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
            menu_overlays_drawn += 1;
        }

        // Statistics and Server Links are the same shape as the block above, and
        // the paragraph above applies to them word for word: `frame_for` returns
        // `None` for both, so without these two blocks each screen draws
        // *nothing*. Statistics differs from Settings in one way worth knowing —
        // it has no out-of-world route at all (it is only ever opened from the
        // pause menu), so its `frame_for` arm is unconditionally `None` rather
        // than conditional on being in a world.
        //
        // Neither builds its frame raw here, for the reason the Settings comment
        // records: the overlay helper stamps the shared canvas facts, and one
        // expression feeds both this draw and `on_screen_frame`'s hit test.
        if let Some(stats_frame) = crate::menu::nav::stats_overlay_frame(&self.ui, &self.nav)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.render_overlay(device, queue, frame.view(), &stats_frame, w, h);
            menu_overlays_drawn += 1;
        }

        if let Some(links_frame) =
            crate::menu::nav::server_links_overlay_frame(&self.ui, &self.nav)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.render_overlay(device, queue, frame.view(), &links_frame, w, h);
            menu_overlays_drawn += 1;
        }

        // Social Interactions is the third screen of this exact shape, and it
        // had the same defect Statistics did: `frame_for` returns `None`, so
        // without this block it draws *nothing*.
        if let Some(social_frame) = crate::menu::nav::social_overlay_frame(&self.ui, &self.nav)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.render_overlay(device, queue, frame.view(), &social_frame, w, h);
            menu_overlays_drawn += 1;
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
            menu_overlays_drawn += 1;
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
            menu_overlays_drawn += 1;
        }

        // The book-editing screen (`EditBook` remainder) — the
        // seventh overlay, same shape as the sign-editing block immediately
        // above and for the same reason: `menu::render::frame_for` has no arm
        // for it (it is an overlay, not a full screen), so without a draw
        // call here the screen would open, hit-test correctly
        // (`nav::on_screen_frame` already calls `book_edit_overlay_frame`),
        // and render nothing.
        if let Some(book_edit_frame) = crate::menu::nav::book_edit_overlay_frame(&self.ui, &self.nav)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.render_overlay(device, queue, frame.view(), &book_edit_frame, w, h);
            menu_overlays_drawn += 1;
        }

        // The Spectator Menu (`TeleportToEntity` remainder) —
        // the eighth overlay, same shape as the book-editing block
        // immediately above and for the same reason: `menu::render::frame_for`
        // has no arm for it (it is an overlay, not a full screen), so without
        // a draw call here the screen would open, hit-test correctly
        // (`nav::on_screen_frame` already calls
        // `spectator_menu_overlay_frame`), and render nothing.
        if let Some(spectator_menu_frame) =
            crate::menu::nav::spectator_menu_overlay_frame(&self.ui, &self.nav)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.render_overlay(device, queue, frame.view(), &spectator_menu_frame, w, h);
            menu_overlays_drawn += 1;
        }

        // `key.screenshot`, and **this position is the whole
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

        self.frame_profile.mark_hud(HudSubphase::MenuOverlays, Instant::now());

        // Close this frame's GPU timing — every encoder of the frame has now
        // been submitted, which is this call's whole precondition (see
        // `RenderState::gpu_timing_end_frame`). It costs one small extra
        // command-buffer submission, and that cost is measured rather than
        // hidden: it is the `hud.gpu_timing_end` sub-phase immediately below.
        render.gpu_timing_end_frame(device, queue);
        self.frame_profile.mark_hud(HudSubphase::GpuTimingEnd, Instant::now());
        self.frame_profile.record_hud_counts(crate::app::frame_profile::HudSubphaseCounts {
            chat_lines: chat_spans_lines.len(),
            debug_lines: debug_lines_built,
            menu_overlays_drawn,
        });

        // HUD, status effects and the container/menu overlay all drew above
        // through their own encoder/submit pairs. The phase is no longer one
        // opaque bucket: `HudSubphase` splits it six ways, and the marks for
        // those sit at the seams above.
        self.frame_profile.mark(FramePhase::HudUiEncodeSubmit, Instant::now());

        if let Some(window) = &self.window {
            window.pre_present_notify();
        }
        frame.present(queue);
        self.frame_profile.mark(FramePhase::Present, Instant::now());

        // The frame-profile tracing line — see `docs/frame-profiling.md` for
        // how to read it. It is gated with `enabled!` so a session with
        // no interest in this target pays no `summary()`/percentile-sort cost
        // for a line nothing will read.
        if self.frame_profile.report_due(Instant::now(), Duration::from_secs(1))
            && tracing::enabled!(target: "frame_profile", tracing::Level::DEBUG)
        {
            // `render` (not `self.render`): see the identical note above,
            // near `self.sim.stats.frame_profile`'s own assignment — the
            // destructure near the top of this function holds `&mut
            // RenderState` for the whole call.
            let gpu_line = if render.gpu_timing_available() {
                let stalled = render.gpu_timing_stalled_frames();
                render
                    .gpu_timing_report()
                    .into_iter()
                    .map(|(name, ms)| match ms {
                        Some(ms) => format!("{name}={ms:.2}ms"),
                        None => format!("{name}=<no reading yet>"),
                    })
                    .chain((stalled > 0).then(|| format!("stalled_frames={stalled}")))
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                "unavailable (device lacks Features::TIMESTAMP_QUERY)".to_string()
            };
            tracing::debug!(
                target: "frame_profile",
                "cpu: {} | gpu: {gpu_line}",
                self.frame_profile
                    .summary()
                    .map(|s| s.line())
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
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
        let ordinary = crate::app::pacing::effective_target_fps(
            self.nav.options().framerate_limit,
            self.nav.options().inactivity_fps_limit,
            self.pacer.idle_secs(now),
        );
        benchmark_target_fps(&self.config, ordinary)
    }

    /// Vanilla's own persisted vsync option (default `true`).
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
        let ordinary = if self.nav.options().enable_vsync {
            target.default_present_mode()
        } else {
            wgpu::PresentMode::AutoNoVsync
        };
        let mode = benchmark_present_mode(&self.config, ordinary);
        target.set_present_mode(gpu.device(), mode);
    }
}

/// Whether `redraw`'s per-frame housekeeping should send one
/// `ClientAction::PingRequest` right now (`PingRequest`
/// remainder) — pulled out as a pure function of its three inputs so the
/// throttle can be checked with no window, no GPU and no session at all; see
/// [`WindowApp::last_ping_request`]'s own doc for why F3 is the gate and one
/// second is the interval.
#[must_use]
pub(super) fn map_debug_when_visible<T>(
    show_debug: bool,
    gather: impl FnOnce() -> Option<T>,
) -> Option<T> {
    show_debug.then(gather).flatten()
}

fn should_send_ping_request(show_debug: bool, last: Option<Instant>, now: Instant) -> bool {
    show_debug && last.is_none_or(|last| now.duration_since(last) >= std::time::Duration::from_secs(1))
}

#[cfg(test)]
mod ping_request_tests {
    use super::*;

    #[test]
    fn never_sent_with_f3_closed_regardless_of_elapsed_time() {
        let now = Instant::now();
        assert!(!should_send_ping_request(false, None, now));
        assert!(!should_send_ping_request(
            false,
            Some(now - std::time::Duration::from_secs(60)),
            now
        ));
    }

    #[test]
    fn sends_immediately_the_first_time_f3_is_open() {
        let now = Instant::now();
        assert!(
            should_send_ping_request(true, None, now),
            "no prior send must not block the first one"
        );
    }

    #[test]
    fn throttles_to_once_per_second_while_f3_stays_open() {
        let last = Instant::now();
        assert!(
            !should_send_ping_request(true, Some(last), last + std::time::Duration::from_millis(999)),
            "999ms after the last send must not fire yet"
        );
        assert!(
            should_send_ping_request(true, Some(last), last + std::time::Duration::from_secs(1)),
            "a full second must fire"
        );
        assert!(should_send_ping_request(
            true,
            Some(last),
            last + std::time::Duration::from_secs(5)
        ));
    }
}

/// Vanilla's own screen-level check for whether the screen currently open is drawing
/// the player's status effects itself, in which case the HUD's own effects
/// extraction draws none.
///
/// Only the ordinary inventory screen overrides it, and its answer is
/// whether there is already `>= 32` px of canvas
/// to the right of the panel? So the same three inputs the column's own draw
/// uses — the panel origin, its width, and the recipe-book shift — decide this,
/// and they are read the same way here rather than approximated, because the two
/// answers disagreeing means either two copies of the effects on screen or none.
///
/// The creative screen is excluded: vanilla's own creative-inventory screen does
/// show the column, but this client does not route that screen through the
/// container frame yet, so suppressing the overlay there would leave the effects
/// nowhere.
///
/// A free function, not a method, for the reason `recipe_panel_geometry` is one:
/// the caller is past the field-borrow split and cannot lend out `&self`.
fn screen_shows_active_effects(
    sim: &crate::sim::Sim,
    container_open: bool,
    creative_open: bool,
    recipe_open: bool,
    gui_scale: u32,
    w: u32,
    h: u32,
) -> bool {
    if creative_open || !container_open || sim.open_menu().is_some() {
        return false;
    }
    let menu = sim.player_menu();
    let layout = crate::container::slot_layout(&menu);
    let (canvas_w, _) = crate::menu::render::logical_canvas(gui_scale, w, h);
    let (panel_x, _) = crate::container::panel_origin_with_scale(&layout, gui_scale, w, h);
    let panel_x =
        panel_x + crate::container::recipe_book_panel_shift(canvas_w, layout.width, recipe_open);
    let x0 = crate::effects::inventory_column_x0(panel_x, layout.width);
    crate::effects::inventory_can_see_effects(canvas_w - x0)
}
