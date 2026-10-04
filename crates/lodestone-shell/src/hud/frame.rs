use super::*;

/// Everything the HUD draws for one frame, bundled so the geometry builder and
/// the GPU renderer take one argument that can grow without churning every call
/// site. Borrows so building it per frame allocates nothing beyond the `chat`
/// slice the caller already has.
#[derive(Debug)]
pub struct HudFrame<'a> {
    /// The debug-overlay stats (drawn only when `show_debug`).
    pub stats: &'a DebugStats,
    /// Whether the F3 debug overlay is visible.
    pub show_debug: bool,
    /// Whether to draw the centre aiming reticle. Suppressed while any screen is
    /// up (pause, chat, a container).
    ///
    /// Vanilla is *not* the authority for that suppression — settled, not just
    /// suspected. Read directly rather than from memory:
    /// `Hud.extractRenderState` (vanilla's own hud rendering, `.cache/mc/26.2/client-src`)
    /// calls `extractCrosshair` whenever the HUD itself is not F1-hidden and the
    /// active screen is not a `LevelLoadingScreen` — there is no
    /// `screen() == null` guard on this call, unlike the sibling
    /// `extractSubtitleOverlay` three lines below it, which does gate on
    /// `screen() == null || screen().isInGameUi()`. And `extractCrosshair` itself
    /// gates only on `options.getCameraType().isFirstPerson()`
    /// and not being in spectator mode (or, in spectator, aiming at a
    /// `MenuProvider` via `canRenderCrosshairForSpectator`). So a vanilla
    /// crosshair stays visible — dimmed only by whatever the screen itself draws
    /// on top of it afterward — behind a pause menu, an inventory, or chat.
    ///
    /// We hide it outright instead, a confirmed divergence. The draw-order half
    /// vanilla relies on already exists on this side, just not wired to the
    /// crosshair: `container.rs`'s dim gradient (leftover) draws
    /// *after* the HUD pass and paints over it uniformly, which is exactly what
    /// dims [`Self::hotbar`] for free while a container is open. Matching
    /// vanilla for the crosshair is therefore a gating change in `app.rs`
    /// (`crosshair = self.ui.is_playing()` would need to become something
    /// shaped like [`Self::hotbar`]'s `world_hud`), not a rendering one — but
    /// doing that correctly also needs vanilla's `isFirstPerson()` / spectator /
    /// `canRenderCrosshairForSpectator` gate folded in, or a third-person or
    /// spectator session would grow a crosshair vanilla never draws there. That
    /// is a distinct, larger change than this issue asked for ("settle whether
    /// vanilla hides the crosshair behind a screen", not "make it pixel-exact"),
    /// so behaviour is left as-is; this comment is the settled answer plus the
    /// pointer for whoever picks up the rest.
    ///
    /// **This flag is about the crosshair and nothing else.** It used to double as
    /// the hotbar's gate — one boolean answering two questions — which is exactly
    /// how the hotbar came to vanish behind the pause menu. See
    /// [`Self::hotbar`].
    pub crosshair: bool,
    /// Recent chat lines, oldest-first; drawn bottom-left. Each is a legacy
    /// `§`-code string paired with its **age in seconds**, which drives the
    /// vanilla fade-out (older lines dim, then vanish, while the box is closed).
    ///
    /// **Lossy: a `TextColor::Rgb` cannot survive this field.** `§` codes name
    /// only the sixteen legacy colours (`TextColor::legacy_code()` returns
    /// `None` for a hex colour), so any caller that reaches this field via
    /// `Text::to_legacy_string` has already dropped every hex colour a 1.16+
    /// server sent — that flattening happens one layer up, in
    /// `lodestone_game::chat::ChatLog::recent`/`recent_ages`. Prefer
    /// [`Self::chat_spans`]; this field exists for callers that have not been
    /// switched to the span-carrying accessor yet.
    pub chat: &'a [(&'a str, f32)],
    /// The hex-carrying sibling of [`Self::chat`]: the same oldest-first,
    /// age-paired chat lines, but as [`TextSpan`] runs from
    /// `lodestone_game::chat::ChatLog::recent_spans`/`recent_ages_spans`
    /// instead of a `§`-flattened `String`.
    ///
    /// **When this is non-empty, the draw uses it and ignores [`Self::chat`]
    /// entirely** — the two are alternatives, not layers to composite, so a
    /// caller populating both would just be paying to flatten the same `Text`
    /// twice. Empty (the default, see [`HudFrame::new`]) falls back to
    /// [`Self::chat`], which is what every caller not yet switched over does.
    ///
    /// This is the still-open half of `docs/text-colour.md`'s "Chat is still
    /// hex-blind" section: `ChatLog` itself has carried a span-returning read
    /// path for a while, but nothing between the session layer and this frame
    /// threaded it through. Wiring a caller in means switching
    /// `Session::recent_chat` (`crates/lodestone-shell/src/sim/session.rs`) to
    /// call `recent_ages_spans` instead of `recent_ages`, and `app/redraw.rs`
    /// to fill this field instead of [`Self::chat`].
    pub chat_spans: &'a [(&'a [TextSpan], f32)],
    /// Each [`Self::chat_spans`] row's message trust, for the indicator bar —
    /// `None` where the row is a system message (no signature, so no verdict),
    /// `Some` for a player message.
    ///
    /// A **parallel** slice rather than a third tuple element on
    /// `chat_spans`, so every existing caller and every test asserting on that
    /// tuple's shape is untouched. It is filled by the same slice-and-window
    /// step in `app/redraw.rs` that builds `chat_spans`, and is therefore
    /// index-aligned with it — but a consumer must still index defensively:
    /// empty (the default, see [`HudFrame::new`]) is the honest reading for
    /// every caller not yet wired, and drawing a badge for a row this slice
    /// does not cover would be inventing a verdict.
    pub chat_trust: &'a [Option<lodestone_game::chat::MessageTrust>],
    /// Sound-subtitle captions, **oldest first** — vanilla's own
    /// order, with row 0 at the bottom of the stack. Drawn bottom-right, above the
    /// hotbar, and empty whenever `showSubtitles` is off or nothing is audible.
    pub sound_subtitles: &'a [crate::audio::subtitles::SubtitleCaption],
    /// The in-progress chat input line, `Some` only while the chat box is open.
    pub chat_input: Option<&'a str>,
    /// The selected character range in [`Self::chat_input`], if any. It is
    /// ordered by [`crate::chat::ChatInput::selection`] and clipped again by
    /// the renderer to its text and input-strip bounds.
    pub chat_selection: Option<(usize, usize)>,
    /// The caret's position in [`Self::chat_input`], as a **`char`** index —
    /// vanilla's `EditBox.cursorPos`. `None` means "at the end of the line",
    /// which is what every caller that predates a movable caret meant and what
    /// [`HudFrame::new`] defaults to.
    ///
    /// This exists because the draw needs it and had no way to ask: the caret
    /// used to be placed at `text_width(whole line)` and drawn as an `_`
    /// unconditionally, so `ChatInput`'s Left/Right really did move the
    /// insertion point while the indicator stayed pinned to the end. Both
    /// halves of vanilla's rule — where the caret sits, and whether it is the
    /// 1 px insert bar or the appended underscore
    /// (`TextCursorUtils.extractInsertCursor` vs `extractAppendCursor`) — read
    /// this one field.
    pub chat_cursor: Option<usize>,
    /// Whether the input line's blinking caret is in its "on" phase this
    /// frame; only meaningful while `chat_input` is `Some`. Which *shape* it
    /// blinks in is [`Self::chat_cursor`]'s business, not this flag's. Vanilla
    /// blinks it every 300ms (its own text-cursor blink interval,
    /// `isCursorVisible(millis) == (millis / 300) % 2 == 0`) — the caller
    /// computes this from a wall clock with that same formula so this pure
    /// geometry module owns no clock of its own. Defaults to always-visible
    /// (see [`HudFrame::new`]) so every pre-existing test keeps drawing a
    /// caret without having to know about blinking.
    pub chat_caret_visible: bool,
    /// The grey preview of the highlighted suggestion, drawn straight after the
    /// caret — vanilla's `EditBox.suggestion`, set by
    /// `SuggestionsList.select`. `None` whenever there is no popup, or the
    /// highlighted candidate is not an extension of what is typed.
    pub chat_suggestion_ghost: Option<&'a str>,
    /// The command-suggestion dropdown, `Some` only while it is up. See
    /// [`SuggestionPopup`] and [`draw_command_suggestions`].
    pub chat_suggestions: Option<SuggestionPopup<'a>>,
    /// A `hover_event` tooltip under the cursor, `Some` only while a
    /// hover-carrying span is under it. [`chat_interaction_at`] finds the
    /// same span `WindowApp::chat_interaction` dispatches clicks from; this
    /// is its hover half finally reaching a draw — see `docs/chat.md`'s
    /// "Interactivity" section for why that used to stop one hop short of
    /// pixels.
    pub chat_hover_tooltip: Option<ChatHoverTooltip<'a>>,
    /// The scrollback's scroll indicator — vanilla's own scrollbar
    /// (`ChatComponent.extractRenderState`'s `if (total > 0 && isForeground)`
    /// block), `Some` only while there is [`crate::chat::ChatScroll`] state to
    /// show. `None` draws nothing, matching vanilla's own
    /// `virtualHeight != chatHeight` gate for "nothing to scroll into".
    pub chat_scrollbar: Option<ChatScrollbar>,
    /// The persisted Chat Settings values that shape the scrollback/input
    /// draw — see [`ChatDisplayOptions`]. Defaults to vanilla's own defaults
    /// (see [`HudFrame::new`]), so a caller that never sets this renders
    /// exactly as the fields alone would suggest, not as some other implicit
    /// baseline.
    pub chat_options: ChatDisplayOptions,
    /// Where the chat log's wrapped rows are persisted between frames — see
    /// [`ChatWrapCache`]. `None` (every hermetic test, and any caller with no
    /// frame-to-frame state) wraps from scratch, which is correct, just not
    /// free; the running app always supplies one.
    pub chat_wrap: Option<&'a ChatWrapCache>,
    /// The [`Self::chat_spans`] sibling of [`Self::chat_wrap`] — see
    /// [`ChatWrapCacheSpans`]. `None` wraps from scratch every frame, exactly
    /// like `chat_wrap`'s own `None` case.
    pub chat_wrap_spans: Option<&'a ChatWrapCacheSpans>,
    /// The player list to draw, `Some` only while the tab overlay is held.
    ///
    /// **This used to be `Option<&[String]>`** — a flat list of pre-formatted
    /// `"NAME  30ms"` rows — and that was the defect, not the plumbing:
    /// `PLAYER_INFO_UPDATE` was decoded, folded, and reaching pixels the whole
    /// time, so `cargo xtask connectedness` reported the wire green before and
    /// after. What the flattening threw away was the game mode, the styled
    /// display name and the latency *band*, and what it invented was a
    /// `"PLAYERS (n)"` caption vanilla has no equivalent of. Carry
    /// [`crate::tablist::TabListView`] and let the draw do vanilla's layout.
    pub players: Option<&'a crate::tablist::TabListView>,
    /// The top-right status-effect icons — `Hud.extractEffects`' own list,
    /// already sorted and filtered by [`crate::effects::hud_icons`].
    ///
    /// Empty and `None` are the same picture here, but they are not the same
    /// *claim*: `None` is "this frame must not draw the overlay", which is
    /// vanilla's `screen() == null || !screen().showsActiveEffects()` gate,
    /// and the caller owns that decision. The draw takes the list as data and
    /// has no opinion about which screen is open — the same split
    /// `container::geometry::draw_effect_column` already uses for the
    /// inventory column, so the two surfaces can never both paint.
    pub effects: Option<&'a [crate::effects::HudEffectIcon]>,
    /// The scoreboard sidebar to draw on the right edge, `Some` when displayed.
    pub sidebar: Option<&'a Sidebar>,
    /// Active boss bars, drawn stacked at the top-centre in render order.
    pub boss_bars: &'a [BossBarView],
    /// Whether this player can be hurt at all — vanilla's
    /// own client-side can-hurt-player check, which is `localPlayerMode.isSurvival()`
    /// and therefore `SURVIVAL || ADVENTURE`. Creative **and spectator** are both
    /// false, which is why this is not a `GameMode::Creative` test: naming the mode
    /// would leave a spectator with a heart row vanilla never draws.
    ///
    /// `Hud.extractHotbarAndDecorations` calls `extractPlayerHealth` only under this
    /// predicate, and that one call draws the *whole* left/right column — the armour
    /// bar, the hearts, the hunger row and the air bubbles. So one flag gates all
    /// four here too, and all four are now present: [`Self::armour`] joined this gate
    /// rather than getting one of its own, which is what this field's own note asked
    /// for while it was the missing fifth of five.
    ///
    /// It also stands in for vanilla's `hasExperience()`, which gates the XP bar and
    /// the level number through `nextContextualInfoState`: in 26.2 both methods have
    /// the identical body (`localPlayerMode.isSurvival()`), so one boolean carries
    /// both. **Split this into two fields if they ever diverge upstream** — the
    /// questions are genuinely different even where today's answers are not.
    ///
    /// Finally it supplies the signal `held_item`'s 14 px shift needed
    /// (`extractSelectedItemName`'s `y += 14` when `!canHurtPlayer()`), because with
    /// no vitals row below it the label drops into the space they vacated.
    ///
    /// Defaults to `true`, so every caller that predates this field — and every
    /// hermetic test that sets `health`/`food`/`xp` directly — draws exactly as it
    /// did before.
    pub can_hurt_player: bool,
    /// Current player health in `0..=1024`, `Some` only on a live survival server.
    /// A player without a dynamic health attribute still has the normal 20-point
    /// ceiling.
    pub health: Option<f32>,
    /// Server-reported `minecraft:max_health`, if the local attribute snapshot
    /// has named it. `None` is the normal 20-point ceiling before an attribute
    /// packet, so legacy/offline HUDs retain their single row.
    pub max_health: Option<f32>,
    /// Armour points in `0..=20` — vanilla's own get-armor-value accessor, which
    /// is `Mth.floor(getAttributeValue(Attributes.ARMOR))` and **not** a per-item
    /// table. `Some` once the local player carries a server-fed attribute snapshot;
    /// `None` off a live server, which draws nothing.
    ///
    /// `Some(0)` is a real state — a live player wearing nothing — and also draws
    /// nothing, because `extractArmor` is wrapped in `if (armor > 0)`: vanilla shows
    /// **no** row at all rather than ten empty icons. Both cases therefore agree, and
    /// the `Option` exists only so a caller that has not wired the attribute through
    /// is distinguishable from one reporting a real zero.
    ///
    /// The scale is 20 points = 10 icons, same as hearts, but the units are armour
    /// points rather than half-hearts: full diamond is exactly 20 and the registry
    /// clamps `minecraft:armor` to `0..=30`, so a value above 20 saturates the row at
    /// ten full icons rather than drawing an eleventh.
    pub armour: Option<i32>,
    /// Current food level in `0..=20`, `Some` only on a live survival server.
    pub food: Option<i32>,
    /// Current food saturation (the hidden reserve that drains before `food`
    /// itself does), `Some` only on a live survival server. Drives the
    /// hunger-row wobble while it is empty (vanilla's own hud rendering,
    /// `getSaturationLevel() <= 0.0`) — `None` is treated as "not empty" (the
    /// row stays flush), which is also this field's default, so a caller that
    /// has not wired it through yet (see `docs/hud-animations.md`) draws
    /// exactly as before this field existed rather than guessing at a real
    /// saturation value.
    pub saturation: Option<f32>,
    /// `(air, max_air, eye_in_water)`, `Some` only on a live survival server.
    /// Drives the underwater bubble row (`lodestone_render::bubble_row`) —
    /// `Some` does not by itself mean the row draws; [`bubble_row_visible`]
    /// (full air and not underwater draws nothing, matching vanilla's own
    /// guard) decides that per-frame, same as vanilla never showing bubbles at
    /// full air on dry land.
    ///
    /// [`bubble_row_visible`]: lodestone_render::bubble_row_visible
    pub air: Option<(i32, i32, bool)>,
    /// The selected hotbar slot in `0..9`, `Some` whenever a **world** is on
    /// screen — including behind the pause menu, the chat box and a container.
    /// Drawn as a 9-cell bar at the bottom centre with the selected cell
    /// highlighted.
    ///
    /// This used to say "`Some` while in active play", and the call site agreed
    /// with it, so the hotbar disappeared the moment any screen opened.
    /// Vanilla draws the hotbar under a "ready for level rendering" flag
    /// and gates it on game
    /// mode only; the *screen* then paints its translucent
    /// background over the top, and that is the whole of the difference. See
    /// `app::hud_follows_world`, and [`Self::crosshair`] for the one element we
    /// do deliberately hide.
    pub hotbar: Option<usize>,
    /// The paginated spectator selector, independent of the inventory hotbar.
    pub spectator_hotbar: Option<&'a crate::menu::spectator_menu::SpectatorHotbarView>,
    /// The nine hotbar item stacks (`0..9`), `Some` on a live server once the
    /// player inventory has been folded. Each slot is `Some(HotbarSlot)` when
    /// occupied. Icons are drawn from the [`ItemAtlas`] supplied to
    /// [`HudRenderer::attach_items`]; without that atlas the wells stay empty.
    pub hotbar_items: Option<&'a [Option<HotbarSlot>]>,
    /// Remaining server item-use cooldown fraction for each hotbar slot. This
    /// is index-aligned with [`Self::hotbar_items`]: `0.0` leaves the icon
    /// untouched and a positive value draws a dark veil from the bottom up.
    /// The app resolves it from the session cooldown groups once per frame;
    /// the HUD only draws the supplied projection.
    pub hotbar_cooldowns: &'a [f32],
    /// The XP bar `(level, progress 0..=1)`, `Some` once the server has sent
    /// experience. Drawn as a green progress bar above the hotbar with the level
    /// centred above it. Off a live server this is `None` — no bar is drawn.
    pub xp: Option<(i32, f32)>,
    /// The locator bar's dots, already resolved to a screen
    /// offset and colour by [`locator::locator_dots`] — empty when the
    /// local player is tracking no waypoint, which is also
    /// [`HudFrame::new`]'s default. Occupies the same on-screen slot as
    /// [`Self::xp`] (`ContextualBar` is one mutually-exclusive bar in
    /// vanilla); a non-empty [`Self::locator`] draws instead of the XP bar
    /// rather than alongside it — see `sprite_vitals`'s own doc for the
    /// priority order this build models (and the one term of vanilla's it
    /// does not).
    pub locator: &'a [locator::LocatorDot],
    /// The title/subtitle overlay `(title, subtitle, alpha)`, drawn large and
    /// centred with a server-driven fade. `None` when no title is showing.
    ///
    /// Both strings are **styled spans**, and the reason is the same one
    /// [`crate::overlay::Sidebar`] carries: `Sim::title_overlay` used to flatten
    /// through `Text::to_legacy_string`, which can express only the sixteen colours
    /// that have a `§` code. A server's hex title therefore arrived here white, and
    /// no amount of work in the *renderer* could recover it — the loss was one
    /// layer above. Spans keep [`TextColor`] itself, so hex survives to the quad.
    pub title: Option<(Vec<TextSpan>, Option<Vec<TextSpan>>, f32)>,
    /// The action-bar message `(text, alpha)`, drawn just above the hotbar
    /// cluster with a fade. `None` when nothing is showing. Spans rather than a
    /// `String` for the reason [`Self::title`] gives.
    pub action_bar: Option<(Vec<TextSpan>, f32)>,
    /// The held-item name highlight `(name, alpha)` — a `§`-coded, already-
    /// styled string (see `lodestone_game::item::styled_hover_name`) and the
    /// opacity from `lodestone_game::player_state::HeldItemHighlight::alpha`.
    /// `None`/`alpha <= 0.0` draws nothing. Unlike [`Self::action_bar`] and
    /// [`Self::title`], the *timer* this alpha comes from is not
    /// server-driven — it is a purely client-side reaction to the selected
    /// hotbar item's identity changing, so a caller populates
    /// this from whatever owns that timer each frame rather than from a
    /// decoded packet.
    ///
    /// Vanilla shifts this label down 14 px when `!canHurtPlayer()`
    /// (creative/spectator have no health/hunger row for it to clear); the signal is
    /// [`Self::can_hurt_player`] and the draw site is in
    /// [`HudGeometry::build_inner`].
    pub held_item: Option<(String, f32)>,
    /// The [`Self::held_item`] sibling that keeps [`TextColor::Rgb`]: spans
    /// from `lodestone_game::item::styled_hover_name_spans` rather than
    /// `styled_hover_name`'s `§`-coded `String`, for the reason [`Self::title`]
    /// gives — `to_legacy_string` has no representation for a hex colour, so a
    /// custom item name naming one arrived here flattened to white. When
    /// non-empty this is drawn **instead of** `held_item`, not composited with
    /// it, matching [`Self::chat_spans`]'s own convention.
    pub held_item_spans: Option<(Vec<TextSpan>, f32)>,
    /// `(recipes, tags)` loaded into the local recipe corpus (see
    /// `crate::resources::load_recipe_book`), appended to the debug overlay as
    /// one extra line when `Some`. `None` before the corpus has loaded or on a
    /// jar-less run — the line is omitted rather than showing a misleading
    /// `0 0`, the same convention [`Self::hotbar_items`] uses for "not yet
    /// known" versus "known empty".
    pub recipe_stats: Option<(usize, usize)>,
    /// `(distance_to_border, warning_distance, warning_strength)` for the
    /// folded world border, appended to the debug overlay as one extra line
    /// when `Some`.
    ///
    /// `None` until the server has actually sent a border packet
    /// (`WorldBorder::initialized`), so an unbounded default border omits the
    /// line rather than drawing a meaningless `2.999e7` — the same
    /// "omit rather than mislead" convention [`Self::recipe_stats`] uses.
    ///
    /// **This is a diagnostic, not the real consumer.** Vanilla's border
    /// warning is a blue tint applied to the vignette in
    /// `Hud.extractVignette`, which needs a
    /// multiply-blend `RenderPipelines.VIGNETTE` equivalent and
    /// `misc/vignette.png` — neither of which exists in `lodestone-render`
    /// yet. This line is the same "did the datum actually reach the running
    /// client" signal `recipe_stats` plays for the corpus loader, and the
    /// strength it prints is the *exact* value that overlay will consume.
    pub border_debug: Option<(f64, f64, f32)>,
    /// The player's spawn point, appended to the debug overlay as one extra line
    /// when the server has reported one.
    ///
    /// `None` when `SpawnPoint::is_reported()` is false, which is the honest
    /// distinction the compass needs too — see
    /// [`Sim::spawn_point`](crate::sim::Sim::spawn_point).
    pub spawn_debug: Option<lodestone_model::BlockPos>,
    /// `(map count, the lowest-numbered map's explored fraction)` from
    /// `SessionMaps`, for the F3 overlay.
    ///
    /// **This is the fold's only reader today, and it is deliberately a
    /// diagnostic rather than the map's own picture.** `MAP_ITEM_DATA` decodes
    /// and `MapStore` blits its sub-rectangle patches correctly; what is missing
    /// is the *renderer* — a per-map 128x128 dynamic texture plus the held/framed
    /// quad, which is a texture-and-bind-group job of its own (see
    /// `docs/filled-map-item.md`). Same shape as [`Self::border_debug`] and
    /// [`Self::spawn_debug`], and for the same reason: a fold with no reader at
    /// all cannot be told apart from a fold that never runs.
    pub map_debug: Option<(usize, f32)>,
    /// The attack-cooldown fraction (`0.0..=1.0`, full strength at `1.0`) the
    /// crosshair indicator fills to — `Sim::attack_strength_scale`'s value,
    /// vanilla's `getAttackStrengthScale(0.0F)`. Drawn only while
    /// [`Self::crosshair`] is also set (see that field), and only once the
    /// atlas resolves the two indicator sprites — see the crosshair draw site
    /// in [`HudGeometry::build_inner`] for exactly which vanilla condition is
    /// and is not modelled (no hotbar-style variant, no full-charge "ready"
    /// icon; `docs/combat.md` names the cut). `None` draws nothing, the
    /// pre-#121 behaviour.
    pub attack_cooldown: Option<f32>,
    /// Which of vanilla's three `AttackIndicatorStatus` placements the
    /// attack-strength value from [`Self::attack_cooldown`] is drawn in —
    /// `options.attackIndicator`, copied here per frame by `app/redraw.rs`.
    ///
    /// Both draw sites gate on this value, while `Hotbar` is a *different* draw
    /// (an 18x18 gauge filling bottom-up beside the hotbar), not the same bar
    /// moved.
    pub attack_indicator: crate::config::AttackIndicator,
    /// The recipe-unlock toast to draw top-right, `Some` only while
    /// [`lodestone_game::recipe::RecipeToastQueue`] has a live entry. See
    /// [`RecipeToastView`] for the geometry and reference-layout notes.
    ///
    /// The queue receives entries only from the `recipe_book_add` decode. Until
    /// that packet path supplies an entry, live sessions keep this field
    /// `None`; the consumer is ready for real entries and does not synthesize
    /// them.
    pub recipe_toast: Option<RecipeToastView>,
    /// The advancement-completion toast, `Some` while one is inside
    /// its 5000 ms window. Drawn in the same top-right slot as
    /// [`Self::recipe_toast`] — vanilla's own toast manager stacks them, and this
    /// client only ever has one queue live at a time.
    pub advancement_toast: Option<AdvancementToastView>,
    /// A Friends-service notification. The app schedules it only while the
    /// recipe and advancement queues do not own the shared top-right slot.
    pub friends_toast: Option<FriendsToastView>,
}

