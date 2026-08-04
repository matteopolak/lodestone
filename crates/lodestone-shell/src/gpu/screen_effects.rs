//! Per-frame input to [`super::RenderState`]'s underwater/fire overlay pass
//! (issues #108, #112) — see `docs/screen-overlays.md`.
//!
//! Unlike [`super::EntityLightSource`]/[`super::SkyDarkenSource`] and friends,
//! this is **not** a source: those exist because `RenderState` has no way to
//! reach `Sim` or the network handle itself, so a caller installs a boxed
//! closure once and it is polled every frame. `eye_in_water`/`on_fire`/
//! `spectator` are already synchronously available wherever `app.rs` calls
//! [`super::RenderState::render`] (the same place `outline` and
//! `entity_draws` are computed), so this is a plain per-call argument —
//! exactly like `outline: Option<[i32; 3]>` — rather than a second
//! install-once seam for state that was never hard to reach.

/// Per-frame state the underwater/fire overlay pass needs, gathered by the
/// caller (`app.rs`) at each `render`/`render_with_crack` call — see the
/// module doc for why this is a plain argument rather than a `*Source`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScreenEffects {
    /// `PhysicsState::eye_in_water` — the same predicate the submerged fog and
    /// the air-bubble row already read (`docs/sky-and-air-bubbles.md`,
    /// `docs/fluid-classification.md`). Drives the underwater overlay.
    pub eye_in_water: bool,
    /// The local player's on-fire shared-entity-flag bit. **Always `false` in
    /// this build** — see `docs/screen-overlays.md`'s "what does not reach
    /// the shell yet" section: the byte decodes in `metadata.rs` and reaches
    /// a generic `EntityFlags` ECS component, but the local player is
    /// deliberately excluded from the generic entity-view path
    /// (`lodestone-ecs/src/ingest.rs`'s `apply_local_player_login` doc), and
    /// no session-scoped fold like `apply_local_player_air_supply` exists for
    /// it yet. Drives the fire overlay.
    pub on_fire: bool,
    /// Whether the local player is in spectator mode. Vanilla's
    /// `ScreenEffectRenderer.submit` skips both overlays entirely for a
    /// spectator (`!this.minecraft.player.isSpectator()`), matching a
    /// spectator's general "nothing about my own body renders" treatment.
    pub spectator: bool,
    /// The current game tick, for the fire overlay's animation frame
    /// (`tick % ScreenEffectRenderer::fire_frame_count`). The same tick
    /// already passed to `RenderState::update_animation` for the block atlas.
    pub tick: u64,
    /// Whether the local player's helmet slot holds a carved pumpkin (issue
    /// #185). Vanilla derives this generically from *any* equipped item's
    /// `minecraft:equippable.camera_overlay` component
    /// (`Hud.extractCameraOverlays`,
    /// `.cache/mc/26.2/client-src/net/minecraft/client/gui/Hud.java:269-291`)
    /// — carved pumpkin is simply the only item that currently ships with the
    /// field set, so this is named for the one concrete case rather than
    /// modelling the general per-item lookup table that has exactly one
    /// entry today.
    pub wearing_pumpkin: bool,
}

impl ScreenEffects {
    /// Whether any overlay should draw this frame, gated the way vanilla
    /// gates both underwater/fire (`isFirstPerson && !isSleeping &&
    /// !isSpectator`) — this crate has no "sleeping" concept yet, so that
    /// conjunct is omitted (never a false negative today: an unmodelled
    /// state cannot suppress a draw it never influences) — see
    /// `docs/screen-overlays.md`. The pumpkin overlay is folded into the same
    /// gate: vanilla's `extractCameraOverlays` is also first-person-only
    /// (`getCameraType().isFirstPerson()`), and this codebase's convention
    /// for spectator is "nothing about my own body renders" (the same reason
    /// `eye_in_water`/`on_fire` are gated on `!spectator` here even though
    /// vanilla's own `ScreenEffectRenderer.submit` is the one that spells out
    /// that particular conjunct).
    #[must_use]
    pub fn any_active(&self, first_person: bool) -> bool {
        first_person
            && !self.spectator
            && (self.eye_in_water || self.on_fire || self.wearing_pumpkin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_draws_nothing() {
        let fx = ScreenEffects::default();
        assert!(!fx.any_active(true));
    }

    #[test]
    fn spectator_suppresses_both_even_when_eye_in_water_and_on_fire() {
        let fx = ScreenEffects {
            eye_in_water: true,
            on_fire: true,
            spectator: true,
            tick: 0,
            wearing_pumpkin: false,
        };
        assert!(!fx.any_active(true));
    }

    #[test]
    fn spectator_suppresses_pumpkin_too() {
        let fx = ScreenEffects {
            wearing_pumpkin: true,
            spectator: true,
            ..ScreenEffects::default()
        };
        assert!(!fx.any_active(true));
    }

    #[test]
    fn third_person_suppresses_both() {
        let fx = ScreenEffects {
            eye_in_water: true,
            on_fire: true,
            spectator: false,
            tick: 0,
            wearing_pumpkin: false,
        };
        assert!(!fx.any_active(false));
    }

    #[test]
    fn third_person_suppresses_pumpkin_too() {
        let fx = ScreenEffects {
            wearing_pumpkin: true,
            ..ScreenEffects::default()
        };
        assert!(!fx.any_active(false));
    }

    #[test]
    fn first_person_wearing_pumpkin_activates() {
        let fx = ScreenEffects {
            wearing_pumpkin: true,
            ..ScreenEffects::default()
        };
        assert!(fx.any_active(true));
    }

    #[test]
    fn first_person_eye_in_water_activates() {
        let fx = ScreenEffects {
            eye_in_water: true,
            ..ScreenEffects::default()
        };
        assert!(fx.any_active(true));
    }

    #[test]
    fn first_person_on_fire_activates() {
        let fx = ScreenEffects {
            on_fire: true,
            ..ScreenEffects::default()
        };
        assert!(fx.any_active(true));
    }
}
