//! Aggregate per-frame render statistics, surfaced to the debug overlay.

/// Aggregate numbers for one rendered frame, surfaced to the debug overlay.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderStats {
    /// Sections with non-empty geometry drawn this frame.
    pub sections_drawn: usize,
    /// Total merged quads across all drawn sections.
    pub total_quads: usize,
    /// Draw calls issued (one per non-empty section).
    pub draw_calls: usize,
    /// Approximate mesh VRAM in bytes.
    pub vram_bytes: usize,
    /// Entity instances drawn this frame (post-frustum-cull).
    pub entities_drawn: usize,
    /// Entity instances frustum-culled this frame.
    pub entities_culled: usize,
    /// Particle billboards drawn this frame.
    pub particles_drawn: usize,
    /// Dropped-item entities drawn this frame (item entities with a known stack
    /// *and* baked geometry). Distinct from `entities_drawn`, which counts only
    /// the cuboid-rig mobs the entity pipeline handles — an item entity never
    /// appears there, so without this counter a frame full of drops is
    /// indistinguishable from an empty one.
    pub item_drops_drawn: usize,
    /// Items drawn in a mob's hand this frame (a `MainHand`/`OffHand` equipment
    /// slot with baked geometry, on an entity whose rig has that arm). Counted
    /// separately from `item_drops_drawn` for the same reason that exists: a
    /// held item goes through the *model* pipeline, so it never shows up in
    /// `entities_drawn`, and a silently-broken equipment chain would otherwise
    /// look exactly like a server that sent no equipment.
    pub held_items_drawn: usize,
    /// Humanoid armour **layers** drawn this frame — one per
    /// `(wearer, slot, texture layer)`, so a leather chestplate counts 2 (its
    /// dyeable base and its overlay) and a diamond one counts 1.
    ///
    /// Counted per layer rather than per piece precisely because the second
    /// leather layer is the one at risk: it is coplanar with the first and
    /// depends on the armour pipeline's `LessEqual` depth compare, so a
    /// regression there shows up as a count that is right and pixels that are
    /// not — but a count that *drops* to one per piece localises the break to
    /// resolution rather than to depth.
    ///
    /// Zero with no vanilla pack: armour has no synthetic-texture fallback.
    pub armour_layers_drawn: usize,
    /// Sheep wool layers drawn this frame — one per unsheared sheep whose
    /// wool attached to its own body (issue #53). Mirrors
    /// [`armour_layers_drawn`](Self::armour_layers_drawn)'s role: a sheared
    /// sheep, a non-sheep quadruped with `wool: Some(..)` (should never
    /// happen — see `docs/entity-rendering.md`'s pig/cow trap), and a missing
    /// vanilla pack all leave this at zero without leaving `entities_drawn`
    /// at zero, so a broken wool attach cannot hide behind "nothing rendered
    /// at all".
    pub wool_layers_drawn: usize,
    /// Whether the first-person arm was drawn this frame. `false` means the
    /// `player_wide` mesh, its texture, or its arm part was missing — i.e. a
    /// real defect, not a quiet frame, because this pass is unconditional
    /// whenever [`third_person_body_drawn`](Self::third_person_body_drawn) is
    /// `false`.
    pub first_person_arm_drawn: bool,
    /// Whether [`RenderState::set_third_person_body_source`]'s closure
    /// returned a body this frame — i.e. whether the local player's own
    /// third-person avatar was folded into this frame's entity list at all
    /// (not whether it survived frustum culling, which
    /// [`entities_drawn`](Self::entities_drawn)/
    /// [`entities_culled`](Self::entities_culled) already cover generically).
    /// `false` for every caller today: nothing in this shell installs the
    /// source yet.
    pub third_person_body_drawn: bool,
    /// Thrown item projectiles drawn this frame — snowballs, eggs, pearls,
    /// potions, fireballs and the eye of ender, each a camera-facing billboard of
    /// its own item model ([`lodestone_render::entity::thrown_item_for`]).
    ///
    /// Its own counter for the same reason [`item_drops_drawn`](Self::item_drops_drawn)
    /// is: a projectile is neither a cuboid rig (so it never reaches
    /// `entities_drawn`) nor an item entity (so it never reaches
    /// `item_drops_drawn`). Before this counter existed a sky full of snowballs
    /// and an empty sky produced byte-identical stats.
    pub projectiles_drawn: usize,
    /// Whether the item in the local player's first-person hand was drawn this
    /// frame *instead of* the bare arm.
    ///
    /// Mutually exclusive with [`first_person_arm_drawn`](Self::first_person_arm_drawn),
    /// which is vanilla's own structure: `submitArmWithItem` renders the arm only
    /// when the stack is empty. Both `false` in third person; both `false` also
    /// means the `player_wide` rig failed to load, which is a defect.
    pub first_person_item_drawn: bool,
    /// Whether the sky pass ran this frame — i.e. whether
    /// [`RenderState::install_sky`] has been called. `false` for every caller
    /// today that has not installed one (every headless test, a jar-less run);
    /// also what the block pass's clear-vs-load choice keys off, so a wrong
    /// value here is not just a missing counter, it is a missing frame clear.
    pub sky_drawn: bool,
    /// Whether the underwater overlay (issue #108) drew this frame — a pass
    /// is installed, first-person, not spectator, and
    /// `ScreenEffects::eye_in_water` was true.
    pub underwater_overlay_drawn: bool,
    /// Whether the fire overlay (issue #112) drew this frame — same gating as
    /// [`underwater_overlay_drawn`](Self::underwater_overlay_drawn), keyed on
    /// `ScreenEffects::on_fire` instead.
    pub fire_overlay_drawn: bool,
}