impl<'a> HudFrame<'a> {
    /// Apply game-mode visibility after gathering this frame's live HUD payloads.
    pub fn apply_game_mode(&mut self, mode: Option<lodestone_model::GameMode>) {
        self.can_hurt_player = can_hurt_player(mode);
        if mode == Some(lodestone_model::GameMode::Spectator) {
            self.crosshair = false;
            self.hotbar = None;
            self.hotbar_items = None;
            self.hotbar_cooldowns = &[];
            self.held_item = None;
            self.held_item_spans = None;
        } else {
            self.spectator_hotbar = None;
        }
    }

    /// A frame that draws just the debug overlay and crosshair — the default
    /// single-player / pre-connect HUD, and a concise base for tests.
    #[must_use]
    pub fn new(stats: &'a DebugStats) -> Self {
        Self {
            stats,
            show_debug: true,
            crosshair: true,
            chat: &[],
            chat_spans: &[],
            chat_trust: &[],
            sound_subtitles: &[],
            chat_input: None,
            chat_selection: None,
            chat_cursor: None,
            chat_caret_visible: true,
            chat_suggestion_ghost: None,
            chat_suggestions: None,
            chat_hover_tooltip: None,
            chat_scrollbar: None,
            chat_options: ChatDisplayOptions::default(),
            chat_wrap: None,
            chat_wrap_spans: None,
            players: None,
            effects: None,
            sidebar: None,
            boss_bars: &[],
            can_hurt_player: true,
            health: None,
            max_health: None,
            armour: None,
            food: None,
            saturation: None,
            air: None,
            hotbar: None,
            spectator_hotbar: None,
            hotbar_items: None,
            hotbar_cooldowns: &[],
            xp: None,
            locator: &[],
            title: None,
            action_bar: None,
            held_item: None,
            held_item_spans: None,
            recipe_stats: None,
            border_debug: None,
            spawn_debug: None,
            map_debug: None,
            attack_cooldown: None,
            // Vanilla's own default, and the behaviour every build before this
            // field had: the strength bar under the crosshair.
            attack_indicator: crate::config::AttackIndicator::Crosshair,
            recipe_toast: None,
            advancement_toast: None,
            friends_toast: None,
        }
    }
}
