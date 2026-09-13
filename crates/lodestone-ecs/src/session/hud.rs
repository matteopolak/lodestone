use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{Component, Query, ResMut, With};
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::world::World;
use lodestone_model::{Identifier, ResolvedText};
use std::collections::HashMap;

use super::components::{SessionItemCooldowns, SessionMenus};
use crate::player::{LocalPlayer, SelectedSlot};
use crate::schedules::GameTick;
use crate::sets::TickSet;

// The driver half: components in the shell's `World`
// ---------------------------------------------------------------------------

/// The coarse phase of the client's session.
///
/// Moved here from `lodestone_shell::sim` (which re-exports it, so `app.rs` and
/// the live gates are unchanged) because it is session state like everything
/// else in this module, and because `crate::player::Egress` is derived from it.
///
/// Purely a read-model: it never affects physics or rendering directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPhase {
    /// No live connection — the offline fixture world.
    LocalOnly,
    /// A live connection is attached and still handshaking / logging in.
    Connecting,
    /// Logged in to the server.
    Connected,
    /// The session ended; carries why, and the styled text to show for it.
    /// Terminal until a new connection is attached.
    ///
    /// Boxed because [`SessionEnd`] owns a whole [`Text`] tree and every other
    /// variant is a unit — inlining it would make the common `Connected` phase
    /// pay for the terminal one.
    Ended(Box<SessionEnd>),
}

/// Why a session ended, and the text to show for it.
///
/// # The reason is a [`Text`], not a formatted string
///
/// It used to be `format!("disconnected: {reason}")`, and that `format!` was
/// where every kick message lost its colour: the styled component was flattened
/// to a `String` before any screen saw it, so nothing downstream *could* render
/// a span. The reason is now carried as the tree it arrived as, resolved (so
/// `multiplayer.disconnect.kicked` is already English) but not flattened.
///
/// # Nothing prefixes the reason
///
/// The `"disconnected: "` prefix was ours, and vanilla does not do it: a
/// `DisconnectedScreen` puts its `title` in its own `StringWidget` *above* the
/// reason's `MultiLineTextWidget`, never glued onto it. [`SessionEndKind`] is
/// what a screen derives that title from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEnd {
    /// Which of the two very different things happened.
    pub kind: SessionEndKind,
    /// The reason, resolved but still styled. For [`SessionEndKind::Disconnected`]
    /// this is the *server's* own component and belongs on screen verbatim; for
    /// [`SessionEndKind::Failed`] it is ours.
    ///
    /// [`ResolvedText`] and not [`Text`]: a kick reason is a `translate`
    /// component (`multiplayer.disconnect.kicked` and friends), and the end
    /// screen that draws it is the surface that shipped the raw key. Taking the
    /// resolved form means the language table has provably been consulted before
    /// the value gets this far.
    pub reason: ResolvedText,
}

impl SessionEnd {
    /// A server-sent disconnect carrying the server's own component.
    #[must_use]
    pub fn disconnected(reason: ResolvedText) -> Self {
        Self {
            kind: SessionEndKind::Disconnected,
            reason,
        }
    }

    /// A client-side failure carrying our own error text.
    #[must_use]
    pub fn failed(reason: ResolvedText) -> Self {
        Self {
            kind: SessionEndKind::Failed,
            reason,
        }
    }

    /// The local player died.
    #[must_use]
    pub fn died(reason: ResolvedText) -> Self {
        Self {
            kind: SessionEndKind::Died,
            reason,
        }
    }

    /// The reason with all styling dropped — for logs and for assertions that
    /// only care about the wording.
    #[must_use]
    pub fn plain(&self) -> String {
        self.reason.to_plain_string()
    }
}

/// The two — three, with death — genuinely different ways a session ends.
///
/// Conflating them is what made a client-side failure wear a server disconnect's
/// clothes: the adapter's real message was logged, the session then died of a
/// generic codec error, and the screen said `stream closed`. "The server kicked
/// you, here is its message" and "we failed to talk to the server, here is why"
/// need different titles and different logging, which is the whole reason this
/// is a separate field rather than something a screen guesses from the wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndKind {
    /// The server sent a disconnect packet. [`SessionEnd::reason`] is its
    /// component and is shown verbatim; vanilla's title for this is
    /// `disconnect.lost` ("Connection Lost").
    Disconnected,
    /// We could not talk to the server: a connect failure, a transport error, a
    /// codec error, a missing adapter. [`SessionEnd::reason`] is ours, and it
    /// should carry the error's cause chain because nothing else will.
    /// Vanilla's title for this is `connect.failed` ("Failed to connect to the
    /// server").
    Failed,
    /// The local player died. Not a connection failure at all, and the one case
    /// where the session is still perfectly healthy.
    Died,
}

