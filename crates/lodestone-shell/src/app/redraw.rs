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
        let step = self.pacer.begin_frame(frame_start);
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

        // Vanilla's View Bobbing option, pushed down before either draw path
        // because the toggle lives on a menu screen and should take effect while
        // that screen is still showing. Polled per frame rather than fired on the
        // toggle for the same reason the present-mode sync was: `MenuNav` owns the
        // `Options` and is pure, and `Sim` owns none.
        self.sim.set_view_bobbing(self.nav.view_bobbing());

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

        // The hand needs its own copy of the view bob: vanilla applies `bobView`
        // a *second* time to a fresh pose stack seeded with the unbobbed
        // model-view (`GameRenderer.java:333-362`), rather than letting the hand
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
            .map(|record| (record.item.clone(), record.enchanted));
        // The tuple's first element, re-derived rather than cloned: issue #154's
        // spyglass FOV/vignette needs the bare location further down in this
        // function (`ScreenEffects::scoping`), and the closure otherwise takes
        // ownership of the whole tuple for the render source's lifetime.
        let held_for_scoping = held.as_ref().map(|(loc, _)| loc.clone());
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

        // Shulker boxes (issue #23). Same per-frame install as the four above, and
        // the same reason this call site matters as much as the geometry: a 26.2
        // shulker box has **no block model**, so without it the terrain mesher
        // leaves a hole where every box is — the chest failure mode exactly.
        if let Some(f) = self.sim.shulker_source() {
            render.set_shulker_source(f);
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
        // ±0.01/tick over ~100 ticks (`ServerLevel.java:762-768`), and a
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
        // The three inputs are the ones `Minecraft.java:2601-2621` uses, and two of
        // them are easy to get wrong: `creative` is `instabuild && mayfly` and not
        // a gamemode check (`Sim::music_creative`), and `underwater` is
        // water-specific rather than any fluid (`Sim::music_underwater`).
        //
        // `background_music` is the standing biome's own three-slot record, from
        // the 42-biome table, with a **dimension-specific** fallback — see
        // `Sim::background_music`. It is not the biome id: the biome only chooses
        // the record, and `BackgroundMusic::select` makes the pick.
        let background = self.sim.background_music();
        let now = std::time::Instant::now();
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
        // keys on (`Hud.extractCameraOverlays`, `Hud.java:269-291`). That is a
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
            // SPYGLASS)` (`Player.java:1936-1938`). Both halves: `Sim::
            // using_item()` (the two-line accessor issue #154 was waiting
            // on) and `held_for_scoping`, the same item id already computed
            // above for the first-person hand pass.
            scoping: self.sim.using_item()
                && held_for_scoping
                    .as_ref()
                    .is_some_and(|loc| loc.namespace() == "minecraft" && loc.path() == "spyglass"),
            // No potion-effect-duration tracker or nether-portal-proximity
            // tracker exists anywhere in this codebase yet to compute these
            // — `0.0` is the honest current answer, not a placeholder
            // pretending to work. See `docs/screen-overlays.md`'s "Confusion
            // and portal" section.
            nausea_intensity: 0.0,
            portal_intensity: 0.0,
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
        let inst_fps = if dt > 0.0 { (1.0 / dt) as f32 } else { 0.0 };
        self.fps_ema = if self.fps_ema == 0.0 {
            inst_fps
        } else {
            self.fps_ema * 0.9 + inst_fps * 0.1
        };
        self.sim.stats.section_count = stats.sections_drawn;
        self.sim.stats.quads = stats.total_quads;
        self.sim.stats.vram_bytes = stats.vram_bytes;
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
        self.sim.stats.fps = self.fps_ema;
        // Issue #411: `ServerDifficulty` reached a real, tested ECS fold but
        // nothing in the shell read it — this is that last hop, onto the F3
        // debug overlay's own `DIFFICULTY` line (`hud.rs`'s `DebugStats::lines`).
        self.sim.stats.difficulty = self.sim.difficulty();

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
        // Pull enough history for the HUD to fade/scroll; it caps and ages them.
        // The feed hands back owned legacy strings (flattened from the canonical
        // `ChatFeed`'s `Text` at read time); borrow them into the `&str` slice
        // the HUD frame takes, keeping both locals alive for the frame's scope.
        let chat_owned: Vec<(String, f32)> = self.sim.recent_chat(if chat_open { 20 } else { 10 });
        let chat_lines: Vec<(&str, f32)> = chat_owned
            .iter()
            .map(|(line, age)| (line.as_str(), *age))
            .collect();
        let player_rows: Vec<String> = if self.tab_held {
            self.sim.player_rows()
        } else {
            Vec::new()
        };
        // Read on the same condition as the rows, and for the same reason: both
        // are a world clone, and neither is drawn unless the overlay is up.
        let (tab_header, tab_footer) = if self.tab_held {
            self.sim.tab_banner()
        } else {
            (Vec::new(), Vec::new())
        };
        let health = self.sim.health();
        let food = self.sim.food();
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

        let mut hud_frame = HudFrame::new(&self.sim.stats);
        hud_frame.show_debug = self.show_debug;
        hud_frame.crosshair = crosshair;
        hud_frame.chat = &chat_lines;
        hud_frame.sound_subtitles = &sound_subtitles;
        // Persisted wrap results (issue #527 (a)): without this the whole
        // visible log is re-wrapped, quadratically, every frame.
        hud_frame.chat_wrap = Some(&self.chat_wrap);
        hud_frame.chat_input = chat_open.then(|| self.chat_input.as_str());
        // Vanilla blinks the text cursor on a 300 ms half-period:
        // `TextCursorUtils.CURSOR_BLINK_INTERVAL_MS == 300` and
        // `isCursorVisible(ms) == (ms / 300) % 2 == 0`
        // (`.cache/mc/26.2/client-src/.../TextCursorUtils.java:9,20-22`). The
        // phase has to come from wall time rather than the tick clock, because
        // the caret keeps blinking while the game is paused.
        hud_frame.chat_caret_visible = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_millis() / 300) % 2 == 0)
            .unwrap_or(true);
        // Without this the whole chat-option chain is an island: the fields are
        // persisted, `ChatDisplayOptions` is read by the draw, and the live
        // client would still show vanilla defaults forever.
        let chat_opts = self.nav.options();
        hud_frame.chat_options = crate::hud::ChatDisplayOptions {
            scale: chat_opts.chat_scale,
            width_pct: chat_opts.chat_width,
            height_pct_unfocused: chat_opts.chat_height_unfocused,
            height_pct_focused: chat_opts.chat_height_focused,
            line_spacing: chat_opts.chat_line_spacing,
            text_opacity: chat_opts.chat_opacity,
            background_opacity: chat_opts.chat_background_opacity,
            colors: chat_opts.chat_colors,
        };
        hud_frame.players = self.tab_held.then_some(player_rows.as_slice());
        hud_frame.tab_header = tab_header.as_slice();
        hud_frame.tab_footer = tab_footer.as_slice();
        hud_frame.sidebar = sidebar.as_ref();
        hud_frame.boss_bars = &boss_bars;
        hud_frame.health = health;
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
        // translucent background over it (`Screen.java:375-386`,
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
        let player_menu;
        let (container_menu, container_title) = if let Some(open) = open_menu.as_ref() {
            // Through the language table, not `Text::to_plain_string` — the
            // server sends `translate("container.crafting")`, and the model's
            // stub table has no `container.*` key, so flattening it directly put
            // the raw key on screen (issue #52). See `container::menu_title`.
            (
                Some(&open.menu),
                crate::container::menu_title(&open.title, self.sim.translator().as_ref()),
            )
        } else if self.ui.is_container_open() {
            player_menu = self.sim.player_menu();
            // **"Crafting"**, not "Inventory" (issue #370). `InventoryScreen`
            // passes `translatable("container.crafting")` as its title
            // (`InventoryScreen.java:28`) — it names the 2x2 grid — and the
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
            // (`Inventory.java:55`), so there is no server component to resolve.
            let inventory_label =
                crate::container::player_inventory_label(self.sim.translator().as_ref());
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
                .with_drag(self.menu_input.drag_paint())
                // The wire `menu_type`, which is what `menu_type_title_anchor`
                // keys on. Without this line the nine per-screen title anchors
                // are correct and **unfed**, so a furnace or an anvil silently
                // falls back to the generic `(8, 6)` — the same class of gap as
                // a source installed but never set.
                .with_menu_type(open_menu.as_ref().map(|open| &open.menu_type))
                .with_recipe_book(self.recipe_book.as_ref())
                // The anvil's XP cost and the enchanting table's three level
                // costs (`docs/container-cost-screens.md`'s "What is not yet
                // wired" gap). `&[]` on the player-inventory screen (no
                // `open_menu`), which draws neither cost — correct, since
                // neither special layout is ever the player's own inventory.
                .with_cost_context(
                    open_menu.as_ref().map_or(&[][..], |open| open.data.as_slice()),
                    self.sim.has_infinite_materials(),
                    self.sim.xp().map_or(0, |(level, _)| level),
                );
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
            container_renderer.render_with_icons_scaled(
                device,
                queue,
                frame.view(),
                Some(render.depth_view()),
                &container_frame,
                item_models,
                self.nav.gui_scale(),
                w,
                h,
            );

            // The recipe-book panel (issue #163), as its own pass **over** the
            // container panel it belongs to — the toggle button sits on the
            // container's own chrome and the book body overlaps its left edge at
            // narrow canvases (`container.rs`'s documented clamp), so drawing it
            // before the container would bury both.
            //
            // This call is what stops the whole
            // `recipe_book_panel_layout`/`_hit_test`/`_geometry` family being an
            // island: it was built and unit-tested with 75 tests and reached
            // zero pixels because nothing composited the vertices.
            if let Some(menu) = container_menu {
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
            }
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
                Some(progress) => {
                    crate::menu::render::loading_frame_with_progress(label, progress)
                }
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
        if self.ui.is_settings()
            && self.ui.settings_in_world()
            && let Some(menu) = self.menu.as_mut()
        {
            let settings_frame = crate::menu::options::settings_frame(
                self.nav.settings(),
                self.nav.options(),
                self.nav.options_save_error(),
            );
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
                match crate::screenshot::capture(device, queue, texture, SystemTime::now()) {
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
}
