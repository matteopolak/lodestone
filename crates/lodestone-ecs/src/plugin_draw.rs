//! Extract-time custom draw-buffer API for client plugins (issue #161):
//! generalizes [`crate::player::DebugLines`]' precedent — the one working
//! instance of "a plugin pushes world-space geometry into a resource, the
//! renderer polls it" — into textured/billboard-style world-space draws, the
//! concrete building block a waypoint, hologram or minimap-overlay plugin
//! needs beyond plain debug lines.
//!
//! # What it is
//!
//! [`PluginBillboards`] is a per-frame resource of [`PluginBillboard`]s — a
//! world-space position, a size, a tint, and either
//! [`PluginTexture::Solid`] (a flat tinted quad, no texture — the same "no
//! icon atlas, tint a flat quad" simplification the status-effect HUD chips
//! and the beacon screen's power buttons already use) or
//! [`PluginTexture::Named`] (a texture the renderer's own atlas already
//! knows by id — no new naming authority, since a plugin already gets that
//! id space from `VersionAdapter::block_name`/`item_prototype`).
//!
//! # How it works
//!
//! Same shape as [`crate::player::DebugLines`], deliberately: a plugin
//! system ordered `.in_set(ExtractSet::Debug)` pushes into
//! `ResMut<PluginBillboards>`; [`clear_plugin_billboards`], ordered
//! `.before(ExtractSet::Debug)` (not `.in_set` it, for the identical reason
//! `clear_debug_lines` is — so it can never race a plugin's own writer for a
//! position within the set), empties the resource before that set runs each
//! frame, so a plugin only ever appends this frame's geometry. Reusing
//! `ExtractSet::Debug` rather than adding a sibling `SystemSet` variant is a
//! deliberate scope choice: the two channels are mechanically identical
//! (clear, then append, at the same point in `Extract`) and a new variant is
//! an ABI change (`docs/plugin-api.md`'s ordering-anchor changelog) that buys
//! nothing structural here — see that document's own section on this issue
//! for the full reasoning.
//!
//! # How to change it — the boundary this crate stops at
//!
//! **This crate carries no `wgpu::Device`, and does not resolve
//! [`PluginTexture::Named`] to an actual bound texture.** That is the render
//! half — `lodestone_shell::gpu::plugin_billboards`'s
//! `PluginBillboardRenderer`/`PluginBillboardsSource`, mirroring
//! `DebugLineRenderer`/`DebugLinesSource`, plus the wire
//! (`WindowApp::install_plugin_billboards_source`, mirroring
//! `install_debug_lines_source`) — is out of this crate's reach by design
//! (`docs/plugin-api.md`'s "what stays privileged" list: the GPU
//! device/queue/pipelines are a hardware-constraint firewall no plugin
//! author can be asked to respect correctly). **Landed**: a plugin billboard
//! now reaches real pixels, proven by
//! `lodestone_shell::gpu::pixel_gates::plugin_billboards_source_draws_visible_pixels`
//! (a headless GPU readback gate, with
//! `no_plugin_billboards_source_installed_draws_nothing` as its negative
//! control) — see that crate's `gpu/plugin_billboards.rs` for the pipeline and
//! `docs/plugin-api.md`'s section on this issue for how `PluginTexture::Named`
//! resolves against the block atlas, and what it falls back to when it does
//! not.
//!
//! # Configuration
//!
//! None.
//!
//! # Dependencies
//!
//! `bevy_ecs` and `lodestone-physics` (`Vec3d`) only.

use bevy_ecs::prelude::*;
use lodestone_physics::Vec3d;

/// What a [`PluginBillboard`] draws. See the module doc for exactly what
/// resolving [`Self::Named`] needs on the render side.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginTexture {
    /// A flat tinted quad, no texture.
    Solid,
    /// A texture the renderer's own atlas already knows by this id (e.g.
    /// `"minecraft:diamond"`).
    Named(String),
}

/// One world-space billboard quad a plugin wants drawn this frame — a
/// waypoint marker, a custom particle, a minimap-style icon floating over a
/// block.
///
/// Always camera-facing: the renderer computes the facing basis from its own
/// camera each frame, so this carries no orientation field — the same reason
/// `crate::player::CameraOverride` carries no near/far/FOV, per that type's
/// own doc ("an override cannot open a clip plane wrong by omission").
#[derive(Debug, Clone, PartialEq)]
pub struct PluginBillboard {
    /// World-space centre.
    pub position: Vec3d,
    /// Width/height, in blocks.
    pub size: [f32; 2],
    /// Linear RGBA tint, `0.0..=1.0` — multiplied against the texture's own
    /// colour in **gamma space**, matching `CLAUDE.md`'s "vanilla is not
    /// colour-managed" rule (tint and shade both multiply pre-linearization,
    /// never in linear light). `[1.0, 1.0, 1.0, 1.0]` leaves
    /// [`PluginTexture::Named`] unmodified; for [`PluginTexture::Solid`] this
    /// is the entire visible colour.
    pub color: [f32; 4],
    pub texture: PluginTexture,
}

