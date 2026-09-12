use super::*;

impl MenuNav {
    /// The World Creation screen. Every key is routed through
    /// [`crate::menu::create_world::CreateWorldNav::handle_key`], which
    /// already implements vanilla's `Screen.keyPressed` order (Escape, then
    /// the focused field, then Tab/arrow navigation, then Enter on whatever
    /// is focused) — this arm only decides what leaving the screen means.
    fn key_create_world(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = self.create_world.handle_key(key);
        self.apply_create_world(ui, outcome)
    }

    /// What a [`crate::menu::create_world::CreateWorldOutcome`] means at the
    /// `UiState` level.
    ///
    /// `Create` (queued patch): the screen is left *by the app*,
    /// not here — mirroring [`Self::apply_world_select`]'s `Play` arm above,
    /// for the identical reason: `begin_singleplayer` must stay able to show
    /// a launch failure over a screen the player recognises rather than over
    /// a screen that has already navigated away.
    fn apply_create_world(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::create_world::CreateWorldOutcome,
    ) -> MenuAction {
        use crate::menu::create_world::CreateWorldOutcome;
        match outcome {
            CreateWorldOutcome::Handled => MenuAction::None,
            // Back to the world list, **re-read**: the player may have cancelled
            // after a create that failed, and the list they return to must be
            // what is on disk rather than what was there when they left.
            CreateWorldOutcome::Cancel => {
                ui.close_create_world();
                self.open_world_list(ui);
                MenuAction::None
            }
            // **This is where a world is actually created** (reading
            // 2), and it is here rather than in `app.rs` because this is the layer
            // that knows the saves root — the same reason `ServerList::save_to` is
            // called from this file.
            //
            // `game_type` is the one `WorldCreationConfig` field that reaches disk:
            // it lands in `level.dat`'s `GameType`, so the list row says Creative
            // for a creative world. Hardcore maps to survival's `0` because
            // `LevelDat::for_new_world` writes `hardcore: 0` and this layer has no
            // business hand-editing that compound — so a Hardcore world is created
            // as Survival, which is the same gap `create_world.rs`'s own
            // "decorative" list already records for difficulty, structures, bonus
            // chest and cheats.
            CreateWorldOutcome::Create(config) => {
                let Some(auth) = self.singleplayer_permit() else {
                    return self.refuse_unowned(ui);
                };
                let game_type = match config.game_mode {
                    crate::menu::create_world::WorldGameMode::Creative => 1,
                    crate::menu::create_world::WorldGameMode::Survival
                    | crate::menu::create_world::WorldGameMode::Hardcore => 0,
                };
                // Browser: no directory, no `level.dat`, straight to the launch.
                //
                // `saves::create_world_in` deliberately *refuses* on wasm32 — a page
                // has no `saves/` to write into — so routing through it made Create
                // New World a button that did nothing: it returned to the world list
                // with "Could not create the world", which was correct and useless.
                // A browser world is real, it is simply **in memory**:
                // `IntegratedServer::open_in_memory` needs no directory and no
                // `level.dat`, and everything downstream of here already handled its
                // absence under the same `cfg`.
                //
                // The typed **name** is the one thing lost with the directory — it
                // normally lands in `level.dat` — and nothing in a browser session
                // reads it back, because the world it would label cannot be re-opened.
                // The seed still travels, in `config`, and is still honoured: a fresh
                // in-memory world has no stored settings to override it, which is the
                // same reason the native `Created` arm honours it.
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = game_type;
                    tracing::info!(
                        target: "saves",
                        name = %config.name,
                        "creating an in-memory browser world (nothing is written to disk; \
                         it is lost when the tab closes)"
                    );
                    return MenuAction::Singleplayer(
                        auth,
                        SingleplayerLaunch::Created { config },
                    );
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                // "Customize Type" (World tab) — mutually exclusive by
                // construction (`WorldCreationConfig::flat_layers`'s own
                // doc), collected into the shape
                // `saves::create_world_in` writes into
                // `world_gen_settings.dat` alongside a real, resolved seed.
                // `None` for every world type besides Flat/Single Biome,
                // which changes nothing about world creation, exactly as
                // before this half of the screen existed.
                let generator_override = match (&config.flat_layers, &config.single_biome) {
                    (Some(preset), None) => Some(crate::saves::GeneratorOverride::Flat {
                        layers: preset.layers().iter().map(|&(block, height)| (block.to_string(), height)).collect(),
                        biome: preset.biome().to_string(),
                        features: preset.features(),
                        lakes: preset.lakes(),
                    }),
                    (None, Some(biome)) => {
                        Some(crate::saves::GeneratorOverride::FixedBiome { biome: biome.id().to_string() })
                    }
                    _ => None,
                };
                match crate::saves::create_world_in(
                    &self.saves_root,
                    &config.name,
                    game_type,
                    &config.experiments,
                    generator_override.as_ref().map(|g| (g, config.seed.as_str())),
                ) {
                    Ok(world_dir) => MenuAction::Singleplayer(
                        auth,
                        SingleplayerLaunch::Created { world_dir, config },
                    ),
                    // Reported over a screen the player recognises, never routed
                    // around: a failed `create_dir` means the data directory is
                    // unwritable, and silently opening *some other* world would be
                    // the worst possible answer.
                    Err(e) => {
                        ui.close_create_world();
                        self.open_world_list(ui);
                        self.world_select
                            .set_error(format!("Could not create the world: {e}"));
                        MenuAction::None
                    }
                }
                }
            }
        }
    }

    /// The settings tree. Up/Down move the cursor, Enter activates
    /// what it is on, Escape unwinds one page.
    ///
    /// **This is the re-pointing the previous version of this comment predicted.**
    /// It used to say: the screen has no row highlight, each control owns its own
    /// key (Up/Down stepped the GUI scale, Enter toggled View Bobbing), and "when
    /// a third control lands, that is the point to introduce a real highlight and
    /// vanilla's own `OptionsScreen` list and re-point those tests once, on
    /// purpose." A hundred and thirty-third control landed; this is that.
    ///
    /// So Up/Down no longer change a value — they move a cursor, like every other
    /// screen in this shell — and the GUI scale is cycled by pressing Enter on
    /// **its own row**, which is vanilla's `CycleButton.onPress`. The tests that
    /// asserted the old binding were rewritten rather than deleted: the behaviour
    /// they protected (a scale that cycles and reaches `options.json`) is still
    /// asserted, through the new path.
    fn key_settings(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        // Key Binds has its own cursor and its own outcome type —
        // see `hover`'s matching guard for why this is a separate arm rather
        // than a branch inside the match below.
        if self.settings.page() == crate::menu::options::SettingsPage::KeyBinds {
            return self.key_key_binds(ui, key);
        }
        // Language — same reasoning.
        if self.settings.page() == crate::menu::options::SettingsPage::Language {
            return self.key_language(ui, key);
        }
        // Telemetry — same reasoning.
        if self.settings.page() == crate::menu::options::SettingsPage::Telemetry {
            return self.key_telemetry(ui, key);
        }
        // Resource Packs — same reasoning.
        if self.settings.page() == crate::menu::options::SettingsPage::ResourcePacks {
            return self.key_packs(ui, key);
        }
        let outcome = match key {
            MenuKey::Up => {
                self.settings.step(false);
                return MenuAction::None;
            }
            MenuKey::Down => {
                self.settings.step(true);
                return MenuAction::None;
            }
            MenuKey::Enter => self.settings.enter(),
            MenuKey::Escape => self.settings.escape(),
            _ => return MenuAction::None,
        };
        self.apply_settings(ui, outcome)
    }

    /// [`Self::key_settings`]'s Key Binds half.
    fn key_key_binds(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = match key {
            MenuKey::Up => {
                self.settings.key_binds_mut().step(false);
                return MenuAction::None;
            }
            MenuKey::Down => {
                self.settings.key_binds_mut().step(true);
                return MenuAction::None;
            }
            MenuKey::Enter => self
                .settings
                .key_binds_mut()
                .enter(&self.options.keybinds),
            MenuKey::Escape => self.settings.key_binds_mut().escape(),
            _ => return MenuAction::None,
        };
        self.apply_key_binds(ui, outcome)
    }

    /// What a [`super::options::key_binds::KeyBindsOutcome`] asks of the shell.
    /// Mirrors [`Self::apply_settings`]'s reason for living here: this owns
    /// [`Options`] and the file it persists to, and a rebind or a reset that
    /// only saved on exit would be the one a crash loses — the same eager-
    /// persistence rule every other live row in this tree already follows.
    ///
    /// Takes `ui` even though most arms do not touch it, rather than
    /// fabricating a throwaway [`UiState`] for the one arm
    /// ([`KeyBindsOutcome::Back`]) that can: `SettingsNav::leave_key_binds`
    /// asks to close the whole tree if its page stack is ever unexpectedly
    /// empty, and that has to reach the *real* `ui.close_settings()` or the
    /// fallback would silently do nothing to a state nobody can see.
    fn apply_key_binds(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::key_binds::KeyBindsOutcome,
    ) -> MenuAction {
        use crate::menu::key_binds::KeyBindsOutcome;
        match outcome {
            KeyBindsOutcome::None => MenuAction::None,
            // Back to Controls — `leave_key_binds` pops `SettingsNav`'s own
            // page stack (always back to Controls in practice; see its doc)
            // and its `SettingsOutcome` is routed through `apply_settings`
            // rather than discarded, for the reason this method's own doc
            // gives.
            KeyBindsOutcome::Back => {
                let outcome = self.settings.leave_key_binds();
                self.apply_settings(ui, outcome)
            }
            KeyBindsOutcome::ResetOne(action) => {
                self.options.keybinds.reset(action);
                self.persist_options();
                MenuAction::None
            }
            KeyBindsOutcome::ResetAll => {
                self.options.keybinds.reset_all();
                self.persist_options();
                MenuAction::None
            }
        }
    }

    /// [`Self::key_settings`]'s Language half. Up/Down/Enter
    /// move the list+footer cursor; typed characters always go to the search
    /// box regardless of where that cursor is — see
    /// [`crate::menu::language::LanguageNav`]'s module doc on why the two are
    /// independent.
    fn key_language(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = match key {
            MenuKey::Up => {
                self.settings.language_mut().step(false);
                return MenuAction::None;
            }
            MenuKey::Down => {
                self.settings.language_mut().step(true);
                return MenuAction::None;
            }
            MenuKey::Enter => self.settings.language_mut().enter(),
            MenuKey::Escape => self.settings.language_mut().escape(),
            // The search box is always the keyboard's text target on this
            // page (see `LanguageNav`'s doc) — routed here rather than
            // falling into the catch-all below, which is exactly the island
            // this would otherwise be: `LanguageNav::type_char`/`backspace`
            // would compile, be unit-tested, and never run.
            MenuKey::Char(ch) => {
                self.settings.language_mut().type_char(ch);
                return MenuAction::None;
            }
            MenuKey::Backspace => {
                self.settings.language_mut().backspace();
                return MenuAction::None;
            }
            _ => return MenuAction::None,
        };
        self.apply_language(ui, outcome)
    }

    /// What a [`crate::menu::language::LanguageOutcome`] asks of the shell —
    /// mirrors [`Self::apply_key_binds`]'s reason for living here (it can
    /// reach the real `ui.close_settings()` fallback [`SettingsNav::
    /// leave_language`]'s doc names, not a throwaway one).
    fn apply_language(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::language::LanguageOutcome,
    ) -> MenuAction {
        use crate::menu::language::LanguageOutcome;
        match outcome {
            LanguageOutcome::None => MenuAction::None,
            LanguageOutcome::Back => {
                let outcome = self.settings.leave_language();
                self.apply_settings(ui, outcome)
            }
        }
    }

    /// [`Self::key_settings`]'s Telemetry half. Up/Down/Enter/
    /// Escape only — no text field on this page, unlike Language.
    fn key_telemetry(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = match key {
            MenuKey::Up => {
                self.settings.telemetry_mut().step(false);
                return MenuAction::None;
            }
            MenuKey::Down => {
                self.settings.telemetry_mut().step(true);
                return MenuAction::None;
            }
            MenuKey::Enter => self.settings.telemetry_mut().enter(),
            MenuKey::Escape => self.settings.telemetry_mut().escape(),
            _ => return MenuAction::None,
        };
        self.apply_telemetry(ui, outcome)
    }

    /// What a [`crate::menu::telemetry::TelemetryOutcome`] asks of the shell
    /// — mirrors [`Self::apply_language`]. Opening a URL is not one of
    /// these outcomes: `TelemetryNav::activate` performs it directly (see
    /// that module's own doc), so the only thing this ever asks for is
    /// leaving the page.
    fn apply_telemetry(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::telemetry::TelemetryOutcome,
    ) -> MenuAction {
        use crate::menu::telemetry::TelemetryOutcome;
        match outcome {
            TelemetryOutcome::None => MenuAction::None,
            TelemetryOutcome::Back => {
                let outcome = self.settings.leave_telemetry();
                self.apply_settings(ui, outcome)
            }
        }
    }

    /// [`Self::key_settings`]'s Resource Packs half. Up/Down/
    /// Enter/Escape only — no text field, same shape as
    /// [`Self::key_telemetry`].
    fn key_packs(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = match key {
            MenuKey::Up => {
                self.settings.packs_mut().step(false);
                return MenuAction::None;
            }
            MenuKey::Down => {
                self.settings.packs_mut().step(true);
                return MenuAction::None;
            }
            MenuKey::Enter => self.settings.packs_mut().enter(),
            MenuKey::Escape => self.settings.packs_mut().escape(),
            _ => return MenuAction::None,
        };
        self.apply_packs(ui, outcome)
    }

    /// What a [`crate::menu::packs::PacksOutcome`] asks of the shell —
    /// mirrors [`Self::apply_telemetry`].
    fn apply_packs(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::packs::PacksOutcome,
    ) -> MenuAction {
        use crate::menu::packs::PacksOutcome;
        match outcome {
            PacksOutcome::None => MenuAction::None,
            PacksOutcome::Back => {
                // **This is the call that makes the screen do anything** (issue
                // it installs the column's order into
                // `resources::selected_packs` and persists it. It has to happen
                // *before* `leave_packs`, which resets the nav — vanilla commits
                // in `PackSelectionScreen.onClose` for the same reason, and
                // Escape comes through here too, so leaving is never a cancel.
                crate::menu::packs::commit(self.settings.packs());
                let outcome = self.settings.leave_packs();
                self.apply_settings(ui, outcome)
            }
        }
    }

    /// The two things a [`super::options::SettingsOutcome`] can ask of the shell.
    ///
    /// The mutation lives here rather than in [`super::options`] because this is
    /// what owns the [`Options`] and the file it is written to — and because the
    /// **eager persistence** rule is a `MenuNav` rule (see the module docs): a
    /// setting that only saved on exit is the setting a crash loses.
    fn apply_settings(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::options::SettingsOutcome,
    ) -> MenuAction {
        use crate::menu::options::{LiveOption, SettingsOutcome};
        match outcome {
            SettingsOutcome::None => MenuAction::None,
            // The root page's Done, or Escape from it. `close_settings` is what
            // knows whether that means the title screen or the pause menu.
            SettingsOutcome::Close => {
                ui.close_settings();
                MenuAction::None
            }
            SettingsOutcome::OpenFriendsSettings => {
                // The Online page is title-only: after leaving it, reuse the
                // ordinary Friends route so account-scoped service state,
                // save failures, and rollback remain in one consumer.
                ui.close_settings();
                self.friends.reset();
                self.friends.open_settings();
                ui.open_friends_from_title();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::GuiScale) => {
                self.cycle_gui_scale(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ViewBobbing) => {
                self.toggle_view_bobbing();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ShowSubtitles) => {
                self.toggle_show_subtitles();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ToggleSneak) => {
                self.toggle_toggle_sneak();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ToggleSprint) => {
                self.toggle_toggle_sprint();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ToggleAttack) => {
                self.toggle_toggle_attack();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ToggleUse) => {
                self.toggle_toggle_use();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::AutoJump) => {
                self.toggle_auto_jump();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::SprintWindow) => {
                self.step_sprint_window(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::InvertMouseX) => {
                self.toggle_invert_mouse_x();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::DiscreteMouseScroll) => {
                self.toggle_discrete_mouse_scroll();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::InvertMouseY) => {
                self.toggle_invert_mouse_y();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::MouseWheelSensitivity) => {
                self.cycle_mouse_wheel_sensitivity(1);
                MenuAction::None
            }
            // The eight chat/text-background options. Each one steps its
            // `UnitDouble` by [`crate::config::UNIT_DOUBLE_STEP`] and persists,
            // and `app.rs` already copies all eight into
            // `hud_frame.chat_options` from `self.nav.options()` every frame —
            // so no threading is needed beyond the mutation here.
            SettingsOutcome::Cycle(LiveOption::ChatScale) => {
                self.step_unit_double_option(|o| &mut o.chat_scale, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatWidth) => {
                self.step_unit_double_option(|o| &mut o.chat_width, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatHeightFocused) => {
                self.step_unit_double_option(|o| &mut o.chat_height_focused, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatHeightUnfocused) => {
                self.step_unit_double_option(|o| &mut o.chat_height_unfocused, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatLineSpacing) => {
                self.step_unit_double_option(|o| &mut o.chat_line_spacing, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatOpacity) => {
                self.step_unit_double_option(|o| &mut o.chat_opacity, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::TextBackgroundOpacity) => {
                self.step_unit_double_option(|o| &mut o.chat_background_opacity, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatColors) => {
                self.toggle_chat_colors();
                MenuAction::None
            }
            // The two migrated options persist eagerly like
            // every arm above; unlike the chat eight, neither takes effect in
            // the *current* session, because their consumers read
            // `config::Config` and `Config::resolve_persisted` folds
            // `options.json` in at launch. That is vanilla's own behaviour for
            // `renderDistance` (`applyValueImmediately = false`) and a
            // documented departure for `sensitivity` — see
            // `Config::resolve_persisted`.
            SettingsOutcome::Cycle(LiveOption::Sensitivity) => {
                self.step_unit_double_option(|o| &mut o.sensitivity, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::RenderDistance) => {
                self.step_render_distance(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::DistantHorizon) => {
                self.step_horizon_distance(16);
                MenuAction::None
            }
            // The two Accessibility-page sliders whose consumers were already
            // live. Neither needs threading beyond the mutation here:
            // `app/redraw.rs` already reads `MenuNav::damage_tilt_strength` every
            // frame, and `render::frame_for` stamps
            // `MenuFrame::panorama_speed` onto every frame beside `gui_scale`.
            SettingsOutcome::Cycle(LiveOption::DamageTiltStrength) => {
                self.step_unit_double_option(|o| &mut o.damage_tilt_strength, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::PanoramaSpeed) => {
                self.step_unit_double_option(|o| &mut o.panorama_speed, 1);
                MenuAction::None
            }
            // The fifteen rows whose consumers already ran every frame against a
            // hardcoded constant: eleven mixer buses, the projection FOV, the two
            // glint parameters and the cloud geometry. Nothing needs threading
            // beyond the mutation here — `app/redraw.rs` reads all four of
            // `Sim::set_sound_volumes`, `Sim::set_fov_y_degrees`,
            // `RenderState::set_glint_options` and `RenderState::set_cloud_status`
            // off `MenuNav::options` once per presented frame.
            SettingsOutcome::Cycle(LiveOption::SoundVolume(index)) => {
                self.step_sound_volume(index, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::Fov) => {
                self.step_fov(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::GlintSpeed) => {
                self.step_unit_double_option(|o| &mut o.glint_speed, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::GlintStrength) => {
                self.step_unit_double_option(|o| &mut o.glint_strength, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::CloudStatus) => {
                self.cycle_cloud_status(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::FramerateLimit) => {
                self.step_framerate_limit(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::EnableVsync) => {
                self.toggle_enable_vsync();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::InactivityFpsLimit) => {
                self.cycle_inactivity_fps_limit(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::GraphicsPreset) => {
                self.step_graphics_preset(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::CutoutLeaves) => {
                self.toggle_cutout_leaves();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::MipmapLevels) => {
                self.step_mipmap_levels(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::EntityShadows) => {
                self.toggle_entity_shadows();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::WeatherRadius) => {
                self.step_weather_radius(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::MenuBackgroundBlurriness) => {
                self.step_menu_background_blurriness(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::AttackIndicator) => {
                self.cycle_attack_indicator(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::Particles) => {
                self.cycle_particle_level(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::BiomeBlendRadius) => {
                self.step_biome_blend_radius(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::InGameNotification) => {
                self.options.in_game_notification = !self.options.in_game_notification;
                self.persist_options();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::SharePresence) => {
                self.cycle_share_presence(1);
                MenuAction::None
            }
        }
    }

    /// Steps one of the eleven `soundSource.*` volumes and persists it eagerly.
    ///
    /// Goes through [`Self::step_unit_double_option`] rather than writing the
    /// wrap out again — the eleven are `UnitDouble`s like the chat sliders, and
    /// the only thing that varies is the array slot.
    ///
    /// An out-of-range index is a **no-op**, not a panic: the index arrives from
    /// a `const` cell on the Sound page, so a bad one is an authoring mistake in
    /// `menu::options::SOUND` and is caught by
    /// `sound_rows_index_the_category_they_name`, not something a player can
    /// provoke mid-session.
    fn step_sound_volume(&mut self, index: u8, delta: i32) {
        let slot = index as usize;
        if slot >= self.options.sound_volumes.len() {
            return;
        }
        self.step_unit_double_option(move |o| &mut o.sound_volumes[slot], delta);
    }

    /// Steps `fov` by one degree and wraps, then persists.
    ///
    /// **Wraps rather than saturating**, for the reason
    /// [`Self::step_render_distance`] records at length: a keyboard Enter is a
    /// click, and a value parked at 110 has to be able to come back down. A
    /// *mouse* click on the track goes through [`Self::set_live_slider`] instead
    /// and lands wherever the cursor is, so the 81-degree span is not 81 clicks
    /// for a mouse user.
    ///
    /// The bounds are `config`'s [`crate::config::MIN_FOV`]/`MAX_FOV`, which are
    /// vanilla's `IntRange(30, 110)` — the same pair
    /// `menu::options::INT_RANGE_SLIDERS` places the handle with, so the value a
    /// click can reach and the track it draws on cannot disagree.
    fn step_fov(&mut self, delta: i32) {
        use crate::config::{MAX_FOV, MIN_FOV};
        let span = (MAX_FOV - MIN_FOV + 1) as i32;
        let offset = self.options.fov as i32 - MIN_FOV as i32;
        let wrapped = (offset + delta).rem_euclid(span);
        self.options.fov = MIN_FOV + wrapped as u32;
        self.persist_options();
    }

    /// Cycles Clouds through `CloudStatus.values()`' own declaration order — OFF,
    /// FAST, FANCY — and wraps, then persists.
    ///
    /// **Three states, and the order is the enum's rather than a chosen one**,
    /// because that is what `CycleButton` visits. A hand-picked order would put
    /// FANCY (the default) somewhere other than where vanilla's third click
    /// leaves it.
    ///
    /// A `cloud_status` that is somehow not in the list restarts at OFF rather
    /// than sticking, which cannot happen through
    /// [`crate::config::cloud_status_from_name`] but keeps the `position` lookup
    /// honest about being fallible.
    fn cycle_cloud_status(&mut self, delta: i32) {
        use lodestone_render::CloudStatus;
        const ORDER: [CloudStatus; 3] = [CloudStatus::Off, CloudStatus::Fast, CloudStatus::Fancy];
        let index = ORDER
            .iter()
            .position(|s| *s == self.options.cloud_status)
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(ORDER.len() as i32) as usize;
        self.options.cloud_status = ORDER[next];
        self.persist_options();
    }

    /// Cycles `attackIndicator` through its three declared states
    /// (`Off`, `Crosshair`, `Hotbar`) and wraps, then persists —
    /// [`Self::cycle_cloud_status`]'s shape, and vanilla's own `CycleButton`
    /// order, which is the enum's declaration order.
    ///
    /// No consumer push: `app/redraw.rs` copies the field onto
    /// `HudFrame::attack_indicator` every frame, the same way it already copies
    /// the eight chat options.
    fn cycle_attack_indicator(&mut self, delta: i32) {
        use crate::config::AttackIndicator;
        const ORDER: [AttackIndicator; 3] = [
            AttackIndicator::Off,
            AttackIndicator::Crosshair,
            AttackIndicator::Hotbar,
        ];
        let index = ORDER
            .iter()
            .position(|s| *s == self.options.attack_indicator)
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(ORDER.len() as i32) as usize;
        self.options.attack_indicator = ORDER[next];
        self.persist_options();
    }

    /// Cycles `particles` through its three declared states (`All`,
    /// `Decreased`, `Minimal`) and wraps, then persists —
    /// [`Self::cycle_attack_indicator`]'s shape.
    ///
    /// No consumer push: `app/redraw.rs` hands the field to
    /// `Sim::set_particle_level` every presented frame.
    fn cycle_particle_level(&mut self, delta: i32) {
        use crate::config::ParticleLevel;
        const ORDER: [ParticleLevel; 3] = [
            ParticleLevel::All,
            ParticleLevel::Decreased,
            ParticleLevel::Minimal,
        ];
        let index = ORDER
            .iter()
            .position(|s| *s == self.options.particles)
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(ORDER.len() as i32) as usize;
        self.options.particles = ORDER[next];
        self.persist_options();
    }

    /// Steps `framerateLimit` by one bucket (10 fps) and wraps, then persists.
    ///
    /// Wraps in the `[10, 260]` domain rather than saturating, for
    /// [`Self::step_render_distance`]'s reason: this is a click-only control
    /// (a *drag* goes through [`Self::set_live_slider`] instead), and a value
    /// parked at "Unlimited" has to be able to come back down.
    fn step_framerate_limit(&mut self, delta: i32) {
        use crate::config::{MIN_FRAMERATE_LIMIT, UNLIMITED_FRAMERATE_CUTOFF};
        let buckets = (UNLIMITED_FRAMERATE_CUTOFF - MIN_FRAMERATE_LIMIT) / 10 + 1;
        let offset = (self.options.framerate_limit - MIN_FRAMERATE_LIMIT) / 10;
        let wrapped = (offset as i32 + delta).rem_euclid(buckets as i32) as u32;
        self.options.framerate_limit = MIN_FRAMERATE_LIMIT + wrapped * 10;
        self.persist_options();
    }

    /// Steps `mipmapLevels` by one and wraps, then persists — vanilla's own
    /// `IntRange(0, 4)` (`menu::options::INT_RANGE_SLIDERS`'s `"mipmapLevels"`
    /// row), the same click-only wrap shape as [`Self::step_fov`]: a *drag*
    /// goes through [`Self::set_live_slider`] instead, and a value parked at
    /// the maximum has to be able to come back down through a click alone.
    ///
    /// Also pushes the new depth into `crate::resources::set_mipmap_levels`,
    /// which is what actually rebuilds the atlas — this function only owns
    /// the menu-side value and its wrap, exactly as
    /// [`Self::set_live_slider`]'s own `MipmapLevels` arm does for a drag.
    fn step_mipmap_levels(&mut self, delta: i32) {
        let max = lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS as i32;
        let span = max + 1;
        let offset = self.options.mipmap_levels as i32;
        let wrapped = (offset + delta).rem_euclid(span);
        self.options.mipmap_levels = wrapped as u32;
        crate::resources::set_mipmap_levels(self.options.mipmap_levels);
        self.persist_options();
    }

    /// Flips `options.vsync` and saves immediately, same eager-persistence
    /// rule as [`Self::toggle_chat_colors`]. The live consumer is
    /// `WindowApp::sync_vsync_present_mode`, which polls this field every
    /// presented frame rather than being pushed from here — see that method's
    /// doc for why a poll is the safe shape against a GPU setter.
    fn toggle_enable_vsync(&mut self) {
        self.options.enable_vsync = !self.options.enable_vsync;
        self.persist_options();
    }

    /// Cycles `inactivityFpsLimit` through its two declared states
    /// (`Minimized`, `Afk`) and wraps, then persists. Same shape as
    /// [`Self::cycle_cloud_status`], two states instead of three.
    fn cycle_inactivity_fps_limit(&mut self, delta: i32) {
        use crate::config::InactivityFpsLimit;
        const ORDER: [InactivityFpsLimit; 2] =
            [InactivityFpsLimit::Minimized, InactivityFpsLimit::Afk];
        let index = ORDER
            .iter()
            .position(|s| *s == self.options.inactivity_fps_limit)
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(ORDER.len() as i32) as usize;
        self.options.inactivity_fps_limit = ORDER[next];
        self.persist_options();
    }

    /// Cycles the three Friends activity visibility levels and persists them.
    fn cycle_share_presence(&mut self, delta: i32) {
        use crate::config::PresenceSharing;
        const ORDER: [PresenceSharing; 3] = [
            PresenceSharing::All,
            PresenceSharing::Limited,
            PresenceSharing::None,
        ];
        let index = ORDER
            .iter()
            .position(|value| *value == self.options.share_presence)
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(ORDER.len() as i32) as usize;
        self.options.share_presence = ORDER[next];
        self.persist_options();
    }

    /// Steps `graphicsPreset` through `GraphicsPreset::ORDER` and wraps, then
    /// applies it — [`Self::apply_graphics_preset`] — and persists.
    ///
    /// The apply happens **every** step, `Custom` included, matching vanilla:
    /// `Options::applyGraphicsPreset` calls `value.apply(minecraft)`
    /// unconditionally, and `GraphicsPreset.apply`'s `switch` simply has no
    /// `CUSTOM` case, so applying `Custom` is a real call that writes nothing
    /// — not a skipped call. [`Self::apply_graphics_preset`] mirrors that
    /// shape rather than special-casing `Custom` at this call site.
    fn step_graphics_preset(&mut self, delta: i32) {
        use crate::config::GraphicsPreset;
        let index = GraphicsPreset::ORDER
            .iter()
            .position(|p| *p == self.options.graphics_preset)
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(GraphicsPreset::ORDER.len() as i32) as usize;
        self.options.graphics_preset = GraphicsPreset::ORDER[next];
        self.apply_graphics_preset();
        self.persist_options();
    }

    /// Vanilla's `GraphicsPreset::apply`, the
    /// three fields of its seventeen this client has real consumers for.
    ///
    /// | preset | `renderDistance` | `cloudStatus` | `cutoutLeaves` |
    /// |---|---|---|---|
    /// | `Fast` | 8 | `Fast` | `false` |
    /// | `Fancy` | 16 | `Fancy` | `true` |
    /// | `Fabulous` | 32 | `Fancy` | `true` |
    /// | `Custom` | — | — | — |
    ///
    /// `Custom` writes nothing, matching vanilla's own `switch` (no `CUSTOM`
    /// arm — see [`Self::step_graphics_preset`]'s doc). The fourteen fields
    /// this client does not have a consumer for at all
    /// (`biomeBlendRadius`, `simulationDistance`, `particles`,
    /// `mipmapLevels`, `entityShadows`, `menuBackgroundBlurriness`,
    /// `cloudRange`, `improvedTransparency`, `weatherRadius`,
    /// `maxAnisotropyBit`, `textureFiltering`, `prioritizeChunkUpdates`,
    /// `entityDistanceScaling`, `ambientOcclusion`) are left alone — writing
    /// them would move a settings-row *label* with nothing behind it to
    /// consume the new value, the exact fabrication `docs/`'s "departure 1"
    /// exists to name rather than hide.
    ///
    /// Never resets a hand-picked `render_distance`/`cloud_status`/
    /// `cutout_leaves` back to `Custom` on its own: vanilla's
    /// `setGraphicsPresetToCustom` (called from each of those options'
    /// individual `onChange`) has no counterpart here yet, so choosing FAST
    /// and then hand-tweaking Render Distance leaves the Preset row reading
    /// "Fast" even though the value it placed has moved — a known,
    /// documented gap rather than a silent one.
    fn apply_graphics_preset(&mut self) {
        use crate::config::GraphicsPreset;
        use lodestone_render::CloudStatus;
        match self.options.graphics_preset {
            GraphicsPreset::Fast => {
                self.options.render_distance = 8;
                self.options.cloud_status = CloudStatus::Fast;
                self.options.cutout_leaves = false;
            }
            GraphicsPreset::Fancy => {
                self.options.render_distance = 16;
                self.options.cloud_status = CloudStatus::Fancy;
                self.options.cutout_leaves = true;
            }
            GraphicsPreset::Fabulous => {
                self.options.render_distance = 32;
                self.options.cloud_status = CloudStatus::Fancy;
                self.options.cutout_leaves = true;
            }
            GraphicsPreset::Custom => {}
        }
    }

    /// Flips `options.cutoutLeaves` and saves immediately, same eager-persistence
    /// rule as [`Self::toggle_chat_colors`]. See
    /// [`crate::config::Options::cutout_leaves`]'s doc for the render-side
    /// consumer and why toggling it forces a remesh.
    fn toggle_cutout_leaves(&mut self) {
        self.options.cutout_leaves = !self.options.cutout_leaves;
        self.persist_options();
    }

    /// Flips `options.entityShadows` (owner report: "entity shadows are
    /// missing") and saves immediately, same eager-persistence rule as
    /// [`MenuNav::toggle_cutout_leaves`]. `app/redraw.rs` reads it into
    /// `RenderState::set_entity_shadows_enabled` every presented frame, so no
    /// further threading is needed beyond the mutation here.
    fn toggle_entity_shadows(&mut self) {
        self.options.entity_shadows = !self.options.entity_shadows;
        self.persist_options();
    }

    /// Steps `weatherRadius` by one block and wraps, then persists — the same
    /// shape and the same wrap reasoning as [`Self::step_render_distance`].
    ///
    /// The bounds are `config`'s, which are vanilla's `IntRange(3, 10)`: the
    /// same pair `menu::options::INT_RANGE_SLIDERS` places the handle with, so
    /// the value a click can reach and the track it draws on cannot disagree.
    /// No consumer push is needed — `app::weather::weather_columns_for_frame`
    /// reads the field off `MenuNav::options` once per presented frame, exactly
    /// as `cutout_leaves` and `entity_shadows` are polled.
    fn step_weather_radius(&mut self, delta: i32) {
        use crate::config::{MAX_WEATHER_RADIUS, MIN_WEATHER_RADIUS};
        let span = MAX_WEATHER_RADIUS - MIN_WEATHER_RADIUS + 1;
        let offset = self.options.weather_radius - MIN_WEATHER_RADIUS;
        self.options.weather_radius = MIN_WEATHER_RADIUS + (offset + delta).rem_euclid(span);
        self.persist_options();
    }

    /// Steps `biomeBlendRadius` by one and wraps, then persists —
    /// [`Self::step_weather_radius`]'s shape, over vanilla's `IntRange(0, 7)`.
    ///
    /// Steps the **radius**, which is what the value set ranges over; the row's
    /// label turns it into the window width. No consumer push:
    /// `app/redraw.rs` hands the field to `Sim::set_blend_radius` every
    /// presented frame, whose own equality guard is what stops that poll
    /// re-meshing the world every frame.
    fn step_biome_blend_radius(&mut self, delta: i32) {
        use crate::config::{MAX_BIOME_BLEND_RADIUS, MIN_BIOME_BLEND_RADIUS};
        let span = MAX_BIOME_BLEND_RADIUS - MIN_BIOME_BLEND_RADIUS + 1;
        let offset = self.options.biome_blend_radius - MIN_BIOME_BLEND_RADIUS;
        self.options.biome_blend_radius =
            MIN_BIOME_BLEND_RADIUS + (offset + delta).rem_euclid(span);
        self.persist_options();
    }

    /// Steps `menuBackgroundBlurriness` by one and wraps, then persists —
    /// [`Self::step_weather_radius`]'s shape, over vanilla's `IntRange(0, 10)`.
    ///
    /// The wrap is what makes `0` (OFF) reachable again after a click walks the
    /// value up to 10, which matters more here than on most rows: the two ends
    /// of this slider are the only two states most players will want.
    fn step_menu_background_blurriness(&mut self, delta: i32) {
        use crate::config::{MAX_MENU_BACKGROUND_BLURRINESS, MIN_MENU_BACKGROUND_BLURRINESS};
        let min = MIN_MENU_BACKGROUND_BLURRINESS as i32;
        let span = MAX_MENU_BACKGROUND_BLURRINESS as i32 - min + 1;
        let offset = self.options.menu_background_blurriness as i32 - min;
        self.options.menu_background_blurriness = (min + (offset + delta).rem_euclid(span)) as u32;
        self.persist_options();
    }

    /// Steps `renderDistance` by one chunk and wraps, then persists.
    ///
    /// **Wraps rather than saturating**, matching every other live control on
    /// this tree (`cycle_gui_scale`'s `rem_euclid`, `cycle_mouse_wheel_sensitivity`'s
    /// period): a click is the only way to move these rows, so a value parked at
    /// the maximum has to be able to come back down. Vanilla drags instead and
    /// therefore needs no wrap at all — this is a consequence of departure 1, not
    /// a transcription of `IntRangeBase::next`,
    /// which really does saturate.
    ///
    /// The bounds are `config`'s, which are vanilla's `IntRange(2, 32)` — the same
    /// pair `menu::options::INT_RANGE_SLIDERS` places the handle with, so the
    /// value a click can reach and the track it draws on cannot disagree.
    fn step_render_distance(&mut self, delta: i32) {
        use crate::config::{MAX_RENDER_DISTANCE, MIN_RENDER_DISTANCE};
        let span = (MAX_RENDER_DISTANCE - MIN_RENDER_DISTANCE + 1) as i32;
        let offset = self.options.render_distance as i32 - MIN_RENDER_DISTANCE as i32;
        let wrapped = (offset + delta).rem_euclid(span);
        self.options.render_distance = MIN_RENDER_DISTANCE + wrapped as u32;
        self.persist_options();
    }

    /// Steps the coarse visual horizon in 16-chunk cells and persists it.
    ///
    /// This setting deliberately has no route to `Config::render_distance` or
    /// the server view radius: it only bounds the fixed distant-terrain
    /// representation the redraw path may populate. The 16-chunk quantum is
    /// one coarse cell, so every reachable nonzero value has a meaningful
    /// visual effect instead of paying for a fraction of a cell.
    fn step_horizon_distance(&mut self, delta: i32) {
        use crate::config::MAX_HORIZON_DISTANCE_CHUNKS;
        const STEP: i32 = 16;
        let buckets = MAX_HORIZON_DISTANCE_CHUNKS as i32 / STEP + 1;
        let bucket = self.options.horizon_distance_chunks as i32 / STEP;
        self.options.horizon_distance_chunks =
            ((bucket + delta.div_euclid(STEP)).rem_euclid(buckets) * STEP) as u32;
        self.persist_options();
    }

    /// Set a live slider's value from a track fraction — the drag half of
    /// vanilla's `AbstractSliderButton` (`onClick`/`onDrag` both call
    /// `setValueFromMouse`).
    ///
    /// Returns `true` when the fraction was applied, `false` for a
    /// [`LiveOption`] that is not slider-shaped (every toggle, and the two the
    /// ranges below do not cover) — the caller falls back to the click-step
    /// path on `false` rather than swallowing the click, so a control this does
    /// not understand keeps working exactly as it did.
    ///
    /// **This is the one place the departure recorded on
    /// [`Self::step_render_distance`] is lifted.** That doc says "a click is the
    /// only way to move these rows, so a value parked at the maximum has to be
    /// able to come back down", which is why every `step_*` wraps. With a real
    /// drag the wrap is no longer load-bearing for reachability — but the
    /// `step_*` functions keep it, because they are still what a *keyboard*
    /// Enter uses and that has no other way down.
    ///
    /// The two conversions both come from the tables the *handle draw* uses
    /// (`LiveOption::unit_double_mut`, `LiveOption::int_range`), never from a
    /// restated range: a slider whose drag and whose handle disagreed about the
    /// bounds would land the handle somewhere the value cannot be.
    fn set_live_slider(&mut self, live: LiveOption, fraction: f32) -> bool {
        let f = fraction.clamp(0.0, 1.0);
        // The eight `UnitDouble` options plus `sensitivity`: the fraction *is*
        // the value, so this needs no conversion at all.
        if let Some(slot) = live.unit_double_mut(&mut self.options) {
            *slot = f;
            self.persist_options();
            return true;
        }
        // The `IntRange` options, through vanilla's own bucket map.
        if let Some(range) = live.int_range() {
            let value = range.from_slider_value(f);
            match live {
                LiveOption::RenderDistance => {
                    self.options.render_distance = value.max(0) as u32;
                }
                LiveOption::DistantHorizon => {
                    self.options.horizon_distance_chunks = value.clamp(
                        0,
                        crate::config::MAX_HORIZON_DISTANCE_CHUNKS as i32,
                    ) as u32;
                }
                LiveOption::SprintWindow => {
                    self.options.sprint_window_ticks = value.clamp(0, 255) as u8;
                }
                // The clamp is `config`'s own bounds rather than `0..`: an FOV of
                // zero is a degenerate projection matrix, and `from_slider_value`
                // is only guaranteed in range for a fraction in `[0, 1]`.
                LiveOption::Fov => {
                    self.options.fov = value.clamp(
                        crate::config::MIN_FOV as i32,
                        crate::config::MAX_FOV as i32,
                    ) as u32;
                }
                // `framerateLimit`'s bucket is `fps / 10` (`INT_RANGE_SLIDERS`'s
                // own row), so the value this bucket map returns has to be
                // multiplied back before it is a real fps.
                LiveOption::FramerateLimit => {
                    self.options.framerate_limit = (value.max(1) as u32 * 10).clamp(
                        crate::config::MIN_FRAMERATE_LIMIT,
                        crate::config::UNLIMITED_FRAMERATE_CUTOFF,
                    );
                }
                // `mipmapLevels`' bucket is the depth itself (`INT_RANGE_SLIDERS`'s
                // own row is `IntRange(0, 4)` with no xmap), so unlike
                // `FramerateLimit` above nothing has to be scaled back. Pushes the
                // new depth into `crate::resources::set_mipmap_levels`, which is
                // what actually rebuilds the atlas — see that function's doc.
                LiveOption::MipmapLevels => {
                    self.options.mipmap_levels = value
                        .clamp(0, lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS as i32)
                        as u32;
                    crate::resources::set_mipmap_levels(self.options.mipmap_levels);
                }
                // `weatherRadius`' bucket is the block radius itself
                // (`INT_RANGE_SLIDERS`'s `IntRange(3, 10)` with no xmap), so
                // nothing has to be scaled back. Clamped to `config`'s own
                // bounds rather than `0..`: a zero radius draws no
                // precipitation at all, which reads as a broken renderer.
                LiveOption::WeatherRadius => {
                    self.options.weather_radius = value.clamp(
                        crate::config::MIN_WEATHER_RADIUS,
                        crate::config::MAX_WEATHER_RADIUS,
                    );
                }
                // `menuBackgroundBlurriness`' bucket is the blurriness itself
                // (`IntRange(0, 10)`, no xmap). Clamped to `config`'s own
                // bounds; `0` is a legitimate value here (vanilla's OFF), so
                // unlike `weatherRadius` the low end is not a degenerate state.
                LiveOption::MenuBackgroundBlurriness => {
                    self.options.menu_background_blurriness = value.clamp(
                        crate::config::MIN_MENU_BACKGROUND_BLURRINESS as i32,
                        crate::config::MAX_MENU_BACKGROUND_BLURRINESS as i32,
                    ) as u32;
                }
                // `biomeBlendRadius`' bucket is the radius itself
                // (`IntRange(0, 7)`, no xmap). Clamped to `config`'s own
                // bounds, whose maximum is also `BlendRowCursor`'s ring
                // capacity — so this clamp is a memory bound as well as a
                // fidelity one.
                LiveOption::BiomeBlendRadius => {
                    self.options.biome_blend_radius = value.clamp(
                        crate::config::MIN_BIOME_BLEND_RADIUS,
                        crate::config::MAX_BIOME_BLEND_RADIUS,
                    );
                }
                // `int_range` only answers for the eight above; a ninth would
                // have to add its own write here, and falling through to
                // `false` is the honest result until it does.
                _ => return false,
            }
            self.persist_options();
            return true;
        }
        // `graphicsPreset`'s `SliderableEnum`, the third shape alongside
        // `UnitDouble` and `IntRange` above — see
        // `menu::options::graphics_preset_from_fraction`.
        if live == LiveOption::GraphicsPreset {
            self.options.graphics_preset = crate::menu::options::graphics_preset_from_fraction(f);
            self.apply_graphics_preset();
            self.persist_options();
            return true;
        }
        false
    }

    /// Steps one `UnitDouble`-backed option and persists it eagerly.
    ///
    /// Takes a field selector rather than being written out once per option:
    /// every one of these has an identical `[0, 1]` domain and an identical wrap,
    /// so the only thing that varies is which field is being moved. The
    /// per-option *semantics* (the pixel and percent mappings, the OFF caption)
    /// live in `menu::options::live_value`, where the vanilla stringifier they
    /// come from is cited.
    ///
    /// **Was `step_chat_option`**, and the rename is the point rather than
    /// tidying: it now carries the Damage Tilt and Panorama Scroll Speed rows on
    /// the Accessibility page too, and a name claiming otherwise is how the next
    /// reader concludes there is no generic stepper and writes a second one.
    fn step_unit_double_option(
        &mut self,
        field: impl FnOnce(&mut Options) -> &mut f32,
        delta: i32,
    ) {
        let slot = field(&mut self.options);
        *slot = crate::config::step_unit_double(*slot, delta);
        self.persist_options();
    }

    /// Flips `options.chat.color`, the one non-slider chat option.
    fn toggle_chat_colors(&mut self) {
        self.options.chat_colors = !self.options.chat_colors;
        self.persist_options();
    }

}
