//! Renderer-facing entity draw data and model-path selection.

use super::*;

/// Name-selected visual variants that have crossed the entity render boundary.
///
/// These flags are resolved from the entity's exact custom-name text during
/// extraction, rather than inferred from the display tag. That keeps a hidden
/// custom name able to affect the model while ordinary names remain inert.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NamedEntityCosmetics {
    /// Rotate the complete supported living-entity draw, including layers.
    pub upside_down: bool,
    /// Use the age-driven rainbow tint for sheep wool.
    pub rainbow_wool: bool,
}

/// A single entity ready to draw this frame: its model type and interpolated
/// transform inputs. The renderer turns this into an
/// [`EntityInstance`](lodestone_render::EntityInstance).
///
/// Produced by [`extract_entity_draws`], the `ExtractSet::Entities` system —
/// `docs/bevy-migration.md` §4.4's rule that extract systems live upstream of
/// `lodestone-render`, which stays bevy-free, and that they emit plain PODs it
/// already consumes.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityDraw {
    /// The server-assigned entity id. Carried through to the draw because a
    /// dropped item's bob/spin phase is derived from it
    /// ([`item_bob_offset`](lodestone_render::entity::item_bob_offset)) — vanilla rolls
    /// that phase from a client RNG we cannot observe, so a stable hash of the
    /// id stands in for it.
    pub id: i32,
    /// The entity type's canonical path (e.g. `"pig"`).
    ///
    /// `Arc<str>`, not `String` — cloned from [`RenderKind`] once
    /// per tracked entity per frame in `extract_entity_draws`; see that
    /// component's doc for why a refcount bump replaced a heap allocation
    /// here.
    pub type_path: Arc<str>,
    /// Exact-name cosmetic decisions made by the entity extraction boundary.
    pub named_cosmetics: NamedEntityCosmetics,
    /// Which item's model to draw, for any entity whose server metadata
    /// carried an `ITEM_STACK` field — a dropped item
    /// ([`ITEM_ENTITY_TYPE_PATH`]), an item frame's contents
    /// (`ItemFrame.DATA_ITEM`, including a framed `filled_map`), and a thrown
    /// projectile's stack (`ThrowableItemProjectile`/`Fireball`/`EyeOfEnder`
    /// all sync through the same serializer). `None` for an entity that has
    /// never reported one.
    ///
    /// **This used to be narrowed to [`ITEM_ENTITY_TYPE_PATH`] in
    /// `extract_entity_draws`, which is what made the other three consumers
    /// dead**; the draw sites each gate on their own entity type, so the
    /// narrowing bought nothing and cost every framed item, every framed map
    /// and every projectile's live tint.
    pub item: Option<ResourceLocation>,
    /// The source stack's optional `minecraft:item_model` component. This is
    /// retained for final render-candidate diagnostics without changing the
    /// base-item routing semantics of entity renderers.
    pub item_model: Option<ResourceLocation>,
    /// The custom skin URL from a player head's `minecraft:profile`.
    /// `None` remains the vanilla default Steve skull.
    pub item_skin: Option<Arc<str>>,
    /// What this entity is holding/wearing, narrowed to the slots that actually
    /// have something in them: an entry here means "there is an item in this
    /// slot", so the renderer needs no second `Option` check.
    ///
    /// **Stale note, kept for history:** this used to say only `MainHand`/
    /// `OffHand` could reach a pixel, because the `entity_models` corpus had
    /// no armour layer at all. `docs/armour-rendering.md` landed a *separate*
    /// humanoid mesh set since — `RenderState::prepare_armour` in `gpu.rs`
    /// now walks `ArmourSlot::ALL` against this same list, so every humanoid
    /// slot draws, not just the two hand slots.
    ///
    /// Order follows [`EquipmentSlot::ALL`] only by accident of what the server
    /// sent; treat it as an unordered set.
    pub equipment: Vec<(EquipmentSlot, ResourceLocation)>,
    /// Per-slot `minecraft:dyed_color`, mirroring
    /// [`EntityFacts::equipment_dye`] narrowed the same way `equipment`
    /// narrows [`EntityFacts::equipment`] — see that field's doc for why
    /// this is additive rather than folded into `equipment`'s own tuple.
    pub equipment_dye: Vec<(EquipmentSlot, u32)>,
    /// Per-slot custom player-head texture URL, narrowed from
    /// [`EntityFacts::equipment_skin`] alongside [`Self::equipment`]. The
    /// special-item renderer consumes this only for player-head rigs; absent
    /// means retain that rig's static default (Steve) texture.
    pub equipment_skin: Vec<(EquipmentSlot, Arc<str>)>,
    /// Per-slot `minecraft:trim`, mirroring
    /// [`EntityFacts::equipment_trim`] and narrowed exactly as
    /// [`Self::equipment_dye`] is.
    ///
    /// Additive rather than folded into [`Self::equipment`]'s tuple for that
    /// field's reason, and additive rather than folded into `equipment_dye`'s
    /// because an item can carry both: trimmed leather armour is dyed *and*
    /// trimmed, and the two reach the GPU differently — dye as an instance tint,
    /// trim as its own texture and therefore its own batch.
    pub equipment_trim: Vec<(EquipmentSlot, lodestone_model::item::ArmorTrim)>,
    /// This entity's wool state, when [`Self::type_path`] is `"sheep"` and a
    /// variant has been reported — `None` for every other entity type
    /// unconditionally, per [`sheep_wool`]'s gate.
    ///
    /// The mesh/tint/pose plumbing (`WoolMesh`/`SheepWoolModelSet::attach` in
    /// `lodestone-render/src/entity.rs`, `RenderState::prepare_wool` in
    /// `gpu.rs`) consumes this field. A sheep carrying the exact rainbow name
    /// selects an age-driven tint there; baby sheep use the same path with
    /// their existing render scale.
    pub wool: Option<SheepWool>,
    /// How many items [`Self::item`] represents, when it is `Some`.
    /// Meaningless (and left at the neutral `1`) for every entity with no
    /// reported stack, and for the consumers that draw one copy whatever the
    /// count — an item frame holds a stack and draws it once.
    ///
    /// `prepare_item_geometry` turns this into vanilla's 1–5 copies via
    /// `lodestone_render::entity::rendered_amount`, scattered by
    /// `item_cluster_jitter` — see `docs/dropped-items.md`.
    pub count: u32,
    /// Whether the carried stack is enchanted, so the drop gets the glint second
    /// pass. `false` for every entity with no reported stack. Read by the
    /// framed-item pass too, not only the drop pass.
    pub foil: bool,
    /// [`Self::item`]'s `minecraft:dyed_color`, mirroring
    /// [`EntityFacts::item_dyed_color`] narrowed the same way [`Self::count`]
    /// narrows `EntityFacts::count`. `None` for an undyed stack or when
    /// [`Self::item`] is `None`.
    ///
    /// Fed into [`lodestone_render::stamp_live_item_tint`] by
    /// `gpu::world_items`, alongside [`Self::item_potion_color`] — the pair
    /// that resolves a dropped item's or a thrown projectile's real tint
    /// instead of the item definition's plain default.
    pub item_dyed_color: Option<u32>,
    /// [`Self::item`]'s already-mixed `minecraft:potion_contents` colour,
    /// mirroring [`Self::item_dyed_color`] exactly — see
    /// [`lodestone_model::item::ItemComponents::potion_color`]'s doc for why
    /// this is the pre-mixed colour and not the raw patch.
    pub item_potion_color: Option<u32>,
    /// Interpolated feet position in world space.
    pub feet: Vec3,
    /// Interpolated body yaw in degrees.
    pub yaw: f32,
    /// Interpolated head yaw in degrees (absolute), for head tracking.
    pub head_yaw: f32,
    /// Interpolated head pitch in degrees.
    pub pitch: f32,
    /// Uniform render scale.
    pub scale: f32,
    /// Per-part animation drive (head tracking, walk cycle, idle age), already
    /// interpolated for this frame and in the units
    /// [`Skeleton::pose`](lodestone_render::Skeleton::pose) expects — note
    /// `head_yaw_deg` is **relative to the body**, matching vanilla's
    /// `netHeadYaw`.
    pub anim: AnimInput,
    /// The block state this entity is imitating, when it is a
    /// `minecraft:falling_block` and its spawn packet has been decoded — the
    /// source-tagged block-state reference. The moving-block renderer accepts a
    /// built-in canonical state only after validating it against its census;
    /// protocol-local values remain opaque until a matching resolver exists.
    ///
    /// `None` for every other entity type, and the switch the moving-block-model
    /// pass keys on (`gpu/moving_blocks.rs`). Absence is deliberate rather than a
    /// sentinel `0`: state id `0` is a real state (`minecraft:air`), so a caller
    /// could not tell "not a falling block" from "a falling block made of air".
    ///
    /// Bridged off the *ingest* entity through [`EntityIndex`] in
    /// [`extract_entity_draws`], like [`Self::hurt`] and [`Self::item_use`], not
    /// folded through `EntityFacts` — see [`Self::hurt`] for why that hop is
    /// avoided.
    pub block_state: Option<BlockStateRef>,
    /// Which of the eight 45° steps the stack in an item frame is turned to —
    /// vanilla's own item-frame rotation accessor, `0..8`.
    ///
    /// `0` for everything that is not an item frame, and for a frame whose
    /// rotation has not been reported: that is vanilla's own accessor default
    /// (an upright item), so unlike [`Self::block_state`] there is nothing an
    /// `Option` would distinguish. A frame *always* draws its contents; only
    /// their in-plane angle is at stake.
    ///
    /// Bridged off the *ingest* entity through [`EntityIndex`] in
    /// [`extract_entity_draws`], like [`Self::block_state`] above.
    pub item_frame_rotation: u8,
    /// Which painting is hung, as [`lodestone_render::painting`]'s own static
    /// variant name — `None` for every entity that is not a painting, and for a
    /// painting whose variant this build has no table entry for.
    ///
    /// **`None` must draw nothing.** A painting's size in blocks is a property
    /// of its variant, so there is no default shape to fall back on: a 1x1
    /// stand-in where a 4x4 belongs reads as a rendering bug rather than as an
    /// unsupported data pack. That is why this is an `Option<&'static str>` and
    /// not, say, a name with a fallback — the narrowing to a known variant
    /// happens once here, and the draw site is left with no decision to make.
    ///
    /// Bridged off the *ingest* entity's [`lodestone_ecs::entity::PaintingVariant`]
    /// through [`EntityIndex`] in [`extract_entity_draws`], like
    /// [`Self::block_state`] and [`Self::item_frame_rotation`] above.
    ///
    /// The painting's **facing** is not here and does not need to be:
    /// `HangingEntity` writes the direction into the entity's ordinary yaw, so
    /// [`Self::yaw`] already carries it — see
    /// `lodestone_render::painting::painting_matrix`.
    pub painting: Option<&'static str>,
    /// A firework rocket's two draw flags, or `None` for every entity that has
    /// never reported either — which includes every entity that is not a
    /// firework, and a firework rocket at both vanilla defaults.
    ///
    /// **`None` is not "do not draw".** A plain shot rocket reports neither
    /// flag, so the draw site keys on the entity *type* and reads these with
    /// their vanilla defaults (`false`, `false`). Only
    /// [`lodestone_ecs::entity::FireworkFlags::attached`] suppresses the draw,
    /// and that is `Some(true)` when it does.
    ///
    /// Bridged off the *ingest* entity through [`EntityIndex`] in
    /// [`extract_entity_draws`], like [`Self::painting`] above.
    pub firework: Option<lodestone_ecs::entity::FireworkFlags>,
    /// Who launched this projectile — the caster's entity id, or `None` for
    /// every entity the adapter has not established an owner reading for. Today
    /// that means it is `Some` on a `fishing_bobber` and `None` on everything
    /// else, which is the switch the fishing-line pass keys on.
    ///
    /// Bridged off the *ingest* entity's
    /// [`lodestone_ecs::entity::ProjectileOwner`] through [`EntityIndex`] in
    /// [`extract_entity_draws`], like [`Self::firework`] above.
    ///
    /// **An id, deliberately, and one that need not resolve.** The draw site
    /// looks it up in the same frame's draw slice; a miss means the owner is the
    /// *local player*, who `extract_entity_draws` excludes by construction, so
    /// the anchor falls back to the camera exactly as vanilla's own
    /// first-person branch does. A remote owner outside tracking range would
    /// take that same fallback and anchor the line at our own hand — vanilla
    /// draws nothing at all there, but a bobber whose owner is untracked is
    /// already outside anything this client can see.
    pub projectile_owner: Option<i32>,
    /// This entity's resolved nametag, narrowed from
    /// [`RenderNameTag`]. `None` draws nothing — the common case for every
    /// entity with no visible custom name.
    pub name_tag: Option<NameTag>,
    /// Whether the hurt/death **red overlay** applies to this entity's model
    /// this frame. The overlay is active while either countdown is positive.
    ///
    /// Boolean, not a fade: vanilla does not interpolate by how much of
    /// `hurtTime` remains, so neither does this (see
    /// [`lodestone_render::EntityInstanceRaw::with_hurt_overlay`]).
    ///
    /// Read off [`lodestone_ecs::entity::HurtTime`] through [`EntityIndex`] in
    /// [`extract_entity_draws`], **not** folded through [`EntityFacts`] —
    /// exactly like [`Self::anim`]'s `attack_anim`, and for the same reason:
    /// the component lives on the *ingest* entity rather than the render one.
    /// Keeping this value on the render-side draw record avoids introducing a
    /// second snapshot boundary for data the extract system can already reach
    /// directly.
    ///
    /// Both halves of the disjunction are live: [`Self::death_time`] carries
    /// `deathTime`, so the overlay now persists through the fall-over instead of
    /// ending ten ticks after the killing blow.
    pub hurt: bool,
    /// This entity's `deathTime + partialTicks` while it is dying, `0.0` while it
    /// is alive — vanilla's own living-entity render-state extraction writes
    /// the death time as the tick count plus the partial tick while dying, or
    /// zero while alive.
    ///
    /// Read off [`lodestone_ecs::entity::DeathTime`] through [`EntityIndex`] in
    /// [`extract_entity_draws`], bridged exactly like [`Self::hurt`] and for the
    /// same reason — the component lives on the *ingest* entity.
    ///
    /// # One field, two consumers, and one of them is a rotation
    ///
    /// It feeds [`Self::hurt`]'s `deathTime > 0` half (right here, in this module)
    /// and the **fall-over rotation**
    /// ([`lodestone_render::entity_anim::death_fall_over_degrees`], composed into
    /// the placement by [`lodestone_render::dying_entity_model_matrix`]). Keeping
    /// one tick count rather than a bool and an angle is what stops the tint and the
    /// topple disagreeing about when death began.
    ///
    /// Not to be confused with `camera_rig`'s own `death_roll_degrees`, which is a
    /// *different* vanilla expression (vanilla's own damage-bob transform's
    /// `40 - 8000/(min(deathTime, 20) + 200)`) on the same input: that one rolls the
    /// local player's **camera** when *they* die, this one topples an entity's
    /// **model**. Both are driven by a `deathTime` and they are not interchangeable.
    pub death_time: f32,
    /// This entity's using-item state, when it has ever reported the
    /// `LivingEntity` flags byte — `None` otherwise, like every other component
    /// bridged off the ingest entity.
    ///
    /// # Why the draw needs it and not just [`Self::anim`]
    ///
    /// [`arm_pose_for`] already folds this into `anim.arm_pose`, and that is
    /// enough for the *arms*. It is not enough for the **item**: an item's
    /// definition tree branches on `minecraft:using_item` and dispatches on
    /// `minecraft:use_duration`, so a drawn bow is a different *model*
    /// (`item/bow_pulling_0/1/2`) and not just a different pose.
    /// [`ArmPose::BowAndArrow`] carries no tick count, so it cannot tell
    /// `bow_pulling_0` from `_2` — which is exactly the flattening
    /// [`lodestone_render::ItemVariants`] exists to undo.
    ///
    /// `off_hand` is load-bearing here in a way it is not for the pose: vanilla's
    /// `using_item` property is
    /// `owner.isUsingItem() && owner.getUseItem() == itemStack`, so
    /// `RenderState::merge_held_items` must compare it against the arm it is
    /// drawing or a skeleton drawing a bow would bend its off-hand item too.
    pub item_use: Option<ItemUse>,
    /// Vanilla's own main-arm accessor equal to left-handed, i.e.
    /// [`lodestone_ecs::entity::MobState::left_handed`]. `false` (right-handed)
    /// for every entity that has never reported the mob-flags byte, same as
    /// [`AnimInput::aggressive`](lodestone_render::entity_anim::AnimInput::aggressive).
    ///
    /// Flips which physical arm both [`arm_pose_for`]'s pose and the held-item
    /// mesh resolve to: a left-handed mob's main-hand item and its ranged pose
    /// both belong on its left arm, not its right. Every equipment-slot → `Arm`
    /// mapping in `gpu/entity_passes.rs`/`gpu/world_items.rs` must XOR against
    /// this rather than assume vanilla's own main-arm accessor is always right-handed.
    pub main_arm_left: bool,
    /// A creeper's pre-detonation swell, `0.0..~1.07`, vanilla's own
    /// per-partial-tick swelling accessor — `0.0` (and hence
    /// [`lodestone_render::entity_anim::Skeleton::pose_swelling`]'s exact
    /// identity case) for every non-creeper, and for a creeper whose fuse is
    /// unlit. Interpolated from [`CreeperFuse::old_swell`]/`swell` by this
    /// frame's partial tick, the same way [`Self::feet`] etc. are.
    ///
    /// This one field feeds **two** consumers downstream (both in `gpu.rs`,
    /// not this module): the whole-model scale
    /// (`Skeleton::pose_swelling`/`creeper_swell_scale`) and, via
    /// [`lodestone_render::entity_anim::creeper_white_overlay_progress`] and
    /// [`lodestone_render::entity_pipeline::creeper_overlay_alpha_from_progress`],
    /// the white-flash overlay — see those two functions' docs for why one
    /// swelling value is enough to drive both.
    pub creeper_swelling: f32,
    /// Vanilla's own swim-amount field, interpolated for this frame — a `0..1` ramp
    /// toward the swim pose, `0.0` for every entity that has never reported
    /// [`Pose`]`::`[`Swimming`](lodestone_model::EntityPose::Swimming). Mirrors
    /// [`lodestone_physics::player::PlayerState::swim_amount`] for a network
    /// entity we do not run physics for; see [`SwimRamp`]/[`tick_swim_ramp`]
    /// for the client-side integration this interpolates between.
    ///
    /// Its only consumer today is the body-pitch rotation
    /// `gpu/entity_passes.rs` applies to a `"player"` [`Self::type_path`] —
    /// see that module for why only the player is ported (vanilla's own
    /// living-entity rotation setup has no swim branch at all; only
    /// its own avatar renderer and drowned renderer override it, with two different
    /// formulas, and this field only drives the one this build implements).
    pub swim_amount: f32,
    /// Whether this entity's shared-flags byte reports bit `0x01` — vanilla's
    /// own display-fire-animation gate: on fire and not a spectator. Player
    /// report: "mobs dont show
    /// flames yet".
    ///
    /// Read off [`lodestone_ecs::entity::EntityFlags`] through [`EntityIndex`]
    /// in [`extract_entity_draws`], bridged the **same way** [`Self::hurt`]
    /// and [`Self::item_use`] already are — `false` for an entity that has
    /// never reported the shared-flags byte at all (`EntityFlags` absent),
    /// which is the correct default: an entity metadata has never described
    /// cannot be known to be on fire.
    ///
    /// This deliberately does **not** re-check vanilla's `!isSpectator()`
    /// half of the gate: a remote entity's game mode is not tracked on this
    /// side of the wire, and the server should never set bit `0x01` on a
    /// spectator's own metadata in the first place (spectators are otherwise
    /// invisible to other clients).
    ///
    /// **Not** the first-person full-screen fire overlay
    /// (`gpu/screen_effects.rs`'s `Vitals::on_fire`, via `ingest.rs`'s
    /// `apply_local_player_on_fire`) — that is a different byte read for a
    /// different, local-player-only purpose, and this field must never feed
    /// it. See `docs/entity-rendering.md`'s "Mob fire" section.
    pub on_fire: bool,
    /// Bit `0x20` of the same shared-flags byte [`Self::on_fire`] reads bit
    /// `0x01` of — vanilla's own is-invisible check. Bridged off the ingest
    /// entity's `EntityFlags` through `EntityIndex` in
    /// [`extract_entity_draws`], the same way `on_fire` is; `false` for an
    /// entity that has never reported the byte.
    ///
    /// Gates only the entity's own body/rig — `RenderState::prepare_entities`
    /// (`gpu/entity_passes.rs`) skips the model batch entirely when this is
    /// set, matching vanilla's own living-entity render submission's
    /// is-body-visible gate on
    /// its own submit-model call. Armour and held items are unaffected: they are
    /// drawn by `prepare_armour`/`merge_held_items`/`special_item_instances`,
    /// each of which re-resolves the entity's pose independently rather than
    /// reusing the body pass's instance, matching vanilla's own
    /// `shouldRenderLayers` running unconditionally regardless of body
    /// visibility. The nametag pass reads this same `entities` slice too and
    /// is equally untouched by the body-batch skip — an invisible, named
    /// entity still shows its tag, which covers the server-hologram case of an
    /// invisible, custom-named armour stand.
    ///
    /// **Not implemented, on purpose, rather than half-built:**
    /// `state.isInvisibleToPlayer` — vanilla still shows an invisible entity,
    /// translucently, to a spectator or a teammate whose team has
    /// `canSeeFriendlyInvisibles`. This draw site has no notion of the
    /// *local* viewer's own game mode, and doing this faithfully needs a
    /// translucent render path this renderer does not have. The glowing
    /// outline (bit `0x40`, vanilla's own is-currently-glowing check) is the same story
    /// — it needs a real outline pass — and is left decoded-and-unread on the
    /// shared-flags byte rather than added here as a field with no consumer,
    /// which is the exact island shape this repo's evidence standards call
    /// out.
    pub invisible: bool,
    /// This entity's own `ArmorStand.DATA_CLIENT_FLAGS` byte, `None` for
    /// every non-`armor_stand` type and for one that has never reported it.
    /// Bridged off the ingest entity's
    /// [`lodestone_ecs::entity::ArmorStandFlags`] through `EntityIndex` in
    /// [`extract_entity_draws`], the same way [`Self::invisible`] is.
    ///
    /// `small` is not read from here a second time — [`resolve_entity_facts`]
    /// already folds it into [`Self::scale`] (a uniform half-scale,
    /// approximating vanilla's separate small-model bake). `show_arms` and
    /// `no_base_plate` are consumed in `gpu/entity_passes.rs`'s
    /// `prepare_entities`, which collapses the named part's own matrix to a
    /// point instead of drawing it — the corpus's `armor_stand` model has
    /// real `left_arm`/`right_arm`/`base_plate` parts to hide, matching
    /// vanilla's own armor-stand-model animation setup toggling each part's
    /// visibility on
    /// the same three. `marker` has no consumer: vanilla's own use of it is a
    /// render-type switch (vanilla's own armor-stand render-type accessor, cutout instead
    /// of the default humanoid render type) with no equivalent pipeline state
    /// here, so it stays decoded-and-unread rather than approximated.
    pub armor_stand: Option<lodestone_ecs::entity::ArmorStandFlags>,
    /// This player's declared skin, carried through from
    /// [`EntityFacts::player_skin`] — `None` for every non-player.
    ///
    /// Two consumers, and they must agree: [`Self::model_type_path`] picks the
    /// **rig** from `model`, and `RenderState::prepare_entities` groups by `url`
    /// so the batch carries the **sheet**. A slim-authored sheet on the wide rig
    /// puts the arm UVs a texel out; the wide sheet on the slim rig leaves a gap
    /// at the shoulder. Neither reads as a model bug.
    pub player_skin: Option<crate::remote_skins::RemoteSkin>,
    /// The **variant** texture sheet this entity resolves to — a corpus reference
    /// like `entity/wolf/wolf_ashen` — or `None` for an entity whose model has no
    /// variant axis, or whose reported variant carries nothing that axis can use.
    ///
    /// Resolved once per poll in [`extract_entity_draws`] by
    /// [`lodestone_render::entity_variant_sheet_for`], which is
    /// `EntityTexture::resolve`'s **first production caller**: the corpus has
    /// modelled nine wolf breeds and three climate skins the whole time, and every
    /// consumer asked only for `default_path()`, so every wolf drew pale and every
    /// pig drew temperate. A function with zero production *readers* is the dual of
    /// this repo's usual island, and a connectedness scan cannot see it — the packet
    /// decodes, the fold lands on a component, and nothing downstream asks.
    ///
    /// Consumed exactly like [`Self::player_skin`]: it joins the draw-grouping key
    /// so one batch is one sheet, and a *miss* in the shell's variant texture map
    /// falls back to the model's own sheet rather than failing. Two mobs of the same
    /// species and different breeds are therefore two batches, which is what vanilla
    /// pays too — its `getTextureLocation` is per entity.
    ///
    /// **A wolf's tame state is part of this**:
    /// [`extract_entity_draws`] bridges [`lodestone_ecs::entity::Tamed`] off the
    /// ingest entity, the same way it bridges `Variant`, and passes it through to
    /// `entity_variant_sheet_for`'s `tamed` parameter — see that function's own doc
    /// for the wire chain and for why only a wolf's sheet reads the bit.
    pub variant_sheet: Option<&'static str>,
    /// An experience orb's XP value (`ExperienceOrb.DATA_VALUE`), bridged off the
    /// ingest entity's [`ExperienceOrbValue`] component — `None` for every entity
    /// that is not an orb, which is the switch the orb pass keys on.
    ///
    /// Its only consumer is `lodestone_render::experience_orb_icon`, which buckets
    /// it into one of eleven sprite cells. `Some(0)` and `None` therefore draw the
    /// same cell, and that is correct rather than sloppy: vanilla's accessor
    /// default *is* `0`, so an orb whose value never reached us looks exactly like
    /// an orb genuinely worth nothing. What must not happen is the two collapsing
    /// the other way — `None` reading as "not an orb" for a real orb, which draws
    /// nothing at all.
    ///
    /// **Not the orb's `count`.** Vanilla keeps `value` (what one absorption pays,
    /// synced) and `count` (how many absorptions the entity holds after merging,
    /// server-only) as two different numbers, and only the first is on the wire.
    pub experience_orb_value: Option<i32>,
    /// A primed TNT entity's synchronized fuse time, in ticks remaining. It is
    /// `None` for every other entity and for the short window before a TNT
    /// spawn's first metadata packet arrives; the moving-block pass renders
    /// that missing-data state at its unlit, unswelled default. Once present,
    /// the value drives the final-ten-tick swell and five-tick white flash.
    pub tnt_fuse: Option<f32>,
    /// This frame's interpolated `(capeLean, capeLean2, capeFlap)`, all
    /// degrees — see [`cape_sway`] for the derivation and [`CapeLag`] for the
    /// per-tick state it comes from. Computed for every tracked entity (the
    /// state is cheap — see [`CapeLag`]'s doc), consumed only when
    /// [`Self::type_path`] is `"player"` and [`Self::player_skin`] declares a
    /// cape, exactly like [`Self::swim_amount`]'s single player-only reader.
    pub cape_sway: (f32, f32, f32),
}

impl EntityDraw {
    /// The corpus name to resolve this entity's mesh from — [`Self::type_path`]
    /// for everything except a player whose skin declares the **slim** rig.
    ///
    /// A separate accessor rather than rewriting `type_path` at the fold, and
    /// that distinction is load-bearing: `type_path` is also what
    /// `gpu/nametag.rs` resolves through `entity_dimensions::base_dimensions` to place
    /// the tag above the head, and `"player_slim"` is **not** an entity-type
    /// registry path — it would miss, fall back to `FALLBACK_HEIGHT`, and put
    /// every slim player's nametag at the wrong height. `world_items.rs`,
    /// `debug_lines.rs` and the flame pass read it the same way.
    ///
    /// `lodestone_render::entity::player_model_name` is the one place the two
    /// rig names live; both are first-class corpus entries, so
    /// `canonical_model_name` resolves the literal with no extra plumbing.
    #[must_use]
    pub fn model_type_path(&self) -> &str {
        match &self.player_skin {
            Some(skin) if skin.model == lodestone_assets::PlayerModelType::Slim => {
                lodestone_render::entity::player_model_name(true)
            }
            _ => &self.type_path,
        }
    }
}