/// World-space billboard geometry a plugin wants drawn this frame. See the
/// module doc for the full design; this is
/// [`crate::player::DebugLines`]'s sibling, not a replacement for it — a
/// pathfinder's planned route still wants a plain line, not a quad.
#[derive(Resource, Debug, Clone, Default)]
pub struct PluginBillboards(pub Vec<PluginBillboard>);

/// Empty [`PluginBillboards`] before this frame's `ExtractSet::Debug`
/// systems run. See the module doc for why this is `.before(ExtractSet::Debug)`
/// rather than a member of the set.
pub fn clear_plugin_billboards(mut billboards: ResMut<PluginBillboards>) {
    billboards.0.clear();
}

#[cfg(test)]
mod tests {
    use bevy_ecs::schedule::IntoScheduleConfigs;

    use super::*;
    use crate::schedules::Extract;
    use crate::sets::ExtractSet;

    fn sample() -> PluginBillboard {
        PluginBillboard {
            position: Vec3d::new(1.0, 2.0, 3.0),
            size: [0.5, 0.75],
            color: [1.0, 0.5, 0.25, 1.0],
            texture: PluginTexture::Solid,
        }
    }

    /// [`clear_plugin_billboards`] genuinely empties the resource through a
    /// real schedule — mirrors
    /// `crate::player::tests::clear_debug_lines_empties_the_resource_through_the_schedule`.
    #[test]
    fn clear_plugin_billboards_empties_the_resource_through_the_schedule() {
        let mut app = bevy_app::App::new();
        app.init_resource::<PluginBillboards>();
        app.configure_sets(Extract, ExtractSet::Debug);
        app.add_systems(Extract, clear_plugin_billboards.before(ExtractSet::Debug));

        app.world_mut()
            .resource_mut::<PluginBillboards>()
            .0
            .push(sample());
        app.world_mut().run_schedule(Extract);

        assert!(app.world().resource::<PluginBillboards>().0.is_empty());
    }

    /// The negative control for the test above: without running the
    /// schedule, a pushed billboard genuinely stays put — proof the clear
    /// above is not trivially satisfied by a resource that starts empty.
    #[test]
    fn a_pushed_billboard_survives_until_the_schedule_actually_runs() {
        let mut app = bevy_app::App::new();
        app.init_resource::<PluginBillboards>();

        app.world_mut()
            .resource_mut::<PluginBillboards>()
            .0
            .push(sample());

        assert_eq!(app.world().resource::<PluginBillboards>().0.len(), 1);
    }

    /// A plugin system ordered `.in_set(ExtractSet::Debug)` — the sanctioned
    /// way in, per the module doc — appends this frame's geometry and it
    /// survives to be read after the schedule, exactly the "reaches the
    /// screen" contract `DebugLines` already proves for lines: this is the
    /// same contract for a textured/billboard draw.
    #[test]
    fn a_plugin_system_in_extract_debug_reaches_the_resource_after_one_frame() {
        fn push_a_waypoint(mut billboards: ResMut<PluginBillboards>) {
            billboards.0.push(PluginBillboard {
                position: Vec3d::new(4.0, 5.0, 6.0),
                size: [1.0, 1.0],
                color: [0.0, 1.0, 0.0, 1.0],
                texture: PluginTexture::Named("minecraft:diamond".to_owned()),
            });
        }

        let mut app = bevy_app::App::new();
        app.init_resource::<PluginBillboards>();
        app.configure_sets(Extract, ExtractSet::Debug);
        app.add_systems(Extract, clear_plugin_billboards.before(ExtractSet::Debug));
        app.add_systems(Extract, push_a_waypoint.in_set(ExtractSet::Debug));

        app.world_mut().run_schedule(Extract);

        let billboards = &app.world().resource::<PluginBillboards>().0;
        assert_eq!(billboards.len(), 1);
        assert_eq!(billboards[0].position, Vec3d::new(4.0, 5.0, 6.0));
        assert_eq!(
            billboards[0].texture,
            PluginTexture::Named("minecraft:diamond".to_owned())
        );
    }

    /// The clear really does run every frame, not just once: with the
    /// writer system removed, a second frame comes back empty even though
    /// the first one populated the resource — proof this is "only this
    /// frame's geometry, never last frame's leftovers", matching
    /// `DebugLines`' own guarantee, rather than a one-shot push that happens
    /// to still be sitting there.
    #[test]
    fn a_billboard_does_not_survive_into_a_frame_with_no_writer() {
        let mut app = bevy_app::App::new();
        app.init_resource::<PluginBillboards>();
        app.configure_sets(Extract, ExtractSet::Debug);
        app.add_systems(Extract, clear_plugin_billboards.before(ExtractSet::Debug));

        app.world_mut()
            .resource_mut::<PluginBillboards>()
            .0
            .push(sample());
        assert_eq!(app.world().resource::<PluginBillboards>().0.len(), 1);

        app.world_mut().run_schedule(Extract);
        assert!(app.world().resource::<PluginBillboards>().0.is_empty());
    }
}
