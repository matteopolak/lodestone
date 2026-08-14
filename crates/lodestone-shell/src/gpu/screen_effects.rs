//! Per-frame input to [`super::RenderState`]'s underwater/fire overlay pass
//! — see `docs/screen-overlays.md`.
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
    /// The local player's on-fire shared-entity-flag bit. **Closed, issue
    /// That fix** — see `docs/screen-overlays.md`'s "The on-fire flag's route to
    /// the shell" section: the byte decodes in `metadata.rs`, and
    /// `lodestone-ecs/src/ingest.rs::apply_local_player_on_fire` is a
    /// session-scoped fold (the local player is deliberately excluded from
    /// the generic entity-view path, so the generic `EntityFlags` component
    /// alone can never reach it) into `Vitals::on_fire`, read by `app.rs` off
    /// `PlayerSnapshot::on_fire`. Drives the fire overlay.
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
    /// That fix). Vanilla derives this generically from *any* equipped item's
    /// `minecraft:equippable.camera_overlay` component
    /// (`Hud.extractCameraOverlays`,
    /// `.cache/mc/26.2/client-src/net/minecraft/client/gui/Hud.java`)
    /// — carved pumpkin is simply the only item that currently ships with the
    /// field set, so this is named for the one concrete case rather than
    /// modelling the general per-item lookup table that has exactly one
    /// entry today.
    pub wearing_pumpkin: bool,
    /// Vanilla's `Entity.getPercentFrozen()`, `0.0..=1.0` —
    /// drives the freeze overlay's alpha. `0.0` (the default) means "not
    /// freezing", which is also the honest value while no live producer feeds
    /// this yet (`ScreenEffects` doc's own construction-site pattern; see
    /// `docs/screen-overlays.md`). **Unlike the four fields above, this is
    /// *not* first-person-gated** — see [`Self::any_active`]'s doc.
    pub freeze_percent: f32,
    /// Whether the local player is scoping with a held spyglass
    /// — vanilla's `Player.isScoping()`:
    /// `isUsingItem() && getUseItem().is(Items.SPYGLASS)`
    /// (`Player.java`). First-person-gated, like
    /// [`Self::wearing_pumpkin`] (both live inside `Hud.
    /// extractCameraOverlays`'s `if (getCameraType().isFirstPerson())`
    /// block, `Hud.java`) — unlike freeze/nausea/portal below.
    pub scoping: bool,
    /// Vanilla's `LivingEntity.getEffectBlendFactor(MobEffects.NAUSEA,
    /// partialTicks)`, `0.0..=1.0` — drives the confusion
    /// overlay's strength and (blended with [`Self::portal_intensity`]) the
    /// world-projection "spinning" warp (`Camera::view_projection_warped`).
    /// `0.0` (the default) is the honest value today: no potion-effect
    /// duration tracker exists anywhere in this codebase yet to feed it — see
    /// `docs/screen-overlays.md`'s "what does not reach the shell yet"
    /// section for that fix. **Not first-person-gated** — see
    /// [`Self::any_active`]'s doc.
    pub nausea_intensity: f32,
    /// Vanilla's `Entity.portalEffectIntensity`, `0.0..=1.0` —
    /// drives the portal overlay's alpha and (blended with
    /// [`Self::nausea_intensity`]) the same projection warp. Takes priority
    /// over nausea when both are positive
    /// (`Hud.java`: `if (portalIntensity > 0.0F) { portal } else if
    /// (nauseaIntensity > 0.0F) { confusion }`) — `RenderState::render_inner`
    /// reproduces that `if`/`else if`, not an independent pair of checks.
    /// `0.0` (the default) is the honest value today: no nether-portal
    /// proximity tracker exists in this codebase yet — see
    /// `docs/screen-overlays.md`. **Not first-person-gated.**
    pub portal_intensity: f32,
}