/// The session phase, as a component on the local player.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct Phase(pub SessionPhase);

impl Default for Phase {
    fn default() -> Self {
        Self(SessionPhase::LocalOnly)
    }
}

/// The title/subtitle overlay and its vanilla fade timer.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct TitleOverlay(pub lodestone_game::player_state::TitleState);

/// The action-bar (GameInfo) overlay; self-clears after 60 ticks.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct ActionBarOverlay(pub lodestone_game::player_state::ActionBar);

/// The held-item name highlight: vanilla's `Hud.tick`
/// timer for the label that appears above the hotbar
/// when the selected item's *identity* changes. See
/// [`lodestone_game::player_state::HeldItemHighlight`]'s own doc for why this
/// is keyed on item id + hover name rather than slot index — switching
/// between two slots holding the same item must not retrigger it.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct HeldItemOverlay(pub lodestone_game::player_state::HeldItemHighlight);

/// The local player's active status effects for the HUD stack.
///
/// Distinct from `PhysicsState`'s `effects`, which is the *physics* view (only
/// motion-relevant effects, no durations). This is the full display set, and
/// the two are not a duplicate of each other: the physics one is an integrator
/// input, this one is a row of icons with countdowns.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct HudEffects(pub lodestone_game::effect::ActiveEffects);

/// Active effects keyed by server entity id for non-HUD consumers.
///
/// The local player's [`HudEffects`] remains the HUD's single source of truth.
/// This resource retains the same wire state for every entity, including the
/// local player, so effect-particle extraction can neither drop remote mobs
/// nor require a second packet-specific particle path.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct EntityStatusEffects(HashMap<i32, lodestone_game::effect::ActiveEffects>);

impl EntityStatusEffects {
    /// Applies or refreshes an effect for `entity_id`.
    pub fn apply(&mut self, entity_id: i32, effect: lodestone_game::effect::StatusEffect) {
        self.0.entry(entity_id).or_default().apply(effect);
    }

    /// Removes one effect, dropping an entity entry that became empty.
    pub fn remove(&mut self, entity_id: i32, id: &Identifier) {
        if let Some(effects) = self.0.get_mut(&entity_id) {
            effects.remove(id);
            if effects.is_empty() {
                self.0.remove(&entity_id);
            }
        }
    }

    /// Effects currently retained for `entity_id`.
    #[must_use]
    pub fn get(&self, entity_id: i32) -> Option<&lodestone_game::effect::ActiveEffects> {
        self.0.get(&entity_id)
    }

    /// Discards state for entities no longer retained by the live entity index.
    pub fn retain_entity_ids(&mut self, mut retain: impl FnMut(i32) -> bool) {
        self.0.retain(|id, _| retain(*id));
    }

    /// Removes all tracked entity effects at a session boundary.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Every entity's active effects, keyed by server entity id.
    pub fn iter(&self) -> impl Iterator<Item = (i32, &lodestone_game::effect::ActiveEffects)> {
        self.0.iter().map(|(id, effects)| (*id, effects))
    }
}

/// Respawns observed this session — the diagnostic the live death gate reads to
/// confirm the client actually recovered rather than merely never dying.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespawnCount(pub u64);

/// The received chat/system scrollback, with each line's arrival time.
///
/// # Why this arrives in Stage 5 and not Stage 3
///
/// Stage 3 moved every other session aggregate and deferred this one explicitly:
/// every push needs a monotonic client clock and every read needs it again to age
/// the line for the vanilla fade-out, so a component here while the clock stayed
/// a `Sim` field would have put a *second* clock in the process — the exact
/// failure the authority test exists to catch. The clock is now
/// [`crate::FrameClock`] and they moved together.
///
/// The driver's ingest stamps each line with `FrameClock::secs` and the HUD reads
/// `ChatLog::recent_ages(n, FrameClock::secs)`. Nothing in this crate reads the
/// clock for it: which frame a line belongs to is the driver's fact, not a fold's.
#[derive(Component, Debug, Clone, Default)]
pub struct SessionChat(pub lodestone_game::chat::ChatLog);

