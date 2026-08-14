//! Aggregate per-frame render statistics, surfaced to the debug overlay.

/// Aggregate numbers for one rendered frame, surfaced to the debug overlay.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderStats {
    /// Sections with non-empty **opaque** geometry drawn this frame.
    pub sections_drawn: usize,
    /// Opaque sections rejected by vanilla's circular view membership
    /// ([`lodestone_render::within_view_distance`]) this frame.
    pub sections_culled_distance: usize,
    /// Opaque sections rejected by the view frustum this frame — i.e. resident,
    /// inside the view circle, and still off screen.
    pub sections_culled_frustum: usize,
    /// Opaque sections rejected by the section occlusion graph this frame:
    /// on screen and in range, but not reachable from the camera through
    /// connected open space. Always `0` while
    /// [`TerrainCull::occlusion_active`](lodestone_render::TerrainCull::occlusion_active)
    /// is false, which is how a walk that silently degraded to pure frustum is
    /// told apart from one that found nothing to cull.
    pub sections_culled_occlusion: usize,
    /// Opaque sections the occlusion walk **would** have culled, and which drew
    /// anyway because the graph is in [`TerrainOcclusion::Shadow`]
    /// (`crate::gpu::TerrainOcclusion`). Always `0` when enforcing, where the
    /// same sections are in
    /// [`sections_culled_occlusion`](Self::sections_culled_occlusion) instead.
    ///
    /// This is the soak counter: it says what flipping the cull on would remove,
    /// on the real world you are standing in, while nothing can yet disappear.
    pub sections_occlusion_shadow: usize,
    /// Whether the occlusion graph produced a reachable set that is **culling**
    /// this frame.
    ///
    /// The discriminator the silent-degradation failure mode needs: every way the
    /// walk can go wrong draws *more*, so `sections_culled_occlusion == 0` on its
    /// own cannot tell "an open surface with nothing occluded" from "the graph
    /// refused to walk and the cull has quietly been frustum-only all session".
    pub occlusion_active: bool,
    /// Sections in the occlusion graph — every section the mesher has produced,
    /// **including the ones with no geometry**, which is strictly more than
    /// `section_count()`. A value that tracks `sections_drawn` instead means the
    /// empty (fully-solid) sections are missing and the walk cannot see a floor.
    pub occlusion_graph_sections: usize,
    /// Camera walks performed this session (cumulative, not per frame — the claim
    /// the cadence makes is that this does *not* increment while you turn on the
    /// spot, and a per-frame counter cannot express that).
    pub occlusion_walks: u64,
    /// Sections with **water** geometry drawn this frame.
    ///
    /// Its own counter because the invariant only closes per pass: a water-only
    /// section carries `mesh: None`, so it never reaches
    /// [`sections_drawn`](Self::sections_drawn) while still issuing a draw.
    /// Measured 189 `sections_drawn` against 195 uploads and 304 `draw_calls`
    /// before culling existed — a single combined invariant reads
    /// as a cull bug on a perfectly healthy frame.
    pub water_sections_drawn: usize,
    /// Water sections culled this frame, all three reasons summed (the split is
    /// only tracked for the opaque pass, which is the dominant one).
    pub water_sections_culled: usize,
    /// Total merged quads across all drawn sections.
    pub total_quads: usize,
    /// Draw calls issued (one per non-empty section).
    pub draw_calls: usize,
    /// Vertex+index **buffer-bind pairs** the two live terrain passes issued this
    /// frame — one per arena block actually drawn from, plus one for each section
    /// that fell back to a dedicated buffer.
    ///
    /// This is the number that fix's second half moves: before the shared mesh
    /// arena it was exactly `sections_drawn + water_sections_drawn` (every section
    /// bound its own two buffers), and it should now be in the low tens
    /// regardless of render distance. A value that tracks `sections_drawn` again
    /// means either the arena is refusing placements (check for the
    /// dedicated-buffer warning) or the per-block grouping in
    /// `emit_terrain_draws` stopped grouping.
    pub terrain_buffer_binds: usize,
    /// Exact bytes of GPU mesh storage occupied by **resident** sections, from
    /// [`RenderState::resident_mesh_bytes`](crate::gpu::RenderState::resident_mesh_bytes).
    ///
    /// Residency, not visibility. This used to be
    /// `vram_bytes(self.total_quads)` — i.e. derived from the *drawn* quad count,
    /// which the frustum cull changes on every camera rotation — so the overlay's
    /// VRAM figure moved whenever the player looked around and was read as
    /// load/unload churn. Nothing is allocated or freed by a rotation; see
    /// `resident_mesh_bytes` for the full account and for what this excludes.
    pub vram_bytes: usize,
    /// Bytes of GPU mesh storage the driver is **holding**, from
    /// [`RenderState::reserved_mesh_bytes`](crate::gpu::RenderState::reserved_mesh_bytes)
    /// — the model arena's whole blocks rather than the spans handed out of them.
    ///
    /// Always `>= vram_bytes`. Its own counter because the two answer different
    /// questions and only the pair can tell healthy reuse (`vram_bytes` moving
    /// under a flat reserved figure) from fragmentation (reserved climbing while
    /// `vram_bytes` does not).
    pub vram_reserved_bytes: usize,
    /// Entity instances drawn this frame (post-frustum-cull).
    pub entities_drawn: usize,
    /// Entity instances frustum-culled this frame.
    pub entities_culled: usize,
    /// Particle billboards drawn this frame.
    pub particles_drawn: usize,
    /// Of [`particles_drawn`](Self::particles_drawn), how many sample the
    /// stitched **particle sheet** (flame, smoke, crits, splashes) rather than
    /// the block-model atlas that terrain debris comes from.
    ///
    /// Its own counter because the particle pass used to bind only one
    /// texture, so sheet particles resolved, uploaded, drew — and sampled
    /// *block* texels at particle-sheet coordinates. `particles_drawn` was
    /// perfectly healthy throughout. Read together with
    /// [`particle_sheet_atlas_bound`](Self::particle_sheet_atlas_bound): non-zero
    /// here with that `false` means every one of them discarded on alpha.
    pub particles_from_sheet: usize,
    /// Whether [`RenderState::install_particle_sheet_atlas`] has run, i.e.
    /// whether the sheet slots of the particle pass's bind group hold the real
    /// stitched sheet or the 1×1 transparent stand-in. `false` on a jar-less run
    /// and in every headless test that does not install one.
    pub particle_sheet_atlas_bound: bool,
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
    /// Campfire cooking items drawn this frame — one per **occupied** slot, so a
    /// campfire with two steaks on it counts 2 and a lit but empty one counts 0.
    ///
    /// Its own counter rather than folded into
    /// [`item_drops_drawn`](Self::item_drops_drawn), and the reason is specific:
    /// a campfire is the one block entity whose renderer contributes nothing to
    /// `block_entities_drawn` either, so without this a broken campfire gather is
    /// invisible in **both** counters at once.
    pub campfire_items_drawn: usize,
    /// Filled-map pictures drawn this frame — the held one counts 1, plus one per
    /// item frame carrying a map.
    ///
    /// Its own counter for the reason [`item_drops_drawn`](Self::item_drops_drawn)
    /// has one, plus one specific to maps: a map whose colour grid is entirely
    /// `MapColor.NONE` draws a fully transparent quad, so "the map is unexplored"
    /// and "the map never reached the pipeline" are the same number of visible
    /// pixels. This counter separates them.
    pub filled_maps_drawn: usize,
    /// Banner **pattern layers** drawn this frame — one per mask per banner, so a
    /// plain undyed banner counts 1 (its `base`) and a heavily decorated one up to
    /// 17.
    ///
    /// Its own counter because a banner's pole and cloth draw through the ordinary
    /// opaque batch and are therefore already counted in `block_entities_drawn`: a
    /// banner whose *patterns* never reached the translucent pass looks like a
    /// blank white banner, which is a perfectly ordinary thing to see.
    pub banner_layers_drawn: usize,
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
    /// Armour **trims** drawn this frame — one per `(wearer, slot)` carrying a
    /// `minecraft:trim`, so a leather piece counts 1 even though its armour is 2
    /// layers.
    ///
    /// Its own counter because a trim is subtle by design: a gold-trimmed diamond
    /// chestplate whose trim never reached the pipeline looks like a plain
    /// chestplate, which is a perfectly plausible thing for a mob to be wearing.
    pub armour_trims_drawn: usize,
    /// Sheep wool layers drawn this frame — one per unsheared sheep whose
    /// wool attached to its own body. Mirrors
    /// [`armour_layers_drawn`](Self::armour_layers_drawn)'s role: a sheared
    /// sheep, a non-sheep quadruped with `wool: Some(..)` (should never
    /// happen — see `docs/entity-rendering.md`'s pig/cow trap), and a missing
    /// vanilla pack all leave this at zero without leaving `entities_drawn`
    /// at zero, so a broken wool attach cannot hide behind "nothing rendered
    /// at all".
    pub wool_layers_drawn: usize,
    /// Mob-fire billboards drawn this frame — one per on-fire,
    /// frustum-visible entity whose type has a baked flame mesh. Zero with no
    /// vanilla pack (no flame texture) or when no entity currently has
    /// `EntityDraw::on_fire` set — see `RenderState::prepare_flame`'s doc for
    /// why there is no synthetic-texture fallback here, mirroring
    /// `armour_layers_drawn`/`wool_layers_drawn`.
    pub flame_billboards_drawn: usize,
    /// Experience-orb billboards drawn this frame — one per frustum-visible orb.
    /// Zero with no vanilla pack (no orb sheet), and zero when no orb is on
    /// screen; see `RenderState::prepare_orbs` for why there is no
    /// synthetic-texture fallback, mirroring `flame_billboards_drawn`.
    ///
    /// Counts **orbs**, not draw calls: several orbs whose values bucket into the
    /// same sprite cell share one instanced draw, so this is always ≥ the number
    /// of orb draw calls.
    pub experience_orbs_drawn: usize,
    /// `minecraft:special` items (chest, shulker box, skull) drawn as **dropped
    /// stacks** this frame, through the block-entity rig rather than baked quads.
    ///
    /// Three separate counters rather than one, because the three surfaces failed
    /// independently: each was its own island, and a single total would report
    /// "special items are drawing" while two of the three still drew nothing.
    pub special_item_drops_drawn: usize,
    /// `minecraft:special` items drawn in **another entity's hand** this frame.
    pub special_item_hands_drawn: usize,
    /// `minecraft:special` items drawn in an **item frame** this frame.
    pub special_item_frames_drawn: usize,
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
    /// **Moving block models** drawn this frame — falling sand/gravel today, and
    /// piston heads when that producer lands (`gpu/moving_blocks.rs`).
    ///
    /// Its own counter for the reason [`projectiles_drawn`](Self::projectiles_drawn)
    /// has one, and one more: a moving block is the only thing on screen that is
    /// *block* geometry at a non-block position, so it reaches neither
    /// `entities_drawn` (no cuboid rig) nor `sections_drawn` (not in a chunk mesh).
    /// Without this counter a falling block that drew nothing and one that drew
    /// correctly produce byte-identical stats — which is exactly the island shape
    /// this crate has paid for nine times, and the block state travelling on one
    /// unvalidated VarInt makes a silent zero here the likely failure.
    pub moving_blocks_drawn: usize,
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
    /// Whether the underwater overlay drew this frame — a pass
    /// is installed, first-person, not spectator, and
    /// `ScreenEffects::eye_in_water` was true.
    pub underwater_overlay_drawn: bool,
    /// Whether the fire overlay drew this frame — same gating as
    /// [`underwater_overlay_drawn`](Self::underwater_overlay_drawn), keyed on
    /// `ScreenEffects::on_fire` instead.
    pub fire_overlay_drawn: bool,
    /// Whether the pumpkin overlay drew this frame — same gating
    /// as [`underwater_overlay_drawn`](Self::underwater_overlay_drawn), keyed
    /// on `ScreenEffects::wearing_pumpkin` instead.
    pub pumpkin_overlay_drawn: bool,
    /// Whether the spyglass overlay drew this frame — same
    /// first-person/spectator gating as
    /// [`pumpkin_overlay_drawn`](Self::pumpkin_overlay_drawn), keyed on
    /// `ScreenEffects::scoping`.
    pub spyglass_overlay_drawn: bool,
    /// Whether the freeze overlay drew this frame — **not**
    /// first-person-gated (see `ScreenEffects::any_active`'s doc), keyed on
    /// `ScreenEffects::freeze_percent > 0.0`.
    pub freeze_overlay_drawn: bool,
    /// Whether the confusion overlay drew this frame — not
    /// first-person-gated, keyed on `ScreenEffects::nausea_intensity > 0.0`
    /// **and** `ScreenEffects::portal_intensity <= 0.0` (portal takes
    /// priority — `Hud.java:300-302`).
    pub confusion_overlay_drawn: bool,
    /// Whether the portal overlay drew this frame — not
    /// first-person-gated, keyed on `ScreenEffects::portal_intensity > 0.0`.
    pub portal_overlay_drawn: bool,
    /// Block-entity rigs drawn this frame — chests today.
    ///
    /// Its own counter, not folded into `entities_drawn`, for the reason
    /// [`item_drops_drawn`](Self::item_drops_drawn) and
    /// [`projectiles_drawn`](Self::projectiles_drawn) have theirs: a chest is not
    /// an entity, so it never reaches `entities_drawn`, and before this counter a
    /// room full of chests and an empty room produced byte-identical stats.
    ///
    /// A chest has **no block model at all** in 26.2 (`block/chest.json` is
    /// `{"textures":{"particle":…}}`, zero elements), so this is also the only
    /// number that separates "chests render" from "chests are invisible holes" —
    /// `sections_drawn` is unaffected either way.
    pub block_entities_drawn: usize,
    /// Block-entity rigs frustum-culled this frame.
    pub block_entities_culled: usize,
    /// How many block-entity **sheets** loaded from the vanilla pack.
    ///
    /// The discriminator between "nothing in view" and "nothing can ever draw":
    /// a chest with no sheet draws nothing rather than a placeholder box (see
    /// `gpu/block_entities.rs`), so `block_entities_drawn == 0` is ambiguous on
    /// its own. Zero here means no jar; 22 means every stem resolved.
    pub block_entity_sheets_loaded: usize,
    /// Sign-text vertices uploaded this frame, six per glyph ink
    /// run across both sides of every installed [`lodestone_render::SignSpawn`].
    /// The exact, non-pixel-based corroboration a pixel gate needs alongside
    /// [`block_entities_drawn`](Self::block_entities_drawn): a sign's *board*
    /// already draws through the ordinary terrain pass with no counter of its
    /// own, so this is the only number that separates "no sign text in view"
    /// from "sign text can never draw" (no jar, or a blank sign — both are
    /// `0`, which is why a pixel gate must also install real text to tell
    /// them apart).
    pub sign_text_vertices: u32,
    /// Mining-crack overlays actually drawn this frame — one per
    /// [`CrackTarget`](crate::gpu::CrackTarget) in the slice passed to
    /// [`RenderState::render_with_crack`](crate::gpu::RenderState::render_with_crack)
    /// whose target block resolved to real geometry. Before that fix the pipeline
    /// accepted at most one target, so this could never exceed `1`; a live
    /// frame with two players digging different blocks now reports `2`, which
    /// is the number a single-target regression cannot produce.
    pub cracks_drawn: usize,
    /// Distinct terrain **camera bind-group objects** (group 0: shared
    /// view-projection + per-section origin arena) bound this frame, across
    /// both the packed and model/fluid draw loops combined.
    ///
    /// This is the measured shape the section-camera-uniform fix
    /// (`docs/section-camera-uniform.md`) actually claims — not "one
    /// `set_bind_group` call per section" (there are exactly that many, one per
    /// draw, because each carries a different dynamic offset and that is
    /// cheap) but "one bind-group **object**, built once, reused by every
    /// draw." Counted by pointer identity: incremented only when the
    /// `&wgpu::BindGroup` passed to `set_bind_group(0, ..)` for a terrain draw
    /// differs from the previous terrain group-0 bind, so a run of draws that
    /// all reuse `packed_cam_bind_group` or `model.cam_bind_group` contributes
    /// exactly one regardless of how many sections it covers.
    ///
    /// Healthy value: `1` (packed-only or model-only frame) or `2` (both paths
    /// drew this frame — packed table plus live model terrain, entering each
    /// exactly once). A value that scales with `sections_drawn` means the
    /// per-section bind-group shape those fixes removed has come back — the
    /// measured counterpart to that fix's "bind-group count independent of
    /// section count" ask, rather than a code-reading argument for it.
    pub terrain_camera_bind_group_switches: usize,
}