impl ScreenEffects {
    /// Whether **any** overlay should draw this frame — the outer
    /// short-circuit `RenderState::render_inner` checks before opening any
    /// pass. Two genuinely different gates fold into this one bool, matching
    /// a real split in vanilla's own source rather than a simplification:
    ///
    /// - [`Self::eye_in_water`]/[`Self::on_fire`]/[`Self::wearing_pumpkin`]/
    ///   [`Self::scoping`] are **first-person-only**. Underwater/fire come
    ///   from `ScreenEffectRenderer.submit`'s own `isFirstPerson &&
    ///   !isSleeping && !isSpectator` (this crate has no "sleeping" concept,
    ///   so that conjunct is omitted — never a false negative, since an
    ///   unmodelled state cannot suppress a draw it never influences);
    ///   pumpkin/scoping come from `Hud.extractCameraOverlays`'s own nested
    ///   `if (getCameraType().isFirstPerson())` block (`Hud.java`).
    /// - [`Self::freeze_percent`]/[`Self::nausea_intensity`]/
    ///   [`Self::portal_intensity`] are **not** — vanilla draws
    ///   `player.getTicksFrozen() > 0` (`Hud.java`) and the
    ///   portal/confusion overlays (`Hud.java`) as *siblings* of the
    ///   `if (isFirstPerson())` block, not nested inside it, so they paint in
    ///   third person too. Checked against the jar directly, not assumed —
    ///   see `docs/screen-overlays.md`.
    ///
    /// `spectator` still gates both groups: vanilla's own `Hud.java` has no
    /// explicit spectator check anywhere in `extractCameraOverlays`, but this
    /// codebase's established convention (already applied to underwater/fire/
    /// pumpkin before this) is "nothing about my own body renders in
    /// spectator", and nothing here has reason to be the first exception.
    #[must_use]
    pub fn any_active(&self, first_person: bool) -> bool {
        self.first_person_group_active(first_person) || self.camera_agnostic_group_active()
    }

    /// The first-person-gated group — see [`Self::any_active`]'s doc.
    #[must_use]
    pub fn first_person_group_active(&self, first_person: bool) -> bool {
        first_person
            && !self.spectator
            && (self.eye_in_water || self.on_fire || self.wearing_pumpkin || self.scoping)
    }

    /// The camera-agnostic group — see [`Self::any_active`]'s doc. Still
    /// gated on `!spectator` (the codebase convention, not a vanilla literal
    /// — see that doc).
    #[must_use]
    pub fn camera_agnostic_group_active(&self) -> bool {
        !self.spectator && (self.freeze_percent > 0.0 || self.nausea_intensity > 0.0 || self.portal_intensity > 0.0)
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
            ..ScreenEffects::default()
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
            ..ScreenEffects::default()
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

    #[test]
    fn first_person_scoping_activates() {
        let fx = ScreenEffects {
            scoping: true,
            ..ScreenEffects::default()
        };
        assert!(fx.any_active(true));
    }

    #[test]
    fn third_person_suppresses_scoping() {
        let fx = ScreenEffects {
            scoping: true,
            ..ScreenEffects::default()
        };
        assert!(!fx.any_active(false));
    }

    #[test]
    fn spectator_suppresses_scoping() {
        let fx = ScreenEffects {
            scoping: true,
            spectator: true,
            ..ScreenEffects::default()
        };
        assert!(!fx.any_active(true));
    }

    /// The three-way distinction [`ScreenEffects::any_active`]'s doc asks
    /// for: freeze/nausea/portal must activate in *both* camera modes,
    /// unlike every field tested above.
    #[test]
    fn freeze_activates_in_first_and_third_person() {
        let fx = ScreenEffects {
            freeze_percent: 0.5,
            ..ScreenEffects::default()
        };
        assert!(fx.any_active(true), "freeze must activate in first person");
        assert!(fx.any_active(false), "freeze must activate in third person too -- Hud.java is a sibling of the isFirstPerson block, not nested in it");
    }

    #[test]
    fn nausea_activates_in_first_and_third_person() {
        let fx = ScreenEffects {
            nausea_intensity: 0.3,
            ..ScreenEffects::default()
        };
        assert!(fx.any_active(true));
        assert!(fx.any_active(false));
    }

    #[test]
    fn portal_activates_in_first_and_third_person() {
        let fx = ScreenEffects {
            portal_intensity: 0.3,
            ..ScreenEffects::default()
        };
        assert!(fx.any_active(true));
        assert!(fx.any_active(false));
    }

    #[test]
    fn zero_freeze_percent_does_not_activate() {
        let fx = ScreenEffects {
            freeze_percent: 0.0,
            ..ScreenEffects::default()
        };
        assert!(!fx.any_active(true));
        assert!(!fx.any_active(false));
    }

    #[test]
    fn spectator_suppresses_the_camera_agnostic_group_too() {
        let fx = ScreenEffects {
            freeze_percent: 1.0,
            nausea_intensity: 1.0,
            portal_intensity: 1.0,
            spectator: true,
            ..ScreenEffects::default()
        };
        assert!(!fx.any_active(true));
        assert!(!fx.any_active(false));
    }

    #[test]
    fn first_person_group_and_camera_agnostic_group_are_independently_queryable() {
        let fx = ScreenEffects {
            freeze_percent: 0.5,
            ..ScreenEffects::default()
        };
        assert!(!fx.first_person_group_active(true), "freeze is not in the first-person group");
        assert!(fx.camera_agnostic_group_active());

        let fx = ScreenEffects {
            wearing_pumpkin: true,
            ..ScreenEffects::default()
        };
        assert!(fx.first_person_group_active(true));
        assert!(!fx.camera_agnostic_group_active(), "pumpkin is not in the camera-agnostic group");
    }
}