/// `TickSet::Animate`: age the three self-expiring HUD overlays one tick.
///
/// **Must stay in the fixed 20 Hz schedule.** Every duration here is counted in
/// *ticks* — vanilla's action bar is 60 ticks, a title's fade is `TitleTimes`
/// ticks, an effect's remaining duration is ticks — so ageing them per frame
/// makes each one frame-rate dependent: an action bar would vanish twice as
/// fast at 120 fps as at 60.
pub fn tick_hud_overlays(
    mut players: Query<
        (
            &mut TitleOverlay,
            &mut ActionBarOverlay,
            &mut HudEffects,
            &mut HeldItemOverlay,
            Option<&mut SessionItemCooldowns>,
            Option<&SelectedSlot>,
            Option<&SessionMenus>,
        ),
        With<LocalPlayer>,
    >,
) {
    for (mut title, mut action_bar, mut effects, mut held_item, cooldowns, selected_slot, menus) in &mut players
    {
        title.0.tick(1);
        action_bar.0.tick(1);
        effects.0.tick(1);
        if let Some(mut cooldowns) = cooldowns {
            cooldowns.0.tick();
        }
        // `SelectedSlot`/`SessionMenus` are `Option` here, not required
        // query terms: this module's own docs establish that
        // `SessionHudPlugin` and `SessionPlugin` are separate plugins a
        // harness can install independently (`SessionHudPlugin` alone, as
        // `a_game_tick_run_expires_the_action_bar` does), so a required term
        // would have silently stopped every other overlay in this same
        // system from ageing at all on such a harness — the query simply
        // would not have matched the entity.
        //
        // The selected hotbar stack's identity, resolved through the same
        // `Menus::player_native` this module's own doc names as the
        // "borrow-friendly counterpart... the HUD's held item" — reading the
        // native index directly rather than cloning a whole `Menu`.
        // Translation is `|_| None` here: no language table reaches this
        // crate, matching `styled_hover_name`'s own documented gap. Identity
        // is unaffected — the same untranslated key always resolves the same
        // way, so retrigger detection stays correct even though the drawn
        // text is the best-effort fallback rather than a localised string.
        let selected = selected_slot.zip(menus).and_then(|(slot, menus)| {
            menus
                .0
                .player_native(slot.0)
                .filter(|stack| !stack.is_empty())
        });
        let identity = selected.map(|stack| {
            let name = lodestone_game::item::styled_hover_name(stack, &|_| None);
            (stack.item().clone(), name)
        });
        held_item
            .0
            .tick(identity.as_ref().map(|(item, name)| (item, name.as_str())));
        // The span-carrying sibling (`HeldItemHighlight::set_spans`'s own
        // doc): same stack, same `&|_| None` translation gap, computed
        // alongside `tick`'s legacy string so a hex-coloured custom item name
        // reaches the draw site instead of being dropped by
        // `styled_hover_name`'s `to_legacy_string` flattening.
        held_item.0.set_spans(
            selected
                .map(|stack| lodestone_game::item::styled_hover_name_spans(stack, &|_| None))
                .unwrap_or_default(),
        );
    }
}

/// Ages the non-HUD effect set on the same 20 Hz clock as [`tick_hud_overlays`].
pub fn tick_entity_status_effects(mut effects: ResMut<EntityStatusEffects>) {
    for effects in effects.0.values_mut() {
        effects.tick(1);
    }
    effects.0.retain(|_, effects| !effects.is_empty());
}

/// Insert the driver-half session/HUD component set onto `entity`.
///
/// Called by both the spawn and the reset path in the driver, so a component
/// added here cannot be missed by a quit-to-title (the failure mode
/// `reset_local_player`'s docs warn about).
pub fn insert_hud_components(world: &mut World, entity: bevy_ecs::entity::Entity) {
    if let Ok(mut entity) = world.get_entity_mut(entity) {
        entity.insert((
            Phase::default(),
            TitleOverlay::default(),
            ActionBarOverlay::default(),
            HeldItemOverlay::default(),
            HudEffects::default(),
            RespawnCount::default(),
            // Stage 5. Reset with the rest rather than by hand in the driver's
            // teardown: the old `Sim::end_session` cleared `chat_log` on its own
            // line, which is exactly the shape that gets missed when a component
            // is added later.
            SessionChat::default(),
        ));
    }
}

/// Registers the driver half: [`tick_hud_overlays`] in [`TickSet::Animate`].
///
/// Separate from [`SessionPlugin`] because they belong to different `World`s
/// (see this module's docs). Adding both to one `App` is legal and is what the
/// §4.1 unification will do.
#[derive(Debug, Default)]
pub struct SessionHudPlugin;

impl Plugin for SessionHudPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.init_resource::<EntityStatusEffects>();
        app.add_systems(
            GameTick,
            (tick_hud_overlays, tick_entity_status_effects).in_set(TickSet::Animate),
        );
    }
}
